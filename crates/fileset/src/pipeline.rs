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
    run_with(jobs, producer, decide, sink, hash_file)
}

/// [`run`] with the hash function supplied, for tests.
fn run_with<P, D, S, H>(jobs: usize, producer: P, decide: D, sink: S, hash: H) -> anyhow::Result<()>
where
    P: FnOnce(&mut dyn FnMut(WalkEvent) -> anyhow::Result<()>) -> anyhow::Result<()>
        + Send
        + 'static,
    D: FnMut(&mut Observed) -> Decision,
    S: FnMut(WalkEvent) -> anyhow::Result<()>,
    H: Fn(&std::path::Path) -> std::io::Result<[u8; 32]> + Sync,
{
    let hash = &hash;
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
            s.spawn(move || hasher(job_rx, done_tx, hash));
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

fn hasher<H: Fn(&std::path::Path) -> std::io::Result<[u8; 32]>>(
    job_rx: Receiver<Job>,
    done_tx: Sender<Job>,
    hash: &H,
) {
    for (seq, mut obs) in job_rx {
        // A panic in the hash is reported like a read error; letting it unwind the
        // thread would leave the consumer waiting for this job forever.
        let hashed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| hash(&obs.abs)))
            .unwrap_or_else(|panic| {
                let msg = panic
                    .downcast_ref::<&str>()
                    .map(|s| s.to_string())
                    .or_else(|| panic.downcast_ref::<String>().cloned())
                    .unwrap_or_default();
                Err(std::io::Error::other(format!("hasher panicked: {msg}")))
            });
        match hashed {
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
            entry: Entry::from_path(abs).unwrap().0,
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
                WalkEvent::NotDescended(p) | WalkEvent::NestedManifest(p) => {
                    panic!("unexpected event for {p:?}")
                }
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
    fn pipeline_turns_a_hasher_panic_into_a_hash_error() {
        let dir = tempfile::tempdir().unwrap();
        let files: Vec<_> = (0..20)
            .map(|i| {
                let p = dir.path().join(format!("f{i:02}"));
                std::fs::write(&p, b"x").unwrap();
                p
            })
            .collect();
        let produced = files.clone();
        let mut seen = Vec::new();
        run_with(
            3,
            move |emit: &mut dyn FnMut(WalkEvent) -> anyhow::Result<()>| {
                for (i, p) in produced.iter().enumerate() {
                    emit(WalkEvent::Entry(observed(p, &format!("f{i:02}"))))?;
                }
                Ok(())
            },
            |_obs: &mut Observed| Decision::Hash,
            |ev| {
                seen.push(ev);
                Ok(())
            },
            |p: &Path| {
                if p.ends_with("f07") {
                    panic!("boom");
                }
                crate::hash::hash_file(p)
            },
        )
        .expect("a panic in one hasher does not hang or fail the run");
        assert_eq!(seen.len(), 20);
        for (i, ev) in seen.iter().enumerate() {
            let WalkEvent::Entry(obs) = ev else {
                panic!("unexpected event {ev:?}")
            };
            if i == 7 {
                assert!(obs.entry.blake3.is_none());
                assert!(obs.entry.flags.contains(Flags::UNREADABLE));
                let e = obs.hash_error.as_deref().unwrap();
                assert!(e.contains("panicked") && e.contains("boom"), "{e}");
            } else {
                assert!(obs.entry.blake3.is_some(), "f{i:02}");
                assert!(obs.hash_error.is_none());
            }
        }
    }

    #[test]
    fn pipeline_zero_jobs_still_hashes() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("f");
        std::fs::write(&p, b"data").unwrap();
        let mut seen = Vec::new();
        run(
            0,
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
        let WalkEvent::Entry(obs) = &seen[0] else {
            panic!("expected entry, got {:?}", seen[0]);
        };
        assert_eq!(obs.entry.blake3, Some(*blake3::hash(b"data").as_bytes()));
    }

    #[test]
    fn pipeline_producer_error_with_hashes_in_flight() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("f");
        std::fs::write(&p, vec![0u8; 1 << 20]).unwrap();
        let mut delivered = 0;
        let err = run(
            2,
            move |emit: &mut dyn FnMut(WalkEvent) -> anyhow::Result<()>| {
                for i in 0..100 {
                    emit(WalkEvent::Entry(observed(&p, &format!("f{i}"))))?;
                }
                anyhow::bail!("walk died")
            },
            |_obs: &mut Observed| Decision::Hash,
            |_ev| {
                delivered += 1;
                Ok(())
            },
        )
        .unwrap_err();
        assert!(format!("{err:#}").contains("walk died"), "{err:#}");
        // Everything emitted before the failure is still delivered, in order, hashed.
        assert_eq!(delivered, 100);
    }

    #[test]
    fn pipeline_reorder_window_bounds_what_waits_behind_a_slow_hash() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::Arc;
        use std::time::{Duration, Instant};
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("f");
        std::fs::write(&p, b"x").unwrap();
        let total = 4 * (JOB_CAPACITY + WALK_CAPACITY);
        let emitted = Arc::new(AtomicUsize::new(0));
        let counted = emitted.clone();
        // How far the walk got while the hash held everything back (a panic inside the
        // hash would only mark the entry unreadable, so the check happens outside).
        let ahead = Arc::new(AtomicUsize::new(0));
        let report = ahead.clone();
        let jobs = 2;
        let mut seen = 0;
        run_with(
            jobs,
            move |emit: &mut dyn FnMut(WalkEvent) -> anyhow::Result<()>| {
                for i in 0..total {
                    emit(WalkEvent::Entry(observed(&p, &format!("f{i}"))))?;
                    counted.fetch_add(1, Ordering::SeqCst);
                }
                Ok(())
            },
            // Only the first entry is hashed; the rest skip straight to the reorder buffer.
            |obs: &mut Observed| {
                if obs.path == b"f0" {
                    Decision::Hash
                } else {
                    Decision::Skip
                }
            },
            |_ev| {
                seen += 1;
                Ok(())
            },
            |path: &Path| {
                // The one hash holds everything behind it: wait until the walk stops
                // being fed, then check how much was taken in.
                let mut last = emitted.load(Ordering::SeqCst);
                let mut stable_since = Instant::now();
                loop {
                    std::thread::sleep(Duration::from_millis(20));
                    let now = emitted.load(Ordering::SeqCst);
                    if now != last {
                        last = now;
                        stable_since = Instant::now();
                    } else if stable_since.elapsed() > Duration::from_millis(400) {
                        break;
                    }
                }
                report.store(last, Ordering::SeqCst);
                crate::hash::hash_file(path)
            },
        )
        .unwrap();
        assert_eq!(seen, total);
        // Taken by the consumer: at most the window. Buffered in the walk channel: at
        // most its capacity, plus the one send that blocks.
        let bound = (JOB_CAPACITY + jobs) + WALK_CAPACITY + 1;
        let ahead = ahead.load(Ordering::SeqCst);
        assert!(
            ahead <= bound,
            "walk ran {ahead} entries ahead, bound is {bound}"
        );
        assert!(ahead > 0, "the hash never saw the walk run");
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
