//! The generator state file: human-readable YAML, replaced atomically.

use crate::caps::Caps;
use crate::config::Config;
use crate::kind::OpKind;
use crate::log::KindSummary;
use crate::platform::Platform;
use rand_chacha::ChaCha8Rng;
use serde::{Deserialize, Serialize};
use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

pub const FORMAT_VERSION: u32 = 4;

/// The oldest version [`read`] still takes: a Unix state without the
/// `platform` and `caps` keys.
const OLDEST_VERSION: u32 = 3;

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct State {
    pub format_version: u32,
    pub seed: u64,
    pub steps_done: u64,
    pub rng: ChaCha8Rng,
    pub chain_state: ChainState,
    pub dry_run: bool,
    /// The platform the run was on; a resume must be on the same one.
    pub platform: Platform,
    /// The capabilities the run found; a resume must find the same ones.
    pub caps: Caps,
    pub config: Config,
    pub tree: TreeInfo,
    pub last_run: LastRun,
}

/// A version-3 state: every field of [`State`] but `platform` and `caps`.
#[derive(Deserialize)]
struct StateV3 {
    seed: u64,
    steps_done: u64,
    rng: ChaCha8Rng,
    chain_state: ChainState,
    dry_run: bool,
    config: Config,
    tree: TreeInfo,
    last_run: LastRun,
}

impl StateV3 {
    /// Version 3 was written only on Unix, which has every capability.
    fn into_state(self) -> State {
        State {
            format_version: 3,
            seed: self.seed,
            steps_done: self.steps_done,
            rng: self.rng,
            chain_state: self.chain_state,
            dry_run: self.dry_run,
            platform: Platform::Unix,
            caps: Caps::ALL,
            config: self.config,
            tree: self.tree,
            last_run: self.last_run,
        }
    }
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
    Yaml(Box<serde_saphyr::Error>),
    #[error("state file format_version {found} is not supported (expected 3 or 4)")]
    Version { found: u32 },
}

// The YAML error is large, so it is boxed to keep `Result`s small.
impl From<serde_saphyr::Error> for StateError {
    fn from(e: serde_saphyr::Error) -> Self {
        Self::Yaml(Box::new(e))
    }
}

#[derive(Deserialize)]
struct Probe {
    #[serde(default)]
    format_version: u32,
}

pub fn read(path: &Path) -> Result<State, StateError> {
    let text = fs::read_to_string(path)?;
    let probe: Probe = serde_saphyr::from_str(&text)?;
    match probe.format_version {
        FORMAT_VERSION => Ok(serde_saphyr::from_str(&text)?),
        OLDEST_VERSION => Ok(serde_saphyr::from_str::<StateV3>(&text)?.into_state()),
        found => Err(StateError::Version { found }),
    }
}

/// Write `s` to a temp file next to `path`, fsync it, and rename over `path`.
pub fn write_atomic(path: &Path, s: &State) -> io::Result<()> {
    let yaml = serde_saphyr::to_string(s).map_err(io::Error::other)?;
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
            platform: Platform::CURRENT,
            caps: Caps::ALL,
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
        let y = serde_saphyr::to_string(&s).unwrap();
        for want in [
            "seed: 42",
            "steps_done:",
            "max_total_size: 25G",
            "last_kind: mkdir",
        ] {
            assert!(y.contains(want), "missing {want} in:\n{y}");
        }
        assert!(y.starts_with("format_version: 4\nseed: 42\n"));
    }

    #[test]
    fn a_version_3_state_reads_as_unix_with_every_capability() {
        let (mut s, _) = sample();
        s.platform = Platform::Unix;
        let yaml = serde_saphyr::to_string(&s).unwrap();
        let mut skipping = false;
        let mut v3 = String::new();
        for line in yaml.lines() {
            if line.starts_with("platform:") {
                continue;
            }
            if line.starts_with("caps:") {
                skipping = true;
                continue;
            }
            if skipping && line.starts_with(' ') {
                continue;
            }
            skipping = false;
            if line == format!("format_version: {FORMAT_VERSION}") {
                v3.push_str("format_version: 3\n");
            } else {
                v3.push_str(line);
                v3.push('\n');
            }
        }
        assert!(!v3.contains("platform:") && !v3.contains("caps:"), "{v3}");
        let dir = TempDir::new().unwrap();
        let p = dir.path().join("s.yaml");
        fs::write(&p, v3).unwrap();
        let mut back = read(&p).unwrap();
        assert_eq!(back.format_version, 3);
        assert_eq!(back.platform, Platform::Unix);
        assert_eq!(back.caps, Caps::ALL);
        back.format_version = s.format_version;
        assert_eq!(back, s);
    }

    #[test]
    fn a_version_5_state_is_refused() {
        let (mut s, _) = sample();
        s.format_version = 5;
        let dir = TempDir::new().unwrap();
        let p = dir.path().join("s.yaml");
        write_atomic(&p, &s).unwrap();
        assert!(matches!(read(&p), Err(StateError::Version { found: 5 })));
    }

    #[test]
    fn platform_and_caps_are_written_by_name() {
        let (s, _) = sample();
        let y = serde_saphyr::to_string(&s).unwrap();
        assert!(
            y.contains(&format!("platform: {}\n", Platform::CURRENT.as_str())),
            "{y}"
        );
        assert!(y.contains("symlinks: true"), "{y}");
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
