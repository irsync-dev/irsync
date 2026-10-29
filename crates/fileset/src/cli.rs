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
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum BackendArg {
    Sqlite,
    Fjall,
}

#[derive(Args, Debug)]
pub struct CommonArgs {
    /// Root of the tree
    pub path: PathBuf,
    /// Location of the manifest instead of <PATH>/.fileset.sqlite or <PATH>/.fileset.fjall; may be outside the tree
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
    /// Index the macOS volume internals (.Spotlight-V100, .Trashes, ...) that are skipped by default
    #[arg(long)]
    pub no_builtin_excludes: bool,
    /// Suppress progress output
    #[arg(short = 'q', long, conflicts_with = "verbose")]
    pub quiet: bool,
    /// Also report what is skipped on purpose
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
}

#[derive(Args, Debug)]
pub struct UpdateArgs {
    #[command(flatten)]
    pub common: CommonArgs,
    /// Rehash all files regardless of cached metadata
    #[arg(long)]
    pub rehash: bool,
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
