#![forbid(unsafe_code)]

use std::fs::File;
use std::io::{BufReader, BufWriter};
use std::process::ExitCode;

use anyhow::Context as _;
use pbwalk::{Args, Parser as _, Summary};

fn main() -> ExitCode {
    let args = Args::parse();
    let summary = match walk(&args) {
        Ok(summary) => summary,
        Err(e) => {
            eprintln!("pbwalk: {e:#}");
            return ExitCode::from(2);
        }
    };
    if summary.mismatches > 0 {
        eprintln!("pbwalk: {} message(s) do not add up", summary.mismatches);
    }
    if summary.truncated {
        eprintln!("pbwalk: the capture is cut off; totals cover what was read");
    }
    if let Some(why) = &summary.stopped {
        eprintln!("pbwalk: stopped reading: {why}; totals cover what was read");
    }
    if summary.mismatches > 0 || summary.truncated || summary.stopped.is_some() {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    }
}

fn walk(args: &Args) -> anyhow::Result<Summary> {
    let file =
        File::open(&args.file).with_context(|| format!("{}: cannot open", args.file.display()))?;
    let mut out = BufWriter::new(std::io::stdout().lock());
    pbwalk::run(args, BufReader::new(file), &mut out)
}
