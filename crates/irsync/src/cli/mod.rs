pub mod location;
pub mod progress;

use crate::config::duration;
use crate::net::addr::HostPort;
use clap::{ArgAction, CommandFactory, FromArgMatches, Parser, Subcommand};
use location::Location;
use std::ffi::OsString;
use std::path::PathBuf;
use std::time::Duration;

#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct UsageError(pub String);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    ClientSends,
    DaemonSends,
}

/// Exactly one side must be remote; several sources only for local -> remote.
pub fn validate(src: &[Location], dest: &Location) -> Result<Direction, UsageError> {
    if src.is_empty() {
        return Err(UsageError("missing source argument".into()));
    }
    let remote_srcs = src.iter().filter(|l| l.is_remote()).count();
    match (remote_srcs, dest.is_remote()) {
        (0, true) => Ok(Direction::ClientSends),
        (0, false) => Err(UsageError(
            "neither source nor destination is remote; irsync needs one remote side".into(),
        )),
        (_, true) => Err(UsageError(
            "source and destination cannot both be remote".into(),
        )),
        (1, false) if src.len() == 1 => Ok(Direction::DaemonSends),
        _ => Err(UsageError(
            "multiple sources are only supported from local paths to a remote destination".into(),
        )),
    }
}

#[derive(Debug, Parser)]
#[command(
    name = "irsync",
    version,
    about = "rsync-like directory sync over iroh"
)]
#[command(args_conflicts_with_subcommands = true, disable_help_subcommand = true)]
pub struct Args {
    #[command(subcommand)]
    pub cmd: Option<Command>,
    #[command(flatten)]
    pub sync: SyncArgs,
}

impl Args {
    /// Like `Parser::try_parse_from`, but also records the command-line order
    /// of the include/exclude options, which clap keeps per option.
    pub fn try_parse_from<I, T>(it: I) -> Result<Args, clap::Error>
    where
        I: IntoIterator<Item = T>,
        T: Into<OsString> + Clone,
    {
        let m = Args::command().try_get_matches_from(it)?;
        let mut args = Args::from_arg_matches(&m)?;
        if args.cmd.is_none() && (args.sync.paths.len() < 2) {
            return Err(Args::command().error(
                clap::error::ErrorKind::MissingRequiredArgument,
                "the following required arguments were not provided:\n  <SRC>... <DEST>",
            ));
        }
        if let Some(Command::Index(ix)) = &args.cmd {
            if !ix.daemon && ix.targets.is_empty() {
                return Err(Args::command().error(
                    clap::error::ErrorKind::MissingRequiredArgument,
                    "the following required arguments were not provided:\n  <TARGET>... (or --daemon)",
                ));
            }
        }
        args.sync.filters = ordered_filters(&m);
        Ok(args)
    }

    pub fn parse() -> Args {
        Args::try_parse_from(std::env::args_os()).unwrap_or_else(|e| e.exit())
    }
}

fn ordered_filters(m: &clap::ArgMatches) -> Vec<FilterRule> {
    let mut v: Vec<(usize, FilterRule)> = Vec::new();
    let mut collect = |id: &str, mk: fn(String) -> FilterRule| {
        if let (Some(vals), Some(idx)) = (m.get_many::<String>(id), m.indices_of(id)) {
            v.extend(idx.zip(vals).map(|(i, s)| (i, mk(s.clone()))));
        }
    };
    collect("exclude", FilterRule::Exclude);
    collect("include", FilterRule::Include);
    collect("exclude_from", |s| FilterRule::ExcludeFrom(s.into()));
    collect("include_from", |s| FilterRule::IncludeFrom(s.into()));
    v.sort_by_key(|(i, _)| *i);
    v.into_iter().map(|(_, r)| r).collect()
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Run the daemon.
    Daemon(DaemonArgs),
    /// Generate an Ed25519 identity.
    Keygen {
        #[arg(long)]
        out: Option<PathBuf>,
    },
    /// Garbage-collect resume state.
    Gc {
        #[arg(short = 'n', long)]
        dry_run: bool,
    },
    /// Hash the files under local roots (or daemon exports) into their
    /// index, so a destination irsync did not create contributes chunks
    /// and rename candidates from the first session on.
    Index(IndexArgs),
    /// Export a directory from this machine's daemon (edits exports.toml)
    /// and print the daemon's id.
    Add(AddArgs),
    /// Stop exporting a directory (edits exports.toml).
    Remove {
        /// The export's name.
        name: String,
        #[command(flatten)]
        files: FilesArgs,
    },
    /// Grant a peer access to an export (edits exports.toml).
    Allow {
        /// The export's name (`all`: the top level, every export).
        export: String,
        /// An alias from hosts.toml, an EndpointId, a public key line or a
        /// .pub file.
        who: String,
        /// Read-only access (default: read-write).
        #[arg(long)]
        ro: bool,
        #[command(flatten)]
        files: FilesArgs,
    },
    /// Remove grants: `revoke WHO`, `revoke EXPORT WHO` or `revoke EXPORT all`
    /// (`all` in place of EXPORT means everywhere, of WHO every peer).
    Revoke {
        #[arg(required = true, num_args = 1..=2, value_name = "ARG")]
        args: Vec<String>,
        #[command(flatten)]
        files: FilesArgs,
    },
}

/// The config files the management commands read and edit.
#[derive(Debug, clap::Args)]
pub struct FilesArgs {
    /// The settings file (default: irsync.toml in the config directory).
    #[arg(long, value_name = "PATH")]
    pub config: Option<PathBuf>,
    /// The host aliases file (default: hosts.toml in the config directory).
    #[arg(long, value_name = "PATH")]
    pub hosts: Option<PathBuf>,
    /// The exports file (default: exports.toml in the config directory).
    #[arg(long, value_name = "PATH")]
    pub exports: Option<PathBuf>,
}

#[derive(Debug, clap::Args)]
pub struct AddArgs {
    /// `NAME:PATH`, or a directory named by its final component.
    pub target: String,
    /// Export read-only.
    #[arg(long)]
    pub ro: bool,
    /// Keep the export's index warm in the background.
    #[arg(long)]
    pub warm_index: bool,
    #[command(flatten)]
    pub files: FilesArgs,
}

#[derive(Debug, clap::Args)]
pub struct IndexArgs {
    /// Index daemon exports under the daemon's state directory: the
    /// targets are export names, all exports when none is given.
    #[arg(long)]
    pub daemon: bool,
    /// The settings file (with `--daemon`).
    #[arg(long, requires = "daemon", value_name = "PATH")]
    pub config: Option<PathBuf>,
    /// The exports file (with `--daemon`; default: exports.toml in the
    /// config directory).
    #[arg(long, requires = "daemon", value_name = "PATH")]
    pub exports: Option<PathBuf>,
    /// Walk and count what would be hashed; read nothing.
    #[arg(short = 'n', long)]
    pub dry_run: bool,
    /// Local roots to index, or with `--daemon` export names.
    #[arg(value_name = "TARGET")]
    pub targets: Vec<PathBuf>,
}

#[derive(Debug, clap::Args)]
pub struct DaemonArgs {
    #[arg(long)]
    pub config: Option<PathBuf>,
    /// The host aliases file (default: hosts.toml in the config directory).
    #[arg(long, value_name = "PATH")]
    pub hosts: Option<PathBuf>,
    /// The exports file (default: exports.toml in the config directory).
    #[arg(long, value_name = "PATH")]
    pub exports: Option<PathBuf>,
    #[arg(long)]
    pub show_id: bool,
    /// Read the identity's passphrase from the first line of this file
    /// (else IRSYNC_PASSPHRASE, else a prompt on a terminal).
    #[arg(long, value_name = "PATH")]
    pub passphrase_file: Option<PathBuf>,
    /// Bind this socket (`ip:port`, `[ipv6]:port` or `host:port`; port 0 =
    /// pick one) instead of the config's `listen` list. Repeatable, at most
    /// one IPv4 and one IPv6.
    #[arg(long, value_name = "ADDR")]
    pub listen: Vec<HostPort>,
}

fn parse_bwlimit(s: &str) -> Result<u64, String> {
    let bad = || format!("invalid rate {s:?}");
    let lower = s.to_ascii_lowercase();
    let t = lower.strip_suffix('b').unwrap_or(&lower);
    let (digits, mult) = match t.chars().last() {
        Some('k') => (&t[..t.len() - 1], 1024u64),
        Some('m') => (&t[..t.len() - 1], 1024 * 1024),
        Some('g') => (&t[..t.len() - 1], 1024 * 1024 * 1024),
        _ => (t, 1024),
    };
    let n: u64 = digits.parse().map_err(|_| bad())?;
    n.checked_mul(mult).ok_or_else(bad)
}

#[derive(Debug, clap::Args)]
pub struct SyncArgs {
    /// `SRC... DEST`
    #[arg(value_name = "SRC... DEST")]
    pub paths: Vec<String>,

    #[arg(short, long)]
    pub archive: bool,
    /// Accepted for rsync compatibility: irsync always recurses into
    /// directories, with or without -r/-a.
    #[arg(short = 'r', long = "recursive")]
    pub recursive: bool,
    #[arg(short = 'l', long = "links")]
    pub links: bool,
    #[arg(short = 'p', long = "perms")]
    pub perms: bool,
    #[arg(short = 't', long = "times")]
    pub times: bool,
    #[arg(short = 'g', long = "group")]
    pub group: bool,
    #[arg(short = 'o', long = "owner")]
    pub owner: bool,
    #[arg(short = 'D')]
    pub devices: bool,
    #[arg(short = 'H', long = "hard-links")]
    pub hard_links: bool,
    #[arg(short = 'S', long = "sparse")]
    pub sparse: bool,
    #[arg(short, long, action = ArgAction::Count)]
    pub verbose: u8,
    #[arg(short, long)]
    pub quiet: bool,
    #[arg(short = 'n', long)]
    pub dry_run: bool,
    #[arg(short, long)]
    pub checksum: bool,
    #[arg(short, long = "update")]
    pub update: bool,
    #[arg(long)]
    pub delete: bool,
    #[arg(long)]
    pub delete_before: bool,
    /// Accepted for rsync compatibility; irsync runs it like --delete-after.
    #[arg(long)]
    pub delete_during: bool,
    #[arg(long)]
    pub delete_after: bool,
    #[arg(long)]
    pub delete_excluded: bool,
    #[arg(long, value_name = "N")]
    pub max_delete: Option<u64>,
    #[arg(long, value_name = "PATTERN", action = ArgAction::Append)]
    pub exclude: Vec<String>,
    #[arg(long, value_name = "PATTERN", action = ArgAction::Append)]
    pub include: Vec<String>,
    #[arg(long, value_name = "FILE", action = ArgAction::Append)]
    pub exclude_from: Vec<String>,
    #[arg(long, value_name = "FILE", action = ArgAction::Append)]
    pub include_from: Vec<String>,
    /// Bytes/s limit: KiB/s by default, or with k/m/g suffix; 0 = unlimited.
    #[arg(long, value_name = "RATE", value_parser = parse_bwlimit)]
    pub bwlimit: Option<u64>,
    #[arg(long)]
    pub numeric_ids: bool,
    #[arg(long, value_name = "DIR")]
    pub partial_dir: Option<PathBuf>,
    #[arg(long)]
    pub ignore_errors: bool,
    #[arg(long)]
    pub stats: bool,
    #[arg(long)]
    pub progress: bool,
    #[arg(long, value_name = "SPEC")]
    pub info: Option<String>,
    #[arg(short = 'i', long)]
    pub itemize_changes: bool,
    /// Inactivity timeout: a connection that moves no data and no control
    /// message for this long is dropped and the session reconnects.
    #[arg(long, value_name = "SECS")]
    pub timeout: Option<u64>,
    /// Files in flight at once (default: `parallel` from the host entry or
    /// the client settings, else 32).
    #[arg(long, value_name = "N")]
    pub parallel: Option<u32>,
    #[arg(long, value_name = "DUR", value_parser = duration::parse)]
    pub reconnect: Option<Duration>,
    #[arg(long, value_name = "URL")]
    pub relay: Option<String>,
    #[arg(long)]
    pub no_dht: bool,
    /// Dial the daemon at this `ip:port`, `[ipv6]:port` or `host:port`
    /// directly (repeatable). Turns relay, DNS, DHT, mDNS and the port
    /// mapper off; the daemon's EndpointId is still required.
    #[arg(long, value_name = "ADDR", conflicts_with = "relay")]
    pub address: Vec<HostPort>,
    /// The settings file (default: irsync.toml in the config directory).
    #[arg(long, value_name = "PATH")]
    pub config: Option<PathBuf>,
    /// The host aliases file (default: hosts.toml in the config directory).
    #[arg(long, value_name = "PATH")]
    pub hosts: Option<PathBuf>,
    #[arg(long, value_name = "FILE")]
    pub identity: Option<PathBuf>,
    /// Read the identity's passphrase from the first line of this file
    /// (else IRSYNC_PASSPHRASE, else a prompt on a terminal).
    #[arg(long, value_name = "PATH")]
    pub passphrase_file: Option<PathBuf>,
    #[arg(long)]
    pub no_resume: bool,
    #[arg(long)]
    pub no_rename_detection: bool,
    /// Write a changed file into the destination itself instead of a
    /// `.part` renamed over it: no second copy on disk, but readers see a
    /// torn file during the transfer and a crash leaves a mixture.
    #[arg(long)]
    pub inplace: bool,
    /// Extend a destination file that is shorter than the source by the
    /// source's tail, trusting what is already there; a destination that
    /// is not shorter is skipped (rsync's rule).
    #[arg(long, conflicts_with = "append_verify")]
    pub append: bool,
    /// Like --append, but the existing prefix is verified too (a delta
    /// transfer with --append's skip rule).
    #[arg(long)]
    pub append_verify: bool,

    /// Include/exclude rules in command-line order (filled after parsing).
    #[arg(skip)]
    pub filters: Vec<FilterRule>,
}

impl SyncArgs {
    /// All positionals but the last.
    pub fn src(&self) -> &[String] {
        self.paths.split_last().map_or(&[], |(_, s)| s)
    }

    pub fn dest(&self) -> Option<&str> {
        self.paths.last().map(String::as_str)
    }
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
    pub links: bool,
    pub perms: bool,
    pub times: bool,
    pub group: bool,
    pub owner: bool,
    pub devices: bool,
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
    pub progress: bool,
    pub info: Option<String>,
    pub itemize: bool,
    pub timeout: Option<Duration>,
    pub parallel: Option<u32>,
    pub reconnect: Option<Duration>,
    pub relay: Option<String>,
    pub no_dht: bool,
    pub addresses: Vec<HostPort>,
    pub config: Option<PathBuf>,
    pub hosts: Option<PathBuf>,
    pub identity: Option<PathBuf>,
    pub passphrase_file: Option<PathBuf>,
    pub no_resume: bool,
    pub no_rename_detection: bool,
    pub inplace: bool,
    pub append: Append,
    pub verbose: u8,
    pub quiet: bool,
}

impl SyncOptions {
    pub fn from_args(args: &Args) -> SyncOptions {
        let a = &args.sync;
        let preserve = Preserve {
            // Always: irsync syncs directory trees (-r is implied).
            recursive: true,
            links: a.archive || a.links,
            perms: a.archive || a.perms,
            times: a.archive || a.times,
            group: a.archive || a.group,
            owner: a.archive || a.owner,
            devices: a.archive || a.devices,
            hard_links: a.hard_links,
            sparse: a.sparse,
        };
        let delete = if a.delete_before {
            DeleteMode::Before
        } else if a.delete_during {
            DeleteMode::During
        } else if a.delete_after || a.delete || a.delete_excluded {
            DeleteMode::After
        } else {
            DeleteMode::Off
        };
        SyncOptions {
            preserve,
            delete,
            delete_excluded: a.delete_excluded,
            max_delete: a.max_delete,
            dry_run: a.dry_run,
            checksum: a.checksum,
            update_only: a.update,
            filters: a.filters.clone(),
            bwlimit: a.bwlimit.filter(|&b| b != 0),
            // Ids are always numeric in v1; --numeric-ids is accepted as a no-op.
            numeric_ids: true,
            partial_dir: a.partial_dir.clone(),
            ignore_errors: a.ignore_errors,
            stats: a.stats,
            progress: a.progress,
            info: a.info.clone(),
            itemize: a.itemize_changes,
            timeout: a.timeout.map(Duration::from_secs),
            parallel: a.parallel,
            reconnect: a.reconnect,
            relay: a.relay.clone(),
            no_dht: a.no_dht,
            addresses: a.address.clone(),
            config: a.config.clone(),
            hosts: a.hosts.clone(),
            identity: a.identity.clone(),
            passphrase_file: a.passphrase_file.clone(),
            no_resume: a.no_resume,
            no_rename_detection: a.no_rename_detection,
            inplace: a.inplace,
            append: if a.append {
                Append::Append
            } else if a.append_verify {
                Append::Verify
            } else {
                Append::Off
            },
            verbose: a.verbose,
            quiet: a.quiet,
        }
    }
}
