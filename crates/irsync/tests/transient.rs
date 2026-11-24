//! Bounded retry of transient OS errors (`EMFILE` and friends) around file
//! opens, and a writer that really runs out of descriptors.

#[cfg(unix)]
use std::io;
#[cfg(unix)]
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

#[cfg(unix)]
use irsync::fs::paths::RelPath;
#[cfg(unix)]
use irsync::fs::transient::{is_transient, retry_with};
use irsync::fs::transient::{waits, RETRIES};
#[cfg(unix)]
use irsync::index::Index;
#[cfg(unix)]
use irsync::platform::testutil::errno;
#[cfg(unix)]
use irsync::recv::FileWriter;
#[cfg(unix)]
use irsync_proto::chunk::digest_bytes;

#[cfg(unix)]
fn os(code: i32) -> io::Error {
    io::Error::from_raw_os_error(code)
}

#[test]
#[cfg(unix)]
fn resource_exhaustion_is_transient_but_permission_and_space_are_not() {
    for code in [
        errno::EMFILE,
        errno::ENFILE,
        errno::EAGAIN,
        errno::ENOMEM,
        errno::ENOBUFS,
    ] {
        assert!(is_transient(&os(code)), "{code}");
    }
    for code in [
        errno::EACCES,
        errno::ENOSPC,
        errno::ENOENT,
        errno::EIO,
        errno::EBUSY,
    ] {
        assert!(!is_transient(&os(code)), "{code}");
    }
    assert!(!is_transient(&io::Error::new(
        io::ErrorKind::WouldBlock,
        "part file busy"
    )));
}

#[test]
#[cfg(unix)]
fn retry_returns_the_first_success() {
    let calls = AtomicUsize::new(0);
    let r = retry_with(
        || {
            let n = calls.fetch_add(1, Ordering::SeqCst);
            if n < 2 {
                Err(os(errno::EMFILE))
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
#[cfg(unix)]
fn retry_gives_up_with_the_last_error_when_the_schedule_runs_out() {
    let calls = AtomicUsize::new(0);
    let r: io::Result<()> = retry_with(
        || {
            calls.fetch_add(1, Ordering::SeqCst);
            Err(os(errno::EMFILE))
        },
        std::iter::repeat_n(Duration::ZERO, 3),
    );
    assert_eq!(r.unwrap_err().raw_os_error(), Some(errno::EMFILE));
    assert_eq!(
        calls.load(Ordering::SeqCst),
        4,
        "one try per wait, plus the first"
    );
}

#[test]
#[cfg(unix)]
fn a_permanent_error_is_not_retried() {
    let calls = AtomicUsize::new(0);
    let r: io::Result<()> = retry_with(
        || {
            calls.fetch_add(1, Ordering::SeqCst);
            Err(os(errno::EACCES))
        },
        std::iter::repeat_n(Duration::ZERO, RETRIES),
    );
    assert_eq!(r.unwrap_err().raw_os_error(), Some(errno::EACCES));
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
#[cfg(unix)]
fn exhaust_descriptors() -> Vec<std::fs::File> {
    use irsync::platform::testutil::{open_file_limits, set_open_file_limits};
    let (soft, hard) = open_file_limits().unwrap();
    let low = soft.min(256);
    set_open_file_limits(low, hard).unwrap();
    let mut held = Vec::new();
    loop {
        match std::fs::File::open("/dev/null") {
            Ok(f) => held.push(f),
            Err(e) => {
                assert_eq!(e.raw_os_error(), Some(errno::EMFILE), "{e}");
                return held;
            }
        }
    }
}

#[tokio::test]
#[cfg(unix)]
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
