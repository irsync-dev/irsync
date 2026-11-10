//! A lock file in the state dir that a running daemon holds, so a second
//! daemon refuses to start and `irsync ssh-setup` can tell which copy of
//! irsync (by exe path and version) is running.

use std::fs::{File, OpenOptions, TryLockError};
use std::io::{Read, Seek, Write};
use std::path::{Path, PathBuf};

use crate::build::LONG_VERSION;

const LOCK_NAME: &str = "daemon.lock";

/// Who holds the lock, as the holder wrote it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LockInfo {
    pub pid: u32,
    pub exe: PathBuf,
    pub version: String,
}

#[derive(Debug, thiserror::Error)]
pub enum LockError {
    #[error(
        "another irsync daemon (pid {}, {}) is already running",
        .0.pid,
        .0.exe.display()
    )]
    Held(LockInfo),
    #[error("daemon lock {}: {source}", .path.display())]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
}

/// The held lock; released when dropped (or when the process exits).
#[derive(Debug)]
pub struct DaemonLock {
    _file: File,
}

fn parse(text: &str) -> LockInfo {
    let mut info = LockInfo {
        pid: 0,
        exe: PathBuf::new(),
        version: String::new(),
    };
    for line in text.lines() {
        if let Some(v) = line.strip_prefix("pid=") {
            info.pid = v.trim().parse().unwrap_or(0);
        } else if let Some(v) = line.strip_prefix("exe=") {
            info.exe = PathBuf::from(v);
        } else if let Some(v) = line.strip_prefix("version=") {
            info.version = v.to_string();
        }
    }
    info
}

fn read_info(file: &mut File) -> LockInfo {
    let mut text = String::new();
    let _ = file.rewind();
    let _ = file.read_to_string(&mut text);
    parse(&text)
}

/// Takes the daemon lock in `state_dir` and records this process in it.
pub fn acquire(state_dir: &Path) -> Result<DaemonLock, LockError> {
    let path = state_dir.join(LOCK_NAME);
    let io = |source| LockError::Io {
        path: path.clone(),
        source,
    };
    std::fs::create_dir_all(state_dir).map_err(io)?;
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&path)
        .map_err(io)?;
    match file.try_lock() {
        Ok(()) => {}
        Err(TryLockError::WouldBlock) => return Err(LockError::Held(read_info(&mut file))),
        Err(TryLockError::Error(e)) => return Err(io(e)),
    }
    let exe = std::env::current_exe().map_err(io)?;
    file.set_len(0).map_err(io)?;
    file.rewind().map_err(io)?;
    write!(
        file,
        "pid={}\nexe={}\nversion={}\n",
        std::process::id(),
        exe.display(),
        *LONG_VERSION
    )
    .map_err(io)?;
    file.flush().map_err(io)?;
    Ok(DaemonLock { _file: file })
}

/// `Some` when a live daemon holds the lock in `state_dir`.
pub fn probe(state_dir: &Path) -> Option<LockInfo> {
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(state_dir.join(LOCK_NAME))
        .ok()?;
    match file.try_lock() {
        Ok(()) => {
            let _ = file.unlock();
            None
        }
        Err(TryLockError::WouldBlock) => Some(read_info(&mut file)),
        Err(TryLockError::Error(_)) => None,
    }
}
