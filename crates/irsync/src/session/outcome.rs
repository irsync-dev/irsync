//! What a session did, from one side's point of view.

use super::reconnect;
use crate::fs::paths::RelPath;
use crate::telemetry::meter::Usage;

/// What a session did, from one side's point of view.
#[derive(Debug, Clone, Default)]
pub struct Outcome {
    pub session_id: [u8; 16],
    /// The daemon resumed an existing session (`HelloAck.resumed`).
    pub resumed: bool,
    /// Files placed (receiver) or acknowledged as placed (sender) on this connection.
    pub files_transferred: u64,
    /// File data bytes that crossed the network on data streams.
    pub bytes_transferred: u64,
    /// Regular files found up to date (known on the receiving side only).
    pub files_skipped: u64,
    /// Files this side's hashers had to read during the session.
    pub files_hashed: u64,
    pub errors: Vec<(RelPath, String)>,
    /// Source entries by kind and their total size (`--stats`).
    pub listed: crate::scan::ListStats,
    /// Items created new at the destination (files, dirs, links, specials).
    pub created: u64,
    /// Sum of the sizes of the regular files transferred.
    pub transferred_size: u64,
    /// Bytes this side's connection sent and received (QUIC level).
    pub bytes_sent: u64,
    pub bytes_received: u64,
    /// Paths the receiver deleted (`--delete*`), deepest first. A pushing
    /// client gets only the first of them from the daemon (at most 1000,
    /// and at most 64 KiB); [`Outcome::deleted_count`] counts them all.
    pub deleted: Vec<RelPath>,
    /// Deleted paths the daemon left out of `deleted`.
    pub deleted_omitted: u64,
    /// `--dry-run`: paths the receiver would have deleted, capped as
    /// `deleted` is.
    pub would_delete: Vec<RelPath>,
    /// Would-delete paths the daemon left out of `would_delete`.
    pub would_delete_omitted: u64,
    /// Deletions stopped at `--max-delete` with candidates left.
    pub max_delete_reached: bool,
    /// `--paranoid`: listed source files that were gone (or no longer
    /// regular files) at the final re-check. A pulling client gets only
    /// the first of them from the daemon, capped as `deleted` is.
    pub vanished: Vec<RelPath>,
    /// Vanished paths the daemon left out of `vanished`.
    pub vanished_omitted: u64,
    /// What this side spent its time and bytes on.
    pub usage: Usage,
    /// Times the client dialled again after losing the connection.
    pub reconnects: u32,
    /// How the last connection was made (`Direct` or `Relay`), if known.
    pub conn: Option<reconnect::ConnState>,
    /// The key exchange group of the last connection, as rustls names it
    /// (`X25519MLKEM768`), if known. Set on the client only.
    pub key_exchange: Option<String>,
    /// Per-file records the role still held when it finished (should be 0).
    #[cfg(feature = "test-hooks")]
    pub retained_files: usize,
}

impl Outcome {
    /// Adds another session's counts (several sources in one run).
    pub fn absorb(&mut self, o: Outcome) {
        self.files_transferred += o.files_transferred;
        self.bytes_transferred += o.bytes_transferred;
        self.files_skipped += o.files_skipped;
        self.files_hashed += o.files_hashed;
        self.listed.add(&o.listed);
        self.created += o.created;
        self.transferred_size += o.transferred_size;
        self.bytes_sent += o.bytes_sent;
        self.bytes_received += o.bytes_received;
        self.errors.extend(o.errors);
        self.deleted.extend(o.deleted);
        self.deleted_omitted += o.deleted_omitted;
        self.would_delete.extend(o.would_delete);
        self.would_delete_omitted += o.would_delete_omitted;
        self.max_delete_reached |= o.max_delete_reached;
        self.vanished.extend(o.vanished);
        self.vanished_omitted += o.vanished_omitted;
        self.usage.add(&o.usage);
    }

    /// Every path the receiver deleted, listed or not.
    pub fn deleted_count(&self) -> u64 {
        self.deleted.len() as u64 + self.deleted_omitted
    }

    /// `--paranoid`: every listed source file that vanished, listed or not.
    pub fn vanished_count(&self) -> u64 {
        self.vanished.len() as u64 + self.vanished_omitted
    }

    /// `--dry-run`: every path the receiver would have deleted.
    pub fn would_delete_count(&self) -> u64 {
        self.would_delete.len() as u64 + self.would_delete_omitted
    }

    /// rsync's convention: 0 ok, 23 partial transfer due to error, 24 some
    /// source files vanished (`--paranoid`'s final re-check), 25 deletions
    /// stopped at `--max-delete`. 25 outranks 23, which outranks 24.
    pub fn exit_code(&self) -> i32 {
        if self.max_delete_reached {
            25
        } else if !self.errors.is_empty() {
            23
        } else if self.vanished_count() > 0 {
            24
        } else {
            0
        }
    }
}
