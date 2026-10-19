#![allow(dead_code)]

use fileset::engine::update::{update, UpdateOpts};
use fileset::engine::{CommonOpts, Outcome};
use fileset::progress::NoProgress;
use fileset::store::{self, Backend, OpenMode, Store};
use std::fs::{File, FileTimes};
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;
use std::time::{Duration, UNIX_EPOCH};
use tempfile::TempDir;

/// Creates a temp dir holding `spec` files (`"a/b.txt" => content`); parents are created.
pub fn tree(spec: &[(&str, &str)]) -> TempDir {
    let dir = tempfile::tempdir().unwrap();
    for (rel, content) in spec {
        let p = dir.path().join(rel);
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(&p, content).unwrap();
    }
    dir
}

pub fn symlink(dir: &Path, name: &str, target: &str) {
    std::os::unix::fs::symlink(target, dir.join(name)).unwrap();
}

pub fn chmod(path: &Path, mode: u32) {
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
}

pub fn set_mtime(path: &Path, sec: u64, nsec: u32) {
    let t = UNIX_EPOCH + Duration::new(sec, nsec);
    File::options()
        .write(true)
        .open(path)
        .unwrap()
        .set_times(FileTimes::new().set_modified(t))
        .unwrap();
}

pub fn mkfifo(path: &Path) {
    let status = Command::new("mkfifo").arg(path).status().unwrap();
    assert!(status.success(), "mkfifo {} failed", path.display());
}

pub fn update_opts(root: &Path, backend: Backend, rehash: bool, fail_on_warnings: bool) -> UpdateOpts {
    UpdateOpts {
        common: CommonOpts {
            root: root.to_path_buf(),
            manifest: None,
            cross_mount_points: false,
            sort: true,
            jobs: 2,
            fail_on_warnings,
        },
        backend: Some(backend),
        rehash,
    }
}

/// Runs `update`; returns the outcome and captured stderr. Asserts nothing went to stdout.
pub fn run_update_opts(opts: &UpdateOpts) -> anyhow::Result<(Outcome, String)> {
    let mut out = Vec::new();
    let mut err = Vec::new();
    let outcome = update(opts, &mut out, &mut err, &NoProgress)?;
    assert!(out.is_empty(), "update wrote to stdout: {}", String::from_utf8_lossy(&out));
    Ok((outcome, String::from_utf8(err).unwrap()))
}

pub fn run_update(root: &Path, backend: Backend, rehash: bool, fail_on_warnings: bool) -> (Outcome, String) {
    run_update_opts(&update_opts(root, backend, rehash, fail_on_warnings)).unwrap()
}

pub fn store_name(backend: Backend) -> &'static str {
    match backend {
        Backend::Sqlite => store::SQLITE_NAME,
        Backend::Fjall => store::FJALL_NAME,
    }
}

pub fn open_ro(root: &Path, backend: Backend) -> Box<dyn Store> {
    store::open(&root.join(store_name(backend)), backend, OpenMode::ReadOnly).unwrap()
}

/// All keys of `store`, in store order.
pub fn keys(store: &dyn Store) -> Vec<String> {
    let mut keys = Vec::new();
    store
        .for_each(&mut |k, _| {
            keys.push(String::from_utf8(k.to_vec()).unwrap());
            Ok(())
        })
        .unwrap();
    keys
}
