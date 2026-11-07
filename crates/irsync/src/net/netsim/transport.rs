//! The iroh custom transport: a real loopback UDP socket with every datagram
//! run through the link model, and on the client also through the outages.

use std::cmp::Reverse;
use std::collections::BinaryHeap;
use std::io;
use std::net::{SocketAddr, SocketAddrV4};
use std::sync::{Arc, Mutex};
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
}

impl CustomTransport for NetsimTransport {
    fn bind(&self) -> io::Result<Box<dyn CustomEndpoint>> {
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
        tokio::spawn(deliver(socket.clone(), rx));
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
        });
        Ok(Box::new(NetsimEndpoint {
            local_addrs: n0_watcher::Watchable::new(vec![custom_addr(local)]),
            shared,
        }))
    }
}

/// Sends each queued datagram at its delivery time; ends when every sender is gone.
async fn deliver(
    socket: Arc<UdpSocket>,
    mut rx: mpsc::UnboundedReceiver<(Instant, SocketAddr, Vec<u8>)>,
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
                        tracing::debug!(%dst, "netsim send: {e}");
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
            let mut buf = ReadBuf::new(&mut bufs[filled]);
            let from = match self.shared.socket.poll_recv_from(cx, &mut buf) {
                Poll::Pending => break,
                Poll::Ready(Err(e)) if filled == 0 => return Poll::Ready(Err(e)),
                Poll::Ready(Err(_)) => break,
                Poll::Ready(Ok(from)) => from,
            };
            let len = buf.filled().len();
            let SocketAddr::V4(from) = from else {
                continue;
            };
            if !self.shared.link_up(len) {
                continue;
            }
            metas[filled].len = len;
            metas[filled].stride = len;
            recv_infos[filled] = RecvInfo::new(custom_addr(from), Some(local.clone()));
            filled += 1;
        }
        if filled == 0 {
            Poll::Pending
        } else {
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
        let s = &self.0;
        for packet in segments(transmit.contents, transmit.segment_size) {
            if !s.link_up(packet.len()) {
                continue;
            }
            let fates = {
                let mut link = s.link.lock().expect("link poisoned");
                link.offer(s.start.elapsed(), packet.len())
            };
            for at in fates {
                // The delivery task is gone only once the endpoint is.
                let _ = s.queue.send((s.start + at, dst.into(), packet.to_vec()));
            }
        }
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
