//! Receiver write path: verified chunks go into `.irsync.<name>.part`, the whole
//! file's hash is folded from them, and only then is it renamed over its final
//! name. Progress is recorded in the index so a later session can adopt a
//! half-written file. Where the filesystem clones files (`FICLONE`), a fresh
//! `.part` for a changed file starts as a clone of the old file, so the
//! chunks the old file already holds at the same offset are verified in
//! place and never copied.

use crate::fs::meta::{apply_metadata_fd_owned, fstat_entry_with_ctime, ApplyOpts};
use crate::fs::paths::RelPath;
use crate::fs::safe::{nofollow_error, open_regular_nofollow, require_regular};
use crate::index::{IndexHandle, Partial};
use crate::telemetry::meter::{Activity, Counter, Meter};
use crate::tree_hash::TreeHash;
use irsync_proto::chunk::{digest_reader, ChunkRecord, FileDigest};
use irsync_proto::v1::{FileEntry, Range, RangeSet};
use std::ffi::OsStr;
use std::fs::{File, OpenOptions};
use std::io::{self, Read};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{FileExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// Persist the partial record after this many newly verified chunks...
const PERSIST_EVERY_CHUNKS: u32 = 64;
/// ...or once the oldest of them is this old, whichever comes first. The
/// age counts from that chunk, not from opening: a file that waited long for
/// its data but then arrives in one go is placed without a record, which
/// would cost three writes under the index lock (here, at finalize, and its
/// removal). Counting from opening made every file pay them once files
/// spent more than this long in flight (a slow disk), slowing them further.
const PERSIST_EVERY: Duration = Duration::from_secs(2);

/// How long `open` waits for another holder of the `.part` lock (typically
/// this session's own previous connection, still finalizing) to let go.
const BUSY_WAIT: Duration = Duration::from_secs(60);
/// Pause between lock attempts while waiting.
const BUSY_RETRY: Duration = Duration::from_millis(200);

/// Why `finalize` did not put the file in place.
#[derive(Debug, thiserror::Error)]
pub enum FinalizeError {
    #[error("file is incomplete: chunks still missing")]
    Incomplete,
    #[error("whole-file hash mismatch")]
    HashMismatch {
        expected: [u8; 32],
        actual: [u8; 32],
    },
    #[error(transparent)]
    Io(#[from] io::Error),
}

/// The old destination file a fresh `.part` may be cloned from, with the
/// chunk list it was hashed to (its chunks at unchanged offsets are then
/// verified in place).
pub struct CloneSource<'a> {
    pub file: &'a File,
    pub digest: &'a FileDigest,
}

pub use crate::fs::clone::reflink;

/// Writes one file's chunks into its `.part` file and finalizes it atomically.
pub struct FileWriter {
    rel: RelPath,
    dest: PathBuf,
    part: PathBuf,
    /// The `.part`, locked. Absent until the first write when nothing was
    /// there to adopt: a file waiting for its data holds no descriptor.
    file: Option<Arc<File>>,
    /// Which file `file` is, as read when it was opened.
    held_id: FileId,
    /// Its owner and group then (`(uid, gid)`), which only this writer,
    /// holding the lock, changes.
    held_owner: (u32, u32),
    size: u64,
    whole_hash: [u8; 32],
    /// The whole-file hash, folded from the verified chunks as they come
    /// (boxed: the writer travels by value through the receiver's events).
    tree: Box<TreeHash>,
    chunks: Vec<ChunkRecord>,
    verified: Vec<bool>,
    index: IndexHandle,
    chunks_since_persist: u32,
    last_persist: tokio::time::Instant,
    /// Set once the record must no longer be written back (after finalize).
    done: bool,
    /// Bytes of the chunks written from the sender's data (not local copies).
    received: u64,
    /// Whether the index holds a partial record for this file (this writer
    /// wrote or adopted one). Without one, finalizing writes none: a `.part`
    /// left by a crash meanwhile is adopted by re-chunking it.
    recorded: bool,
    /// `-S` / sparse source: all-zero chunks are not written (left as holes).
    sparse: bool,
    /// Length of the `.part` when opened: everything from here on is a hole
    /// (never written by an earlier writer), so it already reads as zeros.
    hole_from: u64,
    /// The session's activity marker: bumped by local progress (chunks
    /// written, finalize, re-hashing) so the inactivity watchdogs see it.
    activity: Arc<AtomicU64>,
    /// Times and counts this writer's disk work (none outside a session).
    meter: Option<Arc<Meter>>,
    /// The `.part` started as a clone of the old file.
    cloned: bool,
    /// `--inplace`: `part` is the destination itself; no rename at the end.
    inplace: bool,
    /// Compare the folded whole-file hash with the expected one at
    /// finalize (false for an `APPEND`, whose expected hash is unknown).
    verify_whole: bool,
    /// `--fsync`: flush the file before its metadata and rename.
    sync: bool,
    #[cfg(feature = "test-hooks")]
    written: u64,
    #[cfg(feature = "test-hooks")]
    fail_at: Option<u32>,
    #[cfg(feature = "test-hooks")]
    finalize_delay: Duration,
}

fn invalid(msg: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg.into())
}

fn index_err(e: crate::index::IndexError) -> io::Error {
    io::Error::other(e)
}

fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Validates `expected`'s records as tiling `[0, size)` exactly (they came
/// off the wire) and copies them for the writer.
fn native_chunks(expected: &FileDigest) -> io::Result<Vec<ChunkRecord>> {
    let mut chunks = Vec::with_capacity(expected.chunks.len());
    let mut next = 0u64;
    for &c in &expected.chunks {
        if c.offset != next || c.len == 0 {
            return Err(invalid("chunk list does not tile the file"));
        }
        // Chunks are buffered whole: never more than the chunker's maximum.
        if c.len > MAX_CHUNK_LEN {
            return Err(invalid(format!("chunk of {} bytes exceeds 1 MiB", c.len)));
        }
        next += u64::from(c.len);
        chunks.push(c);
    }
    if next != expected.size {
        return Err(invalid("chunk list does not cover the file size"));
    }
    Ok(chunks)
}

/// Where `.part` files go instead of beside their files (`--partial-dir`):
/// under `dir` (absolute, or relative to the writer's root), mirroring each
/// file's parent directories. `skip` leading components of the file's path
/// are not mirrored: the session root's depth below the writer's root, so a
/// daemon receiving into `sub/` with `.p` writes `sub/.p/<parents>/`, not
/// `sub/.p/sub/<parents>/`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PartialDir {
    pub dir: PathBuf,
    pub skip: usize,
}

impl PartialDir {
    /// A partial dir for a writer whose root is the session root.
    pub fn new(dir: &Path) -> PartialDir {
        PartialDir {
            dir: dir.to_path_buf(),
            skip: 0,
        }
    }
}

/// Paths of the destination and of its `.part` file.
fn paths(
    root: &Path,
    rel: &RelPath,
    partial_dir: Option<&PartialDir>,
) -> io::Result<(PathBuf, PathBuf)> {
    let comps: Vec<&[u8]> = rel.components().collect();
    let (name, parents) = comps
        .split_last()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "empty relative path"))?;
    let (mut part_dir, skip) = match partial_dir {
        Some(pd) => (root.join(&pd.dir), pd.skip),
        None => (root.to_path_buf(), 0),
    };
    for p in parents.iter().skip(skip) {
        part_dir.push(OsStr::from_bytes(p));
    }
    if is_temp_name(name) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "refusing a name reserved for irsync's temporary files",
        ));
    }
    let part_name = temp_name(name, b".part");
    Ok((
        root.join(rel.to_os()),
        part_dir.join(OsStr::from_bytes(&part_name)),
    ))
}

/// Removes a leftover `.part` of `rel` and its progress record, unless
/// another writer holds the `.part` (then both stay for that writer).
pub fn discard_part(
    root: &Path,
    rel: &RelPath,
    partial_dir: Option<&PartialDir>,
    index: &IndexHandle,
) -> io::Result<()> {
    let (_, part) = paths(root, rel, partial_dir)?;
    match open_regular_nofollow(&part) {
        Ok(file) => match file.try_lock() {
            Ok(()) => std::fs::remove_file(&part)?,
            Err(std::fs::TryLockError::WouldBlock) => return Ok(()),
            Err(std::fs::TryLockError::Error(e)) => return Err(e),
        },
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    index
        .lock()
        .map_err(|_| io::Error::other("index poisoned"))?
        .remove_partial(rel)
        .map_err(io::Error::other)
}

/// The longest file name irsync creates, in bytes: `NAME_MAX` on Linux and macOS.
pub const NAME_MAX: usize = 255;

const TEMP_PREFIX: &[u8] = b".irsync.";
/// Hex digits of the name hash in a shortened temporary name.
const TEMP_HASH_HEX: usize = 16;

/// irsync's temporary name for `name`: `.irsync.<name><suffix>`. When that
/// would exceed [`NAME_MAX`], `<name>` is cut (on a character boundary if it
/// is UTF-8) and `~` plus the first 16 hex digits of its BLAKE3 hash are put
/// before the suffix, so the result fits and two long names that share a
/// prefix still get distinct temporaries. Every temporary irsync makes
/// (`.part` and `.tmp`) comes from here, so the mapping is one place.
pub fn temp_name(name: &[u8], suffix: &[u8]) -> Vec<u8> {
    let plain = TEMP_PREFIX.len() + name.len() + suffix.len();
    let mut out = Vec::with_capacity(plain.min(NAME_MAX));
    out.extend_from_slice(TEMP_PREFIX);
    if plain <= NAME_MAX {
        out.extend_from_slice(name);
    } else {
        let keep = NAME_MAX - TEMP_PREFIX.len() - 1 - TEMP_HASH_HEX - suffix.len();
        let keep = match std::str::from_utf8(name) {
            Ok(s) => (0..=keep)
                .rev()
                .find(|&i| s.is_char_boundary(i))
                .unwrap_or(0),
            Err(_) => keep,
        };
        out.extend_from_slice(&name[..keep]);
        out.push(b'~');
        out.extend_from_slice(blake3::hash(name).to_hex()[..TEMP_HASH_HEX].as_bytes());
    }
    out.extend_from_slice(suffix);
    out
}

/// Whether a final path component is one of irsync's own temporary names
/// (`.irsync.<name>.part` / `.irsync.<name>.tmp`, possibly shortened). Such
/// names are never listed, never accepted from a peer and never created as
/// destinations.
pub fn is_temp_name(name: &[u8]) -> bool {
    name.starts_with(TEMP_PREFIX) && (name.ends_with(b".part") || name.ends_with(b".tmp"))
}

/// The largest chunk a peer's `ChunkList` may describe (FastCDC's maximum).
pub const MAX_CHUNK_LEN: u32 = 1024 * 1024;

/// The error message when another writer holds the `.part` (`WouldBlock`).
pub const PART_BUSY: &str = "part file busy";

/// Never follow a symlink at the `.part` name, never block on a FIFO there.
const PART_FLAGS: i32 = nix::libc::O_NOFOLLOW | nix::libc::O_NONBLOCK | nix::libc::O_CLOEXEC;

fn open_rw(part: &Path) -> io::Result<File> {
    OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .custom_flags(PART_FLAGS)
        .open(part)
        .map_err(|e| nofollow_error(e, part))
}

/// Add owner read/write to our own open `.part` file (`fchmod`).
fn make_owner_writable(file: &File) -> io::Result<()> {
    let mode = file.metadata()?.permissions().mode();
    file.set_permissions(std::fs::Permissions::from_mode(mode | 0o600))
}

/// A previous `finalize` may have applied a read-only mode to the `.part`
/// before failing: open it with whatever access the mode still allows
/// (never following a symlink) and restore owner read/write on that fd.
fn restore_owner_rw(part: &Path) -> io::Result<()> {
    let open = |read: bool| {
        OpenOptions::new()
            .read(read)
            .write(!read)
            .custom_flags(PART_FLAGS)
            .open(part)
    };
    let f = open(true).or_else(|_| open(false))?;
    require_regular(&f, part)?;
    make_owner_writable(&f)
}

/// Open the `.part` read/write without following a symlink at its name
/// ([`open_locked_part`] checks that it is a regular file). The file is ours (`.irsync.<name>.part`): if a
/// read-only mode left by a failed finalize is in the way, restore owner
/// read/write through a descriptor and retry once.
fn open_part(part: &Path) -> io::Result<File> {
    let file = match open_rw(part) {
        Err(e) if e.kind() == io::ErrorKind::PermissionDenied => {
            if restore_owner_rw(part).is_err() {
                return Err(e);
            }
            open_rw(part)?
        }
        r => r?,
    };
    Ok(file)
}

/// Whether anything is at the `.part` name (a regular file to adopt, or
/// something `open_part` will refuse).
fn part_exists(part: &Path) -> io::Result<bool> {
    match std::fs::symlink_metadata(part) {
        Ok(_) => Ok(true),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e),
    }
}

/// Which file a descriptor holds: what its path must still name.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct FileId {
    dev: u64,
    ino: u64,
}

impl FileId {
    fn of(m: &std::fs::Metadata) -> FileId {
        FileId {
            dev: m.dev(),
            ino: m.ino(),
        }
    }
}

/// Fails unless the `.part` path still names the file we hold open (`held`),
/// so a rename by path can only move the file that was verified.
fn still_same_file(held: FileId, part: &Path) -> io::Result<()> {
    let now = std::fs::symlink_metadata(part)?;
    if !now.file_type().is_file() || FileId::of(&now) != held {
        return Err(io::Error::other(format!(
            "{} was replaced while it was written; not placing it",
            part.display()
        )));
    }
    Ok(())
}

/// Reads a held file from its start with `pread` (no shared cursor).
struct AtReader<'a> {
    file: &'a File,
    pos: u64,
}

impl Read for AtReader<'_> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.file.read_at(buf, self.pos)?;
        self.pos += n as u64;
        Ok(n)
    }
}

/// Chunks and hashes the held file (never re-opened by path), bumping
/// `activity` as it goes (a large file can take many minutes).
fn digest_held(file: &File, activity: &AtomicU64) -> io::Result<FileDigest> {
    digest_reader(crate::hasher::Ticking::new(
        AtReader { file, pos: 0 },
        activity,
    ))
}

/// Whether `part` still names the file held open (`held`).
fn names_file(held: FileId, part: &Path) -> io::Result<bool> {
    Ok(match std::fs::symlink_metadata(part) {
        Ok(now) => now.file_type().is_file() && FileId::of(&now) == held,
        Err(e) if e.kind() == io::ErrorKind::NotFound => false,
        Err(e) => return Err(e),
    })
}

/// Opens and locks the `.part`, waiting up to [`BUSY_WAIT`] for another
/// holder: a finalize that outlived its connection releases the lock when it
/// finishes. The lock is only kept on a regular file the path still names
/// (a finalize renames its `.part` away; then a fresh one is opened).
/// Returns the file's metadata too, read once the lock is held: until then
/// another writer may still have been growing it.
fn open_locked_part(part: &Path, activity: &AtomicU64) -> io::Result<(File, std::fs::Metadata)> {
    let start = Instant::now();
    loop {
        // A momentary descriptor shortage is waited out rather than failing
        // the file (see `fs::transient`).
        let file = crate::fs::transient::retry(|| open_part(part))?;
        match file.try_lock() {
            Ok(()) => {
                let meta = file.metadata()?;
                if !meta.file_type().is_file() {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        format!("{} is not a regular file", part.display()),
                    ));
                }
                if names_file(FileId::of(&meta), part)? {
                    return Ok((file, meta));
                }
            }
            Err(std::fs::TryLockError::WouldBlock) if start.elapsed() < BUSY_WAIT => {
                drop(file);
                activity.fetch_add(1, Ordering::Relaxed);
                std::thread::sleep(BUSY_RETRY);
            }
            Err(std::fs::TryLockError::WouldBlock) => {
                return Err(io::Error::new(io::ErrorKind::WouldBlock, PART_BUSY))
            }
            Err(std::fs::TryLockError::Error(e)) => {
                // macOS refuses to lock a FIFO (ENOTSUP): report what is
                // there, as the check above does where the lock succeeds.
                require_regular(&file, part)?;
                return Err(e);
            }
        }
    }
}

/// [`open_locked_part`], creating the `.part`'s directory only when the
/// open finds it missing: the planner has nearly always made it, and
/// `create_dir_all` on an existing directory costs a failing mkdir and a
/// stat per file.
fn open_part_in_dir(part: &Path, activity: &AtomicU64) -> io::Result<(File, std::fs::Metadata)> {
    match open_locked_part(part, activity) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            if let Some(dir) = part.parent() {
                std::fs::create_dir_all(dir)?;
            }
            open_locked_part(part, activity)
        }
        r => r,
    }
}

fn read_chunk(file: &File, c: &ChunkRecord) -> io::Result<Vec<u8>> {
    let mut buf = vec![0u8; c.len as usize];
    file.read_exact_at(&mut buf, c.offset)?;
    Ok(buf)
}

/// Normalise ranges: sort by offset and merge overlapping or adjacent ones.
fn coalesce(mut v: Vec<(u64, u64)>) -> Vec<(u64, u64)> {
    v.sort_unstable();
    let mut out: Vec<(u64, u64)> = Vec::with_capacity(v.len());
    for (start, end) in v {
        match out.last_mut() {
            Some(last) if start <= last.1 => last.1 = last.1.max(end),
            _ => out.push((start, end)),
        }
    }
    out
}

impl FileWriter {
    /// Open (or create) the `.part` file for `rel` and adopt any verified progress.
    ///
    /// This does blocking I/O and may re-hash a large `.part` file; async callers
    /// should run it inside `tokio::task::spawn_blocking`.
    pub fn open(
        root: &Path,
        rel: &RelPath,
        expected: &FileDigest,
        partial_dir: Option<&PartialDir>,
        index: IndexHandle,
    ) -> io::Result<FileWriter> {
        Self::open_tracked(
            root,
            rel,
            expected,
            partial_dir,
            index,
            Arc::default(),
            None,
        )
    }

    /// [`FileWriter::open`], bumping `activity` on local progress (see the
    /// `activity` field). Waits up to 60 s for a busy `.part` lock.
    pub fn open_tracked(
        root: &Path,
        rel: &RelPath,
        expected: &FileDigest,
        partial_dir: Option<&PartialDir>,
        index: IndexHandle,
        activity: Arc<AtomicU64>,
        meter: Option<Arc<Meter>>,
    ) -> io::Result<FileWriter> {
        Self::open_from(
            root,
            rel,
            expected,
            partial_dir,
            index,
            activity,
            meter,
            None,
        )
    }

    /// [`FileWriter::open_tracked`] with the old destination file to clone
    /// a fresh `.part` from (see [`reflink`]). A `.part` that already holds
    /// bytes is adopted as it is; a clone the filesystem refuses costs
    /// nothing but the attempt.
    #[allow(clippy::too_many_arguments)]
    pub fn open_from(
        root: &Path,
        rel: &RelPath,
        expected: &FileDigest,
        partial_dir: Option<&PartialDir>,
        index: IndexHandle,
        activity: Arc<AtomicU64>,
        meter: Option<Arc<Meter>>,
        clone: Option<CloneSource<'_>>,
    ) -> io::Result<FileWriter> {
        let chunks = native_chunks(expected)?;
        let (dest, part) = paths(root, rel, partial_dir)?;
        // Nothing to adopt and nothing to clone: the `.part` is opened by
        // the first write (or by `finalize`), so a file waiting for the
        // sender's stream holds no descriptor.
        if clone.is_none() && !part_exists(&part)? {
            let mut w = Self::new(
                rel, dest, part, None, expected, chunks, index, activity, meter,
            );
            w.hole_from = 0;
            return Ok(w);
        }
        // One writer per `.part`: a second (another session on the same
        // destination) could otherwise interleave unverified bytes with ours
        // between our re-hash and rename. Held until the file is closed.
        let (file, meta) = open_part_in_dir(&part, &activity)?;
        let mut hole_from = meta.len();
        let mut cloned = false;
        if let Some(c) = &clone {
            if hole_from == 0 && reflink(c.file, &file).is_ok() {
                cloned = true;
                hole_from = file.metadata()?.len();
            }
        }
        let mut w = Self::new(
            rel,
            dest,
            part,
            Some(Arc::new(file)),
            expected,
            chunks,
            index,
            activity,
            meter,
        );
        w.hole_from = hole_from;
        w.held_id = FileId::of(&meta);
        w.held_owner = (meta.uid(), meta.gid());
        w.cloned = cloned;
        let adopted_from_record = w.adopt_recorded()?;
        if cloned {
            w.adopt_from(clone.as_ref().expect("cloned from a source").digest)?;
        } else if !adopted_from_record {
            w.adopt_unrecorded()?;
        }
        w.held()?.set_len(w.size)?;
        if !adopted_from_record && w.verified.iter().any(|&v| v) {
            w.persist_sync()?;
        }
        Ok(w)
    }

    /// Opens the destination itself as the file to write (`--inplace`):
    /// `old` is that file, as hashed to `old.digest`, and must still be the
    /// inode at the destination. Its chunks at unchanged offsets are
    /// verified in place; the rest are written over it. Writes must come
    /// in ascending order with only forward reads of the old content (the
    /// caller's gate enforces that); the file is never shrunk before
    /// `finalize`, which also applies metadata without a rename.
    pub fn open_inplace(
        root: &Path,
        rel: &RelPath,
        expected: &FileDigest,
        index: IndexHandle,
        activity: Arc<AtomicU64>,
        meter: Option<Arc<Meter>>,
        old: CloneSource<'_>,
    ) -> io::Result<FileWriter> {
        let chunks = native_chunks(expected)?;
        let (dest, _) = paths(root, rel, None)?;
        let (file, now) = open_locked_part(&dest, &activity)?;
        let hashed = old.file.metadata()?;
        if now.ino() != hashed.ino() || now.dev() != hashed.dev() {
            return Err(invalid("destination is no longer the file that was hashed"));
        }
        let hole_from = now.len();
        let mut w = Self::new(
            rel,
            dest.clone(),
            dest,
            Some(Arc::new(file)),
            expected,
            chunks,
            index,
            activity,
            meter,
        );
        w.hole_from = hole_from;
        w.held_id = FileId::of(&now);
        w.held_owner = (now.uid(), now.gid());
        w.inplace = true;
        let adopted_from_record = w.adopt_recorded()?;
        w.adopt_from(old.digest)?;
        // Grown now; shrunk only at finalize (forward copies may still
        // read above the new size).
        if w.size > hole_from {
            w.held()?.set_len(w.size)?;
        }
        if !adopted_from_record && w.verified.iter().any(|&v| v) {
            w.persist_sync()?;
        }
        Ok(w)
    }

    /// A writer over `chunks` with nothing verified yet; `hole_from`,
    /// `held_id`, `held_owner`, `cloned` and `inplace` are set by the caller.
    #[allow(clippy::too_many_arguments)]
    fn new(
        rel: &RelPath,
        dest: PathBuf,
        part: PathBuf,
        file: Option<Arc<File>>,
        expected: &FileDigest,
        chunks: Vec<ChunkRecord>,
        index: IndexHandle,
        activity: Arc<AtomicU64>,
        meter: Option<Arc<Meter>>,
    ) -> FileWriter {
        FileWriter {
            rel: rel.clone(),
            dest,
            part,
            file,
            held_id: FileId::default(),
            held_owner: (0, 0),
            size: expected.size,
            whole_hash: expected.hash,
            tree: Box::new(TreeHash::new(expected.size)),
            verified: vec![false; chunks.len()],
            chunks,
            index,
            chunks_since_persist: 0,
            last_persist: tokio::time::Instant::now(),
            done: false,
            received: 0,
            recorded: false,
            sparse: false,
            hole_from: 0,
            activity,
            meter,
            cloned: false,
            inplace: false,
            verify_whole: true,
            sync: false,
            #[cfg(feature = "test-hooks")]
            written: 0,
            #[cfg(feature = "test-hooks")]
            fail_at: None,
            #[cfg(feature = "test-hooks")]
            finalize_delay: Duration::ZERO,
        }
    }

    /// The held `.part` (adoption only runs on an opened writer).
    fn held(&self) -> io::Result<Arc<File>> {
        self.file
            .clone()
            .ok_or_else(|| io::Error::other("internal: .part not open"))
    }

    /// Whether the `.part` is open (and locked) yet.
    pub fn is_open(&self) -> bool {
        self.file.is_some()
    }

    /// The `.part`, opened and locked now if the open was deferred: created
    /// (with its directory), sized, and from here on a descriptor held
    /// until the file is placed. Waits for a busy lock like `open` does.
    async fn ensure_open(&mut self) -> io::Result<Arc<File>> {
        if let Some(f) = &self.file {
            return Ok(f.clone());
        }
        let (part, size, activity) = (self.part.clone(), self.size, self.activity.clone());
        let (file, meta) =
            tokio::task::spawn_blocking(move || open_sized_part(&part, size, &activity))
                .await
                .map_err(io::Error::other)??;
        Ok(self.opened(file, &meta))
    }

    /// Holds the `.part` [`open_sized_part`] opened, `meta` as it was then.
    fn opened(&mut self, file: File, meta: &std::fs::Metadata) -> Arc<File> {
        self.hole_from = meta.len();
        self.held_id = FileId::of(meta);
        self.held_owner = (meta.uid(), meta.gid());
        let file = Arc::new(file);
        self.file = Some(file.clone());
        file
    }

    /// The `.part` started as a clone of the old file.
    pub fn cloned(&self) -> bool {
        self.cloned
    }

    /// The writer writes into the destination itself (`--inplace`).
    pub fn inplace(&self) -> bool {
        self.inplace
    }

    /// Whether `finalize` compares the whole-file hash with the expected
    /// one. Off for an `APPEND`: the sender never read the prefix, so the
    /// receiver's folded hash is the only one there is.
    pub fn set_verify_whole(&mut self, verify: bool) {
        self.verify_whole = verify;
    }

    /// Whether `finalize` flushes the file to disk before applying its
    /// metadata and renaming it (`--fsync`). Off by default, as in rsync: a
    /// power loss or kernel crash shortly after can then leave the file at
    /// its name with its size and mtime but not its content.
    pub fn set_sync(&mut self, sync: bool) {
        self.sync = sync;
    }

    /// Bytes written into the `.part` so far (test hook).
    #[cfg(feature = "test-hooks")]
    pub fn bytes_written(&self) -> u64 {
        self.written
    }

    /// Adopt the chunks a clone of the old file (chunk list `old`) holds at
    /// an offset where the new list wants the same chunk: verified by
    /// reading, never copied.
    fn adopt_from(&mut self, old: &FileDigest) -> io::Result<()> {
        let file = self.held()?;
        let file_len = file.metadata()?.len();
        for o in &old.chunks {
            let Ok(i) = self.chunks.binary_search_by_key(&o.offset, |c| c.offset) else {
                continue;
            };
            let c = self.chunks[i];
            if self.verified[i] || c != *o || c.offset + u64::from(c.len) > file_len {
                continue;
            }
            let bytes = read_chunk(&file, &c)?;
            if *blake3::hash(&bytes).as_bytes() == c.hash {
                self.verified[i] = true;
                self.tree.add(c.offset, &bytes);
                self.activity.fetch_add(1, Ordering::Relaxed);
            }
        }
        Ok(())
    }

    /// Adopt chunks covered by the index record. Returns whether a usable record existed.
    fn adopt_recorded(&mut self) -> io::Result<bool> {
        // Looked up on a read connection: every file's writer asks.
        let found = self.index.read(|r| r.partial(&self.rel));
        let Some(p) = found.map_err(index_err)? else {
            return Ok(false);
        };
        if p.expected_hash != self.whole_hash || p.size != self.size {
            // A record for a different expected file: it can never be adopted.
            self.index
                .lock()
                .map_err(|_| io::Error::other("index poisoned"))?
                .remove_partial(&self.rel)
                .map_err(index_err)?;
            return Ok(false);
        }
        self.recorded = true;
        let ranges = coalesce(
            p.ranges
                .ranges
                .iter()
                .map(|r| (r.offset, r.offset.saturating_add(r.length)))
                .collect(),
        );
        let file = self.held()?;
        let file_len = file.metadata()?.len();
        for (i, c) in self.chunks.iter().enumerate() {
            let end = c.offset + u64::from(c.len);
            let covered = ranges.iter().any(|&(s, e)| s <= c.offset && end <= e);
            if covered && end <= file_len {
                let bytes = read_chunk(&file, c)?;
                if *blake3::hash(&bytes).as_bytes() == c.hash {
                    self.verified[i] = true;
                    self.tree.add(c.offset, &bytes);
                }
            }
        }
        Ok(true)
    }

    /// Adopt chunks of a `.part` file that has no index record by re-chunking it.
    fn adopt_unrecorded(&mut self) -> io::Result<()> {
        let file = self.held()?;
        if file.metadata()?.len() == 0 {
            return Ok(());
        }
        let found = digest_held(&file, &self.activity)?;
        for f in found.chunks {
            if let Ok(i) = self.chunks.binary_search_by_key(&f.offset, |c| c.offset) {
                if self.chunks[i] == f {
                    self.verified[i] = true;
                    self.tree.add(f.offset, &read_chunk(&file, &f)?);
                }
            }
        }
        Ok(())
    }

    /// Byte ranges of verified chunks, coalesced.
    pub fn have(&self) -> RangeSet {
        let spans = self
            .chunks
            .iter()
            .zip(&self.verified)
            .filter(|(_, &v)| v)
            .map(|(c, _)| (c.offset, c.offset + u64::from(c.len)))
            .collect();
        RangeSet {
            ranges: coalesce(spans)
                .into_iter()
                .map(|(s, e)| Range {
                    offset: s,
                    length: e - s,
                })
                .collect(),
        }
    }

    /// Indices of chunks not yet verified on disk.
    pub fn missing_chunks(&self) -> Vec<u32> {
        (0..self.chunks.len() as u32)
            .filter(|&i| !self.verified[i as usize])
            .collect()
    }

    /// Leave all-zero chunks as holes instead of writing them (`-S`, or a
    /// sparse source file). Chunk-granular: only whole zero chunks are skipped.
    pub fn set_sparse(&mut self, sparse: bool) {
        self.sparse = sparse;
    }

    /// Make the `pwrite` of chunk `idx` fail with `ENOSPC`.
    #[cfg(feature = "test-hooks")]
    pub fn fail_write_at(&mut self, idx: u32) {
        self.fail_at = Some(idx);
    }

    /// Make `finalize` take `delay` longer, as slow storage would (reporting
    /// progress every 250 ms like a long re-hash does).
    #[cfg(feature = "test-hooks")]
    pub fn slow_finalize(&mut self, delay: Duration) {
        self.finalize_delay = delay;
    }

    /// Whether `data` hashes to chunk `c`'s hash, timed as hashing.
    /// Bytes of the chunks this writer took from the sender's data.
    pub fn received(&self) -> u64 {
        self.received
    }

    /// Verify `data`, received from the sender, against chunk `idx` and
    /// write it at the chunk's offset.
    pub async fn write_chunk(&mut self, idx: u32, data: &[u8]) -> io::Result<()> {
        self.put_chunk(idx, Source::Bytes(data.to_vec()), true)
            .await
    }

    /// [`FileWriter::write_chunk`] for chunk data read from a local file.
    pub async fn write_copied(&mut self, idx: u32, data: &[u8]) -> io::Result<()> {
        self.put_chunk(idx, Source::Bytes(data.to_vec()), false)
            .await
    }

    /// Read chunk `idx`'s bytes from `from` at `offset`, then write them
    /// like `write_copied`, all in one blocking call.
    pub async fn copy_local(&mut self, idx: u32, from: &Path, offset: u64) -> io::Result<()> {
        let from = from.to_path_buf();
        self.put_chunk(idx, Source::Local { from, offset }, false)
            .await
    }

    /// Verifies chunk `idx` from `src` (the sender, `remote`, or a local
    /// file) and writes it at the chunk's offset: read, check, the `.part`'s
    /// open on the first write, and the write in one blocking call.
    async fn put_chunk(&mut self, idx: u32, src: Source, remote: bool) -> io::Result<()> {
        let c = *self.chunks.get(idx as usize).ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "chunk index out of range")
        })?;
        let meter = self.meter.clone();
        #[cfg(feature = "test-hooks")]
        let inject = self.fail_at == Some(idx);
        #[cfg(not(feature = "test-hooks"))]
        let inject = false;
        let (held, hole_from, sparse) = (self.file.clone(), self.hole_from, self.sparse);
        let (part, size, activity) = (self.part.clone(), self.size, self.activity.clone());
        let (opened, data, hole, res) = tokio::task::spawn_blocking(move || {
            let data = match src {
                Source::Bytes(data) => data,
                Source::Local { from, offset } => {
                    let _timed = meter.as_deref().map(|m| m.start(Activity::Read));
                    let mut buf = vec![0u8; c.len as usize];
                    crate::fs::safe::open_regular_nofollow(&from)?
                        .read_exact_at(&mut buf, offset)?;
                    if let Some(m) = &meter {
                        m.add(Counter::DiskRead, buf.len() as u64);
                    }
                    buf
                }
            };
            if data.len() != c.len as usize || !verify(meter.as_deref(), &c, &data) {
                return Err(invalid(format!("chunk {idx} does not match its hash")));
            }
            let _timed = meter.as_deref().map(|m| m.start(Activity::Write));
            let opened = match held {
                Some(_) => None,
                None => Some(open_sized_part(&part, size, &activity)?),
            };
            let (file, hole_from) = match (&held, &opened) {
                (Some(f), _) => (&**f, hole_from),
                (None, Some((f, meta))) => (f, meta.len()),
                (None, None) => unreachable!("opened above"),
            };
            // A hole (`set_len` past the opened length) already reads as
            // zeros; below `hole_from` an earlier writer's bytes may be there.
            let hole = sparse && c.offset >= hole_from && data.iter().all(|&b| b == 0);
            let res = if hole && !inject {
                Ok(())
            } else if inject {
                Err(io::Error::from_raw_os_error(nix::libc::ENOSPC))
            } else {
                file.write_all_at(&data, c.offset)
            };
            Ok((opened, data, hole, res))
        })
        .await
        .map_err(io::Error::other)??;
        if let Some((file, meta)) = opened {
            self.opened(file, &meta);
        }
        if let Err(e) = res {
            if let Err(pe) = self.persist().await {
                tracing::warn!(path = ?self.part, error = %pe, "persisting partial after write error failed");
            }
            return Err(e);
        }
        if !hole {
            #[cfg(feature = "test-hooks")]
            {
                self.written += data.len() as u64;
            }
            if let Some(m) = &self.meter {
                m.add(Counter::DiskWrite, data.len() as u64);
            }
        }
        self.activity.fetch_add(1, Ordering::Relaxed);
        if !self.verified[idx as usize] {
            self.verified[idx as usize] = true;
            self.tree.add(c.offset, &data);
            if self.chunks_since_persist == 0 {
                self.last_persist = tokio::time::Instant::now();
            }
            self.chunks_since_persist += 1;
            if remote {
                self.received += data.len() as u64;
            }
        }
        if self.chunks_since_persist >= PERSIST_EVERY_CHUNKS
            || self.last_persist.elapsed() >= PERSIST_EVERY
        {
            self.persist().await?;
        }
        Ok(())
    }

    fn partial_record(&self) -> Partial {
        Partial {
            expected_hash: self.whole_hash,
            size: self.size,
            ranges: self.have(),
            updated_at: now_secs(),
        }
    }

    /// Write the current progress to the index (synchronous; never call across an await).
    fn persist_sync(&mut self) -> io::Result<()> {
        let rec = self.partial_record();
        self.index
            .lock()
            .map_err(|_| io::Error::other("index poisoned"))?
            .put_partial(&self.rel, &rec)
            .map_err(index_err)?;
        self.chunks_since_persist = 0;
        self.last_persist = tokio::time::Instant::now();
        self.recorded = true;
        Ok(())
    }

    async fn persist(&mut self) -> io::Result<()> {
        let rec = self.partial_record();
        let (index, rel) = (self.index.clone(), self.rel.clone());
        tokio::task::spawn_blocking(move || {
            index
                .lock()
                .map_err(|_| io::Error::other("index poisoned"))?
                .put_partial(&rel, &rec)
                .map_err(index_err)
        })
        .await
        .map_err(io::Error::other)??;
        self.chunks_since_persist = 0;
        self.last_persist = tokio::time::Instant::now();
        self.recorded = true;
        Ok(())
    }

    /// Verify the whole `.part`, apply metadata and rename it over the destination.
    /// On any error the `.part` stays, with its progress recorded (on drop at
    /// the latest); nothing appears at the final name.
    /// Returns the file's whole-file hash (folded from its chunks).
    pub async fn finalize(
        self,
        entry: &FileEntry,
        opts: &ApplyOpts,
    ) -> Result<[u8; 32], FinalizeError> {
        self.place(entry, opts).await.map(|p| p.hash)
    }

    /// [`FileWriter::finalize`], also returning what the placed file's
    /// entry now is, as the same blocking step read it.
    pub async fn place(self, entry: &FileEntry, opts: &ApplyOpts) -> Result<Placed, FinalizeError> {
        self.verify().await?.commit(entry, opts).await
    }

    /// The first half of [`FileWriter::place`]: every chunk is there, the
    /// file is sized (in place, shrunk), flushed if asked, and its folded
    /// whole-file hash is the expected one. Nothing is at the final name
    /// yet; [`Verified::commit`] puts it there. On an error the `.part`
    /// stays, with its progress recorded (on drop at the latest).
    pub async fn verify(mut self) -> Result<Verified, FinalizeError> {
        if !self.missing_chunks().is_empty() {
            return Err(FinalizeError::Incomplete);
        }
        let meter = self.meter.clone();
        let _timed = meter.as_deref().map(|m| m.start(Activity::Place));
        let file = self.ensure_open().await?;
        self.activity.fetch_add(1, Ordering::Relaxed);
        // A record already there is brought up to date; a file without one
        // (most arrive in one go) gets none just to drop it again below.
        if self.recorded {
            self.persist().await?;
        }
        // Folded from the bytes verified in memory: no read of the `.part`.
        let actual = self
            .tree
            .root()
            .ok_or_else(|| io::Error::other("internal: hash tree incomplete"))?;
        let (size, expected, inplace) = (
            self.size,
            self.verify_whole.then_some(self.whole_hash),
            self.inplace,
        );
        let activity = self.activity.clone();
        let sync_meter = meter.clone();
        let sync = self.sync;
        let held = file.clone();
        #[cfg(feature = "test-hooks")]
        let delay = self.finalize_delay;
        tokio::task::spawn_blocking(move || {
            // Simulated slow storage: local work that reports progress.
            #[cfg(feature = "test-hooks")]
            {
                let step = Duration::from_millis(250);
                let mut left = delay;
                while !left.is_zero() {
                    std::thread::sleep(left.min(step));
                    left = left.saturating_sub(step);
                    activity.fetch_add(1, Ordering::Relaxed);
                }
            }
            // A `.part` was sized when opened; only a file written in place
            // may still need shrinking.
            if inplace {
                held.set_len(size)?;
            }
            if sync {
                let synced = sync_meter
                    .as_deref()
                    .filter(|m| m.is_enabled())
                    .map(|_| Instant::now());
                held.sync_all()?;
                if let (Some(m), Some(t)) = (&sync_meter, synced) {
                    let took = t.elapsed();
                    m.add(
                        Counter::FsyncNanos,
                        u64::try_from(took.as_nanos()).unwrap_or(u64::MAX),
                    );
                    metrics::histogram!("irsync_fsync_seconds", m.labels())
                        .record(took.as_secs_f64());
                }
                activity.fetch_add(1, Ordering::Relaxed);
            }
            if let Some(expected) = expected.filter(|e| *e != actual) {
                return Err(FinalizeError::HashMismatch { expected, actual });
            }
            Ok(())
        })
        .await
        .map_err(io::Error::other)??;
        Ok(Verified {
            w: self,
            file,
            hash: actual,
        })
    }
}

/// A file [`FileWriter::verify`] found complete and correct, not yet at its
/// name: [`Verified::commit`] places it, [`Verified::discard`] drops it.
/// Holds the writer, so the `.part` stays locked meanwhile.
pub struct Verified {
    w: FileWriter,
    file: Arc<File>,
    hash: [u8; 32],
}

impl Verified {
    /// Applies metadata and renames the `.part` over the destination (in
    /// place, the file is already there). On an error the `.part` stays,
    /// with its progress recorded; nothing appears at the final name.
    pub async fn commit(
        self,
        entry: &FileEntry,
        opts: &ApplyOpts,
    ) -> Result<Placed, FinalizeError> {
        let Verified {
            mut w,
            file,
            hash: actual,
        } = self;
        let meter = w.meter.clone();
        let _timed = meter.as_deref().map(|m| m.start(Activity::Place));
        let (part, dest, inplace, rel, held_id, held_owner) = (
            w.part.clone(),
            w.dest.clone(),
            w.inplace,
            w.rel.clone(),
            w.held_id,
            w.held_owner,
        );
        let (entry, opts) = (entry.clone(), *opts);
        let placed = tokio::task::spawn_blocking(move || {
            // Metadata goes through the held descriptor; the rename (by
            // path) only once the path still names that very file. In
            // place, the file is already at its name. A directory at the
            // destination fails the rename (`EISDIR`), never replaced.
            let placed = apply_metadata_fd_owned(&*file, &entry, &opts, held_owner)
                .and_then(|()| still_same_file(held_id, &part))
                .and_then(|()| {
                    if inplace {
                        return Ok(());
                    }
                    std::fs::rename(&part, &dest).map_err(|e| {
                        if e.raw_os_error() == Some(nix::libc::EISDIR) {
                            return io::Error::other(format!(
                                "refusing to replace directory {} with a file",
                                dest.display()
                            ));
                        }
                        io::Error::new(
                            e.kind(),
                            format!("rename {} -> {}: {e}", part.display(), dest.display()),
                        )
                    })
                });
            if let Err(e) = placed {
                // The applied mode may lack owner write; keep the .part resumable.
                if let Err(pe) = make_owner_writable(&file) {
                    tracing::warn!(path = ?part, error = %pe, "restoring u+w on .part failed");
                }
                return Err(e);
            }
            // Read after the rename, which changes the ctime.
            Ok(fstat_entry_with_ctime(&file, &rel).ok())
        })
        .await
        .map_err(io::Error::other)??;
        let placed = Placed {
            hash: actual,
            stat: placed,
        };
        w.activity.fetch_add(1, Ordering::Relaxed);
        w.done = true;
        if !w.recorded {
            return Ok(placed);
        }
        let (index, rel) = (w.index.clone(), w.rel.clone());
        let removed = tokio::task::spawn_blocking(move || {
            index
                .lock()
                .map_err(|_| io::Error::other("index poisoned"))?
                .remove_partial(&rel)
                .map_err(index_err)
        })
        .await
        .map_err(io::Error::other)
        .and_then(|r| r);
        if let Err(e) = removed {
            // The verified file is already in place; a stale record is harmless.
            tracing::warn!(path = ?w.dest, error = %e, "removing partial record failed");
        }
        Ok(placed)
    }

    /// Drops a verified file that is not to be placed (its source changed
    /// after it was hashed): the `.part` and its record go, so nothing of
    /// it is left behind or adopted. In place there is no `.part`: the
    /// destination already holds the written data, and its progress is
    /// recorded as for any writer dropped.
    pub async fn discard(self) {
        let Verified { mut w, file, .. } = self;
        if w.inplace {
            return;
        }
        w.done = true;
        let (part, held_id, index, rel, recorded) = (
            w.part.clone(),
            w.held_id,
            w.index.clone(),
            w.rel.clone(),
            w.recorded,
        );
        let removed = tokio::task::spawn_blocking(move || {
            // Removed only while it is still the file this writer holds
            // (and has locked); the lock goes with the descriptor after.
            still_same_file(held_id, &part)?;
            std::fs::remove_file(&part)?;
            drop(file);
            if recorded {
                index
                    .lock()
                    .map_err(|_| io::Error::other("index poisoned"))?
                    .remove_partial(&rel)
                    .map_err(index_err)?;
            }
            Ok::<_, io::Error>(())
        })
        .await
        .map_err(io::Error::other)
        .and_then(|r| r);
        if let Err(e) = removed {
            tracing::warn!(path = ?w.part, error = %e, "discarding a verified partial file failed");
        }
    }
}

/// Where [`FileWriter::put_chunk`] takes a chunk's bytes from.
enum Source {
    Bytes(Vec<u8>),
    /// Read at `offset` of the regular file `from`.
    Local {
        from: PathBuf,
        offset: u64,
    },
}

/// Whether `data` is chunk `c`, the hashing timed on `meter`.
fn verify(meter: Option<&Meter>, c: &ChunkRecord, data: &[u8]) -> bool {
    let _hashing = meter.map(|m| m.start(Activity::Hash));
    if let Some(m) = meter {
        m.add(Counter::BytesHashed, data.len() as u64);
    }
    *blake3::hash(data).as_bytes() == c.hash
}

/// Opens and locks the `.part` at `part` (creating its directory) and
/// sizes it to `size`; also returns its metadata from before that: its
/// length then (another writer may have left bytes there meanwhile, and
/// below that length nothing is known to be a hole), which file it is and
/// its owner.
fn open_sized_part(
    part: &Path,
    size: u64,
    activity: &AtomicU64,
) -> io::Result<(File, std::fs::Metadata)> {
    let (file, meta) = open_part_in_dir(part, activity)?;
    file.set_len(size)?;
    Ok((file, meta))
}

/// A file [`Verified::commit`] put at its name.
pub struct Placed {
    /// Its whole-file hash, folded from its chunks.
    pub hash: [u8; 32],
    /// Its entry and ctime, read through the descriptor once it was in
    /// place (`None` if that read failed).
    pub stat: Option<(FileEntry, (i64, u32))>,
}

impl Drop for FileWriter {
    /// Record unpersisted progress so a later writer can adopt it.
    fn drop(&mut self) {
        if self.done || self.chunks_since_persist == 0 {
            return;
        }
        let rec = self.partial_record();
        // try_lock: never block (or self-deadlock) in Drop.
        match self.index.try_lock() {
            Ok(mut ix) => {
                if let Err(e) = ix.put_partial(&self.rel, &rec) {
                    tracing::warn!(path = ?self.part, error = %e, "persisting partial on drop failed");
                }
            }
            Err(_) => {
                tracing::warn!(path = ?self.part, "index busy or poisoned; partial not persisted on drop");
            }
        }
    }
}
