//! What a session tells the user as it runs: events on a bus, and the
//! progress a side sends its peer in heartbeats.

use super::reconnect;
use crate::fs::paths::RelPath;

/// Which side of the session an event describes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Party {
    Local,
    Peer,
}

/// One finished (or failed) session, for the `-vv` summary.
#[derive(Debug, Clone, PartialEq)]
pub struct SessionSummary {
    /// 0-based position among the run's sources, and their count.
    pub index: usize,
    pub of: usize,
    /// `"push"` or `"pull"`.
    pub verb: &'static str,
    /// The operands as typed.
    pub from: String,
    pub to: String,
    pub reconnects: u32,
    /// How the last connection was made; `None` if it never was.
    pub conn: Option<reconnect::ConnState>,
    /// The key exchange group of the last connection (`X25519MLKEM768`).
    pub key_exchange: Option<String>,
    pub usage: crate::telemetry::meter::Usage,
    pub files_transferred: u64,
    pub files_hashed: u64,
    /// The session's QUIC bytes, as in the `sent`/`received` trailer.
    pub bytes_sent: u64,
    pub bytes_received: u64,
    /// The session ended with an error (files that failed in a session
    /// that ran to its end do not count).
    pub failed: bool,
}

/// Something worth telling the user about, emitted as a session runs.
/// Events are best-effort: a slow reporter loses some (`Lagged`), and the
/// final [`Outcome`] is authoritative for totals.
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    /// The sender opened a data stream for this file; its size.
    FileStart(RelPath, u64),
    /// The receiver opened this file for writing and asked for its data;
    /// its size. No data has come yet, and with many files ahead of it
    /// none may come for a while: `FileBytes` says when it does.
    FileOpened(RelPath, u64),
    /// A hasher started reading this file; its size.
    FileHashing(RelPath, u64),
    /// The hasher finished reading this file.
    FileHashed(RelPath),
    /// Literal bytes moved on a data stream for this file.
    FileBytes(RelPath, u64),
    /// Bytes of this file that need no transfer (matched basis chunks,
    /// local copies). Absolute: the latest value replaces the one before
    /// (the sender re-reports it as more of the file is requested).
    FileMatched(RelPath, u64),
    /// The sender finished all data streams for this file; the receiver's
    /// ack is still outstanding.
    FileSent(RelPath),
    /// A file was placed (receiver) or acknowledged (sender); its size.
    FileDone(RelPath, u64),
    /// A dry run (`-n`) found this file would be transferred; listed for
    /// `-v`, counted nowhere.
    WouldTransfer(RelPath),
    /// The planner found this regular file up to date; its size.
    Skipped(RelPath, u64),
    /// Bytes of source files the peer's planner skipped so far (cumulative).
    PeerSkipped(u64),
    /// This connection resumes a stored plan: bytes of the source that need
    /// nothing more (found up to date, or placed by an earlier connection).
    /// Every other file is requested again.
    Settled(u64),
    /// This connection resumes a session and lists and plans again: bytes
    /// of the files earlier connections placed. The planner does not report
    /// them as skipped; it reports the files found up to date anew, and
    /// every other file is requested again.
    PlacedBefore(u64),
    /// File data bytes moved on a data stream (aggregate, as before).
    Bytes(u64),
    /// Listing progress for one side.
    Scanned {
        party: Party,
        entries: u64,
        bytes: u64,
        at: Option<RelPath>,
    },
    /// One side's listing is complete: regular files and their total size.
    Total {
        party: Party,
        files: u64,
        bytes: u64,
    },
    /// Deletion pass progress for one side.
    Delete {
        party: Party,
        done: u64,
        total: Option<u64>,
    },
    /// A new session (one source) starts: its 0-based index, the count,
    /// and the source and destination operands as typed.
    SessionStart {
        index: usize,
        of: usize,
        from: String,
        to: String,
    },
    /// A session (one source) ended, well or not: where its time went.
    SessionEnd(std::sync::Arc<SessionSummary>),
    Conn(reconnect::ConnState),
    Deleted(RelPath),
    /// `--dry-run`: this path would be deleted.
    WouldDelete(RelPath),
    /// rsync's `-i` code for an item (see [`crate::plan::itemize`]).
    Itemized(RelPath, String),
    /// A hard link `path` to `target` was made.
    Hardlinked(RelPath, RelPath),
    /// This file failed; the message is the one the [`Outcome`] records.
    Failed(RelPath, String),
}

/// The channel events travel on. Sending never blocks; with nobody
/// listening it is a cheap no-op.
#[derive(Clone)]
pub struct EventBus(tokio::sync::broadcast::Sender<Event>);

impl EventBus {
    pub fn new(capacity: usize) -> EventBus {
        EventBus(tokio::sync::broadcast::channel(capacity).0)
    }

    pub fn subscribe(&self) -> tokio::sync::broadcast::Receiver<Event> {
        self.0.subscribe()
    }

    pub fn emit(&self, ev: Event) {
        if self.0.receiver_count() > 0 {
            let _ = self.0.send(ev);
        }
    }

    /// A connection that resumes a stored plan lists nothing. Reports what
    /// `source`'s listing held on the first connection as that side's
    /// finished walk, and the other side's walk as finished too, so the
    /// progress display has its total from the start, and `settled`, how
    /// many of those bytes need nothing more (see [`Event::Settled`]).
    pub(crate) fn emit_listed(&self, source: Party, listed: &crate::scan::ListStats, settled: u64) {
        let other = match source {
            Party::Local => Party::Peer,
            Party::Peer => Party::Local,
        };
        self.emit(Event::Scanned {
            party: source,
            entries: listed.files(),
            bytes: listed.total_size,
            at: None,
        });
        self.emit(Event::Total {
            party: source,
            files: listed.reg,
            bytes: listed.total_size,
        });
        self.emit(Event::Total {
            party: other,
            files: 0,
            bytes: 0,
        });
        self.emit(Event::Settled(settled));
    }
}

impl Default for EventBus {
    fn default() -> Self {
        EventBus::new(4096)
    }
}

/// This side's progress the peer cannot see, carried on `Heartbeat`.
#[derive(Default)]
pub struct Busy {
    /// The directory this side's listing walk is in, while it walks.
    pub walking: std::sync::Mutex<Option<RelPath>>,
    /// Bytes of source files this side's planner found up to date.
    pub skipped_bytes: std::sync::atomic::AtomicU64,
    /// Paths this side's deletion pass removed so far.
    pub deleted: std::sync::atomic::AtomicU64,
    /// The deletion pass's candidate count once known, else 0.
    pub delete_total: std::sync::atomic::AtomicU64,
    /// File data this side's receiver took in on this connection.
    pub received: std::sync::atomic::AtomicU64,
}

impl Busy {
    /// The frame that tells the peer where this side is.
    pub fn heartbeat(&self) -> irsync_proto::v1::Heartbeat {
        use std::sync::atomic::Ordering::Relaxed;
        let walking = self
            .walking
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .map(|p| p.as_bytes().to_vec());
        irsync_proto::v1::Heartbeat {
            walking,
            skipped_bytes: self.skipped_bytes.load(Relaxed),
            deleted_count: self.deleted.load(Relaxed),
            delete_total: self.delete_total.load(Relaxed),
            received_bytes: self.received.load(Relaxed),
            listed: None,
            listed_settled: 0,
            placed_before: None,
        }
    }
}
