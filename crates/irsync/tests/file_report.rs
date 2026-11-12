//! The file report (`IRSYNC_FILE_REPORT`): one JSON line per retried,
//! given-up or vanished file. The in-process harness gives each side its own
//! sink through the `file_report` hook.

mod common;

use std::fs;
use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use common::{archive_opts, Flow, Harness};
use irsync::file_report::{record_outcome_to, record_to, ReportEvent};
use irsync::fs::paths::RelPath;
use irsync::hasher::HashHook;
use irsync::session::{Outcome, SideHooks};

const MIB: usize = 1024 * 1024;
const SIZE: usize = 8 * MIB;
const TEAR: usize = 256 * 1024;

async fn within30<F: std::future::Future>(f: F) -> F::Output {
    tokio::time::timeout(Duration::from_secs(30), f)
        .await
        .expect("test timed out after 30 s")
}

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

fn set_mtime(f: &fs::File, secs: u64) {
    f.set_modified(SystemTime::UNIX_EPOCH + Duration::from_secs(secs))
        .unwrap();
}

fn seed(h: &Harness, flow: Flow) {
    let content = data(SIZE, 7);
    fs::write(h.src(flow).join("x.bin"), &content).unwrap();
    let dst = h.dst(flow).join("x.bin");
    fs::write(&dst, &content).unwrap();
    set_mtime(
        &fs::File::options().write(true).open(&dst).unwrap(),
        1_000_000_000,
    );
}

/// Tears `x.bin` once per pass, after 4 MiB of it were hashed.
fn tearing_hook(src: PathBuf) -> HashHook {
    let passes = Arc::new(AtomicU32::new(0));
    let torn = Arc::new(AtomicBool::new(false));
    Arc::new(move |p: &RelPath, bytes: u64| {
        if p.as_bytes() != b"x.bin" {
            return;
        }
        if bytes == 0 {
            passes.fetch_add(1, Ordering::SeqCst);
            torn.store(false, Ordering::SeqCst);
        } else if bytes >= (4 * MIB) as u64 && !torn.swap(true, Ordering::SeqCst) {
            let pass = passes.load(Ordering::SeqCst);
            let f = fs::File::options()
                .write(true)
                .open(src.join("x.bin"))
                .unwrap();
            f.write_all_at(&data(TEAR, 100 + 2 * pass), 0).unwrap();
            set_mtime(&f, 1_000_000_000 + u64::from(pass));
        }
    })
}

/// Runs a sync with the tear on every pass; returns the outcome and the
/// client's and the daemon's report lines.
async fn run(flow: Flow) -> (Outcome, Vec<String>, Vec<String>) {
    let h = Harness::new().await;
    seed(&h, flow);
    let dir = tempfile::tempdir().unwrap();
    let (cr, dr) = (dir.path().join("client"), dir.path().join("daemon"));
    let hook = tearing_hook(h.src(flow).to_path_buf());
    let mut local = h.local_side();
    local.hooks = SideHooks {
        file_report: Some(cr.clone()),
        during_hash: matches!(flow, Flow::Push).then(|| hook.clone()),
        ..SideHooks::default()
    };
    {
        let mut d = h.daemon.hooks.lock().unwrap();
        d.file_report = Some(dr.clone());
        if matches!(flow, Flow::Pull) {
            d.during_hash = Some(hook);
        }
    }
    let out = h.run_side(&archive_opts(), flow, local).await.unwrap();
    // The binary writes this from the final outcome (see main.rs).
    record_outcome_to(&cr, &out);
    (out, lines(&cr), lines(&dr))
}

fn lines(p: &Path) -> Vec<String> {
    fs::read_to_string(p)
        .map(|s| s.lines().map(String::from).collect())
        .unwrap_or_default()
}

fn count(lines: &[String], event: &str) -> usize {
    let key = format!("\"event\":\"{event}\"");
    lines.iter().filter(|l| l.contains(&key)).count()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn report_lists_retry_and_give_up_push() {
    let (out, client, daemon) = within30(run(Flow::Push)).await;
    assert_eq!(out.exit_code(), 23);
    assert_eq!(count(&client, "retried"), 1, "{client:?}");
    assert_eq!(count(&client, "gave_up"), 1, "{client:?}");
    assert_eq!(client.len(), 2, "{client:?}");
    assert!(client
        .iter()
        .all(|l| l.contains("\"path_hex\":\"782e62696e\"")));
    assert!(client
        .iter()
        .all(|l| l.contains("changed while it was hashed")));
    assert!(daemon.is_empty(), "{daemon:?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn report_lists_give_up_pull() {
    let (out, client, daemon) = within30(run(Flow::Pull)).await;
    assert_eq!(out.exit_code(), 23);
    assert_eq!(count(&client, "gave_up"), 1, "{client:?}");
    assert_eq!(client.len(), 1, "{client:?}");
    assert!(client[0].contains("changed while it was hashed"));
    assert_eq!(count(&daemon, "retried"), 1, "{daemon:?}");
    assert_eq!(daemon.len(), 1, "{daemon:?}");
    assert!(daemon[0].contains("\"path\":\"x.bin\""), "{daemon:?}");
}

#[test]
fn report_paths_are_bytes() {
    let dir = tempfile::tempdir().unwrap();
    let f = dir.path().join("r");
    let p = RelPath::new(b"d/\xff\n").unwrap();
    record_to(&f, ReportEvent::Vanished, &p, "say \"hi\"\n");
    let text = fs::read_to_string(&f).unwrap();
    assert_eq!(text.lines().count(), 1, "{text:?}");
    let hex = text
        .split("\"path_hex\":\"")
        .nth(1)
        .unwrap()
        .split('"')
        .next()
        .unwrap();
    let bytes: Vec<u8> = (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap())
        .collect();
    assert_eq!(bytes, b"d/\xff\n");
    assert!(text.contains("\"event\":\"vanished\""));
    assert!(text.contains(r#""reason":"say \"hi\"\n""#), "{text:?}");
}
