mod common;

use common::*;
use fileset::compare::{Field, Tier};
use fileset::engine::Outcome;
use fileset::model::{Flags, Kind};
use fileset::report::fmt_blake3;
use fileset::store::{self, Backend, OpenMode};
use std::path::Path;

/// Invokes each backend-generic test once per backend.
macro_rules! for_both_backends {
    ($($name:ident => $sqlite:ident, $fjall:ident;)*) => {
        $(
            #[test]
            fn $sqlite() {
                $name(Backend::Sqlite)
            }
            #[test]
            fn $fjall() {
                $name(Backend::Fjall)
            }
        )*
    };
}

for_both_backends! {
    update_records_every_kind => update_records_every_kind_sqlite, update_records_every_kind_fjall;
    update_reuses_hash_when_unchanged => update_reuses_hash_when_unchanged_sqlite, update_reuses_hash_when_unchanged_fjall;
    update_rehash_forces => update_rehash_forces_sqlite, update_rehash_forces_fjall;
    update_rehashes_when_mtime_changes => update_rehashes_when_mtime_changes_sqlite, update_rehashes_when_mtime_changes_fjall;
    update_sweeps_stale_generation => update_sweeps_stale_generation_sqlite, update_sweeps_stale_generation_fjall;
    update_warns_on_unreadable => update_warns_on_unreadable_sqlite, update_warns_on_unreadable_fjall;
    update_warns_on_nested_manifest => update_warns_on_nested_manifest_sqlite, update_warns_on_nested_manifest_fjall;
    update_rejects_wrong_store_flag => update_rejects_wrong_store_flag_sqlite, update_rejects_wrong_store_flag_fjall;
    update_with_external_manifest_flags_nothing => update_with_external_manifest_flags_nothing_sqlite, update_with_external_manifest_flags_nothing_fjall;
    check_clean_after_update => check_clean_after_update_sqlite, check_clean_after_update_fjall;
    check_reports_add_del_chg => check_reports_add_del_chg_sqlite, check_reports_add_del_chg_fjall;
    check_relaxed_ignores_mode_and_mtime => check_relaxed_ignores_mode_and_mtime_sqlite, check_relaxed_ignores_mode_and_mtime_fjall;
    check_paranoid_reports_nlink => check_paranoid_reports_nlink_sqlite, check_paranoid_reports_nlink_fjall;
    check_ignore_mode_hides_chmod => check_ignore_mode_hides_chmod_sqlite, check_ignore_mode_hides_chmod_fjall;
    check_require_uid_stays_clean_same_user => check_require_uid_stays_clean_same_user_sqlite, check_require_uid_stays_clean_same_user_fjall;
    check_symlink_retarget => check_symlink_retarget_sqlite, check_symlink_retarget_fjall;
    check_kind_change => check_kind_change_sqlite, check_kind_change_fjall;
    check_dir_replaced_by_file_deletes_children => check_dir_replaced_by_file_deletes_children_sqlite, check_dir_replaced_by_file_deletes_children_fjall;
    check_warns_no_stored_checksum => check_warns_no_stored_checksum_sqlite, check_warns_no_stored_checksum_fjall;
    check_unreadable_at_check_time => check_unreadable_at_check_time_sqlite, check_unreadable_at_check_time_fjall;
    check_missing_manifest_is_error => check_missing_manifest_is_error_sqlite, check_missing_manifest_is_error_fjall;
    check_with_external_manifest => check_with_external_manifest_sqlite, check_with_external_manifest_fjall;
    check_output_is_deterministic => check_output_is_deterministic_sqlite, check_output_is_deterministic_fjall;
    check_unreadable_directory_is_a_difference => check_unreadable_directory_is_a_difference_sqlite, check_unreadable_directory_is_a_difference_fjall;
}

fn other(b: Backend) -> Backend {
    match b {
        Backend::Sqlite => Backend::Fjall,
        Backend::Fjall => Backend::Sqlite,
    }
}

/// True (after saying so) when file permissions do not restrict this process, e.g. root.
fn permissions_bypassed(path: &Path) -> bool {
    if std::fs::read(path).is_ok() {
        eprintln!("skipping: running with privileges that bypass file permissions");
        return true;
    }
    false
}

fn sorted(mut v: Vec<&str>) -> Vec<String> {
    v.sort();
    v.into_iter().map(String::from).collect()
}

fn update_records_every_kind(b: Backend) {
    let dir = tree(&[("d/f", "x")]);
    symlink(dir.path(), "l", "f");
    mkfifo(&dir.path().join("p"));
    let (outcome, err) = run_update(dir.path(), b, false, false);
    assert_eq!(outcome, Outcome::Clean);
    assert_eq!(err, "entries=6 hashed=1 reused=0 warnings=0\n");

    let store = open_ro(dir.path(), b);
    assert_eq!(
        keys(&*store),
        sorted(vec![".", "d", "d/f", "l", "p", store_name(b)])
    );
    let f = store.get(b"d/f").unwrap().unwrap();
    assert_eq!(f.blake3, Some(*blake3::hash(b"x").as_bytes()));
    assert_eq!(
        store.get(b"l").unwrap().unwrap().target.as_deref(),
        Some(&b"f"[..])
    );
    assert_eq!(store.get(b"p").unwrap().unwrap().kind, Kind::Fifo);
    let s = store.get(store_name(b).as_bytes()).unwrap().unwrap();
    assert!(s.flags.contains(Flags::STORE));
    assert_eq!(s.blake3, None);
    let meta = store.meta().unwrap();
    assert_eq!(meta.generation, 1);
    assert_eq!(meta.tool_version, env!("CARGO_PKG_VERSION"));
    store
        .for_each(&mut |k, e| {
            assert_eq!(e.generation, 1, "{}", String::from_utf8_lossy(k));
            Ok(())
        })
        .unwrap();
}

fn update_reuses_hash_when_unchanged(b: Backend) {
    let dir = tree(&[("f", "x")]);
    let (_, first) = run_update(dir.path(), b, false, false);
    assert_eq!(first, "entries=3 hashed=1 reused=0 warnings=0\n");
    let created = open_ro(dir.path(), b).meta().unwrap().created_at;
    let (outcome, second) = run_update(dir.path(), b, false, false);
    assert_eq!(outcome, Outcome::Clean);
    assert_eq!(second, "entries=3 hashed=0 reused=1 warnings=0\n");
    let store = open_ro(dir.path(), b);
    let meta = store.meta().unwrap();
    assert_eq!(meta.generation, 2);
    assert_eq!(meta.created_at, created);
    assert_eq!(
        store.get(b"f").unwrap().unwrap().blake3,
        Some(*blake3::hash(b"x").as_bytes())
    );
}

fn update_rehash_forces(b: Backend) {
    let dir = tree(&[("f", "x")]);
    run_update(dir.path(), b, false, false);
    let (_, second) = run_update(dir.path(), b, true, false);
    assert_eq!(second, "entries=3 hashed=1 reused=0 warnings=0\n");
}

fn update_rehashes_when_mtime_changes(b: Backend) {
    let dir = tree(&[("f", "x")]);
    run_update(dir.path(), b, false, false);
    set_mtime(&dir.path().join("f"), 1_000_000, 5);
    let (_, second) = run_update(dir.path(), b, false, false);
    assert_eq!(second, "entries=3 hashed=1 reused=0 warnings=0\n");
    let f = open_ro(dir.path(), b).get(b"f").unwrap().unwrap();
    assert_eq!((f.mtime.sec, f.mtime.nsec), (1_000_000, 5));
}

fn update_sweeps_stale_generation(b: Backend) {
    let dir = tree(&[("f", "x")]);
    run_update(dir.path(), b, false, false);
    let path = dir.path().join(store_name(b));
    let mut st = store::open(&path, b, OpenMode::ReadWrite).unwrap();
    let mut ghost = st.get(b"f").unwrap().unwrap();
    ghost.generation = 0;
    st.put_batch(&[(b"ghost".to_vec(), ghost)]).unwrap();
    st.close().unwrap();
    assert!(keys(&*open_ro(dir.path(), b)).contains(&"ghost".to_string()));

    run_update(dir.path(), b, false, false);
    assert_eq!(
        keys(&*open_ro(dir.path(), b)),
        sorted(vec![".", "f", store_name(b)])
    );
}

fn update_warns_on_unreadable(b: Backend) {
    let dir = tree(&[("f", "secret")]);
    let f = dir.path().join("f");
    chmod(&f, 0o000);
    if permissions_bypassed(&f) {
        return;
    }
    let (outcome, err) = run_update(dir.path(), b, false, false);
    assert_eq!(outcome, Outcome::Clean);
    assert!(err.starts_with("WARN cannot hash f: "), "{err}");
    assert!(
        err.ends_with("\nentries=3 hashed=1 reused=0 warnings=1\n"),
        "{err}"
    );
    let e = open_ro(dir.path(), b).get(b"f").unwrap().unwrap();
    assert!(e.flags.contains(Flags::UNREADABLE));
    assert_eq!(e.blake3, None);

    let (outcome, _) = run_update(dir.path(), b, false, true);
    assert_eq!(outcome, Outcome::Differences);
}

fn update_warns_on_nested_manifest(b: Backend) {
    let dir = tree(&[("sub/.fileset.sqlite", "not a manifest")]);
    let (outcome, err) = run_update(dir.path(), b, false, false);
    assert_eq!(outcome, Outcome::Clean);
    assert!(
        err.starts_with("WARN nested manifest at sub/.fileset.sqlite\n"),
        "{err}"
    );
    let (outcome, _) = run_update(dir.path(), b, false, true);
    assert_eq!(outcome, Outcome::Differences);
}

fn update_rejects_wrong_store_flag(b: Backend) {
    let dir = tree(&[("f", "x")]);
    run_update(dir.path(), b, false, false);
    let e = run_update_opts(&update_opts(dir.path(), other(b), false, false))
        .expect_err("a conflicting --store backend must fail");
    assert!(
        format!("{e:#}").contains("conflicts with the requested --store"),
        "{e:#}"
    );
}

fn update_with_external_manifest_flags_nothing(b: Backend) {
    let dir = tree(&[("d/f", "x")]);
    let outside = tempfile::tempdir().unwrap();
    let manifest = outside.path().join("m");
    let mut opts = update_opts(dir.path(), b, false, false);
    opts.common.manifest = Some(manifest.clone());
    let (outcome, _) = run_update_opts(&opts).unwrap();
    assert_eq!(outcome, Outcome::Clean);
    let st = store::open(&manifest, b, OpenMode::ReadOnly).unwrap();
    assert_eq!(keys(&*st), sorted(vec![".", "d", "d/f"]));
    st.for_each(&mut |_, e| {
        assert!(!e.flags.contains(Flags::STORE));
        Ok(())
    })
    .unwrap();
}

fn lines(out: &str) -> Vec<&str> {
    out.lines().collect()
}

fn check_clean_after_update(b: Backend) {
    let dir = tree(&[("d/f", "x")]);
    run_update(dir.path(), b, false, false);
    let (outcome, out, err) = run_check(dir.path(), Tier::Default, &[], &[], false);
    assert_eq!(out, "");
    assert_eq!(err, "added=0 deleted=0 changed=0 unchanged=4 warnings=0\n");
    assert_eq!(outcome, Outcome::Clean);
}

fn check_reports_add_del_chg(b: Backend) {
    let dir = tree(&[("d/f", "x"), ("m", "m"), ("x", "1")]);
    let m = dir.path().join("m");
    chmod(&m, 0o644);
    run_update(dir.path(), b, false, false);
    std::fs::write(dir.path().join("n"), "new").unwrap();
    // Removing d/f bumps d's mtime; restore it so only the listed lines appear.
    let d = dir.path().join("d");
    let d_mtime = std::fs::metadata(&d).unwrap().modified().unwrap();
    std::fs::remove_file(d.join("f")).unwrap();
    set_mtime_ro(&d, d_mtime);
    std::fs::write(dir.path().join("x"), "12").unwrap();
    chmod(&m, 0o600);

    let (outcome, out, err) = run_check(dir.path(), Tier::Default, &[], &[], false);
    let l = lines(&out);
    assert_eq!(l.len(), 4, "{out}");
    // Walk order is sorted: m, n, x; deletions follow the walk.
    assert_eq!(l[0], "CHG m mode=0644→0600");
    assert_eq!(l[1], "ADD n kind=file");
    let old = fmt_blake3(blake3::hash(b"1").as_bytes());
    let new = fmt_blake3(blake3::hash(b"12").as_bytes());
    assert!(
        l[2].starts_with(&format!("CHG x blake3={old}→{new} mtime=")),
        "{}",
        l[2]
    );
    assert!(l[2].ends_with(" size=1→2"), "{}", l[2]);
    assert_eq!(l[3], "DEL d/f");
    assert_eq!(err, "added=1 deleted=1 changed=2 unchanged=3 warnings=0\n");
    assert_eq!(outcome, Outcome::Differences);
}

fn check_relaxed_ignores_mode_and_mtime(b: Backend) {
    let dir = tree(&[("f", "x")]);
    let f = dir.path().join("f");
    chmod(&f, 0o644);
    run_update(dir.path(), b, false, false);
    chmod(&f, 0o600);
    set_mtime(&f, 1_000_000, 0);
    let (outcome, out, _) = run_check(dir.path(), Tier::Relaxed, &[], &[], false);
    assert_eq!(out, "");
    assert_eq!(outcome, Outcome::Clean);
    let (outcome, _, _) = run_check(dir.path(), Tier::Default, &[], &[], false);
    assert_eq!(outcome, Outcome::Differences);
}

fn check_paranoid_reports_nlink(b: Backend) {
    let dir = tree(&[("f", "x")]);
    run_update(dir.path(), b, false, false);
    std::fs::hard_link(dir.path().join("f"), dir.path().join("g")).unwrap();
    let (outcome, out, _) = run_check(dir.path(), Tier::Paranoid, &[], &[], false);
    assert_eq!(lines(&out), vec!["CHG f nlink=1→2", "ADD g kind=file"]);
    assert_eq!(outcome, Outcome::Differences);
}

fn check_ignore_mode_hides_chmod(b: Backend) {
    let dir = tree(&[("f", "x")]);
    let f = dir.path().join("f");
    chmod(&f, 0o644);
    run_update(dir.path(), b, false, false);
    chmod(&f, 0o600);
    let (outcome, out, _) = run_check(dir.path(), Tier::Default, &[Field::Mode], &[], false);
    assert_eq!(out, "");
    assert_eq!(outcome, Outcome::Clean);
}

fn check_require_uid_stays_clean_same_user(b: Backend) {
    let dir = tree(&[("f", "x")]);
    run_update(dir.path(), b, false, false);
    let (outcome, out, err) = run_check(dir.path(), Tier::Relaxed, &[], &[Field::Uid], false);
    assert_eq!(out, "");
    assert_eq!(err, "added=0 deleted=0 changed=0 unchanged=3 warnings=0\n");
    assert_eq!(outcome, Outcome::Clean);
}

fn check_symlink_retarget(b: Backend) {
    let dir = tree(&[("f", "x"), ("g", "y")]);
    symlink(dir.path(), "l", "f");
    run_update(dir.path(), b, false, false);
    std::fs::remove_file(dir.path().join("l")).unwrap();
    symlink(dir.path(), "l", "g");
    // Recreating the link may give it a new mtime; that is the only other field allowed.
    let (outcome, out, _) = run_check(dir.path(), Tier::Default, &[], &[], false);
    let l = lines(&out);
    assert_eq!(l.len(), 1, "{out}");
    assert!(
        l[0] == "CHG l target=f→g" || l[0].starts_with("CHG l target=f→g mtime="),
        "{}",
        l[0]
    );
    assert_eq!(outcome, Outcome::Differences);
    let (_, out, _) = run_check(dir.path(), Tier::Default, &[Field::Mtime], &[], false);
    assert_eq!(out, "CHG l target=f→g\n");
}

fn check_kind_change(b: Backend) {
    let dir = tree(&[("f", "x")]);
    run_update(dir.path(), b, false, false);
    let f = dir.path().join("f");
    std::fs::remove_file(&f).unwrap();
    std::fs::create_dir(&f).unwrap();
    let (outcome, out, _) = run_check(dir.path(), Tier::Paranoid, &[], &[], false);
    assert_eq!(out, "CHG f kind=file→dir\n");
    assert_eq!(outcome, Outcome::Differences);
}

fn check_dir_replaced_by_file_deletes_children(b: Backend) {
    let dir = tree(&[("d/x", "x")]);
    run_update(dir.path(), b, false, false);
    let d = dir.path().join("d");
    std::fs::remove_dir_all(&d).unwrap();
    std::fs::write(&d, "now a file").unwrap();
    let (outcome, out, err) = run_check(dir.path(), Tier::Default, &[], &[], false);
    assert_eq!(out, "CHG d kind=dir→file\nDEL d/x\n");
    assert!(err.contains("deleted=1 changed=1 "), "{err}");
    assert!(err.ends_with(" warnings=0\n"), "{err}");
    assert_eq!(outcome, Outcome::Differences);
}

fn check_warns_no_stored_checksum(b: Backend) {
    let dir = tree(&[("f", "secret")]);
    let f = dir.path().join("f");
    chmod(&f, 0o000);
    if permissions_bypassed(&f) {
        return;
    }
    run_update(dir.path(), b, false, false);
    chmod(&f, 0o644);
    // The mode necessarily changed; this test is about the missing checksum.
    let (outcome, out, err) = run_check(dir.path(), Tier::Default, &[Field::Mode], &[], false);
    assert_eq!(out, "");
    assert_eq!(
        err,
        "WARN no stored checksum for f\nadded=0 deleted=0 changed=0 unchanged=3 warnings=1\n"
    );
    assert_eq!(outcome, Outcome::Clean);
    let (outcome, _, _) = run_check(dir.path(), Tier::Default, &[Field::Mode], &[], true);
    assert_eq!(outcome, Outcome::Differences);
}

fn check_unreadable_at_check_time(b: Backend) {
    let dir = tree(&[("f", "secret")]);
    let f = dir.path().join("f");
    run_update(dir.path(), b, false, false);
    chmod(&f, 0o000);
    if permissions_bypassed(&f) {
        return;
    }
    // chmod changes the mode too; ignore it so the blake3 field is the whole line.
    let (outcome, out, err) = run_check(dir.path(), Tier::Default, &[Field::Mode], &[], false);
    let hex = fmt_blake3(blake3::hash(b"secret").as_bytes());
    assert_eq!(out, format!("CHG f blake3={hex}→unreadable\n"));
    assert_eq!(err, "added=0 deleted=0 changed=1 unchanged=2 warnings=0\n");
    assert_eq!(outcome, Outcome::Differences);
}

fn check_missing_manifest_is_error(_b: Backend) {
    let dir = tree(&[("f", "x")]);
    let e = run_check_opts(&check_opts(dir.path(), Tier::Default, &[], &[], false))
        .expect_err("check without a manifest must fail");
    assert!(format!("{e:#}").contains(".fileset.sqlite"), "{e:#}");
}

fn check_with_external_manifest(b: Backend) {
    let dir = tree(&[("d/f", "x")]);
    let outside = tempfile::tempdir().unwrap();
    let manifest = outside.path().join("m");
    let mut uopts = update_opts(dir.path(), b, false, false);
    uopts.common.manifest = Some(manifest.clone());
    run_update_opts(&uopts).unwrap();
    let mut copts = check_opts(dir.path(), Tier::Default, &[], &[], false);
    copts.common.manifest = Some(manifest);
    let (outcome, out, err) = run_check_opts(&copts).unwrap();
    assert_eq!(out, "");
    assert_eq!(err, "added=0 deleted=0 changed=0 unchanged=3 warnings=0\n");
    assert_eq!(outcome, Outcome::Clean);
}

fn check_output_is_deterministic(b: Backend) {
    let dir = tree(&[("a/1", "1"), ("a/2", "2"), ("b/3", "3"), ("c", "c")]);
    run_update(dir.path(), b, false, false);
    std::fs::remove_file(dir.path().join("a/1")).unwrap();
    std::fs::remove_file(dir.path().join("b/3")).unwrap();
    std::fs::write(dir.path().join("c"), "changed").unwrap();
    std::fs::write(dir.path().join("a/new"), "n").unwrap();
    std::fs::write(dir.path().join("z"), "z").unwrap();
    let (_, first, _) = run_check(dir.path(), Tier::Default, &[], &[], false);
    let (_, second, _) = run_check(dir.path(), Tier::Default, &[], &[], false);
    assert!(
        first.contains("DEL a/1\n") && first.contains("ADD z kind=file\n"),
        "{first}"
    );
    assert_eq!(first, second);
}

fn check_unreadable_directory_is_a_difference(b: Backend) {
    let dir = tree(&[("d/f", "x"), ("e", "e")]);
    run_update(dir.path(), b, false, false);
    let d = dir.path().join("d");
    // Corrupt the content, then make the directory searchable but not listable.
    std::fs::write(d.join("f"), "y").unwrap();
    chmod(&d, 0o311);
    if std::fs::read_dir(&d).is_ok() {
        chmod(&d, 0o755);
        eprintln!("skipping: running with privileges that bypass directory permissions");
        return;
    }
    let result = run_check_opts(&check_opts(dir.path(), Tier::Relaxed, &[], &[], false));
    chmod(&d, 0o755);
    let (outcome, out, err) = result.unwrap();
    assert_eq!(out, "CHG d/f kind=file→unverified\n");
    assert!(err.starts_with("WARN cannot read directory d: "), "{err}");
    assert!(
        err.ends_with("\nadded=0 deleted=0 changed=1 unchanged=4 warnings=1\n"),
        "{err}"
    );
    assert_eq!(outcome, Outcome::Differences);
}
