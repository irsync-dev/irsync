//! Long-lived work stays off tokio's blocking pool. A walk and its listing
//! loop each wait on bounded channels for the whole walk; on the pool they
//! held a thread each, so once concurrent sessions had taken every thread
//! with them, the per-file blocking calls that would drain those channels
//! queued behind them and nothing moved (with tokio's default of 512
//! threads, 256 concurrent pulls hung the daemon).

mod common;

use std::fs;
use std::time::Duration;

use common::{archive_opts, Flow, Harness};

const SESSIONS: usize = 2;
const FILES: usize = 20_000;
const DIRS: usize = 20;

/// A leaf directory three 240-byte names deep: the listing of a session
/// (about 800 bytes an entry, 16 MB) outgrows the QUIC stream window and
/// the queues before it, so the walk has to wait for the peer.
fn leaf(d: usize) -> std::path::PathBuf {
    (0..3).map(|level| format!("{level}{d:_>239}")).collect()
}

#[test]
fn concurrent_pulls_finish_when_their_walks_could_fill_the_blocking_pool() {
    // Two walking threads per pulling session: the pool is exactly full.
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4)
        .max_blocking_threads(2 * SESSIONS)
        .enable_all()
        .build()
        .unwrap();
    let res = rt.block_on(async {
        let h = Harness::new().await;
        for s in 0..SESSIONS {
            for d in 0..DIRS {
                let dir = h.export.path().join(format!("s{s}")).join(leaf(d));
                fs::create_dir_all(&dir).unwrap();
                for f in 0..FILES / DIRS {
                    fs::write(dir.join(format!("{f:0>60}")), f.to_le_bytes()).unwrap();
                }
            }
        }
        let opts = archive_opts();
        let pulls = (0..SESSIONS).map(|s| {
            let dst = h.local.path().join(format!("p{s}"));
            let state = h.client_state.path().join(format!("p{s}"));
            fs::create_dir_all(&dst).unwrap();
            fs::create_dir_all(&state).unwrap();
            let mut hello = Harness::hello(Flow::Pull);
            hello.path = format!("m/s{s}").into_bytes();
            let (h, opts) = (&h, &opts);
            async move {
                let out = h
                    .run_hello(opts, hello, Harness::side_for(&dst, &state))
                    .await;
                (out, dst)
            }
        });
        let done =
            tokio::time::timeout(Duration::from_secs(60), futures::future::join_all(pulls)).await;
        let Ok(done) = done else {
            return Err("the pulls did not finish within 60 s".to_string());
        };
        for (out, dst) in done {
            let out = out.map_err(|e| format!("{e:#}"))?;
            if !out.errors.is_empty() {
                return Err(format!("{:?}", out.errors));
            }
            let n: usize = (0..DIRS)
                .map(|d| fs::read_dir(dst.join(leaf(d))).map_or(0, |r| r.count()))
                .sum();
            if n != FILES {
                return Err(format!("{} holds {n} of {FILES} files", dst.display()));
            }
        }
        Ok(())
    });
    // Without the fix the walks are still blocked: do not wait for them.
    rt.shutdown_timeout(Duration::from_secs(1));
    res.unwrap();
}
