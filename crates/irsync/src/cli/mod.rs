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
    version = crate::build::LONG_VERSION.as_str(),
    about = "rsync-like directory sync over iroh",
    help_template = HELP_TEMPLATE,
    after_help = AFTER_HELP,
    propagate_version = true
)]
#[command(args_conflicts_with_subcommands = true, disable_help_subcommand = true)]
pub struct Args {
    #[command(subcommand)]
    pub cmd: Option<Command>,
    #[command(flatten)]
    pub sync: SyncArgs,
}

/// clap's default template with the build info on top; subcommands get it
/// in [`Args::command_tree`] since the template is not inherited.
const HELP_TEMPLATE: &str = "{name} {version}\n{about-with-newline}\n\
                             {usage-heading} {usage}\n\n{all-args}{after-help}";

/// Cross-cutting facts about a sync that no single flag's help can carry.
const AFTER_HELP: &str = "\
Notes:
  A push (local -> remote) always applies -lptgoD on the daemon's side,
  whatever the flags say; they matter on a pull.
  Filters apply on the sending side only, so a push with --delete and
  filters also needs --delete-excluded.
  Exit codes as rsync: 0 ok, 1 usage or connection error, 23 partial
  transfer, 25 deletions stopped at --max-delete.";

impl Args {
    /// The command with the help template applied to the subcommands too,
    /// at every level (`irsync host add --help` included).
    fn command_tree() -> clap::Command {
        fn apply(c: clap::Command) -> clap::Command {
            c.help_template(HELP_TEMPLATE).mut_subcommands(apply)
        }
        Args::command().mut_subcommands(apply)
    }

    /// Like `Parser::try_parse_from`, but also records the command-line order
    /// of the include/exclude options, which clap keeps per option.
    pub fn try_parse_from<I, T>(it: I) -> Result<Args, clap::Error>
    where
        I: IntoIterator<Item = T>,
        T: Into<OsString> + Clone,
    {
        let m = Args::command_tree().try_get_matches_from(it)?;
        let mut args = Args::from_arg_matches(&m)?;
        // One operand is a listing when it is remote (`irsync HOST:` or
        // `irsync HOST:PATH`); a lone local path is still a usage error.
        let listing = args.sync.paths.len() == 1
            && super::cli::location::Location::parse(&args.sync.paths[0])
                .is_ok_and(|l| l.is_remote());
        if args.cmd.is_none() && args.sync.paths.len() < 2 && !listing {
            return Err(Args::command().error(
                clap::error::ErrorKind::MissingRequiredArgument,
                "the following required arguments were not provided:\n  <SRC>... <DEST>",
            ));
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
    ///
    /// Serves the exports in exports.toml to the peers they are granted to
    /// until interrupted, and prints its EndpointId at start. Logs go to
    /// stderr at info level (-v: debug, -vv: trace, -q: errors only;
    /// RUST_LOG overrides).
    Daemon(DaemonArgs),
    /// Garbage-collect resume state.
    ///
    /// Removes abandoned partial files and dormant sessions older than 7
    /// days under every local root irsync has synced, and prints the paths
    /// removed and a count. A sync does the same for its own root at most
    /// daily.
    Gc {
        /// List what would be removed without removing anything.
        #[arg(short = 'n', long)]
        dry_run: bool,
        #[command(flatten)]
        log: LogArgs,
    },
    /// Hash the files under local roots into their index, so a
    /// destination irsync did not create contributes chunks and rename
    /// candidates from the first session on (`export update` does the
    /// same for the daemon's exports).
    Index(IndexArgs),
    /// Manage host aliases (edits irsync.toml).
    Host(HostArgs),
    /// Manage exports (edits irsync.toml).
    Export(ExportArgs),
    /// Manage grants: who may use which export (edits irsync.toml).
    Grant(GrantArgs),
    /// This machine's identities (the client's and the daemon's).
    Id(IdArgs),
    /// Show the config files, identities, host aliases and exports.
    Show {
        /// Read an encrypted identity's passphrase from the first line of
        /// this file (else IRSYNC_PASSPHRASE, else a prompt on a terminal).
        #[arg(long, value_name = "PATH")]
        passphrase_file: Option<PathBuf>,
        #[command(flatten)]
        files: FilesArgs,
    },
}

#[derive(Debug, clap::Args)]
pub struct IdArgs {
    #[command(subcommand)]
    pub cmd: IdCmd,
    #[command(flatten)]
    pub files: FilesArgs,
}

#[derive(Debug, Subcommand)]
pub enum IdCmd {
    /// Print the public key line and EndpointId of the client (default),
    /// the daemon, or both; an identity is generated if it does not exist.
    Show {
        /// The daemon's identity instead of the client's.
        #[arg(long, conflicts_with = "all")]
        daemon: bool,
        /// Both identities, labelled `client:` and `daemon:`.
        #[arg(short = 'a', long)]
        all: bool,
        /// Read an encrypted identity's passphrase from the first line of
        /// this file (else IRSYNC_PASSPHRASE, else a prompt on a terminal).
        #[arg(long, value_name = "PATH")]
        passphrase_file: Option<PathBuf>,
    },
    /// Generate an Ed25519 identity and print its public key line and
    /// EndpointId. Refuses to overwrite an existing key or .pub file
    /// unless --force is given.
    Generate {
        /// The daemon's identity (its configured path) instead of the
        /// client's.
        #[arg(long, conflicts_with = "out")]
        daemon: bool,
        /// Write the private key here instead of the configured path; the
        /// .pub file goes beside it.
        #[arg(long, value_name = "PATH")]
        out: Option<PathBuf>,
        /// Replace an existing key and .pub file. The old key is gone for
        /// good: every grant made for it stops working.
        #[arg(short, long)]
        force: bool,
    },
}

#[derive(Debug, clap::Args)]
pub struct HostArgs {
    #[command(subcommand)]
    pub cmd: HostCmd,
    #[command(flatten)]
    pub files: FilesArgs,
}

#[derive(Debug, Subcommand)]
pub enum HostCmd {
    /// Add a host alias, or update the keys given for an existing one.
    Add(HostAddArgs),
    /// Remove a host alias.
    Remove {
        /// The alias.
        alias: String,
    },
    /// List host aliases.
    List,
}

#[derive(Debug, clap::Args)]
pub struct ExportArgs {
    #[command(subcommand)]
    pub cmd: ExportCmd,
    #[command(flatten)]
    pub files: FilesArgs,
}

#[derive(Debug, Subcommand)]
pub enum ExportCmd {
    /// Export a directory from this machine's daemon and print the
    /// daemon's id.
    Add(ExportAddArgs),
    /// Stop exporting a directory, with its grants.
    Remove {
        /// The export's name.
        name: String,
    },
    /// List the exports with their paths, flags and grants.
    List,
    /// Hash the exports' files into their indexes under the daemon's
    /// state directory, so files irsync did not place are reused from
    /// the next session on (what `warm_index` does at every start).
    Update {
        /// Walk and count what would be hashed; read nothing.
        #[arg(short = 'n', long)]
        dry_run: bool,
        #[command(flatten)]
        log: LogArgs,
        /// Export names, or `all` for every export.
        #[arg(value_name = "NAME", required = true)]
        names: Vec<String>,
    },
}

#[derive(Debug, clap::Args)]
pub struct GrantArgs {
    #[command(subcommand)]
    pub cmd: GrantCmd,
    #[command(flatten)]
    pub files: FilesArgs,
}

#[derive(Debug, Subcommand)]
pub enum GrantCmd {
    /// Grant a peer access to an export (`all`: every export).
    Add {
        /// The export's name, or `all` for every export.
        export: String,
        /// An alias from hosts.toml, an EndpointId, a public key line or a
        /// .pub file.
        who: String,
        /// Read-only access (default: read-write).
        #[arg(long)]
        ro: bool,
    },
    /// Remove grants: of one peer or of every peer, on one export or on
    /// every export.
    Remove {
        /// The export's name, or `all` for every export.
        export: String,
        /// An alias, an EndpointId, a public key line or a .pub file; or
        /// `all` for every peer.
        who: String,
    },
    /// List grants: all of them, one export's, or what one peer may reach.
    List {
        /// An export's name (`all`: the every-export grants), or a peer (an
        /// alias or an EndpointId); nothing lists every grant.
        target: Option<String>,
    },
}

#[derive(Debug, clap::Args)]
pub struct HostAddArgs {
    /// The alias (no ':' or '/', not an EndpointId).
    pub alias: String,
    /// The host's EndpointId.
    pub id: String,
    /// The host's home relay, used instead of the client settings' or n0's
    /// default.
    #[arg(long, value_name = "URL")]
    pub relay: Option<String>,
    /// Dial the host at this `ip:port`, `[ipv6]:port` or `host:port`
    /// directly, with relay and lookups off (repeatable; replaces the
    /// stored list).
    #[arg(long, value_name = "ADDR")]
    pub address: Vec<HostPort>,
    /// Files in flight at once when syncing with this host (default: the
    /// client settings', else 32).
    #[arg(long, value_name = "N")]
    pub parallel: Option<u32>,
    /// How long a sync with this host keeps redialling after a dropped
    /// connection: `<N>[s|m|h|d]`, e.g. `1h`; 0 gives up at once.
    #[arg(long, value_name = "DUR", value_parser = validate_duration)]
    pub reconnect: Option<String>,
    /// Look this host up in the mainline DHT (default: the client
    /// settings', else yes).
    #[arg(long, value_name = "yes|no", value_parser = parse_yes_no)]
    pub dht: Option<bool>,
    /// Look this host up with mDNS on the local network (default: the
    /// client settings', else yes).
    #[arg(long, value_name = "yes|no", value_parser = parse_yes_no)]
    pub mdns: Option<bool>,
}

/// Accepts a duration the config files accept; keeps the user's text.
fn validate_duration(s: &str) -> Result<String, String> {
    duration::parse(s)
        .map(|_| s.to_string())
        .map_err(|e| e.to_string())
}

fn parse_yes_no(s: &str) -> Result<bool, String> {
    match s.to_ascii_lowercase().as_str() {
        "yes" | "true" => Ok(true),
        "no" | "false" => Ok(false),
        _ => Err(format!("expected yes or no, got {s:?}")),
    }
}

/// How much the commands that log (`daemon`, `gc`, `index`) say on stderr.
#[derive(Debug, Clone, Copy, Default, clap::Args)]
pub struct LogArgs {
    /// Log more; repeat for more still (up to -vvv).
    #[arg(short, long, action = ArgAction::Count, conflicts_with = "quiet")]
    pub verbose: u8,
    /// Log errors only, and print no summary.
    #[arg(short, long)]
    pub quiet: bool,
}

/// The config files the management commands read and edit. Global, so a
/// noun accepts them before or after its verb.
#[derive(Debug, clap::Args)]
pub struct FilesArgs {
    /// The config file (default: irsync.toml in the config directory). A
    /// hosts.toml or exports.toml beside it is read too, but never edited.
    #[arg(long, global = true, value_name = "PATH")]
    pub config: Option<PathBuf>,
}

#[derive(Debug, clap::Args)]
pub struct ExportAddArgs {
    /// `NAME:PATH`, or a directory named by its final component.
    pub target: String,
    /// Export read-only.
    #[arg(long)]
    pub ro: bool,
    /// Hash the export's files into its index whenever the daemon starts,
    /// so files irsync did not place are reused from the first session on.
    #[arg(long)]
    pub warm_index: bool,
}

#[derive(Debug, clap::Args)]
pub struct IndexArgs {
    /// Walk and count what would be hashed; read nothing.
    #[arg(short = 'n', long)]
    pub dry_run: bool,
    #[command(flatten)]
    pub log: LogArgs,
    /// Local roots to index.
    #[arg(value_name = "ROOT", required = true)]
    pub targets: Vec<PathBuf>,
}

#[derive(Debug, clap::Args)]
pub struct DaemonArgs {
    /// The config file (default: irsync.toml in the config directory). A
    /// hosts.toml or exports.toml beside it is read too.
    #[arg(long, value_name = "PATH")]
    pub config: Option<PathBuf>,
    /// Read the identity's passphrase from the first line of this file
    /// (else IRSYNC_PASSPHRASE, else a prompt on a terminal).
    #[arg(long, value_name = "PATH")]
    pub passphrase_file: Option<PathBuf>,
    /// Bind this socket (`ip:port`, `[ipv6]:port` or `host:port`; port 0 =
    /// pick one) instead of the config's `listen` list. Repeatable, at most
    /// one IPv4 and one IPv6.
    #[arg(long, value_name = "ADDR")]
    pub listen: Vec<HostPort>,
    #[command(flatten)]
    pub log: LogArgs,
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
    /// Local paths or `HOST:EXPORT/PATH` (HOST: an alias from hosts.toml or
    /// an EndpointId; EXPORT: a directory the daemon shares by name).
    /// Exactly one side is remote; several sources only when pushing. A
    /// trailing `/` copies a directory's contents, none the directory
    /// itself.
    #[arg(value_name = "SRC... DEST")]
    pub paths: Vec<String>,

    /// Archive mode: -rlptgoD (symlinks, permissions, times, group, owner,
    /// devices and specials).
    #[arg(short, long)]
    pub archive: bool,
    /// Accepted for rsync compatibility: irsync always recurses into
    /// directories, with or without -r/-a.
    #[arg(short = 'r', long = "recursive")]
    pub recursive: bool,
    /// Copy symlinks as symlinks (without it they are skipped).
    #[arg(short = 'l', long = "links")]
    pub links: bool,
    /// Preserve permissions.
    #[arg(short = 'p', long = "perms")]
    pub perms: bool,
    /// Preserve modification times.
    #[arg(short = 't', long = "times")]
    pub times: bool,
    /// Preserve the group, where the receiving user may set it.
    #[arg(short = 'g', long = "group")]
    pub group: bool,
    /// Preserve the owner (only when the receiving side runs as root).
    #[arg(short = 'o', long = "owner")]
    pub owner: bool,
    /// Same as --devices --specials.
    #[arg(short = 'D')]
    pub devices_and_specials: bool,
    /// Preserve character and block device nodes.
    #[arg(long)]
    pub devices: bool,
    /// Preserve special files, that is FIFOs (sockets are never copied).
    #[arg(long)]
    pub specials: bool,
    /// Preserve hard links: files linked together in the source are linked
    /// together in the destination.
    #[arg(short = 'H', long = "hard-links")]
    pub hard_links: bool,
    /// Write every run of zeros as a hole; without it only files that are
    /// sparse in the source are written sparse.
    #[arg(short = 'S', long = "sparse")]
    pub sparse: bool,
    /// List each file as it is handled, and log more per repeat (-vv:
    /// debug, -vvv: trace).
    #[arg(short, long, action = ArgAction::Count)]
    pub verbose: u8,
    /// Print errors only; also turns the live progress block off.
    #[arg(short, long)]
    pub quiet: bool,
    /// Change nothing on either side; with -v or -i, list what would be
    /// done.
    #[arg(short = 'n', long)]
    pub dry_run: bool,
    /// Compare by content, not by size and time: every file on both sides
    /// is read, no cached hash is trusted, and the index is refreshed from
    /// what was read.
    #[arg(short, long)]
    pub checksum: bool,
    /// Skip files that are newer on the destination.
    #[arg(short, long = "update")]
    pub update: bool,
    /// Delete destination files that are gone from the source, after the
    /// transfer (same as --delete-after). Nothing is deleted without one
    /// of the --delete flags.
    #[arg(long)]
    pub delete: bool,
    /// Delete before the transfer starts.
    #[arg(long)]
    pub delete_before: bool,
    /// Accepted for rsync compatibility; irsync runs it like --delete-after.
    #[arg(long)]
    pub delete_during: bool,
    /// Delete after the transfer (what --delete does).
    #[arg(long)]
    pub delete_after: bool,
    /// Also delete destination files the filters exclude (implies --delete;
    /// required for a push with --delete and filters).
    #[arg(long)]
    pub delete_excluded: bool,
    /// Stop deleting after N files; exit code 25 when the cap stops a run.
    #[arg(long, value_name = "N")]
    pub max_delete: Option<u64>,
    /// Exclude paths matching this rsync pattern (`*`, `**`, `?`, `[..]`; a
    /// leading `/` anchors it to the root, a trailing `/` matches
    /// directories only). Rules apply in command-line order; the first
    /// match wins.
    #[arg(long, value_name = "PATTERN", action = ArgAction::Append)]
    pub exclude: Vec<String>,
    /// Include paths matching this pattern even if a later --exclude would
    /// hide them.
    #[arg(long, value_name = "PATTERN", action = ArgAction::Append)]
    pub include: Vec<String>,
    /// Read exclude patterns from FILE, one per line (blank lines and lines
    /// starting with `#` or `;` are skipped).
    #[arg(long, value_name = "FILE", action = ArgAction::Append)]
    pub exclude_from: Vec<String>,
    /// Read include patterns from FILE, one per line, as --exclude-from.
    #[arg(long, value_name = "FILE", action = ArgAction::Append)]
    pub include_from: Vec<String>,
    /// Bytes/s limit: KiB/s by default, or with k/m/g suffix; 0 = unlimited.
    #[arg(long, value_name = "RATE", value_parser = parse_bwlimit)]
    pub bwlimit: Option<u64>,
    /// Accepted for rsync compatibility: owners and groups are always
    /// transferred as numbers, never mapped by name.
    #[arg(long)]
    pub numeric_ids: bool,
    /// Keep the partial files of interrupted transfers under DIR (relative
    /// to the destination root, or absolute) instead of beside their
    /// files. Pull only; a push with it is refused.
    #[arg(long, value_name = "DIR")]
    pub partial_dir: Option<PathBuf>,
    /// Run the deletion pass even when a listing was cut short by I/O
    /// errors (by default it is skipped then).
    #[arg(long)]
    pub ignore_errors: bool,
    /// Print a block of transfer statistics at the end.
    #[arg(long)]
    pub stats: bool,
    /// Accepted for rsync compatibility: the live progress block is always
    /// drawn when stdout is a terminal, and off with -q.
    #[arg(long)]
    pub progress: bool,
    /// Accepted for rsync compatibility (e.g. `progress2`); see --progress.
    #[arg(long, value_name = "SPEC")]
    pub info: Option<String>,
    /// Print rsync's per-file change summary (the itemize codes).
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
    /// How long to keep redialling after a dropped connection:
    /// `<N>[s|m|h|d]`; 0 gives up at once like rsync (default: `reconnect`
    /// from the host entry or the client settings, else 24h).
    #[arg(long, value_name = "DUR", value_parser = duration::parse)]
    pub reconnect: Option<Duration>,
    /// Use this relay server instead of the host entry's, the client
    /// settings' or n0's default.
    #[arg(long, value_name = "URL")]
    pub relay: Option<String>,
    /// Do not look the daemon up in the mainline DHT.
    #[arg(long)]
    pub no_dht: bool,
    /// Dial the daemon at this `ip:port`, `[ipv6]:port` or `host:port`
    /// directly (repeatable). Turns relay, DNS, DHT, mDNS and the port
    /// mapper off; the daemon's EndpointId is still required.
    #[arg(long, value_name = "ADDR", conflicts_with = "relay")]
    pub address: Vec<HostPort>,
    /// The config file (default: irsync.toml in the config directory). A
    /// hosts.toml beside it is read too.
    #[arg(long, value_name = "PATH")]
    pub config: Option<PathBuf>,
    /// The private key to connect with (default: `identity` in irsync.toml,
    /// else id_ed25519 in the config directory; generated if missing).
    #[arg(long, value_name = "FILE")]
    pub identity: Option<PathBuf>,
    /// Read the identity's passphrase from the first line of this file
    /// (else IRSYNC_PASSPHRASE, else a prompt on a terminal).
    #[arg(long, value_name = "PATH")]
    pub passphrase_file: Option<PathBuf>,
    /// Start a fresh plan instead of resuming a dormant session (partial
    /// files are still picked up).
    #[arg(long)]
    pub no_resume: bool,
    /// Do not fill a new file from a destination file with the same
    /// content; a moved or copied file is transferred again.
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
            devices: a.archive || a.devices_and_specials || a.devices,
            specials: a.archive || a.devices_and_specials || a.specials,
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
