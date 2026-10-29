//! With telemetry never enabled, a session's usage is all zeros.

mod common;

use std::fs;
use std::time::Duration;

use common::{archive_opts, sync_trees, Flow};
use irsync::telemetry::meter::{Activity, Counter};

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn disabled_usage_is_zero() {
    let opts = archive_opts();
    for flow in [Flow::Push, Flow::Pull] {
        let (out, _h) = sync_trees(
            |r| fs::write(r.join("a.bin"), vec![7u8; 100_000]).unwrap(),
            |_| {},
            &opts,
            flow,
        )
        .await;
        assert_eq!(out.files_transferred, 1);
        for a in Activity::ALL {
            assert_eq!(out.usage.busy(a), Duration::ZERO, "{a:?}");
        }
        for c in Counter::ALL {
            assert_eq!(out.usage.count(c), 0, "{c:?}");
        }
        assert_eq!(out.usage.wall, Duration::ZERO);
        assert_eq!(out.usage.peer_wait, Duration::ZERO);
    }
}
