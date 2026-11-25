//! What a sync is asked to do, as the engine reads it. The command line
//! is read into these in [`crate::cli`]; the session, the planner, the
//! filters and the deletion pass take them from here.

use crate::config::settings::PqKeyExchange;
use crate::net::addr::HostPort;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

/// Which side sends the data.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    ClientSends,
    DaemonSends,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FilterRule {
    Include(String),
    Exclude(String),
    IncludeFrom(PathBuf),
    ExcludeFrom(PathBuf),
}

/// `--append` / `--append-verify`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Append {
    #[default]
    Off,
    /// The destination's existing prefix is trusted.
    Append,
    /// The prefix is verified too: a delta, with the skip rule.
    Verify,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeleteMode {
    Off,
    Before,
    During,
    After,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Preserve {
    pub recursive: bool,
    /// Without recursion: directories themselves are transferred (`-d`).
    pub dirs: bool,
    pub links: bool,
    pub perms: bool,
    pub times: bool,
    pub group: bool,
    pub owner: bool,
    /// Character and block device nodes.
    pub devices: bool,
    /// FIFOs (and sockets, which are never copied).
    pub specials: bool,
    pub hard_links: bool,
    pub sparse: bool,
}

#[derive(Debug, Clone)]
pub struct SyncOptions {
    pub preserve: Preserve,
    pub delete: DeleteMode,
    pub delete_excluded: bool,
    pub max_delete: Option<u64>,
    pub dry_run: bool,
    pub checksum: bool,
    pub update_only: bool,
    pub filters: Vec<FilterRule>,
    pub bwlimit: Option<u64>,
    pub numeric_ids: bool,
    pub partial_dir: Option<PathBuf>,
    pub ignore_errors: bool,
    pub stats: bool,
    /// `--progress`, `-P` or `--info=progress1|2`.
    pub progress: bool,
    pub itemize: bool,
    /// `--idle-timeout`; after the dial is resolved, the timeout in force
    /// (`None`: none).
    pub idle_timeout: Option<Duration>,
    pub parallel: Option<u32>,
    pub reconnect: Option<Duration>,
    pub dial_retries: Option<u32>,
    pub relay: Option<crate::config::settings::Relay>,
    pub pq_key_exchange: Option<PqKeyExchange>,
    pub no_dht: bool,
    pub no_dns: bool,
    pub no_mdns: bool,
    pub addresses: Vec<HostPort>,
    pub config: Option<PathBuf>,
    /// `state_dir` from the config file, once a sync has read it; `None`:
    /// the default.
    pub state_dir: Option<PathBuf>,
    pub identity: Option<PathBuf>,
    pub passphrase_file: Option<PathBuf>,
    pub no_resume: bool,
    pub no_rename_detection: bool,
    pub inplace: bool,
    pub fsync: bool,
    /// `--paranoid`: the sending side hashes each sent file again before
    /// it is placed.
    pub paranoid: bool,
    pub append: Append,
    pub debug_addr: Option<SocketAddr>,
    pub debug_no_wait: bool,
    pub verbose: u8,
    pub quiet: bool,
}

impl SyncOptions {
    /// These options with deletion off: a single-file copy never deletes
    /// anything, since nothing else in the file's directory is part of it.
    /// The client asks for a single-file session with these; the daemon
    /// enforces the same rule on its side.
    pub fn without_deletion(&self) -> SyncOptions {
        SyncOptions {
            delete: DeleteMode::Off,
            delete_excluded: false,
            max_delete: None,
            ..self.clone()
        }
    }
}
