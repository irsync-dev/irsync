//! Chunk lists in parts: a list travels as a `ChunkListStart` and then
//! `ChunkListMore` messages (sender), reassembled into one `FileDigest` on
//! the receiver. Pure functions; the roles drive them.
//!
//! Chunk lists travel in room the receiver grants (`limits::LIST_BUDGET`
//! records per connection): the sender sends a file's list only once its
//! announced `total` fits in the room it has left ([`ListCredit`]); the
//! receiver reserves that total at the list's start and grants it back
//! with `ListRoom` once it drops the list ([`ListBudget`]).

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

use irsync_proto::chunk::{ChunkRecord, FileDigest, CHUNK_PARAMS};
use irsync_proto::v1::{control::Msg, ChunkListMore, ChunkListStart, Hash, SourceStat};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

/// Records per part: about 1.4 MiB on the wire, a tenth of a control frame.
pub(super) const PART_RECORDS: usize = 32 * 1024;

/// Most chunks a list may announce (`total`): bounds what a receiver holds
/// for one file (about 800 MB of records; 4 TiB of typical data).
pub(super) const MAX_LIST_CHUNKS: usize = 1 << 24;

/// The stat a chunk list was hashed under, as its start carries it: the
/// receiver places the file with it, not the walk's, and `-H`'s link check
/// compares it. `user` and `group` name `stat.uid` and `stat.gid` on the
/// sender (empty when they have no name).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct ListStat {
    pub stat: SourceStat,
    pub user: String,
    pub group: String,
}

/// The sender's side of a `SourceStat`: what its own stat is compared with.
pub(super) fn freshness_of(s: &SourceStat) -> crate::hasher::Freshness {
    crate::hasher::Freshness {
        size: s.size,
        mtime: (s.mtime_sec, s.mtime_nsec),
        ctime: (s.ctime_sec, s.ctime_nsec),
        inode: s.inode,
        mode: s.mode,
        uid: s.uid,
        gid: s.gid,
    }
}

/// Cuts a digest into a `ChunkListStart` and `ChunkListMore` messages of at
/// most `per_part` records each, in order. `with_hash` false leaves the
/// whole-file hash out (an `APPEND` tail). `stat` is what the file was
/// hashed under; it rides on the start.
pub(super) fn split(
    file_id: u64,
    d: &FileDigest,
    stat: ListStat,
    per_part: usize,
    with_hash: bool,
) -> Vec<Msg> {
    let per_part = per_part.max(1);
    let records = |from: usize| {
        d.chunks[from..(from + per_part).min(d.chunks.len())]
            .iter()
            .map(|&c| c.into())
            .collect::<Vec<_>>()
    };
    let mut out = vec![Msg::ChunkListStart(ChunkListStart {
        file_id,
        whole_hash: with_hash.then(|| Hash {
            bytes: d.hash.to_vec(),
        }),
        total: d.chunks.len() as u32,
        stat: Some(stat.stat),
        user: stat.user,
        group: stat.group,
        chunks: records(0),
    })];
    out.extend((per_part..d.chunks.len()).step_by(per_part).map(|first| {
        Msg::ChunkListMore(ChunkListMore {
            file_id,
            first: first as u32,
            chunks: records(first),
        })
    }));
    out
}

/// A complete chunk list, and the stat the file was hashed under.
pub(super) type Assembled = (FileDigest, ListStat);

/// A chunk list under assembly from its parts.
pub(super) struct Assembly {
    digest: FileDigest,
    stat: ListStat,
    total: usize,
    /// Where the next record starts.
    next: u64,
}

/// Starts the file's assembly slot over with `s` (a re-digested file comes
/// as a new list), whose first record is at `from`: 0, or an `APPEND`
/// tail's `append_from`. `Ok(Some)` is the list when `s` holds all of it,
/// with the stat it was hashed under; `Ok(None)` means more parts are due;
/// `Err` names what was wrong (the slot is then cleared).
pub(super) fn start(
    slot: &mut Option<Assembly>,
    s: ChunkListStart,
    from: u64,
) -> Result<Option<Assembled>, String> {
    *slot = None;
    let total = s.total as usize;
    // Absent for an `APPEND` tail (the receiver folds the hash itself);
    // zeros then, which no real file hashes to.
    let hash: [u8; 32] = match &s.whole_hash {
        Some(h) => h
            .bytes
            .as_slice()
            .try_into()
            .map_err(|_| "whole-file hash is not 32 bytes".to_string())?,
        None => [0; 32],
    };
    let Some(stat) = s.stat else {
        return Err("a chunk list without the stat it was hashed under".into());
    };
    if total > MAX_LIST_CHUNKS {
        return Err(format!(
            "{total} chunks announced, above the cap of {MAX_LIST_CHUNKS}"
        ));
    }
    // Chunks are at least `min` long (a file shorter than that is one chunk).
    let possible = stat.size / u64::from(CHUNK_PARAMS.min) + 1;
    if total as u64 > possible {
        return Err(format!(
            "{total} chunks announced for a size of {} bytes",
            stat.size
        ));
    }
    *slot = Some(Assembly {
        digest: FileDigest {
            size: stat.size,
            hash,
            chunks: Vec::with_capacity(total.min(PART_RECORDS * 4)),
        },
        stat: ListStat {
            stat,
            user: s.user,
            group: s.group,
        },
        total,
        next: from,
    });
    finish(slot, s.chunks)
}

/// Feeds the next records of the file's list into its assembly slot; as
/// [`start`] for the result.
pub(super) fn more(
    slot: &mut Option<Assembly>,
    m: ChunkListMore,
) -> Result<Option<Assembled>, String> {
    let Some(a) = slot.as_mut() else {
        return Err(format!("part continues at {} without a start", m.first));
    };
    if m.first as usize != a.digest.chunks.len() {
        let e = format!(
            "part continues at {} but {} records are assembled",
            m.first,
            a.digest.chunks.len()
        );
        *slot = None;
        return Err(e);
    }
    finish(slot, m.chunks)
}

/// Adds `chunks` to the list in `slot`: the list, once all its records
/// have come. An error clears the slot.
fn finish(
    slot: &mut Option<Assembly>,
    chunks: Vec<irsync_proto::v1::ChunkRecord>,
) -> Result<Option<Assembled>, String> {
    let Some(a) = slot.as_mut() else {
        return Err("no list under assembly".into());
    };
    let added = (|| {
        if a.digest.chunks.len() + chunks.len() > a.total {
            return Err(format!(
                "{} records exceeds the {} announced",
                a.digest.chunks.len() + chunks.len(),
                a.total
            ));
        }
        for c in chunks {
            let c = ChunkRecord::from_wire(c, a.next).map_err(|e| format!("bad record: {e}"))?;
            a.next = c.end();
            a.digest.chunks.push(c);
        }
        Ok(a.digest.chunks.len() == a.total)
    })();
    match added {
        Ok(true) => Ok(slot.take().map(|a| (a.digest, a.stat))),
        Ok(false) => Ok(None),
        Err(e) => {
            *slot = None;
            Err(e)
        }
    }
}

/// Receiver: the chunk records held for all files' lists, against the
/// room granted to the sender. A list's start reserves its announced
/// `total` whole, before anything is assembled or allocated; the later
/// parts are covered by it (assembly refuses more than the total).
pub(super) struct ListBudget {
    limit: u64,
    held: HashMap<u64, u64>,
    total: u64,
}

impl ListBudget {
    pub fn new(limit: u64) -> Self {
        ListBudget {
            limit,
            held: HashMap::new(),
            total: 0,
        }
    }

    /// Reserves a list of `total` records for `file_id` (at its start, its
    /// announced total; 0 for a withdrawal). It replaces the file's earlier list: what
    /// that held is released first and returned, for the caller to grant
    /// back. `Err` when the records held would exceed the limit; nothing is
    /// reserved then (an earlier list stays held).
    pub(crate) fn reserve(&mut self, file_id: u64, total: u64) -> Result<u64, String> {
        let old = self.held.get(&file_id).copied().unwrap_or(0);
        let held = self.total - old + total;
        if held > self.limit {
            return Err(format!(
                "{total} records with {} held, beyond the {} granted",
                self.total - old,
                self.limit
            ));
        }
        self.total = held;
        self.held.insert(file_id, total);
        Ok(old)
    }

    /// Drops what the file holds; returns it, for the caller to grant back.
    pub fn release(&mut self, file_id: u64) -> u64 {
        let n = self.held.remove(&file_id).unwrap_or(0);
        self.total -= n;
        n
    }

    /// Records held now, across files.
    #[cfg_attr(not(any(test, feature = "test-hooks")), allow(dead_code))]
    pub fn total(&self) -> u64 {
        self.total
    }
}

/// Sender: chunk lists waiting for room the receiver has granted. A file's
/// list goes out whole once its total fits, never in part: two half-sent
/// lists could otherwise each wait for room only the other frees. First
/// in, first out, so a large list is not overtaken forever.
///
/// The sender digests ahead of the room only as much as the room holds:
/// a digest takes [`ListCredit::ahead`] permits, one per record it may
/// have (see [`records_at_most`]), before it reads the file, and its list
/// keeps them until it goes out. So the lists digested and waiting, and
/// the digests running, never hold more records than one room, however
/// many files are in flight; a file that needs the whole room is digested
/// alone.
pub(super) struct ListCredit {
    room: u64,
    /// The room a connection starts with.
    limit: u64,
    queue: VecDeque<(u64, Vec<Msg>, Option<OwnedSemaphorePermit>)>,
    ahead: Arc<Semaphore>,
}

/// The most chunk records a file of `size` bytes can have (every chunk but
/// the last is at least `CHUNK_PARAMS.min`), at least one and at most
/// `limit`: the permits its digest takes (see [`ListCredit`]).
pub(super) fn records_at_most(size: u64, limit: u32) -> u32 {
    let n = size / u64::from(CHUNK_PARAMS.min) + 1;
    u32::try_from(n).unwrap_or(u32::MAX).min(limit).max(1)
}

impl ListCredit {
    pub fn new(room: u64) -> Self {
        ListCredit {
            room,
            limit: room,
            queue: VecDeque::new(),
            ahead: Arc::new(Semaphore::new(Self::permits(room) as usize)),
        }
    }

    /// The permits of digests ahead of the room: one per record of room.
    fn permits(room: u64) -> u32 {
        u32::try_from(room).unwrap_or(u32::MAX).max(1)
    }

    /// What a digest waits on before it reads the file: the semaphore, and
    /// the most permits one digest takes. It hands its permits to
    /// [`ListCredit::push`] with its list.
    pub fn ahead(&self) -> (Arc<Semaphore>, u32) {
        (self.ahead.clone(), Self::permits(self.limit))
    }

    /// Queues a file's whole list, with the permits its digest took.
    pub fn push(&mut self, file_id: u64, parts: Vec<Msg>, held: Option<OwnedSemaphorePermit>) {
        self.queue.push_back((file_id, parts, held));
    }

    /// Room the receiver freed. It frees only what our lists took, so the
    /// room never exceeds what the connection started with; `Err` (nothing
    /// is granted) if it would.
    pub fn grant(&mut self, records: u64) -> Result<(), String> {
        match self.room.checked_add(records) {
            Some(room) if room <= self.limit => {
                self.room = room;
                Ok(())
            }
            _ => Err(format!(
                "{records} records granted with {} left, above the {} a connection starts with",
                self.room, self.limit
            )),
        }
    }

    /// Drops the file's queued list (it was superseded or given up);
    /// false if none was queued.
    pub fn cancel(&mut self, file_id: u64) -> bool {
        let before = self.queue.len();
        self.queue.retain(|(id, _, _)| *id != file_id);
        self.queue.len() != before
    }

    /// The records of the lists waiting for room.
    #[cfg(feature = "test-hooks")]
    pub fn queued(&self) -> u64 {
        self.queue.iter().map(|(_, p, _)| list_total(p)).sum()
    }

    /// The parts of every file, from the front, whose list fits in the room
    /// left; their room is taken.
    pub fn ready(&mut self) -> Vec<Msg> {
        let mut out = Vec::new();
        while let Some((_, parts, _)) = self.queue.front() {
            let total = list_total(parts);
            if total > self.room {
                break;
            }
            self.room -= total;
            // Out under the room: its digest's permits go back.
            out.extend(
                self.queue
                    .pop_front()
                    .map(|(_, p, _)| p)
                    .unwrap_or_default(),
            );
        }
        out
    }
}

/// The records a list announces (its start carries the total).
pub(super) fn list_total(parts: &[Msg]) -> u64 {
    match parts.first() {
        Some(Msg::ChunkListStart(s)) => u64::from(s.total),
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn st(size: u64) -> ListStat {
        ListStat {
            stat: SourceStat {
                size,
                mtime_sec: 1_700_000_000,
                mtime_nsec: 123_456_789,
                ctime_sec: 1_700_000_001,
                ctime_nsec: 5,
                inode: 77,
                mode: 0o100640,
                uid: 1001,
                gid: 1002,
            },
            user: "alice".into(),
            group: "staff".into(),
        }
    }

    fn digest(n: usize) -> FileDigest {
        let mut chunks = Vec::with_capacity(n);
        let mut offset = 0;
        for i in 0..n {
            let len = CHUNK_PARAMS.min + (i as u32 % 7) * 1024;
            let mut hash = [0u8; 32];
            hash[..8].copy_from_slice(&(i as u64).to_le_bytes());
            chunks.push(ChunkRecord { offset, len, hash });
            offset += u64::from(len);
        }
        FileDigest {
            size: offset,
            hash: [7u8; 32],
            chunks,
        }
    }

    /// The parts of `d`'s list, `per_part` records each.
    fn parts(d: &FileDigest, per_part: usize, with_hash: bool) -> Vec<Msg> {
        split(42, d, st(d.size), per_part, with_hash)
    }

    fn feed(slot: &mut Option<Assembly>, m: Msg) -> Result<Option<Assembled>, String> {
        match m {
            Msg::ChunkListStart(s) => start(slot, s, 0),
            Msg::ChunkListMore(m) => more(slot, m),
            m => panic!("not a part: {m:?}"),
        }
    }

    fn run_with_stat(parts: Vec<Msg>) -> Result<Option<Assembled>, String> {
        let mut slot = None;
        let mut out = None;
        for p in parts {
            out = feed(&mut slot, p)?;
        }
        Ok(out)
    }

    fn run(parts: Vec<Msg>) -> Result<Option<FileDigest>, String> {
        Ok(run_with_stat(parts)?.map(|(d, _)| d))
    }

    fn start_of(m: &mut Msg) -> &mut ChunkListStart {
        match m {
            Msg::ChunkListStart(s) => s,
            m => panic!("not a start: {m:?}"),
        }
    }

    #[test]
    fn a_tail_list_without_a_whole_hash_assembles_with_zeros() {
        let d = digest(3);
        let mut ps = parts(&d, 4, false);
        assert!(start_of(&mut ps[0]).whole_hash.is_none());
        let (got, _) = run_with_stat(ps).unwrap().unwrap();
        assert_eq!(got.chunks, d.chunks);
        assert_eq!(got.hash, [0; 32]);
    }

    #[test]
    fn the_hashed_stat_rides_on_the_start() {
        let d = digest(10);
        let mut ps = parts(&d, 4, true);
        let s = start_of(&mut ps[0]);
        assert_eq!(s.stat, Some(st(d.size).stat));
        assert_eq!((s.user.as_str(), s.group.as_str()), ("alice", "staff"));
        let (cl, stat) = run_with_stat(ps).unwrap().expect("complete");
        assert_eq!(cl.size, d.size);
        assert_eq!(stat, st(d.size));
    }

    #[test]
    fn a_file_reserves_at_least_the_records_it_can_have_within_the_room() {
        let min = u64::from(CHUNK_PARAMS.min);
        assert_eq!(records_at_most(0, 24), 1);
        assert_eq!(records_at_most(min - 1, 24), 1);
        // Two chunks: one at the minimum and a short tail.
        assert_eq!(records_at_most(min + 1, 24), 2);
        assert_eq!(records_at_most(100 * min, 24), 24);
        assert_eq!(records_at_most(u64::MAX, u32::MAX), u32::MAX);
        let bytes: Vec<u8> = (0..3 * min as u32 + 5).map(|i| (i % 251) as u8).collect();
        let d = irsync_proto::chunk::digest_bytes(&bytes);
        assert!(d.chunks.len() as u32 <= records_at_most(bytes.len() as u64, u32::MAX));
    }

    #[test]
    fn split_cuts_a_start_and_contiguous_parts() {
        let d = digest(10);
        let ps = parts(&d, 4, true);
        assert_eq!(ps.len(), 3);
        let mut records = Vec::new();
        for (k, p) in ps.iter().enumerate() {
            let chunks = match p {
                Msg::ChunkListStart(s) => {
                    assert_eq!(k, 0);
                    assert_eq!((s.file_id, s.total), (42, 10));
                    assert_eq!(s.whole_hash.as_ref().unwrap().bytes, d.hash.to_vec());
                    &s.chunks
                }
                Msg::ChunkListMore(m) => {
                    assert!(k > 0);
                    assert_eq!(m.file_id, 42);
                    assert_eq!(m.first as usize, records.len());
                    &m.chunks
                }
                m => panic!("{m:?}"),
            };
            assert!(chunks.len() <= 4);
            for c in chunks {
                let at = records.last().map_or(0, ChunkRecord::end);
                records.push(ChunkRecord::from_wire(c.clone(), at).unwrap());
            }
        }
        assert_eq!(records, d.chunks);
    }

    #[test]
    fn split_of_an_empty_digest_is_one_empty_start() {
        let mut ps = parts(&digest(0), 4, true);
        assert_eq!(ps.len(), 1);
        let s = start_of(&mut ps[0]);
        assert!(s.chunks.is_empty());
        assert_eq!(s.total, 0);
        assert!(run(ps).unwrap().unwrap().chunks.is_empty());
    }

    #[test]
    fn assemble_rebuilds_the_list_from_its_parts() {
        let d = digest(10);
        let cl = run(parts(&d, 4, true)).unwrap().expect("complete");
        assert_eq!(cl.size, d.size);
        assert_eq!(cl.hash, d.hash);
        assert_eq!(cl.chunks, d.chunks);
    }

    #[test]
    fn a_tail_list_starts_where_the_tail_does() {
        let d = digest(3);
        let mut slot = None;
        let mut out = None;
        for p in parts(&d, 2, false) {
            out = match p {
                Msg::ChunkListStart(s) => start(&mut slot, s, 1000),
                Msg::ChunkListMore(m) => more(&mut slot, m),
                m => panic!("{m:?}"),
            }
            .unwrap();
        }
        let (got, _) = out.expect("complete");
        let offsets: Vec<u64> = got.chunks.iter().map(|c| c.offset).collect();
        let want: Vec<u64> = d.chunks.iter().map(|c| c.offset + 1000).collect();
        assert_eq!(offsets, want);
    }

    #[test]
    fn assemble_waits_for_the_last_part() {
        let mut ps = parts(&digest(10), 4, true);
        ps.pop();
        assert_eq!(run(ps).unwrap(), None);
    }

    #[test]
    fn a_start_restarts_the_assembly() {
        let d = digest(10);
        let mut ps = parts(&d, 4, true);
        let restart = ps[0].clone();
        ps.insert(1, restart);
        let cl = run(ps).unwrap().expect("complete");
        assert_eq!(cl.chunks.len(), 10);
    }

    #[test]
    fn a_part_that_skips_ahead_is_an_error() {
        let mut ps = parts(&digest(10), 4, true);
        ps.remove(1);
        assert!(run(ps).unwrap_err().contains("continues at"));
    }

    #[test]
    fn a_continuation_without_a_start_is_an_error() {
        let ps = parts(&digest(10), 4, true);
        assert!(run(ps[1..].to_vec()).unwrap_err().contains("continues at"));
    }

    #[test]
    fn more_records_than_announced_is_an_error() {
        let mut ps = parts(&digest(10), 4, true);
        start_of(&mut ps[0]).total = 9;
        assert!(run(ps).unwrap_err().contains("exceeds"));
    }

    #[test]
    fn a_start_without_a_stat_is_an_error() {
        let mut ps = parts(&digest(2), 4, true);
        start_of(&mut ps[0]).stat = None;
        assert!(run(ps).unwrap_err().contains("stat"));
    }

    #[test]
    fn a_total_above_the_cap_is_refused_before_anything_is_held() {
        let mut ps = parts(&digest(2), 4, true);
        let s = start_of(&mut ps[0]);
        s.total = (MAX_LIST_CHUNKS + 1) as u32;
        s.stat.as_mut().unwrap().size = u64::MAX;
        assert!(run(ps).unwrap_err().contains("cap"));
    }

    #[test]
    fn a_total_impossible_for_the_size_is_refused() {
        let mut ps = parts(&digest(2), 4, true);
        // 2 chunks of at least 64 KiB cannot describe a 10-byte file.
        start_of(&mut ps[0]).stat.as_mut().unwrap().size = 10;
        assert!(run(ps).unwrap_err().contains("size"));
    }

    #[test]
    fn a_reservation_beyond_the_limit_is_refused() {
        let mut b = ListBudget::new(10);
        assert_eq!(b.reserve(1, 6), Ok(0));
        assert!(b.reserve(2, 5).is_err());
        // Refused: nothing was taken.
        assert_eq!(b.total(), 6);
        assert_eq!(b.reserve(2, 4), Ok(0));
        assert_eq!(b.total(), 10);
        assert_eq!(b.release(1), 6);
        assert_eq!(b.release(1), 0);
        assert_eq!(b.total(), 4);
    }

    #[test]
    fn a_new_start_releases_the_old_list() {
        let mut b = ListBudget::new(10);
        assert_eq!(b.reserve(1, 10), Ok(0));
        assert_eq!(b.reserve(1, 4), Ok(10));
        assert_eq!(b.total(), 4);
        assert_eq!(b.release(1), 4);
        assert_eq!(b.total(), 0);
    }

    /// Parts of a list of `n` records, one record per part.
    fn list(file_id: u64, n: usize) -> Vec<Msg> {
        let d = digest(n);
        split(file_id, &d, st(d.size), 1, true)
    }

    fn ids(parts: &[Msg]) -> Vec<(u64, u32)> {
        parts
            .iter()
            .map(|p| match p {
                Msg::ChunkListStart(s) => (s.file_id, 0),
                Msg::ChunkListMore(m) => (m.file_id, m.first),
                m => panic!("{m:?}"),
            })
            .collect()
    }

    #[test]
    fn credit_sends_whole_files_in_order() {
        let mut c = ListCredit::new(10);
        c.push(1, list(1, 8), None);
        c.push(2, list(2, 5), None);
        let out = c.ready();
        assert_eq!(out.len(), 8);
        assert!(ids(&out).iter().all(|&(id, _)| id == 1));
        assert!(c.ready().is_empty());
        c.grant(8).unwrap();
        let out = c.ready();
        assert_eq!(ids(&out), (0..5).map(|k| (2, k)).collect::<Vec<_>>());
        assert!(c.ready().is_empty());
    }

    #[test]
    fn credit_never_splits_a_file() {
        let mut c = ListCredit::new(10);
        c.push(1, list(1, 6), None);
        assert_eq!(c.ready().len(), 6);
        c.push(2, list(2, 5), None);
        // 5 records never fit in the 4 left, not even in part.
        assert!(c.ready().is_empty());
        c.push(3, list(3, 1), None);
        // ...and nothing overtakes the file at the front.
        assert!(c.ready().is_empty());
        c.grant(1).unwrap();
        assert_eq!(c.ready().len(), 5);
        c.grant(1).unwrap();
        assert_eq!(ids(&c.ready()), vec![(3, 0)]);
    }

    #[test]
    fn room_above_the_start_is_refused() {
        let mut c = ListCredit::new(10);
        c.push(1, list(1, 6), None);
        assert_eq!(c.ready().len(), 6);
        assert!(c.grant(7).is_err());
        // Refused: nothing was granted.
        c.push(2, list(2, 5), None);
        assert!(c.ready().is_empty());
        c.grant(6).unwrap();
        assert_eq!(c.ready().len(), 5);
        assert!(c.grant(u64::MAX).is_err());
    }

    #[test]
    fn an_empty_list_needs_no_room() {
        let mut c = ListCredit::new(0);
        c.push(1, list(1, 0), None);
        assert_eq!(c.ready().len(), 1);
    }

    #[test]
    fn a_cancelled_file_leaves_the_queue() {
        let mut c = ListCredit::new(4);
        c.push(1, list(1, 8), None);
        c.push(2, list(2, 3), None);
        assert!(c.cancel(1));
        assert!(!c.cancel(1));
        assert_eq!(ids(&c.ready()), vec![(2, 0), (2, 1), (2, 2)]);
    }

    #[test]
    fn a_malformed_hash_is_an_error_and_a_missing_one_is_zeros() {
        let mut ps = parts(&digest(2), 4, true);
        start_of(&mut ps[0]).whole_hash = Some(Hash { bytes: vec![1; 31] });
        assert!(run(ps).unwrap_err().contains("hash"));
        // Absent (an APPEND tail): assembled with zeros.
        let ps = parts(&digest(2), 4, false);
        assert_eq!(run(ps).unwrap().unwrap().hash, [0; 32]);
    }
}
