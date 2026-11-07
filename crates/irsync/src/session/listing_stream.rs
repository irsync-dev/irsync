//! The listing stream: each side's listing travels on a bidirectional
//! stream of its own, so a planner that stops taking the peer's listing
//! (waiting for files in flight to complete) stalls only that stream, never
//! the control stream whose frames complete those files.
//!
//! The client opens it right after `HelloAck` and sends a
//! `ListingStreamHeader`; then each half carries listing frames
//! (`ListingEntry`, `ListingError`, one `ListingEnd`) and ends.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use anyhow::{bail, Context};
use iroh::endpoint::{Connection, RecvStream, SendStream};
use irsync_proto::v1::{control::Msg, Control, ListingEntry, ListingStreamHeader};
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;
use tracing::Instrument;

use super::control::{msg_name, side_of, Ev, Listed, PeerTx, DRAIN_TIMEOUT};
use super::limits::LISTING_QUEUE;
use super::{SideHooks, StreamKind};
use crate::fs::ids::IdMap;
use crate::fs::paths::RelPath;
use crate::net::frame::{read_msg, write_msg};
use crate::scan::ListStats;

/// Client: opens the listing stream and names the session on it.
pub(super) async fn open(
    conn: &Connection,
    session_id: &[u8; 16],
) -> std::io::Result<(SendStream, RecvStream)> {
    let (mut send, recv) = conn.open_bi().await.map_err(std::io::Error::other)?;
    let header = ListingStreamHeader {
        session_id: session_id.to_vec(),
    };
    write_msg(&mut send, &header).await?;
    Ok((send, recv))
}

/// Daemon: accepts the client's listing stream; it must name `session_id`.
pub(super) async fn accept(
    conn: &Connection,
    session_id: &[u8; 16],
) -> anyhow::Result<(SendStream, RecvStream)> {
    let (send, mut recv) = conn.accept_bi().await.context("accept listing stream")?;
    let header: ListingStreamHeader = read_msg(&mut recv)
        .await
        .context("read ListingStreamHeader")?
        .context("the client ended the listing stream before its header")?;
    if header.session_id != session_id {
        bail!("protocol error: the listing stream names another session");
    }
    Ok((send, recv))
}

/// Handle for queueing listing frames to the listing writer.
pub(super) struct ListingOut {
    tx: mpsc::Sender<Control>,
    #[cfg(feature = "test-hooks")]
    waits: Option<Arc<AtomicU64>>,
}

impl ListingOut {
    /// Queues a frame from the walk's blocking thread, waiting while the
    /// queue is full (the link, or the peer's planner, is slower than the
    /// walk). A closed writer surfaces as `Ev::WriterFailed`.
    pub fn send_blocking(&self, msg: Msg) {
        let c = Control { msg: Some(msg) };
        #[cfg(feature = "test-hooks")]
        let c = match self.tx.try_send(c) {
            Ok(()) => return,
            Err(mpsc::error::TrySendError::Full(c)) => {
                if let Some(w) = &self.waits {
                    w.fetch_add(1, Ordering::Relaxed);
                }
                c
            }
            Err(mpsc::error::TrySendError::Closed(_)) => return,
        };
        let _ = self.tx.blocking_send(c);
    }

    /// Queues the first frame from async code: the queue is empty, so this
    /// never waits.
    pub fn send_first(&self, msg: Msg) {
        let _ = self.tx.try_send(Control { msg: Some(msg) });
    }

    /// Ends this side's half of the stream after everything queued so far
    /// (dropping the handle does the same).
    pub fn finish(self) {}
}

/// Spawns the listing writer: frames go out in queue order; once every
/// [`ListingOut`] is gone it ends the stream and waits (bounded) until the
/// peer has the bytes.
///
/// A half whose last frame queued is not a `ListingEnd` (nothing queued
/// at all, or cut short) waits for the returned gate to be dropped
/// first, which the caller does once the role has ended (on failure, after
/// its `Error` had its chance to arrive). A role that fails before or while
/// it lists drops its handle, and that half would otherwise reach the peer
/// ahead of the `Error`: a receiver listing afresh takes an empty half as
/// a protocol error of ours, and a half cut short as a transport error
/// (retried), either of which would hide the real error. An honest half
/// without `ListingEnd` (no listing, as a resumed session decides) is never
/// waited for, so nothing waits on the gate.
pub(super) fn spawn_listing_writer(
    mut send: SendStream,
    events: mpsc::UnboundedSender<Ev>,
    hooks: &SideHooks,
) -> (ListingOut, oneshot::Sender<()>, JoinHandle<()>) {
    let tamper = hooks.tamper();
    let (tx, mut rx) = mpsc::channel::<Control>(LISTING_QUEUE);
    let (gate, gated) = oneshot::channel::<()>();
    let task = tokio::spawn(
        async move {
            // The last frame the role queued was `ListingEnd`: the half is
            // complete. (Judged before the test hook that tampers with what
            // goes out, so a test can still cut a running role's half short.)
            let mut ended = false;
            while let Some(c) = rx.recv().await {
                ended = matches!(c.msg, Some(Msg::ListingEnd(_)));
                for msg in c
                    .msg
                    .into_iter()
                    .flat_map(|m| tamper.frames(StreamKind::Listing, m))
                {
                    let c = Control { msg: Some(msg) };
                    if let Err(e) = write_msg(&mut send, &c).await {
                        let _ = events.send(Ev::WriterFailed(format!("listing stream: {e}")));
                        return;
                    }
                }
            }
            if !ended {
                let _ = gated.await;
            }
            if send.finish().is_ok() {
                let _ = tokio::time::timeout(DRAIN_TIMEOUT, send.stopped()).await;
            }
        }
        .instrument(tracing::Span::current()),
    );
    (
        ListingOut {
            tx,
            #[cfg(feature = "test-hooks")]
            waits: hooks.listing_waits.clone(),
        },
        gate,
        task,
    )
}

/// Where the peer's listing goes.
pub(super) enum ListingSink {
    /// The receiver's planner, closed at the peer's `ListingEnd` (and only
    /// then: a listing cut short must never look complete to it).
    Planner(mpsc::Sender<Listed>),
    /// The sender: the receiver's listing is only a start signal and scan
    /// progress; it is read in full and dropped, so it never stalls the
    /// receiver's walk.
    Drain,
}

/// What the listing reader has seen of the peer's listing, for the control
/// reader, which reports the peer's walk from its heartbeats.
#[derive(Default)]
pub(super) struct PeerScan {
    /// Set at the peer's `ListingEnd`: a later heartbeat's walk position is stale.
    ended: AtomicBool,
    seen: Mutex<ListStats>,
}

impl PeerScan {
    pub fn ended(&self) -> bool {
        self.ended.load(Ordering::Relaxed)
    }

    pub fn seen(&self) -> ListStats {
        *self.seen.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Counts one entry; returns the counts so far.
    fn count(&self, e: &irsync_proto::v1::FileEntry) -> ListStats {
        let mut seen = self.seen.lock().unwrap_or_else(|e| e.into_inner());
        seen.count(e);
        *seen
    }
}

/// Spawns the listing reader: entries and gaps go to `sink`; the first frame
/// posts `Ev::PeerListing`, the `ListingEnd` `Ev::Ctrl` (a full queue of
/// either kind stops the reading). A listing that ends any other way keeps
/// `sink` open until the task is aborted.
pub(super) fn spawn_listing_reader(
    recv: RecvStream,
    sink: ListingSink,
    ids: Option<IdMap>,
    events: PeerTx,
    hooks: &SideHooks,
    activity: Arc<AtomicU64>,
    scan: Arc<PeerScan>,
) -> JoinHandle<()> {
    let hooks = hooks.clone();
    tokio::spawn(
        read_listing(recv, sink, ids, events, hooks, activity, scan)
            .instrument(tracing::Span::current()),
    )
}

/// A listing path at or before the last one.
///
/// Where a walk puts its gaps (`scan::run_walk`, `ScanEvent::Gap`):
/// - a directory that could not be read: right after its own entry, under
///   the same path;
/// - an entry that could not be stat'ed: in its place, so after the last
///   path; a directory's may be followed by a second gap under the same
///   path (its children could not be read either);
/// - a child the walk could not read (a walk error naming it): in its
///   place among its siblings (`scan::sort_children`);
/// - a walk error with no path inside the root: at the root (empty path),
///   anywhere.
///
/// So an entry must come after the last path listed or named by a gap, and
/// a gap at or after it, or at the root. A gap at the root says entries may
/// be missing anywhere, which only ever withholds deletions.
fn out_of_order(path: &RelPath, last: &RelPath) -> Ev {
    Ev::ProtocolError(format!(
        "listing out of order: {:?} after {:?}",
        String::from_utf8_lossy(path.as_bytes()),
        String::from_utf8_lossy(last.as_bytes())
    ))
}

/// The reader's loop; see [`spawn_listing_reader`].
async fn read_listing<R: tokio::io::AsyncRead + Unpin>(
    mut recv: R,
    sink: ListingSink,
    mut ids: Option<IdMap>,
    events: PeerTx,
    hooks: SideHooks,
    activity: Arc<AtomicU64>,
    scan: Arc<PeerScan>,
) {
    use crate::session::{Event, Party};
    let mut planner = match sink {
        ListingSink::Planner(tx) => Some(tx),
        ListingSink::Drain => None,
    };
    let mut started = false;
    let mut ended = false;
    // The order check: the last path listed, or named by a gap.
    let mut last: Option<RelPath> = None;
    // `ListingEntry` frames received, against `ListingEnd.count`.
    let mut entries = 0u64;
    // Why the listing stopped short, if it did.
    let failure = loop {
        let frame = read_msg::<Control, _>(&mut recv).await;
        activity.fetch_add(1, Ordering::Relaxed);
        let msg = match frame {
            Ok(Some(Control { msg: Some(msg) })) => msg,
            Ok(Some(Control { msg: None })) => {
                tracing::warn!("empty listing frame; ignored");
                continue;
            }
            Ok(None) if ended => break None,
            // Nothing at all, to a planner still waiting for it: a sender
            // always lists (its root at least, and `ListingEnd`) once the
            // receiver plans afresh, and the planner would wait forever.
            Ok(None) if !started && planner.as_ref().is_some_and(|tx| !tx.is_closed()) => {
                break Some(Ev::ProtocolError(
                    "the peer's listing ended without a frame".into(),
                ))
            }
            // Nothing at all: the peer did not list (a receiver resuming
            // its stored plan; or the sender, when the receiver resumes,
            // which has let go of its planner's queue before asking for
            // anything, see `receiver::run`).
            Ok(None) if !started => break None,
            Ok(None) => {
                break Some(Ev::PeerFailed(
                    "listing stream: the peer's listing is incomplete (the stream ended before ListingEnd)"
                        .into(),
                ))
            }
            // Not a frame of ours (undecodable, or longer than any frame):
            // the peer broke the protocol, which no reconnect mends.
            Err(e) if e.kind() == std::io::ErrorKind::InvalidData => {
                break Some(Ev::ProtocolError(format!("listing stream: {e}")))
            }
            Err(_) if ended => break None,
            // The connection is gone: the control reader reports that, after
            // any `Error` the peer sent first on its stream.
            Err(e) if e.kind() == std::io::ErrorKind::NotConnected => break None,
            Err(e) => break Some(Ev::PeerFailed(format!("listing stream: {e}"))),
        };
        if ended {
            break Some(Ev::ProtocolError(format!(
                "{} after ListingEnd on the listing stream",
                msg_name(&msg)
            )));
        }
        if !started {
            started = true;
            events.send(Ev::PeerListing).await;
        }
        match msg {
            Msg::Listing(ListingEntry { entry: Some(mut e) }) => {
                entries += 1;
                // Strictly increasing: the planner's merge would otherwise
                // drop entries, and a source path that looks absent makes
                // its destination copy extraneous. (A path that is not a
                // `RelPath` is refused per entry by the planner.)
                if let Ok(p) = RelPath::new(&e.path) {
                    if let Some(l) = last.as_ref().filter(|l| p <= **l) {
                        break Some(out_of_order(&p, l));
                    }
                    last = Some(p);
                }
                // The peer's owner and group names become this side's ids.
                if let Some(map) = ids.as_mut() {
                    map.localize(&mut e);
                }
                let seen = scan.count(&e);
                if seen.files().is_multiple_of(1000) {
                    hooks.bus.emit(Event::Scanned {
                        party: Party::Peer,
                        entries: seen.files(),
                        bytes: seen.total_size,
                        at: None,
                    });
                }
                if let Some(tx) = &planner {
                    // A planner gone means the session is ending.
                    let _ = tx.send(Listed::Side(side_of(e))).await;
                    #[cfg(feature = "test-hooks")]
                    if let Some(p) = &hooks.listing_peak {
                        p.fetch_max(tx.max_capacity() - tx.capacity(), Ordering::SeqCst);
                    }
                }
            }
            Msg::Listing(ListingEntry { entry: None }) => {
                entries += 1;
                tracing::warn!("listing frame without entry; ignored");
            }
            // The receiver's planner wants the gap in order with the entries.
            Msg::ListingError(e) => {
                tracing::info!(path = ?String::from_utf8_lossy(&e.path), error = %e.message, "a gap in the peer's listing");
                let path = RelPath::new(&e.path).unwrap_or_default();
                // At or after the last path, or at the root (see `out_of_order`).
                if !path.as_bytes().is_empty() {
                    if let Some(l) = last.as_ref().filter(|l| path < **l) {
                        break Some(out_of_order(&path, l));
                    }
                    last = Some(path.clone());
                }
                if let Some(tx) = &planner {
                    let _ = tx.send(Listed::Gap(path)).await;
                }
            }
            Msg::ListingEnd(e) if e.count != entries => {
                break Some(Ev::ProtocolError(format!(
                    "listing count mismatch: ListingEnd says {}, {entries} received",
                    e.count
                )));
            }
            m @ Msg::ListingEnd(_) => {
                let seen = scan.seen();
                hooks.bus.emit(Event::Total {
                    party: Party::Peer,
                    files: seen.reg,
                    bytes: seen.total_size,
                });
                planner = None;
                ended = true;
                scan.ended.store(true, Ordering::Relaxed);
                events.send(Ev::Ctrl(m)).await;
            }
            m => {
                break Some(Ev::ProtocolError(format!(
                    "{} on the listing stream",
                    msg_name(&m)
                )))
            }
        }
    };
    if let Some(ev) = failure {
        events.send(ev).await;
        // Keep the stream (and the planner) until the session ends and
        // aborts this task: dropping it would stop the peer's writer, and
        // that transport error could reach the peer before our `Error`.
        std::future::pending::<()>().await;
    }
    if planner.is_some() {
        // No `ListingEnd`: the rest of the peer's listing would look absent
        // to the planner (and its destination copies extraneous). Keep the
        // planner waiting until the session ends and aborts this task.
        std::future::pending::<()>().await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Bytes that are not a frame of ours, on the listing stream, break the
    /// protocol; they are not a failed stream, which a client would retry.
    #[tokio::test]
    async fn an_undecodable_listing_frame_is_a_protocol_error() {
        // Field 1 with wire type 7, which does not exist; then a length
        // longer than any frame.
        for bytes in [&[0u8, 0, 0, 1, 0x0f][..], &[0xff, 0xff, 0xff, 0xff][..]] {
            let (tx, mut rx) = PeerTx::channel(&SideHooks::default());
            let reader = tokio::spawn(read_listing(
                bytes,
                ListingSink::Drain,
                None,
                tx,
                SideHooks::default(),
                Arc::default(),
                Arc::default(),
            ));
            match rx.recv().await.expect("an event") {
                Ev::ProtocolError(e) => assert!(e.contains("listing stream"), "{e}"),
                Ev::PeerFailed(e) => panic!("a transport error: {e}"),
                _ => panic!("another event"),
            }
            reader.abort();
        }
    }
}
