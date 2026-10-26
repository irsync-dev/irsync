//! Bounded retry of transient OS errors (`EMFILE` and friends) around file
//! opens, and a writer that really runs out of descriptors.

use std::io;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use irsync::fs::paths::RelPath;
use irsync::fs::transient::{is_transient, retry_with, waits, RETRIES};
use irsync::index::Index;
use irsync::recv::FileWriter;
use irsync_proto::chunk::digest_bytes;

fn os(code: i32) -> io::Error {
    io::Error::from_raw_os_error(code)
}

#[test]
fn resource_exhaustion_is_transient_but_permission_and_space_are_not() {
    for code in [
        nix::libc::EMFILE,
        nix::libc::ENFILE,
        nix::libc::EAGAIN,
        nix::libc::ENOMEM,
        nix::libc::ENOBUFS,
    ] {
        assert!(is_transient(&os(code)), "{code}");
    }
    for code in [
        nix::libc::EACCES,
        nix::libc::ENOSPC,
        nix::libc::ENOENT,
        nix::libc::EIO,
        nix::libc::EBUSY,
    ] {
        assert!(!is_transient(&os(code)), "{code}");
    }
    assert!(!is_transient(&io::Error::new(
        io::ErrorKind::WouldBlock,
        "part file busy"
    )));
}

#[test]
fn retry_returns_the_first_success() {
    let calls = AtomicUsize::new(0);
    let r = retry_with(
        || {
            let n = calls.fetch_add(1, Ordering::SeqCst);
            if n < 2 {
                Err(os(nix::libc::EMFILE))
            } else {
                Ok(n)
            }
        },
        std::iter::repeat_n(Duration::ZERO, RETRIES),
    );
    assert_eq!(r.unwrap(), 2);
    assert_eq!(calls.load(Ordering::SeqCst), 3);
}

#[test]
fn retry_gives_up_with_the_last_error_when_the_schedule_runs_out() {
    let calls = AtomicUsize::new(0);
    let r: io::Result<()> = retry_with(
        || {
            calls.fetch_add(1, Ordering::SeqCst);
            Err(os(nix::libc::EMFILE))
        },
        std::iter::repeat_n(Duration::ZERO, 3),
    );
    assert_eq!(r.unwrap_err().raw_os_error(), Some(nix::libc::EMFILE));
    assert_eq!(
        calls.load(Ordering::SeqCst),
        4,
        "one try per wait, plus the first"
    );
}

#[test]
fn a_permanent_error_is_not_retried() {
    let calls = AtomicUsize::new(0);
    let r: io::Result<()> = retry_with(
        || {
            calls.fetch_add(1, Ordering::SeqCst);
            Err(os(nix::libc::EACCES))
        },
        std::iter::repeat_n(Duration::ZERO, RETRIES),
    );
    assert_eq!(r.unwrap_err().raw_os_error(), Some(nix::libc::EACCES));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[test]
fn the_default_schedule_is_bounded_to_about_a_second() {
    let w: Vec<Duration> = waits().collect();
    assert_eq!(w.len(), RETRIES);
    let total: Duration = w.iter().sum();
    assert!(
        total >= Duration::from_millis(500) && total <= Duration::from_millis(1500),
        "{total:?}"
    );
    assert!(w.windows(2).all(|p| p[0] <= p[1]), "waits grow: {w:?}");
}

/// Fills the descriptor table (under a soft limit lowered to at most 256)
/// and returns the files holding it full.
fn exhaust_descriptors() -> Vec<std::fs::File> {
    use nix::sys::resource::{getrlimit, setrlimit, Resource};
    let (soft, hard) = getrlimit(Resource::RLIMIT_NOFILE).unwrap();
    let low = soft.min(256);
    setrlimit(Resource::RLIMIT_NOFILE, low, hard).unwrap();
    let mut held = Vec::new();
    loop {
        match std::fs::File::open("/dev/null") {
            Ok(f) => held.push(f),
            Err(e) => {
                assert_eq!(e.raw_os_error(), Some(nix::libc::EMFILE), "{e}");
                return held;
            }
        }
    }
}

#[tokio::test]
async fn a_writer_opens_its_part_once_descriptors_free_up() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("root");
    let state = tmp.path().join("state");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::create_dir_all(&state).unwrap();
    let index = Index::open(&state, &root).unwrap().into_handle();
    let data = vec![7u8; 100_000];
    let cl = digest_bytes(&data);
    let rel = RelPath::new(b"f.bin").unwrap();
    let mut w = FileWriter::open(&root, &rel, &cl, None, index).unwrap();
    let first = cl.chunks[0].len as usize;

    let held = exhaust_descriptors();
    let writer = tokio::spawn(async move {
        let r = w.write_chunk(0, &data[..first]).await;
        (w, r)
    });
    // The open is failing with EMFILE right now; free the table while the
    // writer is still inside its retry schedule.
    tokio::time::sleep(Duration::from_millis(150)).await;
    drop(held);
    let (w, r) = writer.await.unwrap();
    r.unwrap_or_else(|e| panic!("the write did not outlast the exhaustion: {e}"));
    assert!(w.is_open());
    assert!(root.join(".irsync.f.bin.part").exists());
}
