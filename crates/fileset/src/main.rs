use std::io::{BufWriter, IsTerminal, Write};

use clap::Parser;
use fileset::cli::{BackendArg, Cli, Command, CommonArgs};
use fileset::compare::{Field, Tier};
use fileset::engine::check::{check, CheckOpts};
use fileset::engine::list::{list, ListOpts};
use fileset::engine::update::{update, Mode, UpdateOpts};
use fileset::engine::{CommonOpts, Outcome};
use fileset::platform;
use fileset::progress::{NoProgress, Progress, TermProgress};
use fileset::store::Backend;

/// The engine options, and whether to draw the spinner.
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
    let spinner = show_spinner(a.quiet, a.verbose, std::io::stderr().is_terminal());
    (opts, spinner)
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

/// The spinner is drawn on a terminal, unless `-q` asks for silence or `-v` prints a line
/// per entry: the lines and the spinner would share the screen.
fn show_spinner(quiet: bool, verbose: bool, stderr_is_terminal: bool) -> bool {
    !quiet && !verbose && stderr_is_terminal
}

/// `jobs` is the number of hasher threads, shown when busy cores cannot be measured.
fn progress_for(spinner: bool, jobs: usize) -> Box<dyn Progress> {
    if spinner {
        Box::new(TermProgress::new(jobs))
    } else {
        Box::new(NoProgress)
    }
}

fn main() {
    let cli = Cli::parse();
    let mut out = BufWriter::new(std::io::stdout().lock());
    let mut err = std::io::stderr();
    // A listing cut short by its reader (`fileset list ... | head`) is not a failure.
    let listing = matches!(cli.command, Command::List(_));
    let result: anyhow::Result<Outcome> = match cli.command {
        Command::Init(a) => {
            let (common, spinner) = common_opts(a.common);
            let backend = a.store.map(|b| match b {
                BackendArg::Sqlite => Backend::Sqlite,
                BackendArg::Fjall => Backend::Fjall,
                BackendArg::Redb => Backend::Redb,
            });
            let opts = UpdateOpts {
                common,
                mode: Mode::Init { backend },
                compact: !a.no_compaction,
            };
            update(
                &opts,
                &mut out,
                &mut err,
                progress_for(spinner, opts.common.jobs).as_ref(),
            )
        }
        Command::Update(a) => {
            let (common, spinner) = common_opts(a.common);
            let opts = UpdateOpts {
                common,
                mode: Mode::Update {
                    rehash: a.rehash,
                    force_xattr_reads: a.force_xattr_reads,
                },
                compact: !a.no_compaction,
            };
            update(
                &opts,
                &mut out,
                &mut err,
                progress_for(spinner, opts.common.jobs).as_ref(),
            )
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
            let (common, spinner) = common_opts(a.common);
            let opts = CheckOpts {
                common,
                tier,
                ignore,
                require,
            };
            check(
                &opts,
                &mut out,
                &mut err,
                progress_for(spinner, opts.common.jobs).as_ref(),
            )
        }
        Command::List(a) => {
            if !a.extra.is_empty() {
                fail("more than one pattern; quote the pattern so the shell does not expand it");
            }
            let opts = ListOpts {
                root: a.path,
                manifest: a.manifest,
                pattern: a.pattern.map(|p| platform::os_to_bytes(&p)),
                verbose: a.verbose,
            };
            list(&opts, &mut out)
        }
    };
    let flushed = match out.flush() {
        Err(e) if listing && e.kind() == std::io::ErrorKind::BrokenPipe => Ok(()),
        other => other,
    };
    match result {
        Ok(outcome) if flushed.is_ok() => std::process::exit(outcome.exit_code()),
        Ok(_) => fail("cannot write to stdout"),
        Err(e) => fail(&format!("{e:#}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spinner_only_on_a_terminal_without_quiet_or_verbose() {
        assert!(show_spinner(false, false, true));
        assert!(!show_spinner(false, true, true));
        assert!(!show_spinner(true, false, true));
        assert!(!show_spinner(false, false, false));
    }
}
