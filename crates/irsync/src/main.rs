use irsync::cli::{Args, Command, DaemonArgs, SyncOptions};
use irsync::config::daemon::DaemonConfig;

fn main() {
    let args = Args::parse();
    match &args.cmd {
        Some(Command::Daemon(d)) => std::process::exit(run_daemon(d, args.sync.verbose)),
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

fn run_daemon(d: &DaemonArgs, verbose: u8) -> i32 {
    let default = match verbose {
        0 => "info",
        1 => "debug",
        _ => "trace",
    };
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(default));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .init();

    let result = (|| -> anyhow::Result<()> {
        let cfg = DaemonConfig::load(d.config.as_deref())?;
        if d.show_id {
            println!("{}", irsync::daemon::show_id(&cfg)?);
            return Ok(());
        }
        let rt = tokio::runtime::Runtime::new()?;
        rt.block_on(async {
            let shutdown = tokio_util::sync::CancellationToken::new();
            let token = shutdown.clone();
            tokio::spawn(async move {
                let _ = tokio::signal::ctrl_c().await;
                token.cancel();
            });
            irsync::daemon::run_from_config(cfg, shutdown).await
        })
    })();
    match result {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("irsync daemon: {e:#}");
            1
        }
    }
}
