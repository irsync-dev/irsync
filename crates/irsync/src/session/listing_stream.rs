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
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tracing::Instrument;

use super::control::{msg_name, side_of, Ev, Listed, DRAIN_TIMEOUT};
use super::limits::LISTING_QUEUE;
use super::{EventBus, StreamKind, Tamper};
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
}

impl ListingOut {
    /// Queues a frame from the walk's blocking thread, waiting while the
    /// queue is full (the link, or the peer's planner, is slower than the
    /// walk). A closed writer surfaces as `Ev::WriterFailed`.
    pub fn send_blocking(&self, msg: Msg) {
        let _ = self.tx.blocking_send(Control { msg: Some(msg) });
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
pub(super) fn spawn_listing_writer(
    mut send: SendStream,
    events: mpsc::UnboundedSender<Ev>,
    tamper: Tamper,
) -> (ListingOut, JoinHandle<()>) {
    let (tx, mut rx) = mpsc::channel::<Control>(LISTING_QUEUE);
    let task = tokio::spawn(
        async move {
            while let Some(c) = rx.recv().await {
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
            if send.finish().is_ok() {
                let _ = tokio::time::timeout(DRAIN_TIMEOUT, send.stopped()).await;
            }
        }
        .instrument(tracing::Span::current()),
    );
    (ListingOut { tx }, task)
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
/// posts `Ev::PeerListing`, the `ListingEnd` `Ev::Ctrl`. A listing that ends
/// any other way keeps `sink` open until the task is aborted.
pub(super) fn spawn_listing_reader(
    recv: RecvStream,
    sink: ListingSink,
    ids: Option<IdMap>,
    events: mpsc::UnboundedSender<Ev>,
    bus: EventBus,
    activity: Arc<AtomicU64>,
    scan: Arc<PeerScan>,
) -> JoinHandle<()> {
    tokio::spawn(
        read_listing(recv, sink, ids, events, bus, activity, scan)
            .instrument(tracing::Span::current()),
    )
}

/// The reader's loop; see [`spawn_listing_reader`].
async fn read_listing(
    mut recv: RecvStream,
    sink: ListingSink,
    mut ids: Option<IdMap>,
    events: mpsc::UnboundedSender<Ev>,
    bus: EventBus,
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
            // Nothing at all: the peer did not list (a receiver resuming
            // its stored plan).
            Ok(None) if !started || ended => break None,
            Ok(None) => {
                break Some(Ev::PeerFailed(
                    "listing stream: the peer's listing is incomplete (the stream ended before ListingEnd)"
                        .into(),
                ))
            }
            Err(_) if ended => break None,
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
            let _ = events.send(Ev::PeerListing);
        }
        match msg {
            Msg::Listing(ListingEntry { entry: Some(mut e) }) => {
                // The peer's owner and group names become this side's ids.
                if let Some(map) = ids.as_mut() {
                    map.localize(&mut e);
                }
                let seen = scan.count(&e);
                if seen.files().is_multiple_of(1000) {
                    bus.emit(Event::Scanned {
                        party: Party::Peer,
                        entries: seen.files(),
                        bytes: seen.total_size,
                        at: None,
                    });
                }
                if let Some(tx) = &planner {
                    // A planner gone means the session is ending.
                    let _ = tx.send(Listed::Side(side_of(e))).await;
                }
            }
            Msg::Listing(ListingEntry { entry: None }) => {
                tracing::warn!("listing frame without entry; ignored");
            }
            // The receiver's planner wants the gap in order with the entries.
            Msg::ListingError(e) => {
                tracing::info!(path = ?String::from_utf8_lossy(&e.path), error = %e.message, "a gap in the peer's listing");
                if let Some(tx) = &planner {
                    let path = RelPath::new(&e.path).unwrap_or_default();
                    let _ = tx.send(Listed::Gap(path)).await;
                }
            }
            m @ Msg::ListingEnd(_) => {
                let seen = scan.seen();
                bus.emit(Event::Total {
                    party: Party::Peer,
                    files: seen.reg,
                    bytes: seen.total_size,
                });
                planner = None;
                ended = true;
                scan.ended.store(true, Ordering::Relaxed);
                let _ = events.send(Ev::Ctrl(m));
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
        let _ = events.send(ev);
    }
    if planner.is_some() {
        // No `ListingEnd`: the rest of the peer's listing would look absent
        // to the planner (and its destination copies extraneous). Keep the
        // planner waiting until the session ends and aborts this task.
        std::future::pending::<()>().await;
    }
}
