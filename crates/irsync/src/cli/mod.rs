pub mod location;
pub mod progress;

use crate::config::duration;
use crate::config::settings::{parse_debug_addr, PqKeyExchange};
use crate::net::addr::HostPort;
use clap::{ArgAction, Args as _, CommandFactory, FromArgMatches, Parser, Subcommand};
use location::Location;
use std::ffi::OsString;
use std::net::SocketAddr;
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
#[command(disable_help_subcommand = true)]
pub struct Args {
    #[command(subcommand)]
    pub cmd: Option<Command>,
    #[command(flatten)]
    pub log: LogArgs,
    #[command(flatten)]
    pub files: FilesArgs,
    #[command(flatten)]
    pub sync: SyncArgs,
}

/// clap's default template with the build info on top; subcommands get it
/// in [`Args::command_tree`] since the template is not inherited.
const HELP_TEMPLATE: &str = "{name} {version}\n{about-with-newline}\n\
                             {usage-heading} {usage}\n\n{all-args}{after-help}";

/// A first sync, from nothing to a push and a pull: over ssh, or by hand.
const AFTER_HELP: &str = "\
Quick start over ssh (irsync installed on the client only):
  If `ssh myserver` logs you in, set the server up once:
    irsync pair myserver
  This installs this version of irsync on the server, grants this client
  access to any path (all_paths=rw), starts the daemon and saves `myserver`
  as a server. Then push and pull over iroh:
    irsync -av ~/Photos/ myserver:/srv/photos/
    irsync -av myserver:/srv/photos/ ~/Photos/  # pull them back
  Only `irsync pair` uses ssh; a sync never does. To share only named
  directories instead of any path, set the server up by hand as below.

Getting started by hand (irsync installed on both sides):
  On the client, print its id (the last line):
    irsync id show

  On the server, export a directory, let the client in and start the daemon:
    irsync export add photos:/srv/photos        # prints the daemon's id
    irsync client add myclient <client-id>
    irsync grant add photos myclient
    irsync daemon

  On the client, name the server, then list, push and pull:
    irsync server add myserver <server-id>
    irsync myserver:                            # the exports you may use
    irsync -av ~/Photos/ myserver:photos/       # push the contents of ~/Photos
    irsync -av myserver:photos/ ~/Photos/       # pull them back
    irsync -avn --delete ~/Photos/ myserver:photos/   # dry run, with deletions

  A trailing / on the source copies its contents, as in rsync.
  The book: https://irsync.org/book/";

impl Args {
    /// The command with the help template applied to the subcommands too,
    /// at every level (`irsync server add --help` included).
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
        let argv: Vec<OsString> = it.into_iter().map(Into::into).collect();
        let m = match Args::command_tree().try_get_matches_from(&argv) {
            // A path came first, so the line is a sync, as it always was:
            // clap only took a later path for a command because a flag
            // (`h:photos/ -v gc`) ended the run of paths.
            Ok(m) if m.subcommand_name().is_some() && paths_given(&m) => sync_reading(&argv)?,
            Ok(m) => m,
            // The same when the would-be command fails to parse
            // (`h:photos/ -v host add`). A line that starts with a command
            // name after the flags (`-v gc host:path`) keeps its error.
            Err(e) => match sync_reading(&argv) {
                Ok(m) if paths_given(&m) && !names_a_command(&m) => m,
                _ => return Err(e),
            },
        };
        reject_sync_args_with_command(&m)?;
        let mut args = Args::from_arg_matches(&m)?;
        args.cmd = match args.cmd.take() {
            None => {
                let mut s = std::mem::take(&mut args.sync);
                s.filters = ordered_filters(&m);
                Some(sync_or_list(s, &m)?)
            }
            Some(Command::Sync(mut s)) => {
                let sub = sub_matches(&m, "sync");
                s.filters = ordered_filters(sub);
                Some(sync_or_list(s, sub)?)
            }
            Some(Command::List(mut l)) => {
                l.filters = ordered_filters(sub_matches(&m, "list"));
                Some(Command::List(list_operand(l)?))
            }
            other => other,
        };
        Ok(args)
    }

    /// The options of a `sync`, which every sync is by the time
    /// [`Args::try_parse_from`] returns.
    pub fn sync_args(&self) -> &SyncArgs {
        match &self.cmd {
            Some(Command::Sync(s)) => s,
            _ => panic!("sync_args: not a sync command"),
        }
    }

    pub fn parse() -> Args {
        Args::try_parse_from(std::env::args_os()).unwrap_or_else(|e| e.exit())
    }
}

/// The paths of a sync were given on the command line.
fn paths_given(m: &clap::ArgMatches) -> bool {
    m.value_source("paths") == Some(clap::parser::ValueSource::CommandLine)
}

/// The line read as a sync, the way irsync read every line before the
/// global flags: once any argument is seen, no command is looked for.
fn sync_reading(argv: &[OsString]) -> Result<clap::ArgMatches, clap::Error> {
    Args::command_tree()
        .args_conflicts_with_subcommands(true)
        .try_get_matches_from(argv)
}

/// The first path of a sync reading is the name of a command, so the line
/// was meant as that command (`-v gc host:path`), not as a sync.
fn names_a_command(m: &clap::ArgMatches) -> bool {
    let Some(first) = m.get_many::<String>("paths").and_then(|mut p| p.next()) else {
        return false;
    };
    Args::command()
        .get_subcommands()
        .any(|c| c.get_name() == first || c.get_all_aliases().any(|a| a == first))
}

/// With a command, every sync argument set on the command line is an
/// error: clap no longer rejects it once `-v`/`-q` may come first.
fn reject_sync_args_with_command(m: &clap::ArgMatches) -> Result<(), clap::Error> {
    let Some(cmd) = m.subcommand_name() else {
        return Ok(());
    };
    let sync = SyncArgs::augment_args(clap::Command::new("sync"));
    for arg in sync.get_arguments() {
        let id = arg.get_id().as_str();
        if m.value_source(id) != Some(clap::parser::ValueSource::CommandLine) {
            continue;
        }
        let flag = match (arg.get_short(), arg.get_long()) {
            (Some(c), _) => format!("-{c}"),
            (None, Some(l)) => format!("--{l}"),
            // The paths. Not reached: a line with a path and a command is
            // re-read as a sync in `Args::try_parse_from` before this check.
            (None, None) => continue,
        };
        return Err(Args::command().error(
            clap::error::ErrorKind::ArgumentConflict,
            format!("{flag} is a sync option and cannot be used with '{cmd}'"),
        ));
    }
    Ok(())
}

fn sub_matches<'a>(m: &'a clap::ArgMatches, name: &str) -> &'a clap::ArgMatches {
    m.subcommand_matches(name)
        .expect("clap parsed this subcommand")
}

/// A sync with one remote operand is a listing (`irsync HOST:`, `irsync
/// HOST:PATH`), which takes the flags of `list` only; a lone local path
/// is a usage error. The operands of a sync are checked here, so that
/// every usage error exits 2.
fn sync_or_list(s: SyncArgs, m: &clap::ArgMatches) -> Result<Command, clap::Error> {
    let locations = s
        .paths
        .iter()
        .map(|p| Location::parse(p))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| usage_error(&e))?;
    if let [one] = locations.as_slice() {
        if one.is_remote() {
            reject_sync_only_args(m)?;
            let mut paths = s.paths;
            return Ok(Command::List(ListArgs {
                operand: paths.remove(0),
                no_recursive: s.no_recursive,
                conn: s.conn,
                filter_args: s.filter_args,
                filters: s.filters,
            }));
        }
    }
    let Some((dest, srcs)) = locations.split_last().filter(|(_, s)| !s.is_empty()) else {
        return Err(Args::command().error(
            clap::error::ErrorKind::MissingRequiredArgument,
            "the following required arguments were not provided:\n  <SRC>... <DEST>",
        ));
    };
    validate(srcs, dest).map_err(|e| usage_error(&e))?;
    Ok(Command::Sync(s))
}

/// A flag of a sync that a listing does not take, set on the command
/// line of `irsync HOST:[PATH]`, is an error, as it is for `irsync list`.
fn reject_sync_only_args(m: &clap::ArgMatches) -> Result<(), clap::Error> {
    let list = ListArgs::augment_args(clap::Command::new("list"));
    let sync = SyncArgs::augment_args(clap::Command::new("sync"));
    for arg in sync.get_arguments() {
        let id = arg.get_id();
        if list.get_arguments().any(|a| a.get_id() == id)
            || m.value_source(id.as_str()) != Some(clap::parser::ValueSource::CommandLine)
        {
            continue;
        }
        let flag = match (arg.get_short(), arg.get_long()) {
            (Some(c), _) => format!("-{c}"),
            (None, Some(l)) => format!("--{l}"),
            (None, None) => continue,
        };
        return Err(list_error(
            clap::error::ErrorKind::ArgumentConflict,
            &format!("{flag} is a sync option and cannot be used with a listing"),
        ));
    }
    Ok(())
}

/// A usage error of the operands, as clap reports its own: exit code 2.
fn usage_error(e: &UsageError) -> clap::Error {
    Args::command().error(clap::error::ErrorKind::ValueValidation, &e.0)
}

/// `list` takes one remote operand; a bare `HOST` means `HOST:`.
fn list_operand(mut l: ListArgs) -> Result<ListArgs, clap::Error> {
    let op = &mut l.operand;
    // `.` and `..` are paths, not hosts: no `:` for them.
    if !op.contains(':') && !op.contains('/') && op != "." && op != ".." {
        op.push(':');
    }
    let remote = Location::parse(op)
        .map_err(|e| list_error(clap::error::ErrorKind::ValueValidation, &e.0))?
        .is_remote();
    if !remote {
        return Err(list_error(
            clap::error::ErrorKind::ValueValidation,
            "list takes one operand: HOST or HOST:PATH",
        ));
    }
    Ok(l)
}

/// A usage error that prints the usage of `list`, not of the top level.
fn list_error(kind: clap::error::ErrorKind, msg: &str) -> clap::Error {
    let mut c = Args::command_tree();
    c.build();
    c.find_subcommand_mut("list")
        .expect("list is a subcommand")
        .error(kind, msg)
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
    /// Copy files to or from a server (the rsync-style form, named).
    Sync(SyncArgs),
    /// List a server's exports, or the files a pull of HOST:PATH would fetch.
    List(ListArgs),
    /// Run the daemon.
    ///
    /// Serves the exports in the config file to the clients they are granted to
    /// until interrupted, and prints its EndpointId at start. Logs go to
    /// stderr at warning level (-vv: info, -vvv: debug, -vvvv: trace, -q:
    /// errors only; RUST_LOG overrides).
    Daemon(DaemonArgs),
    /// Garbage-collect resume state.
    ///
    /// Removes abandoned partial files and dormant sessions older than 7
    /// days under every local root irsync has synced, the index entries of
    /// files that are gone, and index files an older irsync left; prints
    /// the paths removed and a count. A sync does the same for its own root
    /// at most daily. Exits 23 when something could not be read or
    /// removed.
    Gc {
        /// List what would be removed without removing anything.
        #[arg(short = 'n', long)]
        dry_run: bool,
    },
    /// Hash the files under local roots into their index, so a
    /// destination irsync did not create contributes chunks and rename
    /// candidates from the first session on (`export update` does the
    /// same for the daemon's exports).
    Index(IndexArgs),
    /// Manage the servers this machine connects to (edits irsync.toml).
    /// With no subcommand, runs `list`.
    Server(ServerArgs),
    /// Manage the clients this machine's daemon lets in (edits irsync.toml).
    /// With no subcommand, runs `list`.
    Client(ClientArgs),
    /// Manage exports (edits irsync.toml).
    /// With no subcommand, runs `list`.
    Export(ExportArgs),
    /// Manage grants: who may use which export (edits irsync.toml).
    /// With no subcommand, runs `list`.
    Grant(GrantArgs),
    /// This machine's identities (the client's and the daemon's).
    /// With no subcommand, runs `list`.
    Id(IdArgs),
    /// Read and edit any key of the config file by its dotted path, as
    /// `git config` does: `config get daemon.listen`, `config set
    /// exports.photos.read_only true`.
    /// With no subcommand, runs `list`.
    Config(ConfigArgs),
    /// Set a server up over ssh: install irsync there, let this client in,
    /// start the daemon and save the server. Syncs then go over iroh; ssh is
    /// used only here.
    Pair(PairArgs),
    /// Show the config files, identities, servers, clients and exports.
    Show,
    /// Manage a server from this machine: its exports, grants and
    /// clients. Needs `admin = "rw"` in this client's entry on the server
    /// (`admin = "ro"` for the listings). The server must be running.
    ///
    /// Each command changes or reads the server's config file at once, as the
    /// same command does on the server itself.
    Remote(RemoteArgs),
    /// The server side of SSH onboarding, run by the client over ssh:
    /// makes sure the daemon identity exists, gives the client
    /// `all_paths = "rw"`, gets this copy's daemon running and prints a report.
    #[command(hide = true)]
    SshSetup {
        /// The client's EndpointId.
        #[arg(long)]
        client: String,
        /// The client's host name, as hex digits of its UTF-8 bytes: the
        /// name this server knows the client by.
        #[arg(long)]
        client_name_hex: String,
        /// A client pairing a server it already has: do not add back an
        /// `all_paths` right the owner removed (only a freshly generated daemon
        /// identity gets one).
        #[arg(long)]
        recheck: bool,
        /// Turn the daemon's DHT publishing off (`[daemon] dht`).
        #[arg(long)]
        no_dht: bool,
        /// Start the daemon in the background, not as a user service.
        #[arg(long)]
        no_service: bool,
    },
    /// Checks a minisign signature of FILE against the irsync release keys
    /// (or the keys given). Used by install.sh. Exit 0 is a good signature,
    /// 1 a bad, missing or untrusted one, 2 a usage error.
    #[command(hide = true)]
    VerifySignature {
        /// The signature file. Default: FILE.minisig.
        #[arg(long)]
        signature: Option<PathBuf>,
        /// A public key (the base64 line of a minisign .pub file). Can be
        /// repeated. Replaces the built-in release keys.
        #[arg(long = "public-key")]
        public_key: Vec<String>,
        /// The signature's trusted comment must be exactly this.
        #[arg(long)]
        trusted_comment: Option<String>,
        /// The file that was signed.
        file: PathBuf,
    },
}

#[derive(Debug, clap::Args)]
pub struct ConfigArgs {
    #[command(subcommand)]
    pub cmd: Option<ConfigCmd>,
}

#[derive(Debug, Subcommand)]
pub enum ConfigCmd {
    /// Print a key: a bare scalar, one array element per line, or a
    /// table's keys as `key = value` lines. An unset settings key prints
    /// its default; a key with no value and no default exits 1.
    Get {
        /// A dotted path such as `daemon.listen` or `servers.home.relay`;
        /// quote a segment that has a dot: `exports."www.example.com".path`.
        key: String,
    },
    /// Set a key, in its type; for an array key every VALUE is one
    /// element (no VALUE: an empty array). Tables are created as needed;
    /// the edited file is validated before it is written.
    Set {
        key: String,
        #[arg(value_name = "VALUE")]
        values: Vec<String>,
    },
    /// Remove a key, or a whole table (`config unset servers.home`).
    Unset { key: String },
    /// Every key the file sets, as `dotted.key = value`, file order.
    List,
}

#[derive(Debug, clap::Args)]
pub struct RemoteArgs {
    /// A server name from `irsync server list`, or its daemon's EndpointId.
    #[arg(value_name = "SERVER")]
    pub server: String,
    #[command(subcommand)]
    pub cmd: RemoteCmd,
}

#[derive(Debug, Subcommand)]
pub enum RemoteCmd {
    /// Manage the server's exports. With no subcommand, runs `list`.
    Export(RemoteExportArgs),
    /// Manage the server's grants. With no subcommand, runs `list`.
    Grant(GrantArgs),
    /// Manage the clients the server lets in. With no subcommand, runs `list`.
    Client(ClientArgs),
    /// Show the server's identity, clients and exports.
    Show,
    /// Read the server's config file. With no subcommand, runs `list`.
    Config(RemoteConfigArgs),
}

#[derive(Debug, clap::Args)]
pub struct RemoteExportArgs {
    #[command(subcommand)]
    pub cmd: Option<RemoteExportCmd>,
}

#[derive(Debug, Subcommand)]
pub enum RemoteExportCmd {
    /// Export a directory of the server. Quote a path that starts with ~,
    /// so the shell leaves it for the server: `'photos:~/Photos'`.
    Add(ExportAddArgs),
    /// Stop exporting a directory, with its grants.
    Remove {
        /// The export's name.
        name: String,
    },
    /// List the exports with their paths, flags and grants.
    List,
}

#[derive(Debug, clap::Args)]
pub struct RemoteConfigArgs {
    #[command(subcommand)]
    pub cmd: Option<RemoteConfigCmd>,
}

#[derive(Debug, Subcommand)]
pub enum RemoteConfigCmd {
    /// Print a key, as `irsync config get` does on the server.
    Get {
        /// A dotted path such as `daemon.listen`.
        key: String,
    },
    /// Every key the server's file sets, as `dotted.key = value`.
    List,
}

#[derive(Debug, clap::Args)]
pub struct IdArgs {
    #[command(subcommand)]
    pub cmd: Option<IdCmd>,
}

#[derive(Debug, Subcommand)]
pub enum IdCmd {
    /// List the identities this machine has: the client's, then the
    /// daemon's when there is one, each under a `client:` or `daemon:`
    /// label. A missing client identity is generated as for a sync; the
    /// daemon's is never generated.
    List,
    /// Print the public key line and EndpointId of the client (default),
    /// the daemon, or both, each under a `client:` or `daemon:` label. A
    /// missing client identity is generated as for a sync; a missing daemon
    /// identity is an error, never generated.
    Show {
        /// The daemon's identity instead of the client's.
        #[arg(long, conflicts_with = "all")]
        daemon: bool,
        /// Both identities.
        #[arg(short = 'a', long)]
        all: bool,
    },
    /// Generate an Ed25519 identity and print its public key line and
    /// EndpointId. Refuses to overwrite an existing key or .pub file
    /// unless --force is given.
    Generate {
        /// The daemon's identity instead of the client's: at its
        /// configured path, or at --out.
        #[arg(long)]
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
pub struct ServerArgs {
    #[command(subcommand)]
    pub cmd: Option<ServerCmd>,
}

#[derive(Debug, Subcommand)]
pub enum ServerCmd {
    /// Add a server, or update the keys given for an existing one.
    Add(ServerAddArgs),
    /// Remove a server.
    Remove {
        /// The server's name.
        name: String,
    },
    /// Rename a server.
    Rename {
        /// The current name.
        old: String,
        /// The new name.
        new: String,
    },
    /// List servers.
    List,
}

#[derive(Debug, clap::Args)]
pub struct ClientArgs {
    #[command(subcommand)]
    pub cmd: Option<ClientCmd>,
}

#[derive(Debug, Subcommand)]
pub enum ClientCmd {
    /// Add a client, or change the id of an existing one.
    Add {
        /// The name (no ':' or '/', not an EndpointId).
        name: String,
        /// The client's EndpointId (`irsync id show` on that machine).
        id: String,
    },
    /// Remove a client. Refused while grants name it.
    Remove {
        /// The client's name.
        name: String,
        /// Also remove every grant that names the client.
        #[arg(short, long)]
        force: bool,
    },
    /// Rename a client, and every grant that names it.
    Rename {
        /// The current name.
        old: String,
        /// The new name.
        new: String,
    },
    /// Set a client's server-wide rights: all_exports=rw|ro|none,
    /// all_paths=rw|ro|none.
    Set {
        /// The client's name.
        name: String,
        /// A right and its value: `none` removes the right.
        #[arg(required = true, value_name = "KEY=VALUE")]
        settings: Vec<String>,
    },
    /// List clients, with their server-wide rights.
    List,
}

#[derive(Debug, clap::Args)]
pub struct ExportArgs {
    #[command(subcommand)]
    pub cmd: Option<ExportCmd>,
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
        /// Export names, or `all` for every export.
        #[arg(value_name = "NAME", required = true)]
        names: Vec<String>,
    },
}

#[derive(Debug, clap::Args)]
pub struct GrantArgs {
    #[command(subcommand)]
    pub cmd: Option<GrantCmd>,
}

#[derive(Debug, Subcommand)]
pub enum GrantCmd {
    /// Grant a client access to an export. For every export or any path,
    /// see `irsync client set`.
    Add {
        /// The export's name.
        export: String,
        /// A client name, an EndpointId, a public key line or a .pub file.
        who: String,
        /// Read-only access (default: read-write).
        #[arg(long)]
        ro: bool,
    },
    /// Remove grants: of one client or of every client, on one export or on
    /// every export's own list. Server-wide rights go with
    /// `irsync client set`.
    Remove {
        /// The export's name, or `all` for every export's own list.
        export: String,
        /// A client name, an EndpointId, a public key line or a .pub file;
        /// or `all` for every client.
        who: String,
    },
    /// List grants: all of them, one export's, or what one client may reach.
    List {
        /// An export's name (`all_exports`, `all_paths`, `admin`: the
        /// clients with that server-wide right), or a client (its name or
        /// its EndpointId); nothing lists every grant.
        target: Option<String>,
    },
}

#[derive(Debug, clap::Args)]
pub struct ServerAddArgs {
    /// The name (no ':' or '/', not an EndpointId).
    pub name: String,
    /// The server's EndpointId: its daemon, the id dialled.
    pub id: String,
    /// This server's relay: `default` (n0's relays), `disabled` (direct
    /// connections only; the lookups stay on) or the URL of a relay of your
    /// own (default: `[defaults]`'s, else n0's).
    #[arg(long, value_name = "default|disabled|URL")]
    pub relay: Option<crate::config::settings::Relay>,
    /// Dial the server at this `ip:port`, `[ipv6]:port` or `host:port`
    /// directly, with relay and lookups off (repeatable; replaces the
    /// stored list).
    #[arg(long, value_name = "ADDR")]
    pub address: Vec<HostPort>,
    /// Data streams a push to this server keeps open at once (default:
    /// `[defaults]`'s, else 128).
    #[arg(long, value_name = "N", value_parser = clap::value_parser!(u32).range(1..))]
    pub parallel: Option<u32>,
    /// How long a sync with this server keeps redialling after a dropped
    /// connection: `<N>[s|m|h|d]`, e.g. `1h`; 0 gives up at once.
    #[arg(long, value_name = "DUR", value_parser = validate_duration)]
    pub reconnect: Option<String>,
    /// How often a sync retries this server when it has not answered yet,
    /// before giving up; 0 gives up on the first failure (default:
    /// `[defaults]`'s, else 4).
    #[arg(long, value_name = "N")]
    pub dial_retries: Option<u32>,
    /// Look this server up in the mainline DHT (default:
    /// `[defaults]`'s, else yes).
    #[arg(long, value_name = "yes|no", value_parser = parse_yes_no)]
    pub dht: Option<bool>,
    /// Look this server up through n0's DNS service (default:
    /// `[defaults]`'s, else yes).
    #[arg(long, value_name = "yes|no", value_parser = parse_yes_no)]
    pub dns: Option<bool>,
    /// Look this server up with mDNS on the local network (default:
    /// `[defaults]`'s, else yes).
    #[arg(long, value_name = "yes|no", value_parser = parse_yes_no)]
    pub mdns: Option<bool>,
    /// `require`: refuse this server when it does not offer the
    /// post-quantum hybrid key exchange (X25519MLKEM768); `prefer`: use it
    /// when offered (default: `[defaults]`'s, else prefer).
    #[arg(long, value_name = "prefer|require")]
    pub pq_key_exchange: Option<PqKeyExchange>,
}

/// Accepts a duration the config files accept; keeps the user's text.
fn validate_duration(s: &str) -> Result<String, String> {
    duration::parse(s)
        .map(|_| s.to_string())
        .map_err(|e| e.to_string())
}

/// Whether an rsync `--info` list (`flist0,progress2`) turns progress on.
fn info_asks_progress(spec: &str) -> bool {
    spec.split(',')
        .filter_map(|f| f.strip_prefix("progress"))
        .any(|level| level != "0")
}

fn parse_yes_no(s: &str) -> Result<bool, String> {
    match s.to_ascii_lowercase().as_str() {
        "yes" | "true" => Ok(true),
        "no" | "false" => Ok(false),
        _ => Err(format!("expected yes or no, got {s:?}")),
    }
}

/// How much any command says. Global: accepted before or after the command.
#[derive(Debug, Clone, Copy, Default, clap::Args)]
pub struct LogArgs {
    /// More output. -v: the command's own extra output (a sync lists each
    /// file transferred and ends with a transfer summary). -vv adds info
    /// logging (and a sync's summary per session), -vvv debug, -vvvv trace,
    /// -vvvvv trace plus every protocol message.
    #[arg(short, long, global = true, action = ArgAction::Count)]
    pub verbose: u8,
    /// Print errors only, and no summary or progress. Wins over -v.
    #[arg(short, long, global = true)]
    pub quiet: bool,
}

/// The files every command reads. Global: accepted before or after the
/// command.
#[derive(Debug, clap::Args)]
pub struct FilesArgs {
    /// The config file (default: irsync.toml in the config directory).
    #[arg(long, global = true, value_name = "PATH")]
    pub config: Option<PathBuf>,
    /// Read an identity's passphrase from the first line of this file (else
    /// IRSYNC_PASSPHRASE, else a prompt on a terminal). id generate encrypts
    /// the new key with it.
    #[arg(long, global = true, value_name = "PATH")]
    pub passphrase_file: Option<PathBuf>,
    /// The client's private key, for every command that uses it: a sync,
    /// a listing, pair, remote, id and show (default: `[defaults]
    /// identity`, else id_ed25519 in the config directory, else
    /// ~/.ssh/id_ed25519; generated if missing). id generate writes the
    /// new client key here.
    #[arg(long, global = true, value_name = "FILE")]
    pub identity: Option<PathBuf>,
}

#[derive(Debug, clap::Args)]
pub struct PairArgs {
    /// A server name, or an ssh destination (`[user@]host`).
    #[arg(value_name = "HOST")]
    pub host: String,
    /// The remote shell for pairing (default: IRSYNC_RSH, else `ssh`);
    /// split on whitespace like rsync's `-e`.
    #[arg(long, value_name = "CMD")]
    pub rsh: Option<String>,
    /// Copy nothing to the server: use the irsync already at
    /// `~/.local/bin/irsync` there, and stop if it is missing or another
    /// version. Default: `[defaults] ssh_install` in the config file,
    /// else install.
    #[arg(long)]
    pub no_ssh_install: bool,
    /// Turn the server's DHT publishing off (`[daemon] dht =
    /// false`). Without it the server's setting is left alone.
    #[arg(long)]
    pub server_no_dht: bool,
    /// Do not make the daemon a systemd or launchd user service: start it
    /// in the background, until the server reboots. For a daemon you run
    /// your own way.
    #[arg(long)]
    pub no_service: bool,
}

#[derive(Debug, clap::Args)]
pub struct ExportAddArgs {
    /// `NAME:PATH`, or a directory named by its final component.
    pub target: String,
    /// Refuse pushes into the export from every client (`read_only`).
    #[arg(long)]
    pub read_only: bool,
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
    /// Local roots to index.
    #[arg(value_name = "ROOT", required = true)]
    pub targets: Vec<PathBuf>,
}

#[derive(Debug, clap::Args)]
pub struct DaemonArgs {
    /// Bind this socket (`ip:port`, `[ipv6]:port` or `host:port`; port 0 =
    /// pick one) instead of the config's `listen` list. Repeatable, at most
    /// one IPv4 and one IPv6.
    #[arg(long, value_name = "ADDR")]
    pub listen: Vec<HostPort>,
    /// Run even as root. The daemon has not had a security audit, and as
    /// root any client with write access can create setuid files and
    /// device nodes. The config key `daemon.allow_root` does the same.
    #[arg(long = "run-as-root-i-know-what-i-am-doing")]
    pub run_as_root: bool,
}

/// rsync's rate syntax: a number, decimals allowed (`1.5M`), then an optional
/// unit. No unit means KiB. `K`/`M`/`G`/`T`/`P` and the `iB` forms (`KiB`)
/// are powers of 1024; the `B` forms (`KB`) are powers of 1000; `b` alone is
/// bytes. Returns bytes per second; 0 means no limit.
fn parse_bwlimit(s: &str) -> Result<u64, String> {
    let bad = || format!("invalid rate {s:?}");
    let lower = s.to_ascii_lowercase();
    let split = lower
        .find(|c: char| !(c.is_ascii_digit() || c == '.'))
        .unwrap_or(lower.len());
    let (num, unit) = lower.split_at(split);
    let (int, frac) = num.split_once('.').unwrap_or((num, ""));
    if (int.is_empty() && frac.is_empty()) || frac.contains('.') {
        return Err(bad());
    }
    let mult = match unit {
        "" => 1024.0,
        "b" => 1.0,
        _ => {
            let (letter, rest) = unit.split_at(1);
            let base = match rest {
                "" | "ib" => 1024.0_f64,
                "b" => 1000.0,
                _ => return Err(bad()),
            };
            let exp = match letter {
                "k" => 1,
                "m" => 2,
                "g" => 3,
                "t" => 4,
                "p" => 5,
                _ => return Err(bad()),
            };
            base.powi(exp)
        }
    };
    let n: f64 = format!("{}.{}0", if int.is_empty() { "0" } else { int }, frac)
        .parse()
        .map_err(|_| bad())?;
    let bytes = (n * mult).floor();
    if !bytes.is_finite() || bytes >= u64::MAX as f64 {
        return Err(bad());
    }
    Ok(bytes as u64)
}

#[derive(Debug, Default, clap::Args)]
#[command(group(
    clap::ArgGroup::new("delete_when")
        .args(["delete_before", "delete_during", "delete_after"])
        .multiple(false)
))]
pub struct SyncArgs {
    /// Local paths or `HOST:PATH` (HOST: a server name or an EndpointId;
    /// PATH may start with an export name, a directory the daemon shares).
    /// Exactly one side is remote; several sources only when pushing. A
    /// trailing `/` copies a directory's contents, none the directory
    /// itself. A source may be one file: it goes into DEST if that is a
    /// directory, becomes DEST if that does not exist, and updates DEST if
    /// that is a file (DEST ending in `/` is always a directory).
    #[arg(value_name = "SRC... DEST")]
    pub paths: Vec<String>,

    #[command(flatten)]
    pub conn: ConnArgs,
    #[command(flatten)]
    pub filter_args: FilterArgs,

    /// Archive mode: -rlptgoD (symlinks, permissions, times, group, owner,
    /// devices and specials).
    #[arg(short, long)]
    pub archive: bool,
    /// Recurse into directories (implied by -a). Without it only the
    /// top-level entries are transferred and directories are skipped.
    #[arg(short = 'r', long = "recursive")]
    pub recursive: bool,
    /// Turn the recursion implied by -a off.
    #[arg(long = "no-recursive", alias = "no-r", conflicts_with = "recursive")]
    pub no_recursive: bool,
    /// Transfer directories without their contents (rsync's -d), where
    /// recursion is off.
    #[arg(short = 'd', long = "dirs")]
    pub dirs: bool,
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
    /// Leave chunks that are all zeros as holes; without it only files that
    /// are sparse in the source are written sparse.
    #[arg(short = 'S', long = "sparse")]
    pub sparse: bool,
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
    /// Delete each directory's extraneous entries as the run leaves that directory
    #[arg(long)]
    pub delete_during: bool,
    /// Delete after the transfer (what --delete does).
    #[arg(long)]
    pub delete_after: bool,
    /// Also delete destination files the filters exclude (implies --delete).
    #[arg(long)]
    pub delete_excluded: bool,
    /// Stop deleting after N files; exit code 25 when the cap stops a run.
    #[arg(long, value_name = "N")]
    pub max_delete: Option<u64>,
    /// Rate limit on file data, push or pull: KiB/s by default; suffixes
    /// K, M, G are 1024-based (also KiB), KB, MB, GB 1000-based, `b` bytes;
    /// decimals allowed (1.5M); 0 = unlimited.
    #[arg(long, value_name = "RATE", value_parser = parse_bwlimit)]
    pub bwlimit: Option<u64>,
    /// Apply owner and group ids as sent, instead of mapping the sender's
    /// user and group names to this side's ids (rsync's default).
    #[arg(long)]
    pub numeric_ids: bool,
    /// Keep the partial files of interrupted transfers under DIR (relative
    /// to the destination root, or absolute) instead of beside their
    /// files. On a push the daemon keeps them there; only a relative DIR
    /// inside the destination is allowed then.
    #[arg(long, value_name = "DIR")]
    pub partial_dir: Option<PathBuf>,
    /// Run the deletion pass even when a listing was cut short by I/O
    /// errors (by default it is skipped then).
    #[arg(long)]
    pub ignore_errors: bool,
    /// Print a block of transfer statistics at the end.
    #[arg(long)]
    pub stats: bool,
    /// Draw a live progress block while stdout is a terminal.
    #[arg(short = 'P', long)]
    pub progress: bool,
    /// rsync's output selection; only `progress1` or `progress2` is read,
    /// as --progress.
    #[arg(long, value_name = "SPEC")]
    pub info: Option<String>,
    /// Print rsync's per-file change summary (the itemize codes).
    #[arg(short = 'i', long)]
    pub itemize_changes: bool,
    /// Data streams a push keeps open at once (default: `parallel` from the
    /// server entry or `[defaults]`, else 128).
    #[arg(long, value_name = "N", value_parser = clap::value_parser!(u32).range(1..))]
    pub parallel: Option<u32>,
    /// How long to keep redialling after a dropped connection:
    /// `<N>[s|m|h|d]`; 0 gives up at once like rsync (default: `reconnect`
    /// from the server entry or `[defaults]`, else 24h). A server that
    /// has never answered gets only `--dial-retries` retries.
    #[arg(long, value_name = "DUR", value_parser = duration::parse)]
    pub reconnect: Option<Duration>,
    /// Start a fresh plan instead of resuming a dormant session (partial
    /// files are still picked up).
    #[arg(long)]
    pub no_resume: bool,
    /// Do not fill files from other files on the receiving side that hold
    /// the same content or parts of it: a moved or copied file is
    /// transferred again (a changed file still reuses its own old version).
    #[arg(long)]
    pub no_rename_detection: bool,
    /// Write a changed file into the destination itself instead of a
    /// `.part` renamed over it: no second copy on disk, but readers see a
    /// torn file during the transfer and a crash leaves a mixture.
    #[arg(long)]
    pub inplace: bool,
    /// Flush each received file to disk before it is renamed into place
    /// (rsync's --fsync). Without it, as in rsync, a power loss or kernel
    /// crash in the seconds after a sync can leave a file at its name with
    /// the right size and time but not its content.
    #[arg(long)]
    pub fsync: bool,
    /// Send each file from a reflink clone where the filesystem can make
    /// one, else hash it again before the receiver places it; at the end,
    /// re-check every listed file. This is meant to catch writers that
    /// change a file without updating its times (memory-mapped databases);
    /// only ZFS 2.4 clones have been shown to include such writes, and the
    /// second hash has not been tested against them.
    #[arg(long)]
    pub paranoid: bool,
    /// Extend a destination file that is shorter than the source by the
    /// source's tail, trusting what is already there; a destination that
    /// is not shorter is skipped (rsync's rule).
    #[arg(long, conflicts_with = "append_verify")]
    pub append: bool,
    /// Like --append, but the existing prefix is verified too (a delta
    /// transfer with --append's skip rule).
    #[arg(long)]
    pub append_verify: bool,
    /// Serve metrics and span timings over HTTP at `IP:PORT` (a bare port
    /// means 127.0.0.1); the port stays open after the sync until SIGINT
    /// or SIGHUP.
    #[arg(long, value_name = "ADDR", value_parser = parse_debug_addr)]
    pub debug_addr: Option<SocketAddr>,
    /// Close the debug port when the sync ends instead of waiting.
    #[arg(long, requires = "debug_addr")]
    pub debug_port_no_wait: bool,

    /// Include/exclude rules in command-line order (filled after parsing).
    #[arg(skip)]
    pub filters: Vec<FilterRule>,
}

/// How to reach the server: the flags a sync and a listing share.
#[derive(Debug, Default, Clone, clap::Args)]
pub struct ConnArgs {
    /// Drop a connection that moves no data and no control message for
    /// this long, and reconnect: `<N>[s|m|h|d]`, a bare number is seconds;
    /// 0 = never (default: `idle_timeout` from `[defaults]`, else 0).
    #[arg(long, value_name = "DUR", value_parser = duration::parse)]
    pub idle_timeout: Option<Duration>,
    /// How often to retry a server that has not answered yet, with growing
    /// waits (1s, 2s, 4s, ...), before giving up; 0 gives up on the first
    /// failure (default: `dial_retries` from the server entry or
    /// `[defaults]`, else 4; 0 for a server `irsync pair` saved).
    #[arg(long, value_name = "N")]
    pub dial_retries: Option<u32>,
    /// The relay for this run: `default` (n0's relays), `disabled` (direct
    /// connections only; the lookups stay on) or the URL of a relay of your
    /// own. Default: `relay` from the server entry, else `[defaults]`, else
    /// n0's.
    #[arg(long, value_name = "default|disabled|URL")]
    pub relay: Option<crate::config::settings::Relay>,
    /// `require`: refuse a daemon that does not offer the post-quantum
    /// hybrid key exchange (X25519MLKEM768), such as an irsync built without it.
    /// `prefer`: use it when offered. Default: `pq_key_exchange` from the
    /// server entry or `[defaults]`, else prefer.
    #[arg(long, value_name = "prefer|require")]
    pub pq_key_exchange: Option<PqKeyExchange>,
    /// Do not look the daemon up in the mainline DHT for this run (the
    /// key `dht`).
    #[arg(long)]
    pub no_dht: bool,
    /// Do not look the daemon up through n0's DNS service for this run
    /// (the key `dns`).
    #[arg(long)]
    pub no_dns: bool,
    /// Do not look the daemon up with mDNS on the local network for this
    /// run (the key `mdns`).
    #[arg(long)]
    pub no_mdns: bool,
    /// Dial the daemon at this `ip:port`, `[ipv6]:port` or `host:port`
    /// directly (repeatable). Turns relay, DNS, DHT, mDNS and the port
    /// mapper off; the daemon's EndpointId is still required.
    #[arg(long, value_name = "ADDR", conflicts_with = "relay")]
    pub address: Vec<HostPort>,
}

/// The include and exclude rules: the flags a sync and a listing share.
/// [`Args::try_parse_from`] puts them in command-line order in
/// `filters` of the sync or the listing.
#[derive(Debug, Default, Clone, clap::Args)]
pub struct FilterArgs {
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
}

/// `irsync list`, and `irsync HOST:[PATH]`.
#[derive(Debug, Default, Clone, clap::Args)]
pub struct ListArgs {
    /// A server name or an EndpointId, with or without `:`, lists the
    /// exports this client may use; HOST:PATH lists the files a pull of it
    /// would fetch.
    #[arg(value_name = "HOST[:PATH]")]
    pub operand: String,
    /// List only the top level of HOST:PATH (a listing recurses by
    /// default).
    #[arg(long = "no-recursive", alias = "no-r")]
    pub no_recursive: bool,
    #[command(flatten)]
    pub conn: ConnArgs,
    #[command(flatten)]
    pub filter_args: FilterArgs,
    /// Include/exclude rules in command-line order (filled after parsing).
    #[arg(skip)]
    pub filters: Vec<FilterRule>,
}

impl ListArgs {
    /// The listing as a sync's arguments with `paths`: what a file
    /// listing, a dry-run pull, runs with.
    pub fn as_sync(&self, paths: Vec<String>) -> SyncArgs {
        SyncArgs {
            paths,
            conn: self.conn.clone(),
            filter_args: self.filter_args.clone(),
            no_recursive: self.no_recursive,
            filters: self.filters.clone(),
            ..SyncArgs::default()
        }
    }
}

/// The operands of a sync, read: [`Args::try_parse_from`] has checked them.
#[derive(Debug, Clone)]
pub struct Operands {
    pub srcs: Vec<Location>,
    pub dest: Location,
    pub direction: Direction,
}

impl SyncArgs {
    /// The sources, the destination and which way the data flows. Panics
    /// on operands [`Args::try_parse_from`] would have refused.
    pub fn operands(&self) -> Operands {
        let mut all: Vec<Location> = self
            .paths
            .iter()
            .map(|p| Location::parse(p).expect("operands are checked while parsing"))
            .collect();
        let dest = all.pop().expect("a sync has a destination");
        let direction = validate(&all, &dest).expect("operands are checked while parsing");
        Operands {
            srcs: all,
            dest,
            direction,
        }
    }

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
    /// The options of a sync, or of a listing as the dry-run pull it is.
    pub fn from_args(args: &Args) -> SyncOptions {
        match &args.cmd {
            Some(Command::List(l)) => Self::with(args, &l.as_sync(vec![l.operand.clone()])),
            _ => Self::with(args, args.sync_args()),
        }
    }

    /// The options of a command that dials a server but syncs nothing
    /// (`remote`): the defaults, with the global flags.
    pub fn for_command(args: &Args) -> SyncOptions {
        Self::with(args, &args.sync)
    }

    fn with(args: &Args, a: &SyncArgs) -> SyncOptions {
        let preserve = Preserve {
            recursive: (a.archive || a.recursive) && !a.no_recursive,
            dirs: a.dirs,
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
            numeric_ids: a.numeric_ids,
            partial_dir: a.partial_dir.clone(),
            ignore_errors: a.ignore_errors,
            stats: a.stats,
            progress: a.progress || a.info.as_deref().is_some_and(info_asks_progress),
            itemize: a.itemize_changes,
            idle_timeout: a.conn.idle_timeout,
            parallel: a.parallel,
            reconnect: a.reconnect,
            dial_retries: a.conn.dial_retries,
            relay: a.conn.relay.clone(),
            pq_key_exchange: a.conn.pq_key_exchange,
            no_dht: a.conn.no_dht,
            no_dns: a.conn.no_dns,
            no_mdns: a.conn.no_mdns,
            addresses: a.conn.address.clone(),
            config: args.files.config.clone(),
            state_dir: None,
            identity: args.files.identity.clone(),
            passphrase_file: args.files.passphrase_file.clone(),
            no_resume: a.no_resume,
            no_rename_detection: a.no_rename_detection,
            inplace: a.inplace,
            fsync: a.fsync,
            paranoid: a.paranoid,
            append: if a.append {
                Append::Append
            } else if a.append_verify {
                Append::Verify
            } else {
                Append::Off
            },
            debug_addr: a.debug_addr,
            debug_no_wait: a.debug_port_no_wait,
            verbose: args.log.verbose,
            quiet: args.log.quiet,
        }
    }
}
