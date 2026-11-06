//! Bounds on the session's queues.

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
