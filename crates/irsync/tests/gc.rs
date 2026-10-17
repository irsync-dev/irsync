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
fn keygen_prints_endpoint_id_matching_pub() {
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("k/id_ed25519");
    let run = || {
        Command::new(env!("CARGO_BIN_EXE_irsync"))
            .args(["keygen", "--out"])
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
fn daemon_show_id_prints_only_the_id() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = dir.path().join("d.toml");
    std::fs::write(
        &cfg,
        format!(
            "identity = {:?}\nauthorized_keys = {:?}\nstate_dir = {:?}\n",
            dir.path().join("id"),
            dir.path().join("ak"),
            dir.path().join("state")
        ),
    )
    .unwrap();
    let o = Command::new(env!("CARGO_BIN_EXE_irsync"))
        .args(["daemon", "--show-id", "--config"])
        .arg(&cfg)
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let out = String::from_utf8(o.stdout).unwrap();
    assert_eq!(out.lines().count(), 1, "{out}");
    irsync::identity::parse_endpoint_id(out.trim()).unwrap();
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
async fn gc_keeps_part_referenced_by_module_relative_plan() {
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
    // A partial record untouched for two hours makes it due again.
    f.ix.lock()
        .unwrap()
        .put_partial(&rel("p"), &partial(now() - 7200))
        .unwrap();
    assert!(gc_if_due(f.ix.clone(), f.root.path(), age, stale)
        .await
        .unwrap()
        .is_some());
}
