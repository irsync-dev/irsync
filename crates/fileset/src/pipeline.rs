//! Ordered hashing pipeline: walk on one thread, hash on a pool, deliver in walk order.

use std::collections::BTreeMap;

use crossbeam_channel::{bounded, select, unbounded, Receiver, Sender};

use crate::hash::hash_file;
use crate::model::Flags;
use crate::walk::{Observed, WalkEvent};

pub enum Decision {
    Hash,
    Skip,
}

pub const WALK_CAPACITY: usize = 1024;
pub const JOB_CAPACITY: usize = 1024;
/// Stack of the walk (producer) thread.
const PRODUCER_STACK: usize = 256 * 1024 * 1024;

type Job = (u64, Observed);

/// Runs `producer` on its own thread, applies `decide` on the calling thread in walk order,
/// hashes `Decision::Hash` entries on `jobs` threads, and calls `sink` in walk order.
pub fn run<P, D, S>(jobs: usize, producer: P, decide: D, sink: S) -> anyhow::Result<()>
where
    P: FnOnce(&mut dyn FnMut(WalkEvent) -> anyhow::Result<()>) -> anyhow::Result<()>
        + Send
        + 'static,
    D: FnMut(&mut Observed) -> Decision,
    S: FnMut(WalkEvent) -> anyhow::Result<()>,
{
    let jobs = jobs.max(1);
    let (walk_tx, walk_rx) = bounded::<WalkEvent>(WALK_CAPACITY);
    let (job_tx, job_rx) = bounded::<Job>(JOB_CAPACITY);
    let (done_tx, done_rx) = unbounded::<Job>();

    std::thread::scope(|s| {
        // The walk recurses once per directory level, so its stack is sized for very deep
        // trees; this reserves address space, memory is committed only as it is used.
        let producer = std::thread::Builder::new()
            .name("fileset-walk".into())
            .stack_size(PRODUCER_STACK)
            .spawn_scoped(s, move || {
                let mut emit = |ev: WalkEvent| {
                    walk_tx
                        .send(ev)
                        .map_err(|_| anyhow::anyhow!("pipeline consumer stopped"))
                };
                producer(&mut emit)
            })
            .map_err(|e| anyhow::anyhow!("cannot start the walk thread: {e}"))?;
        for _ in 0..jobs {
            let job_rx = job_rx.clone();
            let done_tx = done_tx.clone();
            s.spawn(move || hasher(job_rx, done_tx));
        }
        drop(job_rx);
        drop(done_tx);

        // Consumes all channel ends it holds; returning drops them, which stops
        // the producer (send fails) and the hashers (job channel closes or
        // result send fails).
        let consumed = consume(jobs, walk_rx, job_tx, done_rx, decide, sink);
        let produced = match producer.join() {
            Ok(r) => r,
            Err(panic) => std::panic::resume_unwind(panic),
        };
        consumed.and(produced)
    })
}

fn hasher(job_rx: Receiver<Job>, done_tx: Sender<Job>) {
    for (seq, mut obs) in job_rx {
        match hash_file(&obs.abs) {
            Ok(h) => obs.entry.blake3 = Some(h),
            Err(e) => {
                obs.entry.blake3 = None;
                obs.entry.flags.insert(Flags::UNREADABLE);
                obs.hash_error = Some(e.to_string());
            }
        }
        if done_tx.send((seq, obs)).is_err() {
            return;
        }
    }
}

fn consume<D, S>(
    jobs: usize,
    walk_rx: Receiver<WalkEvent>,
    job_tx: Sender<Job>,
    done_rx: Receiver<Job>,
    mut decide: D,
    mut sink: S,
) -> anyhow::Result<()>
where
    D: FnMut(&mut Observed) -> Decision,
    S: FnMut(WalkEvent) -> anyhow::Result<()>,
{
    // Events taken from the walk but not yet delivered are capped, so the
    // reorder buffer cannot grow without bound behind one slow hash.
    let window = (JOB_CAPACITY + jobs) as u64;
    let mut ready: BTreeMap<u64, WalkEvent> = BTreeMap::new();
    let mut next_seq = 0u64;
    let mut next_expected = 0u64;
    let mut in_flight = 0usize;
    let mut walk_open = true;

    loop {
        while let Some(entry) = ready.first_entry() {
            if *entry.key() != next_expected {
                break;
            }
            sink(entry.remove())?;
            next_expected += 1;
        }
        if !walk_open && in_flight == 0 {
            debug_assert!(ready.is_empty());
            return Ok(());
        }

        let done = if walk_open && next_seq - next_expected < window {
            select! {
                recv(walk_rx) -> msg => match msg {
                    Ok(ev) => {
                        let seq = next_seq;
                        next_seq += 1;
                        match ev {
                            WalkEvent::Entry(mut obs) => match decide(&mut obs) {
                                Decision::Hash => {
                                    job_tx
                                        .send((seq, obs))
                                        .map_err(|_| anyhow::anyhow!("hasher threads stopped"))?;
                                    in_flight += 1;
                                }
                                Decision::Skip => {
                                    ready.insert(seq, WalkEvent::Entry(obs));
                                }
                            },
                            other => {
                                ready.insert(seq, other);
                            }
                        }
                        continue;
                    }
                    Err(_) => {
                        walk_open = false;
                        continue;
                    }
                },
                recv(done_rx) -> msg => msg,
            }
        } else {
            done_rx.recv()
        };
        let (seq, obs) = done.map_err(|_| anyhow::anyhow!("hasher threads stopped"))?;
        in_flight -= 1;
        ready.insert(seq, WalkEvent::Entry(obs));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Entry, Flags};
    use crate::walk::{Observed, WalkEvent};
    use std::os::unix::fs::PermissionsExt;
    use std::path::Path;

    fn observed(abs: &Path, rel: &str) -> Observed {
        Observed {
            path: rel.as_bytes().to_vec(),
            abs: abs.to_path_buf(),
            entry: Entry::from_path(abs).unwrap(),
            hash_error: None,
        }
    }

    #[test]
    fn pipeline_preserves_walk_order_with_parallel_hashing() {
        let dir = tempfile::tempdir().unwrap();
        let mut files = Vec::new();
        for i in 0..200usize {
            let p = dir.path().join(format!("f{i:03}"));
            // Varying sizes so hash completion order differs from submission order.
            let size = (i * 7919) % 300_000;
            std::fs::write(&p, vec![(i % 251) as u8; size]).unwrap();
            files.push(p);
        }
        let expected: Vec<[u8; 32]> = files
            .iter()
            .map(|p| *blake3::hash(&std::fs::read(p).unwrap()).as_bytes())
            .collect();
        let produced = files.clone();
        let mut seen = Vec::new();
        run(
            4,
            move |emit: &mut dyn FnMut(WalkEvent) -> anyhow::Result<()>| {
                for (i, p) in produced.iter().enumerate() {
                    emit(WalkEvent::Entry(observed(p, &format!("f{i:03}"))))?;
                    if i == 50 {
                        emit(WalkEvent::Warning("after 50".into()))?;
                    }
                }
                Ok(())
            },
            |_obs: &mut Observed| Decision::Hash,
            |ev| {
                seen.push(ev);
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(seen.len(), 201);
        let mut file_idx = 0;
        for (i, ev) in seen.iter().enumerate() {
            match ev {
                WalkEvent::Warning(w) => {
                    assert_eq!(i, 51);
                    assert_eq!(w, "after 50");
                }
                WalkEvent::NotDescended(p) => panic!("unexpected NotDescended {p:?}"),
                WalkEvent::Entry(obs) => {
                    assert_ne!(i, 51);
                    assert_eq!(obs.path, format!("f{file_idx:03}").into_bytes());
                    assert_eq!(obs.entry.blake3, Some(expected[file_idx]));
                    assert!(obs.hash_error.is_none());
                    file_idx += 1;
                }
            }
        }
        assert_eq!(file_idx, 200);
    }

    #[test]
    fn pipeline_skip_leaves_prefilled_hash() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("f");
        std::fs::write(&p, b"data").unwrap();
        let mut seen = Vec::new();
        run(
            2,
            move |emit: &mut dyn FnMut(WalkEvent) -> anyhow::Result<()>| {
                emit(WalkEvent::Entry(observed(&p, "f")))
            },
            |obs: &mut Observed| {
                obs.entry.blake3 = Some([7; 32]);
                Decision::Skip
            },
            |ev| {
                seen.push(ev);
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(seen.len(), 1);
        let WalkEvent::Entry(obs) = &seen[0] else {
            panic!("expected entry, got {:?}", seen[0]);
        };
        assert_eq!(obs.entry.blake3, Some([7; 32]));
    }

    #[test]
    fn pipeline_marks_unreadable() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("f");
        std::fs::write(&p, b"secret").unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o000)).unwrap();
        if std::fs::read(&p).is_ok() {
            eprintln!("skipping: running with privileges that bypass file permissions");
            return;
        }
        let mut seen = Vec::new();
        run(
            1,
            move |emit: &mut dyn FnMut(WalkEvent) -> anyhow::Result<()>| {
                emit(WalkEvent::Entry(observed(&p, "f")))
            },
            |_obs: &mut Observed| Decision::Hash,
            |ev| {
                seen.push(ev);
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(seen.len(), 1);
        let WalkEvent::Entry(obs) = &seen[0] else {
            panic!("expected entry, got {:?}", seen[0]);
        };
        assert_eq!(obs.entry.blake3, None);
        assert!(obs.entry.flags.contains(Flags::UNREADABLE));
        assert!(obs.hash_error.is_some());
    }

    #[test]
    fn pipeline_propagates_producer_error() {
        let err = run(
            2,
            |emit: &mut dyn FnMut(WalkEvent) -> anyhow::Result<()>| {
                emit(WalkEvent::Warning("w".into()))?;
                anyhow::bail!("boom")
            },
            |_obs: &mut Observed| Decision::Hash,
            |_ev| Ok(()),
        )
        .unwrap_err();
        assert!(format!("{err:#}").contains("boom"), "{err:#}");
    }

    #[test]
    fn pipeline_sink_error_aborts_without_deadlock() {
        // More events than the walk channel holds, so the producer is blocked
        // on send when the sink fails.
        let err = run(
            2,
            |emit: &mut dyn FnMut(WalkEvent) -> anyhow::Result<()>| {
                for _ in 0..(WALK_CAPACITY * 4) {
                    emit(WalkEvent::Warning("w".into()))?;
                }
                Ok(())
            },
            |_obs: &mut Observed| Decision::Hash,
            |_ev| anyhow::bail!("sink full"),
        )
        .unwrap_err();
        assert!(format!("{err:#}").contains("sink full"), "{err:#}");
    }
}
