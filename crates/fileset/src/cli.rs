use std::ffi::OsString;
use std::path::PathBuf;

use clap::builder::TypedValueParser;
use clap::{Args, Parser, Subcommand, ValueEnum};

#[derive(Parser, Debug)]
#[command(
    name = "fileset",
    version,
    about = "Manifest and verify a directory tree"
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Create the manifest for a directory tree (fails if one exists)
    Init(InitArgs),
    /// Bring an existing manifest up to date (fails if none exists)
    Update(UpdateArgs),
    /// Verify a directory tree against its manifest
    Check(CheckArgs),
    /// List the entries recorded in a manifest
    #[command(visible_alias = "ls")]
    List(ListArgs),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum BackendArg {
    Sqlite,
    Fjall,
    Redb,
}

#[derive(Args, Debug)]
pub struct CommonArgs {
    /// Root of the tree
    pub path: PathBuf,
    /// Location of the manifest instead of <PATH>/.fileset.sqlite, .fileset.fjall or .fileset.redb; may be outside the tree
    #[arg(long, value_name = "MPATH")]
    pub manifest: Option<PathBuf>,
    /// Treat warnings as failures
    #[arg(long)]
    pub fail_on_warnings: bool,
    /// Descend into other mount points
    #[arg(long)]
    pub cross_mount_points: bool,
    /// Do not sort directory entries
    #[arg(long)]
    pub no_dir_sort: bool,
    /// Number of worker threads
    #[arg(short = 'j', long, value_name = "N", value_parser = clap::value_parser!(u64).range(1..).map(|n| n as usize))]
    pub jobs: Option<usize>,
    /// Number of threads that list directories and read metadata
    #[arg(long, value_name = "N", default_value_t = crate::walk::DEFAULT_THREADS, value_parser = clap::value_parser!(u64).range(1..).map(|n| n as usize))]
    pub walk_threads: usize,
    #[cfg_attr(
        unix,
        doc = "Index the macOS volume internals (.Spotlight-V100, .Trashes, ...) that are skipped by default"
    )]
    #[cfg_attr(
        windows,
        doc = "Index the Windows volume internals ($RECYCLE.BIN, System Volume Information) that are skipped by default"
    )]
    #[arg(long)]
    pub no_builtin_excludes: bool,
    /// Suppress progress output
    #[arg(short = 'q', long, conflicts_with = "verbose")]
    pub quiet: bool,
    /// Print one line per entry on stdout, without progress output
    #[arg(short = 'v', long)]
    pub verbose: bool,
}

#[derive(Args, Debug)]
pub struct InitArgs {
    #[command(flatten)]
    pub common: CommonArgs,
    /// Manifest storage backend
    #[arg(long, value_enum)]
    pub store: Option<BackendArg>,
    /// Leave a redb manifest as large as the run made it, instead of compacting it
    #[arg(long)]
    pub no_compaction: bool,
}

#[derive(Args, Debug)]
pub struct UpdateArgs {
    #[command(flatten)]
    pub common: CommonArgs,
    /// Rehash all files regardless of cached metadata
    #[arg(long)]
    pub rehash: bool,
    /// Read every entry's xattrs, also where size, mtime and ctime match the manifest
    #[arg(long)]
    pub force_xattr_reads: bool,
    /// Leave a redb manifest as large as the run made it, instead of compacting it
    #[arg(long)]
    pub no_compaction: bool,
}

#[derive(Args, Debug)]
pub struct CheckArgs {
    #[command(flatten)]
    pub common: CommonArgs,
    /// Compare only type, content and symlink target
    #[arg(long, conflicts_with = "paranoid")]
    pub relaxed: bool,
    /// Compare everything that is recorded and comparable
    #[arg(long, conflicts_with = "relaxed")]
    pub paranoid: bool,
    /// Drop one field from the comparison (repeatable)
    #[arg(long, value_name = "FIELD")]
    pub ignore: Vec<String>,
    /// Add one field to the comparison (repeatable)
    #[arg(long, value_name = "FIELD")]
    pub require: Vec<String>,
}

#[derive(Args, Debug)]
pub struct ListArgs {
    /// Root of the tree
    pub path: PathBuf,
    /// List only entries whose path matches this glob (quote it): `*` and `?` stop at `/`, `**` crosses it
    pub pattern: Option<OsString>,
    /// Location of the manifest instead of <PATH>/.fileset.sqlite, .fileset.fjall or .fileset.redb; with it, <PATH> need not exist
    #[arg(long, value_name = "MPATH")]
    pub manifest: Option<PathBuf>,
    /// Also print owner, link count, device numbers and extended attributes
    #[arg(short = 'v', long)]
    pub verbose: bool,
    /// More than one pattern: what an unquoted glob becomes once the shell expanded it.
    #[arg(hide = true, trailing_var_arg = true)]
    pub extra: Vec<OsString>,
}
