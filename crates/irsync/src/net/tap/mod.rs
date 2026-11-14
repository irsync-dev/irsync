//! Taps: per-connection and per-stream observers of protocol frames.
//!
//! A [`ConnTap`] carries what is known about one QUIC connection (side,
//! number, endpoint ids, session); a [`StreamTap`] adds one stream's kind, id
//! and file id. The tapped frame functions in [`crate::net::frame`] report
//! each message through a stream tap as a capture [`Record`].
//!
//! A tap is on only when the wire trace or a capture recorder is on when
//! [`ConnTap::of`] first sees the connection. A tap that is off is a `None`:
//! one branch, no allocation, no registry access.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, OnceLock};

use iroh::endpoint::{Connection, WeakConnectionHandle};
use iroh::EndpointId;
use irsync_proto::capture::v1::{record, stream_event, Direction, PathChange, Record, StreamEvent};
use irsync_proto::v1::{
    control, Control, DataChunk, DataStreamHeader, Error, ExportList, Hello, HelloAck,
    ListingStreamHeader, PathInfo,
};

mod capture;
mod pcapng;
mod scrub;
mod text;

pub use irsync_proto::capture::v1::{Side, StreamKind};

/// A protocol message the taps can report.
pub trait WireMsg: prost::Message + Default + Clone {
    /// The message as a [`Record`]'s payload.
    fn to_record(&self) -> record::Msg;
    /// The file the message is about, where it names one.
    fn file_id(&self) -> Option<u64> {
        None
    }
}

macro_rules! wire_msg {
    ($($ty:ident => $variant:ident),* $(,)?) => {$(
        impl WireMsg for $ty {
            fn to_record(&self) -> record::Msg {
                record::Msg::$variant(self.clone())
            }
        }
    )*};
}

wire_msg! {
    Hello => Hello,
    HelloAck => HelloAck,
    Error => Error,
    ExportList => ExportList,
    PathInfo => PathInfo,
    ListingStreamHeader => ListingStreamHeader,
    DataChunk => DataChunk,
}

impl WireMsg for Control {
    fn to_record(&self) -> record::Msg {
        record::Msg::Control(self.clone())
    }
    fn file_id(&self) -> Option<u64> {
        use control::Msg;
        match self.msg.as_ref()? {
            Msg::Plan(m) => Some(m.file_id),
            Msg::Need(m) => Some(m.file_id),
            Msg::Ack(m) => Some(m.file_id),
            Msg::ChunkListPart(m) => Some(m.file_id),
            Msg::ChunkBasis(m) => Some(m.file_id),
            Msg::MayPlace(m) => Some(m.file_id),
            Msg::PlaceVerdict(m) => Some(m.file_id),
            _ => None,
        }
    }
}

impl WireMsg for DataStreamHeader {
    fn to_record(&self) -> record::Msg {
        record::Msg::DataStreamHeader(self.clone())
    }
    fn file_id(&self) -> Option<u64> {
        Some(self.file_id)
    }
}

/// Whether `-vvvvv` (or `RUST_LOG`) asks for every protocol message.
pub(crate) fn wire_trace_on() -> bool {
    tracing::enabled!(target: "irsync::wire", tracing::Level::TRACE)
}

/// Whether a capture recorder exists. Opens it on the first call when
/// `IRSYNC_CAPTURE` is set; after that, one atomic load.
fn recorder_on() -> bool {
    capture::recorder().is_some()
}

fn any_on() -> bool {
    #[cfg(test)]
    if tests::forced_on() {
        return true;
    }
    wire_trace_on() || recorder_on()
}

/// Hands a finished record to the outputs that are on.
fn sink(conn: &ConnInner, record: Record) {
    if wire_trace_on() {
        text::emit(&record);
    }
    #[cfg(test)]
    if let Some(c) = &conn.collector {
        c.lock().unwrap().push(record.clone());
    }
    #[cfg(not(test))]
    let _ = conn;
    // Scrubs file content before anything reaches the file.
    capture::submit(record);
}

/// Next connection number, process-wide, from 1.
static NEXT_CONNECTION: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(1);

struct Entry {
    /// Keeps the connection's allocation, and so its `stable_id`, from being
    /// reused while the entry exists.
    _pin: WeakConnectionHandle,
    tap: Arc<ConnInner>,
}

/// The taps of open connections, by `Connection::stable_id`. Only touched
/// when a tap is on; an entry goes when its connection closes.
static REGISTRY: Mutex<BTreeMap<usize, Entry>> = Mutex::new(BTreeMap::new());

struct ConnInner {
    side: Side,
    connection: u32,
    remote: [u8; 32],
    local: OnceLock<[u8; 32]>,
    session: OnceLock<[u8; 16]>,
    #[cfg(test)]
    collector: Option<Arc<Mutex<Vec<Record>>>>,
}

/// One connection's tap; off when it holds nothing.
#[derive(Clone, Default)]
pub struct ConnTap(Option<Arc<ConnInner>>);

impl ConnTap {
    /// The tap of `conn`: the same one for every call on the same connection,
    /// off unless the wire trace or a capture recorder is on.
    pub fn of(conn: &Connection) -> ConnTap {
        if !any_on() {
            return ConnTap(None);
        }
        let key = conn.stable_id();
        let mut reg = REGISTRY.lock().unwrap();
        if let Some(e) = reg.get(&key) {
            return ConnTap(Some(e.tap.clone()));
        }
        let side = match conn.side() {
            iroh::endpoint::Side::Client => Side::Client,
            iroh::endpoint::Side::Server => Side::Daemon,
        };
        let tap = Arc::new(ConnInner {
            side,
            connection: NEXT_CONNECTION.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
            remote: *conn.remote_id().as_bytes(),
            local: OnceLock::new(),
            session: OnceLock::new(),
            #[cfg(test)]
            collector: None,
        });
        let weak = conn.weak_handle();
        let closed = weak.closed();
        reg.insert(
            key,
            Entry {
                _pin: weak,
                tap: tap.clone(),
            },
        );
        drop(reg);
        if let Ok(rt) = tokio::runtime::Handle::try_current() {
            rt.spawn(async move {
                closed.await;
                REGISTRY.lock().unwrap().remove(&key);
            });
            if recorder_on() {
                watch_paths(&rt, conn, tap.clone());
            }
        }
        ConnTap(Some(tap))
    }

    /// Records the session id once the Hello names it; later calls are ignored.
    pub fn set_session(&self, id: &[u8]) {
        if let (Some(c), Ok(id)) = (&self.0, <[u8; 16]>::try_from(id)) {
            let _ = c.session.set(id);
        }
    }

    /// Records this side's endpoint id; later calls are ignored. A
    /// connection does not know its local endpoint, so the caller that holds
    /// the endpoint sets it.
    pub fn set_local_id(&self, id: EndpointId) {
        if let Some(c) = &self.0 {
            let _ = c.local.set(*id.as_bytes());
        }
    }

    /// Whether this tap reports anything.
    pub fn is_on(&self) -> bool {
        self.0.is_some()
    }

    /// QUIC client is [`Side::Client`], QUIC server [`Side::Daemon`];
    /// unspecified when off.
    pub fn side(&self) -> Side {
        self.0.as_ref().map_or(Side::Unspecified, |c| c.side)
    }

    /// The connection's number, from 1; 0 when off.
    pub fn connection(&self) -> u32 {
        self.0.as_ref().map_or(0, |c| c.connection)
    }

    /// The session id, once set.
    pub fn session(&self) -> Option<[u8; 16]> {
        self.0.as_ref().and_then(|c| c.session.get().copied())
    }

    /// A tap for one stream of this connection; records it as opened.
    pub fn stream(&self, kind: StreamKind, quic_stream_id: u64) -> StreamTap {
        let Some(conn) = &self.0 else {
            return StreamTap::off();
        };
        let tap = StreamTap(Some(Arc::new(StreamInner {
            conn: conn.clone(),
            kind,
            quic_stream_id,
            file_id: OnceLock::new(),
        })));
        tap.event(stream_event::Kind::Opened, Direction::Unspecified, "");
        tap
    }
}

/// The selected path's remote address as iroh prints it (`ip:…`,
/// `relay:…`), if a path is selected.
fn selected_path(conn: &Connection) -> Option<String> {
    let paths = conn.paths();
    let selected = paths.iter().find(|p| p.is_selected());
    selected.map(|p| p.remote_addr().to_string())
}

/// Records the selected path now and each time it changes, until the
/// connection closes. Holds only a weak handle, so it never keeps the
/// connection open.
fn watch_paths(rt: &tokio::runtime::Handle, conn: &Connection, tap: Arc<ConnInner>) {
    use futures::StreamExt as _;
    use iroh::endpoint::PathEvent;
    // Subscribe before the first look, so no change falls in between.
    let mut events = conn.path_events();
    let weak = conn.weak_handle();
    let mut last = selected_path(conn);
    if let Some(remote) = &last {
        tap.path_change(remote);
    }
    rt.spawn(async move {
        while let Some(event) = events.next().await {
            let now = match event {
                PathEvent::Selected { remote_addr, .. } => Some(remote_addr.to_string()),
                PathEvent::Lagged { .. } => weak.upgrade().and_then(|c| selected_path(&c)),
                _ => continue,
            };
            if now.is_some() && now != last {
                tap.path_change(now.as_deref().unwrap_or_default());
                last = now;
            }
        }
    });
}

impl ConnInner {
    /// A record of this connection, not of any one stream.
    fn record(&self, stream: Option<&StreamInner>, direction: Direction) -> Record {
        Record {
            side: self.side.into(),
            direction: direction.into(),
            stream: stream.map_or(StreamKind::Unspecified, |s| s.kind).into(),
            quic_stream_id: stream.map_or(0, |s| s.quic_stream_id),
            session_id: self.session.get().map(|s| s.to_vec()).unwrap_or_default(),
            file_id: None,
            connection: self.connection,
            local_endpoint_id: self.local.get().map(|l| l.to_vec()).unwrap_or_default(),
            remote_endpoint_id: self.remote.to_vec(),
            encoded_len: 0,
            trimmed: None,
            msg: None,
        }
    }

    fn path_change(&self, remote: &str) {
        let mut record = self.record(None, Direction::Unspecified);
        record.msg = Some(record::Msg::PathChange(PathChange {
            remote: remote.to_owned(),
        }));
        sink(self, record);
    }
}

struct StreamInner {
    conn: Arc<ConnInner>,
    kind: StreamKind,
    quic_stream_id: u64,
    /// Set by the stream's `DataStreamHeader`; inherited by its `DataChunk`s.
    file_id: OnceLock<u64>,
}

impl StreamInner {
    fn record(
        &self,
        direction: Direction,
        file_id: Option<u64>,
        encoded_len: usize,
        msg: record::Msg,
    ) {
        let c = &*self.conn;
        let record = Record {
            file_id: file_id.or_else(|| self.file_id.get().copied()),
            encoded_len: u32::try_from(encoded_len).unwrap_or(u32::MAX),
            msg: Some(msg),
            ..c.record(Some(self), direction)
        };
        sink(c, record);
    }
}

/// One stream's tap; off when it holds nothing.
#[derive(Clone, Default)]
pub struct StreamTap(Option<Arc<StreamInner>>);

impl StreamTap {
    /// A tap that reports nothing.
    pub fn off() -> Self {
        StreamTap(None)
    }

    /// Whether this tap reports anything.
    pub fn is_on(&self) -> bool {
        self.0.is_some()
    }

    /// Reports `m`, `encoded_len` bytes on the wire, as sent.
    pub fn sent<M: WireMsg>(&self, m: &M, encoded_len: usize) {
        self.message(Direction::Sent, m, encoded_len);
    }

    /// Reports `m`, `encoded_len` bytes on the wire, as received.
    pub fn received<M: WireMsg>(&self, m: &M, encoded_len: usize) {
        self.message(Direction::Received, m, encoded_len);
    }

    /// Reports that this side finished sending on the stream.
    pub fn finished(&self) {
        self.event(stream_event::Kind::Finished, Direction::Sent, "");
    }

    /// Reports that the stream was reset or failed, and why.
    pub fn reset(&self, why: &str) {
        self.event(stream_event::Kind::Reset, Direction::Unspecified, why);
    }

    fn message<M: WireMsg>(&self, direction: Direction, m: &M, encoded_len: usize) {
        let Some(s) = &self.0 else { return };
        let file_id = m.file_id();
        if s.kind == StreamKind::Data {
            if let Some(id) = file_id {
                let _ = s.file_id.set(id);
            }
        }
        s.record(direction, file_id, encoded_len, m.to_record());
    }

    pub(crate) fn event(&self, kind: stream_event::Kind, direction: Direction, detail: &str) {
        let Some(s) = &self.0 else { return };
        let ev = StreamEvent {
            kind: kind.into(),
            detail: detail.to_owned(),
        };
        s.record(direction, None, 0, record::Msg::StreamEvent(ev));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net::frame::read_msg_tapped;
    use irsync_proto::capture::v1::stream_event;
    use irsync_proto::v1::{Control, DataChunk, DataStreamHeader};
    use std::cell::Cell;

    thread_local! {
        /// Live [`force_on_for_test`] guards on this thread. Thread-local, so
        /// tests running in parallel do not turn each other's taps on.
        static FORCED: Cell<usize> = const { Cell::new(0) };
    }

    pub(super) fn forced_on() -> bool {
        FORCED.with(|f| f.get() > 0)
    }

    /// Turns taps on, without a recorder, for this thread until dropped.
    pub(crate) struct ForceOn(());

    impl Drop for ForceOn {
        fn drop(&mut self) {
            FORCED.with(|f| f.set(f.get() - 1));
        }
    }

    pub(crate) fn force_on_for_test() -> ForceOn {
        FORCED.with(|f| f.set(f.get() + 1));
        ForceOn(())
    }

    type Records = Arc<Mutex<Vec<Record>>>;

    /// An on stream tap, not tied to a connection, whose records land in the
    /// returned list.
    pub(crate) fn collecting_stream_tap(
        kind: StreamKind,
        quic_stream_id: u64,
    ) -> (StreamTap, Records) {
        let records = Records::default();
        let conn = ConnTap(Some(Arc::new(ConnInner {
            side: Side::Client,
            connection: 0,
            remote: [0; 32],
            local: OnceLock::new(),
            session: OnceLock::new(),
            collector: Some(records.clone()),
        })));
        (conn.stream(kind, quic_stream_id), records)
    }

    /// The kind of the last stream event in `records`.
    pub(crate) fn last_event(records: &Records) -> Option<stream_event::Kind> {
        records
            .lock()
            .unwrap()
            .iter()
            .rev()
            .find_map(|r| match &r.msg {
                Some(record::Msg::StreamEvent(e)) => Some(e.kind()),
                _ => None,
            })
    }

    /// Two offline loopback endpoints, connected: `(client_side, server_side)`.
    /// The endpoints are leaked so the connections stay open for the test.
    async fn loopback_conn_pair() -> (Connection, Connection) {
        use crate::net::{build_endpoint, EndpointOpts};
        let endpoint = || async {
            let mut opts = EndpointOpts::isolated(iroh::SecretKey::generate());
            opts.bind_addrs = vec!["127.0.0.1:0".parse().unwrap()];
            build_endpoint(opts).await.unwrap()
        };
        let (a, b) = (endpoint().await, endpoint().await);
        let pair = tokio::join!(
            async { a.connect(b.addr(), irsync_proto::ALPN).await.unwrap() },
            async { b.accept().await.unwrap().await.unwrap() },
        );
        std::mem::forget((a, b));
        pair
    }

    #[tokio::test]
    async fn off_when_nothing_is_on() {
        let (c, _s) = loopback_conn_pair().await;
        assert!(!ConnTap::of(&c).stream(StreamKind::Control, 0).is_on());
    }

    #[tokio::test]
    async fn sides_follow_quic_roles() {
        let _on = force_on_for_test();
        let (c, s) = loopback_conn_pair().await;
        assert_eq!(ConnTap::of(&c).side(), Side::Client);
        assert_eq!(ConnTap::of(&s).side(), Side::Daemon);
    }

    #[tokio::test]
    async fn second_connection_gets_new_address() {
        let _on = force_on_for_test();
        let (a, _) = loopback_conn_pair().await;
        let (b, _) = loopback_conn_pair().await;
        let (ta, tb) = (ConnTap::of(&a), ConnTap::of(&b));
        ta.set_session(&[1; 16]);
        tb.set_session(&[1; 16]);
        assert_ne!(ta.connection(), tb.connection());
        assert_eq!(ta.session(), tb.session());
    }

    #[tokio::test]
    async fn data_chunk_inherits_header_file_id() {
        let (tap, records) = collecting_stream_tap(StreamKind::Data, 2);
        tap.sent(
            &DataStreamHeader {
                file_id: 12,
                ..Default::default()
            },
            5,
        );
        tap.sent(&DataChunk::default(), 0);
        assert_eq!(records.lock().unwrap()[2].file_id, Some(12)); // [0] is OPENED
    }

    #[tokio::test]
    async fn tapped_read_records_finish() {
        let (tap, records) = collecting_stream_tap(StreamKind::Control, 0);
        let mut empty: &[u8] = &[];
        assert!(read_msg_tapped::<Control, _>(&mut empty, &tap)
            .await
            .unwrap()
            .is_none());
        assert_eq!(last_event(&records), Some(stream_event::Kind::Finished));
    }
    #[tokio::test]
    async fn same_connection_same_tap_until_closed() {
        let _on = force_on_for_test();
        let (c, _s) = loopback_conn_pair().await;
        let (t1, t2) = (ConnTap::of(&c), ConnTap::of(&c));
        t1.set_session(&[3; 16]);
        assert_eq!(t1.connection(), t2.connection());
        assert_eq!(t2.session(), Some([3; 16]));
        let key = c.stable_id();
        c.close(0u32.into(), b"done");
        tokio::time::timeout(std::time::Duration::from_secs(10), async {
            while REGISTRY.lock().unwrap().contains_key(&key) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("registry entry dropped after close");
    }

    #[tokio::test]
    async fn tapped_round_trip_records_both_ends() {
        use crate::net::frame::write_msg_tapped;
        let (w_tap, sent) = collecting_stream_tap(StreamKind::Control, 4);
        let (r_tap, got) = collecting_stream_tap(StreamKind::Control, 4);
        let msg = Control {
            msg: Some(control::Msg::Need(irsync_proto::v1::NeedChunks {
                file_id: 9,
                chunk_idx: vec![1, 2],
            })),
        };
        let mut buf = Vec::new();
        write_msg_tapped(&mut buf, &msg, &w_tap).await.unwrap();
        let mut rd = buf.as_slice();
        let back = read_msg_tapped::<Control, _>(&mut rd, &r_tap)
            .await
            .unwrap();
        assert_eq!(back.as_ref(), Some(&msg));
        let (s, g) = (sent.lock().unwrap(), got.lock().unwrap());
        for r in [&s[1], &g[1]] {
            assert_eq!(r.file_id, Some(9));
            assert_eq!(r.encoded_len as usize, prost::Message::encoded_len(&msg));
            assert_eq!(r.msg, Some(record::Msg::Control(msg.clone())));
        }
        assert_eq!(s[1].direction(), Direction::Sent);
        assert_eq!(g[1].direction(), Direction::Received);
    }

    #[tokio::test]
    async fn selected_path_is_recorded() {
        let (c, _s) = loopback_conn_pair().await;
        let records = Records::default();
        let tap = Arc::new(ConnInner {
            side: Side::Client,
            connection: 7,
            remote: [0; 32],
            local: OnceLock::new(),
            session: OnceLock::new(),
            collector: Some(records.clone()),
        });
        watch_paths(&tokio::runtime::Handle::current(), &c, tap);
        let change = tokio::time::timeout(std::time::Duration::from_secs(10), async {
            loop {
                let found = records.lock().unwrap().iter().find_map(|r| match &r.msg {
                    Some(record::Msg::PathChange(p)) => Some((r.clone(), p.remote.clone())),
                    _ => None,
                });
                if let Some(f) = found {
                    break f;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("a path change is recorded");
        let (r, remote) = change;
        assert!(remote.starts_with("ip:127.0.0.1:"), "{remote}");
        assert_eq!(r.stream(), StreamKind::Unspecified);
        assert_eq!(r.connection, 7);
    }

    #[tokio::test]
    async fn tapped_read_error_records_reset() {
        let (tap, records) = collecting_stream_tap(StreamKind::Control, 0);
        let mut cut: &[u8] = &[5, 1];
        assert!(read_msg_tapped::<Control, _>(&mut cut, &tap).await.is_err());
        assert_eq!(last_event(&records), Some(stream_event::Kind::Reset));
    }

    #[tokio::test]
    async fn off_tap_still_reads_and_writes() {
        use crate::net::frame::write_msg_tapped;
        let mut buf = Vec::new();
        let hello = irsync_proto::v1::Hello {
            protocol_version: 5,
            ..Default::default()
        };
        write_msg_tapped(&mut buf, &hello, &StreamTap::off())
            .await
            .unwrap();
        let mut rd = buf.as_slice();
        let back = read_msg_tapped::<irsync_proto::v1::Hello, _>(&mut rd, &StreamTap::off()).await;
        assert_eq!(back.unwrap(), Some(hello));
    }
}
