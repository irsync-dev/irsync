//! Options a pulling client sends to the sending daemon (`--bwlimit`).

mod common;

use std::fs;
use std::time::{Duration, Instant};

use common::{archive_opts, Flow, Harness};

const MIB: usize = 1024 * 1024;

async fn within30<F: std::future::Future>(f: F) -> F::Output {
    tokio::time::timeout(Duration::from_secs(30), f)
        .await
        .expect("test timed out after 30 s")
}

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

/// On a pull the daemon sends the file data, so it is the daemon that has
/// to pace it: 2 MiB at 1 MiB/s takes about two seconds, not the few
/// milliseconds loopback needs.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn bwlimit_paces_a_pull() {
    within30(async {
        let flow = Flow::Pull;
        let h = Harness::new().await;
        let bytes = data(2 * MIB, 5);
        fs::write(h.src(flow).join("slow.bin"), &bytes).unwrap();
        let mut opts = archive_opts();
        opts.bwlimit = Some(MIB as u64);
        let t = Instant::now();
        let out = h.run(&opts, flow).await.unwrap_or_else(|e| panic!("{e:#}"));
        let el = t.elapsed();
        assert!(out.errors.is_empty(), "{:?}", out.errors);
        assert_eq!(fs::read(h.dst(flow).join("slow.bin")).unwrap(), bytes);
        assert!(el >= Duration::from_millis(1500), "too fast: {el:?}");
    })
    .await;
}
