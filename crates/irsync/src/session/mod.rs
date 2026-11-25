//! Sync sessions. This module holds the local side of a session
//! ([`LocalSide`]) and the trailing-slash rule ([`resolve_roots`]); the
//! rest has a file each: the request a `Hello` carries (`hello`), the
//! session key (`key`), the stored record (`record`), the daemon's
//! registry of hosted sessions (`registry`), events and heartbeats
//! (`events`), the outcome (`outcome`) and the test hooks (`hooks`). The
//! connection-level driver is [`engine`]; the roles are `receiver` and
//! `sender`.

mod control;
pub mod engine;
mod events;
mod hello;
mod hooks;
mod key;
mod limits;
mod listing_stream;
mod outcome;
mod parts;
mod receiver;
pub mod reconnect;
mod record;
mod registry;
mod sender;
pub mod single;

pub use events::{Busy, Event, EventBus, Party, SessionSummary};
pub use hello::{hello, SyncHello};
pub use hooks::*;
pub use key::SessionKey;
pub use outcome::Outcome;
pub use record::{IndexScope, SessionRecord};
pub use registry::{DaemonSessions, SessionClaim, SessionRegistry, CLOSE_SUPERSEDED};

use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::filter::FilterSet;
use crate::hasher::Hasher;
use crate::index::IndexHandle;
use crate::options::Direction;
use crate::platform::{bytes_to_os, os_to_bytes};
use crate::telemetry::meter::Meter;

/// Files hashed concurrently by a session's hasher, at most
/// `hasher::LARGE_WORKERS` of them large. Many small reads in flight let a
/// spinning disk order them (`docs/IRSYNC_DECISIONS.md`, 2.1 item 7).
pub const HASH_WORKERS: usize = 128;

/// This process's end of a session: the directory it reads or writes, with
/// its index and hasher. `filters` only shape the walk when this side sends;
/// the client's filters are not transmitted to the daemon in v1.
#[derive(Clone)]
pub struct LocalSide {
    pub root: PathBuf,
    /// rsync's trailing-slash rule as typed (part of the session key; the
    /// root itself is already resolved, see [`resolve_roots`]).
    pub contents_only: bool,
    pub index: IndexHandle,
    pub hasher: Arc<Hasher>,
    pub filters: Arc<FilterSet>,
    pub hooks: SideHooks,
    /// Progress events for the user interface.
    pub bus: EventBus,
    /// A single-file session: this side's name for the file, in `root`.
    pub single: Option<Vec<u8>>,
}

impl LocalSide {
    pub fn new(
        root: PathBuf,
        contents_only: bool,
        index: IndexHandle,
        filters: Arc<FilterSet>,
    ) -> LocalSide {
        let hasher = Arc::new(Hasher::new(index.clone(), root.clone(), HASH_WORKERS));
        LocalSide {
            root,
            contents_only,
            index,
            hasher,
            filters,
            hooks: SideHooks::default(),
            bus: EventBus::default(),
            single: None,
        }
    }

    /// This side as the one file `name` in `root`.
    pub fn with_single(self, name: Vec<u8>) -> LocalSide {
        LocalSide {
            single: Some(name),
            ..self
        }
    }
}

/// The start of a timed wait, when `m` is recording (it reads no clock otherwise).
pub(crate) fn wait_start(m: &Meter) -> Option<std::time::Instant> {
    m.is_enabled().then(std::time::Instant::now)
}

/// Records how long a role waited on its hasher.
pub(crate) fn record_hash_wait(m: &Meter, since: Option<std::time::Instant>) {
    if let Some(t) = since {
        metrics::histogram!("irsync_hash_wait_seconds", m.labels())
            .record(t.elapsed().as_secs_f64());
    }
}

/// Records a transferred file: its size, and the time from its first request to its DONE.
pub(crate) fn record_file(m: &Meter, since: Option<std::time::Instant>, size: u64) {
    let Some(t) = since else { return };
    metrics::histogram!("irsync_file_transfer_seconds", m.labels())
        .record(t.elapsed().as_secs_f64());
    metrics::histogram!("irsync_file_size_bytes", m.labels()).record(size as f64);
}

/// Whether the local operand `typed`, as the user typed it, names an
/// entry (`src`, `a/src`, `src/.`) or only a directory's contents (`.`,
/// `..`, `a/..`, a root, and on Windows a drive alone, `C:`): rsync's
/// meaning, which only the path as typed still shows. Making it absolute
/// first ([`crate::platform::local_path`]) would turn `.` into the current
/// directory's own name.
pub fn operand_names_itself(typed: &Path) -> bool {
    typed.file_name().is_some()
}

/// Applies rsync's trailing-slash rule. On a push, `src` without a slash
/// copies the directory itself: its basename is returned as `Hello.name`
/// for the daemon to join after resolving `remote`. On a pull, the last
/// component of `remote` names the local directory. The remote path is
/// never changed here: the daemon resolves it.
///
/// `local` is the local path as it is opened, `typed` the same operand as
/// the user typed it (on Unix the two are the same path): whether a push
/// sends a name is decided from `typed` ([`operand_names_itself`]), the
/// name itself is the last one of `local`, the directory that is read.
pub(crate) fn resolve_roots(
    direction: Direction,
    typed: &Path,
    local: &Path,
    local_contents_only: bool,
    remote: &[u8],
    remote_contents_only: bool,
) -> (PathBuf, Vec<u8>) {
    match direction {
        Direction::ClientSends => {
            let name = (!local_contents_only && operand_names_itself(typed))
                .then(|| local.file_name())
                .flatten()
                .map(os_to_bytes)
                .unwrap_or_default();
            (local.to_path_buf(), name)
        }
        Direction::DaemonSends => {
            // Split as typed here: at `/`, and on Windows at `\` too.
            let last = remote
                .rsplit(|&b| crate::platform::Platform::CURRENT.is_separator(b))
                .find(|c| !c.is_empty() && *c != b"." && *c != b"~");
            let root = match last {
                // The remote path was typed here as text (`Location::parse`
                // takes a `&str`), so it is a name on every system.
                Some(name) if !remote_contents_only => local
                    .join(bytes_to_os(name).expect("a path typed here is a name of this system")),
                _ => local.to_path_buf(),
            };
            (root, Vec::new())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What an operand names is read from it as typed: `.`, `..` and
    /// `a/..` send only the contents (no name), whatever directory they
    /// are once made absolute.
    #[test]
    fn whether_an_operand_names_itself_is_read_as_typed() {
        for (typed, names) in [
            (".", false),
            ("..", false),
            ("a/..", false),
            ("src", true),
            ("src/", true),
            ("src/.", true),
            ("a/src", true),
        ] {
            assert_eq!(operand_names_itself(Path::new(typed)), names, "{typed}");
        }
        // A push of `.` from the directory `/w/photos`: the directory read
        // is `/w/photos`, but no name is sent, so its contents land in the
        // remote directory itself. `src/.` sends `src`.
        let opened = Path::new("/w/photos");
        for typed in [".", "..", "a/.."] {
            let (root, name) = resolve_roots(
                Direction::ClientSends,
                Path::new(typed),
                opened,
                false,
                b"m",
                false,
            );
            assert_eq!((root.as_path(), name), (opened, vec![]), "{typed}");
        }
        let (_, name) = resolve_roots(
            Direction::ClientSends,
            Path::new("photos/."),
            opened,
            false,
            b"m",
            false,
        );
        assert_eq!(name, b"photos");
    }

    /// The same on Windows, with its own spellings: `.\`, a drive alone
    /// (its current directory), `C:\x\.` (names `x`) and `a\..`.
    #[cfg(windows)]
    #[test]
    fn whether_an_operand_names_itself_is_read_as_typed_on_windows() {
        for (typed, names) in [
            (r".\", false),
            (r"..\", false),
            (r"a\..", false),
            ("C:", false),
            (r"C:\", false),
            (r"C:\.", false),
            (r"C:\x\.", true),
            (r"C:\x", true),
            (r"a\src", true),
        ] {
            assert_eq!(operand_names_itself(Path::new(typed)), names, "{typed}");
        }
        let opened = Path::new(r"C:\w\photos");
        let (_, name) = resolve_roots(
            Direction::ClientSends,
            Path::new(r"a\.."),
            opened,
            false,
            b"m",
            false,
        );
        assert_eq!(name, b"");
        // `photos.` names the directory Windows reads, `photos`.
        let (_, name) = resolve_roots(
            Direction::ClientSends,
            Path::new("photos."),
            opened,
            false,
            b"m",
            false,
        );
        assert_eq!(name, b"photos");
    }

    #[test]
    fn trailing_slash_rule_puts_the_basename_in_name() {
        use Direction::*;
        let p = Path::new("/l/photos");
        let r = |d, l: &Path, lco, rem: &[u8], rco| resolve_roots(d, l, l, lco, rem, rco);
        assert_eq!(
            r(ClientSends, p, false, b"", false),
            (p.into(), b"photos".to_vec())
        );
        assert_eq!(r(ClientSends, p, true, b"", false), (p.into(), vec![]));
        assert_eq!(
            r(ClientSends, p, false, b"/srv/x", false),
            (p.into(), b"photos".to_vec())
        );
        // Pull: the remote's last component names the local directory; `name` stays empty.
        let l = Path::new("/l");
        assert_eq!(
            r(DaemonSends, l, false, b"/srv/photos", false),
            (PathBuf::from("/l/photos"), vec![])
        );
        assert_eq!(
            r(DaemonSends, l, false, b"/srv/photos", true),
            (PathBuf::from("/l"), vec![])
        );
        assert_eq!(
            r(DaemonSends, l, false, b"", false),
            (PathBuf::from("/l"), vec![])
        );
        assert_eq!(
            r(DaemonSends, l, false, b"/", false),
            (PathBuf::from("/l"), vec![])
        );
        assert_eq!(
            r(DaemonSends, l, false, b"photos", false),
            (PathBuf::from("/l/photos"), vec![])
        );
    }
}
