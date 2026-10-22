//! The sub-chunk codec: rsync-style block signatures over a basis region of
//! the receiver's old file, a rolling match of the new chunk's bytes
//! against them on the sender, and a literal/copy operation list the
//! receiver rebuilds the chunk from.

use std::io;

use irsync::delta::{
    block_len, decode, encode, encoded_len, Basis, Op, Rolling, MAX_BLOCK, MIN_BLOCK,
};

const KIB: usize = 1024;

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

/// Rebuilds from `ops` with `old` as the old file.
fn rebuild(ops: &[Op], len: usize, old: &[u8]) -> io::Result<Vec<u8>> {
    decode(ops, len, &mut |off: u64, buf: &mut [u8]| {
        let start = usize::try_from(off).map_err(|_| io::Error::other("offset"))?;
        let src = old
            .get(start..start + buf.len())
            .ok_or_else(|| io::Error::new(io::ErrorKind::UnexpectedEof, "past the old file"))?;
        buf.copy_from_slice(src);
        Ok(())
    })
}

fn literal_bytes(ops: &[Op]) -> usize {
    ops.iter()
        .map(|o| match o {
            Op::Literal(b) => b.len(),
            Op::Copy { .. } => 0,
        })
        .sum()
}

#[test]
fn rolling_checksum_matches_a_fresh_computation_at_every_position() {
    for block in [MIN_BLOCK, 700, 1024, 3000, MAX_BLOCK] {
        let bytes = data(4 * block + 17, block as u32);
        let mut r = Rolling::new(&bytes[..block]);
        for start in 1..=bytes.len() - block {
            r.roll(bytes[start - 1], bytes[start + block - 1]);
            assert_eq!(
                r.value(),
                Rolling::new(&bytes[start..start + block]).value(),
                "block {block}, position {start}"
            );
        }
    }
}

#[test]
fn block_length_grows_with_the_basis_within_bounds() {
    assert_eq!(block_len(0), MIN_BLOCK);
    assert_eq!(block_len(64 * KIB), MIN_BLOCK);
    assert_eq!(block_len(256 * KIB), 512);
    assert_eq!(block_len(768 * KIB), 886);
    assert_eq!(block_len(3 * 1024 * KIB), 1773);
    assert_eq!(block_len(64 * 1024 * KIB), MAX_BLOCK);
    assert!(block_len(usize::MAX) <= MAX_BLOCK);
}

#[test]
fn an_unchanged_chunk_is_one_copy() {
    let old = data(256 * KIB, 1);
    let basis = Basis::sign(0, &old);
    let ops = encode(&old, &basis);
    assert!(
        matches!(ops.as_slice(), [Op::Copy { offset: 0, len }] if *len as usize >= old.len() - basis.block as usize),
        "{ops:?}"
    );
    assert_eq!(rebuild(&ops, old.len(), &old).unwrap(), old);
}

#[test]
fn one_changed_byte_costs_about_one_block_of_literal() {
    let old = data(256 * KIB, 2);
    let mut new = old.clone();
    new[100_000] ^= 0xff;
    let basis = Basis::sign(0, &old);
    let ops = encode(&new, &basis);
    assert_eq!(rebuild(&ops, new.len(), &old).unwrap(), new);
    let block = basis.block as usize;
    assert!(
        literal_bytes(&ops) <= 2 * block,
        "{} literal bytes for a {block}-byte block",
        literal_bytes(&ops)
    );
    assert!(encoded_len(&ops) < new.len() / 16, "{}", encoded_len(&ops));
}

#[test]
fn an_insertion_shifts_the_rest_but_still_matches() {
    let old = data(200 * KIB, 3);
    let mut new = old[..50_000].to_vec();
    new.extend_from_slice(b"inserted text that moves everything after it");
    new.extend_from_slice(&old[50_000..]);
    let basis = Basis::sign(0, &old);
    let ops = encode(&new, &basis);
    assert_eq!(rebuild(&ops, new.len(), &old).unwrap(), new);
    assert!(
        literal_bytes(&ops) <= 2 * basis.block as usize + 64,
        "{} literal bytes",
        literal_bytes(&ops)
    );
}

#[test]
fn a_deletion_still_matches_on_both_sides_of_it() {
    let old = data(200 * KIB, 4);
    let mut new = old[..60_000].to_vec();
    new.extend_from_slice(&old[70_000..]);
    let basis = Basis::sign(0, &old);
    let ops = encode(&new, &basis);
    assert_eq!(rebuild(&ops, new.len(), &old).unwrap(), new);
    assert!(
        literal_bytes(&ops) <= 2 * basis.block as usize,
        "{} literal bytes",
        literal_bytes(&ops)
    );
}

#[test]
fn copies_carry_the_basis_offset_in_the_old_file() {
    let old = data(300 * KIB, 5);
    let region = 100 * KIB..250 * KIB;
    let basis = Basis::sign(region.start as u64, &old[region.clone()]);
    let new = old[150 * KIB..200 * KIB].to_vec();
    let ops = encode(&new, &basis);
    assert_eq!(rebuild(&ops, new.len(), &old).unwrap(), new);
    for op in &ops {
        if let Op::Copy { offset, len } = op {
            let (s, e) = (*offset as usize, *offset as usize + *len as usize);
            assert!(
                s >= region.start && e <= region.end,
                "copy {s}..{e} outside the basis"
            );
        }
    }
}

#[test]
fn an_unrelated_basis_yields_a_literal_as_large_as_the_chunk() {
    let old = data(128 * KIB, 6);
    let new = data(128 * KIB, 7);
    let ops = encode(&new, &Basis::sign(0, &old));
    assert_eq!(literal_bytes(&ops), new.len());
    assert!(encoded_len(&ops) >= new.len());
    assert_eq!(rebuild(&ops, new.len(), &old).unwrap(), new);
}

#[test]
fn a_chunk_shorter_than_a_block_is_all_literal() {
    let old = data(100, 8);
    let basis = Basis::sign(0, &old);
    assert_eq!(basis.blocks(), 0);
    let ops = encode(&old, &basis);
    assert_eq!(literal_bytes(&ops), old.len());
}

#[test]
fn adjacent_operations_are_coalesced() {
    let old = data(64 * KIB, 9);
    let mut new = old.clone();
    new[10_000] ^= 1;
    new[40_000] ^= 1;
    let ops = encode(&new, &Basis::sign(0, &old));
    // copy, literal, copy, literal, copy: never two of a kind in a row.
    for w in ops.windows(2) {
        assert!(
            matches!(
                w,
                [Op::Literal(_), Op::Copy { .. }] | [Op::Copy { .. }, Op::Literal(_)]
            ),
            "{ops:?}"
        );
    }
    assert_eq!(rebuild(&ops, new.len(), &old).unwrap(), new);
}

#[test]
fn decode_refuses_operations_that_do_not_add_up_to_the_chunk() {
    let old = data(8 * KIB, 10);
    let short = [Op::Copy {
        offset: 0,
        len: 100,
    }];
    assert_eq!(
        rebuild(&short, 200, &old).unwrap_err().kind(),
        io::ErrorKind::InvalidData
    );
    let long = [Op::Literal(vec![1; 300])];
    assert_eq!(
        rebuild(&long, 200, &old).unwrap_err().kind(),
        io::ErrorKind::InvalidData
    );
    let overflow = [Op::Copy {
        offset: u64::MAX - 10,
        len: 100,
    }];
    assert!(rebuild(&overflow, 100, &old).is_err());
}

#[test]
fn decode_reports_a_copy_the_old_file_cannot_supply() {
    let old = data(8 * KIB, 11);
    let ops = [Op::Copy {
        offset: 8 * KIB as u64 - 10,
        len: 100,
    }];
    assert_eq!(
        rebuild(&ops, 100, &old).unwrap_err().kind(),
        io::ErrorKind::UnexpectedEof
    );
}

#[test]
fn a_basis_round_trips_through_its_wire_form() {
    let old = data(300 * KIB, 12);
    let basis = Basis::sign(64 * KIB as u64, &old);
    let wire = basis.to_wire(5, 7);
    assert_eq!((wire.file_id, wire.chunk_idx), (5, 7));
    assert_eq!(wire.strong.len(), 8 * basis.blocks());
    let back = Basis::from_wire(wire).unwrap();
    assert_eq!(back, basis);
    // Strong hashes must come in whole 8-byte units, one per weak sum.
    let mut bad = basis.to_wire(5, 7);
    bad.strong.pop();
    assert_eq!(
        Basis::from_wire(bad).unwrap_err().kind(),
        io::ErrorKind::InvalidData
    );
    let mut tiny = basis.to_wire(5, 7);
    tiny.block = 1;
    assert_eq!(
        Basis::from_wire(tiny).unwrap_err().kind(),
        io::ErrorKind::InvalidData
    );
}

#[test]
fn operations_round_trip_through_their_wire_form() {
    let ops = vec![
        Op::Literal(vec![1, 2, 3]),
        Op::Copy {
            offset: 1 << 40,
            len: 4096,
        },
        Op::Literal(vec![]),
    ];
    let wire: Vec<irsync_proto::v1::DeltaOp> = ops.iter().map(Op::to_wire).collect();
    let back: Vec<Op> = wire
        .into_iter()
        .map(|w| Op::from_wire(w).unwrap())
        .collect();
    assert_eq!(back, ops);
    assert!(Op::from_wire(irsync_proto::v1::DeltaOp::default()).is_err());
}
