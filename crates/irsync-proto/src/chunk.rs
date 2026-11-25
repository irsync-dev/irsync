//! Content-defined chunking (FastCDC 2020, normalisation level 1) and BLAKE3
//! digests. The parameters and hash choice are protocol constants.

use std::io::{self, Read};

use fastcdc::v2020::{Normalization, StreamCDC};

use crate::v1;

/// FastCDC size parameters in bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChunkParams {
    pub min: u32,
    pub avg: u32,
    pub max: u32,
}

/// The protocol's chunking parameters.
pub const CHUNK_PARAMS: ChunkParams = ChunkParams {
    min: 65536,
    avg: 262144,
    max: 1048576,
};

/// One content-defined chunk: its position and BLAKE3 hash.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChunkRecord {
    pub offset: u64,
    pub len: u32,
    pub hash: [u8; 32],
}

/// Whole-file size and hash plus its chunk list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileDigest {
    pub size: u64,
    pub hash: [u8; 32],
    pub chunks: Vec<ChunkRecord>,
}

/// The wire form leaves the offset out: it follows from the lengths before.
impl From<ChunkRecord> for v1::ChunkRecord {
    fn from(c: ChunkRecord) -> Self {
        v1::ChunkRecord {
            len: c.len,
            hash: Some(v1::Hash {
                bytes: c.hash.to_vec(),
            }),
        }
    }
}

impl ChunkRecord {
    /// The record `c` of a chunk list, at `offset`: the end of the record
    /// before it, or where the list starts.
    pub fn from_wire(c: v1::ChunkRecord, offset: u64) -> io::Result<ChunkRecord> {
        let bad = |m: &str| io::Error::new(io::ErrorKind::InvalidData, m.to_owned());
        let hash = c
            .hash
            .ok_or_else(|| bad("chunk record missing hash"))?
            .bytes
            .try_into()
            .map_err(|_| bad("chunk hash is not 32 bytes"))?;
        Ok(ChunkRecord {
            offset,
            len: c.len,
            hash,
        })
    }

    /// Where the next chunk starts.
    pub fn end(&self) -> u64 {
        self.offset + u64::from(self.len)
    }
}

/// Chunk and hash a reader in a single pass.
pub fn digest_reader<R: Read>(r: R) -> io::Result<FileDigest> {
    let p = CHUNK_PARAMS;
    let cdc = StreamCDC::with_level(
        r,
        p.min as usize,
        p.avg as usize,
        p.max as usize,
        Normalization::Level1,
    );
    let mut whole = blake3::Hasher::new();
    let mut chunks = Vec::new();
    let mut size = 0u64;
    for item in cdc {
        let c = item.map_err(|e| match e {
            fastcdc::v2020::Error::IoError(e) => e,
            other => io::Error::other(other.to_string()),
        })?;
        whole.update(&c.data);
        size += c.data.len() as u64;
        chunks.push(ChunkRecord {
            offset: c.offset,
            len: c.data.len() as u32,
            hash: *blake3::hash(&c.data).as_bytes(),
        });
    }
    Ok(FileDigest {
        size,
        hash: *whole.finalize().as_bytes(),
        chunks,
    })
}

/// Chunk and hash an in-memory buffer.
pub fn digest_bytes(b: &[u8]) -> FileDigest {
    digest_reader(b).expect("reading from a slice cannot fail")
}
