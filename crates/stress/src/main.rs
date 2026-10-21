use std::process::exit;

use clap::error::ErrorKind;
use clap::Parser;
use stress::cli::Cli;

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
    println!("{cli:?}");
}
