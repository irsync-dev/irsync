//! The sessions a daemon hosts: one connection per session id, and what
//! the daemon shares with every session it serves.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use iroh::endpoint::Connection;

use super::control;
#[cfg(feature = "test-hooks")]
use super::{EventLog, FaultInjector, SideHooks};

/// Close code for a connection whose session was taken over by a newer
/// connection resuming it.
pub const CLOSE_SUPERSEDED: u32 = 3;
/// How long a resuming connection waits for the session's previous host to stop.
const TAKEOVER_TIMEOUT: Duration = Duration::from_secs(30);

struct Hosted {
    conn: Connection,
    lock: Arc<tokio::sync::Mutex<()>>,
}

/// The sessions a daemon is hosting. A session id runs on at most one
/// connection: a resuming connection closes the previous one and waits until
/// its role has stopped (and persisted) before loading the session.
#[derive(Default)]
pub struct SessionRegistry {
    map: std::sync::Mutex<HashMap<[u8; 16], Hosted>>,
    /// Every id hosted so far, in order (test hook).
    #[cfg(feature = "test-hooks")]
    pub hosted: Arc<std::sync::Mutex<Vec<[u8; 16]>>>,
}

/// Holding this means the session runs on this connection only.
pub struct SessionClaim {
    registry: Arc<SessionRegistry>,
    id: [u8; 16],
    conn_id: usize,
    _guard: tokio::sync::OwnedMutexGuard<()>,
}

impl Drop for SessionClaim {
    fn drop(&mut self) {
        let mut map = self.registry.map.lock().unwrap_or_else(|e| e.into_inner());
        if map
            .get(&self.id)
            .is_some_and(|h| h.conn.stable_id() == self.conn_id)
        {
            map.remove(&self.id);
        }
    }
}

impl SessionRegistry {
    /// The ids of the sessions a connection hosts right now.
    pub fn live(&self) -> std::collections::HashSet<[u8; 16]> {
        let map = self.map.lock().unwrap_or_else(|e| e.into_inner());
        map.keys().copied().collect()
    }

    /// Makes `conn` the host of session `id`, closing any earlier host and
    /// waiting (bounded) until it has let go.
    pub async fn claim(
        self: &Arc<Self>,
        id: [u8; 16],
        conn: &Connection,
    ) -> anyhow::Result<SessionClaim> {
        let (previous, lock) = {
            let mut map = self.map.lock().unwrap_or_else(|e| e.into_inner());
            let lock = map.get(&id).map(|h| h.lock.clone()).unwrap_or_default();
            let previous = map.insert(
                id,
                Hosted {
                    conn: conn.clone(),
                    lock: lock.clone(),
                },
            );
            (previous.map(|h| h.conn), lock)
        };
        if let Some(old) = previous {
            tracing::info!(session = %control::hex(&id), "taking over the session from an older connection");
            old.close(CLOSE_SUPERSEDED.into(), b"superseded");
        }
        let guard = tokio::time::timeout(TAKEOVER_TIMEOUT, lock.lock_owned())
            .await
            .map_err(|_| anyhow::anyhow!("the session's previous connection did not stop"))?;
        #[cfg(feature = "test-hooks")]
        self.hosted
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(id);
        Ok(SessionClaim {
            registry: self.clone(),
            id,
            conn_id: conn.stable_id(),
            _guard: guard,
        })
    }
}

/// What the daemon shares with every session it serves.
#[derive(Clone, Default)]
pub struct DaemonSessions {
    pub registry: Arc<SessionRegistry>,
    /// How long an authenticated session may go without activity (default
    /// [`engine::DAEMON_IDLE_TIMEOUT`]; zero disables the limit).
    pub idle_timeout: Option<Duration>,
    /// How many sessions that no connection hosts one key may leave stored
    /// in an export's index; zero is no limit.
    /// Shared, so a reload reaches the sessions already running.
    pub max_dormant_sessions: Arc<std::sync::atomic::AtomicU32>,
    #[cfg(feature = "test-hooks")]
    pub fault: Option<Arc<FaultInjector>>,
    /// `NeedChunks` re-requests sent by daemon receivers (test hook).
    #[cfg(feature = "test-hooks")]
    pub rerequests: Arc<std::sync::atomic::AtomicU64>,
    /// The event log of every daemon receiver (test hook).
    #[cfg(feature = "test-hooks")]
    pub events: EventLog,
    /// Every daemon receiver's finalize takes this much longer (test hook,
    /// settable after the daemon is built).
    #[cfg(feature = "test-hooks")]
    pub slow_finalize: Arc<std::sync::Mutex<Option<Duration>>>,
    /// Streams a daemon sender runs at once, instead of the default (test
    /// hook; zero keeps the default). Bounded, a byte-count fault lands with
    /// some files complete and others still to come.
    #[cfg(feature = "test-hooks")]
    pub sender_parallel: Arc<std::sync::atomic::AtomicUsize>,
    /// The hooks every daemon session starts with: each session clones
    /// them, then takes `events` and `slow_finalize` from the fields above
    /// (test hook, settable after the daemon is built).
    #[cfg(feature = "test-hooks")]
    pub daemon_hooks: Arc<std::sync::Mutex<SideHooks>>,
}
