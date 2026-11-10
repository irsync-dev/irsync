//! `daemon.addr` in the state dir: where the running daemon can be reached
//! right now (its home relay and its direct addresses), kept current while
//! it runs. `ssh-setup` reads it to hand a freshly started daemon's address
//! to the client, which can then dial before any lookup has a record.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use iroh::{Endpoint, EndpointAddr, RelayUrl, Watcher};

/// What `daemon.addr` holds.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AddrInfo {
    /// The daemon that wrote it.
    pub pid: u32,
    pub relay: Option<RelayUrl>,
    pub addrs: Vec<SocketAddr>,
}

impl AddrInfo {
    pub fn from_endpoint_addr(pid: u32, addr: &EndpointAddr) -> AddrInfo {
        AddrInfo {
            pid,
            relay: addr.relay_urls().next().cloned(),
            addrs: addr.ip_addrs().copied().collect(),
        }
    }

    fn to_text(&self) -> String {
        let mut out = format!("pid={}\n", self.pid);
        if let Some(r) = &self.relay {
            out.push_str(&format!("relay={r}\n"));
        }
        for a in &self.addrs {
            out.push_str(&format!("addr={a}\n"));
        }
        out
    }

    fn parse(text: &str) -> AddrInfo {
        let mut info = AddrInfo::default();
        for line in text.lines() {
            if let Some(v) = line.strip_prefix("pid=") {
                info.pid = v.trim().parse().unwrap_or(0);
            } else if let Some(v) = line.strip_prefix("relay=") {
                info.relay = v.trim().parse().ok();
            } else if let Some(v) = line.strip_prefix("addr=") {
                info.addrs.extend(v.trim().parse::<SocketAddr>().ok());
            }
        }
        info
    }
}

pub fn path(state: &Path) -> PathBuf {
    state.join("daemon.addr")
}

/// Writes `info` to `daemon.addr` in `state`, replacing it whole.
pub fn write(state: &Path, info: &AddrInfo) -> std::io::Result<()> {
    let tmp = state.join("daemon.addr.tmp");
    std::fs::write(&tmp, info.to_text())?;
    std::fs::rename(&tmp, path(state))
}

/// `daemon.addr` in `state`, if it is there.
pub fn read(state: &Path) -> Option<AddrInfo> {
    std::fs::read_to_string(path(state))
        .ok()
        .map(|t| AddrInfo::parse(&t))
}

/// Keeps `daemon.addr` current for `ep` until it closes.
pub fn spawn_writer(state: PathBuf, ep: Endpoint) {
    let pid = std::process::id();
    tokio::spawn(async move {
        let mut watch = ep.watch_addr();
        loop {
            let info = AddrInfo::from_endpoint_addr(pid, &watch.get());
            if let Err(e) = write(&state, &info) {
                tracing::warn!("writing {}: {e}", path(&state).display());
            }
            tokio::select! {
                changed = watch.updated() => if changed.is_err() { break },
                _ = ep.closed() => break,
            }
        }
    });
}

/// Waits up to `limit` for daemon `pid` to write an address it can be
/// reached at: its home relay when `want_relay`, else any direct address.
/// Returns what it has by then, or `None` when `pid` wrote nothing.
pub fn wait_for(state: &Path, pid: u32, want_relay: bool, limit: Duration) -> Option<AddrInfo> {
    let deadline = Instant::now() + limit;
    loop {
        let info = read(state).filter(|i| i.pid == pid);
        let ready = info.as_ref().is_some_and(|i| {
            if want_relay {
                i.relay.is_some()
            } else {
                !i.addrs.is_empty()
            }
        });
        if ready || Instant::now() >= deadline {
            return info;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrips_and_waits_for_the_right_pid() {
        let dir = tempfile::tempdir().unwrap();
        let info = AddrInfo {
            pid: 42,
            relay: Some("https://relay.example.net./".parse().unwrap()),
            addrs: vec![
                "192.0.2.1:7000".parse().unwrap(),
                "[::1]:7000".parse().unwrap(),
            ],
        };
        write(dir.path(), &info).unwrap();
        assert_eq!(read(dir.path()), Some(info.clone()));
        let quick = Duration::from_millis(100);
        assert_eq!(wait_for(dir.path(), 42, true, quick), Some(info));
        // A stale file from another daemon is not this one's address.
        assert_eq!(wait_for(dir.path(), 7, false, quick), None);
    }
}
