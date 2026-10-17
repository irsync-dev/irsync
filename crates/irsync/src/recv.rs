//! Receiver write path: verified chunks go into `.irsync.<name>.part`, the whole
//! file is re-hashed, and only then is it renamed over its final name. Progress
//! is recorded in the index so a later session can adopt a half-written file.

use crate::fs::meta::{apply_metadata_fd, ApplyOpts};
use crate::fs::paths::RelPath;
use crate::fs::safe::{nofollow_error, require_regular};
use crate::index::{IndexHandle, Partial};
use irsync_proto::chunk::{digest_reader, ChunkRecord, FileDigest};
use irsync_proto::v1::{ChunkList, FileEntry, Range, RangeSet};
use std::ffi::OsStr;
use std::fs::{File, OpenOptions};
use std::io::{self, Read};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{FileExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// Persist the partial record after this many newly verified chunks...
const PERSIST_EVERY_CHUNKS: u32 = 64;
/// ...or after this much time, whichever comes first.
const PERSIST_EVERY: Duration = Duration::from_secs(2);

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

/// Writes one file's chunks into its `.part` file and finalizes it atomically.
pub struct FileWriter {
    rel: RelPath,
    dest: PathBuf,
    part: PathBuf,
    file: Arc<File>,
    size: u64,
    whole_hash: [u8; 32],
    chunks: Vec<ChunkRecord>,
    verified: Vec<bool>,
    index: IndexHandle,
    chunks_since_persist: u32,
    last_persist: Instant,
    /// Set once the record must no longer be written back (after finalize).
    done: bool,
    /// `-S` / sparse source: all-zero chunks are not written (left as holes).
    sparse: bool,
    /// Length of the `.part` when opened: everything from here on is a hole
    /// (never written by an earlier writer), so it already reads as zeros.
    hole_from: u64,
    #[cfg(feature = "test-hooks")]
    fail_at: Option<u32>,
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

/// Validate `expected` into native chunk records that tile `[0, size)` exactly.
fn native_chunks(expected: &ChunkList) -> io::Result<([u8; 32], Vec<ChunkRecord>)> {
    let whole_hash: [u8; 32] = expected
        .whole_hash
        .as_ref()
        .ok_or_else(|| invalid("chunk list missing whole_hash"))?
        .bytes
        .clone()
        .try_into()
        .map_err(|_| invalid("whole_hash is not 32 bytes"))?;
    let mut chunks = Vec::with_capacity(expected.chunks.len());
    let mut next = 0u64;
    for c in &expected.chunks {
        let c = ChunkRecord::try_from(c.clone())?;
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
    Ok((whole_hash, chunks))
}

/// Paths of the destination and of its `.part` file.
fn paths(root: &Path, rel: &RelPath, partial_dir: Option<&Path>) -> io::Result<(PathBuf, PathBuf)> {
    let comps: Vec<&[u8]> = rel.components().collect();
    let (name, parents) = comps
        .split_last()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "empty relative path"))?;
    let mut part_dir = match partial_dir {
        Some(pd) => root.join(pd),
        None => root.to_path_buf(),
    };
    for p in parents {
        part_dir.push(OsStr::from_bytes(p));
    }
    if is_temp_name(name) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "refusing a name reserved for irsync's temporary files",
        ));
    }
    let mut part_name = b".irsync.".to_vec();
    part_name.extend_from_slice(name);
    part_name.extend_from_slice(b".part");
    Ok((
        root.join(rel.to_os()),
        part_dir.join(OsStr::from_bytes(&part_name)),
    ))
}

/// Whether a final path component is one of irsync's own temporary names
/// (`.irsync.<name>.part` / `.irsync.<name>.tmp`). Such names are never
/// listed, never accepted from a peer and never created as destinations.
pub fn is_temp_name(name: &[u8]) -> bool {
    name.starts_with(b".irsync.") && (name.ends_with(b".part") || name.ends_with(b".tmp"))
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

/// Open the `.part` read/write without following a symlink at its name, and
/// require a regular file. The file is ours (`.irsync.<name>.part`): if a
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
    require_regular(&file, part)?;
    Ok(file)
}

/// Fails unless the `.part` path still names the file we hold open, so a
/// rename by path can only move the file that was verified.
fn still_same_file(file: &File, part: &Path) -> io::Result<()> {
    let held = file.metadata()?;
    let now = std::fs::symlink_metadata(part)?;
    if !now.file_type().is_file() || now.dev() != held.dev() || now.ino() != held.ino() {
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

/// Chunks and hashes the held file (never re-opened by path).
fn digest_held(file: &File) -> io::Result<FileDigest> {
    digest_reader(AtReader { file, pos: 0 })
}

fn hash_at(file: &File, c: &ChunkRecord) -> io::Result<[u8; 32]> {
    let mut buf = vec![0u8; c.len as usize];
    file.read_exact_at(&mut buf, c.offset)?;
    Ok(*blake3::hash(&buf).as_bytes())
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
        expected: &ChunkList,
        partial_dir: Option<&Path>,
        index: IndexHandle,
    ) -> io::Result<FileWriter> {
        let (whole_hash, chunks) = native_chunks(expected)?;
        let (dest, part) = paths(root, rel, partial_dir)?;
        if let Some(dir) = part.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let file = open_part(&part)?;
        // One writer per `.part`: a second (another session on the same
        // destination) could otherwise interleave unverified bytes with ours
        // between our re-hash and rename. Held until the file is closed.
        match file.try_lock() {
            Ok(()) => {}
            Err(std::fs::TryLockError::WouldBlock) => {
                return Err(io::Error::new(io::ErrorKind::WouldBlock, PART_BUSY))
            }
            Err(std::fs::TryLockError::Error(e)) => return Err(e),
        }
        let hole_from = file.metadata()?.len();
        let mut w = FileWriter {
            rel: rel.clone(),
            dest,
            part,
            file: Arc::new(file),
            size: expected.size,
            whole_hash,
            verified: vec![false; chunks.len()],
            chunks,
            index,
            chunks_since_persist: 0,
            last_persist: Instant::now(),
            done: false,
            sparse: false,
            hole_from,
            #[cfg(feature = "test-hooks")]
            fail_at: None,
        };
        let adopted_from_record = w.adopt_recorded()?;
        if !adopted_from_record {
            w.adopt_unrecorded()?;
        }
        w.file.set_len(w.size)?;
        if !adopted_from_record && w.verified.iter().any(|&v| v) {
            w.persist_sync()?;
        }
        Ok(w)
    }

    /// Adopt chunks covered by the index record. Returns whether a usable record existed.
    fn adopt_recorded(&mut self) -> io::Result<bool> {
        let mut ix = self
            .index
            .lock()
            .map_err(|_| io::Error::other("index poisoned"))?;
        let Some(p) = ix.partial(&self.rel).map_err(index_err)? else {
            return Ok(false);
        };
        if p.expected_hash != self.whole_hash || p.size != self.size {
            // A record for a different expected file: it can never be adopted.
            ix.remove_partial(&self.rel).map_err(index_err)?;
            return Ok(false);
        }
        drop(ix);
        let ranges = coalesce(
            p.ranges
                .ranges
                .iter()
                .map(|r| (r.offset, r.offset.saturating_add(r.length)))
                .collect(),
        );
        let file_len = self.file.metadata()?.len();
        for (i, c) in self.chunks.iter().enumerate() {
            let end = c.offset + u64::from(c.len);
            let covered = ranges.iter().any(|&(s, e)| s <= c.offset && end <= e);
            if covered && end <= file_len && hash_at(&self.file, c)? == c.hash {
                self.verified[i] = true;
            }
        }
        Ok(true)
    }

    /// Adopt chunks of a `.part` file that has no index record by re-chunking it.
    fn adopt_unrecorded(&mut self) -> io::Result<()> {
        if self.file.metadata()?.len() == 0 {
            return Ok(());
        }
        let found = digest_held(&self.file)?;
        for f in found.chunks {
            if let Ok(i) = self.chunks.binary_search_by_key(&f.offset, |c| c.offset) {
                if self.chunks[i] == f {
                    self.verified[i] = true;
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

    /// Verify `data` against chunk `idx` and write it at the chunk's offset.
    pub async fn write_chunk(&mut self, idx: u32, data: &[u8]) -> io::Result<()> {
        let c = *self.chunks.get(idx as usize).ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "chunk index out of range")
        })?;
        if data.len() != c.len as usize || *blake3::hash(data).as_bytes() != c.hash {
            return Err(invalid(format!("chunk {idx} does not match its hash")));
        }
        #[cfg(feature = "test-hooks")]
        let inject = self.fail_at == Some(idx);
        #[cfg(not(feature = "test-hooks"))]
        let inject = false;
        // A hole (`set_len` past the opened length) already reads as zeros;
        // below `hole_from` an earlier writer's bytes may be there.
        let hole = self.sparse && c.offset >= self.hole_from && data.iter().all(|&b| b == 0);
        let res = if hole && !inject {
            Ok(())
        } else {
            let file = self.file.clone();
            let buf = data.to_vec();
            tokio::task::spawn_blocking(move || {
                if inject {
                    return Err(io::Error::from_raw_os_error(nix::libc::ENOSPC));
                }
                file.write_all_at(&buf, c.offset)
            })
            .await
            .map_err(io::Error::other)
            .and_then(|r| r)
        };
        if let Err(e) = res {
            if let Err(pe) = self.persist().await {
                tracing::warn!(path = ?self.part, error = %pe, "persisting partial after write error failed");
            }
            return Err(e);
        }
        if !self.verified[idx as usize] {
            self.verified[idx as usize] = true;
            self.chunks_since_persist += 1;
        }
        if self.chunks_since_persist >= PERSIST_EVERY_CHUNKS
            || self.last_persist.elapsed() >= PERSIST_EVERY
        {
            self.persist().await?;
        }
        Ok(())
    }

    /// Read chunk `idx`'s bytes from `from` at `offset`, then write them like `write_chunk`.
    pub async fn copy_local(&mut self, idx: u32, from: &Path, offset: u64) -> io::Result<()> {
        let c = *self.chunks.get(idx as usize).ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "chunk index out of range")
        })?;
        let from = from.to_path_buf();
        let data = tokio::task::spawn_blocking(move || {
            let mut buf = vec![0u8; c.len as usize];
            crate::fs::safe::open_regular_nofollow(&from)?.read_exact_at(&mut buf, offset)?;
            Ok::<_, io::Error>(buf)
        })
        .await
        .map_err(io::Error::other)??;
        self.write_chunk(idx, &data).await
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
        self.last_persist = Instant::now();
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
        self.last_persist = Instant::now();
        Ok(())
    }

    /// Verify the whole `.part`, apply metadata and rename it over the destination.
    /// On any error the `.part` and its record stay; nothing appears at the final name.
    pub async fn finalize(
        mut self,
        entry: &FileEntry,
        opts: &ApplyOpts,
    ) -> Result<(), FinalizeError> {
        if !self.missing_chunks().is_empty() {
            return Err(FinalizeError::Incomplete);
        }
        self.persist().await?;
        let (file, part, dest, size, expected) = (
            self.file.clone(),
            self.part.clone(),
            self.dest.clone(),
            self.size,
            self.whole_hash,
        );
        let (entry, opts) = (entry.clone(), *opts);
        tokio::task::spawn_blocking(move || {
            file.set_len(size)?;
            file.sync_all()?;
            let actual = digest_held(&file)?.hash;
            if actual != expected {
                return Err(FinalizeError::HashMismatch { expected, actual });
            }
            match std::fs::symlink_metadata(&dest) {
                Ok(m) if m.is_dir() => {
                    return Err(FinalizeError::Io(io::Error::other(format!(
                        "refusing to replace directory {} with a file",
                        dest.display()
                    ))));
                }
                Ok(_) => {}
                Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                Err(e) => return Err(e.into()),
            }
            // Metadata goes through the held descriptor; the rename (by
            // path) only once the path still names that very file.
            let placed = apply_metadata_fd(&*file, &entry, &opts)
                .and_then(|()| still_same_file(&file, &part))
                .and_then(|()| {
                    std::fs::rename(&part, &dest).map_err(|e| {
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
                return Err(e.into());
            }
            Ok(())
        })
        .await
        .map_err(io::Error::other)??;
        self.done = true;
        let (index, rel) = (self.index.clone(), self.rel.clone());
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
            tracing::warn!(path = ?self.dest, error = %e, "removing partial record failed");
        }
        Ok(())
    }
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
