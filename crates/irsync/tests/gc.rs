use std::path::Path;
use std::process::Command;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use irsync::daemon::gc;
use irsync::fs::paths::RelPath;
use irsync::identity::endpoint_id_of_public_key;
use irsync::index::{Index, Partial, SessionRow};
use irsync_proto::v1::{plan_item::Action, PlanEntry, RangeSet, SessionPlan};
use nix::sys::stat::{utimensat, UtimensatFlags};
use nix::sys::time::TimeSpec;
use prost::Message;

const DAY: u64 = 86400;

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}

fn touch_ago(path: &Path, days: i64) {
    let t = TimeSpec::new(now() - days * DAY as i64, 0);
    utimensat(
        nix::fcntl::AT_FDCWD,
        path,
        &t,
        &t,
        UtimensatFlags::NoFollowSymlink,
    )
    .unwrap();
}

fn rel(s: &str) -> RelPath {
    RelPath::new(s.as_bytes()).unwrap()
}

fn partial(updated_at: i64) -> Partial {
    Partial {
        expected_hash: [1; 32],
        size: 10,
        ranges: RangeSet::default(),
        updated_at,
    }
}

fn session(id: u8, path: &str, age_days: i64) -> SessionRow {
    let plan = SessionPlan {
        entries: vec![PlanEntry {
            file_id: 1,
            path: path.as_bytes().to_vec(),
            action: Action::Delta as i32,
            ..PlanEntry::default()
        }],
        ..SessionPlan::default()
    };
    let t = now() - age_days * DAY as i64;
    SessionRow {
        id: [id; 16],
        key: vec![id],
        plan: plan.encode_to_vec(),
        progress: vec![],
        created_at: t,
        updated_at: t,
    }
}

struct Fixture {
    _state: tempfile::TempDir,
    root: tempfile::TempDir,
    ix: irsync::index::IndexHandle,
}

fn fixture() -> Fixture {
    let state = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    let ix = Index::open(state.path(), root.path())
        .unwrap()
        .into_handle();
    let r = root.path();
    std::fs::create_dir(r.join("d")).unwrap();
    // Orphan, 8 days old.
    std::fs::write(r.join("d/.irsync.old.part"), b"x").unwrap();
    touch_ago(&r.join("d/.irsync.old.part"), 8);
    // Orphan, 1 day old.
    std::fs::write(r.join(".irsync.new.part"), b"x").unwrap();
    touch_ago(&r.join(".irsync.new.part"), 1);
    // Old, referenced by a dormant session's plan.
    std::fs::write(r.join(".irsync.held.part"), b"x").unwrap();
    touch_ago(&r.join(".irsync.held.part"), 8);
    ix.lock()
        .unwrap()
        .save_session(&session(1, "held", 3))
        .unwrap();
    // Not a part file: must never be touched.
    std::fs::write(r.join("d/precious.txt"), b"keep").unwrap();
    touch_ago(&r.join("d/precious.txt"), 30);
    // Looks close, but is not the pattern.
    std::fs::write(r.join(".irsync.nopart"), b"keep").unwrap();
    touch_ago(&r.join(".irsync.nopart"), 30);
    Fixture {
        _state: state,
        root,
        ix,
    }
}

#[tokio::test]
async fn gc_removes_orphan_part_older_than_timeout() {
    let f = fixture();
    let r = f.root.path();
    let rep = gc(f.ix.clone(), r, Duration::from_secs(7 * DAY), false)
        .await
        .unwrap();
    assert_eq!(rep.parts_removed, vec![rel("d/.irsync.old.part")]);
    assert!(!r.join("d/.irsync.old.part").exists());
    assert!(r.join(".irsync.new.part").exists());
    assert!(r.join(".irsync.held.part").exists());
    assert!(r.join("d/precious.txt").exists());
    assert!(r.join(".irsync.nopart").exists());
}

#[tokio::test]
async fn gc_collects_stale_partial_rows_and_dormant_sessions() {
    let f = fixture();
    let r = f.root.path();
    {
        let mut ix = f.ix.lock().unwrap();
        // Stale unreferenced record with its file; fresh record; stale but referenced.
        std::fs::write(r.join(".irsync.rec.part"), b"x").unwrap();
        touch_ago(&r.join(".irsync.rec.part"), 9);
        ix.put_partial(&rel("rec"), &partial(now() - 9 * DAY as i64))
            .unwrap();
        std::fs::write(r.join(".irsync.live.part"), b"x").unwrap();
        touch_ago(&r.join(".irsync.live.part"), 9);
        ix.put_partial(&rel("live"), &partial(now())).unwrap();
        ix.put_partial(&rel("held"), &partial(now() - 9 * DAY as i64))
            .unwrap();
        ix.save_session(&session(2, "gone", 9)).unwrap();
    }
    let rep = gc(f.ix.clone(), r, Duration::from_secs(7 * DAY), false)
        .await
        .unwrap();
    assert!(!r.join(".irsync.rec.part").exists());
    assert!(
        r.join(".irsync.live.part").exists(),
        "fresh record guards the file"
    );
    assert!(r.join(".irsync.held.part").exists());
    assert!(rep.parts_removed.contains(&rel(".irsync.rec.part")));
    assert!(!rep.parts_removed.contains(&rel(".irsync.live.part")));
    assert_eq!(rep.sessions_removed, 1);
    let ix = f.ix.lock().unwrap();
    assert!(ix.partial(&rel("rec")).unwrap().is_none());
    assert!(ix.partial(&rel("live")).unwrap().is_some());
    assert!(ix.partial(&rel("held")).unwrap().is_some());
}

#[tokio::test]
async fn gc_never_follows_symlinks() {
    let f = fixture();
    let r = f.root.path();
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(outside.path().join("victim"), b"v").unwrap();
    std::os::unix::fs::symlink(outside.path().join("victim"), r.join(".irsync.link.part")).unwrap();
    std::os::unix::fs::symlink(outside.path(), r.join("dirlink")).unwrap();
    std::fs::write(outside.path().join(".irsync.out.part"), b"v").unwrap();
    touch_ago(&outside.path().join(".irsync.out.part"), 30);
    gc(f.ix.clone(), r, Duration::from_secs(7 * DAY), false)
        .await
        .unwrap();
    assert!(outside.path().join("victim").exists());
    assert!(outside.path().join(".irsync.out.part").exists());
}

#[tokio::test]
async fn gc_dry_run_removes_nothing() {
    let f = fixture();
    let r = f.root.path();
    let rep = gc(f.ix.clone(), r, Duration::from_secs(7 * DAY), true)
        .await
        .unwrap();
    assert_eq!(rep.parts_removed, vec![rel("d/.irsync.old.part")]);
    assert!(r.join("d/.irsync.old.part").exists());
    assert_eq!(rep.sessions_removed, 0);
}

#[test]
fn id_generate_prints_endpoint_id_matching_pub() {
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("k/id_ed25519");
    let run = || {
        Command::new(env!("CARGO_BIN_EXE_irsync"))
            .args(["id", "generate", "--out"])
            .arg(&out)
            .output()
            .unwrap()
    };
    let o = run();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let text = String::from_utf8(o.stdout).unwrap();
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    assert_eq!(lines.len(), 2, "{text}");
    let pk = ssh_key::PublicKey::from_openssh(lines[0]).unwrap();
    let id = endpoint_id_of_public_key(&pk).unwrap();
    assert_eq!(lines[1].trim(), id.to_string());
    // A second run refuses to overwrite.
    let again = run();
    assert!(!again.status.success());
    assert!(String::from_utf8_lossy(&again.stderr).contains("refusing to overwrite"));
}

#[test]
fn id_daemon_prints_the_key_line_and_the_id_of_the_configured_identity() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = dir.path().join("d.toml");
    std::fs::write(
        &cfg,
        format!(
            "state_dir = {:?}\n[daemon]\nidentity = {:?}\n",
            dir.path().join("state"),
            dir.path().join("id")
        ),
    )
    .unwrap();
    let o = Command::new(env!("CARGO_BIN_EXE_irsync"))
        .args(["id", "show", "--daemon", "--config"])
        .arg(&cfg)
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let out = String::from_utf8(o.stdout).unwrap();
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines.len(), 2, "{out}");
    assert!(lines[0].starts_with("ssh-ed25519 "), "{out}");
    irsync::identity::parse_endpoint_id(lines[1]).unwrap();
    assert!(
        dir.path().join("id").exists(),
        "generated at the configured path"
    );
}

#[tokio::test]
async fn gc_continues_after_one_removal_failure() {
    use std::os::unix::fs::PermissionsExt;
    if nix::unistd::geteuid().is_root() {
        return;
    }
    let f = fixture();
    let r = f.root.path();
    std::fs::create_dir(r.join("ro")).unwrap();
    std::fs::write(r.join("ro/.irsync.stuck.part"), b"x").unwrap();
    touch_ago(&r.join("ro/.irsync.stuck.part"), 8);
    std::fs::set_permissions(r.join("ro"), std::fs::Permissions::from_mode(0o555)).unwrap();
    f.ix.lock()
        .unwrap()
        .save_session(&session(9, "gone", 9))
        .unwrap();
    let rep = gc(f.ix.clone(), r, Duration::from_secs(7 * DAY), false)
        .await
        .unwrap();
    std::fs::set_permissions(r.join("ro"), std::fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(rep.errors.len(), 1, "{rep:?}");
    assert_eq!(rep.errors[0].0, rel("ro/.irsync.stuck.part"));
    assert!(!r.join("d/.irsync.old.part").exists());
    assert_eq!(rep.sessions_removed, 1, "session pass still ran");
}

#[tokio::test]
async fn gc_keeps_part_referenced_by_export_relative_plan() {
    for (plan_path, kept) in [("b/x", true), ("c/x", false)] {
        let state = tempfile::tempdir().unwrap();
        let root = tempfile::tempdir().unwrap();
        let ix = Index::open(state.path(), root.path())
            .unwrap()
            .into_handle();
        std::fs::create_dir_all(root.path().join("a/b")).unwrap();
        let part = root.path().join("a/b/.irsync.x.part");
        std::fs::write(&part, b"x").unwrap();
        touch_ago(&part, 8);
        ix.lock()
            .unwrap()
            .save_session(&session(3, plan_path, 1))
            .unwrap();
        gc(ix, root.path(), Duration::from_secs(7 * DAY), false)
            .await
            .unwrap();
        assert_eq!(part.exists(), kept, "plan path {plan_path}");
    }
}

#[tokio::test]
async fn gc_row_without_inplace_file_is_not_counted_as_removed() {
    let f = fixture();
    f.ix.lock()
        .unwrap()
        .put_partial(&rel("pd"), &partial(now() - 9 * DAY as i64))
        .unwrap();
    let rep = gc(
        f.ix.clone(),
        f.root.path(),
        Duration::from_secs(7 * DAY),
        false,
    )
    .await
    .unwrap();
    assert_eq!(rep.rows_without_file, 1);
    assert!(!rep.parts_removed.contains(&rel(".irsync.pd.part")));
    assert!(f.ix.lock().unwrap().partial(&rel("pd")).unwrap().is_none());
}

#[tokio::test]
async fn gc_dry_run_counts_sessions() {
    let f = fixture();
    f.ix.lock()
        .unwrap()
        .save_session(&session(5, "gone", 9))
        .unwrap();
    let rep = gc(
        f.ix.clone(),
        f.root.path(),
        Duration::from_secs(7 * DAY),
        true,
    )
    .await
    .unwrap();
    assert_eq!(rep.sessions_removed, 1);
    assert!(f
        .ix
        .lock()
        .unwrap()
        .load_session_by_id(&[5; 16])
        .unwrap()
        .is_some());
    // A dry run is not a collection: it does not postpone the next one.
    assert_eq!(f.ix.lock().unwrap().last_gc_at().unwrap(), None);
}

#[test]
fn client_gc_skipped_within_24h() {
    use irsync::daemon::gc_due;
    let t = 1_700_000_000;
    assert!(gc_due(None, t, false), "never collected");
    assert!(!gc_due(Some(t), t + 3600, false));
    assert!(!gc_due(Some(t), t + DAY as i64 - 1, false));
    assert!(gc_due(Some(t), t + DAY as i64, false));
    assert!(gc_due(Some(t), t + 60, true), "stale partials make it due");
    // The clock stepped back past the last walk: its time says nothing.
    assert!(gc_due(Some(t + 3600), t, false), "last walk in the future");
}

#[tokio::test]
async fn gc_if_due_records_and_skips() {
    use irsync::daemon::gc_if_due;
    let f = fixture();
    let (age, stale) = (Duration::from_secs(7 * DAY), Duration::from_secs(3600));
    let first = gc_if_due(f.ix.clone(), f.root.path(), age, stale)
        .await
        .unwrap();
    assert!(first.is_some(), "first run walks");
    let at = f.ix.lock().unwrap().last_gc_at().unwrap().unwrap();
    assert!((now() - at).abs() < 60);
    // Within a day and nothing stale: skipped.
    assert!(gc_if_due(f.ix.clone(), f.root.path(), age, stale)
        .await
        .unwrap()
        .is_none());
    // A record that went stale (an hour untouched) since the last walk
    // makes it due again; the walk keeps it (younger than `age`).
    f.ix.lock()
        .unwrap()
        .set_last_gc_at(now() - 3 * 3600)
        .unwrap();
    f.ix.lock()
        .unwrap()
        .put_partial(&rel("p"), &partial(now() - 7200))
        .unwrap();
    assert!(gc_if_due(f.ix.clone(), f.root.path(), age, stale)
        .await
        .unwrap()
        .is_some());
    assert!(f.ix.lock().unwrap().partial(&rel("p")).unwrap().is_some());
    // It was stale at that walk already: not due again until the day is up.
    assert!(gc_if_due(f.ix.clone(), f.root.path(), age, stale)
        .await
        .unwrap()
        .is_none());
}

/// A destination name too long for the plain `.irsync.<name>.part` form.
fn long_name(c: u8) -> Vec<u8> {
    vec![c; 250]
}

fn long_part(c: u8) -> RelPath {
    RelPath::new(&irsync::recv::temp_name(&long_name(c), b".part")).unwrap()
}

#[tokio::test]
async fn gc_handles_shortened_part_names() {
    let f = fixture();
    let r = f.root.path();
    let dest = |c: u8| RelPath::new(&long_name(c)).unwrap();
    let plant = |c: u8, days: i64| {
        let p = r.join(long_part(c).to_os());
        assert!(p.file_name().unwrap().len() <= 255);
        std::fs::write(&p, b"x").unwrap();
        touch_ago(&p, days);
    };
    // Orphan with a shortened name, stale: removed.
    plant(b'o', 8);
    // Shortened name whose destination has a fresh record: kept.
    plant(b'r', 9);
    // Shortened name whose destination has a stale record: removed, with the row.
    plant(b's', 9);
    // Shortened name whose destination a dormant session's plan references: kept.
    plant(b'h', 8);
    {
        let mut ix = f.ix.lock().unwrap();
        ix.put_partial(&dest(b'r'), &partial(now())).unwrap();
        ix.put_partial(&dest(b's'), &partial(now() - 9 * DAY as i64))
            .unwrap();
        let plan = SessionPlan {
            entries: vec![PlanEntry {
                file_id: 1,
                path: long_name(b'h'),
                action: Action::Delta as i32,
                ..PlanEntry::default()
            }],
            ..SessionPlan::default()
        };
        let t = now() - 3 * DAY as i64;
        ix.save_session(&SessionRow {
            id: [9; 16],
            key: vec![9],
            plan: plan.encode_to_vec(),
            progress: vec![],
            created_at: t,
            updated_at: t,
        })
        .unwrap();
    }
    let rep = gc(f.ix.clone(), r, Duration::from_secs(7 * DAY), false)
        .await
        .unwrap();
    assert!(!r.join(long_part(b'o').to_os()).exists(), "orphan removed");
    assert!(
        r.join(long_part(b'r').to_os()).exists(),
        "fresh record kept"
    );
    assert!(
        !r.join(long_part(b's').to_os()).exists(),
        "stale record removed"
    );
    assert!(r.join(long_part(b'h').to_os()).exists(), "referenced kept");
    assert!(rep.parts_removed.contains(&long_part(b'o')));
    assert!(rep.parts_removed.contains(&long_part(b's')));
    let ix = f.ix.lock().unwrap();
    assert!(ix.partial(&dest(b'r')).unwrap().is_some());
    assert!(ix.partial(&dest(b's')).unwrap().is_none());
}

/// Indexes `root/path` as it is on disk now.
fn index_file(ix: &irsync::index::IndexHandle, root: &Path, path: &str) {
    let (e, ctime) = irsync::fs::meta::lstat_entry_with_ctime(root, &rel(path)).unwrap();
    ix.lock()
        .unwrap()
        .put_entry(&rel(path), &e, ctime, None)
        .unwrap();
}

#[tokio::test]
async fn gc_prunes_entries_of_files_gone_or_replaced() {
    let f = fixture();
    let r = f.root.path();
    std::fs::create_dir(r.join("locked")).unwrap();
    for p in ["keep", "gone", "replaced", "locked/hidden"] {
        std::fs::write(r.join(p), b"content").unwrap();
        index_file(&f.ix, r, p);
    }
    std::fs::remove_file(r.join("gone")).unwrap();
    // Another inode at the same path.
    std::fs::write(r.join("new"), b"content").unwrap();
    std::fs::rename(r.join("new"), r.join("replaced")).unwrap();
    // A directory the walk cannot read hides its files; they stay indexed.
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(r.join("locked"), std::fs::Permissions::from_mode(0o000)).unwrap();

    let dry = gc(f.ix.clone(), r, Duration::from_secs(7 * DAY), true)
        .await
        .unwrap();
    assert_eq!(dry.entries_removed, 2);
    assert!(f
        .ix
        .lock()
        .unwrap()
        .get_entry(&rel("gone"))
        .unwrap()
        .is_some());

    let report = gc(f.ix.clone(), r, Duration::from_secs(7 * DAY), false)
        .await
        .unwrap();
    std::fs::set_permissions(r.join("locked"), std::fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(report.entries_removed, 2);
    let ix = f.ix.lock().unwrap();
    assert!(ix.get_entry(&rel("gone")).unwrap().is_none());
    assert!(ix.get_entry(&rel("replaced")).unwrap().is_none());
    assert!(ix.get_entry(&rel("keep")).unwrap().is_some());
    assert!(ix.get_entry(&rel("locked/hidden")).unwrap().is_some());
}

#[tokio::test]
async fn gc_keeps_entries_when_the_root_holds_no_files() {
    // An empty root may be a mount point whose filesystem is not mounted:
    // its files are not gone, so the index keeps them.
    let state = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    let r = root.path();
    let ix = Index::open(state.path(), r).unwrap().into_handle();
    std::fs::write(r.join("a"), b"content").unwrap();
    index_file(&ix, r, "a");
    std::fs::remove_file(r.join("a")).unwrap();
    let report = gc(ix.clone(), r, Duration::from_secs(7 * DAY), false)
        .await
        .unwrap();
    assert_eq!(report.entries_removed, 0);
    assert!(ix.lock().unwrap().get_entry(&rel("a")).unwrap().is_some());
}

/// A stale record whose part file was written since (a receiver that has
/// not refreshed the record yet): the file's own mtime keeps both.
#[tokio::test]
async fn gc_keeps_a_stale_record_whose_file_is_fresh() {
    let f = fixture();
    let r = f.root.path();
    std::fs::write(r.join(".irsync.busy.part"), b"x").unwrap();
    touch_ago(&r.join(".irsync.busy.part"), 1);
    f.ix.lock()
        .unwrap()
        .put_partial(&rel("busy"), &partial(now() - 9 * DAY as i64))
        .unwrap();
    let rep = gc(f.ix.clone(), r, Duration::from_secs(7 * DAY), false)
        .await
        .unwrap();
    assert!(r.join(".irsync.busy.part").exists());
    assert!(!rep.parts_removed.contains(&rel(".irsync.busy.part")));
    assert_eq!(rep.parts_kept, 2, "busy, and held by a session");
    assert!(f
        .ix
        .lock()
        .unwrap()
        .partial(&rel("busy"))
        .unwrap()
        .is_some());
}

/// `irsync gc` as a user runs it: finds every index under the configured
/// state dir by the root recorded in it, lists what it removes (or would),
/// and counts parts, sessions and index entries.
#[test]
fn irsync_gc_end_to_end() {
    let d = tempfile::tempdir().unwrap();
    let (home, root, state) = (
        d.path().join("home"),
        d.path().join("root"),
        d.path().join("state"),
    );
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(root.join("d")).unwrap();
    let config = d.path().join("irsync.toml");
    std::fs::write(&config, format!("state_dir = {state:?}\n")).unwrap();
    std::fs::write(root.join("d/.irsync.old.part"), b"x").unwrap();
    touch_ago(&root.join("d/.irsync.old.part"), 8);
    std::fs::write(root.join(".irsync.new.part"), b"x").unwrap();
    touch_ago(&root.join(".irsync.new.part"), 1);
    std::fs::write(root.join("keep"), b"k").unwrap();
    std::fs::write(root.join("gone"), b"g").unwrap();
    {
        let ix = Index::open(&state, &root).unwrap().into_handle();
        index_file(&ix, &root, "keep");
        index_file(&ix, &root, "gone");
        ix.lock()
            .unwrap()
            .save_session(&session(1, "elsewhere", 9))
            .unwrap();
    }
    std::fs::remove_file(root.join("gone")).unwrap();
    let gc = |args: &[&str]| {
        let o = Command::new(env!("CARGO_BIN_EXE_irsync"))
            .arg("gc")
            .args(args)
            .arg("--config")
            .arg(&config)
            .env("HOME", &home)
            .env("XDG_STATE_HOME", home.join("xdg"))
            .output()
            .unwrap();
        assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
        String::from_utf8(o.stdout).unwrap()
    };
    let old = std::fs::canonicalize(&root)
        .unwrap()
        .join("d/.irsync.old.part");
    let line = |what: &str| format!("1 part file(s), 1 session(s), 1 index entry {what}");

    let out = gc(&["-n"]);
    assert_eq!(
        out,
        format!("{}\n{}\n", old.display(), line("would be removed"))
    );
    assert!(old.exists());

    let out = gc(&[]);
    assert_eq!(out, format!("{}\n{}\n", old.display(), line("removed")));
    assert!(!old.exists());
    assert!(root.join(".irsync.new.part").exists());
    assert!(root.join("keep").exists());
    let ix = Index::open(&state, &root).unwrap();
    assert!(ix.load_session_by_id(&[1; 16]).unwrap().is_none());
    assert!(ix.get_entry(&rel("gone")).unwrap().is_none());
    assert!(ix.get_entry(&rel("keep")).unwrap().is_some());
    drop(ix);

    assert_eq!(
        gc(&[]),
        "0 part file(s), 0 session(s), 0 index entries removed\n"
    );
    assert_eq!(gc(&["-q"]), "");
}
