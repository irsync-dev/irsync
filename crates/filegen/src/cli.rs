//! Command line surface and size parsing.

use std::ffi::OsString;
use std::fmt;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use clap::{ArgGroup, Args, Parser, Subcommand};
use serde::de::{self, Visitor};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

#[derive(Parser, Debug)]
#[command(name = "filegen", version, about = "Filesystem workload generator")]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Apply a deterministic random sequence of filesystem operations
    Run(RunArgs),
}

#[derive(Args, Debug)]
#[command(group(ArgGroup::new("start").required(true).multiple(false).args(["seed", "resume"])))]
pub struct RunArgs {
    /// Root directory to operate in
    pub path: PathBuf,
    /// Number of steps to run
    #[arg(long)]
    pub steps: u64,
    /// Seed for a fresh run
    #[arg(long)]
    pub seed: Option<u64>,
    /// Resume from a previous run's state file
    #[arg(long, value_name = "STATE")]
    pub resume: Option<PathBuf>,
    /// YAML configuration file
    #[arg(long)]
    pub config: Option<PathBuf>,
    /// Directory for state and logs
    #[arg(long)]
    pub out_dir: Option<PathBuf>,
    /// Prefix for output file names
    #[arg(long)]
    pub prefix: Option<String>,
    /// Override a config key: KEY=VALUE (repeatable)
    #[arg(long, value_name = "KEY=VALUE")]
    pub set: Vec<String>,
    /// Plan operations without touching the filesystem
    #[arg(long)]
    pub dry_run: bool,
    /// With --seed, truncate an existing log and state file for this prefix instead of refusing
    #[arg(long)]
    pub force: bool,
    /// Suppress progress output
    #[arg(short = 'q', long)]
    pub quiet: bool,
    /// Verbose output
    #[arg(short = 'v', long)]
    pub verbose: bool,
    /// Upper bound on total bytes in the tree
    #[arg(long, value_name = "SIZE")]
    pub max_total_size: Option<Size>,
    /// Upper bound on a single file's size
    #[arg(long, value_name = "SIZE")]
    pub max_file_size: Option<Size>,
    /// Maximum directory depth
    #[arg(long)]
    pub max_depth: Option<u32>,
    /// Maximum path length
    #[arg(long)]
    pub max_path_len: Option<u32>,
    /// Maximum entries a single tree removal may delete
    #[arg(long)]
    pub remove_tree_max_entries: Option<u32>,
    /// Whether to fsync after writes
    #[arg(long, value_name = "BOOL")]
    pub fsync: Option<bool>,
    /// Steps between checkpoints
    #[arg(long)]
    pub checkpoint_steps: Option<u64>,
}

/// A byte count that parses from and displays as `123`, `4K`, `256M`, `25G`, `1T`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Size(pub u64);

const UNITS: [(char, u32); 4] = [('T', 40), ('G', 30), ('M', 20), ('K', 10)];

impl FromStr for Size {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, String> {
        let t = s.trim();
        let (digits, shift) = match t.chars().last().map(|c| c.to_ascii_uppercase()) {
            Some(c) if c.is_ascii_alphabetic() => {
                let shift = UNITS
                    .iter()
                    .find(|(u, _)| *u == c)
                    .map(|(_, sh)| *sh)
                    .ok_or_else(|| format!("invalid size suffix in {s:?}"))?;
                (&t[..t.len() - 1], shift)
            }
            _ => (t, 0),
        };
        let n: u64 = digits.parse().map_err(|_| format!("invalid size {s:?}"))?;
        n.checked_mul(1u64 << shift)
            .map(Size)
            .ok_or_else(|| format!("size {s:?} overflows"))
    }
}

impl fmt::Display for Size {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let n = self.0;
        if n != 0 {
            for (u, shift) in UNITS {
                if n.is_multiple_of(1u64 << shift) {
                    return write!(f, "{}{}", n >> shift, u);
                }
            }
        }
        write!(f, "{n}")
    }
}

impl Serialize for Size {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for Size {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl Visitor<'_> for V {
            type Value = Size;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("an integer or a size string such as 25G")
            }

            fn visit_u64<E: de::Error>(self, v: u64) -> Result<Size, E> {
                Ok(Size(v))
            }

            fn visit_i64<E: de::Error>(self, v: i64) -> Result<Size, E> {
                u64::try_from(v)
                    .map(Size)
                    .map_err(|_| E::custom("size must not be negative"))
            }

            fn visit_str<E: de::Error>(self, v: &str) -> Result<Size, E> {
                v.parse().map_err(E::custom)
            }
        }
        d.deserialize_any(V)
    }
}

/// Output directory: flag, then config, then `$FILEGENTMP`, then `$TMPDIR`, then `/tmp`.
pub fn resolve_out_dir(
    flag: Option<&Path>,
    cfg: Option<&Path>,
    env: &dyn Fn(&str) -> Option<OsString>,
) -> PathBuf {
    if let Some(p) = flag.or(cfg) {
        return p.to_path_buf();
    }
    for key in ["FILEGENTMP", "TMPDIR"] {
        if let Some(v) = env(key) {
            return PathBuf::from(v);
        }
    }
    PathBuf::from("/tmp")
}

/// Split `KEY=VALUE` on the first `=`.
pub fn parse_set(s: &str) -> Result<(String, String), String> {
    s.split_once('=')
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .ok_or_else(|| format!("expected KEY=VALUE, got {s:?}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn size_parses_suffixes() {
        assert_eq!("25G".parse::<Size>().unwrap().0, 25 << 30);
        assert_eq!("256M".parse::<Size>().unwrap().0, 256 << 20);
        assert_eq!("4k".parse::<Size>().unwrap().0, 4096);
        assert_eq!("1T".parse::<Size>().unwrap().0, 1 << 40);
        assert_eq!("123".parse::<Size>().unwrap().0, 123);
        assert!("1X".parse::<Size>().is_err());
    }

    #[test]
    fn size_displays_round_trip() {
        assert_eq!(Size(25 << 30).to_string(), "25G");
        assert_eq!(Size(1500).to_string(), "1500");
    }

    #[test]
    fn size_serde_string_or_integer() {
        assert_eq!(serde_yaml_ng::from_str::<Size>("4096").unwrap(), Size(4096));
        assert_eq!(serde_yaml_ng::from_str::<Size>("4K").unwrap(), Size(4096));
        assert_eq!(serde_yaml_ng::to_string(&Size(4096)).unwrap().trim(), "4K");
    }

    #[test]
    fn out_dir_lookup_order() {
        let env = |k: &str| match k {
            "FILEGENTMP" => Some("/ft".into()),
            "TMPDIR" => Some("/td".into()),
            _ => None,
        };
        assert_eq!(
            resolve_out_dir(Some(Path::new("/flag")), Some(Path::new("/cfg")), &env),
            PathBuf::from("/flag")
        );
        assert_eq!(
            resolve_out_dir(None, Some(Path::new("/cfg")), &env),
            PathBuf::from("/cfg")
        );
        assert_eq!(resolve_out_dir(None, None, &env), PathBuf::from("/ft"));
        let env2 = |k: &str| {
            if k == "TMPDIR" {
                Some("/td".into())
            } else {
                None
            }
        };
        assert_eq!(resolve_out_dir(None, None, &env2), PathBuf::from("/td"));
        assert_eq!(
            resolve_out_dir(None, None, &|_| None),
            PathBuf::from("/tmp")
        );
    }

    #[test]
    fn parse_set_splits_on_first_equals() {
        assert_eq!(
            parse_set("content.pattern_fraction=0.2").unwrap(),
            ("content.pattern_fraction".into(), "0.2".into())
        );
        assert_eq!(parse_set("names.extensions=[a=b]").unwrap().1, "[a=b]");
        assert!(parse_set("nokey").is_err());
    }
}
