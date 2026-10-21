use std::process::exit;

use clap::error::ErrorKind;
use clap::Parser;
use stress::cli::Cli;
use stress::run::run;

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
    match run(&cli) {
        Ok(code) => exit(code),
        Err(e) => {
            eprintln!("irsync-stress: {e:#}");
            exit(2);
        }
    }
}
