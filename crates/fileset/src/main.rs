use std::io::{BufWriter, IsTerminal, Write};

use clap::Parser;
use fileset::cli::{BackendArg, Cli, Command, CommonArgs};
use fileset::compare::{Field, Tier};
use fileset::engine::check::{check, CheckOpts};
use fileset::engine::update::{update, Mode, UpdateOpts};
use fileset::engine::{CommonOpts, Outcome};
use fileset::progress::{NoProgress, Progress, TermProgress};
use fileset::store::Backend;

fn common_opts(a: CommonArgs) -> (CommonOpts, bool) {
    let jobs = a
        .jobs
        .unwrap_or_else(|| std::thread::available_parallelism().map_or(1, |n| n.get()));
    let opts = CommonOpts {
        root: a.path,
        manifest: a.manifest,
        cross_mount_points: a.cross_mount_points,
        sort: !a.no_dir_sort,
        jobs,
        walk_threads: a.walk_threads,
        fail_on_warnings: a.fail_on_warnings,
        builtin_excludes: !a.no_builtin_excludes,
        verbose: a.verbose,
    };
    (opts, a.quiet)
}

fn parse_fields(names: &[String]) -> Result<Vec<Field>, String> {
    names
        .iter()
        .map(|n| {
            Field::parse(n).ok_or_else(|| {
                let all: Vec<&str> = Field::ALL.iter().map(|f| f.name()).collect();
                format!("unknown field '{n}', expected one of: {}", all.join(" "))
            })
        })
        .collect()
}

/// Exits with 2. Safe while `main` holds the stdout lock: only stderr is written.
fn fail(msg: &str) -> ! {
    eprintln!("error: {msg}");
    std::process::exit(2);
}

fn progress_for(quiet: bool) -> Box<dyn Progress> {
    if !quiet && std::io::stderr().is_terminal() {
        Box::new(TermProgress::new())
    } else {
        Box::new(NoProgress)
    }
}

fn main() {
    let cli = Cli::parse();
    let mut out = BufWriter::new(std::io::stdout().lock());
    let mut err = std::io::stderr();
    let result: anyhow::Result<Outcome> = match cli.command {
        Command::Init(a) => {
            let (common, quiet) = common_opts(a.common);
            let backend = a.store.map(|b| match b {
                BackendArg::Sqlite => Backend::Sqlite,
                BackendArg::Fjall => Backend::Fjall,
            });
            let opts = UpdateOpts {
                common,
                mode: Mode::Init { backend },
            };
            update(&opts, &mut out, &mut err, progress_for(quiet).as_ref())
        }
        Command::Update(a) => {
            let (common, quiet) = common_opts(a.common);
            let opts = UpdateOpts {
                common,
                mode: Mode::Update { rehash: a.rehash },
            };
            update(&opts, &mut out, &mut err, progress_for(quiet).as_ref())
        }
        Command::Check(a) => {
            let tier = if a.relaxed {
                Tier::Relaxed
            } else if a.paranoid {
                Tier::Paranoid
            } else {
                Tier::Default
            };
            let ignore = parse_fields(&a.ignore).unwrap_or_else(|m| fail(&m));
            let require = parse_fields(&a.require).unwrap_or_else(|m| fail(&m));
            let (common, quiet) = common_opts(a.common);
            let opts = CheckOpts {
                common,
                tier,
                ignore,
                require,
            };
            check(&opts, &mut out, &mut err, progress_for(quiet).as_ref())
        }
    };
    let flushed = out.flush();
    match result {
        Ok(outcome) if flushed.is_ok() => std::process::exit(outcome.exit_code()),
        Ok(_) => fail("cannot write to stdout"),
        Err(e) => fail(&format!("{e:#}")),
    }
}
