//! Sub-chunk delta: rsync's block matching, run inside one content-defined
//! chunk against a region of the receiver's old file.
//!
//! The receiver signs a *basis* (the old file's bytes around where a missing
//! chunk sits): per fixed-length block an rsync-style rolling sum and the
//! first 8 bytes of its BLAKE3 hash. The sender slides a window over the
//! new chunk's bytes, probes the signatures at every position, and emits
//! copies (of old-file ranges) and literals instead of the chunk. The
//! receiver rebuilds the chunk and verifies it against the chunk's hash
//! like any other, so nothing here is trusted: a stale basis, a short
//! strong hash collision or a hostile operation list all end in a chunk
//! that fails verification and is asked for again, raw.

use std::collections::HashMap;
use std::io;

use irsync_proto::v1;

/// Block length bounds: the block grows with the square root of the basis
/// (rsync's rule), between these.
pub const MIN_BLOCK: usize = 512;
pub const MAX_BLOCK: usize = 4096;
/// Blocks a basis may carry: a 3 MiB region at the smallest block is 6144.
pub const MAX_BLOCKS: usize = 8192;
/// Bytes of BLAKE3 kept per block. Short on purpose: the chunk hash is the
/// real check, this only makes false weak matches cheap to reject.
const STRONG: usize = 8;

/// The block length for a basis of `basis_len` bytes.
pub fn block_len(basis_len: usize) -> usize {
    ((basis_len as f64).sqrt() as usize).clamp(MIN_BLOCK, MAX_BLOCK)
}

fn invalid(msg: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg.to_owned())
}

/// rsync's rolling checksum over a window: `a` the byte sum, `b` the sum
/// of the bytes weighted by their distance from the window's end. Both
/// wrap; the 32-bit value packs the low halves.
#[derive(Debug, Clone)]
pub struct Rolling {
    a: u32,
    b: u32,
    len: u32,
}

impl Rolling {
    pub fn new(window: &[u8]) -> Rolling {
        let n = window.len() as u32;
        let (mut a, mut b) = (0u32, 0u32);
        for (i, &x) in window.iter().enumerate() {
            a = a.wrapping_add(u32::from(x));
            b = b.wrapping_add((n - i as u32).wrapping_mul(u32::from(x)));
        }
        Rolling { a, b, len: n }
    }

    /// Slides the window one byte: `out` leaves at the front, `inp` enters
    /// at the back.
    pub fn roll(&mut self, out: u8, inp: u8) {
        self.a = self
            .a
            .wrapping_sub(u32::from(out))
            .wrapping_add(u32::from(inp));
        self.b = self
            .b
            .wrapping_sub(self.len.wrapping_mul(u32::from(out)))
            .wrapping_add(self.a);
    }

    pub fn value(&self) -> u32 {
        (self.a & 0xffff) | (self.b << 16)
    }
}

fn strong_of(block: &[u8]) -> [u8; STRONG] {
    let mut s = [0u8; STRONG];
    s.copy_from_slice(&blake3::hash(block).as_bytes()[..STRONG]);
    s
}

/// Block signatures over a region of the old file starting at `offset`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Basis {
    pub offset: u64,
    pub block: u32,
    pub weak: Vec<u32>,
    pub strong: Vec<[u8; STRONG]>,
}

impl Basis {
    /// Signs `bytes` (the old file from `offset` on) in blocks of
    /// [`block_len`]; a shorter tail is left unsigned.
    pub fn sign(offset: u64, bytes: &[u8]) -> Basis {
        let block = block_len(bytes.len());
        let n = (bytes.len() / block).min(MAX_BLOCKS);
        let mut weak = Vec::with_capacity(n);
        let mut strong = Vec::with_capacity(n);
        for b in bytes.chunks_exact(block).take(n) {
            weak.push(Rolling::new(b).value());
            strong.push(strong_of(b));
        }
        Basis {
            offset,
            block: block as u32,
            weak,
            strong,
        }
    }

    pub fn blocks(&self) -> usize {
        self.weak.len()
    }

    pub fn to_wire(&self, file_id: u64, chunk_idx: u32) -> v1::ChunkBasis {
        v1::ChunkBasis {
            file_id,
            chunk_idx,
            offset: self.offset,
            block: self.block,
            weak: self.weak.clone(),
            strong: self.strong.concat(),
        }
    }

    pub fn from_wire(w: v1::ChunkBasis) -> io::Result<Basis> {
        let block = w.block as usize;
        if !(MIN_BLOCK..=MAX_BLOCK).contains(&block) {
            return Err(invalid("basis block length out of range"));
        }
        if w.weak.len() > MAX_BLOCKS {
            return Err(invalid("basis has too many blocks"));
        }
        if w.strong.len() != STRONG * w.weak.len() {
            return Err(invalid("basis strong hashes do not match its blocks"));
        }
        let strong = w.strong.as_chunks::<STRONG>().0.to_vec();
        Ok(Basis {
            offset: w.offset,
            block: w.block,
            weak: w.weak,
            strong,
        })
    }

    /// Where block `i` sits in the old file.
    fn block_offset(&self, i: usize) -> u64 {
        self.offset + i as u64 * u64::from(self.block)
    }
}

/// The part of the old file the receiver signed for one chunk: `blocks`
/// blocks of `block` bytes from `offset` on. It is all of the old file the
/// sender may ask to have copied into that chunk, and only in whole blocks.
///
/// Without this limit a sender could find out what the old file holds. It
/// would send a chunk record that is right only if one byte of the old
/// file has the value it guessed, ask for that byte to be copied, and see
/// whether the receiver asks for the chunk again. Held to whole signed
/// blocks, a copy tells the sender nothing the signatures did not: it has
/// to know a block's content to name a record the block fits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Region {
    pub offset: u64,
    pub block: u32,
    pub blocks: u32,
}

impl Region {
    /// The region `basis` (as it went on the wire) signs.
    pub fn of(basis: &v1::ChunkBasis) -> Region {
        Region {
            offset: basis.offset,
            block: basis.block,
            blocks: basis.weak.len() as u32,
        }
    }

    /// Checks that every copy in `ops` is a run of whole signed blocks.
    pub(crate) fn admit(&self, ops: &[Op]) -> io::Result<()> {
        let block = u64::from(self.block);
        let end = self.offset + block * u64::from(self.blocks);
        for op in ops {
            let Op::Copy { offset, len } = *op else {
                continue;
            };
            let len = u64::from(len);
            let inside = offset >= self.offset && offset.checked_add(len).is_some_and(|e| e <= end);
            if block == 0
                || len == 0
                || !inside
                || !(offset - self.offset).is_multiple_of(block)
                || !len.is_multiple_of(block)
            {
                return Err(invalid("delta copy is not a run of offered blocks"));
            }
        }
        Ok(())
    }
}

/// One step of rebuilding a chunk: bytes as sent, or a range of the
/// receiver's old file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Op {
    Literal(Vec<u8>),
    Copy { offset: u64, len: u32 },
}

impl Op {
    pub fn to_wire(&self) -> v1::DeltaOp {
        use v1::delta_op::Op as W;
        v1::DeltaOp {
            op: Some(match self {
                Op::Literal(b) => W::Literal(b.clone()),
                Op::Copy { offset, len } => W::Copy(v1::CopyOld {
                    offset: *offset,
                    len: *len,
                }),
            }),
        }
    }

    pub fn from_wire(w: v1::DeltaOp) -> io::Result<Op> {
        use v1::delta_op::Op as W;
        match w.op {
            Some(W::Literal(b)) => Ok(Op::Literal(b)),
            Some(W::Copy(c)) => Ok(Op::Copy {
                offset: c.offset,
                len: c.len,
            }),
            None => Err(invalid("empty delta operation")),
        }
    }

    fn len(&self) -> usize {
        match self {
            Op::Literal(b) => b.len(),
            Op::Copy { len, .. } => *len as usize,
        }
    }
}

/// About how many bytes `ops` take on the wire.
pub fn encoded_len(ops: &[Op]) -> usize {
    ops.iter()
        .map(|o| match o {
            Op::Literal(b) => 6 + b.len(),
            Op::Copy { .. } => 16,
        })
        .sum()
}

fn push_literal(ops: &mut Vec<Op>, bytes: &[u8]) {
    match ops.last_mut() {
        Some(Op::Literal(b)) => b.extend_from_slice(bytes),
        _ => ops.push(Op::Literal(bytes.to_vec())),
    }
}

fn push_copy(ops: &mut Vec<Op>, offset: u64, len: u32) {
    match ops.last_mut() {
        Some(Op::Copy { offset: o, len: l }) if *o + u64::from(*l) == offset => *l += len,
        _ => ops.push(Op::Copy { offset, len }),
    }
}

/// The basis's signatures, indexed for the sender's scan.
struct Table<'a> {
    basis: &'a Basis,
    /// A 64 Ki-bit filter over the weak sums: most positions of the new
    /// chunk fail here without a map lookup.
    filter: Vec<u64>,
    by_weak: HashMap<u32, Vec<u32>>,
}

impl<'a> Table<'a> {
    fn new(basis: &'a Basis) -> Table<'a> {
        let mut filter = vec![0u64; 1024];
        let mut by_weak: HashMap<u32, Vec<u32>> = HashMap::with_capacity(basis.blocks());
        for (i, &w) in basis.weak.iter().enumerate() {
            let bit = Self::bit(w);
            filter[bit / 64] |= 1 << (bit % 64);
            by_weak.entry(w).or_default().push(i as u32);
        }
        Table {
            basis,
            filter,
            by_weak,
        }
    }

    fn bit(weak: u32) -> usize {
        ((weak ^ (weak >> 16)) & 0xffff) as usize
    }

    /// The block `window` is a copy of, if any.
    fn find(&self, weak: u32, window: &[u8]) -> Option<usize> {
        let bit = Self::bit(weak);
        if self.filter[bit / 64] & (1 << (bit % 64)) == 0 {
            return None;
        }
        let cands = self.by_weak.get(&weak)?;
        let strong = strong_of(window);
        cands
            .iter()
            .map(|&i| i as usize)
            .find(|&i| self.basis.strong[i] == strong)
    }
}

/// Encodes `new` (a chunk's bytes) against `basis`: copies where a whole
/// block of the basis recurs at any offset, literals between.
pub fn encode(new: &[u8], basis: &Basis) -> Vec<Op> {
    let block = basis.block as usize;
    let mut ops = Vec::new();
    if basis.blocks() == 0 || new.len() < block {
        push_literal(&mut ops, new);
        return ops;
    }
    let table = Table::new(basis);
    let (mut pos, mut lit_start) = (0usize, 0usize);
    let mut r = Rolling::new(&new[..block]);
    loop {
        if let Some(i) = table.find(r.value(), &new[pos..pos + block]) {
            if lit_start < pos {
                push_literal(&mut ops, &new[lit_start..pos]);
            }
            push_copy(&mut ops, basis.block_offset(i), block as u32);
            pos += block;
            lit_start = pos;
            if pos + block > new.len() {
                break;
            }
            r = Rolling::new(&new[pos..pos + block]);
            continue;
        }
        if pos + block >= new.len() {
            break;
        }
        r.roll(new[pos], new[pos + block]);
        pos += 1;
    }
    if lit_start < new.len() {
        push_literal(&mut ops, &new[lit_start..]);
    }
    ops
}

/// Rebuilds a chunk of `len` bytes from `ops`, reading old-file ranges
/// through `read_old(offset, buf)`. The operations must add up to exactly
/// `len` bytes (checked before anything is read).
pub fn decode(
    ops: &[Op],
    len: usize,
    read_old: &mut dyn FnMut(u64, &mut [u8]) -> io::Result<()>,
) -> io::Result<Vec<u8>> {
    let mut total = 0usize;
    for op in ops {
        if let Op::Copy { offset, len } = op {
            offset
                .checked_add(u64::from(*len))
                .ok_or_else(|| invalid("delta copy past the end of any file"))?;
        }
        total = total
            .checked_add(op.len())
            .ok_or_else(|| invalid("delta operations overflow"))?;
    }
    if total != len {
        return Err(invalid("delta operations do not add up to the chunk"));
    }
    let mut out = vec![0u8; len];
    let mut at = 0usize;
    for op in ops {
        let n = op.len();
        match op {
            Op::Literal(b) => out[at..at + n].copy_from_slice(b),
            Op::Copy { offset, .. } => read_old(*offset, &mut out[at..at + n])?,
        }
        at += n;
    }
    Ok(out)
}

#[cfg(test)]
mod region_tests {
    use super::*;

    fn bytes(n: usize, seed: u32) -> Vec<u8> {
        let mut x = seed.wrapping_mul(2654435761).wrapping_add(1);
        (0..n)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 17;
                x ^= x << 5;
                x as u8
            })
            .collect()
    }

    const REGION: Region = Region {
        offset: 8192,
        block: 1024,
        blocks: 4,
    };

    fn copy(offset: u64, len: u32) -> Op {
        Op::Copy { offset, len }
    }

    #[test]
    fn whole_offered_blocks_are_admitted() {
        for ops in [
            vec![],
            vec![Op::Literal(vec![1, 2, 3])],
            vec![copy(8192, 1024)],
            vec![copy(8192, 4096)],
            vec![copy(11264, 1024), Op::Literal(vec![0]), copy(9216, 2048)],
        ] {
            REGION
                .admit(&ops)
                .unwrap_or_else(|e| panic!("{ops:?}: {e}"));
        }
    }

    /// What a sender would need to learn the old file byte by byte, or to
    /// read a part of it the receiver did not sign.
    #[test]
    fn anything_but_whole_offered_blocks_is_refused() {
        for op in [
            copy(8192, 1),
            copy(8192, 1023),
            copy(8193, 1024),
            copy(8192 + 1023, 1),
            copy(0, 1024),
            copy(7168, 1024),
            copy(12288, 1024),
            copy(11264, 2048),
            copy(8192, 0),
            copy(u64::MAX - 5, 1024),
        ] {
            let ops = vec![Op::Literal(vec![7; 10]), op.clone()];
            assert!(REGION.admit(&ops).is_err(), "{op:?}");
        }
    }

    /// What `encode` makes against a basis always passes for its region.
    #[test]
    fn the_encoders_copies_are_admitted() {
        for (len, seed) in [(64 * 1024, 1), (300 * 1024 + 77, 2), (1024 * 1024, 3)] {
            let old = bytes(len, seed);
            let basis = Basis::sign(4096, &old);
            let wire = basis.to_wire(1, 0);
            let region = Region::of(&wire);
            // The old content moved and patched, so copies come in runs.
            let mut new = bytes(999, seed + 10);
            new.extend_from_slice(&old[..len / 2]);
            new.extend_from_slice(&bytes(5, seed + 20));
            new.extend_from_slice(&old[len / 3..]);
            let ops = encode(&new, &basis);
            assert!(ops.iter().any(|o| matches!(o, Op::Copy { .. })));
            region.admit(&ops).unwrap();
        }
    }
}
