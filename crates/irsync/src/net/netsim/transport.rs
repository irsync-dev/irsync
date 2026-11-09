//! The iroh custom transport: a real loopback UDP socket with every datagram
//! run through the link model, and on the client also through the outages.

use std::cmp::Reverse;
use std::collections::BinaryHeap;
use std::io;
use std::net::{SocketAddr, SocketAddrV4};
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::sync::{Arc, Mutex, PoisonError};
use std::task::{Context, Poll};
use std::time::Duration;

use iroh::endpoint::transports::{
    CustomEndpoint, CustomSender, CustomTransport, RecvInfo, Transmit,
};
use iroh_base::CustomAddr;
use tokio::io::ReadBuf;
use tokio::net::UdpSocket;
use tokio::sync::mpsc;
use tokio::time::Instant;

use super::link::Link;
use super::outage::{Change, Outages, State};
use super::profile::{link, outages, Dir};
use super::{custom_addr, fault_seed, socket_addr, Purpose, Side, Spec, TRANSPORT_ID};

/// Where the last bound transport's socket listens.
pub(super) static BOUND: Mutex<Option<SocketAddrV4>> = Mutex::new(None);

#[derive(Debug)]
pub struct NetsimTransport {
    pub spec: Spec,
    pub bind: SocketAddrV4,
}

/// How often the counts are logged while traffic flows.
const COUNTS_EVERY: Duration = Duration::from_secs(10);

/// A datagram waiting for its delivery time; `seq` keeps equal times in order.
type Delivery = Reverse<(Instant, u64, SocketAddr, Vec<u8>)>;

/// What the endpoint and its senders share.
struct Shared {
    socket: Arc<UdpSocket>,
    local: SocketAddrV4,
    /// The models' time origin.
    start: Instant,
    /// This side's outgoing direction.
    link: Mutex<Link>,
    /// Client only.
    outages: Option<Mutex<Outages>>,
    queue: mpsc::UnboundedSender<(Instant, SocketAddr, Vec<u8>)>,
    counts: Arc<Counts>,
    /// When the counts were last logged, in milliseconds since `start`.
    counts_logged_ms: AtomicU64,
}

/// What became of the datagrams, apart from the link's own drops.
#[derive(Debug, Default)]
struct Counts {
    /// Handed to the transport to send.
    sent: AtomicU64,
    /// Sent or received while an outage was open (client only).
    outage_drops: AtomicU64,
    /// Refused by the socket.
    send_errors: AtomicU64,
    /// Received but larger than iroh's buffer, and dropped.
    truncated: AtomicU64,
}

/// The counts at one moment, the link's drops included.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Totals {
    sent: u64,
    queue_drops: u64,
    loss_drops: u64,
    outage_drops: u64,
    send_errors: u64,
    truncated: u64,
}

impl std::fmt::Debug for Shared {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Shared")
            .field("local", &self.local)
            .finish()
    }
}

impl Shared {
    /// Counts a packet of `len` bytes against the outages; true when the link is up.
    fn link_up(&self, len: usize) -> bool {
        let Some(outages) = &self.outages else {
            return true;
        };
        let mut o = outages.lock().expect("outages poisoned");
        let (state, change) = o.packet(self.start.elapsed(), len);
        match change {
            Some(Change::DeadZoneOpened { secs }) => tracing::warn!(secs, "netsim: dead zone open"),
            Some(Change::DeadZoneClosed) => tracing::warn!("netsim: dead zone closed"),
            None => {}
        }
        state == State::Up
    }

    /// Runs one transmit's datagrams through the outages and the link and
    /// queues the survivors for delivery to `dst`.
    fn send(&self, dst: SocketAddrV4, contents: &[u8], segment_size: Option<usize>) {
        for packet in segments(contents, segment_size) {
            self.counts.sent.fetch_add(1, Relaxed);
            if !self.link_up(packet.len()) {
                self.counts.outage_drops.fetch_add(1, Relaxed);
                continue;
            }
            let fates = {
                let mut link = self.link.lock().expect("link poisoned");
                link.offer(self.start.elapsed(), packet.len())
            };
            for at in fates {
                // The delivery task is gone only once the endpoint is.
                let _ = self
                    .queue
                    .send((self.start + at, dst.into(), packet.to_vec()));
            }
        }
        self.log_counts_if_due();
    }

    fn totals(&self) -> Totals {
        let drops = self
            .link
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .drops();
        let c = &self.counts;
        Totals {
            sent: c.sent.load(Relaxed),
            queue_drops: drops.queue,
            loss_drops: drops.loss,
            outage_drops: c.outage_drops.load(Relaxed),
            send_errors: c.send_errors.load(Relaxed),
            truncated: c.truncated.load(Relaxed),
        }
    }

    /// Logs the counts at `warn`, so any log level shows them.
    fn log_counts(&self) {
        let t = self.totals();
        tracing::warn!(
            sent = t.sent,
            queue_drops = t.queue_drops,
            loss_drops = t.loss_drops,
            outage_drops = t.outage_drops,
            send_errors = t.send_errors,
            truncated = t.truncated,
            "netsim: counts"
        );
    }

    /// Logs the counts when `COUNTS_EVERY` has passed since they last were.
    fn log_counts_if_due(&self) {
        let now = self.start.elapsed().as_millis() as u64;
        let last = self.counts_logged_ms.load(Relaxed);
        if now >= last + COUNTS_EVERY.as_millis() as u64
            && self
                .counts_logged_ms
                .compare_exchange(last, now, Relaxed, Relaxed)
                .is_ok()
        {
            self.log_counts();
        }
    }
}

impl Drop for Shared {
    fn drop(&mut self) {
        self.log_counts();
    }
}

impl CustomTransport for NetsimTransport {
    fn bind(&self) -> io::Result<Box<dyn CustomEndpoint>> {
        Ok(Box::new(self.open()?))
    }
}

impl NetsimTransport {
    fn open(&self) -> io::Result<NetsimEndpoint> {
        let std_socket = std::net::UdpSocket::bind(self.bind)?;
        std_socket.set_nonblocking(true)?;
        let socket = Arc::new(UdpSocket::from_std(std_socket)?);
        let SocketAddr::V4(local) = socket.local_addr()? else {
            return Err(io::Error::other("netsim socket is not IPv4"));
        };
        *BOUND.lock().expect("bound poisoned") = Some(local);

        let Spec {
            profile,
            side,
            seed,
            stream,
            acks: _,
        } = self.spec;
        let dir = match side {
            Side::Client => Dir::Up,
            Side::Daemon => Dir::Down,
        };
        let outages = match side {
            Side::Client => outages(profile).map(|p| {
                Mutex::new(Outages::new(
                    p,
                    fault_seed(seed, side, stream, Purpose::Outages),
                ))
            }),
            Side::Daemon => None,
        };
        let (queue, rx) = mpsc::unbounded_channel();
        let counts = Arc::new(Counts::default());
        tokio::spawn(deliver(socket.clone(), rx, counts.clone()));
        let shared = Arc::new(Shared {
            socket,
            local,
            start: Instant::now(),
            link: Mutex::new(Link::new(
                link(profile, dir),
                fault_seed(seed, side, stream, Purpose::Link),
            )),
            outages,
            queue,
            counts,
            counts_logged_ms: AtomicU64::new(0),
        });
        Ok(NetsimEndpoint {
            local_addrs: n0_watcher::Watchable::new(vec![custom_addr(local)]),
            shared,
            scratch: Vec::new(),
        })
    }
}

/// Sends each queued datagram at its delivery time; ends when every sender is gone.
async fn deliver(
    socket: Arc<UdpSocket>,
    mut rx: mpsc::UnboundedReceiver<(Instant, SocketAddr, Vec<u8>)>,
    counts: Arc<Counts>,
) {
    let mut heap: BinaryHeap<Delivery> = BinaryHeap::new();
    let mut seq = 0u64;
    loop {
        let next = heap.peek().map(|Reverse((at, ..))| *at);
        tokio::select! {
            msg = rx.recv() => match msg {
                Some((at, dst, bytes)) => {
                    heap.push(Reverse((at, seq, dst, bytes)));
                    seq += 1;
                }
                None => return,
            },
            _ = tokio::time::sleep_until(next.unwrap_or_else(|| Instant::now() + Duration::from_secs(3600))),
                if next.is_some() => {
                let now = Instant::now();
                while let Some(Reverse((at, ..))) = heap.peek() {
                    if *at > now {
                        break;
                    }
                    let Reverse((_, _, dst, bytes)) = heap.pop().expect("peeked");
                    if let Err(e) = socket.send_to(&bytes, dst).await {
                        if counts.send_errors.fetch_add(1, Relaxed) == 0 {
                            tracing::warn!(%dst, "netsim: send failed: {e}");
                        } else {
                            tracing::debug!(%dst, "netsim: send failed: {e}");
                        }
                    }
                }
            }
        }
    }
}

#[derive(Debug)]
struct NetsimEndpoint {
    local_addrs: n0_watcher::Watchable<Vec<CustomAddr>>,
    shared: Arc<Shared>,
    /// Receives one byte more than iroh's buffer holds, to see truncation.
    scratch: Vec<u8>,
}

impl CustomEndpoint for NetsimEndpoint {
    fn watch_local_addrs(&self) -> n0_watcher::Direct<Vec<CustomAddr>> {
        self.local_addrs.watch()
    }

    fn create_sender(&self) -> Arc<dyn CustomSender> {
        Arc::new(NetsimSender(self.shared.clone()))
    }

    fn poll_recv(
        &mut self,
        cx: &mut Context,
        bufs: &mut [io::IoSliceMut<'_>],
        metas: &mut [noq_udp::RecvMeta],
        recv_infos: &mut [RecvInfo],
    ) -> Poll<io::Result<usize>> {
        let local = custom_addr(self.shared.local);
        let mut filled = 0;
        while filled < bufs.len() {
            let slot = bufs[filled].len();
            self.scratch.resize(slot + 1, 0);
            let mut buf = ReadBuf::new(&mut self.scratch);
            let from = match self.shared.socket.poll_recv_from(cx, &mut buf) {
                Poll::Pending => break,
                Poll::Ready(Err(e)) if filled == 0 => return Poll::Ready(Err(e)),
                Poll::Ready(Err(_)) => break,
                Poll::Ready(Ok(from)) => from,
            };
            let len = buf.filled().len();
            if len > slot {
                self.shared.counts.truncated.fetch_add(1, Relaxed);
                continue;
            }
            let SocketAddr::V4(from) = from else {
                continue;
            };
            if !self.shared.link_up(len) {
                self.shared.counts.outage_drops.fetch_add(1, Relaxed);
                continue;
            }
            bufs[filled][..len].copy_from_slice(&self.scratch[..len]);
            metas[filled].len = len;
            metas[filled].stride = len;
            recv_infos[filled] = RecvInfo::new(custom_addr(from), Some(local.clone()));
            filled += 1;
        }
        if filled == 0 {
            Poll::Pending
        } else {
            self.shared.log_counts_if_due();
            Poll::Ready(Ok(filled))
        }
    }
}

#[derive(Debug)]
struct NetsimSender(Arc<Shared>);

impl CustomSender for NetsimSender {
    fn is_valid_send_addr(&self, addr: &CustomAddr) -> bool {
        addr.id() == TRANSPORT_ID
    }

    fn poll_send(
        &self,
        _cx: &mut Context,
        dst: &CustomAddr,
        _src: Option<&CustomAddr>,
        transmit: &Transmit<'_>,
    ) -> Poll<io::Result<()>> {
        let Some(dst) = socket_addr(dst) else {
            return Poll::Ready(Err(io::Error::other("not a netsim address")));
        };
        self.0.send(dst, transmit.contents, transmit.segment_size);
        Poll::Ready(Ok(()))
    }
}

/// The datagrams in a transmit: one per segment, or the whole of it.
fn segments(contents: &[u8], segment_size: Option<usize>) -> impl Iterator<Item = &[u8]> {
    contents.chunks(segment_size.unwrap_or(contents.len()).max(1))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net::netsim::ProfileName;

    /// A test-blackout transport on a free loopback port.
    fn open(side: Side) -> NetsimEndpoint {
        NetsimTransport {
            spec: Spec {
                profile: ProfileName::TestBlackout,
                side,
                seed: 1,
                stream: 1,
                acks: None,
            },
            bind: "127.0.0.1:0".parse().unwrap(),
        }
        .open()
        .unwrap()
    }

    #[tokio::test]
    async fn an_oversized_datagram_is_counted_and_dropped() {
        let mut ep = open(Side::Daemon);
        let peer = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        peer.send_to(&[7u8; 100], ep.shared.local).unwrap();
        peer.send_to(&[8u8; 40], ep.shared.local).unwrap();
        let mut slot = [0u8; 50];
        let (n, len) = std::future::poll_fn(|cx| {
            let mut bufs = [io::IoSliceMut::new(&mut slot)];
            let mut metas = [noq_udp::RecvMeta::default()];
            let mut infos = [RecvInfo::default()];
            ep.poll_recv(cx, &mut bufs, &mut metas, &mut infos)
                .map_ok(|n| (n, metas[0].len))
        })
        .await
        .unwrap();
        assert_eq!((n, len), (1, 40));
        assert!(slot[..40].iter().all(|b| *b == 8));
        let t = ep.shared.totals();
        assert_eq!((t.truncated, t.outage_drops), (1, 0));
    }

    #[tokio::test]
    async fn sends_and_outage_drops_are_counted() {
        // the dead zone opens with the datagram that brings the traffic to 256 KiB
        let ep = open(Side::Client);
        let dst = ep.shared.local;
        for _ in 0..400 {
            ep.shared.send(dst, &[0u8; 1000], None);
        }
        let t = ep.shared.totals();
        assert_eq!((t.sent, t.outage_drops), (400, 400 - 262));
        assert_eq!((t.queue_drops, t.loss_drops), (0, 0));
    }

    #[tokio::test]
    async fn a_failed_send_is_counted() {
        // no SO_BROADCAST: the kernel refuses it
        let ep = open(Side::Daemon);
        ep.shared
            .send("255.255.255.255:9".parse().unwrap(), &[1u8; 10], None);
        for _ in 0..200 {
            if ep.shared.totals().send_errors > 0 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let t = ep.shared.totals();
        assert_eq!((t.sent, t.send_errors), (1, 1));
    }

    #[test]
    fn segmented_transmit_is_split() {
        let contents = [1u8; 3000];
        assert_eq!(
            segments(&contents, Some(1200))
                .map(<[u8]>::len)
                .collect::<Vec<_>>(),
            vec![1200, 1200, 600]
        );
        assert_eq!(segments(&contents, None).count(), 1);
    }
}
