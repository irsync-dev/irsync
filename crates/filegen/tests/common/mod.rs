//! Helpers shared by the integration tests (and, via `#[path]`, the
//! run-loop unit tests).
#![allow(dead_code)]

use std::fs;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{FileTypeExt, MetadataExt};
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Files larger than this are compared by size only.
const CONTENT_LIMIT: u64 = 1 << 20;

/// How far back from the snapshot an mtime counts as written by the
/// clock (an ordinary write during the test) rather than drawn by
/// `set_mtime`.
const CLOCK_WINDOW: Duration = Duration::from_secs(3600);

/// One entry: relative path, kind (`d f l p c b s ?`), size (0 for dirs),
/// symlink target, content of files up to 1 MiB, `mode & 0o7777`, and
/// mtime as `(seconds, nanoseconds)`.
///
/// The mtime is `None` when it lies within the hour before the snapshot
/// (or a minute after): such times come from the clock, not the seed, so
/// two runs of one seed never agree on them. Times drawn by `set_mtime`
/// are compared exactly.
pub type Entry = (
    Vec<u8>,
    char,
    u64,
    Option<Vec<u8>>,
    Option<Vec<u8>>,
    u32,
    Option<(i64, i64)>,
);

/// Every entry below `root` (not the root itself), sorted by path, never
/// following symlinks.
pub fn snapshot(root: &Path) -> Vec<Entry> {
    let now = SystemTime::now();
    let lo = secs(now - CLOCK_WINDOW);
    let hi = secs(now + Duration::from_secs(60));
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for e in fs::read_dir(&dir).unwrap() {
            let path = e.unwrap().path();
            let md = fs::symlink_metadata(&path).unwrap();
            let rel = path
                .strip_prefix(root)
                .unwrap()
                .as_os_str()
                .as_bytes()
                .to_vec();
            let ft = md.file_type();
            let kind = if ft.is_dir() {
                stack.push(path.clone());
                'd'
            } else if ft.is_file() {
                'f'
            } else if ft.is_symlink() {
                'l'
            } else if ft.is_fifo() {
                'p'
            } else if ft.is_char_device() {
                'c'
            } else if ft.is_block_device() {
                'b'
            } else if ft.is_socket() {
                's'
            } else {
                '?'
            };
            let size = if kind == 'd' { 0 } else { md.size() };
            let target = (kind == 'l').then(|| {
                fs::read_link(&path)
                    .unwrap()
                    .as_os_str()
                    .as_bytes()
                    .to_vec()
            });
            let content = (kind == 'f' && size <= CONTENT_LIMIT).then(|| fs::read(&path).unwrap());
            let mtime = (md.mtime(), md.mtime_nsec());
            let from_clock = (lo..=hi).contains(&mtime.0);
            out.push((
                rel,
                kind,
                size,
                target,
                content,
                md.mode() & 0o7777,
                (!from_clock).then_some(mtime),
            ));
        }
    }
    out.sort();
    out
}

fn secs(t: SystemTime) -> i64 {
    t.duration_since(UNIX_EPOCH).unwrap().as_secs() as i64
}
