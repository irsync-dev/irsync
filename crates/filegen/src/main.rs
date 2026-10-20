use std::process::exit;

use clap::error::ErrorKind;
use clap::Parser;
use filegen::cli::{parse_set, Cli, Command, RunArgs};
use filegen::run::{run, Outcome, RunError, RunOpts, Start};

fn main() {
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(e) => {
            let code = match e.kind() {
                ErrorKind::DisplayHelp | ErrorKind::DisplayVersion => 0,
                _ => 2,
            };
            let _ = e.print();
            exit(code);
        }
    };
    match cli.command {
        Command::Run(args) => exit(run_command(args)),
    }
}

/// Runs `filegen run`; returns the exit code.
fn run_command(args: RunArgs) -> i32 {
    let sets = match args.set.iter().map(|s| parse_set(s)).collect() {
        Ok(sets) => sets,
        Err(msg) => {
            eprintln!("filegen: --set: {msg}");
            return 2;
        }
    };
    let start = match (args.seed, &args.resume) {
        (Some(seed), _) => Start::Seed(seed),
        (None, Some(state)) => Start::Resume(state.clone()),
        (None, None) => unreachable!("clap requires --seed or --resume"),
    };
    let opts = RunOpts {
        root: args.path.clone(),
        steps: args.steps,
        start,
        config_file: args.config.clone(),
        sets,
        dry_run: args.dry_run,
        force: args.force,
        quiet: args.quiet,
        verbose: args.verbose,
        flags: args,
    };
    match run(opts) {
        Ok(Outcome::Ok) => 0,
        Ok(Outcome::Failed(msg)) => {
            eprintln!("filegen: failed: {msg}");
            1
        }
        Ok(Outcome::Interrupted(sig)) => 128 + sig,
        Err(e) => {
            eprintln!("filegen: {e}");
            match e {
                RunError::Usage(_) | RunError::Config(_) | RunError::State { .. } => 2,
                RunError::Io(_) => 1,
            }
        }
    }
}
