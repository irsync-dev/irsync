mod common;

use common::*;
use fileset::compare::{Field, Tier};
use fileset::engine::update::{update, Mode};
use fileset::engine::Outcome;
use fileset::model::{Flags, Kind};
use fileset::progress::NoProgress;
use fileset::report::fmt_blake3;
use fileset::store::{self, Backend, OpenMode};
use std::os::unix::fs::PermissionsExt;
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

/// macOS also refuses to read the xattrs of an unreadable file or directory, which adds a
/// warning Linux doesn't give. Drops those lines and takes them off the `warnings=` count.
fn without_macos_xattr_warnings(err: &str) -> String {
    if !cfg!(target_os = "macos") {
        return err.to_string();
    }
    let mut dropped = 0;
    let mut kept = String::new();
    for line in err.split_inclusive('\n') {
        if line.starts_with("WARN cannot read xattrs of ") {
            dropped += 1;
        } else {
            kept.push_str(line);
        }
    }
    match kept.rfind(" warnings=") {
        Some(i) => {
            let start = i + " warnings=".len();
            let end = start + kept[start..].find('\n').unwrap_or(kept.len() - start);
            let n: usize = kept[start..end].parse().unwrap();
            kept.replace_range(start..end, &(n - dropped).to_string());
            kept
        }
        None => kept,
    }
}

for_both_backends! {
    update_records_every_kind => update_records_every_kind_sqlite, update_records_every_kind_fjall;
    update_reuses_hash_when_unchanged => update_reuses_hash_when_unchanged_sqlite, update_reuses_hash_when_unchanged_fjall;
    update_rehash_forces => update_rehash_forces_sqlite, update_rehash_forces_fjall;
    update_rehashes_when_mtime_changes => update_rehashes_when_mtime_changes_sqlite, update_rehashes_when_mtime_changes_fjall;
    update_sweeps_stale_generation => update_sweeps_stale_generation_sqlite, update_sweeps_stale_generation_fjall;
    update_warns_on_unreadable => update_warns_on_unreadable_sqlite, update_warns_on_unreadable_fjall;
    update_warns_on_nested_manifest => update_warns_on_nested_manifest_sqlite, update_warns_on_nested_manifest_fjall;
    init_rejects_wrong_store_flag => init_rejects_wrong_store_flag_sqlite, init_rejects_wrong_store_flag_fjall;
    init_refuses_existing_manifest => init_refuses_existing_manifest_sqlite, init_refuses_existing_manifest_fjall;
    init_refuses_nested_manifest => init_refuses_nested_manifest_sqlite, init_refuses_nested_manifest_fjall;
    init_refuses_enclosing_manifest => init_refuses_enclosing_manifest_sqlite, init_refuses_enclosing_manifest_fjall;
    update_requires_existing_manifest => update_requires_existing_manifest_sqlite, update_requires_existing_manifest_fjall;
    update_requires_existing_external_manifest => update_requires_existing_external_manifest_sqlite, update_requires_existing_external_manifest_fjall;
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
    check_reports_entries_under_a_directory_turned_symlink_as_deleted => check_reports_entries_under_a_directory_turned_symlink_as_deleted_sqlite, check_reports_entries_under_a_directory_turned_symlink_as_deleted_fjall;
    init_replaces_the_manifest_an_interrupted_init_left => init_replaces_the_manifest_an_interrupted_init_left_sqlite, init_replaces_the_manifest_an_interrupted_init_left_fjall;
    update_refuses_the_manifest_an_interrupted_init_left => update_refuses_the_manifest_an_interrupted_init_left_sqlite, update_refuses_the_manifest_an_interrupted_init_left_fjall;
    check_no_stored_checksum_warning_needs_blake3_and_a_file => check_no_stored_checksum_warning_needs_blake3_and_a_file_sqlite, check_no_stored_checksum_warning_needs_blake3_and_a_file_fjall;
    check_unopenable_manifest_suggests_manifest_flag => check_unopenable_manifest_suggests_manifest_flag_sqlite, check_unopenable_manifest_suggests_manifest_flag_fjall;
    update_sweeps_entries_of_interrupted_run => update_sweeps_entries_of_interrupted_run_sqlite, update_sweeps_entries_of_interrupted_run_fjall;
    external_manifest_ignores_root_level_manifest_names => external_manifest_ignores_root_level_manifest_names_sqlite, external_manifest_ignores_root_level_manifest_names_fjall;
    in_tree_manifest_with_custom_name_is_clean => in_tree_manifest_with_custom_name_is_clean_sqlite, in_tree_manifest_with_custom_name_is_clean_fjall;
    check_require_uid_reports_a_changed_owner => check_require_uid_reports_a_changed_owner_sqlite, check_require_uid_reports_a_changed_owner_fjall;
    update_stores_a_tree_of_exactly_one_batch => update_stores_a_tree_of_exactly_one_batch_sqlite, update_stores_a_tree_of_exactly_one_batch_fjall;
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
    let err = without_macos_xattr_warnings(&err);
    assert_eq!(outcome, Outcome::Clean);
    assert!(err.starts_with("WARN cannot hash f: "), "{err}");
    // A file that could not be read was not hashed.
    assert!(
        err.ends_with("\nentries=3 hashed=0 reused=0 warnings=1\n"),
        "{err}"
    );
    let e = open_ro(dir.path(), b).get(b"f").unwrap().unwrap();
    assert!(e.flags.contains(Flags::UNREADABLE));
    assert_eq!(e.blake3, None);

    let (outcome, _) = run_update(dir.path(), b, false, true);
    assert_eq!(outcome, Outcome::Differences);
}

fn update_warns_on_nested_manifest(b: Backend) {
    let dir = tree(&[("f", "x")]);
    run_update(dir.path(), b, false, false);
    std::fs::create_dir(dir.path().join("sub")).unwrap();
    std::fs::write(dir.path().join("sub/.fileset.sqlite"), "not a manifest").unwrap();
    let (outcome, err) = run_update(dir.path(), b, false, false);
    assert_eq!(outcome, Outcome::Clean);
    assert!(
        err.starts_with("WARN nested manifest at sub/.fileset.sqlite\n"),
        "{err}"
    );
    let (outcome, _) = run_update(dir.path(), b, false, true);
    assert_eq!(outcome, Outcome::Differences);
}

fn init_rejects_wrong_store_flag(b: Backend) {
    let dir = tree(&[("f", "x")]);
    run_update(dir.path(), b, false, false);
    let e = run_update_opts(&update_opts(dir.path(), other(b), false, false))
        .expect_err("a conflicting --store backend must fail");
    assert!(
        format!("{e:#}").contains("conflicts with the requested --store"),
        "{e:#}"
    );
}

fn init_refuses_existing_manifest(b: Backend) {
    let dir = tree(&[("f", "x")]);
    run_update(dir.path(), b, false, false);
    let before = keys(&*open_ro(dir.path(), b));
    let mut opts = update_opts(dir.path(), b, false, false);
    opts.mode = Mode::Init { backend: None };
    let e = run_update_opts(&opts).expect_err("init over an existing manifest must fail");
    let msg = format!("{e:#}");
    assert!(
        msg.contains("already exists") && msg.contains("fileset update"),
        "{msg}"
    );
    assert_eq!(
        keys(&*open_ro(dir.path(), b)),
        before,
        "init touched the manifest"
    );
}

fn init_refuses_nested_manifest(b: Backend) {
    // One nested manifest of each backend's shape: a file and a directory.
    for nested in [
        ("sub/.fileset.sqlite", "not a manifest"),
        ("sub/.fileset.fjall/version", "3"),
    ] {
        let dir = tree(&[("f", "x"), nested]);
        let e = run_update_opts(&update_opts(dir.path(), b, false, false))
            .expect_err("init over a tree with a nested manifest must fail");
        let msg = format!("{e:#}");
        let nested_dir = nested.0.rsplit_once('/').unwrap().0;
        assert!(
            msg.contains("refusing to init")
                && msg.contains(&format!("nested manifest at {nested_dir}"))
                && msg.contains("nested filesets are not supported"),
            "{msg}"
        );
        // The refused init leaves nothing behind at the root, sidecars included.
        let left: Vec<String> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|d| d.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|n| n.starts_with(".fileset"))
            .collect();
        assert!(left.is_empty(), "left behind: {left:?}");
        // The nested manifest itself is untouched.
        assert!(dir.path().join(nested.0).symlink_metadata().is_ok());
    }
}

fn init_refuses_enclosing_manifest(b: Backend) {
    let dir = tree(&[("sub/deeper/f", "x")]);
    run_update(dir.path(), b, false, false);
    for rel in ["sub", "sub/deeper"] {
        let child = dir.path().join(rel);
        let e = run_update_opts(&update_opts(&child, b, false, false))
            .expect_err("init inside an existing fileset must fail");
        let msg = format!("{e:#}");
        assert!(
            msg.contains("refusing to init")
                && msg.contains("inside the fileset")
                && msg.contains(store_name(b))
                && msg.contains("nested filesets are not supported"),
            "{msg}"
        );
        assert!(
            child.join(store_name(b)).symlink_metadata().is_err(),
            "init created a manifest in {rel}"
        );
    }
    // The enclosing fileset is unaffected.
    let (outcome, out, _) = run_check(dir.path(), Tier::Default, &[], &[], false);
    assert_eq!(out, "");
    assert_eq!(outcome, Outcome::Clean);
}

fn update_requires_existing_manifest(b: Backend) {
    let dir = tree(&[("f", "x")]);
    let mut opts = update_opts(dir.path(), b, false, false);
    opts.mode = Mode::Update { rehash: false };
    let e = run_update_opts(&opts).expect_err("update without a manifest must fail");
    let msg = format!("{e:#}");
    assert!(
        msg.contains("no manifest found")
            && msg.contains(".fileset.sqlite")
            && msg.contains(".fileset.fjall")
            && msg.contains("fileset init"),
        "{msg}"
    );
    assert!(
        dir.path().join(store_name(b)).symlink_metadata().is_err(),
        "update created a manifest"
    );
}

fn update_requires_existing_external_manifest(b: Backend) {
    let dir = tree(&[("f", "x")]);
    let outside = tempfile::tempdir().unwrap();
    let manifest = outside.path().join("m");
    let mut opts = update_opts(dir.path(), b, false, false);
    opts.common.manifest = Some(manifest.clone());
    opts.mode = Mode::Update { rehash: false };
    let e = run_update_opts(&opts).expect_err("update without a manifest must fail");
    let msg = format!("{e:#}");
    assert!(
        msg.contains("no manifest at") && msg.contains("fileset init"),
        "{msg}"
    );
    assert!(
        manifest.symlink_metadata().is_err(),
        "update created a manifest"
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
    // An explicit mtime, so the change shows even where timestamps are coarse.
    set_mtime(&dir.path().join("x"), 1_000_000, 0);
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

fn check_require_uid_reports_a_changed_owner(b: Backend) {
    let dir = tree(&[("f", "x")]);
    run_update(dir.path(), b, false, false);
    // Ownership cannot be changed by an unprivileged test; the manifest side can.
    let path = dir.path().join(store_name(b));
    let mut st = store::open(&path, b, OpenMode::ReadWrite).unwrap();
    let mut f = st.get(b"f").unwrap().unwrap();
    let real = f.uid;
    f.uid = real.wrapping_add(1);
    st.put_batch(&[(b"f".to_vec(), f)]).unwrap();
    st.close().unwrap();
    let (outcome, out, _) = run_check(dir.path(), Tier::Default, &[], &[], false);
    assert_eq!(out, "");
    assert_eq!(outcome, Outcome::Clean);
    let (outcome, out, _) = run_check(dir.path(), Tier::Default, &[], &[Field::Uid], false);
    assert_eq!(out, format!("CHG f uid={}→{real}\n", real.wrapping_add(1)));
    assert_eq!(outcome, Outcome::Differences);
}

/// Root, manifest and 1998 files: exactly `BATCH_SIZE` entries, so the batch is flushed
/// at the boundary and nothing is left for the final flush.
fn update_stores_a_tree_of_exactly_one_batch(b: Backend) {
    let names: Vec<String> = (0..1998).map(|i| format!("f{i:04}")).collect();
    let spec: Vec<(&str, &str)> = names.iter().map(|n| (n.as_str(), "")).collect();
    let dir = tree(&spec);
    let (outcome, err) = run_update(dir.path(), b, false, false);
    assert_eq!(err, "entries=2000 hashed=1998 reused=0 warnings=0\n");
    assert_eq!(outcome, Outcome::Clean);
    assert_eq!(keys(&*open_ro(dir.path(), b)).len(), 2000);
    let (outcome, out, _) = run_check(dir.path(), Tier::Default, &[], &[], false);
    assert_eq!(out, "");
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
    assert_eq!(
        without_macos_xattr_warnings(&err),
        "added=0 deleted=0 changed=1 unchanged=2 warnings=0\n"
    );
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
    let err = without_macos_xattr_warnings(&err);
    assert_eq!(out, "CHG d/f kind=file→unverified\n");
    assert!(err.starts_with("WARN cannot read directory d: "), "{err}");
    assert!(
        err.ends_with("\nadded=0 deleted=0 changed=1 unchanged=4 warnings=1\n"),
        "{err}"
    );
    assert_eq!(outcome, Outcome::Differences);
}

/// Accepts nothing: the first warning aborts the run that writes to it.
struct FailingWriter;

impl std::io::Write for FailingWriter {
    fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
        Err(std::io::Error::other("stderr closed"))
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn update_sweeps_entries_of_interrupted_run(b: Backend) {
    // More entries than one put_batch, then a warning (nested manifest under "zz") whose
    // write fails, so the run dies after committing a batch and before finishing.
    let names: Vec<String> = (0..2000).map(|i| format!("f{i:04}")).collect();
    let spec: Vec<(&str, &str)> = names.iter().map(|n| (n.as_str(), "x")).collect();
    let dir = tree(&spec);
    run_update(dir.path(), b, false, false);
    std::fs::create_dir(dir.path().join("zz")).unwrap();
    std::fs::write(dir.path().join("zz/.fileset.sqlite"), "not a manifest").unwrap();
    let err = update(
        &update_opts(dir.path(), b, false, false),
        &mut Vec::new(),
        &mut FailingWriter,
        &NoProgress,
    )
    .expect_err("a failing warning write must abort update");
    assert!(format!("{err:#}").contains("stderr closed"), "{err:#}");

    // f0000 was committed by the interrupted run; it must not survive the next one.
    std::fs::remove_file(dir.path().join("f0000")).unwrap();
    run_update(dir.path(), b, false, false);
    let k = keys(&*open_ro(dir.path(), b));
    assert!(!k.contains(&"f0000".to_string()), "ghost f0000 survived");
    assert_eq!(k.len(), 2000 + 3, "{k:?}");
    let (outcome, out, _) = run_check(dir.path(), Tier::Default, &[], &[], false);
    assert_eq!(out, "");
    assert_eq!(outcome, Outcome::Clean);
}

fn external_manifest_ignores_root_level_manifest_names(b: Backend) {
    // The read-only workflow: the tree still holds its own manifest (here stand-ins with the
    // default names) while --manifest points at a copy elsewhere.
    let dir = tree(&[
        (".fileset.sqlite", "in-tree"),
        (".fileset.fjall", "in-tree"),
        ("f", "x"),
    ]);
    let outside = tempfile::tempdir().unwrap();
    let manifest = outside.path().join("m");
    let mut uopts = update_opts(dir.path(), b, false, true);
    uopts.common.manifest = Some(manifest.clone());
    // The stand-ins make the helper pick `update`; the external manifest is new.
    uopts.mode = Mode::Init { backend: Some(b) };
    let (outcome, err) = run_update_opts(&uopts).unwrap();
    assert_eq!(err, "entries=4 hashed=3 reused=0 warnings=0\n");
    assert_eq!(outcome, Outcome::Clean);
    let mut copts = check_opts(dir.path(), Tier::Default, &[], &[], true);
    copts.common.manifest = Some(manifest);
    let (outcome, out, err) = run_check_opts(&copts).unwrap();
    assert_eq!(out, "");
    assert_eq!(err, "added=0 deleted=0 changed=0 unchanged=4 warnings=0\n");
    assert_eq!(outcome, Outcome::Clean);
}

fn in_tree_manifest_with_custom_name_is_clean(b: Backend) {
    let dir = tree(&[("sub/g", "g"), ("f", "x")]);
    let name = match b {
        Backend::Sqlite => "m.sqlite",
        Backend::Fjall => "m.fjall",
    };
    let manifest = dir.path().join("sub").join(name);
    let mut uopts = update_opts(dir.path(), b, false, true);
    uopts.common.manifest = Some(manifest.clone());
    let (outcome, err) = run_update_opts(&uopts).unwrap();
    assert!(err.ends_with(" warnings=0\n"), "{err}");
    assert_eq!(outcome, Outcome::Clean);
    let st = store::open(&manifest, b, OpenMode::ReadOnly).unwrap();
    let mut k = keys(&*st);
    drop(st);
    k.sort();
    assert_eq!(
        k,
        sorted(vec![".", "f", "sub", "sub/g", &format!("sub/{name}")])
    );

    let mut copts = check_opts(dir.path(), Tier::Default, &[], &[], true);
    copts.common.manifest = Some(manifest);
    let (outcome, out, err) = run_check_opts(&copts).unwrap();
    assert_eq!(out, "");
    assert_eq!(err, "added=0 deleted=0 changed=0 unchanged=5 warnings=0\n");
    assert_eq!(outcome, Outcome::Clean);
}

/// Recursion depth equals tree depth; the walk thread's stack must hold it. 2000 levels of
/// one-letter names is as deep as an absolute path stays under PATH_MAX (4096). macOS caps
/// paths at 1024, so there the tree goes as deep as that allows.
#[test]
fn deep_tree_does_not_overflow_the_stack() {
    let dir = tempfile::tempdir().unwrap();
    let depth = if cfg!(target_os = "macos") {
        // Room for the base path, "/f" and the manifest name next to the deepest dir.
        (1024 - dir.path().as_os_str().len() - 32) / 2
    } else {
        2000
    };
    let mut deepest = dir.path().to_path_buf();
    for _ in 0..depth {
        deepest.push("d");
    }
    std::fs::create_dir_all(&deepest).unwrap();
    std::fs::write(deepest.join("f"), "x").unwrap();
    let (outcome, err) = run_update(dir.path(), Backend::Sqlite, false, false);
    assert_eq!(
        err,
        format!("entries={} hashed=1 reused=0 warnings=0\n", depth + 3)
    );
    assert_eq!(outcome, Outcome::Clean);
    let (outcome, out, _) = run_check(dir.path(), Tier::Default, &[], &[], false);
    assert_eq!(out, "");
    assert_eq!(outcome, Outcome::Clean);
}

fn check_no_stored_checksum_warning_needs_blake3_and_a_file(b: Backend) {
    let dir = tree(&[("f", "secret")]);
    let f = dir.path().join("f");
    chmod(&f, 0o000);
    if permissions_bypassed(&f) {
        return;
    }
    run_update(dir.path(), b, false, false);
    chmod(&f, 0o644);
    // Content is not compared: no checksum is missed.
    let (outcome, out, err) = run_check(
        dir.path(),
        Tier::Default,
        &[Field::Mode, Field::Blake3],
        &[],
        true,
    );
    assert_eq!(out, "");
    assert_eq!(err, "added=0 deleted=0 changed=0 unchanged=3 warnings=0\n");
    assert_eq!(outcome, Outcome::Clean);
    // No longer a file: the kind change is the report, not a missing checksum.
    std::fs::remove_file(&f).unwrap();
    std::fs::create_dir(&f).unwrap();
    let (_, out, err) = run_check(dir.path(), Tier::Default, &[], &[], false);
    assert_eq!(out, "CHG f kind=file→dir\n");
    assert!(!err.contains("WARN"), "{err}");
}

fn check_unopenable_manifest_suggests_manifest_flag(b: Backend) {
    let dir = tree(&[("f", "x")]);
    run_update(dir.path(), b, false, false);
    let m = dir.path().join(store_name(b));
    let mode = std::fs::metadata(&m).unwrap().permissions().mode();
    chmod(&m, 0o000);
    let readable = match b {
        Backend::Sqlite => std::fs::read(&m).is_ok(),
        Backend::Fjall => std::fs::read_dir(&m).is_ok(),
    };
    if readable {
        chmod(&m, mode);
        eprintln!("skipping: running with privileges that bypass file permissions");
        return;
    }
    let result = run_check_opts(&check_opts(dir.path(), Tier::Default, &[], &[], false));
    chmod(&m, mode);
    let e = result.expect_err("an unopenable manifest must fail check");
    let text = format!("{e:#}");
    assert!(text.contains("cannot open manifest at "), "{text}");
    assert!(text.contains("--manifest"), "{text}");
}

fn check_reports_entries_under_a_directory_turned_symlink_as_deleted(b: Backend) {
    let dir = tree(&[("d/f", "x"), ("e/f", "x")]);
    run_update(dir.path(), b, false, false);
    // `d` becomes a symlink to a directory that has an `f` too: the walk does not
    // descend symlinks, so d/f is gone from the tree even though d/f resolves.
    std::fs::remove_dir_all(dir.path().join("d")).unwrap();
    symlink(dir.path(), "d", "e");
    let (outcome, out, err) = run_check(dir.path(), Tier::Relaxed, &[], &[], false);
    assert!(out.starts_with("CHG d kind=dir\u{2192}symlink"), "{out}");
    assert!(out.contains("\nDEL d/f\n"), "{out}");
    assert!(
        err.ends_with("added=0 deleted=1 changed=1 unchanged=4 warnings=0\n"),
        "{err}"
    );
    assert_eq!(outcome, Outcome::Differences);
}

/// A manifest whose init never finished: created and dropped, never closed.
fn leave_unfinished(root: &Path, b: Backend) {
    let s = store::open(&root.join(store_name(b)), b, OpenMode::Create).unwrap();
    assert!(s.meta().unwrap().unfinished);
    drop(s);
}

fn init_replaces_the_manifest_an_interrupted_init_left(b: Backend) {
    let dir = tree(&[("f", "x")]);
    leave_unfinished(dir.path(), b);
    let mut opts = update_opts(dir.path(), b, false, false);
    opts.mode = Mode::Init { backend: Some(b) };
    let (outcome, err) = run_update_opts(&opts).unwrap();
    assert_eq!(outcome, Outcome::Clean);
    assert!(
        err.starts_with("WARN replacing the unfinished manifest an interrupted init left at "),
        "{err}"
    );
    assert!(
        err.ends_with("\nentries=3 hashed=1 reused=0 warnings=1\n"),
        "{err}"
    );
    let s = open_ro(dir.path(), b);
    assert!(!s.meta().unwrap().unfinished);
    assert_eq!(keys(&*s), sorted(vec![".", "f", store_name(b)]));
}

fn update_refuses_the_manifest_an_interrupted_init_left(b: Backend) {
    let dir = tree(&[("f", "x")]);
    leave_unfinished(dir.path(), b);
    let opts = update_opts(dir.path(), b, false, false);
    assert!(matches!(opts.mode, Mode::Update { .. }));
    let e = run_update_opts(&opts).expect_err("an unfinished manifest is not updatable");
    let text = format!("{e:#}");
    assert!(
        text.contains("interrupted init") && text.contains("fileset init"),
        "{text}"
    );
}

for_both_backends! {
    update_skips_unreadable_builtin_exclude => update_skips_unreadable_builtin_exclude_sqlite, update_skips_unreadable_builtin_exclude_fjall;
    update_without_builtin_excludes_warns => update_without_builtin_excludes_warns_sqlite, update_without_builtin_excludes_warns_fjall;
    builtin_excludes_drop_quietly_from_an_old_manifest => builtin_excludes_drop_quietly_from_an_old_manifest_sqlite, builtin_excludes_drop_quietly_from_an_old_manifest_fjall;
}

/// A tree with an unreadable `.Spotlight-V100` at its root, as on a macOS volume. `None`
/// when running with privileges that bypass directory permissions.
fn unreadable_spotlight_tree() -> Option<tempfile::TempDir> {
    let dir = tree(&[("f", "x"), (".Spotlight-V100/store.db", "y")]);
    let spotlight = dir.path().join(".Spotlight-V100");
    chmod(&spotlight, 0o000);
    if std::fs::read_dir(&spotlight).is_ok() {
        chmod(&spotlight, 0o755);
        eprintln!("skipping: running with privileges that bypass directory permissions");
        return None;
    }
    Some(dir)
}

fn update_skips_unreadable_builtin_exclude(b: Backend) {
    let Some(dir) = unreadable_spotlight_tree() else {
        return;
    };
    let (outcome, err) = run_update(dir.path(), b, false, true);
    chmod(&dir.path().join(".Spotlight-V100"), 0o755);
    assert_eq!(outcome, Outcome::Clean, "{err}");
    assert!(!err.contains("WARN"), "{err}");
    assert!(!err.contains("INFO"), "{err}");
    assert!(err.ends_with(" warnings=0\n"), "{err}");
    let keys = keys(&*open_ro(dir.path(), b));
    assert!(
        !keys.iter().any(|k| k.starts_with(".Spotlight-V100")),
        "{keys:?}"
    );

    let mut opts = update_opts(dir.path(), b, false, true);
    opts.common.verbose = true;
    let (outcome, err) = run_update_opts(&opts).unwrap();
    assert_eq!(outcome, Outcome::Clean, "{err}");
    assert!(
        err.starts_with("INFO skipping built-in exclude .Spotlight-V100\n"),
        "{err}"
    );
    assert!(err.ends_with(" warnings=0\n"), "{err}");
}

fn update_without_builtin_excludes_warns(b: Backend) {
    let Some(dir) = unreadable_spotlight_tree() else {
        return;
    };
    let mut opts = update_opts(dir.path(), b, false, true);
    opts.common.builtin_excludes = false;
    let (outcome, err) = run_update_opts(&opts).unwrap();
    chmod(&dir.path().join(".Spotlight-V100"), 0o755);
    let err = without_macos_xattr_warnings(&err);
    assert_eq!(outcome, Outcome::Differences, "{err}");
    assert!(
        err.starts_with("WARN cannot read directory .Spotlight-V100: "),
        "{err}"
    );
}

fn builtin_excludes_drop_quietly_from_an_old_manifest(b: Backend) {
    // A manifest written before built-in excludes existed holds them, contents and all.
    let dir = tree(&[("f", "x"), (".Trashes/501/old", "y")]);
    let mut opts = update_opts(dir.path(), b, false, false);
    opts.common.builtin_excludes = false;
    run_update_opts(&opts).unwrap();
    assert!(keys(&*open_ro(dir.path(), b)).contains(&".Trashes/501/old".to_string()));

    // Now unreadable, as on macOS: what is stored below it can no longer be verified.
    let trashes = dir.path().join(".Trashes");
    chmod(&trashes, 0o000);
    if std::fs::read_dir(&trashes).is_ok() {
        chmod(&trashes, 0o755);
        eprintln!("skipping: running with privileges that bypass directory permissions");
        return;
    }
    let (outcome, out, err) = run_check(dir.path(), Tier::Default, &[], &[], true);
    assert_eq!(outcome, Outcome::Clean, "{out}{err}");
    assert_eq!(out, "");

    run_update(dir.path(), b, false, true);
    chmod(&trashes, 0o755);
    let keys = keys(&*open_ro(dir.path(), b));
    assert!(!keys.iter().any(|k| k.starts_with(".Trashes")), "{keys:?}");
    let (outcome, out, err) = run_check(dir.path(), Tier::Default, &[], &[], true);
    assert_eq!(outcome, Outcome::Clean, "{out}{err}");
}
