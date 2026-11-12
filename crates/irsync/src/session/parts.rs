//! Chunk lists in parts: a list travels as a sequence of `ChunkListPart`
//! messages (sender), reassembled into one `FileDigest` on the receiver.
//! Pure functions; the roles drive them.
//!
//! Chunk lists travel in room the receiver grants (`limits::LIST_BUDGET`
//! records per connection): the sender sends a file's list only once its
//! announced `total` fits in the room it has left ([`ListCredit`]); the
//! receiver reserves that total at the list's first part and grants it back
//! with `ListRoom` once it drops the list ([`ListBudget`]).

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

use irsync_proto::chunk::{ChunkRecord, FileDigest, CHUNK_PARAMS};
use irsync_proto::v1::{ChunkListPart, Hash};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

/// Records per part: about 1.6 MiB on the wire, a tenth of a control frame.
pub(super) const PART_RECORDS: usize = 32 * 1024;

/// Most chunks a list may announce (`total`): bounds what a receiver holds
/// for one file (about 800 MB of records; 4 TiB of typical data).
pub(super) const MAX_LIST_CHUNKS: usize = 1 << 24;

/// The metadata of the stat a file was hashed under, besides its size: the
/// receiver places the file with these, not the walk's. `user` and `group`
/// name `uid` and `gid` on the sender (empty when they have no name).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct ListStat {
    pub mtime: (i64, u32),
    pub mode: u32,
    pub uid: u32,
    pub gid: u32,
    pub user: String,
    pub group: String,
}

/// Cuts a digest into parts of at most `per_part` records, in order.
/// `with_hash` false leaves the whole-file hash out (an `APPEND` tail).
/// `stat` is what the file was hashed under; it rides on the first part,
/// with the size.
pub(super) fn split(
    file_id: u64,
    d: &FileDigest,
    stat: &ListStat,
    per_part: usize,
    with_hash: bool,
) -> Vec<ChunkListPart> {
    let per_part = per_part.max(1);
    let total = d.chunks.len() as u32;
    let hash = with_hash.then(|| Hash {
        bytes: d.hash.to_vec(),
    });
    let n = d.chunks.len().div_ceil(per_part).max(1);
    (0..n)
        .map(|k| {
            let first = k * per_part;
            let end = (first + per_part).min(d.chunks.len());
            let mut part = ChunkListPart {
                file_id,
                size: d.size,
                whole_hash: hash.clone(),
                total,
                first: first as u32,
                chunks: d.chunks[first..end].iter().map(|&c| c.into()).collect(),
                last: k + 1 == n,
                ..ChunkListPart::default()
            };
            if k == 0 {
                (part.mtime_sec, part.mtime_nsec) = stat.mtime;
                (part.mode, part.uid, part.gid) = (stat.mode, stat.uid, stat.gid);
                (part.user, part.group) = (stat.user.clone(), stat.group.clone());
            }
            part
        })
        .collect()
}

/// A complete chunk list, and the stat the file was hashed under.
pub(super) type Assembled = (FileDigest, ListStat);

/// A chunk list under assembly from its parts.
pub(super) struct Assembly {
    digest: FileDigest,
    stat: ListStat,
    total: usize,
}

/// Feeds one part into the file's assembly slot. `Ok(Some)` is the complete
/// list with the stat it was hashed under; `Ok(None)` means more parts are due; `Err` names what was wrong
/// with the sequence (the slot is then cleared).
pub(super) fn assemble(
    slot: &mut Option<Assembly>,
    part: ChunkListPart,
) -> Result<Option<Assembled>, String> {
    match feed(slot, part) {
        Ok(done) => Ok(done),
        Err(e) => {
            *slot = None;
            Err(e)
        }
    }
}

fn feed(slot: &mut Option<Assembly>, mut part: ChunkListPart) -> Result<Option<Assembled>, String> {
    let total = part.total as usize;
    // Absent for an `APPEND` tail (the receiver folds the hash itself);
    // zeros then, which no real file hashes to.
    let hash: [u8; 32] = match &part.whole_hash {
        Some(h) => h
            .bytes
            .as_slice()
            .try_into()
            .map_err(|_| "whole-file hash is not 32 bytes".to_string())?,
        None => [0; 32],
    };
    if part.first == 0 {
        // A first part starts over (a re-digested file comes as a new sequence).
        if total > MAX_LIST_CHUNKS {
            return Err(format!(
                "{total} chunks announced, above the cap of {MAX_LIST_CHUNKS}"
            ));
        }
        // Chunks are at least `min` long (a file shorter than that is one chunk).
        let possible = part.size / u64::from(CHUNK_PARAMS.min) + 1;
        if total as u64 > possible {
            return Err(format!(
                "{total} chunks announced for a size of {} bytes",
                part.size
            ));
        }
        *slot = Some(Assembly {
            stat: ListStat {
                mtime: (part.mtime_sec, part.mtime_nsec),
                mode: part.mode,
                uid: part.uid,
                gid: part.gid,
                user: std::mem::take(&mut part.user),
                group: std::mem::take(&mut part.group),
            },
            digest: FileDigest {
                size: part.size,
                hash,
                chunks: Vec::with_capacity(total.min(PART_RECORDS * 4)),
            },
            total,
        });
    }
    let Some(a) = slot.as_mut() else {
        return Err(format!("part continues at {} without a start", part.first));
    };
    if part.first as usize != a.digest.chunks.len() {
        return Err(format!(
            "part continues at {} but {} records are assembled",
            part.first,
            a.digest.chunks.len()
        ));
    }
    if part.size != a.digest.size || hash != a.digest.hash || total != a.total {
        return Err("part describes another file (size, hash or total differ)".into());
    }
    if a.digest.chunks.len() + part.chunks.len() > a.total {
        return Err(format!(
            "{} records exceeds the {} announced",
            a.digest.chunks.len() + part.chunks.len(),
            a.total
        ));
    }
    for c in part.chunks {
        let c = ChunkRecord::try_from(c).map_err(|e| format!("bad record: {e}"))?;
        a.digest.chunks.push(c);
    }
    if !part.last {
        return Ok(None);
    }
    if a.digest.chunks.len() != a.total {
        return Err(format!(
            "last part leaves the list short: {} of {} records",
            a.digest.chunks.len(),
            a.total
        ));
    }
    Ok(slot.take().map(|a| (a.digest, a.stat)))
}

/// Receiver: the chunk records held for all files' lists, against the
/// room granted to the sender. A list's first part reserves its announced
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

    /// Reserves a list of `total` records for `file_id` (at its first
    /// part, its announced total). It replaces the file's earlier list: what
    /// that held is released first and returned, for the caller to grant
    /// back. `Err` when the records held would exceed the limit; nothing is
    /// reserved then (an earlier list stays held).
    pub fn reserve(&mut self, file_id: u64, total: u64) -> Result<u64, String> {
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
    queue: VecDeque<(u64, Vec<ChunkListPart>, Option<OwnedSemaphorePermit>)>,
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
    pub fn push(
        &mut self,
        file_id: u64,
        parts: Vec<ChunkListPart>,
        held: Option<OwnedSemaphorePermit>,
    ) {
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
    pub fn ready(&mut self) -> Vec<ChunkListPart> {
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

/// The records a list announces (every part carries the total).
pub(super) fn list_total(parts: &[ChunkListPart]) -> u64 {
    parts.first().map_or(0, |p| u64::from(p.total))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn st() -> ListStat {
        ListStat {
            mtime: (1_700_000_000, 123_456_789),
            mode: 0o100640,
            uid: 1001,
            gid: 1002,
            user: "alice".into(),
            group: "staff".into(),
        }
    }

    #[test]
    fn a_tail_list_without_a_whole_hash_assembles_with_zeros() {
        let d = digest(3);
        let mut parts = split(9, &d, &st(), 4, false);
        assert!(parts.iter().all(|p| p.whole_hash.is_none()));
        let mut slot = None;
        let (got, _) = assemble(&mut slot, parts.remove(0)).unwrap().unwrap();
        assert_eq!(got.chunks, d.chunks);
        assert_eq!(got.hash, [0; 32]);
    }

    #[test]
    fn the_hashed_stat_rides_on_the_first_part() {
        let d = digest(10);
        let parts = split(42, &d, &st(), 4, true);
        let p = &parts[0];
        assert_eq!((p.mtime_sec, p.mtime_nsec), st().mtime);
        assert_eq!((p.mode, p.uid, p.gid), (0o100640, 1001, 1002));
        assert_eq!((p.user.as_str(), p.group.as_str()), ("alice", "staff"));
        assert!(parts[1..].iter().all(|p| {
            (p.mtime_sec, p.mtime_nsec, p.mode, p.uid, p.gid) == (0, 0, 0, 0, 0)
                && p.user.is_empty()
                && p.group.is_empty()
        }));
        let (cl, stat) = run_with_stat(parts).unwrap().expect("complete");
        assert_eq!(cl.size, d.size);
        assert_eq!(stat, st());
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

    fn run_with_stat(parts: Vec<ChunkListPart>) -> Result<Option<Assembled>, String> {
        let mut slot = None;
        let mut out = None;
        for p in parts {
            out = assemble(&mut slot, p)?;
        }
        Ok(out)
    }

    fn run(parts: Vec<ChunkListPart>) -> Result<Option<FileDigest>, String> {
        Ok(run_with_stat(parts)?.map(|(d, _)| d))
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
    fn split_cuts_contiguous_parts_with_the_last_one_flagged() {
        let d = digest(10);
        let parts = split(42, &d, &st(), 4, true);
        assert_eq!(parts.len(), 3);
        let mut first = 0u32;
        for (k, p) in parts.iter().enumerate() {
            assert_eq!(p.file_id, 42);
            assert_eq!(p.size, d.size);
            assert_eq!(p.whole_hash.as_ref().unwrap().bytes, d.hash.to_vec());
            assert_eq!(p.total, 10);
            assert_eq!(p.first, first);
            assert!(p.chunks.len() <= 4);
            assert_eq!(p.last, k == 2);
            first += p.chunks.len() as u32;
        }
        assert_eq!(first, 10);
        let records: Vec<ChunkRecord> = parts
            .iter()
            .flat_map(|p| {
                p.chunks
                    .iter()
                    .map(|c| ChunkRecord::try_from(c.clone()).unwrap())
            })
            .collect();
        assert_eq!(records, d.chunks);
    }

    #[test]
    fn split_of_an_empty_digest_is_one_empty_last_part() {
        let parts = split(1, &digest(0), &st(), 4, true);
        assert_eq!(parts.len(), 1);
        assert!(parts[0].last);
        assert!(parts[0].chunks.is_empty());
        assert_eq!(parts[0].total, 0);
    }

    #[test]
    fn assemble_rebuilds_the_list_from_its_parts() {
        let d = digest(10);
        let cl = run(split(42, &d, &st(), 4, true))
            .unwrap()
            .expect("complete");
        assert_eq!(cl.size, d.size);
        assert_eq!(cl.hash, d.hash);
        assert_eq!(cl.chunks, d.chunks);
    }

    #[test]
    fn assemble_waits_for_the_last_part() {
        let mut parts = split(42, &digest(10), &st(), 4, true);
        parts.pop();
        assert_eq!(run(parts).unwrap(), None);
    }

    #[test]
    fn a_first_part_restarts_the_assembly() {
        let d = digest(10);
        let mut parts = split(42, &d, &st(), 4, true);
        let restart = parts[0].clone();
        parts.insert(1, restart);
        let cl = run(parts).unwrap().expect("complete");
        assert_eq!(cl.chunks.len(), 10);
    }

    #[test]
    fn a_part_that_skips_ahead_is_an_error() {
        let mut parts = split(42, &digest(10), &st(), 4, true);
        parts.remove(1);
        assert!(run(parts).unwrap_err().contains("continues at"));
    }

    #[test]
    fn a_continuation_without_a_start_is_an_error() {
        let parts = split(42, &digest(10), &st(), 4, true);
        assert!(run(parts[1..].to_vec())
            .unwrap_err()
            .contains("continues at"));
    }

    #[test]
    fn a_part_with_another_hash_is_an_error() {
        let mut parts = split(42, &digest(10), &st(), 4, true);
        parts[1].whole_hash = Some(Hash { bytes: vec![1; 32] });
        assert!(run(parts).unwrap_err().contains("hash"));
    }

    #[test]
    fn a_last_part_short_of_the_total_is_an_error() {
        let mut parts = split(42, &digest(10), &st(), 4, true);
        parts.truncate(2);
        parts[1].last = true;
        assert!(run(parts).unwrap_err().contains("short"));
    }

    #[test]
    fn more_records_than_announced_is_an_error() {
        let mut parts = split(42, &digest(10), &st(), 4, true);
        for p in &mut parts {
            p.total = 9;
        }
        assert!(run(parts).unwrap_err().contains("exceeds"));
    }

    #[test]
    fn a_total_above_the_cap_is_refused_before_anything_is_held() {
        let mut parts = split(42, &digest(2), &st(), 4, true);
        parts[0].total = (MAX_LIST_CHUNKS + 1) as u32;
        parts[0].size = u64::MAX;
        assert!(run(parts).unwrap_err().contains("cap"));
    }

    #[test]
    fn a_total_impossible_for_the_size_is_refused() {
        let mut parts = split(42, &digest(2), &st(), 4, true);
        // 2 chunks of at least 64 KiB cannot describe a 10-byte file.
        parts[0].size = 10;
        assert!(run(parts).unwrap_err().contains("size"));
    }

    #[test]
    fn an_empty_unfinished_first_part_withdraws_the_list() {
        let d = digest(10);
        let mut parts = split(42, &d, &st(), 4, true);
        parts.insert(
            1,
            ChunkListPart {
                file_id: 42,
                ..ChunkListPart::default()
            },
        );
        // Withdrawn mid-list: what follows without a new start is refused.
        assert!(run(parts.clone()).unwrap_err().contains("continues at"));
        // A new list after the withdrawal assembles.
        let mut slot = None;
        assert_eq!(assemble(&mut slot, parts[1].clone()), Ok(None));
        let mut got = None;
        for p in split(42, &d, &st(), 4, true) {
            got = assemble(&mut slot, p).unwrap();
        }
        assert_eq!(got.expect("complete").0.chunks, d.chunks);
        assert!(slot.is_none());
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
    fn a_new_first_part_releases_the_old_list() {
        let mut b = ListBudget::new(10);
        assert_eq!(b.reserve(1, 10), Ok(0));
        assert_eq!(b.reserve(1, 4), Ok(10));
        assert_eq!(b.total(), 4);
        assert_eq!(b.release(1), 4);
        assert_eq!(b.total(), 0);
    }

    /// Parts of a list of `n` records, one record per part.
    fn list(file_id: u64, n: usize) -> Vec<ChunkListPart> {
        split(file_id, &digest(n), &st(), 1, true)
    }

    fn ids(parts: &[ChunkListPart]) -> Vec<(u64, u32)> {
        parts.iter().map(|p| (p.file_id, p.first)).collect()
    }

    #[test]
    fn credit_sends_whole_files_in_order() {
        let mut c = ListCredit::new(10);
        c.push(1, list(1, 8), None);
        c.push(2, list(2, 5), None);
        let out = c.ready();
        assert_eq!(out.len(), 8);
        assert!(out.iter().all(|p| p.file_id == 1));
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
        let mut parts = split(42, &digest(2), &st(), 4, true);
        parts[0].whole_hash = Some(Hash { bytes: vec![1; 31] });
        assert!(run(parts).unwrap_err().contains("hash"));
        // Absent on every part (an APPEND tail): assembled with zeros.
        let parts = split(42, &digest(2), &st(), 4, false);
        assert_eq!(run(parts).unwrap().unwrap().hash, [0; 32]);
        // Absent on one part only: the parts disagree about the file.
        let mut parts = split(42, &digest(2), &st(), 1, true);
        parts[1].whole_hash = None;
        assert!(run(parts).unwrap_err().contains("another file"));
    }
}
