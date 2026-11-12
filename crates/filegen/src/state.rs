//! The generator state file: human-readable YAML, replaced atomically.

use crate::config::Config;
use crate::kind::OpKind;
use crate::log::KindSummary;
use rand_chacha::ChaCha8Rng;
use serde::{Deserialize, Serialize};
use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

pub const FORMAT_VERSION: u32 = 3;

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct State {
    pub format_version: u32,
    pub seed: u64,
    pub steps_done: u64,
    pub rng: ChaCha8Rng,
    pub chain_state: ChainState,
    pub dry_run: bool,
    pub config: Config,
    pub tree: TreeInfo,
    pub last_run: LastRun,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct ChainState {
    pub last_kind: OpKind,
    /// Escaped relative path of the locality directory; `""` for the root.
    pub anchor: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct TreeInfo {
    pub entries: u64,
    pub total_bytes: u64,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct LastRun {
    pub started: jiff::Timestamp,
    pub finished: jiff::Timestamp,
    pub steps: u64,
    pub outcome: String,
    pub summary: Vec<KindSummary>,
}

#[derive(Debug, thiserror::Error)]
pub enum StateError {
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error(transparent)]
    Yaml(#[from] serde_yaml_ng::Error),
    #[error("state file format_version {found} is not supported (expected 3)")]
    Version { found: u32 },
}

#[derive(Deserialize)]
struct Probe {
    #[serde(default)]
    format_version: u32,
}

pub fn read(path: &Path) -> Result<State, StateError> {
    let text = fs::read_to_string(path)?;
    let probe: Probe = serde_yaml_ng::from_str(&text)?;
    if probe.format_version != FORMAT_VERSION {
        return Err(StateError::Version {
            found: probe.format_version,
        });
    }
    Ok(serde_yaml_ng::from_str(&text)?)
}

/// Write `s` to a temp file next to `path`, fsync it, and rename over `path`.
pub fn write_atomic(path: &Path, s: &State) -> io::Result<()> {
    let yaml = serde_yaml_ng::to_string(s).map_err(io::Error::other)?;
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(format!(".tmp-{}", std::process::id()));
    let tmp: PathBuf = path.with_file_name(name);
    let res = (|| {
        let mut f = File::create(&tmp)?;
        f.write_all(yaml.as_bytes())?;
        f.sync_all()?;
        drop(f);
        fs::rename(&tmp, path)
    })();
    if res.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    res
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::{Rng, SeedableRng};
    use std::fs;
    use tempfile::TempDir;

    fn sample() -> (State, ChaCha8Rng) {
        let mut rng = ChaCha8Rng::seed_from_u64(42);
        rng.next_u64();
        let now = jiff::Timestamp::now();
        let s = State {
            format_version: FORMAT_VERSION,
            seed: 42,
            steps_done: 1,
            rng: rng.clone(),
            chain_state: ChainState {
                last_kind: OpKind::Mkdir,
                anchor: "a/b".into(),
            },
            dry_run: false,
            config: Config::default(),
            tree: TreeInfo {
                entries: 1,
                total_bytes: 0,
            },
            last_run: LastRun {
                started: now,
                finished: now,
                steps: 1,
                outcome: "ok".into(),
                summary: vec![],
            },
        };
        (s, rng)
    }

    #[test]
    fn state_round_trips_and_rng_continues() {
        let (s, mut rng) = sample();
        let dir = TempDir::new().unwrap();
        let p = dir.path().join("s.yaml");
        write_atomic(&p, &s).unwrap();
        let mut back = read(&p).unwrap();
        assert_eq!(back, s);
        assert_eq!(back.rng.next_u64(), rng.next_u64());
        assert!(!fs::read_dir(dir.path()).unwrap().any(|e| e
            .unwrap()
            .file_name()
            .to_string_lossy()
            .contains(".tmp-")));
    }

    #[test]
    fn state_rejects_other_version() {
        let (mut s, _) = sample();
        // A file from before rand 0.10, whose seeds no longer reproduce.
        s.format_version = 2;
        let dir = TempDir::new().unwrap();
        let p = dir.path().join("s.yaml");
        write_atomic(&p, &s).unwrap();
        assert!(matches!(read(&p), Err(StateError::Version { found: 2 })));
        fs::write(&p, "seed: 1\n").unwrap();
        assert!(matches!(read(&p), Err(StateError::Version { found: 0 })));
    }

    #[test]
    fn state_yaml_is_human_readable() {
        let (s, _) = sample();
        let y = serde_yaml_ng::to_string(&s).unwrap();
        for want in [
            "seed: 42",
            "steps_done:",
            "max_total_size: 25G",
            "last_kind: mkdir",
        ] {
            assert!(y.contains(want), "missing {want} in:\n{y}");
        }
        assert!(y.starts_with("format_version: 3\nseed: 42\n"));
    }

    #[test]
    fn write_replaces_existing_file() {
        let (mut s, _) = sample();
        let dir = TempDir::new().unwrap();
        let p = dir.path().join("s.yaml");
        write_atomic(&p, &s).unwrap();
        s.steps_done = 99;
        write_atomic(&p, &s).unwrap();
        assert_eq!(read(&p).unwrap().steps_done, 99);
    }
}
