#![forbid(unsafe_code)]

use std::process::exit;

use clap::Parser;
use netstress::cli::Cli;
use netstress::run::run;

fn main() {
    let cli = Cli::parse();
    match run(&cli) {
        Ok(code) => exit(code),
        Err(e) => {
            eprintln!("irsync-netstress: {e:#}");
            exit(2);
        }
    }
}
