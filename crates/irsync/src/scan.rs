//! Parallel directory walk that streams entries in `RelPath` order.

use crate::filter::{FilterSet, Verdict};
use crate::fs::meta::lstat_entry_with_ctime;
use crate::fs::paths::RelPath;
use crate::platform::os_to_bytes;
use crate::telemetry::meter::{Activity, Meter};
use irsync_proto::v1::FileEntry;
use jwalk::{Parallelism, WalkDirGeneric};
use std::cell::RefCell;
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

    pub fn to_wire(&self) -> irsync_proto::v1::ListTotals {
        irsync_proto::v1::ListTotals {
            reg: self.reg,
            dir: self.dir,
            link: self.link,
            special: self.special,
            total_size: self.total_size,
        }
    }

    pub fn from_wire(t: &irsync_proto::v1::ListTotals) -> ListStats {
        ListStats {
            reg: t.reg,
            dir: t.dir,
            link: t.link,
            special: t.special,
            total_size: t.total_size,
        }
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

/// `b` below `a` (either may be the root).
fn join_rel(a: &RelPath, b: &RelPath) -> RelPath {
    match (a.as_bytes(), b.as_bytes()) {
        ([], _) => b.clone(),
        (_, []) => a.clone(),
        (x, y) => {
            RelPath::new(&[x, b"/", y].concat()).expect("joining canonical paths stays canonical")
        }
    }
}

/// Whether `path` is `dir` or lies below it.
fn is_below(path: &RelPath, dir: &RelPath) -> bool {
    let (p, d) = (path.as_bytes(), dir.as_bytes());
    d.is_empty() || p == d || (p.starts_with(d) && p.get(d.len()) == Some(&b'/'))
}

fn rel_of(root: &Path, full: &Path) -> Option<RelPath> {
    let rest = full.strip_prefix(root).ok()?;
    RelPath::from_os(rest.as_os_str()).ok()
}

/// Walk `root`, sending every non-filtered entry (root first) in increasing `RelPath`
/// order, with a `Gap` at the position of whatever could not be listed.
pub fn walk(
    root: PathBuf,
    filters: Arc<FilterSet>,
    opts: WalkOpts,
    meter: Option<Arc<Meter>>,
) -> (mpsc::Receiver<ScanEvent>, oneshot::Receiver<ScanSummary>) {
    walk_below(root, RelPath::root(), filters, opts, meter)
}

/// [`walk`] of the tree at `prefix` below `base`, for a session: every
/// entry is stat'ed through its directory, opened from `base` without
/// following a symlink. A component of `prefix`, or a directory in the
/// tree, that someone swaps for a symlink while the session runs is then
/// reported as a gap; what lies behind the link is not listed.
///
/// With an empty `prefix` the root is a name this side trusts, and
/// `opts.follow_root_symlink` applies to it as in [`walk`]. A root below a
/// prefix is never followed.
pub(crate) fn walk_below(
    base: PathBuf,
    prefix: RelPath,
    filters: Arc<FilterSet>,
    opts: WalkOpts,
    meter: Option<Arc<Meter>>,
) -> (mpsc::Receiver<ScanEvent>, oneshot::Receiver<ScanSummary>) {
    let (root, anchor) = if prefix.as_bytes().is_empty() {
        (base, None)
    } else {
        (base.join(prefix.to_os()), Some((base, prefix)))
    };
    let (tx, rx) = mpsc::channel(1024);
    let (stx, srx) = oneshot::channel();
    // Not `spawn_blocking`: the walk waits on the channel for as long as it
    // runs (see `spawn_thread`).
    drop(spawn_thread("irsync-walk", move || {
        let summary = run_walk(root, anchor, filters, opts, tx, meter);
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
pub(crate) fn spawn_thread<T: Send + 'static>(
    name: &str,
    f: impl FnOnce() -> T + Send + 'static,
) -> tokio::task::JoinHandle<T> {
    let (tx, rx) = oneshot::channel();
    let name = name.to_string();
    #[cfg(feature = "netsim")]
    let held = clock_hold::hold();
    let spawned = std::thread::Builder::new()
        .name(name.clone())
        .spawn(move || {
            let _ = tx.send(f());
            // Only once the result is sent: the waiting task is woken
            // before the clock may move again.
            #[cfg(feature = "netsim")]
            drop(held);
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

/// Keeps tokio's paused clock (in-process network tests) from skipping
/// ahead while a [`spawn_thread`] thread works. The runtime cannot see
/// such a thread: waiting for it, it finds nothing to do and jumps to its
/// next timer, again and again, so a walk that a loaded machine runs late
/// costs a session minutes of virtual time, and its timeouts fire. Work on
/// tokio's blocking pool holds the clock still; so, with the hold switched
/// on, every such thread also keeps a pool task waiting until it ends.
/// Never on in a real run: it would put back the pool use
/// [`spawn_thread`] avoids.
#[cfg(feature = "netsim")]
pub mod clock_hold {
    use std::sync::atomic::{AtomicBool, Ordering::Relaxed};

    static ON: AtomicBool = AtomicBool::new(false);

    /// Switches the hold on for the rest of the process.
    pub fn enable() {
        ON.store(true, Relaxed);
    }

    /// When the hold is on: a guard that keeps the clock still until it
    /// drops. Must be called within a runtime.
    pub(super) fn hold() -> Option<std::sync::mpsc::Sender<()>> {
        if !ON.load(Relaxed) {
            return None;
        }
        let (tx, rx) = std::sync::mpsc::channel::<()>();
        drop(tokio::task::spawn_blocking(move || {
            // Returns once the sender drops.
            let _ = rx.recv();
        }));
        Some(tx)
    }
}

/// Puts a directory's children in file-name order, errors included: the
/// walk's sort puts every error after all the entries, but an error for a
/// child that could not be read (its file name known) becomes a gap that
/// must stand where the child would have, or the listing goes out of
/// order. Errors without a file name keep their place after the entries.
/// The sort is stable, so the entries keep the walk's order among equals.
fn sort_children<T, E>(
    children: &mut [Result<T, E>],
    name: impl Fn(&T) -> Vec<u8>,
    err_name: impl Fn(&E) -> Option<Vec<u8>>,
) {
    // `false` sorts before `true`: nameless errors go last.
    let key = |c: &Result<T, E>| -> (bool, Vec<u8>) {
        match c {
            Ok(t) => (false, name(t)),
            Err(e) => match err_name(e) {
                Some(n) => (false, n),
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

/// Whether a directory's name is that of a clone folder, never listed:
/// [`crate::platform::clone::CLONE_DIR_PREFIX`] and a uid (one or more ASCII
/// digits). Any other name with that start is a user's and is synced.
pub fn is_clone_dir(name: &[u8]) -> bool {
    name.strip_prefix(crate::platform::clone::CLONE_DIR_PREFIX.as_bytes())
        .is_some_and(|uid| !uid.is_empty() && uid.iter().all(u8::is_ascii_digit))
}

fn run_walk(
    mut root: PathBuf,
    anchor: Option<(PathBuf, RelPath)>,
    filters: Arc<FilterSet>,
    opts: WalkOpts,
    tx: mpsc::Sender<ScanEvent>,
    meter: Option<Arc<Meter>>,
) -> ScanSummary {
    // Walked as written (see `native_path`: on Windows a trailing dot or
    // space of a name is kept); names below it are the walk's own.
    if let Ok(native) = crate::platform::native_path(&root) {
        root = native.into_owned();
    }
    // At the top of a volume the system keeps names for itself (Windows:
    // `System Volume Information`, `$RECYCLE.BIN`): never walked, listed or
    // deleted. Judged on the root as given, before a link or a `subst`
    // drive is resolved. Never on Unix (the call is false without a look).
    let volume_top = crate::platform::is_volume_top(&root);
    // Timed while the walk works; released while it waits for room in the channel.
    let timing = RefCell::new(meter.as_deref().map(|m| m.start(Activity::Walk)));
    let emit = |ev: ScanEvent| -> bool {
        timing.borrow_mut().take();
        let sent = tx.blocking_send(ev).is_ok();
        *timing.borrow_mut() = meter.as_deref().map(|m| m.start(Activity::Walk));
        sent
    };
    let mut sum = ScanSummary::default();
    if anchor.is_none()
        && opts.follow_root_symlink
        && std::fs::metadata(&root).is_ok_and(|m| m.is_dir())
    {
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
    // The directories down to the entry the walk is at, opened from the
    // root (from the base, below a prefix) without following a symlink.
    let (stat_base, stat_prefix) = match &anchor {
        Some((base, prefix)) => (base.clone(), prefix.clone()),
        None => (root.clone(), RelPath::root()),
    };
    let dirs = RefCell::new(crate::fs::safe::DirStack::new(&stat_base));
    // A directory that turned out to be a symlink: nothing below it is
    // listed or named, and it has had its one gap.
    let blocked: RefCell<Option<RelPath>> = RefCell::new(None);
    let lstat = |rel: &RelPath| -> std::io::Result<Option<(FileEntry, (i64, u32))>> {
        if rel.as_bytes().is_empty() && stat_prefix.as_bytes().is_empty() {
            return lstat_entry_with_ctime(&root, rel).map(Some);
        }
        if blocked.borrow().as_ref().is_some_and(|b| is_below(rel, b)) {
            return Ok(None);
        }
        let key = join_rel(&stat_prefix, rel);
        let (parent, name) = crate::fs::safe::split_last(&key)?;
        let mut dirs = dirs.borrow_mut();
        let dirs = dirs
            .as_mut()
            .map_err(|e| std::io::Error::new(e.kind(), e.to_string()))?;
        let dir = match dirs.dir(&parent) {
            Ok(d) => d,
            Err(e) if e.kind() == std::io::ErrorKind::InvalidInput => {
                // The gap goes to the directory, not to what jwalk read
                // through the link.
                *blocked.borrow_mut() = Some(rel.parent());
                return Err(e);
            }
            Err(e) => return Err(e),
        };
        crate::fs::meta::lstat_at(dir, &name, rel).map(Some)
    };
    let send = |sum: &mut ScanSummary, rel: RelPath| -> bool {
        match lstat(&rel) {
            Ok(None) => true,
            Ok(Some((entry, ctime))) => {
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
            Err(e)
                if e.kind() == std::io::ErrorKind::InvalidInput && !rel.as_bytes().is_empty() =>
            {
                gap(sum, rel.parent(), e.to_string())
            }
            Err(e) => gap(sum, rel, e.to_string()),
        }
    };
    if !send(&mut sum, RelPath::root()) || sum.incomplete {
        // A root that cannot be stat'ed has its gap; the walk would only
        // report it again.
        return sum;
    }
    // A root below a prefix that is not a directory (a symlink put there
    // meanwhile, a file) has nothing below it to walk.
    if anchor.is_some() && !std::fs::symlink_metadata(&root).is_ok_and(|m| m.is_dir()) {
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
                |e| os_to_bytes(e.file_name()),
                |err| err.path().and_then(Path::file_name).map(os_to_bytes),
            );
            let Some(dir_rel) = rel_of(&cb_root, dir) else {
                return;
            };
            children.retain_mut(|c| {
                let Ok(e) = c else { return true };
                if volume_top
                    && dir_rel.as_bytes().is_empty()
                    && crate::platform::is_builtin_exclude(&os_to_bytes(e.file_name()))
                {
                    return false;
                }
                let Ok(rel) = dir_rel.join(&os_to_bytes(e.file_name())) else {
                    // Kept, unwalked, for the loop below to report.
                    e.read_children = None;
                    return true;
                };
                if filters.matches(&rel, e.file_type().is_dir()) == Verdict::Exclude {
                    return false;
                }
                // `--paranoid`'s clone folders, with everything below them.
                !(e.file_type().is_dir() && is_clone_dir(&os_to_bytes(e.file_name())))
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
    use crate::platform::set_mode_path_following;

    /// Only a clone folder's exact name form is pruned: the prefix and a
    /// uid; a user's folder that merely starts the same way is synced.
    #[test]
    fn only_a_uid_after_the_prefix_is_a_clone_dir() {
        assert!(is_clone_dir(b".irsync-clones-1000"));
        assert!(is_clone_dir(b".irsync-clones-0"));
        assert!(!is_clone_dir(b".irsync-clones-backup"));
        assert!(!is_clone_dir(b".irsync-clones-"));
        assert!(!is_clone_dir(b".irsync-clones-1000x"));
        assert!(!is_clone_dir(b"irsync-clones-1000"));
    }

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

    /// [`events`] for the tree at `prefix` below `base`, as a session walks it.
    async fn events_below(base: &Path, prefix: &str) -> (Vec<(String, bool)>, ScanSummary) {
        let (mut rx, summary) = walk_below(
            base.to_path_buf(),
            RelPath::new(prefix.as_bytes()).unwrap(),
            Arc::new(FilterSet::new(vec![])),
            WalkOpts {
                follow_root_symlink: true,
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

    /// A daemon's session root lies below its export. If it, or a
    /// directory on the way to it, has become a link out of the export,
    /// the walk lists nothing of what the link leads to.
    #[tokio::test]
    async fn a_session_root_that_became_a_link_is_not_walked() {
        let (export, outside) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        std::fs::create_dir(outside.path().join("private")).unwrap();
        std::fs::write(outside.path().join("private/key"), b"k").unwrap();
        std::fs::write(outside.path().join("notes"), b"n").unwrap();
        let named = |v: &[(String, bool)]| {
            v.iter()
                .any(|(p, _)| p.contains("private") || p.contains("key") || p.contains("notes"))
        };

        // A real session root is walked as ever.
        std::fs::create_dir_all(export.path().join("real/d")).unwrap();
        std::fs::write(export.path().join("real/d/f"), b"f").unwrap();
        let (v, sum) = events_below(export.path(), "real").await;
        let want = [("", false), ("d", false), ("d/f", false)].map(|(p, g)| (p.to_string(), g));
        assert_eq!(v, want);
        assert!(!sum.incomplete);

        // The session root itself is a link.
        crate::platform::testutil::symlink(outside.path(), &export.path().join("s")).unwrap();
        let (v, _) = events_below(export.path(), "s").await;
        assert!(!named(&v), "{v:?}");
        assert_eq!(v, [(String::new(), false)], "only the link itself");

        // A directory on the way to it is a link.
        std::fs::create_dir(outside.path().join("sub")).unwrap();
        std::fs::write(outside.path().join("sub/inner"), b"i").unwrap();
        let (v, sum) = events_below(export.path(), "s/sub").await;
        assert!(
            !named(&v) && !v.iter().any(|(p, _)| p.contains("inner")),
            "{v:?}"
        );
        assert!(sum.incomplete);
        assert_eq!(v, [(String::new(), true)], "one gap, at the root");

        // By name, the same root does lead outside: what `walk_below` avoids.
        let (v, _) = {
            let (mut rx, summary) = walk(
                export.path().join("s"),
                Arc::new(FilterSet::new(vec![])),
                WalkOpts {
                    follow_root_symlink: true,
                    threads: 2,
                    max_depth: None,
                },
                None,
            );
            let mut v = Vec::new();
            while let Some(ev) = rx.recv().await {
                if let ScanEvent::Entry(i) = ev {
                    v.push((
                        String::from_utf8_lossy(i.path.as_bytes()).into_owned(),
                        false,
                    ));
                }
            }
            (v, summary.await.unwrap())
        };
        assert!(named(&v));
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
        sort_children(
            &mut children,
            |n| n.as_bytes().to_vec(),
            |e| e.map(|n| n.as_bytes().to_vec()),
        );
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
        set_mode_path_following(&t.path().join("locked"), 0o000).unwrap();
        if std::fs::read_dir(t.path().join("locked")).is_ok() {
            set_mode_path_following(&t.path().join("locked"), 0o755).unwrap();
            eprintln!("skipping: running with privileges that bypass directory permissions");
            return;
        }
        let (v, summary) = events(t.path()).await;
        set_mode_path_following(&t.path().join("locked"), 0o755).unwrap();
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
        set_mode_path_following(&t.path().join("d"), 0o644).unwrap();
        if std::fs::symlink_metadata(t.path().join("d/a")).is_ok() {
            set_mode_path_following(&t.path().join("d"), 0o755).unwrap();
            eprintln!("skipping: running with privileges that bypass directory permissions");
            return;
        }
        let (v, summary) = events(t.path()).await;
        set_mode_path_following(&t.path().join("d"), 0o755).unwrap();
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

    /// The walk is listing a directory's files when the directory is
    /// swapped for a link out of the tree. The walk is held there
    /// by its own full queue: the directory has more files than the queue
    /// takes, and nothing more is read from it until the swap is done. The
    /// files listed after the swap are still the directory's own, reached
    /// through the directory the walk had open; none is from behind the
    /// link.
    #[tokio::test]
    async fn a_directory_swapped_for_a_link_in_mid_walk_is_not_followed() {
        let t = tempfile::tempdir().unwrap();
        let (root, outside, aside) = (
            t.path().join("root"),
            t.path().join("out"),
            t.path().join("aside"),
        );
        for d in [&root.join("z"), &outside] {
            std::fs::create_dir_all(d).unwrap();
        }
        const FILES: usize = 2300;
        for i in 0..FILES {
            std::fs::write(root.join(format!("z/f{i:04}")), b"").unwrap();
            std::fs::write(outside.join(format!("f{i:04}")), b"secret").unwrap();
        }
        std::fs::write(outside.join("notes"), b"secret").unwrap();

        let (mut rx, summary) = walk(
            root.clone(),
            Arc::new(FilterSet::new(vec![])),
            WalkOpts {
                follow_root_symlink: false,
                threads: 2,
                max_depth: None,
            },
            None,
        );
        // Up to the first file of the directory: the walk has it open now.
        // (At the directory's own entry it may not have opened it yet, and
        // would then refuse the link and list nothing below it.)
        loop {
            match rx.recv().await.expect("the walk ended early") {
                ScanEvent::Entry(i) if i.path.as_bytes().starts_with(b"z/f") => break,
                _ => {}
            }
        }
        // Windows refuses to rename a directory while a file in it is open,
        // and the walk opens each file for a moment to stat it: tried
        // again until the walk is between files (it soon waits for room
        // in the channel, which this test is not reading). Unix renames at
        // once.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while let Err(e) = std::fs::rename(root.join("z"), &aside) {
            assert!(
                e.kind() == std::io::ErrorKind::PermissionDenied
                    && std::time::Instant::now() < deadline,
                "{e}"
            );
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        crate::platform::testutil::symlink(&outside, &root.join("z")).unwrap();
        assert_eq!(std::fs::read(root.join("z/f0000")).unwrap(), b"secret");

        let mut below = Vec::new();
        while let Some(ev) = rx.recv().await {
            if let ScanEvent::Entry(i) = ev {
                below.push((i.path, i.entry.size));
            }
        }
        let _ = summary.await.unwrap();
        // More than the queue held at the swap: some were stat'ed after it.
        assert!(below.len() > 1100, "{} entries", below.len());
        for (path, size) in &below {
            assert!(path.as_bytes().starts_with(b"z/f"), "{path:?}");
            assert_eq!(*size, 0, "{path:?} was read from behind the link");
        }
    }
}
