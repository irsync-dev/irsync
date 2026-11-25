use std::process::exit;

use clap::Parser;
use stress::cli::Args;
use stress::run::run;

fn main() {
    let cli = Args::parse().into_cli();
    match run(&cli) {
        Ok(code) => exit(code),
        Err(e) => {
            eprintln!("irsync-stress: {e:#}");
            exit(2);
        }
    }
}
