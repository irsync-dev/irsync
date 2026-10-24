//! Pure state behind the live progress block: what each side's listing has
//! found, which files are in flight, and the rsync-style transfer percent.
//! Nothing here touches the terminal; the renderer reads it.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use crate::cli::DeleteMode;
use crate::fs::paths::RelPath;
use crate::session::reconnect::ConnState;
use crate::session::{Event, Party};

/// A file that has moved no literal bytes for this long counts as stalled.
pub const STALL_AFTER: Duration = Duration::from_secs(2);
/// A row with no event for this long is dropped.
pub const ROW_EXPIRY: Duration = Duration::from_secs(30);
/// Minimum window over which a per-row rate is computed.
const RATE_WINDOW: Duration = Duration::from_secs(1);

/// One side's listing progress.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Walk {
    pub entries: u64,
    pub bytes: u64,
    pub at: Option<RelPath>,
    pub done: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub enum RowState {
    Hashing,
    Moving { literal: u64, matched: u64 },
}

/// A file currently being hashed or transferred.
#[derive(Debug, Clone)]
pub struct Row {
    pub size: u64,
    pub state: RowState,
    pub started: Instant,
    pub last_event: Instant,
    /// Literal bytes since `rate_from`, for the per-file rate.
    pub rate_from: Instant,
    pub rate_bytes: u64,
    /// Bytes per second, last computed.
    pub rate: f64,
}

impl Row {
    fn new(size: u64, state: RowState, now: Instant) -> Row {
        Row {
            size,
            state,
            started: now,
            last_event: now,
            rate_from: now,
            rate_bytes: 0,
            rate: 0.0,
        }
    }

    /// Position in the file, capped at `size`.
    pub fn position(&self) -> u64 {
        match self.state {
            RowState::Hashing => 0,
            RowState::Moving { literal, matched } => literal.saturating_add(matched).min(self.size),
        }
    }

    /// How long a moving row has been without bytes, once that reaches
    /// [`STALL_AFTER`].
    pub fn stalled_for(&self, now: Instant) -> Option<Duration> {
        if !matches!(self.state, RowState::Moving { .. }) {
            return None;
        }
        let idle = now.saturating_duration_since(self.last_event);
        (idle >= STALL_AFTER).then_some(idle)
    }
}

#[derive(Debug, Clone)]
pub struct DeletePhase {
    pub mode: DeleteMode,
    pub done: u64,
    pub total: Option<u64>,
    pub started: bool,
    pub finished: bool,
}

impl DeletePhase {
    fn new(mode: DeleteMode) -> DeletePhase {
        DeletePhase {
            mode,
            done: 0,
            total: None,
            started: false,
            finished: false,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Live {
    pub local: Walk,
    pub peer: Walk,
    /// Which party is the source (its total is the transfer denominator).
    pub source: Party,
    /// Wire bytes, for the rate.
    pub literal: u64,
    /// Sum of `FileDone` sizes.
    pub done_bytes: u64,
    /// Sum of `Skipped` sizes.
    pub skipped_local: u64,
    /// Latest `PeerSkipped`.
    pub skipped_peer: u64,
    pub xfr: u64,
    pub rows: BTreeMap<RelPath, Row>,
    pub conn: ConnState,
    pub delete: Option<DeletePhase>,
    pub transfer_done: bool,
    /// (index, of)
    pub session: (usize, usize),
    pub start: Instant,
}

impl Live {
    pub fn new(source: Party, delete: DeleteMode, start: Instant) -> Live {
        Live {
            local: Walk::default(),
            peer: Walk::default(),
            source,
            literal: 0,
            done_bytes: 0,
            skipped_local: 0,
            skipped_peer: 0,
            xfr: 0,
            rows: BTreeMap::new(),
            conn: ConnState::Connecting,
            delete: (delete != DeleteMode::Off).then(|| DeletePhase::new(delete)),
            transfer_done: false,
            session: (0, 1),
            start,
        }
    }

    fn walk_mut(&mut self, party: Party) -> &mut Walk {
        match party {
            Party::Local => &mut self.local,
            Party::Peer => &mut self.peer,
        }
    }

    /// Applies one event at `now`.
    pub fn apply(&mut self, ev: &Event, now: Instant) {
        match ev {
            Event::FileHashing(p, size) => {
                let row = self
                    .rows
                    .entry(p.clone())
                    .or_insert_with(|| Row::new(*size, RowState::Hashing, now));
                row.size = *size;
                row.state = RowState::Hashing;
                row.last_event = now;
            }
            Event::FileHashed(p) => {
                if self
                    .rows
                    .get(p)
                    .is_some_and(|r| r.state == RowState::Hashing)
                {
                    self.rows.remove(p);
                }
            }
            Event::FileStart(p, size) => {
                let moving = RowState::Moving {
                    literal: 0,
                    matched: 0,
                };
                let row = self
                    .rows
                    .entry(p.clone())
                    .or_insert_with(|| Row::new(*size, moving.clone(), now));
                row.size = *size;
                // A re-request (another stream for the same file) or a
                // `FileMatched` that came first keeps what the row has.
                if row.state == RowState::Hashing {
                    row.state = moving;
                }
                row.last_event = now;
                row.rate_from = now;
                row.rate_bytes = 0;
                row.rate = 0.0;
            }
            Event::FileBytes(p, n) => {
                self.literal = self.literal.saturating_add(*n);
                let row = self.rows.entry(p.clone()).or_insert_with(|| {
                    Row::new(
                        0,
                        RowState::Moving {
                            literal: 0,
                            matched: 0,
                        },
                        now,
                    )
                });
                match &mut row.state {
                    RowState::Moving { literal, .. } => *literal = literal.saturating_add(*n),
                    state @ RowState::Hashing => {
                        *state = RowState::Moving {
                            literal: *n,
                            matched: 0,
                        }
                    }
                }
                row.rate_bytes = row.rate_bytes.saturating_add(*n);
                row.last_event = now;
            }
            Event::FileMatched(p, n) => {
                // The sender knows the match before its first stream opens:
                // the row starts here, sized by the `FileStart` to come.
                let row = self.rows.entry(p.clone()).or_insert_with(|| {
                    Row::new(
                        0,
                        RowState::Moving {
                            literal: 0,
                            matched: 0,
                        },
                        now,
                    )
                });
                match &mut row.state {
                    RowState::Moving { matched, .. } => *matched = *n,
                    state @ RowState::Hashing => {
                        *state = RowState::Moving {
                            literal: 0,
                            matched: *n,
                        }
                    }
                }
                row.last_event = now;
            }
            Event::FileDone(p, size) => {
                self.rows.remove(p);
                self.xfr += 1;
                self.done_bytes = self.done_bytes.saturating_add(*size);
            }
            Event::Skipped(_, size) => {
                self.skipped_local = self.skipped_local.saturating_add(*size);
            }
            Event::PeerSkipped(n) => self.skipped_peer = self.skipped_peer.max(*n),
            Event::Scanned {
                party,
                entries,
                bytes,
                at,
            } => {
                let w = self.walk_mut(*party);
                w.entries = w.entries.max(*entries);
                w.bytes = w.bytes.max(*bytes);
                if at.is_some() {
                    w.at = at.clone();
                }
            }
            Event::Total {
                party,
                files,
                bytes,
            } => {
                let w = self.walk_mut(*party);
                w.done = true;
                w.entries = w.entries.max(*files);
                w.bytes = *bytes;
            }
            Event::Delete { done, total, .. } => {
                if let Some(d) = &mut self.delete {
                    d.started = true;
                    d.done = *done;
                    if total.is_some() {
                        d.total = *total;
                    }
                }
            }
            Event::SessionStart { index, of } => {
                if *index > 0 {
                    self.reset(now);
                }
                self.session = (*index, *of);
            }
            Event::Conn(c) => self.conn = *c,
            _ => {}
        }
    }

    /// Drops rows idle for [`ROW_EXPIRY`]; recomputes rates; marks the
    /// delete phase finished. Called each redraw.
    pub fn tick(&mut self, now: Instant) {
        self.rows
            .retain(|_, r| now.saturating_duration_since(r.last_event) < ROW_EXPIRY);
        for row in self.rows.values_mut() {
            if !matches!(row.state, RowState::Moving { .. }) {
                continue;
            }
            let window = now.saturating_duration_since(row.rate_from);
            if window >= RATE_WINDOW {
                row.rate = row.rate_bytes as f64 / window.as_secs_f64();
                row.rate_from = now;
                row.rate_bytes = 0;
            }
        }
        if let Some(d) = &mut self.delete {
            if d.started && d.total == Some(d.done) {
                d.finished = true;
            }
        }
    }

    /// (numerator, denominator) for the transfer percent, with rsync's
    /// `--info=progress2` meaning: bytes accounted for over the source's
    /// total. The numerator is capped at the denominator.
    pub fn transfer_fraction(&self) -> (u64, u64) {
        let total = match self.source {
            Party::Local => self.local.bytes,
            Party::Peer => self.peer.bytes,
        };
        let in_flight: u64 = self.rows.values().map(Row::position).sum();
        let done = self
            .done_bytes
            .saturating_add(self.skipped_local)
            .saturating_add(self.skipped_peer)
            .saturating_add(in_flight);
        (done.min(total), total)
    }

    /// 0..=100, `None` when the denominator is 0.
    pub fn transfer_percent(&self) -> Option<u8> {
        let (num, den) = self.transfer_fraction();
        (den > 0).then(|| (num as u128 * 100 / den as u128) as u8)
    }

    pub fn in_flight(&self) -> usize {
        self.rows.len()
    }

    /// Clears per-session state for a new session; keeps `conn`, the source
    /// party and the delete mode.
    pub fn reset(&mut self, now: Instant) {
        let mode = self.delete.as_ref().map(|d| d.mode);
        *self = Live {
            conn: self.conn,
            delete: mode.map(DeletePhase::new),
            ..Live::new(self.source, DeleteMode::Off, now)
        };
    }
}
