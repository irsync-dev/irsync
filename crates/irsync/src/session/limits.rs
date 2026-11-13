//! Bounds on the session's queues.

use std::sync::Arc;

use irsync_proto::v1::{ChunkBasis, NeedChunks};

use super::parts::MAX_LIST_CHUNKS;

/// Listing items a queue holds: the peer's listing on its way to the
/// planner, the receiver's own listing on its way to the planner, and a
/// listing on its way to the listing stream's writer. A full queue stalls
/// whoever fills it (the listing reader, and through QUIC flow control the
/// peer's walk; or this side's walk), never the control stream.
pub(super) const LISTING_QUEUE: usize = 1024;

/// Peer events a queue holds: control frames, and the listing reader's
/// start, end and failure signals, on their way to the role's main loop. A
/// full queue stops both stream readers, so through QUIC flow control a
/// peer that floods them waits for this side's main loop.
pub(super) const PEER_EVENTS: usize = 1024;

/// Bytes of paced control frames (see `Out::send_paced`) queued for the
/// control stream's writer and not yet written. A task with a per-path or
/// per-chunk frame to send waits while the queue holds this much, so a slow
/// link stalls the task instead of growing the queue.
pub(super) const OUT_BUDGET: usize = 4 * 1024 * 1024;

/// The most bytes one chunk record takes in a `ChunkListPart`: its offset
/// (a varint of at most 10 bytes) and length (at most 5), each with a tag
/// byte, its 32-byte hash in a nested `Hash` (with tag and length, 36
/// bytes), and the record's own tag and length (2). A typical record is a
/// few bytes shorter (offsets below 2^42 take 6 bytes).
const RECORD_WIRE: usize = 11 + 6 + 36 + 2;

/// The most bytes one chunk index takes in a `NeedChunks`: a packed varint
/// below [`MAX_LIST_CHUNKS`] (2^24), so at most 4 bytes.
const INDEX_WIRE: usize = 4;

/// The most chunk indices one `NeedChunks` carries. A request for more
/// (a file of up to [`MAX_LIST_CHUNKS`] chunks) goes out in pieces of this
/// many, each at most 1 MiB ([`INDEX_WIRE`] bytes an index), so every piece
/// fits a frame (`MAX_FRAME`, 16 MiB) and, paced, the [`OUT_BUDGET`].
pub(super) const NEED_PIECE: usize = 1 << 18;

/// This side's [`NEED_PIECE`], unless a test sets another.
pub(super) fn need_piece(hooks: &super::SideHooks) -> usize {
    #[cfg(feature = "test-hooks")]
    if let Some(n) = hooks.need_piece {
        return n.max(1);
    }
    let _ = hooks;
    NEED_PIECE
}

/// The request for `idxs` (sender indices) of file `file_id` as
/// `NeedChunks` of at most `piece` indices each, in order; an empty request
/// is one empty `NeedChunks`. The sender treats consecutive requests for a
/// file as one, except for bases: each request takes all the bases it holds
/// for the file, so bases must precede the piece that names their chunks.
pub(super) fn need_pieces(file_id: u64, idxs: Vec<u32>, piece: usize) -> Vec<NeedChunks> {
    if idxs.len() <= piece {
        return vec![NeedChunks {
            file_id,
            chunk_idx: idxs,
        }];
    }
    idxs.chunks(piece)
        .map(|c| NeedChunks {
            file_id,
            chunk_idx: c.to_vec(),
        })
        .collect()
}

/// What a main loop queues per file in flight besides chunk records and
/// indices, rounded up: a `PlanItem` and an `Itemized`, each with a path
/// (at most 4 KiB, `PATH_MAX`); `FileAck`s with their messages; the
/// `ChunkListPart` headers of up to three lists (see
/// `receiver::MAX_LIST_STARTS`) and a `ListRoom`; a `MayPlace` and a
/// `PlaceVerdict` per list (a few dozen bytes each). About 9 KiB at most.
const FILE_WIRE: usize = 16 * 1024;

/// The most bytes an honest session queues for its control stream's
/// writer, if the peer reads them as fast as it can but the link is slow.
/// Frames a main loop queues are not paced (it must not wait on the
/// peer); each kind is bounded by the protocol instead:
///
/// - Sender: chunk lists, sent only into the room the receiver granted
///   (`list_budget` records, `RECORD_WIRE` each); paced frames
///   ([`OUT_BUDGET`]: the final re-check's `Replan`s, one per listed file
///   that changed, and its `Settled`; the link check's `Unlinked`s and
///   `LinksChecked`); per file in flight
///   ([`MAX_INFLIGHT`]) a `FileAck` (`SOURCE_ERROR`), the empty first
///   part that withdraws a retried file's list and a `PlaceVerdict` per
///   list (`FILE_WIRE`).
/// - Receiver: paced frames ([`OUT_BUDGET`]: the planner's `Itemized`,
///   `offer_bases`, the link check's `CheckLink`s); `NeedChunks` for at most every chunk of the lists it
///   holds (`list_budget` records), once per list (1 + [`MAX_RETRIES`]) and
///   per re-request ([`MAX_REREQUESTS`]), `INDEX_WIRE` each; per file in
///   flight a `PlanItem`, an `Itemized`, a `FileAck`, a `ListRoom` and a
///   `MayPlace` per list (`FILE_WIRE`).
/// - Both: one frame up to [`MAX_FRAME`] beyond these (a paced frame larger
///   than the budget passes alone; `SessionDone`, `Error`), which also
///   covers heartbeats (two a second at most, a few dozen bytes each).
///
/// With the defaults: a sender 4 MiB + 2^24 × 55 B + 4096 × 16 KiB + 16 MiB
/// ≈ 1.0 GB (964 MiB, nearly all of it chunk lists), a receiver 4 MiB + 5 × 2^24
/// × 4 B + 4096 × 16 KiB + 16 MiB ≈ 424 MB.
///
/// [`MAX_INFLIGHT`]: super::planner::MAX_INFLIGHT
/// [`MAX_RETRIES`]: super::sender_role::MAX_RETRIES
/// [`MAX_REREQUESTS`]: super::receiver::MAX_REREQUESTS
/// [`MAX_FRAME`]: crate::net::frame::MAX_FRAME
fn honest_out(list_budget: u64, sends: bool) -> usize {
    use super::planner::MAX_INFLIGHT;
    use crate::net::frame::MAX_FRAME;
    let records = usize::try_from(list_budget).unwrap_or(usize::MAX);
    let files = MAX_INFLIGHT.saturating_mul(FILE_WIRE);
    let frames = if sends {
        OUT_BUDGET.saturating_add(records.saturating_mul(RECORD_WIRE))
    } else {
        let lists =
            (1 + super::sender_role::MAX_RETRIES + super::receiver::MAX_REREQUESTS) as usize;
        OUT_BUDGET.saturating_add(lists.saturating_mul(records).saturating_mul(INDEX_WIRE))
    };
    frames.saturating_add(files).saturating_add(MAX_FRAME)
}

/// Bytes of control frames queued for the writer and not yet written past
/// which the peer counts as not reading its control stream, and the
/// session fails: twice what an honest session can queue (see
/// [`honest_out`]), about 2.0 GB for a sender and 850 MB for a receiver
/// with the defaults. Below it, a peer that stops reading while freeing
/// slots or room with frames of its own (so this side keeps replying)
/// costs no more than that; it ends at once, without waiting for the idle
/// timeout, which such a peer could hold off.
pub(super) fn out_ceiling(hooks: &super::SideHooks, sends: bool) -> usize {
    #[cfg(feature = "test-hooks")]
    if let Some(n) = hooks.out_ceiling {
        return n;
    }
    honest_out(list_budget(hooks), sends).saturating_mul(2)
}

/// Bytes queued for a writer and not yet written, against a ceiling (see
/// [`out_ceiling`]).
pub(super) struct Backlog {
    bytes: std::sync::atomic::AtomicUsize,
    ceiling: usize,
    over: std::sync::atomic::AtomicBool,
}

impl Backlog {
    pub fn new(ceiling: usize) -> Self {
        Backlog {
            bytes: Default::default(),
            ceiling,
            over: Default::default(),
        }
    }

    /// Counts `n` bytes queued; true the first time the count goes past
    /// the ceiling (only once, however long it stays there).
    pub fn add(&self, n: usize) -> bool {
        use std::sync::atomic::Ordering::SeqCst;
        let now = self.bytes.fetch_add(n, SeqCst).saturating_add(n);
        now > self.ceiling && !self.over.swap(true, SeqCst)
    }

    /// Counts `n` bytes written (or dropped).
    pub fn sub(&self, n: usize) {
        self.bytes.fetch_sub(n, std::sync::atomic::Ordering::SeqCst);
    }

    /// Bytes queued now.
    pub fn bytes(&self) -> usize {
        self.bytes.load(std::sync::atomic::Ordering::SeqCst)
    }

    pub fn ceiling(&self) -> usize {
        self.ceiling
    }
}

/// Bytes of chunk bases (see [`basis_wire_len`]) a session holds: the
/// receiver's bases sent but not yet requested, and the sender's bases
/// received and not yet taken by a request. The receiver waits for room
/// (it requests what it sent, which frees it); a sender holding more fails
/// the session.
pub(super) const BASIS_BUDGET: usize = 32 * 1024 * 1024;

/// Bases the receiver sends before the request that uses them.
pub(super) const BASIS_BATCH: usize = 64;

/// Chunk records of lists a receiver holds at once, across files: the room
/// each connection starts with on both sides (see `parts::ListBudget` and
/// `parts::ListCredit`). At least one maximal list, so any legal list can
/// eventually be sent; a sender fails a file whose list is longer.
pub(super) const LIST_BUDGET: u64 = MAX_LIST_CHUNKS as u64;

/// This side's list budget: [`LIST_BUDGET`], unless a test sets another.
pub(super) fn list_budget(hooks: &super::SideHooks) -> u64 {
    #[cfg(feature = "test-hooks")]
    if let Some(n) = hooks.list_budget {
        return n;
    }
    let _ = hooks;
    LIST_BUDGET
}

/// A basis's size as counted against [`BASIS_BUDGET`]: its block hashes
/// as they travel.
pub(super) fn basis_wire_len(b: &ChunkBasis) -> usize {
    4 * b.weak.len() + b.strong.len()
}

/// This side's basis budget: [`BASIS_BUDGET`], unless a test sets another.
pub(super) fn basis_budget(hooks: &super::SideHooks) -> usize {
    #[cfg(feature = "test-hooks")]
    if let Some(n) = hooks.basis_budget {
        return n;
    }
    let _ = hooks;
    BASIS_BUDGET
}

/// A count of bytes held against a limit. A request waits while others hold
/// bytes and it would take the total past the limit; a request larger than
/// the limit still passes once nothing else is held.
pub(super) struct ByteBudget {
    limit: usize,
    held: std::sync::Mutex<usize>,
    freed: tokio::sync::Notify,
}

impl ByteBudget {
    pub fn new(limit: usize) -> Self {
        ByteBudget {
            limit,
            held: std::sync::Mutex::new(0),
            freed: tokio::sync::Notify::new(),
        }
    }

    /// Takes `n` bytes, waiting until they fit. Cancel-safe: nothing is
    /// taken unless it returns.
    pub async fn acquire(&self, n: usize) {
        loop {
            let freed = self.freed.notified();
            tokio::pin!(freed);
            // Registered before the check, so a release in between wakes us.
            freed.as_mut().enable();
            if self.try_acquire(n) {
                return;
            }
            freed.await;
        }
    }

    /// Takes `n` bytes if they fit now.
    pub fn try_acquire(&self, n: usize) -> bool {
        let mut held = self.held.lock().unwrap_or_else(|e| e.into_inner());
        if *held > 0 && *held + n > self.limit {
            return false;
        }
        *held += n;
        true
    }

    /// Gives back `n` bytes taken earlier.
    pub fn release(&self, n: usize) {
        {
            let mut held = self.held.lock().unwrap_or_else(|e| e.into_inner());
            *held = held.saturating_sub(n);
        }
        self.freed.notify_waiters();
    }

    /// Bytes held now.
    #[cfg_attr(not(any(test, feature = "test-hooks")), allow(dead_code))]
    pub fn held(&self) -> usize {
        *self.held.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// Bytes taken from a [`ByteBudget`] and given back together, at the
/// latest when dropped: a task that ends early, fails or is dropped never
/// strands them.
pub(super) struct Held {
    budget: Arc<ByteBudget>,
    n: usize,
}

impl Held {
    pub fn new(budget: Arc<ByteBudget>) -> Self {
        Held { budget, n: 0 }
    }

    /// Takes `n` more bytes if they fit now.
    pub fn try_take(&mut self, n: usize) -> bool {
        let ok = self.budget.try_acquire(n);
        if ok {
            self.n += n;
        }
        ok
    }

    /// Takes `n` more bytes, waiting until they fit. Never call it while
    /// holding bytes others may wait for and only this caller can free.
    pub async fn take(&mut self, n: usize) {
        self.budget.acquire(n).await;
        self.n += n;
    }

    /// Bytes held now.
    pub fn bytes(&self) -> usize {
        self.n
    }

    /// Gives back everything held.
    pub fn release(&mut self) {
        self.budget.release(std::mem::take(&mut self.n));
    }
}

impl Drop for Held {
    fn drop(&mut self) {
        self.release();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::FutureExt;

    #[tokio::test]
    async fn acquire_waits_until_release() {
        let b = ByteBudget::new(10);
        b.acquire(8).await;
        let mut wait = Box::pin(b.acquire(5));
        assert!((&mut wait).now_or_never().is_none());
        tokio::task::yield_now().await;
        assert!((&mut wait).now_or_never().is_none());
        b.release(8);
        assert!(wait.now_or_never().is_some());
        assert_eq!(b.held(), 5);
    }

    #[tokio::test]
    async fn an_oversized_request_passes_alone() {
        let b = ByteBudget::new(10);
        assert!(b.acquire(50).now_or_never().is_some());
        assert_eq!(b.held(), 50);
        // ...but only alone.
        assert!(b.acquire(1).now_or_never().is_none());
    }

    #[tokio::test]
    async fn held_bytes_return_when_dropped() {
        let b = Arc::new(ByteBudget::new(10));
        let mut h = Held::new(b.clone());
        assert!(h.try_take(4));
        h.take(4).await;
        assert_eq!(b.held(), 8);
        h.release();
        assert_eq!(b.held(), 0);
        assert!(h.try_take(6));
        drop(h);
        assert_eq!(b.held(), 0);
    }

    #[test]
    fn the_backlog_trips_once_past_its_ceiling() {
        let b = Backlog::new(100);
        assert!(!b.add(60));
        assert!(!b.add(40), "at the ceiling is not past it");
        assert!(b.add(1));
        assert!(!b.add(1000), "reported once");
        b.sub(1101);
        assert_eq!(b.bytes(), 0);
        assert!(!b.add(500), "still reported once");
    }

    /// The sizes the ceiling is derived from hold for the largest frames
    /// an honest session sends.
    #[test]
    fn the_wire_sizes_behind_the_ceiling_hold() {
        use irsync_proto::v1::{
            control::Msg, file_ack::Status, ChunkListPart, Control, FileAck, Hash, Itemized,
            ListRoom, NeedChunks, PlanItem,
        };
        use prost::Message as _;
        let len = |m: Msg| Control { msg: Some(m) }.encoded_len();
        let record = irsync_proto::v1::ChunkRecord {
            offset: u64::MAX,
            len: u32::MAX,
            hash: Some(Hash {
                bytes: vec![0xff; 32],
            }),
        };
        let part = |n: usize| ChunkListPart {
            file_id: u64::MAX,
            size: u64::MAX,
            whole_hash: Some(Hash {
                bytes: vec![0xff; 32],
            }),
            total: u32::MAX,
            first: u32::MAX,
            chunks: vec![record.clone(); n],
            last: true,
            mtime_sec: i64::MIN,
            mtime_nsec: u32::MAX,
            mode: u32::MAX,
            uid: u32::MAX,
            gid: u32::MAX,
            user: "u".repeat(256),
            group: "g".repeat(256),
            ctime_sec: i64::MIN,
            ctime_nsec: u32::MAX,
            inode: u64::MAX,
        };
        let header = len(Msg::ChunkListPart(part(0)));
        let full = len(Msg::ChunkListPart(part(super::super::parts::PART_RECORDS)));
        // A few bytes more for the longer lengths of the part and the frame.
        assert!(
            full - header <= super::super::parts::PART_RECORDS * RECORD_WIRE + 8,
            "{}",
            full - header
        );
        let need = |n: usize| {
            len(Msg::Need(NeedChunks {
                file_id: u64::MAX,
                chunk_idx: vec![MAX_LIST_CHUNKS as u32 - 1; n],
            }))
        };
        // A few bytes more for the packed field's length.
        assert!(need(100_000) - need(0) <= 100_000 * INDEX_WIRE + 8);
        let path = vec![b'p'; 4096];
        let message = "m".repeat(1024);
        let per_file = len(Msg::Plan(PlanItem {
            file_id: u64::MAX,
            path: path.clone(),
            action: 2,
            append_from: u64::MAX,
        })) + len(Msg::Itemized(Itemized {
            path,
            code: ">f.st......".into(),
        })) + 2 * len(Msg::Ack(FileAck {
            file_id: u64::MAX,
            status: Status::WriteError as i32,
            message,
            matched: u64::MAX,
        })) + 3 * header
            + len(Msg::ListRoom(ListRoom { records: u64::MAX }));
        assert!(per_file <= FILE_WIRE, "{per_file}");
    }

    /// A request for every chunk of the largest legal list does not fit in
    /// one frame; its pieces do, and together they ask for the same chunks.
    #[test]
    fn the_largest_request_goes_out_in_pieces_that_fit_a_frame() {
        use crate::net::frame::MAX_FRAME;
        use irsync_proto::v1::{control::Msg, Control, NeedChunks};
        use prost::Message as _;
        let len = |n: &NeedChunks| {
            Control {
                msg: Some(Msg::Need(n.clone())),
            }
            .encoded_len()
        };
        let all: Vec<u32> = (0..MAX_LIST_CHUNKS as u32).collect();
        let whole = NeedChunks {
            file_id: u64::MAX,
            chunk_idx: all.clone(),
        };
        assert!(len(&whole) > MAX_FRAME, "{}", len(&whole));
        let pieces = need_pieces(u64::MAX, all.clone(), NEED_PIECE);
        assert!(pieces.len() > 1);
        for p in &pieces {
            assert_eq!(p.file_id, u64::MAX);
            assert!(len(p) <= OUT_BUDGET / 2, "{}", len(p));
        }
        let joined: Vec<u32> = pieces.into_iter().flat_map(|p| p.chunk_idx).collect();
        assert_eq!(joined, all);
    }

    /// An empty request still goes out, as one empty `NeedChunks`.
    #[test]
    fn an_empty_request_is_one_empty_piece() {
        let pieces = need_pieces(7, Vec::new(), 3);
        assert_eq!(pieces.len(), 1);
        assert_eq!(pieces[0].file_id, 7);
        assert!(pieces[0].chunk_idx.is_empty());
        let sizes: Vec<usize> = need_pieces(7, (0..7).collect(), 3)
            .iter()
            .map(|p| p.chunk_idx.len())
            .collect();
        assert_eq!(sizes, [3, 3, 1]);
    }

    #[test]
    fn the_ceiling_is_twice_the_honest_most() {
        let hooks = crate::session::SideHooks::default();
        let (sender, receiver) = (out_ceiling(&hooks, true), out_ceiling(&hooks, false));
        assert_eq!(sender, 2 * honest_out(LIST_BUDGET, true));
        assert_eq!(receiver, 2 * honest_out(LIST_BUDGET, false));
        // The figures the documentation gives.
        assert!((1_950_000_000..2_050_000_000).contains(&sender), "{sender}");
        assert!((800_000_000..900_000_000).contains(&receiver), "{receiver}");
    }

    #[test]
    fn try_acquire_never_waits() {
        let b = ByteBudget::new(10);
        assert!(b.try_acquire(8));
        assert!(!b.try_acquire(5));
        assert_eq!(b.held(), 8);
        b.release(8);
        assert!(b.try_acquire(50));
        assert_eq!(b.held(), 50);
    }
}
