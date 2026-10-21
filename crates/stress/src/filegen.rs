//! Wrappers for the `filegen` binary's `run` verb.

use std::ffi::OsString;
use std::path::Path;
use std::process::{Command, Stdio};

use anyhow::{bail, Context, Result};

use crate::process;

pub struct FilegenOpts<'a> {
    pub steps: u64,
    pub max_total_size: Option<&'a str>,
    pub max_file_size: Option<&'a str>,
    pub sets: &'a [String],
}

fn common_flags(args: &mut Vec<OsString>, o: &FilegenOpts) {
    args.push("-q".into());
    if let Some(s) = o.max_total_size {
        args.extend(["--max-total-size".into(), s.into()]);
    }
    if let Some(s) = o.max_file_size {
        args.extend(["--max-file-size".into(), s.into()]);
    }
    for kv in o.sets {
        args.extend(["--set".into(), kv.into()]);
    }
}

/// `run --seed <seed> --steps <n> --out-dir <out_dir> -q [--max-total-size S]
/// [--max-file-size S] [--set KV]... <tree>`.
pub fn fill_args(seed: u64, out_dir: &Path, tree: &Path, o: &FilegenOpts) -> Vec<OsString> {
    let mut args: Vec<OsString> = vec![
        "run".into(),
        "--seed".into(),
        seed.to_string().into(),
        "--steps".into(),
        o.steps.to_string().into(),
        "--out-dir".into(),
        out_dir.into(),
    ];
    common_flags(&mut args, o);
    args.push(tree.into());
    args
}

/// `run --resume <state> --steps <n> -q [same size flags and sets] <tree>`.
pub fn mutate_args(state: &Path, tree: &Path, o: &FilegenOpts) -> Vec<OsString> {
    let mut args: Vec<OsString> = vec![
        "run".into(),
        "--resume".into(),
        state.into(),
        "--steps".into(),
        o.steps.to_string().into(),
    ];
    common_flags(&mut args, o);
    args.push(tree.into());
    args
}

/// Runs `<filegen> <args>` with stdin null and stderr inherited; a non-zero
/// exit is an error naming the exit status.
pub fn run(filegen: &Path, args: &[OsString]) -> Result<()> {
    let status = process::status(
        Command::new(filegen)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit()),
    )
    .with_context(|| format!("running {}", filegen.display()))?;
    if !status.success() {
        bail!("filegen run failed ({status})");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;
    use std::os::unix::fs::PermissionsExt;
    use std::path::Path;

    use super::*;

    fn strs(v: Vec<OsString>) -> Vec<String> {
        v.into_iter()
            .map(|s| s.to_string_lossy().into_owned())
            .collect()
    }

    fn fake(dir: &Path, name: &str, body: &str) -> std::path::PathBuf {
        let p = dir.join(name);
        std::fs::write(&p, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        p
    }

    #[test]
    fn fill_and_mutate_args() {
        let o = FilegenOpts {
            steps: 100,
            max_total_size: Some("8M"),
            max_file_size: Some("1M"),
            sets: &["names.max_len=200".into()],
        };
        let a = strs(fill_args(
            42,
            Path::new("/r/filegen"),
            Path::new("/r/SRC/data"),
            &o,
        ));
        assert_eq!(
            a,
            [
                "run",
                "--seed",
                "42",
                "--steps",
                "100",
                "--out-dir",
                "/r/filegen",
                "-q",
                "--max-total-size",
                "8M",
                "--max-file-size",
                "1M",
                "--set",
                "names.max_len=200",
                "/r/SRC/data"
            ]
        );
        let o2 = FilegenOpts {
            steps: 5,
            max_total_size: None,
            max_file_size: None,
            sets: &[],
        };
        let m = strs(mutate_args(
            Path::new("/r/filegen/filegen-42.state.yaml"),
            Path::new("/r/SRC/data"),
            &o2,
        ));
        assert_eq!(
            m,
            [
                "run",
                "--resume",
                "/r/filegen/filegen-42.state.yaml",
                "--steps",
                "5",
                "-q",
                "/r/SRC/data"
            ]
        );
    }

    #[test]
    fn non_zero_exit_is_an_error_naming_the_code() {
        let t = tempfile::tempdir().unwrap();
        let ok = fake(t.path(), "ok.sh", "exit 0");
        run(&ok, &["run".into()]).unwrap();
        let bad = fake(t.path(), "bad.sh", "exit 2");
        let e = run(&bad, &["run".into()]).unwrap_err();
        assert!(e.to_string().contains('2'), "{e}");
    }
}
