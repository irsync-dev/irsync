//! The `run.toml` file: irsync-stress's fields plus the network section.

use std::path::{Path, PathBuf};

use anyhow::Context;
use serde::{Deserialize, Serialize};
use stress::run_file::RunFile;

#[derive(Serialize, Deserialize, Debug, PartialEq, Clone)]
pub struct Network {
    pub profile: String,
    pub client_timeout_secs: u64,
    /// The client's `--reconnect` window, like "5m".
    pub reconnect: String,
}

#[derive(Serialize, Deserialize, Debug, PartialEq)]
pub struct NetRunFile {
    #[serde(flatten)]
    pub base: RunFile,
    pub network: Network,
}

impl NetRunFile {
    /// Writes atomically: a sibling temp file, then a rename.
    pub fn write(&self, path: &Path) -> anyhow::Result<()> {
        let text = toml::to_string_pretty(self).context("serialising run.toml")?;
        let mut tmp = path.as_os_str().to_owned();
        tmp.push(".tmp");
        let tmp = PathBuf::from(tmp);
        std::fs::write(&tmp, text).with_context(|| format!("writing {}", tmp.display()))?;
        std::fs::rename(&tmp, path)
            .with_context(|| format!("renaming {} to {}", tmp.display(), path.display()))?;
        Ok(())
    }

    pub fn read(path: &Path) -> anyhow::Result<NetRunFile> {
        let text =
            std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))
    }
}

#[cfg(test)]
mod tests {
    use stress::run_file::{DaemonInfo, ToolInfo, ToolVersions};

    use super::*;

    fn tool(n: &str) -> ToolInfo {
        ToolInfo {
            path: format!("/bin/{n}"),
            version: format!("{n} 1.2.3 [netsim]"),
        }
    }

    fn sample(daemon: Option<DaemonInfo>) -> NetRunFile {
        NetRunFile {
            base: RunFile {
                started: "2026-11-07T12:00:00Z".into(),
                seed: 42,
                direction: "push".into(),
                iterations: 3,
                steps: 50,
                max_total_size: Some("8M".into()),
                max_file_size: Some("1M".into()),
                filegen_set: vec![],
                log_level: "debug".into(),
                keep_going: false,
                client_args: vec!["--stats".into()],
                tools: ToolVersions {
                    irsync: tool("irsync"),
                    filegen: tool("filegen"),
                    fileset: tool("fileset"),
                },
                daemon,
            },
            network: Network {
                profile: "test-blackout".into(),
                client_timeout_secs: 2,
                reconnect: "5m".into(),
            },
        }
    }

    #[test]
    fn round_trips_with_and_without_daemon() {
        let t = tempfile::tempdir().unwrap();
        let p = t.path().join("run.toml");
        let daemon = DaemonInfo {
            endpoint_id: "abc".into(),
            address: "127.0.0.1:1234".into(),
            pid: 4321,
        };
        for r in [sample(Some(daemon)), sample(None)] {
            r.write(&p).unwrap();
            assert_eq!(NetRunFile::read(&p).unwrap(), r);
        }
        let text = std::fs::read_to_string(&p).unwrap();
        assert!(
            text.contains("[network]") && text.contains("seed = 42"),
            "{text}"
        );
    }
}
