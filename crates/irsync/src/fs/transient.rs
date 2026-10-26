//! Bounded retry of transient OS errors around file opens.
//!
//! A busy receiver can run into `EMFILE` (this process is at its open-file
//! limit) or `ENFILE` (the system is) for a moment: other files in flight
//! are about to be placed and closed. Failing the file for that would cost
//! a rerun, so an open that hits such an error is tried again on a short,
//! bounded schedule (about a second in all). Errors that will not fix
//! themselves (permission denied, no space, not found) fail at once.

use std::io;
use std::time::Duration;

/// Tries after the first failure.
pub const RETRIES: usize = 6;
/// The first wait; each later one doubles it.
const FIRST_WAIT: Duration = Duration::from_millis(15);

/// Whether `e` is a momentary shortage worth waiting out.
pub fn is_transient(e: &io::Error) -> bool {
    use nix::libc::{EAGAIN, EMFILE, ENFILE, ENOBUFS, ENOMEM};
    matches!(
        e.raw_os_error(),
        Some(EMFILE | ENFILE | EAGAIN | ENOMEM | ENOBUFS)
    )
}

/// The default waits between tries: 15, 30, 60, 120, 240, 480 ms.
pub fn waits() -> impl Iterator<Item = Duration> {
    (0..RETRIES as u32).map(|i| FIRST_WAIT * 2u32.pow(i))
}

/// Runs `op`, trying again after each wait in [`waits`] while it fails
/// with a transient error. Blocking: sleeps on the calling thread, so call
/// it where the open itself is allowed to block.
pub fn retry<T>(op: impl FnMut() -> io::Result<T>) -> io::Result<T> {
    retry_with(op, waits())
}

/// [`retry`] with an explicit wait schedule; one try per wait, plus the
/// first. The last error is returned when the schedule runs out.
pub fn retry_with<T>(
    mut op: impl FnMut() -> io::Result<T>,
    waits: impl IntoIterator<Item = Duration>,
) -> io::Result<T> {
    let mut waits = waits.into_iter();
    loop {
        match op() {
            Err(e) if is_transient(&e) => match waits.next() {
                Some(wait) => {
                    tracing::debug!(error = %e, wait_ms = wait.as_millis() as u64, "transient error; trying again");
                    std::thread::sleep(wait);
                }
                None => return Err(e),
            },
            r => return r,
        }
    }
}
