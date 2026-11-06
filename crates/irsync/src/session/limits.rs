//! Bounds on the session's queues.

use std::sync::Arc;

use irsync_proto::v1::ChunkBasis;

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
