//! Test hooks: what a test build can make a session do, or see it do.
//! In a normal build `SideHooks` is empty.

#[cfg(feature = "test-hooks")]
use std::path::PathBuf;
use std::sync::Arc;

#[cfg(feature = "test-hooks")]
use iroh::endpoint::Connection;

use crate::fs::paths::RelPath;

/// Test hook: closes the connection (code 99) once the daemon's data bytes
/// (received, as receiver; written, as sender) cross a limit — exactly once.
#[cfg(feature = "test-hooks")]
#[derive(Debug, Default)]
pub struct FaultInjector {
    limit: std::sync::atomic::AtomicU64,
    armed: std::sync::atomic::AtomicBool,
    fired: std::sync::atomic::AtomicBool,
    after_ack: std::sync::atomic::AtomicBool,
    stall: std::sync::atomic::AtomicBool,
}

#[cfg(feature = "test-hooks")]
impl FaultInjector {
    pub fn drop_after_bytes(&self, n: u64) {
        use std::sync::atomic::Ordering::SeqCst;
        self.limit.store(n, SeqCst);
        self.armed.store(true, SeqCst);
    }

    /// [`Self::drop_after_bytes`] for a fault that has fired before: it
    /// fires once more.
    pub fn drop_again_after_bytes(&self, n: u64) {
        self.fired.store(false, std::sync::atomic::Ordering::SeqCst);
        self.drop_after_bytes(n);
    }

    /// Every session from now on: close the connection as soon as the
    /// client has the `HelloAck` (its first frame after it arrives), before
    /// any file data moves. Repeatedly, not once.
    pub fn drop_after_ack(&self) {
        self.after_ack
            .store(true, std::sync::atomic::Ordering::SeqCst);
    }

    pub(crate) fn drops_after_ack(&self) -> bool {
        self.after_ack.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// The next session only: after `HelloAck`, send and read nothing more
    /// (the connection stays open) until the client gives up on it.
    pub fn stall_after_ack_once(&self) {
        self.stall.store(true, std::sync::atomic::Ordering::SeqCst);
    }

    pub(crate) fn take_stall(&self) -> bool {
        self.stall.swap(false, std::sync::atomic::Ordering::SeqCst)
    }

    pub fn fired(&self) -> bool {
        self.fired.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// Called with a connection's running byte count. A connection already
    /// closed is not the one to drop: the data streams of one this fault
    /// dropped may still be reading what had arrived, past the limit, after
    /// the fault was armed again for the next connection.
    pub(crate) fn observe(&self, total: u64, conn: &Connection) {
        use std::sync::atomic::Ordering::SeqCst;
        if self.armed.load(SeqCst)
            && total >= self.limit.load(SeqCst)
            && conn.close_reason().is_none()
            && !self.fired.swap(true, SeqCst)
        {
            tracing::warn!(total, "fault injection: dropping the connection");
            conn.close(99u32.into(), b"fault");
        }
    }
}

/// What a receiver did, in order: `delete:<path>` per deletion and
/// `plan:<path>` per requested file (test hook).
pub type EventLog = Arc<std::sync::Mutex<Vec<String>>>;

/// Called by the sender role after the first chunk of a path went out, before
/// the rest; lets a test change the file mid-transfer.
#[cfg(feature = "test-hooks")]
pub type FirstChunkHook =
    Arc<dyn Fn(&RelPath) -> futures_util::future::BoxFuture<'static, ()> + Send + Sync>;

/// Test hook: runs at a point in a file's handling (see [`SideHooks`]).
#[cfg(feature = "test-hooks")]
pub type PathHook =
    Arc<dyn Fn(&RelPath) -> futures_util::future::BoxFuture<'static, ()> + Send + Sync>;

/// Test-only hooks; empty in normal builds.
#[derive(Clone, Default)]
pub struct SideHooks {
    #[cfg(feature = "test-hooks")]
    pub on_first_chunk_sent: Option<FirstChunkHook>,
    /// Where this side writes its file report, in preference to the
    /// `IRSYNC_FILE_REPORT` variable (see [`crate::file_report`]).
    #[cfg(feature = "test-hooks")]
    pub file_report: Option<PathBuf>,
    /// Cuts data streams short (see [`crate::data_stream::TruncateHook`]).
    #[cfg(feature = "test-hooks")]
    pub truncate_streams: Option<Arc<crate::data_stream::TruncateHook>>,
    /// Receives the receiver role's events (see [`EventLog`]).
    #[cfg(feature = "test-hooks")]
    pub events: Option<EventLog>,
    /// Either role's walk lists these extra entries after an entry (crafts
    /// a hostile listing); it runs on the listing thread, so it may also
    /// hold the walk back. On a receiver the extra entries reach only the
    /// peer, not this side's planner.
    #[cfg(feature = "test-hooks")]
    pub inject_listing: Option<ListingHook>,
    /// Receiver: cap on files in flight instead of the default 4096.
    #[cfg(feature = "test-hooks")]
    pub max_inflight: Option<usize>,
    /// Receiver: each file's planning takes this much longer (a slow
    /// planner, so the peer's listing backs up).
    #[cfg(feature = "test-hooks")]
    pub slow_plan: Option<std::time::Duration>,
    /// Receiver: the most peer-listing entries it has had queued for the
    /// planner at once.
    #[cfg(feature = "test-hooks")]
    pub listing_peak: Option<Arc<std::sync::atomic::AtomicUsize>>,
    /// Lister: counts the times the listing queue was full when the walk
    /// had a frame to queue (the walk waited for the link or the peer).
    #[cfg(feature = "test-hooks")]
    pub listing_waits: Option<Arc<std::sync::atomic::AtomicU64>>,
    /// Both roles: the most peer events (control frames, listing signals)
    /// the stream readers have had queued for the main loop at once.
    #[cfg(feature = "test-hooks")]
    pub peer_events_peak: Option<Arc<std::sync::atomic::AtomicUsize>>,
    /// Both roles: the main loop takes this much longer per peer event (a
    /// slow main loop, so the peer's frames back up).
    #[cfg(feature = "test-hooks")]
    pub slow_peer_events: Option<std::time::Duration>,
    /// Sender: the main loop holds the peer's first `Itemized` until this
    /// turns true, so the peer's frames back up however fast it makes
    /// them (a client that stops reading for a while).
    #[cfg(feature = "test-hooks")]
    pub hold_itemized: Option<tokio::sync::watch::Receiver<bool>>,
    /// Both roles: the most bytes of paced control frames (see
    /// `Out::send_paced`) queued for the control stream's writer at once.
    #[cfg(feature = "test-hooks")]
    pub out_peak: Option<Arc<std::sync::atomic::AtomicUsize>>,
    /// Both roles: bytes of control frames queued for the writer and not
    /// yet written above which the peer counts as not reading (see
    /// `limits::out_ceiling`), instead of the default.
    #[cfg(feature = "test-hooks")]
    pub out_ceiling: Option<usize>,
    /// Both roles: bytes of chunk bases (see `limits::BASIS_BUDGET`) the
    /// session holds at once, instead of the default.
    #[cfg(feature = "test-hooks")]
    pub basis_budget: Option<usize>,
    /// Sender: the most bytes of chunk bases it has held at once.
    #[cfg(feature = "test-hooks")]
    pub basis_peak: Option<Arc<std::sync::atomic::AtomicUsize>>,
    /// Receiver: the most chunk indices one `NeedChunks` carries (see
    /// `limits::NEED_PIECE`), instead of the default.
    #[cfg(feature = "test-hooks")]
    pub need_piece: Option<usize>,
    /// Sender: the most send tasks it has had running at once for the
    /// files in flight.
    #[cfg(feature = "test-hooks")]
    pub send_tasks_peak: Option<Arc<std::sync::atomic::AtomicUsize>>,
    /// Sender: bytes of `-H` link checks it holds waiting at once (see
    /// `limits::LINK_BACKLOG`), instead of the default.
    #[cfg(feature = "test-hooks")]
    pub link_backlog: Option<usize>,
    /// Sender: the most bytes of link checks it has held waiting at once.
    #[cfg(feature = "test-hooks")]
    pub link_backlog_peak: Option<Arc<std::sync::atomic::AtomicUsize>>,
    /// Both roles: how long, at a session's end, the side waits for its
    /// control writer (the daemon also for the client to close) while
    /// nothing moves, instead of the default.
    #[cfg(feature = "test-hooks")]
    pub close_quiet: Option<std::time::Duration>,
    /// Receiver: how long it waits for the sender to close its end after
    /// `SessionDone`, instead of the default.
    #[cfg(feature = "test-hooks")]
    pub close_timeout: Option<std::time::Duration>,
    /// Both roles: the control reader stops reading once it has passed on
    /// the peer's `SessionDone`, keeping the stream open (a peer that does
    /// not read).
    #[cfg(feature = "test-hooks")]
    pub stop_reading_after_done: bool,
    /// Both roles: chunk records of room for lists (see
    /// `limits::LIST_BUDGET`) each connection starts with, instead of the
    /// default.
    #[cfg(feature = "test-hooks")]
    pub list_budget: Option<u64>,
    /// Receiver: the most chunk records of lists it has held at once.
    #[cfg(feature = "test-hooks")]
    pub list_peak: Option<Arc<std::sync::atomic::AtomicU64>>,
    /// Sender: the most chunk records of lists it has held at once
    /// digested and waiting for room.
    #[cfg(feature = "test-hooks")]
    pub lists_ahead_peak: Option<Arc<std::sync::atomic::AtomicU64>>,
    /// Receiver: each `Done::Planned` reaches the main loop this much later
    /// than the planner posts it (a completion hidden behind another
    /// producer's half-written slot), still before the planner finishes.
    #[cfg(feature = "test-hooks")]
    pub delay_planned: Option<std::time::Duration>,
    /// Receiver: the most files it has had in flight at once.
    #[cfg(feature = "test-hooks")]
    pub inflight_peak: Option<Arc<std::sync::atomic::AtomicUsize>>,
    /// Receiver: every finalize takes this much longer (slow storage).
    #[cfg(feature = "test-hooks")]
    pub slow_finalize: Option<std::time::Duration>,
    /// Receiver: the main loop hears of each placed file this much later
    /// (a connection can drop in between).
    #[cfg(feature = "test-hooks")]
    pub slow_place: Option<std::time::Duration>,
    /// Receiver: every local chunk copy takes this much longer (slow disk).
    #[cfg(feature = "test-hooks")]
    pub slow_local_copy: Option<std::time::Duration>,
    /// Receiver: runs once a `DELTA` file's content has proved unchanged,
    /// before the destination is checked to still be the file that was
    /// hashed (a test replaces it meanwhile).
    #[cfg(feature = "test-hooks")]
    pub before_identical_check: Option<PathHook>,
    /// Sender: records per chunk list part instead of the default.
    #[cfg(feature = "test-hooks")]
    pub chunk_list_part_records: Option<usize>,
    /// Sender: close the connection once this many parts have been queued
    /// (once per hook instance, across reconnects).
    #[cfg(feature = "test-hooks")]
    pub drop_after_parts: Option<Arc<PartsFault>>,
    /// Sender: close the connection once this many listing frames have been
    /// queued (once per hook instance, across reconnects).
    #[cfg(feature = "test-hooks")]
    pub drop_after_listing: Option<Arc<PartsFault>>,
    /// Sender: close the connection instead of answering a `MayPlace`
    /// (once per hook instance, across reconnects; `PartsFault::after(1)`
    /// drops at the first one).
    #[cfg(feature = "test-hooks")]
    pub drop_before_verdict: Option<Arc<PartsFault>>,
    /// Sender: answer a `MayPlace` without comparing the source's stat with
    /// its hash stat (only `--paranoid`'s second hash is left to see a
    /// change).
    #[cfg(feature = "test-hooks")]
    pub skip_place_stat: bool,
    /// Sender, `--paranoid`: make clone folders no higher than this folder
    /// (see [`crate::platform::clone::Clones::within`]). Unset, a test build makes
    /// no clones, so no test leaves a folder outside its own.
    #[cfg(feature = "test-hooks")]
    pub clones_within: Option<PathBuf>,
    /// Sender: turn off every check for a source that changed (the stat
    /// after hashing, the stat and the second hash before placing,
    /// `--paranoid`'s clones and final re-check), which brings back the
    /// bug of a torn file landing with exit 0. Set from
    /// `IRSYNC_TEST_NO_CHANGE_CHECKS` by the binary.
    #[cfg(feature = "test-hooks")]
    pub no_change_checks: bool,
    /// Sender: queue only this many listing frames, then none, not even
    /// `ListingEnd` (a lister that stops short).
    #[cfg(feature = "test-hooks")]
    pub cut_listing_after: Option<usize>,
    /// Both roles: every outgoing frame of the control stream passes
    /// through this; the frames it returns go out instead (a test drops,
    /// duplicates, alters or injects frames).
    #[cfg(feature = "test-hooks")]
    pub tamper_frames: Option<FrameHook>,
    /// Both roles: as `tamper_frames`, for the listing stream.
    #[cfg(feature = "test-hooks")]
    pub tamper_listing: Option<ListingFrameHook>,
    /// Receiver: counts the `NeedChunks` re-requests it sends.
    #[cfg(feature = "test-hooks")]
    pub rerequests: Option<Arc<std::sync::atomic::AtomicU64>>,
    /// Receiver: runs once a `DELTA` file's chunk bases have been sent,
    /// before the final request goes out (the last batch's chunks and the
    /// rest; earlier batches were requested already); a test changes the
    /// old file meanwhile.
    #[cfg(feature = "test-hooks")]
    pub after_bases: Option<PathHook>,
    /// Receiver: runs when a file's chunk list is about to be opened,
    /// before the old file is looked at (a test changes it meanwhile).
    #[cfg(feature = "test-hooks")]
    pub before_open: Option<PathHook>,
    /// This side's hashers tell it about every pass that reads a file (see
    /// [`crate::hasher::HashHook`]; a test changes the file mid-pass).
    #[cfg(feature = "test-hooks")]
    pub during_hash: Option<crate::hasher::HashHook>,
    /// Daemon: its check for an inner export (see `daemon::inner`) fails
    /// (it panics): the request must be refused.
    #[cfg(feature = "test-hooks")]
    pub fail_inner_export_check: bool,
    /// Daemon: its check for an inner export meets an I/O error: the
    /// request must be refused.
    #[cfg(feature = "test-hooks")]
    pub inner_export_check_io_error: bool,
    /// Client: changes its `Hello` after the options are set and checked
    /// (crafts a hostile request the client itself would refuse).
    #[cfg(feature = "test-hooks")]
    pub tamper_hello: Option<fn(&mut crate::session::SyncHello)>,
}

/// Test hook: a connection drop after a number of frames of one kind
/// (chunk-list parts, listing frames).
#[cfg(feature = "test-hooks")]
#[derive(Debug, Default)]
pub struct PartsFault {
    after: usize,
    sent: std::sync::atomic::AtomicUsize,
    fired: std::sync::atomic::AtomicBool,
}

#[cfg(feature = "test-hooks")]
impl PartsFault {
    pub fn after(n: usize) -> Arc<PartsFault> {
        Arc::new(PartsFault {
            after: n,
            ..PartsFault::default()
        })
    }

    pub fn fired(&self) -> bool {
        self.fired.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// Counts one queued frame; true when the connection is to be dropped now.
    pub(crate) fn queued(&self) -> bool {
        use std::sync::atomic::Ordering::SeqCst;
        self.sent.fetch_add(1, SeqCst) + 1 >= self.after && !self.fired.swap(true, SeqCst)
    }
}

/// Test hook: the frames to send in place of one outgoing control frame
/// (see [`SideHooks`]).
#[cfg(feature = "test-hooks")]
pub type FrameHook = Arc<
    dyn Fn(irsync_proto::v1::control::Msg) -> Vec<irsync_proto::v1::control::Msg> + Send + Sync,
>;

/// Test hook: the frames to send in place of one outgoing listing frame
/// (see [`SideHooks`]).
#[cfg(feature = "test-hooks")]
pub type ListingFrameHook = Arc<
    dyn Fn(irsync_proto::v1::listing_frame::Frame) -> Vec<irsync_proto::v1::listing_frame::Frame>
        + Send
        + Sync,
>;

/// [`SideHooks::tamper_frames`] and [`SideHooks::tamper_listing`] as the
/// stream writers carry them; empty (and a no-op) without test hooks.
#[derive(Clone, Default)]
pub(crate) struct Tamper {
    #[cfg(feature = "test-hooks")]
    control: Option<FrameHook>,
    #[cfg(feature = "test-hooks")]
    listing: Option<ListingFrameHook>,
}

impl Tamper {
    /// The frames to send in place of `msg` on the control stream.
    #[cfg(feature = "test-hooks")]
    pub(crate) fn frames(
        &self,
        msg: irsync_proto::v1::control::Msg,
    ) -> impl Iterator<Item = irsync_proto::v1::control::Msg> {
        match &self.control {
            Some(h) => h(msg),
            None => vec![msg],
        }
        .into_iter()
    }

    #[cfg(not(feature = "test-hooks"))]
    pub(crate) fn frames(
        &self,
        msg: irsync_proto::v1::control::Msg,
    ) -> impl Iterator<Item = irsync_proto::v1::control::Msg> {
        std::iter::once(msg)
    }

    /// The frames to send in place of `frame` on the listing stream.
    #[cfg(feature = "test-hooks")]
    pub(crate) fn listing_frames(
        &self,
        frame: irsync_proto::v1::listing_frame::Frame,
    ) -> impl Iterator<Item = irsync_proto::v1::listing_frame::Frame> {
        match &self.listing {
            Some(h) => h(frame),
            None => vec![frame],
        }
        .into_iter()
    }

    #[cfg(not(feature = "test-hooks"))]
    pub(crate) fn listing_frames(
        &self,
        frame: irsync_proto::v1::listing_frame::Frame,
    ) -> impl Iterator<Item = irsync_proto::v1::listing_frame::Frame> {
        std::iter::once(frame)
    }
}

/// Test hook: entries to list right after `entry` (see [`SideHooks`]).
#[cfg(feature = "test-hooks")]
pub type ListingHook =
    Arc<dyn Fn(&irsync_proto::v1::FileEntry) -> Vec<irsync_proto::v1::FileEntry> + Send + Sync>;

/// Whether `IRSYNC_TEST_NO_CHANGE_CHECKS=1` is set (see
/// [`SideHooks::no_change_checks`]); only a test-hooks build reads it.
#[cfg(feature = "test-hooks")]
pub(crate) fn no_change_checks_from_env() -> bool {
    no_change_checks_value(
        std::env::var("IRSYNC_TEST_NO_CHANGE_CHECKS")
            .ok()
            .as_deref(),
    )
}

/// The switch is on only for the value `1`.
#[cfg(feature = "test-hooks")]
fn no_change_checks_value(v: Option<&str>) -> bool {
    v == Some("1")
}

#[cfg(all(test, feature = "test-hooks"))]
mod no_change_checks_tests {
    use super::no_change_checks_value;

    #[test]
    fn only_one_turns_the_switch_on() {
        assert!(!no_change_checks_value(None));
        assert!(no_change_checks_value(Some("1")));
        for v in ["0", "", "yes", "true", " 1"] {
            assert!(!no_change_checks_value(Some(v)), "{v:?}");
        }
    }
}

impl SideHooks {
    /// Whether a test turned the change checks off (always false in a
    /// normal build).
    pub(crate) fn no_change_checks(&self) -> bool {
        #[cfg(feature = "test-hooks")]
        return self.no_change_checks;
        #[cfg(not(feature = "test-hooks"))]
        false
    }

    /// The `tamper_frames` and `tamper_listing` hooks for the stream writers.
    pub(crate) fn tamper(&self) -> Tamper {
        Tamper {
            #[cfg(feature = "test-hooks")]
            control: self.tamper_frames.clone(),
            #[cfg(feature = "test-hooks")]
            listing: self.tamper_listing.clone(),
        }
    }

    /// Writes one line of the file report: to this side's test sink if it
    /// has one, else to the `IRSYNC_FILE_REPORT` file.
    pub(crate) fn report(&self, event: crate::file_report::ReportEvent, path: &RelPath, why: &str) {
        #[cfg(feature = "test-hooks")]
        if let Some(sink) = &self.file_report {
            crate::file_report::record_to(sink, event, path, why);
            return;
        }
        crate::file_report::record(event, path, why);
    }

    /// Records one receiver event (test hook; a no-op otherwise).
    #[cfg_attr(not(feature = "test-hooks"), allow(unused_variables))]
    pub(crate) fn event(&self, what: &str, path: &RelPath) {
        #[cfg(feature = "test-hooks")]
        if let Some(log) = &self.events {
            log.lock().unwrap_or_else(|e| e.into_inner()).push(format!(
                "{what}:{}",
                String::from_utf8_lossy(path.as_bytes())
            ));
        }
    }
}

impl std::fmt::Debug for SideHooks {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SideHooks")
    }
}
