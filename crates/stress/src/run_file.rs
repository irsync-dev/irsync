//! The `run.toml` file recording how a run was configured.

use std::path::{Path, PathBuf};

use anyhow::Context;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug, PartialEq, Clone)]
pub struct ToolInfo {
    pub path: String,
    pub version: String,
}

#[derive(Serialize, Deserialize, Debug, PartialEq, Clone)]
pub struct ToolVersions {
    pub irsync: ToolInfo,
    pub filegen: ToolInfo,
    pub fileset: ToolInfo,
}

#[derive(Serialize, Deserialize, Debug, PartialEq, Clone)]
pub struct DaemonInfo {
    pub endpoint_id: String,
    pub address: String,
    pub pid: u32,
}

#[derive(Serialize, Deserialize, Debug, PartialEq)]
pub struct RunFile {
    /// RFC 3339.
    pub started: String,
    pub seed: u64,
    /// "push" | "pull".
    pub direction: String,
    pub iterations: u64,
    pub steps: u64,
    pub max_total_size: Option<String>,
    pub max_file_size: Option<String>,
    pub filegen_set: Vec<String>,
    pub log_level: String,
    pub keep_going: bool,
    /// Lossy UTF-8 of the OsStrings.
    pub client_args: Vec<String>,
    /// `--concurrent`: filegen writes to the source during each sync.
    #[serde(default)]
    pub concurrent: bool,
    /// `--pace`, when one pace is fixed for the whole run.
    #[serde(default)]
    pub pace: Option<String>,
    /// `--replay-rounds`: the rounds file this run replays.
    #[serde(default)]
    pub replay_rounds: Option<String>,
    pub tools: ToolVersions,
    /// Filled in once the daemon is known.
    pub daemon: Option<DaemonInfo>,
}

impl RunFile {
    /// Writes atomically: a sibling temp file, then a rename.
    pub fn write(&self, path: &Path) -> anyhow::Result<()> {
        write_toml(self, path)
    }

    pub fn read(path: &Path) -> anyhow::Result<RunFile> {
        read_toml(path)
    }
}

/// Writes `value` as TOML atomically: a sibling temp file, then a rename.
pub fn write_toml<T: Serialize>(value: &T, path: &Path) -> anyhow::Result<()> {
    let text = toml::to_string_pretty(value).context("serialising run.toml")?;
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    std::fs::write(&tmp, text).with_context(|| format!("writing {}", tmp.display()))?;
    std::fs::rename(&tmp, path)
        .with_context(|| format!("renaming {} to {}", tmp.display(), path.display()))?;
    Ok(())
}

pub fn read_toml<T: DeserializeOwned>(path: &Path) -> anyhow::Result<T> {
    let text =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tool(n: &str) -> ToolInfo {
        ToolInfo {
            path: format!("/bin/{n}"),
            version: format!("{n} 1.2.3"),
        }
    }

    fn sample() -> RunFile {
        RunFile {
            started: "2026-10-21T12:00:00Z".into(),
            seed: 42,
            direction: "push".into(),
            iterations: 10,
            steps: 3,
            max_total_size: Some("8M".into()),
            max_file_size: None,
            filegen_set: vec!["a=1".into(), "b=2".into()],
            log_level: "debug".into(),
            keep_going: true,
            client_args: vec!["--x".into(), "y z".into()],
            concurrent: true,
            pace: Some("steady:40".into()),
            replay_rounds: Some("/r/logs/rounds.tsv".into()),
            tools: ToolVersions {
                irsync: tool("irsync"),
                filegen: tool("filegen"),
                fileset: tool("fileset"),
            },
            daemon: Some(DaemonInfo {
                endpoint_id: "abc".into(),
                address: "127.0.0.1:1234".into(),
                pid: 4321,
            }),
        }
    }

    #[test]
    fn run_file_round_trips() {
        let t = tempfile::tempdir().unwrap();
        let r = sample();
        let p = t.path().join("run.toml");
        r.write(&p).unwrap();
        assert_eq!(RunFile::read(&p).unwrap(), r);
        assert!(!t.path().join("run.toml.tmp").exists());
    }

    #[test]
    fn run_file_round_trips_without_daemon() {
        let t = tempfile::tempdir().unwrap();
        let mut r = sample();
        r.daemon = None;
        r.concurrent = false;
        r.pace = None;
        r.replay_rounds = None;
        let p = t.path().join("run.toml");
        r.write(&p).unwrap();
        assert_eq!(RunFile::read(&p).unwrap(), r);
    }
}
