use std::process::exit;

use clap::error::ErrorKind;
use clap::Parser;
use filegen::cli::{Cli, Command, RunArgs};
use filegen::run::{opts_from_args, run, Outcome, RunError};

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
    let opts = match opts_from_args(args) {
        Ok(opts) => opts,
        Err(msg) => {
            eprintln!("{msg}");
            return 2;
        }
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
