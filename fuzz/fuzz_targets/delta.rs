//! Rebuilding a chunk from a peer's delta (`delta::decode`), and the
//! sender's side of it (`delta::encode`).
//!
//! Input: a 2-byte little-endian length, that many bytes of the old file,
//! then a `DataChunk` as it comes off the wire. Its operations are rebuilt
//! against the old file. Then, with the old file as the basis, both the
//! rebuilt chunk and the rest of the input must survive
//! `decode(encode(new, basis), basis) == new`.
#![no_main]

use std::io;

use irsync::delta::{decode, encode, Basis, Op};
use irsync_proto::chunk::CHUNK_PARAMS;
use irsync_proto::v1::DataChunk;
use libfuzzer_sys::fuzz_target;
use prost::Message;

/// The largest chunk: the receiver never rebuilds a longer one.
const MAX_CHUNK: u64 = CHUNK_PARAMS.max as u64;

/// Reads `buf.len()` bytes of `old` at `offset`, like `read_exact_at`.
fn read(old: &[u8], offset: u64, buf: &mut [u8]) -> io::Result<()> {
    let start = usize::try_from(offset).map_err(|_| io::ErrorKind::UnexpectedEof)?;
    let src = start
        .checked_add(buf.len())
        .and_then(|end| old.get(start..end))
        .ok_or(io::ErrorKind::UnexpectedEof)?;
    buf.copy_from_slice(src);
    Ok(())
}

/// `decode(encode(new, basis), basis) == new`, the basis signed over `old`.
fn round_trip(new: &[u8], old: &[u8]) {
    let basis = Basis::sign(0, old);
    assert_eq!(Basis::from_wire(basis.to_wire(0, 0)).unwrap(), basis);
    let ops = encode(new, &basis);
    for op in &ops {
        assert_eq!(&Op::from_wire(op.to_wire()).unwrap(), op);
    }
    let back = decode(&ops, new.len(), &mut |off, buf| read(old, off, buf))
        .expect("an encoded chunk decodes against its basis");
    assert_eq!(back, new);
}

fuzz_target!(|data: &[u8]| {
    let Some((len, rest)) = data.split_first_chunk::<2>() else {
        return;
    };
    let (old, wire) = rest.split_at(usize::from(u16::from_le_bytes(*len)).min(rest.len()));
    round_trip(wire, old);

    let Ok(chunk) = DataChunk::decode(wire) else {
        return;
    };
    let Ok(ops) = chunk
        .ops
        .into_iter()
        .map(Op::from_wire)
        .collect::<io::Result<Vec<Op>>>()
    else {
        return;
    };
    let total = ops.iter().try_fold(0u64, |t, op| {
        t.checked_add(match op {
            Op::Literal(b) => b.len() as u64,
            Op::Copy { len, .. } => u64::from(*len),
        })
    });
    // A chunk is at most MAX_CHUNK long; a longer total is refused.
    let want = total.unwrap_or(u64::MAX).min(MAX_CHUNK) as usize;
    let res = decode(&ops, want, &mut |off, buf| read(old, off, buf));
    if total != Some(want as u64) {
        assert!(res.is_err(), "operations that do not add up were accepted");
        return;
    }
    if let Ok(new) = res {
        assert_eq!(new.len(), want);
        round_trip(&new, old);
    }
});
