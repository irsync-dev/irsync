//! A manifest can come with a tree someone else made. Opening it must not
//! reach outside the manifest, and must not run what its maker put in.

use fileset::platform::testutil::{symlink, symlinks_available};
use fileset::store::{self, Backend, OpenMode, SQLITE_NAME};
use std::path::Path;

/// A fresh, empty manifest of `backend` in `root`.
fn manifest(root: &Path, backend: Backend) -> std::path::PathBuf {
    let path = root.join(backend.file_name());
    store::open(&path, backend, OpenMode::Create)
        .unwrap()
        .close()
        .unwrap();
    path
}

fn open_err(path: &Path, backend: Backend, mode: OpenMode) -> String {
    match store::open(path, backend, mode) {
        Ok(_) => panic!("{} opened", path.display()),
        Err(e) => format!("{e:#}"),
    }
}

/// A manifest in the tree that is a symlink would have `update` rewrite a
/// manifest somewhere else. `--manifest` may still name a link.
#[test]
fn a_symlinked_manifest_in_the_tree_is_refused() {
    if !symlinks_available(tempfile::tempdir().unwrap().path()) {
        return;
    }
    for backend in [Backend::Sqlite, Backend::Fjall, Backend::Redb] {
        let (elsewhere, tree) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let real = manifest(elsewhere.path(), backend);
        let link = tree.path().join(real.file_name().unwrap());
        symlink(&real, &link).unwrap();
        for for_update in [false, true] {
            let err = store::locate(tree.path(), None, None, for_update)
                .unwrap_err()
                .to_string();
            assert!(err.contains("symbolic link"), "{err}");
        }
        let named = store::locate(tree.path(), Some(&link), None, false).unwrap();
        assert_eq!(named.backend, backend);
    }
}

/// fjall repairs a database when it opens it, even to read. A link inside
/// the manifest directory must not take that repair elsewhere.
#[test]
fn a_fjall_manifest_with_a_link_inside_is_refused() {
    if !symlinks_available(tempfile::tempdir().unwrap().path()) {
        return;
    }
    let listing = |dir: &Path| {
        let mut names: Vec<_> = std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        names.sort();
        names
    };
    for mode in [OpenMode::ReadOnly, OpenMode::ReadWrite] {
        // The keyspaces directory itself is a link to a directory of the user.
        let (tree, mine) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let path = manifest(tree.path(), Backend::Fjall);
        for year in ["2023", "2024"] {
            std::fs::create_dir(mine.path().join(year)).unwrap();
            std::fs::write(mine.path().join(year).join("photo"), b"keep").unwrap();
        }
        let before = listing(mine.path());
        std::fs::rename(path.join("keyspaces"), tree.path().join("moved")).unwrap();
        symlink(mine.path(), &path.join("keyspaces")).unwrap();
        let err = open_err(&path, Backend::Fjall, mode);
        assert!(err.contains("neither a file nor a directory"), "{err}");
        assert_eq!(listing(mine.path()), before);
        assert_eq!(
            std::fs::read(mine.path().join("2023/photo")).unwrap(),
            b"keep"
        );

        // A link deeper inside.
        let tree = tempfile::tempdir().unwrap();
        let path = manifest(tree.path(), Backend::Fjall);
        symlink(mine.path(), &path.join("keyspaces").join("7")).unwrap();
        let err = open_err(&path, Backend::Fjall, mode);
        assert!(err.contains("neither a file nor a directory"), "{err}");
        assert_eq!(listing(mine.path()), before);
    }
}

/// A SQLite manifest holds tables and indexes only: no view in place of a
/// table, no trigger that runs when `update` writes, no virtual table.
#[test]
fn a_sqlite_manifest_with_views_or_triggers_is_refused() {
    let cases = [
        ("CREATE VIEW extra AS SELECT 1", "view"),
        (
            "CREATE TRIGGER t AFTER INSERT ON entries BEGIN DELETE FROM meta; END",
            "trigger",
        ),
        (
            "ALTER TABLE entries RENAME TO real; CREATE VIEW entries AS SELECT * FROM real",
            "view",
        ),
        ("CREATE VIRTUAL TABLE v USING fts5(x)", "virtual table"),
        ("DROP TABLE xattrs", "no table named xattrs"),
    ];
    for (sql, needle) in cases {
        let tree = tempfile::tempdir().unwrap();
        let path = manifest(tree.path(), Backend::Sqlite);
        rusqlite::Connection::open(&path)
            .unwrap()
            .execute_batch(sql)
            .unwrap();
        for mode in [OpenMode::ReadOnly, OpenMode::ReadWrite] {
            let err = open_err(&path, Backend::Sqlite, mode);
            assert!(err.contains("not a fileset manifest"), "{sql}: {err}");
            assert!(err.contains(needle), "{sql}: {err}");
        }
    }
    // An untouched manifest still opens.
    let tree = tempfile::tempdir().unwrap();
    let path = manifest(tree.path(), Backend::Sqlite);
    store::open(&path, Backend::Sqlite, OpenMode::ReadOnly).unwrap();
    store::open(&path, Backend::Sqlite, OpenMode::ReadWrite).unwrap();
}

/// SQLite would follow a link at the name of its write-ahead log.
#[test]
fn a_sqlite_manifest_with_a_linked_log_beside_it_is_refused() {
    if !symlinks_available(tempfile::tempdir().unwrap().path()) {
        return;
    }
    for suffix in ["-wal", "-shm", "-journal"] {
        let (tree, mine) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let path = manifest(tree.path(), Backend::Sqlite);
        let victim = mine.path().join("victim");
        std::fs::write(&victim, b"keep").unwrap();
        let side = tree.path().join(format!("{SQLITE_NAME}{suffix}"));
        let _ = std::fs::remove_file(&side);
        symlink(&victim, &side).unwrap();
        for mode in [OpenMode::ReadOnly, OpenMode::ReadWrite] {
            let err = open_err(&path, Backend::Sqlite, mode);
            assert!(err.contains("not a regular file"), "{suffix}: {err}");
        }
        assert_eq!(std::fs::read(&victim).unwrap(), b"keep");
    }
}
