//! The frames a peer sends: `read_body` over the input as one stream, then
//! each frame's body decoded as every message type that is read off the
//! wire. A message that decodes must encode and decode to the same value.
#![no_main]

use std::fmt::Debug;
use std::future::Future;
use std::pin::pin;
use std::task::{Context, Poll, Waker};

use irsync::delta::{Basis, Op};
use irsync::net::frame::{read_body, MAX_FRAME};
use irsync_proto::v1::{
    control, Control, DataChunk, DataStreamHeader, Error, ExportList, Hello, HelloAck,
    ListingStreamHeader, PathInfo,
};
use libfuzzer_sys::fuzz_target;
use prost::Message;

/// Runs a future that never waits: reading from a byte slice is always ready.
fn now<F: Future>(f: F) -> F::Output {
    match pin!(f).poll(&mut Context::from_waker(Waker::noop())) {
        Poll::Ready(v) => v,
        Poll::Pending => panic!("reading from a slice waited"),
    }
}

fn round_trip<M: Message + Default + PartialEq + Debug>(body: &[u8]) -> Option<M> {
    let m = M::decode(body).ok()?;
    let again = M::decode(m.encode_to_vec().as_slice()).expect("a re-encoded message decodes");
    assert_eq!(again, m);
    Some(m)
}

fn body(b: &[u8]) {
    round_trip::<Hello>(b);
    round_trip::<HelloAck>(b);
    round_trip::<Error>(b);
    round_trip::<ExportList>(b);
    round_trip::<PathInfo>(b);
    round_trip::<ListingStreamHeader>(b);
    round_trip::<DataStreamHeader>(b);
    if let Some(c) = round_trip::<DataChunk>(b) {
        // What the receiver does next with a chunk's operations.
        let _ = c.ops.into_iter().map(Op::from_wire).collect::<Result<Vec<_>, _>>();
    }
    if let Some(Control {
        msg: Some(control::Msg::ChunkBasis(basis)),
    }) = round_trip::<Control>(b)
    {
        // What the sender does next with a basis.
        let _ = Basis::from_wire(basis);
    }
}

fuzz_target!(|data: &[u8]| {
    let mut stream = data;
    while let Ok(Some(b)) = now(read_body(&mut stream)) {
        assert!(b.len() <= MAX_FRAME);
        body(&b);
    }
});
