//! Chunk lists in parts: a list too long for one control frame travels as
//! a sequence of `ChunkListPart` messages (sender), reassembled into one
//! `ChunkList` on the receiver. Pure functions; the roles drive them.

use irsync_proto::chunk::{FileDigest, CHUNK_PARAMS};
use irsync_proto::v1::{ChunkList, ChunkListPart, Hash};

/// Records per part: about 1.6 MiB on the wire, a tenth of a control frame.
pub(super) const PART_RECORDS: usize = 32 * 1024;

/// Most chunks a list may announce (`total`): bounds what a receiver holds
/// for one file (about 800 MB of records; 4 TiB of typical data).
pub(super) const MAX_LIST_CHUNKS: usize = 1 << 24;

/// Cuts a digest into parts of at most `per_part` records, in order.
pub(super) fn split(file_id: u64, d: &FileDigest, per_part: usize) -> Vec<ChunkListPart> {
    let per_part = per_part.max(1);
    let total = d.chunks.len() as u32;
    let hash = Some(Hash {
        bytes: d.hash.to_vec(),
    });
    let n = d.chunks.len().div_ceil(per_part).max(1);
    (0..n)
        .map(|k| {
            let first = k * per_part;
            let end = (first + per_part).min(d.chunks.len());
            ChunkListPart {
                file_id,
                size: d.size,
                whole_hash: hash.clone(),
                total,
                first: first as u32,
                chunks: d.chunks[first..end].iter().map(|&c| c.into()).collect(),
                last: k + 1 == n,
            }
        })
        .collect()
}

/// A chunk list under assembly from its parts.
pub(super) struct Assembly {
    cl: ChunkList,
    total: usize,
}

/// Feeds one part into the file's assembly slot. `Ok(Some)` is the complete
/// list; `Ok(None)` means more parts are due; `Err` names what was wrong
/// with the sequence (the slot is then cleared).
pub(super) fn assemble(
    slot: &mut Option<Assembly>,
    part: ChunkListPart,
) -> Result<Option<ChunkList>, String> {
    match feed(slot, part) {
        Ok(done) => Ok(done),
        Err(e) => {
            *slot = None;
            Err(e)
        }
    }
}

fn feed(slot: &mut Option<Assembly>, part: ChunkListPart) -> Result<Option<ChunkList>, String> {
    let total = part.total as usize;
    if part.first == 0 {
        // A first part starts over (a re-digested file comes as a new sequence).
        if total > MAX_LIST_CHUNKS {
            return Err(format!(
                "{total} chunks announced, above the cap of {MAX_LIST_CHUNKS}"
            ));
        }
        // Chunks are at least `min` long (a file shorter than that is one chunk).
        let possible = part.size / u64::from(CHUNK_PARAMS.min) + 1;
        if total as u64 > possible {
            return Err(format!(
                "{total} chunks announced for a size of {} bytes",
                part.size
            ));
        }
        let whole_hash = part.whole_hash.clone();
        if whole_hash.is_none() {
            return Err("no whole-file hash".into());
        }
        *slot = Some(Assembly {
            cl: ChunkList {
                file_id: part.file_id,
                size: part.size,
                whole_hash,
                chunks: Vec::with_capacity(total.min(PART_RECORDS * 4)),
            },
            total,
        });
    }
    let Some(a) = slot.as_mut() else {
        return Err(format!("part continues at {} without a start", part.first));
    };
    if part.first as usize != a.cl.chunks.len() {
        return Err(format!(
            "part continues at {} but {} records are assembled",
            part.first,
            a.cl.chunks.len()
        ));
    }
    if part.size != a.cl.size || part.whole_hash != a.cl.whole_hash || total != a.total {
        return Err("part describes another file (size, hash or total differ)".into());
    }
    if a.cl.chunks.len() + part.chunks.len() > a.total {
        return Err(format!(
            "{} records exceeds the {} announced",
            a.cl.chunks.len() + part.chunks.len(),
            a.total
        ));
    }
    a.cl.chunks.extend(part.chunks);
    if !part.last {
        return Ok(None);
    }
    if a.cl.chunks.len() != a.total {
        return Err(format!(
            "last part leaves the list short: {} of {} records",
            a.cl.chunks.len(),
            a.total
        ));
    }
    Ok(slot.take().map(|a| a.cl))
}

#[cfg(test)]
mod tests {
    use super::*;
    use irsync_proto::chunk::ChunkRecord;

    fn digest(n: usize) -> FileDigest {
        let mut chunks = Vec::with_capacity(n);
        let mut offset = 0;
        for i in 0..n {
            let len = CHUNK_PARAMS.min + (i as u32 % 7) * 1024;
            let mut hash = [0u8; 32];
            hash[..8].copy_from_slice(&(i as u64).to_le_bytes());
            chunks.push(ChunkRecord { offset, len, hash });
            offset += u64::from(len);
        }
        FileDigest {
            size: offset,
            hash: [7u8; 32],
            chunks,
        }
    }

    fn run(parts: Vec<ChunkListPart>) -> Result<Option<ChunkList>, String> {
        let mut slot = None;
        let mut out = None;
        for p in parts {
            out = assemble(&mut slot, p)?;
        }
        Ok(out)
    }

    #[test]
    fn split_cuts_contiguous_parts_with_the_last_one_flagged() {
        let d = digest(10);
        let parts = split(42, &d, 4);
        assert_eq!(parts.len(), 3);
        let mut first = 0u32;
        for (k, p) in parts.iter().enumerate() {
            assert_eq!(p.file_id, 42);
            assert_eq!(p.size, d.size);
            assert_eq!(p.whole_hash.as_ref().unwrap().bytes, d.hash.to_vec());
            assert_eq!(p.total, 10);
            assert_eq!(p.first, first);
            assert!(p.chunks.len() <= 4);
            assert_eq!(p.last, k == 2);
            first += p.chunks.len() as u32;
        }
        assert_eq!(first, 10);
        let records: Vec<ChunkRecord> = parts
            .iter()
            .flat_map(|p| {
                p.chunks
                    .iter()
                    .map(|c| ChunkRecord::try_from(c.clone()).unwrap())
            })
            .collect();
        assert_eq!(records, d.chunks);
    }

    #[test]
    fn split_of_an_empty_digest_is_one_empty_last_part() {
        let parts = split(1, &digest(0), 4);
        assert_eq!(parts.len(), 1);
        assert!(parts[0].last);
        assert!(parts[0].chunks.is_empty());
        assert_eq!(parts[0].total, 0);
    }

    #[test]
    fn assemble_rebuilds_the_list_from_its_parts() {
        let d = digest(10);
        let cl = run(split(42, &d, 4)).unwrap().expect("complete");
        assert_eq!(cl.file_id, 42);
        assert_eq!(cl.size, d.size);
        assert_eq!(cl.whole_hash.unwrap().bytes, d.hash.to_vec());
        let records: Vec<ChunkRecord> = cl
            .chunks
            .into_iter()
            .map(|c| ChunkRecord::try_from(c).unwrap())
            .collect();
        assert_eq!(records, d.chunks);
    }

    #[test]
    fn assemble_waits_for_the_last_part() {
        let mut parts = split(42, &digest(10), 4);
        parts.pop();
        assert_eq!(run(parts).unwrap(), None);
    }

    #[test]
    fn a_first_part_restarts_the_assembly() {
        let d = digest(10);
        let mut parts = split(42, &d, 4);
        let restart = parts[0].clone();
        parts.insert(1, restart);
        let cl = run(parts).unwrap().expect("complete");
        assert_eq!(cl.chunks.len(), 10);
    }

    #[test]
    fn a_part_that_skips_ahead_is_an_error() {
        let mut parts = split(42, &digest(10), 4);
        parts.remove(1);
        assert!(run(parts).unwrap_err().contains("continues at"));
    }

    #[test]
    fn a_continuation_without_a_start_is_an_error() {
        let parts = split(42, &digest(10), 4);
        assert!(run(parts[1..].to_vec())
            .unwrap_err()
            .contains("continues at"));
    }

    #[test]
    fn a_part_with_another_hash_is_an_error() {
        let mut parts = split(42, &digest(10), 4);
        parts[1].whole_hash = Some(Hash { bytes: vec![1; 32] });
        assert!(run(parts).unwrap_err().contains("hash"));
    }

    #[test]
    fn a_last_part_short_of_the_total_is_an_error() {
        let mut parts = split(42, &digest(10), 4);
        parts.truncate(2);
        parts[1].last = true;
        assert!(run(parts).unwrap_err().contains("short"));
    }

    #[test]
    fn more_records_than_announced_is_an_error() {
        let mut parts = split(42, &digest(10), 4);
        for p in &mut parts {
            p.total = 9;
        }
        assert!(run(parts).unwrap_err().contains("exceeds"));
    }

    #[test]
    fn a_total_above_the_cap_is_refused_before_anything_is_held() {
        let mut parts = split(42, &digest(2), 4);
        parts[0].total = (MAX_LIST_CHUNKS + 1) as u32;
        parts[0].size = u64::MAX;
        assert!(run(parts).unwrap_err().contains("cap"));
    }

    #[test]
    fn a_total_impossible_for_the_size_is_refused() {
        let mut parts = split(42, &digest(2), 4);
        // 2 chunks of at least 64 KiB cannot describe a 10-byte file.
        parts[0].size = 10;
        assert!(run(parts).unwrap_err().contains("size"));
    }

    #[test]
    fn a_part_without_a_hash_is_an_error() {
        let mut parts = split(42, &digest(2), 4);
        parts[0].whole_hash = None;
        assert!(run(parts).unwrap_err().contains("hash"));
    }
}
