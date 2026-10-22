//! The whole-file BLAKE3 hash of a file, computed from its verified chunks
//! as they arrive in any order, without reading the file back.
//!
//! BLAKE3 is a Merkle tree over 1 KiB leaves at absolute offsets. A
//! content-defined chunk does not start or end on a leaf boundary, but its
//! interior does: that interior is hashed as aligned power-of-two subtrees
//! (`blake3::hazmat`), and the bytes before the first and after the last
//! boundary are kept until the neighbouring chunk brings the rest of their
//! leaf. Completed sibling nodes are merged eagerly, so what is held is a
//! frontier of a few dozen chaining values, not one per chunk. The root is
//! folded at the end along the tree's own split points, and equals
//! `blake3::hash` of the bytes.

use std::collections::{BTreeMap, HashMap};

use blake3::hazmat::{
    left_subtree_len, merge_subtrees_non_root, merge_subtrees_root, ChainingValue, HasherExt, Mode,
};

/// A BLAKE3 leaf ("chunk" in BLAKE3's terms): 1 KiB.
pub const LEAF: u64 = blake3::CHUNK_LEN as u64;

/// Pieces of one leaf that several chunks straddle.
struct Pieces {
    have: u64,
    parts: Vec<(u64, Vec<u8>)>,
}

/// See the module documentation. Every byte of the file must be fed exactly
/// once, in any order and any chunking; the root is then the file's hash.
pub struct TreeHash {
    size: u64,
    /// Complete tree nodes not yet merged into a parent: offset to (length,
    /// chaining value).
    nodes: BTreeMap<u64, (u64, ChainingValue)>,
    /// Leaves straddling chunk boundaries, by leaf offset, until complete.
    pieces: HashMap<u64, Pieces>,
    /// The whole file arrived as one chunk: its hash directly.
    whole: Option<[u8; 32]>,
}

impl TreeHash {
    pub fn new(size: u64) -> TreeHash {
        TreeHash {
            size,
            nodes: BTreeMap::new(),
            pieces: HashMap::new(),
            whole: None,
        }
    }

    /// Feeds the bytes at `offset`: one verified chunk, fed once.
    pub fn add(&mut self, offset: u64, data: &[u8]) {
        let end = offset + data.len() as u64;
        debug_assert!(end <= self.size, "chunk past the end of the file");
        if offset == 0 && end == self.size {
            self.whole = Some(*blake3::hash(data).as_bytes());
            return;
        }
        if data.is_empty() {
            return;
        }
        // The first leaf boundary at or after the start, and the last at or
        // before the end (the end of the file counts as one).
        let up = offset.div_ceil(LEAF) * LEAF;
        let down = if end == self.size {
            end
        } else {
            end / LEAF * LEAF
        };
        let at = |pos: u64| (pos - offset) as usize;
        if !offset.is_multiple_of(LEAF) {
            // The tail of the leaf the chunk starts in.
            let stop = up.min(end);
            self.piece(offset / LEAF * LEAF, offset, &data[..at(stop)]);
        }
        if up < down {
            self.interior(up, &data[at(up)..at(down)]);
        }
        if !end.is_multiple_of(LEAF) && end < self.size && down >= up {
            // The head of the leaf the chunk ends in.
            self.piece(down, down, &data[at(down)..]);
        }
    }

    /// One more piece of the leaf at `leaf`; hashed once the leaf is whole.
    fn piece(&mut self, leaf: u64, start: u64, bytes: &[u8]) {
        let len = (self.size - leaf).min(LEAF);
        let p = self.pieces.entry(leaf).or_insert_with(|| Pieces {
            have: 0,
            parts: Vec::with_capacity(2),
        });
        p.have += bytes.len() as u64;
        p.parts.push((start, bytes.to_vec()));
        if p.have < len {
            return;
        }
        let mut p = self.pieces.remove(&leaf).expect("just inserted");
        p.parts.sort_by_key(|(s, _)| *s);
        let mut h = blake3::Hasher::new();
        h.set_input_offset(leaf);
        for (_, b) in &p.parts {
            h.update(b);
        }
        self.insert(leaf, len, h.finalize_non_root());
    }

    /// Hashes `[start, start + bytes.len())`, which starts on a leaf
    /// boundary and ends on one or at the end of the file, as the maximal
    /// aligned power-of-two subtrees it decomposes into.
    fn interior(&mut self, start: u64, bytes: &[u8]) {
        let mut off = start;
        let end = start + bytes.len() as u64;
        while off < end {
            let remaining = end - off;
            // The largest subtree that may start at `off` (any for 0), cut
            // down to what is left; below a leaf only at the file's end.
            let mut len = if off == 0 {
                remaining.next_power_of_two()
            } else {
                LEAF << (off / LEAF).trailing_zeros()
            };
            while len > remaining && len > LEAF {
                len /= 2;
            }
            if len > remaining {
                debug_assert_eq!(off + remaining, self.size, "short leaf inside the file");
                len = remaining;
            }
            let mut h = blake3::Hasher::new();
            h.set_input_offset(off);
            h.update(&bytes[(off - start) as usize..(off - start + len) as usize]);
            self.insert(off, len, h.finalize_non_root());
            off += len;
        }
    }

    /// Stores a complete node, merging it with its sibling into their
    /// parent while the parent lies within the file.
    fn insert(&mut self, mut off: u64, mut len: u64, mut cv: ChainingValue) {
        while len >= LEAF && len.is_power_of_two() {
            let parent = off & !(2 * len - 1);
            // Only a parent strictly inside the file: the root is merged
            // differently (`merge_subtrees_root`), in `root`.
            if parent + 2 * len > self.size || (parent == 0 && 2 * len == self.size) {
                break;
            }
            let sibling = if off == parent { off + len } else { off - len };
            match self.nodes.get(&sibling) {
                Some(&(l, scv)) if l == len => {
                    self.nodes.remove(&sibling);
                    cv = if off == parent {
                        merge_subtrees_non_root(&cv, &scv, Mode::Hash)
                    } else {
                        merge_subtrees_non_root(&scv, &cv, Mode::Hash)
                    };
                    off = parent;
                    len *= 2;
                }
                _ => break,
            }
        }
        self.nodes.insert(off, (len, cv));
    }

    /// Nodes held (a test looks at the frontier's size).
    pub fn nodes(&self) -> usize {
        self.nodes.len()
    }

    /// The file's BLAKE3 hash, once every byte has been fed.
    pub fn root(&self) -> Option<[u8; 32]> {
        if let Some(w) = self.whole {
            return Some(w);
        }
        if self.size == 0 {
            return Some(*blake3::hash(b"").as_bytes());
        }
        if self.size <= LEAF {
            return None; // Only ever fed whole.
        }
        let l = left_subtree_len(self.size);
        let left = self.cv(0, l)?;
        let right = self.cv(l, self.size - l)?;
        Some(*merge_subtrees_root(&left, &right, Mode::Hash).as_bytes())
    }

    /// The chaining value of the tree node `[off, off + len)`, from a held
    /// node or by merging its children.
    fn cv(&self, off: u64, len: u64) -> Option<ChainingValue> {
        if let Some(&(l, cv)) = self.nodes.get(&off) {
            if l == len {
                return Some(cv);
            }
        }
        if len <= LEAF {
            return None;
        }
        let l = left_subtree_len(len);
        let left = self.cv(off, l)?;
        let right = self.cv(off + l, len - l)?;
        Some(merge_subtrees_non_root(&left, &right, Mode::Hash))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use irsync_proto::chunk::{digest_bytes, ChunkRecord};

    /// Deterministic pseudo-random bytes.
    fn data(n: usize, seed: u32) -> Vec<u8> {
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

    /// A deterministic permutation of `0..n`.
    fn shuffled(n: usize, seed: u64) -> Vec<usize> {
        let mut order: Vec<usize> = (0..n).collect();
        let mut x = seed
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        for i in (1..n).rev() {
            x = x
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            let j = (x >> 33) as usize % (i + 1);
            order.swap(i, j);
        }
        order
    }

    fn feed(bytes: &[u8], chunks: &[ChunkRecord], order: &[usize]) -> TreeHash {
        let mut t = TreeHash::new(bytes.len() as u64);
        for &i in order {
            let c = chunks[i];
            t.add(
                c.offset,
                &bytes[c.offset as usize..(c.offset + u64::from(c.len)) as usize],
            );
        }
        t
    }

    /// Records of fixed `len` over `bytes` (any chunking must work).
    fn fixed(bytes: &[u8], len: usize) -> Vec<ChunkRecord> {
        bytes
            .chunks(len)
            .scan(0u64, |off, c| {
                let r = ChunkRecord {
                    offset: *off,
                    len: c.len() as u32,
                    hash: *blake3::hash(c).as_bytes(),
                };
                *off += c.len() as u64;
                Some(r)
            })
            .collect()
    }

    #[test]
    fn root_equals_blake3_for_content_defined_chunks_in_any_order() {
        for (k, &n) in [
            0usize,
            1,
            1023,
            1024,
            1025,
            2048,
            4096,
            4097,
            65_536,
            100_000,
            (1 << 20) + 1,
            2 << 20,
            4 << 20,
            3 * (1 << 20) + 12_345,
            (8 << 20) + 777,
        ]
        .iter()
        .enumerate()
        {
            let bytes = data(n, k as u32 + 1);
            let d = digest_bytes(&bytes);
            let want = *blake3::hash(&bytes).as_bytes();
            let forward: Vec<usize> = (0..d.chunks.len()).collect();
            let backward: Vec<usize> = forward.iter().rev().copied().collect();
            for order in [forward, backward, shuffled(d.chunks.len(), n as u64)] {
                let t = feed(&bytes, &d.chunks, &order);
                assert_eq!(t.root(), Some(want), "{n} bytes, order {order:?}");
            }
        }
    }

    #[test]
    fn root_equals_blake3_for_tiny_and_odd_chunkings() {
        for (n, len) in [
            (5000usize, 100usize),
            (10_000, 1500),
            (70_000, 1024),
            (3000, 7),
        ] {
            let bytes = data(n, len as u32);
            let chunks = fixed(&bytes, len);
            let want = *blake3::hash(&bytes).as_bytes();
            let order = shuffled(chunks.len(), n as u64);
            let t = feed(&bytes, &chunks, &order);
            assert_eq!(t.root(), Some(want), "{n} bytes in {len}-byte chunks");
        }
    }

    #[test]
    fn an_incomplete_file_has_no_root() {
        let bytes = data(2 << 20, 9);
        let d = digest_bytes(&bytes);
        assert!(d.chunks.len() >= 2);
        let order: Vec<usize> = (1..d.chunks.len()).collect();
        assert_eq!(feed(&bytes, &d.chunks, &order).root(), None);
    }

    #[test]
    fn feeding_in_order_keeps_a_small_frontier() {
        let bytes = data(4 << 20, 4);
        let d = digest_bytes(&bytes);
        let mut t = TreeHash::new(bytes.len() as u64);
        for c in &d.chunks {
            t.add(
                c.offset,
                &bytes[c.offset as usize..(c.offset + u64::from(c.len)) as usize],
            );
            // Eager sibling merges: at most about two nodes per tree level.
            assert!(t.nodes() <= 32, "{} nodes held", t.nodes());
        }
        assert_eq!(t.root(), Some(*blake3::hash(&bytes).as_bytes()));
    }

    #[test]
    fn empty_file_hashes_to_the_empty_hash() {
        let t = TreeHash::new(0);
        assert_eq!(t.root(), Some(*blake3::hash(b"").as_bytes()));
    }
}
