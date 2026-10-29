//! Sorted depth-first filesystem walker with store and mount-point rules. Directories are
//! listed and their entries stat'ed on a small pool of threads; events come out in the
//! same order as from a single thread.

use std::collections::{BinaryHeap, HashMap};
use std::ffi::OsString;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};
use std::sync::{Condvar, Mutex};

use crate::escape::escape;
use crate::model::{Entry, Flags, Partial};
use crate::store::{Reader, FJALL_NAME, SQLITE_NAME};

#[derive(Clone, Debug)]
pub struct WalkConfig {
    /// Canonical absolute root.
    pub root: PathBuf,
    pub cross_mount_points: bool,
    pub sort: bool,
    /// Absolute path of the store inside the tree, if any.
    pub store_path: Option<PathBuf>,
    /// The store is a SQLite file: its `-wal`, `-shm` and `-journal` sidecars, whatever
    /// the store is named, are skipped silently.
    pub sqlite_store: bool,
    /// Skip the `BUILTIN_EXCLUDES` at the top of every volume the walk visits.
    pub builtin_excludes: bool,
    /// Threads that list directories and read metadata.
    pub threads: usize,
    /// Read every entry's xattrs, even those the manifest holds as current.
    pub force_xattr_reads: bool,
}

/// Walk threads unless asked otherwise. Metadata calls stop scaling well beyond this:
/// on APFS, xattr reads contend in the kernel, and 16 threads walk slower than one.
pub const DEFAULT_THREADS: usize = 4;

/// Names macOS keeps at the top of a volume for its own use. They are unreadable there and
/// junk anywhere else, so the walk neither records nor descends them.
pub const BUILTIN_EXCLUDES: &[&str] = &[
    ".DocumentRevisions-V100",
    ".Spotlight-V100",
    ".TemporaryItems",
    ".Trashes",
    ".fseventsd",
];

#[derive(Debug)]
pub struct Observed {
    /// Relative path bytes; `b"."` for the root.
    pub path: Vec<u8>,
    pub abs: PathBuf,
    pub entry: Entry,
    pub hash_error: Option<String>,
    /// What the manifest holds for this path, when the walk was given a reader.
    pub stored: Option<Entry>,
    /// Why the manifest could not be asked; `stored` is then `None`.
    pub lookup_error: Option<String>,
}

#[derive(Debug)]
// The enum shape is fixed by the design.
#[allow(clippy::large_enum_variant)]
pub enum WalkEvent {
    Entry(Observed),
    Warning(String),
    /// Relative path of a `.fileset.sqlite` or `.fileset.fjall` below the root. The
    /// object itself follows as an ordinary `Entry`; the engine decides whether this is a
    /// warning or a reason to stop.
    NestedManifest(Vec<u8>),
    /// Relative path of a directory the walker did not descend, or of an object it could
    /// not stat; emitted right after the corresponding `Warning`. Nothing stored at or
    /// below it was observed.
    NotDescended(Vec<u8>),
    /// Relative path of a built-in exclude: neither recorded nor descended, and not a
    /// warning.
    Excluded(Vec<u8>),
}

type Sink<'a> = dyn FnMut(WalkEvent) -> anyhow::Result<()> + 'a;

/// The warning text for a `WalkEvent::NestedManifest`.
pub fn nested_manifest_warning(rel: &[u8]) -> String {
    format!("nested manifest at {}", escape(rel))
}

/// The info text for a `WalkEvent::Excluded`.
pub fn excluded_info(rel: &[u8]) -> String {
    format!("skipping built-in exclude {}", escape(rel))
}

/// Walks the tree at `cfg.root`. With a `reader` over the manifest, each entry carries what
/// the manifest holds for it, and xattrs the manifest holds as current are not read again.
pub fn walk(
    cfg: &WalkConfig,
    reader: Option<&dyn Reader>,
    sink: &mut Sink<'_>,
) -> anyhow::Result<()> {
    let (obs, partial) = observe(cfg, reader, cfg.root.clone(), b".".to_vec())?;
    for p in &partial {
        sink(WalkEvent::Warning(p.warning(".")))?;
    }
    let root_dev = obs.entry.dev;
    let is_dir = obs.entry.kind == crate::model::Kind::Dir;
    sink(WalkEvent::Entry(obs))?;
    if is_dir {
        walk_dir(cfg, reader, root_dev, root_dev, true, &cfg.root, b".", sink)?;
    }
    Ok(())
}

/// Stats `abs` and looks `rel` up through `reader`. The xattrs are read unless the
/// manifest's are current for the object found, and `force_xattr_reads` is off.
fn observe(
    cfg: &WalkConfig,
    reader: Option<&dyn Reader>,
    abs: PathBuf,
    rel: Vec<u8>,
) -> std::io::Result<(Observed, Vec<Partial>)> {
    let (mut entry, mut partial) = Entry::stat(&abs)?;
    let (stored, lookup_error) = match reader.map(|r| r.get(&rel)) {
        None => (None, None),
        Some(Ok(stored)) => (stored, None),
        Some(Err(e)) => (None, Some(format!("{e:#}"))),
    };
    match stored
        .as_ref()
        .filter(|s| !cfg.force_xattr_reads && entry.xattrs_current_in(s))
    {
        Some(s) => entry.xattrs = s.xattrs.clone(),
        None => entry.load_xattrs(&abs, &mut partial),
    }
    let obs = Observed {
        path: rel,
        abs,
        entry,
        hash_error: None,
        stored,
        lookup_error,
    };
    Ok((obs, partial))
}

fn is_sidecar(cfg: &WalkConfig, abs: &Path) -> bool {
    if !cfg.sqlite_store {
        return false;
    }
    let Some(store) = cfg.store_path.as_deref() else {
        return false;
    };
    let (Some(store_name), Some(name)) = (store.file_name(), abs.file_name()) else {
        return false;
    };
    let Some(suffix) = name.as_bytes().strip_prefix(store_name.as_bytes()) else {
        return false;
    };
    abs.parent() == store.parent() && [&b"-wal"[..], b"-shm", b"-journal"].contains(&suffix)
}

fn cannot_read_dir(dir_rel: &[u8], err: &std::io::Error) -> String {
    format!("cannot read directory {}: {err}", escape(dir_rel))
}

/// A directory for a walk thread to list.
struct DirJob {
    abs: PathBuf,
    rel: Vec<u8>,
    dev: u64,
    /// The root or a mount point, where built-in excludes live.
    volume_top: bool,
}

/// One step of a listed directory, in emission order.
// Boxing the event would cost an allocation per entry to shrink a one-word marker.
#[allow(clippy::large_enum_variant)]
enum Item {
    Event(WalkEvent),
    /// The next subdirectory's own listing goes here.
    Descend,
}

/// Position of a directory in walk order: the index of each `Descend` taken from the
/// first listed directory down. Ordering keys compares them in walk (preorder) order.
type Key = Vec<u32>;

struct Pending(Key, DirJob);

impl PartialEq for Pending {
    fn eq(&self, other: &Self) -> bool {
        self.0 == other.0
    }
}
impl Eq for Pending {}
impl PartialOrd for Pending {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for Pending {
    // Reversed: the heap pops the earliest directory in walk order first.
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        other.0.cmp(&self.0)
    }
}

type Listing = Result<Vec<Item>, String>;

struct State {
    pending: BinaryHeap<Pending>,
    done: HashMap<Key, Listing>,
    /// Listings started or finished and not yet taken by the emitter.
    held: usize,
    /// The listing the emitter waits for; it may start even when `held` is at the window.
    awaited: Option<Key>,
    stop: bool,
}

struct Shared {
    state: Mutex<State>,
    /// Signalled for the walk threads: new jobs, a freed slot, a new `awaited`, or stop.
    work: Condvar,
    /// Signalled for the emitter: a listing finished.
    ready: Condvar,
}

/// Listings each walk thread may finish ahead of the emitter. A small window stalls the
/// threads behind one slow directory: 64 left them idle a quarter of the time.
const WINDOW_PER_THREAD: usize = 1024;

/// Walks below the directory `dir_abs` on `cfg.threads` threads and emits in sorted
/// depth-first order, exactly as a single thread would. The threads list and stat whole
/// directories, earliest in walk order first; the calling thread emits.
#[allow(clippy::too_many_arguments)]
fn walk_dir(
    cfg: &WalkConfig,
    reader: Option<&dyn Reader>,
    root_dev: u64,
    dir_dev: u64,
    volume_top: bool,
    dir_abs: &Path,
    dir_rel: &[u8],
    sink: &mut Sink<'_>,
) -> anyhow::Result<()> {
    let threads = cfg.threads.max(1);
    let shared = Shared {
        state: Mutex::new(State {
            pending: BinaryHeap::from([Pending(
                Vec::new(),
                DirJob {
                    abs: dir_abs.to_path_buf(),
                    rel: dir_rel.to_vec(),
                    dev: dir_dev,
                    volume_top,
                },
            )]),
            done: HashMap::new(),
            held: 0,
            awaited: None,
            stop: false,
        }),
        work: Condvar::new(),
        ready: Condvar::new(),
    };
    let window = threads * WINDOW_PER_THREAD;
    std::thread::scope(|s| {
        let started = (0..threads).try_for_each(|i| {
            std::thread::Builder::new()
                .name(format!("fileset-walk-{i}"))
                .spawn_scoped(s, || lister(cfg, reader, root_dev, &shared, window))
                .map(drop)
                .map_err(|e| anyhow::anyhow!("cannot start a walk thread: {e}"))
        });
        let emitted = started.and_then(|()| emit_in_order(&shared, sink));
        // Threads already started would otherwise wait for work forever, and the scope
        // for them.
        shared.state.lock().unwrap().stop = true;
        shared.work.notify_all();
        emitted
    })
}

/// Takes listings in walk order and sends their events to `sink`.
fn emit_in_order(shared: &Shared, sink: &mut Sink<'_>) -> anyhow::Result<()> {
    let mut stack: Vec<(Key, std::vec::IntoIter<Item>, u32)> =
        vec![(Vec::new(), take(shared, &[])?.into_iter(), 0)];
    while let Some((key, items, descended)) = stack.last_mut() {
        let child = match items.next() {
            None => {
                stack.pop();
                continue;
            }
            Some(Item::Event(ev)) => {
                sink(ev)?;
                continue;
            }
            Some(Item::Descend) => {
                let mut child = key.clone();
                child.push(*descended);
                *descended += 1;
                child
            }
        };
        let items = take(shared, &child)?.into_iter();
        stack.push((child, items, 0));
    }
    Ok(())
}

/// Waits for the listing at `key`. Every key the emitter asks for was queued when its
/// parent's listing finished, and is the earliest unemitted one, so it always gets to run.
fn take(shared: &Shared, key: &[u32]) -> anyhow::Result<Vec<Item>> {
    let mut st = shared.state.lock().unwrap();
    st.awaited = Some(key.to_vec());
    shared.work.notify_all();
    loop {
        if let Some(listing) = st.done.remove(key) {
            st.held -= 1;
            st.awaited = None;
            shared.work.notify_all();
            return listing.map_err(|msg| anyhow::anyhow!("walk thread panicked: {msg}"));
        }
        st = shared.ready.wait(st).unwrap();
    }
}

/// A walk thread: lists the earliest pending directory while fewer than `window` listings
/// are held, or whenever it is the one the emitter waits for.
fn lister(
    cfg: &WalkConfig,
    reader: Option<&dyn Reader>,
    root_dev: u64,
    shared: &Shared,
    window: usize,
) {
    loop {
        let Pending(key, job) = {
            let mut st = shared.state.lock().unwrap();
            loop {
                if st.stop {
                    return;
                }
                let runnable = st
                    .pending
                    .peek()
                    .is_some_and(|p| st.held < window || st.awaited.as_ref() == Some(&p.0));
                if runnable {
                    break;
                }
                st = shared.work.wait(st).unwrap();
            }
            st.held += 1;
            st.pending.pop().expect("peeked")
        };
        // A panic is handed to the emitter as an error; unwinding here would leave it
        // waiting for this listing forever.
        let listed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            list_dir(cfg, reader, root_dev, &job)
        }));
        let mut st = shared.state.lock().unwrap();
        let listing = match listed {
            Ok((items, subdirs)) => {
                for (i, sub) in subdirs.into_iter().enumerate() {
                    let mut k = key.clone();
                    k.push(i as u32);
                    st.pending.push(Pending(k, sub));
                }
                shared.work.notify_all();
                Ok(items)
            }
            Err(panic) => Err(panic
                .downcast_ref::<&str>()
                .map(|s| s.to_string())
                .or_else(|| panic.downcast_ref::<String>().cloned())
                .unwrap_or_default()),
        };
        st.done.insert(key, listing);
        shared.ready.notify_one();
    }
}

/// Lists, sorts and stats one directory: its events in order, with an `Item::Descend` for
/// each subdirectory to walk, and those subdirectories in the same order.
fn list_dir(
    cfg: &WalkConfig,
    reader: Option<&dyn Reader>,
    root_dev: u64,
    job: &DirJob,
) -> (Vec<Item>, Vec<DirJob>) {
    let mut items = Vec::new();
    let mut subdirs = Vec::new();
    let dir_rel = job.rel.as_slice();
    let mut names: Vec<OsString> = match std::fs::read_dir(&job.abs) {
        Ok(rd) => {
            let mut names = Vec::new();
            let mut warned = false;
            for item in rd {
                match item {
                    Ok(e) => names.push(e.file_name()),
                    Err(err) if !warned => {
                        warned = true;
                        items.push(Item::Event(WalkEvent::Warning(cannot_read_dir(
                            dir_rel, &err,
                        ))));
                    }
                    Err(_) => {}
                }
            }
            names
        }
        Err(err) => {
            items.push(Item::Event(WalkEvent::Warning(cannot_read_dir(
                dir_rel, &err,
            ))));
            items.push(Item::Event(WalkEvent::NotDescended(dir_rel.to_vec())));
            return (items, subdirs);
        }
    };
    if cfg.sort {
        names.sort_by(|a, b| a.as_bytes().cmp(b.as_bytes()));
    }
    for name in names {
        let abs = job.abs.join(&name);
        if is_sidecar(cfg, &abs) {
            continue;
        }
        let name_bytes = name.into_vec();
        let excluded = cfg.builtin_excludes
            && job.volume_top
            && BUILTIN_EXCLUDES.iter().any(|n| n.as_bytes() == name_bytes);
        let rel = if dir_rel == b"." {
            name_bytes.clone()
        } else {
            let mut r = dir_rel.to_vec();
            r.push(b'/');
            r.extend_from_slice(&name_bytes);
            r
        };
        if excluded {
            items.push(Item::Event(WalkEvent::Excluded(rel)));
            continue;
        }
        let mut obs = match observe(cfg, reader, abs.clone(), rel.clone()) {
            Ok((obs, partial)) => {
                for p in &partial {
                    items.push(Item::Event(WalkEvent::Warning(p.warning(&escape(&rel)))));
                }
                obs
            }
            Err(err) => {
                items.push(Item::Event(WalkEvent::Warning(format!(
                    "cannot stat {}: {}",
                    escape(&rel),
                    err
                ))));
                items.push(Item::Event(WalkEvent::NotDescended(rel)));
                continue;
            }
        };
        let is_store = cfg.store_path.as_deref() == Some(abs.as_path());
        if is_store {
            obs.entry.flags.insert(Flags::STORE);
        } else if dir_rel != b"."
            && (name_bytes == SQLITE_NAME.as_bytes() || name_bytes == FJALL_NAME.as_bytes())
        {
            // Below the root only: a root-level manifest is the tree's own, merely not the
            // one in use (--manifest points elsewhere).
            items.push(Item::Event(WalkEvent::NestedManifest(rel.clone())));
        }
        let is_dir = obs.entry.kind == crate::model::Kind::Dir;
        let dev = obs.entry.dev;
        let other_mount = is_dir && dev != root_dev && !cfg.cross_mount_points;
        items.push(Item::Event(WalkEvent::Entry(obs)));
        if is_store || !is_dir {
            continue;
        }
        if other_mount {
            items.push(Item::Event(WalkEvent::Warning(format!(
                "not crossing mount point at {}",
                escape(&rel)
            ))));
            items.push(Item::Event(WalkEvent::NotDescended(rel)));
            continue;
        }
        items.push(Item::Descend);
        subdirs.push(DirJob {
            abs,
            rel,
            dev,
            volume_top: dev != job.dev,
        });
    }
    (items, subdirs)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Flags, Kind};
    use std::path::Path;

    fn run(cfg: &WalkConfig) -> (Vec<Observed>, Vec<String>) {
        let (entries, warnings, _) = run_all(cfg);
        (entries, warnings)
    }

    /// Entries, warnings and not-descended paths.
    fn run_all(cfg: &WalkConfig) -> (Vec<Observed>, Vec<String>, Vec<Vec<u8>>) {
        let s = seen(|sink| walk(cfg, None, sink));
        (s.entries, s.warnings, s.skipped)
    }

    #[derive(Default)]
    struct Seen {
        entries: Vec<Observed>,
        warnings: Vec<String>,
        skipped: Vec<Vec<u8>>,
        excluded: Vec<Vec<u8>>,
    }

    fn seen(run: impl FnOnce(&mut Sink<'_>) -> anyhow::Result<()>) -> Seen {
        let mut s = Seen::default();
        run(&mut |ev| {
            match ev {
                WalkEvent::Entry(o) => s.entries.push(o),
                WalkEvent::Warning(w) => s.warnings.push(w),
                WalkEvent::NestedManifest(p) => s.warnings.push(nested_manifest_warning(&p)),
                WalkEvent::NotDescended(p) => s.skipped.push(p),
                WalkEvent::Excluded(p) => s.excluded.push(p),
            }
            Ok(())
        })
        .unwrap();
        s
    }

    fn cfg(root: &Path, store: Option<PathBuf>) -> WalkConfig {
        WalkConfig {
            root: root.to_path_buf(),
            cross_mount_points: false,
            sort: true,
            sqlite_store: store
                .as_deref()
                .is_some_and(|p| !p.to_string_lossy().ends_with(".fjall")),
            store_path: store,
            builtin_excludes: true,
            threads: 4,
            force_xattr_reads: false,
        }
    }

    /// Every event as a line, in emission order.
    fn event_log(cfg: &WalkConfig) -> Vec<String> {
        let mut log = Vec::new();
        walk(cfg, None, &mut |ev| {
            log.push(match ev {
                WalkEvent::Entry(o) => format!("entry {} {:?}", escape(&o.path), o.entry.kind),
                WalkEvent::Warning(w) => format!("warning {w}"),
                WalkEvent::NestedManifest(p) => format!("nested {}", escape(&p)),
                WalkEvent::NotDescended(p) => format!("not-descended {}", escape(&p)),
                WalkEvent::Excluded(p) => format!("excluded {}", escape(&p)),
            });
            Ok(())
        })
        .unwrap();
        log
    }

    /// Directories at several depths, with names that sort around '/' ("a-b" < "a/x" < "a0"),
    /// plus an exclude, a nested manifest and a symlink.
    fn bushy_tree() -> (tempfile::TempDir, PathBuf) {
        let t = tempfile::tempdir().unwrap();
        let r = std::fs::canonicalize(t.path()).unwrap();
        for a in ["a", "a-b", "a0", "b"] {
            for b in ["x", "y-z", "y"] {
                for c in ["1", "2"] {
                    let d = r.join(a).join(b).join(c);
                    std::fs::create_dir_all(&d).unwrap();
                    std::fs::write(d.join("f"), b"").unwrap();
                }
                std::fs::write(r.join(a).join(b).join("g"), b"").unwrap();
            }
        }
        std::fs::create_dir(r.join(".Trashes")).unwrap();
        std::fs::write(r.join("a/x/.fileset.sqlite"), b"").unwrap();
        std::os::unix::fs::symlink("a", r.join("s")).unwrap();
        (t, r)
    }

    #[test]
    fn walk_order_does_not_depend_on_the_thread_count() {
        let (_t, r) = bushy_tree();
        let mut one = cfg(&r, None);
        one.threads = 1;
        let expected = event_log(&one);
        // Root; per top dir: itself and, per middle dir, itself, two dirs, their files and g;
        // then the exclude, the nested manifest (event and entry) and the symlink.
        assert_eq!(expected.len(), 1 + 4 * (1 + 3 * 6) + 1 + 2 + 1);
        // Independently of the walker: sorted names, recursively, without the exclude.
        fn reference(dir: &Path, rel: &str, out: &mut Vec<String>) {
            let mut names: Vec<_> = std::fs::read_dir(dir)
                .unwrap()
                .map(|e| e.unwrap().file_name().into_string().unwrap())
                .collect();
            names.sort();
            for n in names {
                let r = if rel.is_empty() {
                    n.clone()
                } else {
                    format!("{rel}/{n}")
                };
                if r == ".Trashes" {
                    continue;
                }
                out.push(r.clone());
                let p = dir.join(&n);
                if p.symlink_metadata().unwrap().is_dir() {
                    reference(&p, &r, out);
                }
            }
        }
        let mut want = vec![".".to_string()];
        reference(&r, "", &mut want);
        let got: Vec<String> = expected
            .iter()
            .filter_map(|l| l.strip_prefix("entry "))
            .map(|l| l.rsplit_once(' ').unwrap().0.to_string())
            .collect();
        assert_eq!(got, want);
        for threads in [2, 3, 8, 32] {
            let mut many = cfg(&r, None);
            many.threads = threads;
            assert_eq!(event_log(&many), expected, "threads={threads}");
        }
    }

    #[test]
    fn walk_sink_error_stops_every_thread() {
        let (_t, r) = bushy_tree();
        let mut seen = 0;
        let err = walk(&cfg(&r, None), None, &mut |_ev| {
            seen += 1;
            if seen == 10 {
                anyhow::bail!("sink full")
            }
            Ok(())
        })
        .unwrap_err();
        assert!(format!("{err:#}").contains("sink full"), "{err:#}");
        assert_eq!(seen, 10);
    }

    fn paths(entries: &[Observed]) -> Vec<Vec<u8>> {
        entries.iter().map(|o| o.path.clone()).collect()
    }

    fn strs(v: &[&str]) -> Vec<Vec<u8>> {
        v.iter().map(|s| s.as_bytes().to_vec()).collect()
    }

    #[test]
    fn walk_order_is_sorted_depth_first() {
        let t = tempfile::tempdir().unwrap();
        let r = std::fs::canonicalize(t.path()).unwrap();
        std::fs::create_dir(r.join("b")).unwrap();
        std::fs::create_dir(r.join("a")).unwrap();
        std::fs::write(r.join("a/z"), b"").unwrap();
        std::fs::write(r.join("a/y"), b"").unwrap();
        std::fs::write(r.join("c"), b"").unwrap();
        let (e, w) = run(&cfg(&r, None));
        assert_eq!(paths(&e), strs(&[".", "a", "a/y", "a/z", "b", "c"]));
        assert!(w.is_empty());
    }

    #[test]
    fn walk_flags_store_and_skips_sidecars() {
        let t = tempfile::tempdir().unwrap();
        let r = std::fs::canonicalize(t.path()).unwrap();
        std::fs::write(r.join(".fileset.sqlite"), b"").unwrap();
        std::fs::write(r.join(".fileset.sqlite-wal"), b"").unwrap();
        std::fs::write(r.join("f"), b"").unwrap();
        let (e, w) = run(&cfg(&r, Some(r.join(".fileset.sqlite"))));
        assert_eq!(paths(&e), strs(&[".", ".fileset.sqlite", "f"]));
        assert!(e[1].entry.flags.contains(Flags::STORE));
        assert!(!e[2].entry.flags.contains(Flags::STORE));
        assert!(w.is_empty());
    }

    #[test]
    fn walk_warns_on_nested_manifest() {
        let t = tempfile::tempdir().unwrap();
        let r = std::fs::canonicalize(t.path()).unwrap();
        std::fs::create_dir(r.join("sub")).unwrap();
        std::fs::write(r.join("sub/.fileset.sqlite"), b"").unwrap();
        // Directly below the root is not nested: a store elsewhere leaves it unflagged.
        std::fs::write(r.join(".fileset.fjall"), b"").unwrap();
        let (e, w) = run(&cfg(&r, Some(r.join(".fileset.sqlite"))));
        assert_eq!(w.len(), 1);
        assert!(w[0].contains("nested manifest at sub/.fileset.sqlite"));
        let n = e.iter().find(|o| o.path == b"sub/.fileset.sqlite").unwrap();
        assert!(!n.entry.flags.contains(Flags::STORE));
    }

    #[test]
    fn walk_skips_sidecars_of_custom_named_sqlite_store() {
        let t = tempfile::tempdir().unwrap();
        let r = std::fs::canonicalize(t.path()).unwrap();
        std::fs::create_dir(r.join("sub")).unwrap();
        for n in [
            "m.sqlite",
            "m.sqlite-wal",
            "m.sqlite-shm",
            "m.sqlite-journal",
            "x-wal",
        ] {
            std::fs::write(r.join("sub").join(n), b"").unwrap();
        }
        let (e, w) = run(&cfg(&r, Some(r.join("sub/m.sqlite"))));
        assert_eq!(paths(&e), strs(&[".", "sub", "sub/m.sqlite", "sub/x-wal"]));
        assert!(w.is_empty());
    }

    #[test]
    fn walk_does_not_descend_fjall_store_dir() {
        let t = tempfile::tempdir().unwrap();
        let r = std::fs::canonicalize(t.path()).unwrap();
        std::fs::create_dir(r.join(".fileset.fjall")).unwrap();
        std::fs::write(r.join(".fileset.fjall/x"), b"").unwrap();
        let (e, _) = run(&cfg(&r, Some(r.join(".fileset.fjall"))));
        assert_eq!(paths(&e), strs(&[".", ".fileset.fjall"]));
        assert!(e[1].entry.flags.contains(Flags::STORE));
    }

    #[test]
    fn walk_root_symlink_is_followed_once() {
        let t = tempfile::tempdir().unwrap();
        let base = std::fs::canonicalize(t.path()).unwrap();
        std::fs::create_dir(base.join("real")).unwrap();
        std::fs::write(base.join("real/f"), b"").unwrap();
        std::os::unix::fs::symlink(base.join("real"), base.join("link")).unwrap();
        let root = std::fs::canonicalize(base.join("link")).unwrap();
        let (e, _) = run(&cfg(&root, None));
        assert_eq!(e[0].path, b".");
        assert_eq!(e[0].entry.kind, Kind::Dir);
        assert!(paths(&e).contains(&b"f".to_vec()));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn walk_non_utf8_name() {
        let t = tempfile::tempdir().unwrap();
        let r = std::fs::canonicalize(t.path()).unwrap();
        std::fs::write(r.join(std::ffi::OsStr::from_bytes(b"bad\xff")), b"").unwrap();
        let (e, _) = run(&cfg(&r, None));
        assert!(paths(&e).contains(&b"bad\xff".to_vec()));
    }

    #[test]
    fn walk_unreadable_dir_warns_and_reports_not_descended() {
        use std::os::unix::fs::PermissionsExt;
        let t = tempfile::tempdir().unwrap();
        let r = std::fs::canonicalize(t.path()).unwrap();
        std::fs::create_dir(r.join("d")).unwrap();
        std::fs::write(r.join("d/f"), b"").unwrap();
        std::fs::write(r.join("e"), b"").unwrap();
        let mode = |m| std::fs::set_permissions(r.join("d"), std::fs::Permissions::from_mode(m));
        mode(0o000).unwrap();
        if std::fs::read_dir(r.join("d")).is_ok() {
            mode(0o755).unwrap();
            eprintln!("skipping: running with privileges that bypass directory permissions");
            return;
        }
        let (e, mut w, skipped) = run_all(&cfg(&r, None));
        mode(0o755).unwrap();
        // macOS also refuses to read the xattrs of an unreadable directory.
        if cfg!(target_os = "macos") {
            w.retain(|m| !m.starts_with("cannot read xattrs of d: "));
        }
        assert_eq!(paths(&e), strs(&[".", "d", "e"]));
        assert_eq!(w.len(), 1, "{w:?}");
        assert!(w[0].starts_with("cannot read directory d: "), "{}", w[0]);
        assert_eq!(skipped, strs(&["d"]));
    }

    /// Every directory below the root looks like another filesystem when the walk is told a
    /// device number the tree is not on.
    fn walk_from_foreign_device(cfg: &WalkConfig) -> (Vec<Observed>, Vec<String>, Vec<Vec<u8>>) {
        let s = seen_from_foreign_device(cfg);
        (s.entries, s.warnings, s.skipped)
    }

    fn seen_from_foreign_device(cfg: &WalkConfig) -> Seen {
        let (root, _) = Entry::from_path(&cfg.root).unwrap();
        let dev = root.dev.wrapping_add(1);
        seen(|sink| walk_dir(cfg, None, dev, dev, true, &cfg.root, b".", sink))
    }

    #[test]
    fn walk_does_not_cross_mount_points_unless_asked() {
        let t = tempfile::tempdir().unwrap();
        let r = std::fs::canonicalize(t.path()).unwrap();
        std::fs::create_dir(r.join("d")).unwrap();
        std::fs::write(r.join("d/f"), b"").unwrap();
        std::fs::write(r.join("e"), b"").unwrap();
        let (e, w, skipped) = walk_from_foreign_device(&cfg(&r, None));
        // The mount point itself is recorded; nothing below it is.
        assert_eq!(paths(&e), strs(&["d", "e"]));
        assert_eq!(w, vec!["not crossing mount point at d".to_string()]);
        assert_eq!(skipped, strs(&["d"]));

        let mut crossing = cfg(&r, None);
        crossing.cross_mount_points = true;
        let (e, w, skipped) = walk_from_foreign_device(&crossing);
        assert_eq!(paths(&e), strs(&["d", "d/f", "e"]));
        assert!(w.is_empty(), "{w:?}");
        assert!(skipped.is_empty());
    }

    #[test]
    fn walk_unstatable_entry_warns_and_reports_not_descended() {
        use std::os::unix::fs::PermissionsExt;
        let t = tempfile::tempdir().unwrap();
        let r = std::fs::canonicalize(t.path()).unwrap();
        std::fs::create_dir(r.join("d")).unwrap();
        std::fs::write(r.join("d/f"), b"").unwrap();
        // Listable but not searchable: the names are readable, the entries cannot be stat'ed.
        let mode = |m| std::fs::set_permissions(r.join("d"), std::fs::Permissions::from_mode(m));
        mode(0o644).unwrap();
        if std::fs::symlink_metadata(r.join("d/f")).is_ok() {
            mode(0o755).unwrap();
            eprintln!("skipping: running with privileges that bypass directory permissions");
            return;
        }
        let (e, mut w, skipped) = run_all(&cfg(&r, None));
        mode(0o755).unwrap();
        if cfg!(target_os = "macos") {
            w.retain(|m| !m.starts_with("cannot read xattrs of "));
        }
        assert_eq!(paths(&e), strs(&[".", "d"]));
        assert_eq!(w.len(), 1, "{w:?}");
        assert!(w[0].starts_with("cannot stat d/f: "), "{}", w[0]);
        assert_eq!(skipped, strs(&["d/f"]));
    }

    #[test]
    fn walk_unsorted_visits_the_same_entries() {
        let t = tempfile::tempdir().unwrap();
        let r = std::fs::canonicalize(t.path()).unwrap();
        for n in ["c", "a", "b"] {
            std::fs::write(r.join(n), b"").unwrap();
        }
        let mut unsorted = cfg(&r, None);
        unsorted.sort = false;
        let (e, w) = run(&unsorted);
        let mut got = paths(&e);
        got.sort();
        assert_eq!(got, strs(&[".", "a", "b", "c"]));
        assert!(w.is_empty());
    }

    fn spotlight_tree() -> (tempfile::TempDir, PathBuf) {
        let t = tempfile::tempdir().unwrap();
        let r = std::fs::canonicalize(t.path()).unwrap();
        std::fs::create_dir(r.join(".Spotlight-V100")).unwrap();
        std::fs::write(r.join(".Spotlight-V100/store.db"), b"").unwrap();
        std::fs::create_dir(r.join(".Trashes")).unwrap();
        std::fs::create_dir_all(r.join("sub/.Trashes")).unwrap();
        std::fs::write(r.join("f"), b"").unwrap();
        (t, r)
    }

    #[test]
    fn walk_skips_builtin_excludes_at_the_root_only() {
        let (_t, r) = spotlight_tree();
        let s = seen(|sink| walk(&cfg(&r, None), None, sink));
        assert_eq!(paths(&s.entries), strs(&[".", "f", "sub", "sub/.Trashes"]));
        assert_eq!(s.excluded, strs(&[".Spotlight-V100", ".Trashes"]));
        assert!(s.warnings.is_empty(), "{:?}", s.warnings);
        assert!(s.skipped.is_empty());
    }

    #[test]
    fn walk_without_builtin_excludes_records_them() {
        let (_t, r) = spotlight_tree();
        let mut all = cfg(&r, None);
        all.builtin_excludes = false;
        let s = seen(|sink| walk(&all, None, sink));
        assert_eq!(
            paths(&s.entries),
            strs(&[
                ".",
                ".Spotlight-V100",
                ".Spotlight-V100/store.db",
                ".Trashes",
                "f",
                "sub",
                "sub/.Trashes"
            ])
        );
        assert!(s.excluded.is_empty());
    }

    #[test]
    fn walk_skips_builtin_excludes_at_a_crossed_mount_point() {
        let t = tempfile::tempdir().unwrap();
        let r = std::fs::canonicalize(t.path()).unwrap();
        std::fs::create_dir_all(r.join("d/.Trashes")).unwrap();
        std::fs::create_dir_all(r.join("d/e/.Trashes")).unwrap();
        let mut crossing = cfg(&r, None);
        crossing.cross_mount_points = true;
        let s = seen_from_foreign_device(&crossing);
        assert_eq!(paths(&s.entries), strs(&["d", "d/e", "d/e/.Trashes"]));
        assert_eq!(s.excluded, strs(&["d/.Trashes"]));
    }

    #[test]
    fn walk_symlink_not_followed() {
        let t = tempfile::tempdir().unwrap();
        let r = std::fs::canonicalize(t.path()).unwrap();
        std::fs::create_dir(r.join("d")).unwrap();
        std::fs::write(r.join("d/file"), b"").unwrap();
        std::os::unix::fs::symlink(r.join("d"), r.join("s")).unwrap();
        let (e, _) = run(&cfg(&r, None));
        let p = paths(&e);
        let s = e.iter().find(|o| o.path == b"s").unwrap();
        assert_eq!(s.entry.kind, Kind::Symlink);
        assert!(p.contains(&b"d/file".to_vec()));
        assert!(!p.contains(&b"s/file".to_vec()));
    }
}
