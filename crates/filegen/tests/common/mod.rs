//! Helpers shared by the integration tests (and, via `#[path]`, the
//! run-loop unit tests).
#![allow(dead_code)]

use std::fs;
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

// `filegen` is this crate in the unit tests too: `lib.rs` names itself so.
use filegen::platform::{self, FileKind};

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
            let md = platform::lstat(&path).unwrap();
            let rel = platform::path_to_bytes(path.strip_prefix(root).unwrap());
            let kind = match md.kind {
                FileKind::Dir => {
                    stack.push(path.clone());
                    'd'
                }
                FileKind::File => 'f',
                FileKind::Symlink => 'l',
                FileKind::Fifo => 'p',
                FileKind::CharDev => 'c',
                FileKind::BlockDev => 'b',
                FileKind::Socket => 's',
                FileKind::Other => '?',
            };
            let size = if kind == 'd' { 0 } else { md.size };
            let target = (kind == 'l').then(|| platform::read_link(&path).unwrap());
            let content = (kind == 'f' && size <= CONTENT_LIMIT).then(|| fs::read(&path).unwrap());
            let mtime = (
                md.mtime_ns.div_euclid(1_000_000_000) as i64,
                md.mtime_ns.rem_euclid(1_000_000_000) as i64,
            );
            let from_clock = (lo..=hi).contains(&mtime.0);
            out.push((
                rel,
                kind,
                size,
                target,
                content,
                md.mode & 0o7777,
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
