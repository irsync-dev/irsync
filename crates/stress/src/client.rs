//! The client side of one sync: setup directory, argument vector, running it.

use std::ffi::OsString;
use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};

use crate::process;

use crate::cli::Direction;
use crate::layout::Layout;

/// Client-side files and directories the sync needs, all under the client's config dir.
#[derive(Debug, Clone)]
pub struct ClientSetup {
    pub identity: PathBuf,
    /// An empty `irsync.toml`, passed as `--config`.
    pub config: PathBuf,
    pub home: PathBuf,
    pub xdg_state: PathBuf,
}

/// Creates the empty config file and the `home/` and `xdg/` directories under
/// `config_dir`. `identity` comes from `client_keygen`.
pub fn setup(config_dir: &Path, identity: PathBuf) -> Result<ClientSetup> {
    let config = config_dir.join("irsync.toml");
    File::create(&config).with_context(|| format!("creating {}", config.display()))?;
    let home = config_dir.join("home");
    let xdg_state = config_dir.join("xdg");
    for d in [&home, &xdg_state] {
        std::fs::create_dir_all(d).with_context(|| format!("creating {}", d.display()))?;
    }
    Ok(ClientSetup {
        identity,
        config,
        home,
        xdg_state,
    })
}

fn dir_arg(p: &Path) -> OsString {
    let mut s = p.as_os_str().to_owned();
    s.push("/");
    s
}

/// The complete argument vector (without the program): the fixed flag list,
/// then `extra`, then source and destination.
pub fn sync_args(
    d: Direction,
    layout: &Layout,
    endpoint_id: &str,
    port: u16,
    setup: &ClientSetup,
    extra: &[OsString],
) -> Vec<OsString> {
    let mut a: Vec<OsString> = ["-a", "-H", "--delete", "--reconnect", "0", "--address"]
        .into_iter()
        .map(OsString::from)
        .collect();
    a.push(format!("127.0.0.1:{port}").into());
    a.push("--identity".into());
    a.push(setup.identity.clone().into());
    a.push("--config".into());
    a.push(setup.config.clone().into());
    a.extend(extra.iter().cloned());
    let remote = OsString::from(format!("{endpoint_id}:data/"));
    let (_, data) = layout.client_side(d);
    match d {
        Direction::Push => {
            a.push(dir_arg(data));
            a.push(remote);
        }
        Direction::Pull => {
            a.push(remote);
            a.push(dir_arg(data));
        }
    }
    a
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncExit {
    Ok,
    /// Exit code 23.
    Partial,
    /// Any other exit code; -1 if killed by a signal.
    Other(i32),
}

impl SyncExit {
    /// The client's exit code; -1 if it was killed by a signal.
    pub fn code(self) -> i32 {
        match self {
            SyncExit::Ok => 0,
            SyncExit::Partial => 23,
            SyncExit::Other(c) => c,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct SyncResult {
    pub exit: SyncExit,
    pub elapsed: Duration,
}

/// Runs the client with stdin null and stdout and stderr appended to `log`.
pub fn run_sync(
    irsync: &Path,
    args: &[OsString],
    setup: &ClientSetup,
    log_level: &str,
    log: &Path,
) -> Result<SyncResult> {
    let out = OpenOptions::new()
        .create(true)
        .append(true)
        .open(log)
        .with_context(|| format!("opening {}", log.display()))?;
    let err = out.try_clone().context("cloning log handle")?;
    let start = Instant::now();
    let status = process::status(
        Command::new(irsync)
            .args(args)
            .env("HOME", &setup.home)
            .env("XDG_STATE_HOME", &setup.xdg_state)
            .env_remove("IRSYNC_PASSPHRASE")
            .env("RUST_LOG", log_level)
            .stdin(Stdio::null())
            .stdout(Stdio::from(out))
            .stderr(Stdio::from(err)),
    )
    .with_context(|| format!("running {}", irsync.display()))?;
    let elapsed = start.elapsed();
    let exit = match status.code() {
        Some(0) => SyncExit::Ok,
        Some(23) => SyncExit::Partial,
        Some(c) => SyncExit::Other(c),
        None => SyncExit::Other(-1),
    };
    Ok(SyncResult { exit, elapsed })
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use super::*;

    fn strings(a: &[OsString]) -> Vec<String> {
        a.iter().map(|o| o.to_string_lossy().into_owned()).collect()
    }

    #[test]
    fn push_args_in_spec_order() {
        let t = tempfile::tempdir().unwrap();
        let l = Layout::create(&t.path().join("r")).unwrap();
        let s = setup(&l.src_config, l.src_config.join("id_ed25519")).unwrap();
        let a = sync_args(
            Direction::Push,
            &l,
            "ID",
            4321,
            &s,
            &["--parallel".into(), "4".into()],
        );
        let a = strings(&a);
        let expect_prefix = [
            "-a",
            "-H",
            "--delete",
            "--reconnect",
            "0",
            "--address",
            "127.0.0.1:4321",
            "--identity",
            s.identity.to_str().unwrap(),
            "--config",
            s.config.to_str().unwrap(),
            "--parallel",
            "4",
        ];
        assert_eq!(&a[..expect_prefix.len()], &expect_prefix);
        assert_eq!(
            &a[expect_prefix.len()..],
            [format!("{}/", l.src_data.display()), "ID:data/".to_string()]
        );
        assert!(s.config.is_file() && s.home.is_dir() && s.xdg_state.is_dir());
        assert_eq!(std::fs::metadata(&s.config).unwrap().len(), 0);
    }

    #[test]
    fn pull_swaps_source_and_destination() {
        let t = tempfile::tempdir().unwrap();
        let l = Layout::create(&t.path().join("r")).unwrap();
        let s = setup(&l.dst_config, l.dst_config.join("id_ed25519")).unwrap();
        let a = strings(&sync_args(Direction::Pull, &l, "ID", 99, &s, &[]));
        assert_eq!(a.len(), 13);
        assert_eq!(a[6], "127.0.0.1:99");
        assert!(s.identity.starts_with(&l.dst_config) && s.config.starts_with(&l.dst_config));
        assert_eq!(
            &a[11..],
            ["ID:data/".to_string(), format!("{}/", l.dst_data.display())]
        );
    }

    #[test]
    fn exit_codes_classify() {
        let t = tempfile::tempdir().unwrap();
        let l = Layout::create(&t.path().join("r")).unwrap();
        let s = setup(&l.src_config, l.src_config.join("id_ed25519")).unwrap();
        for n in [0, 23, 1] {
            let script = t.path().join(format!("fake-{n}.sh"));
            std::fs::write(&script, format!("#!/bin/sh\necho err-{n} >&2\nexit {n}\n")).unwrap();
            std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
            let log = t.path().join(format!("log-{n}"));
            let r = run_sync(&script, &[], &s, "info", &log).unwrap();
            match n {
                0 => assert_eq!(r.exit, SyncExit::Ok),
                23 => assert_eq!(r.exit, SyncExit::Partial),
                _ => assert_eq!(r.exit, SyncExit::Other(1)),
            }
            assert!(std::fs::read_to_string(&log)
                .unwrap()
                .contains(&format!("err-{n}")));
        }
    }
}
