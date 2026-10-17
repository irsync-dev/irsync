use irsync::cli::{Args, Command, SyncOptions};

fn main() {
    let args = Args::parse();
    match &args.cmd {
        Some(Command::Daemon(_)) => eprintln!("daemon: not implemented yet"),
        Some(Command::Keygen { .. }) => eprintln!("keygen: not implemented yet"),
        Some(Command::Gc { .. }) => eprintln!("gc: not implemented yet"),
        None => {
            let opts = SyncOptions::from_args(&args);
            if opts.dry_run {
                println!("{opts:#?}");
                return;
            }
            eprintln!("not implemented yet");
        }
    }
    std::process::exit(1);
}
