use irsync_proto::chunk::{digest_bytes, digest_reader, CHUNK_PARAMS};
use serde::{Deserialize, Serialize};

/// Deterministic xorshift64 byte generator (Marsaglia shifts 13/7/17).
/// Each step emits the 8 little-endian bytes of the new state.
fn xorshift_bytes(seed: u64, n: usize) -> Vec<u8> {
    let mut s = seed;
    let mut out = Vec::with_capacity(n + 8);
    while out.len() < n {
        s ^= s << 13;
        s ^= s >> 7;
        s ^= s << 17;
        out.extend_from_slice(&s.to_le_bytes());
    }
    out.truncate(n);
    out
}

#[test]
fn empty_input_has_zero_chunks_and_blake3_of_empty() {
    let d = digest_bytes(&[]);
    assert_eq!(d.size, 0);
    assert!(d.chunks.is_empty());
    assert_eq!(d.hash, *blake3::hash(&[]).as_bytes());
}

#[test]
fn chunks_cover_input_exactly() {
    let data = xorshift_bytes(1, 5 << 20);
    let d = digest_bytes(&data);
    assert_eq!(d.size, data.len() as u64);
    let mut next = 0u64;
    for c in &d.chunks {
        assert_eq!(c.offset, next);
        next += u64::from(c.len);
    }
    assert_eq!(next, data.len() as u64);
}

#[test]
fn chunk_sizes_within_bounds() {
    let data = xorshift_bytes(2, 5 << 20);
    let d = digest_bytes(&data);
    assert!(d.chunks.len() > 1);
    let (last, rest) = d.chunks.split_last().unwrap();
    for c in rest {
        assert!(c.len >= CHUNK_PARAMS.min);
    }
    for c in rest.iter().chain([last]) {
        assert!(c.len <= CHUNK_PARAMS.max);
    }
}

#[test]
fn chunk_hash_is_blake3_of_slice() {
    let data = xorshift_bytes(3, 3 << 20);
    let d = digest_bytes(&data);
    for c in &d.chunks {
        let s = &data[c.offset as usize..c.offset as usize + c.len as usize];
        assert_eq!(c.hash, *blake3::hash(s).as_bytes());
    }
}

#[test]
fn whole_hash_is_blake3_of_input() {
    let data = xorshift_bytes(4, 3 << 20);
    assert_eq!(digest_bytes(&data).hash, *blake3::hash(&data).as_bytes());
}

#[test]
fn small_file_is_single_chunk() {
    let data = xorshift_bytes(5, 10 * 1024);
    let d = digest_reader(&data[..]).unwrap();
    assert_eq!(d.chunks.len(), 1);
    assert_eq!(d.chunks[0].offset, 0);
    assert_eq!(d.chunks[0].len, 10 * 1024);
    assert_eq!(d.chunks[0].hash, d.hash);
}

#[derive(Serialize, Deserialize, PartialEq, Debug)]
struct GoldenChunk {
    offset: u64,
    len: u32,
    hash: String,
}

#[derive(Serialize, Deserialize, PartialEq, Debug)]
struct Golden {
    size: u64,
    hash: String,
    chunks: Vec<GoldenChunk>,
}

fn hex(b: &[u8; 32]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// Input: 1 MiB from `xorshift_bytes(0x9E3779B97F4A7C15, 1 << 20)`.
/// The expected chunk list is pinned in tests/golden/chunk_1mib.json so other
/// implementations can reproduce the chunk boundaries exactly.
#[test]
fn golden_vector_matches() {
    let data = xorshift_bytes(0x9E37_79B9_7F4A_7C15, 1 << 20);
    let d = digest_bytes(&data);
    let actual = Golden {
        size: d.size,
        hash: hex(&d.hash),
        chunks: d
            .chunks
            .iter()
            .map(|c| GoldenChunk {
                offset: c.offset,
                len: c.len,
                hash: hex(&c.hash),
            })
            .collect(),
    };
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/golden/chunk_1mib.json");
    match std::fs::read_to_string(path) {
        Ok(s) => assert_eq!(serde_json::from_str::<Golden>(&s).unwrap(), actual),
        Err(_) => {
            std::fs::write(path, serde_json::to_string_pretty(&actual).unwrap() + "\n").unwrap();
            panic!("golden file did not exist; wrote {path}, review and commit it");
        }
    }
}
