mod common;

use common::*;
use fileset::engine::Outcome;
use fileset::model::{Flags, Kind};
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
    assert_eq!(keys(&*store), sorted(vec![".", "d", "d/f", "l", "p", store_name(b)]));
    let f = store.get(b"d/f").unwrap().unwrap();
    assert_eq!(f.blake3, Some(*blake3::hash(b"x").as_bytes()));
    assert_eq!(store.get(b"l").unwrap().unwrap().target.as_deref(), Some(&b"f"[..]));
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
    assert_eq!(store.get(b"f").unwrap().unwrap().blake3, Some(*blake3::hash(b"x").as_bytes()));
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
    assert_eq!(keys(&*open_ro(dir.path(), b)), sorted(vec![".", "f", store_name(b)]));
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
    assert!(err.ends_with("\nentries=3 hashed=1 reused=0 warnings=1\n"), "{err}");
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
    assert!(err.starts_with("WARN nested manifest at sub/.fileset.sqlite\n"), "{err}");
    let (outcome, _) = run_update(dir.path(), b, false, true);
    assert_eq!(outcome, Outcome::Differences);
}

fn update_rejects_wrong_store_flag(b: Backend) {
    let dir = tree(&[("f", "x")]);
    run_update(dir.path(), b, false, false);
    let e = run_update_opts(&update_opts(dir.path(), other(b), false, false))
        .expect_err("a conflicting --store backend must fail");
    assert!(format!("{e:#}").contains("conflicts with the requested --store"), "{e:#}");
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
