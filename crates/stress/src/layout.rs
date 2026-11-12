//! On-disk layout of one stress run.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context};

use crate::cli::Direction;

/// Every directory and file name a run uses, under one canonical root.
#[derive(Debug, Clone)]
pub struct Layout {
    pub root: PathBuf,
    pub run_file: PathBuf,
    pub src_config: PathBuf,
    pub src_data: PathBuf,
    pub dst_config: PathBuf,
    pub dst_data: PathBuf,
    pub filegen_dir: PathBuf,
    pub logs: PathBuf,
}

impl Layout {
    /// Creates `root` if missing; errors if it exists and is not an empty directory.
    /// Creates every subdirectory.
    pub fn create(root: &Path) -> anyhow::Result<Layout> {
        if root.exists() {
            if !root.is_dir() {
                bail!("{} exists and is not a directory", root.display());
            }
            let mut entries =
                std::fs::read_dir(root).with_context(|| format!("reading {}", root.display()))?;
            if entries.next().is_some() {
                bail!("{} is not empty", root.display());
            }
        }
        std::fs::create_dir_all(root).with_context(|| format!("creating {}", root.display()))?;
        let root = root
            .canonicalize()
            .with_context(|| format!("canonicalising {}", root.display()))?;
        let layout = Layout {
            run_file: root.join("run.toml"),
            src_config: root.join("SRC/config"),
            src_data: root.join("SRC/data"),
            dst_config: root.join("DST/config"),
            dst_data: root.join("DST/data"),
            filegen_dir: root.join("filegen"),
            logs: root.join("logs"),
            root,
        };
        for d in [
            &layout.src_config,
            &layout.src_data,
            &layout.dst_config,
            &layout.dst_data,
            &layout.filegen_dir,
            &layout.logs,
        ] {
            std::fs::create_dir_all(d).with_context(|| format!("creating {}", d.display()))?;
        }
        Ok(layout)
    }

    pub fn daemon_log(&self) -> PathBuf {
        self.logs.join("daemon.log")
    }

    /// filegen's stderr, appended to by every filegen run.
    pub fn filegen_stderr_log(&self) -> PathBuf {
        self.logs.join("filegen.stderr.log")
    }

    /// filegen's own log, which it writes under its `--out-dir`.
    pub fn filegen_log(&self, seed: u64) -> PathBuf {
        self.filegen_dir.join(format!("filegen-{seed}.log"))
    }

    pub fn ping_client_log(&self) -> PathBuf {
        self.logs.join("ping.client.log")
    }

    pub fn client_log(&self, iter: u64) -> PathBuf {
        self.logs.join(format!("iter-{iter:04}.client.log"))
    }

    pub fn check_report(&self, iter: u64) -> PathBuf {
        self.logs.join(format!("iter-{iter:04}.check.txt"))
    }

    pub fn manifest_hashes(&self, iter: u64) -> PathBuf {
        self.logs.join(format!("iter-{iter:04}.manifest.blake3"))
    }

    /// The concurrent mode's per-round scratch: `start/` (the source at
    /// round start), `work/` and `replay/` (the replay's tree and its
    /// filegen dir) and `state.yaml` (filegen's state at round start).
    pub fn round_dir(&self) -> PathBuf {
        self.root.join("round")
    }

    /// Where a failed round's `round/` is moved.
    pub fn failed_round_dir(&self, iter: u64) -> PathBuf {
        self.root.join(format!("round-{iter:04}"))
    }

    /// One `round\tpace\tsteps` line per concurrent round.
    pub fn rounds_file(&self) -> PathBuf {
        self.logs.join("rounds.tsv")
    }

    /// The client's file report (`IRSYNC_FILE_REPORT`) of a concurrent sync.
    pub fn client_report(&self, iter: u64) -> PathBuf {
        self.logs
            .join(format!("iter-{iter:04}.client.report.jsonl"))
    }

    /// The daemon's file report, appended to for the whole run.
    pub fn daemon_report(&self) -> PathBuf {
        self.logs.join("daemon.report.jsonl")
    }

    /// The client log of the quiet sync after a concurrent round.
    pub fn quiet_client_log(&self, iter: u64) -> PathBuf {
        self.logs.join(format!("iter-{iter:04}.quiet.client.log"))
    }

    /// The judge's failures for a concurrent round.
    pub fn judge_report(&self, iter: u64) -> PathBuf {
        self.logs.join(format!("iter-{iter:04}.judge.txt"))
    }

    pub fn ping_file(&self) -> PathBuf {
        self.src_data.join(".irsync-stress-ping")
    }

    pub fn filegen_state(&self, seed: u64) -> PathBuf {
        self.filegen_dir.join(format!("filegen-{seed}.state.yaml"))
    }

    /// (config dir, data dir) of the side the daemon serves: the destination
    /// for a push, the source for a pull.
    pub fn daemon_side(&self, d: Direction) -> (&Path, &Path) {
        match d {
            Direction::Push => (&self.dst_config, &self.dst_data),
            Direction::Pull => (&self.src_config, &self.src_data),
        }
    }

    /// (config dir, data dir) of the side the client runs on.
    pub fn client_side(&self, d: Direction) -> (&Path, &Path) {
        match d {
            Direction::Push => (&self.src_config, &self.src_data),
            Direction::Pull => (&self.dst_config, &self.dst_data),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One `..` per component of `current_dir()`, then `abs` without its leading `/`.
    fn relative_to_cwd(abs: &Path) -> PathBuf {
        let depth = std::env::current_dir().unwrap().components().count() - 1;
        let mut rel = PathBuf::new();
        for _ in 0..depth {
            rel.push("..");
        }
        rel.join(abs.strip_prefix("/").unwrap())
    }

    #[test]
    fn layout_canonicalises_root() {
        let t = tempfile::tempdir().unwrap();
        let rel = relative_to_cwd(&t.path().join("run dir"));
        assert!(rel.is_relative());
        let l = Layout::create(&rel).unwrap();
        assert!(l.root.is_absolute());
        assert!(l.src_data.ends_with("SRC/data") && l.dst_config.ends_with("DST/config"));
        for d in [
            &l.src_config,
            &l.src_data,
            &l.dst_config,
            &l.dst_data,
            &l.filegen_dir,
            &l.logs,
        ] {
            assert!(d.is_dir());
        }
    }

    #[test]
    fn non_empty_root_is_refused_and_missing_root_is_created() {
        let t = tempfile::tempdir().unwrap();
        std::fs::write(t.path().join("x"), b"").unwrap();
        assert!(Layout::create(t.path()).is_err());
        let fresh = t.path().join("new/root");
        assert!(Layout::create(&fresh).is_ok());
    }

    #[test]
    fn names_follow_the_spec() {
        let t = tempfile::tempdir().unwrap();
        let l = Layout::create(&t.path().join("r")).unwrap();
        assert_eq!(l.client_log(1).file_name().unwrap(), "iter-0001.client.log");
        assert_eq!(
            l.check_report(12).file_name().unwrap(),
            "iter-0012.check.txt"
        );
        assert_eq!(
            l.manifest_hashes(3).file_name().unwrap(),
            "iter-0003.manifest.blake3"
        );
        assert_eq!(
            l.filegen_state(42).file_name().unwrap(),
            "filegen-42.state.yaml"
        );
        assert_eq!(l.daemon_side(Direction::Push).1, l.dst_data);
        assert_eq!(l.daemon_side(Direction::Pull).1, l.src_data);
        assert_eq!(l.client_side(Direction::Push).0, l.src_config);
        assert_eq!(l.client_side(Direction::Pull).1, l.dst_data);
        assert!(l.ping_file().ends_with("SRC/data/.irsync-stress-ping"));
        assert!(l.daemon_log().ends_with("logs/daemon.log"));
        assert!(l.ping_client_log().ends_with("logs/ping.client.log"));
        assert!(l.filegen_stderr_log().ends_with("logs/filegen.stderr.log"));
        assert!(l.filegen_log(42).ends_with("filegen/filegen-42.log"));
        assert_eq!(l.round_dir(), l.root.join("round"));
        assert_eq!(l.failed_round_dir(7), l.root.join("round-0007"));
        assert!(l.rounds_file().ends_with("logs/rounds.tsv"));
        assert!(l
            .client_report(2)
            .ends_with("logs/iter-0002.client.report.jsonl"));
        assert!(l.daemon_report().ends_with("logs/daemon.report.jsonl"));
        assert!(l
            .quiet_client_log(3)
            .ends_with("logs/iter-0003.quiet.client.log"));
        assert!(l.judge_report(4).ends_with("logs/iter-0004.judge.txt"));
    }
}
