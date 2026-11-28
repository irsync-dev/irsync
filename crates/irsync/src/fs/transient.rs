//! Bounded retry of transient OS errors around file operations.
//!
//! A busy receiver can run into `EMFILE` (this process is at its open-file
//! limit) or `ENFILE` (the system is) for a moment: other files in flight
//! are about to be placed and closed. Failing the file for that would cost
//! a rerun, so an open that hits such an error is tried again on a short,
//! bounded schedule (about a second in all). Errors that will not fix
//! themselves (permission denied, no space, not found) fail at once.
//!
//! On Windows another program may hold a file so that it cannot be opened,
//! replaced, renamed or removed for a moment ([`Cause::InUse`]): antivirus
//! and the indexer open files the moment they appear. Such an operation is
//! tried again on its own schedule ([`in_use_waits`], about a second), and
//! when that runs out its error says only that the file is in use
//! ([`IN_USE`]). On Unix no operation fails as in use, so nothing there is
//! retried for it.

use std::io;
use std::time::Duration;

use crate::platform::{self, Cause};

/// Tries after the first failure.
pub const RETRIES: usize = 6;
/// The first wait; each later one doubles it.
const FIRST_WAIT: Duration = Duration::from_millis(15);

/// The waits, in milliseconds, before each new try of an operation that
/// failed because another program holds the file: about one second.
pub const IN_USE_WAITS_MS: [u64; 7] = [10, 30, 60, 100, 200, 300, 300];

/// What an entry's failure says when another program held the file through
/// every try: shown as `{path}: in use by another program`.
pub const IN_USE: &str = "in use by another program";

/// Whether `e` is a momentary shortage worth waiting out.
pub fn is_transient(e: &io::Error) -> bool {
    platform::cause(e) == Cause::OutOfHandles || platform::is_transient_extra(e)
}

/// Whether `e` says another program holds the file: the system's error
/// ([`Cause::InUse`]) or the one [`retry_in_use`] gives once its tries are
/// spent.
pub fn is_in_use(e: &io::Error) -> bool {
    platform::cause(e) == Cause::InUse || e.get_ref().is_some_and(|r| r.is::<InUse>())
}

/// The error of an operation another program kept from working through
/// every try: it reads [`IN_USE`], and keeps the system's error as its
/// source.
#[derive(Debug)]
struct InUse(io::Error);

impl std::fmt::Display for InUse {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(IN_USE)
    }
}

impl std::error::Error for InUse {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.0)
    }
}

/// `e` as the error that says the file is in use (see [`IN_USE`]).
fn in_use_error(e: io::Error) -> io::Error {
    io::Error::new(e.kind(), InUse(e))
}

/// The default waits between tries: 15, 30, 60, 120, 240, 480 ms.
pub fn waits() -> impl Iterator<Item = Duration> {
    (0..RETRIES as u32).map(|i| FIRST_WAIT * 2u32.pow(i))
}

/// The waits between tries of an operation on a file in use
/// ([`IN_USE_WAITS_MS`]).
pub fn in_use_waits() -> impl Iterator<Item = Duration> {
    IN_USE_WAITS_MS.into_iter().map(Duration::from_millis)
}

/// Runs `op`, trying again after each wait in [`waits`] while it fails
/// with a transient error, and after each of [`in_use_waits`] while the
/// file is in use. Blocking: sleeps on the calling thread, so call it where
/// the open itself is allowed to block.
pub fn retry<T>(op: impl FnMut() -> io::Result<T>) -> io::Result<T> {
    retry_with(op, waits())
}

/// [`retry`] with an explicit wait schedule for transient errors; one try
/// per wait, plus the first. The last error is returned when the schedule
/// runs out. A file in use is waited out on [`in_use_waits`].
pub fn retry_with<T>(
    op: impl FnMut() -> io::Result<T>,
    waits: impl IntoIterator<Item = Duration>,
) -> io::Result<T> {
    retry_both(op, waits, in_use_waits())
}

/// Runs `op` (a replace, rename or removal of an entry), trying again
/// after each of [`in_use_waits`] while another program holds the file;
/// then fails with the error that says so ([`IN_USE`]). Any other error
/// is returned at once. Blocking, as [`retry`].
pub fn retry_in_use<T>(op: impl FnMut() -> io::Result<T>) -> io::Result<T> {
    retry_in_use_with(op, in_use_waits())
}

/// [`retry_in_use`] with an explicit wait schedule.
pub fn retry_in_use_with<T>(
    op: impl FnMut() -> io::Result<T>,
    in_use: impl IntoIterator<Item = Duration>,
) -> io::Result<T> {
    retry_both(op, std::iter::empty(), in_use)
}

fn retry_both<T>(
    mut op: impl FnMut() -> io::Result<T>,
    waits: impl IntoIterator<Item = Duration>,
    in_use: impl IntoIterator<Item = Duration>,
) -> io::Result<T> {
    let (mut waits, mut in_use) = (waits.into_iter(), in_use.into_iter());
    loop {
        let wait = match op() {
            Err(e) if is_transient(&e) => match waits.next() {
                Some(wait) => {
                    tracing::debug!(error = %e, wait_ms = wait.as_millis() as u64, "transient error; trying again");
                    wait
                }
                None => return Err(e),
            },
            Err(e) if platform::cause(&e) == Cause::InUse => match in_use.next() {
                Some(wait) => {
                    tracing::debug!(error = %e, wait_ms = wait.as_millis() as u64, "in use; trying again");
                    wait
                }
                None => return Err(in_use_error(e)),
            },
            r => return r,
        };
        std::thread::sleep(wait);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    /// An error the platform reads as in use, where it has one.
    fn in_use_os() -> Option<io::Error> {
        // ERROR_SHARING_VIOLATION on Windows; Unix has no such error.
        (platform::Platform::CURRENT == platform::Platform::Windows)
            .then(|| io::Error::from_raw_os_error(32))
    }

    #[test]
    fn the_in_use_schedule_is_about_a_second() {
        let total: u64 = IN_USE_WAITS_MS.iter().sum();
        assert_eq!(total, 1000);
        assert_eq!(in_use_waits().count(), 7);
    }

    #[test]
    fn a_file_in_use_is_tried_again_then_said_to_be_in_use() {
        let Some(_) = in_use_os() else {
            return;
        };
        let calls = Cell::new(0);
        let r: io::Result<()> = retry_in_use_with(
            || {
                calls.set(calls.get() + 1);
                Err(in_use_os().unwrap())
            },
            [Duration::ZERO; 3],
        );
        let e = r.unwrap_err();
        assert_eq!(calls.get(), 4);
        assert_eq!(e.to_string(), IN_USE);
        assert!(is_in_use(&e));
        // Freed meanwhile: the next try works.
        calls.set(0);
        let r = retry_in_use_with(
            || {
                calls.set(calls.get() + 1);
                if calls.get() < 3 {
                    Err(in_use_os().unwrap())
                } else {
                    Ok(7)
                }
            },
            [Duration::ZERO; 3],
        );
        assert_eq!(r.unwrap(), 7);
    }

    /// Only "in use" is retried by `retry_in_use`: not a shortage of
    /// handles, nor anything else, so a call site that gains it does
    /// nothing new on Unix.
    #[test]
    fn retry_in_use_tries_nothing_else_again() {
        for e in [
            io::Error::from(io::ErrorKind::NotFound),
            io::Error::from(io::ErrorKind::PermissionDenied),
            io::Error::other("x"),
        ] {
            let calls = Cell::new(0);
            let kind = e.kind();
            let mut once = Some(e);
            let r: io::Result<()> = retry_in_use_with(
                || {
                    calls.set(calls.get() + 1);
                    Err(once.take().unwrap_or_else(|| io::Error::other("again")))
                },
                [Duration::ZERO; 3],
            );
            assert_eq!(calls.get(), 1);
            assert_eq!(r.unwrap_err().kind(), kind);
        }
    }
}
