use clap::Parser;
use fileset::cli::{Cli, Command};

fn main() {
    let cli = Cli::parse();
    match cli.command {
        Command::Update(_) | Command::Check(_) => {
            eprintln!("not implemented");
            std::process::exit(2);
        }
    }
}
