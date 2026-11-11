//! Parallel directory walk that streams entries in `RelPath` order.

use crate::filter::{FilterSet, Verdict};
use crate::fs::meta::lstat_entry_with_ctime;
use crate::fs::paths::RelPath;
use crate::telemetry::meter::{Activity, Meter};
use irsync_proto::v1::FileEntry;
use jwalk::{Parallelism, WalkDirGeneric};
use std::cell::RefCell;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::{mpsc, oneshot};

#[derive(Debug, Clone)]
pub struct ScanItem {
    pub path: RelPath,
    pub entry: FileEntry,
    pub ctime: (i64, u32),
}

/// What the walk streams, in `RelPath` order.
#[derive(Debug, Clone)]
pub enum ScanEvent {
    Entry(ScanItem),
    /// Something at or below `path` could not be listed, so entries are
    /// missing from here: a directory that could not be read (sent right
    /// after the directory's own entry) or an entry that could not be
    /// stat'ed. The walk goes on; the summary says `incomplete`.
    Gap {
        path: RelPath,
        message: String,
    },
}

/// Entries seen by kind, for `--stats`. Hard links count as regular files.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ListStats {
    pub reg: u64,
    pub dir: u64,
    pub link: u64,
    pub special: u64,
    /// Sum of the sizes of the regular files.
    pub total_size: u64,
}

impl ListStats {
    pub fn count(&mut self, e: &irsync_proto::v1::FileEntry) {
        use irsync_proto::v1::file_entry::Kind;
        match e.kind() {
            Kind::Regular | Kind::Hardlink => {
                self.reg += 1;
                self.total_size += e.size;
            }
            Kind::Directory => self.dir += 1,
            Kind::Symlink => self.link += 1,
            _ => self.special += 1,
        }
    }

    pub fn add(&mut self, o: &ListStats) {
        self.reg += o.reg;
        self.dir += o.dir;
        self.link += o.link;
        self.special += o.special;
        self.total_size += o.total_size;
    }

    pub fn files(&self) -> u64 {
        self.reg + self.dir + self.link + self.special
    }
}

#[derive(Debug, Clone, Default)]
pub struct ScanSummary {
    /// Filled by the session's listing (not by the walk itself).
    pub listed: ListStats,
    pub entries: u64,
    pub incomplete: bool,
    pub errors: Vec<(RelPath, String)>,
}

/// Threads a walk lists directories with. From a cold spinning disk more
/// directories in flight let it order its reads; warm, the count makes no
/// difference (`docs/IRSYNC_DECISIONS.md`, 2.1 item 7).
pub const WALK_THREADS: usize = 16;

#[derive(Debug, Clone, Copy)]
pub struct WalkOpts {
    /// Walk the target if the root itself is a symlink to a directory.
    pub follow_root_symlink: bool,
    pub threads: usize,
    /// Deepest entries to list (the root is depth 0); `None` for the whole tree.
    pub max_depth: Option<usize>,
}

fn rel_of(root: &Path, full: &Path) -> Option<RelPath> {
    let rest = full.strip_prefix(root).ok()?;
    RelPath::new(rest.as_os_str().as_bytes()).ok()
}

/// Walk `root`, sending every non-filtered entry (root first) in increasing `RelPath`
/// order, with a `Gap` at the position of whatever could not be listed.
pub fn walk(
    root: PathBuf,
    filters: Arc<FilterSet>,
    opts: WalkOpts,
    meter: Option<Arc<Meter>>,
) -> (mpsc::Receiver<ScanEvent>, oneshot::Receiver<ScanSummary>) {
    let (tx, rx) = mpsc::channel(1024);
    let (stx, srx) = oneshot::channel();
    // Not `spawn_blocking`: the walk waits on the channel for as long as it
    // runs (see `spawn_thread`).
    drop(spawn_thread("irsync-walk", move || {
        let summary = run_walk(root, filters, opts, tx, meter);
        let _ = stx.send(summary);
    }));
    (rx, srx)
}

/// Runs `f` on a thread of its own, not on tokio's blocking pool, and
/// returns a handle that yields its result (aborting the handle stops only
/// the waiting, as with `spawn_blocking`). For blocking work that lives as
/// long as a walk and waits on bounded channels whose consumers depend on
/// per-file blocking calls: on the pool, such tasks of concurrent sessions
/// took every thread (tokio's limit is 512) while those calls queued behind
/// them, and no session moved again. Must be called within a runtime.
pub fn spawn_thread<T: Send + 'static>(
    name: &str,
    f: impl FnOnce() -> T + Send + 'static,
) -> tokio::task::JoinHandle<T> {
    let (tx, rx) = oneshot::channel();
    let name = name.to_string();
    let spawned = std::thread::Builder::new()
        .name(name.clone())
        .spawn(move || {
            let _ = tx.send(f());
        });
    tokio::spawn(async move {
        if let Err(e) = spawned {
            panic!("cannot start thread {name}: {e}");
        }
        match rx.await {
            Ok(v) => v,
            Err(_) => panic!("thread {name} panicked"),
        }
    })
}

/// Puts a directory's children in file-name order, errors included: the
/// walk's sort puts every error after all the entries, but an error for a
/// child that could not be read (its file name known) becomes a gap that
/// must stand where the child would have, or the listing goes out of
/// order. Errors without a file name keep their place after the entries.
/// The sort is stable, so the entries keep the walk's order among equals.
fn sort_children<T, E>(
    children: &mut [Result<T, E>],
    name: impl Fn(&T) -> &[u8],
    err_name: impl Fn(&E) -> Option<&[u8]>,
) {
    // `false` sorts before `true`: nameless errors go last.
    let key = |c: &Result<T, E>| -> (bool, Vec<u8>) {
        match c {
            Ok(t) => (false, name(t).to_vec()),
            Err(e) => match err_name(e) {
                Some(n) => (false, n.to_vec()),
                None => (true, Vec::new()),
            },
        }
    };
    if children
        .iter()
        .any(|c| c.as_ref().err().is_some_and(|e| err_name(e).is_some()))
    {
        children.sort_by_cached_key(key);
    }
}

/// Whether a directory's name is that of a clone folder
/// ([`crate::fs::clone::CLONE_DIR_PREFIX`]), never listed.
pub fn is_clone_dir(name: &[u8]) -> bool {
    name.starts_with(crate::fs::clone::CLONE_DIR_PREFIX.as_bytes())
}

fn run_walk(
    mut root: PathBuf,
    filters: Arc<FilterSet>,
    opts: WalkOpts,
    tx: mpsc::Sender<ScanEvent>,
    meter: Option<Arc<Meter>>,
) -> ScanSummary {
    // Timed while the walk works; released while it waits for room in the channel.
    let timing = RefCell::new(meter.as_deref().map(|m| m.start(Activity::Walk)));
    let emit = |ev: ScanEvent| -> bool {
        timing.borrow_mut().take();
        let sent = tx.blocking_send(ev).is_ok();
        *timing.borrow_mut() = meter.as_deref().map(|m| m.start(Activity::Walk));
        sent
    };
    let mut sum = ScanSummary::default();
    if opts.follow_root_symlink && std::fs::metadata(&root).is_ok_and(|m| m.is_dir()) {
        if let Ok(real) = std::fs::canonicalize(&root) {
            root = real;
        }
    }
    // A gap is recorded and streamed; the stream stays in path order because
    // each gap is sent where its entries would have been.
    let gap = |sum: &mut ScanSummary, rel: RelPath, message: String| -> bool {
        sum.incomplete = true;
        sum.errors.push((rel.clone(), message.clone()));
        emit(ScanEvent::Gap { path: rel, message })
    };
    let send = |sum: &mut ScanSummary, rel: RelPath| -> bool {
        match lstat_entry_with_ctime(&root, &rel) {
            Ok((entry, ctime)) => {
                if !emit(ScanEvent::Entry(ScanItem {
                    path: rel,
                    entry,
                    ctime,
                })) {
                    return false;
                }
                sum.entries += 1;
                true
            }
            Err(e) => gap(sum, rel, e.to_string()),
        }
    };
    if !send(&mut sum, RelPath::root()) || sum.incomplete {
        // A root that cannot be stat'ed has its gap; the walk would only
        // report it again.
        return sum;
    }

    let cb_root = root.clone();
    let walker = WalkDirGeneric::<((), ())>::new(&root)
        .sort(true)
        .skip_hidden(false)
        .follow_links(false)
        .max_depth(opts.max_depth.unwrap_or(usize::MAX))
        .parallelism(Parallelism::RayonNewPool(opts.threads.max(1)))
        .process_read_dir(move |_, dir, _, children| {
            sort_children(
                children,
                |e| e.file_name().as_bytes(),
                |err| err.path().and_then(Path::file_name).map(|n| n.as_bytes()),
            );
            let Some(dir_rel) = rel_of(&cb_root, dir) else {
                return;
            };
            children.retain_mut(|c| {
                let Ok(e) = c else { return true };
                let Ok(rel) = dir_rel.join(e.file_name().as_bytes()) else {
                    // Kept, unwalked, for the loop below to report.
                    e.read_children = None;
                    return true;
                };
                if filters.matches(&rel, e.file_type().is_dir()) == Verdict::Exclude {
                    return false;
                }
                // `--paranoid`'s clone folders, with everything below them.
                !(e.file_type().is_dir() && is_clone_dir(e.file_name().as_bytes()))
            });
        });

    for item in walker {
        match item {
            Ok(e) => {
                let Some(rel) = rel_of(&root, &e.path()) else {
                    // A name no path can carry (the filesystem handed out
                    // `.`, `..`, or a name with `/` or NUL): missing from
                    // the listing, so a gap, at the root since it has no
                    // place in path order.
                    let msg = format!("{}: invalid file name", e.path().display());
                    if !gap(&mut sum, RelPath::root(), msg) {
                        return sum;
                    }
                    continue;
                };
                let unread = e
                    .read_children
                    .as_ref()
                    .and_then(|r| r.error())
                    .map(|err| err.to_string());
                if e.depth() > 0 && !send(&mut sum, rel.clone()) {
                    return sum;
                }
                // After the directory's own entry: everything below it is missing.
                if let Some(err) = unread {
                    if !gap(&mut sum, rel, err) {
                        return sum;
                    }
                }
            }
            Err(err) => {
                let rel = err
                    .path()
                    .and_then(|p| rel_of(&root, p))
                    .unwrap_or_default();
                if !gap(&mut sum, rel, err.to_string()) {
                    return sum;
                }
            }
        }
    }
    sum
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    /// Every event of a walk of `root`, as (path, is_gap), in stream order.
    async fn events(root: &Path) -> (Vec<(String, bool)>, ScanSummary) {
        let (mut rx, summary) = walk(
            root.to_path_buf(),
            Arc::new(FilterSet::new(vec![])),
            WalkOpts {
                follow_root_symlink: false,
                threads: 2,
                max_depth: None,
            },
            None,
        );
        let mut v = Vec::new();
        while let Some(ev) = rx.recv().await {
            v.push(match ev {
                ScanEvent::Entry(i) => (
                    String::from_utf8_lossy(i.path.as_bytes()).into_owned(),
                    false,
                ),
                ScanEvent::Gap { path, .. } => {
                    (String::from_utf8_lossy(path.as_bytes()).into_owned(), true)
                }
            });
        }
        (v, summary.await.unwrap())
    }

    #[test]
    fn an_error_for_a_named_child_sorts_into_its_place() {
        // As the walk hands them over: entries by name, then the errors.
        let mut children: Vec<Result<&str, Option<&str>>> = vec![
            Ok("a"),
            Ok("c"),
            Ok("z"),
            Err(Some("b")),
            Err(None),
            Err(Some("d")),
        ];
        sort_children(&mut children, |n| n.as_bytes(), |e| e.map(str::as_bytes));
        assert_eq!(
            children,
            vec![
                Ok("a"),
                Err(Some("b")),
                Ok("c"),
                Err(Some("d")),
                Ok("z"),
                Err(None),
            ]
        );
    }

    #[tokio::test]
    async fn an_unreadable_directory_is_a_gap_right_after_its_entry() {
        let t = tempfile::tempdir().unwrap();
        std::fs::create_dir(t.path().join("a")).unwrap();
        std::fs::write(t.path().join("a/x"), b"").unwrap();
        std::fs::create_dir(t.path().join("locked")).unwrap();
        std::fs::write(t.path().join("locked/secret"), b"").unwrap();
        std::fs::write(t.path().join("z"), b"").unwrap();
        std::fs::set_permissions(
            t.path().join("locked"),
            std::fs::Permissions::from_mode(0o000),
        )
        .unwrap();
        if std::fs::read_dir(t.path().join("locked")).is_ok() {
            std::fs::set_permissions(
                t.path().join("locked"),
                std::fs::Permissions::from_mode(0o755),
            )
            .unwrap();
            eprintln!("skipping: running with privileges that bypass directory permissions");
            return;
        }
        let (v, summary) = events(t.path()).await;
        std::fs::set_permissions(
            t.path().join("locked"),
            std::fs::Permissions::from_mode(0o755),
        )
        .unwrap();
        let want: Vec<(String, bool)> = [
            ("", false),
            ("a", false),
            ("a/x", false),
            ("locked", false),
            ("locked", true),
            ("z", false),
        ]
        .into_iter()
        .map(|(p, g)| (p.to_string(), g))
        .collect();
        assert_eq!(v, want);
        assert!(summary.incomplete);
        assert_eq!(summary.errors.len(), 1);
        assert_eq!(summary.errors[0].0.as_bytes(), b"locked");
    }

    #[tokio::test]
    async fn a_missing_root_is_one_gap() {
        let t = tempfile::tempdir().unwrap();
        let (v, summary) = events(&t.path().join("missing")).await;
        assert_eq!(v, vec![(String::new(), true)]);
        assert!(summary.incomplete);
        assert_eq!(summary.errors.len(), 1, "{:?}", summary.errors);
    }

    #[tokio::test]
    async fn an_unstatable_entry_is_a_gap_at_its_position() {
        let t = tempfile::tempdir().unwrap();
        std::fs::create_dir(t.path().join("d")).unwrap();
        std::fs::write(t.path().join("d/a"), b"").unwrap();
        std::fs::write(t.path().join("d/b"), b"").unwrap();
        std::fs::write(t.path().join("e"), b"").unwrap();
        // Listable but not searchable: names are read, nothing in it can be stat'ed.
        std::fs::set_permissions(t.path().join("d"), std::fs::Permissions::from_mode(0o644))
            .unwrap();
        if std::fs::symlink_metadata(t.path().join("d/a")).is_ok() {
            std::fs::set_permissions(t.path().join("d"), std::fs::Permissions::from_mode(0o755))
                .unwrap();
            eprintln!("skipping: running with privileges that bypass directory permissions");
            return;
        }
        let (v, summary) = events(t.path()).await;
        std::fs::set_permissions(t.path().join("d"), std::fs::Permissions::from_mode(0o755))
            .unwrap();
        let want: Vec<(String, bool)> = [
            ("", false),
            ("d", false),
            ("d/a", true),
            ("d/b", true),
            ("e", false),
        ]
        .into_iter()
        .map(|(p, g)| (p.to_string(), g))
        .collect();
        assert_eq!(v, want);
        assert!(summary.incomplete);
        assert_eq!(summary.errors.len(), 2);
    }
}
