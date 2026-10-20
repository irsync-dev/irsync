//! Configuration schema, defaults, merge order and validation.
//!
//! Precedence, lowest first: built-in defaults, the resumed state's config
//! (`base`), the YAML file, `--set KEY=VALUE`, then dedicated flags.

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_yaml_ng::{Mapping, Value};

use crate::cli::{RunArgs, Size};
use crate::kind::OpKind;

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("reading config: {0}")]
    Io(#[from] std::io::Error),
    #[error("parsing config: {0}")]
    Yaml(#[from] serde_yaml_ng::Error),
    #[error("unknown config key `{0}`")]
    UnknownKey(String),
    #[error("invalid config: {0}")]
    Invalid(String),
}

/// Transition weights: previous kind → next kind → weight.
pub type Weights = BTreeMap<OpKind, BTreeMap<OpKind, f64>>;

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub out_dir: Option<PathBuf>,
    pub prefix: Option<String>,
    pub max_total_size: Size,
    pub max_file_size: Size,
    pub max_depth: u32,
    pub max_path_len: u32,
    pub remove_tree_max_entries: u32,
    pub fsync: bool,
    pub checkpoint_steps: u64,
    pub size_distribution: SizeDistribution,
    pub content: ContentConfig,
    pub names: NamesConfig,
    pub symlink: SymlinkConfig,
    pub chmod: ChmodConfig,
    pub mtime: MtimeConfig,
    pub locality: LocalityConfig,
    pub weights: Weights,
    pub xattr: XattrConfig,
    pub chown: ChownConfig,
    pub mknod: MknodConfig,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            out_dir: None,
            prefix: None,
            max_total_size: Size(25 << 30),
            max_file_size: Size(256 << 20),
            max_depth: 32,
            max_path_len: 900,
            remove_tree_max_entries: 1000,
            fsync: false,
            checkpoint_steps: 1000,
            size_distribution: SizeDistribution::default(),
            content: ContentConfig::default(),
            names: NamesConfig::default(),
            symlink: SymlinkConfig::default(),
            chmod: ChmodConfig::default(),
            mtime: MtimeConfig::default(),
            locality: LocalityConfig::default(),
            weights: default_weights(),
            xattr: XattrConfig::default(),
            chown: ChownConfig::default(),
            mknod: MknodConfig::default(),
        }
    }
}

/// File size classes. Within a band, sizes are log-uniform between the
/// previous band's `max` (1 for `small`) and this band's `max`; `large`
/// runs from `medium.max` up to `max_file_size`.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct SizeDistribution {
    pub empty: f64,
    pub small: Band,
    pub medium: Band,
    pub large_weight: f64,
}

impl Default for SizeDistribution {
    fn default() -> Self {
        SizeDistribution {
            empty: 0.05,
            small: Band {
                weight: 0.65,
                max: Size(64 << 10),
            },
            medium: Band {
                weight: 0.25,
                max: Size(8 << 20),
            },
            large_weight: 0.05,
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Band {
    pub weight: f64,
    pub max: Size,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ContentConfig {
    /// Fraction of written content that is patterned rather than random.
    pub pattern_fraction: f64,
    pub patterns: Vec<Pattern>,
    pub block_size: Size,
}

impl Default for ContentConfig {
    fn default() -> Self {
        ContentConfig {
            pattern_fraction: 0.05,
            patterns: vec![Pattern::Zeros, Pattern::RepeatByte, Pattern::RepeatBlock],
            block_size: Size(4 << 10),
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Pattern {
    Zeros,
    RepeatByte,
    RepeatBlock,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct NamesConfig {
    pub classes: NameClassWeights,
    pub extensions: Vec<String>,
    pub max_len: u32,
}

impl Default for NamesConfig {
    fn default() -> Self {
        NamesConfig {
            classes: NameClassWeights::default(),
            extensions: ["txt", "bin", "jpg", "log", "dat", "md", "json", ""]
                .iter()
                .map(|s| s.to_string())
                .collect(),
            max_len: 255,
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct NameClassWeights {
    pub plain: f64,
    pub spaces: f64,
    pub dotfile: f64,
    pub unicode: f64,
    pub long: f64,
    pub awkward: f64,
}

impl Default for NameClassWeights {
    fn default() -> Self {
        NameClassWeights {
            plain: 70.0,
            spaces: 8.0,
            dotfile: 5.0,
            unicode: 8.0,
            long: 3.0,
            awkward: 6.0,
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct SymlinkConfig {
    pub file: f64,
    pub dir: f64,
    pub dangling: f64,
    pub absolute: f64,
}

impl Default for SymlinkConfig {
    fn default() -> Self {
        SymlinkConfig {
            file: 0.5,
            dir: 0.2,
            dangling: 0.3,
            absolute: 0.2,
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ChmodConfig {
    pub modes: Vec<Mode>,
    pub special_weight: f64,
}

impl Default for ChmodConfig {
    fn default() -> Self {
        ChmodConfig {
            modes: [0o644, 0o600, 0o755, 0o700, 0o664, 0o775, 0o444, 0o555]
                .into_iter()
                .map(Mode)
                .collect(),
            special_weight: 0.05,
        }
    }
}

/// A permission mode, written in YAML as an octal string such as `"0644"`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Mode(pub u32);

impl fmt::Display for Mode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:04o}", self.0)
    }
}

impl Serialize for Mode {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for Mode {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        let ok = (3..=4).contains(&s.len()) && s.bytes().all(|b| (b'0'..=b'7').contains(&b));
        if !ok {
            return Err(serde::de::Error::custom(format!(
                "invalid mode {s:?}, expected 3 or 4 octal digits such as \"0644\""
            )));
        }
        Ok(Mode(
            u32::from_str_radix(&s, 8).expect("checked octal digits"),
        ))
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct MtimeConfig {
    pub recent: f64,
    pub distant: f64,
    pub future: f64,
    pub distant_years: u32,
    pub future_years: u32,
}

impl Default for MtimeConfig {
    fn default() -> Self {
        MtimeConfig {
            recent: 0.6,
            distant: 0.3,
            future: 0.1,
            distant_years: 40,
            future_years: 5,
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct LocalityConfig {
    pub same_dir: f64,
    pub parent: f64,
    pub anywhere: f64,
}

impl Default for LocalityConfig {
    fn default() -> Self {
        LocalityConfig {
            same_dir: 0.6,
            parent: 0.2,
            anywhere: 0.2,
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct XattrConfig {
    pub names: Vec<String>,
    pub max_value: Size,
}

impl Default for XattrConfig {
    fn default() -> Self {
        XattrConfig {
            names: ["a", "b", "c", "d"]
                .iter()
                .map(|s| format!("user.filegen.{s}"))
                .collect(),
            max_value: Size(256),
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(deny_unknown_fields)]
pub struct ChownConfig {
    pub uids: Vec<u32>,
    pub gids: Vec<u32>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct MknodConfig {
    pub char_fraction: f64,
}

impl Default for MknodConfig {
    fn default() -> Self {
        MknodConfig { char_fraction: 0.5 }
    }
}

/// The built-in generic workload. Row = previous kind, column = next kind;
/// both in `OpKind::ALL` order:
/// Cr Ap Tr Ov Rw Pu Df Mk Rd Rt Rn Ro Sy Rp Ds Hl Mf Ch Mt Sx Co Mn
/// (create_file append truncate overwrite_range rewrite punch_hole
/// delete_file mkdir rmdir remove_tree rename rename_over symlink
/// repoint_symlink delete_symlink hardlink mkfifo chmod set_mtime set_xattr
/// chown mknod). The last three columns are 0: those kinds are off by default.
#[rustfmt::skip]
const DEFAULT_TABLE: [[f64; 22]; 22] = [
    // Cr  Ap  Tr  Ov  Rw  Pu  Df  Mk  Rd  Rt  Rn  Ro  Sy  Rp  Ds  Hl  Mf  Ch  Mt  Sx  Co  Mn
    [15., 30., 3., 20., 3., 0.7, 2., 3., 1., 0.5, 3., 1., 2., 1., 1., 1., 0.5, 3., 3., 0., 0., 0.], // create_file
    [25., 20., 4., 15., 3., 1., 4., 5., 3., 1., 3., 2., 2., 1., 1., 1., 0.5, 3., 3., 0., 0., 0.],    // append
    [20., 20., 2., 15., 5., 1., 4., 4., 2., 1., 3., 2., 2., 1., 1., 1., 0.5, 3., 3., 0., 0., 0.],    // truncate
    [20., 20., 4., 15., 3., 1., 4., 4., 2., 1., 3., 2., 2., 1., 1., 1., 0.5, 3., 3., 0., 0., 0.],    // overwrite_range
    [20., 18., 4., 15., 3., 1., 4., 4., 2., 1., 3., 2., 2., 1., 1., 1., 0.5, 3., 3., 0., 0., 0.],    // rewrite
    [20., 18., 4., 15., 3., 1., 4., 4., 2., 1., 3., 2., 2., 1., 1., 1., 0.5, 3., 3., 0., 0., 0.],    // punch_hole
    [25., 12., 2., 10., 3., 0.7, 6., 6., 3., 2., 4., 2., 2., 1., 1., 0.7, 0.5, 3., 3., 0., 0., 0.],  // delete_file
    [45., 2., 1., 2., 1., 0.5, 1., 20., 2., 1., 3., 1., 3., 1., 1., 0.5, 0.5, 3., 3., 0., 0., 0.],   // mkdir
    [20., 8., 2., 6., 2., 0.5, 4., 12., 4., 3., 4., 2., 3., 1., 1., 0.5, 0.5, 3., 3., 0., 0., 0.],   // rmdir
    [20., 8., 2., 6., 2., 0.5, 4., 12., 3., 3., 4., 2., 3., 1., 1., 0.5, 0.5, 3., 3., 0., 0., 0.],   // remove_tree
    [20., 15., 4., 12., 3., 0.7, 6., 5., 2., 1., 3., 2., 2., 1., 1., 0.7, 0.5, 3., 3., 0., 0., 0.],  // rename
    [20., 15., 4., 12., 3., 0.7, 6., 5., 2., 1., 3., 2., 2., 1., 1., 0.7, 0.5, 3., 3., 0., 0., 0.],  // rename_over
    [15., 10., 2., 8., 2., 0.5, 3., 5., 2., 1., 3., 2., 6., 5., 4., 0.5, 0.5, 3., 3., 0., 0., 0.],   // symlink
    [15., 10., 2., 8., 2., 0.5, 3., 5., 2., 1., 3., 2., 6., 5., 4., 0.5, 0.5, 3., 3., 0., 0., 0.],   // repoint_symlink
    [20., 15., 4., 12., 3., 0.7, 6., 5., 2., 1., 3., 2., 2., 1., 1., 0.7, 0.5, 3., 3., 0., 0., 0.],  // delete_symlink
    [20., 15., 4., 12., 3., 0.7, 6., 5., 2., 1., 3., 2., 2., 1., 1., 0.7, 0.5, 3., 3., 0., 0., 0.],  // hardlink
    [20., 15., 4., 12., 3., 0.7, 6., 5., 2., 1., 3., 2., 2., 1., 1., 0.7, 0.5, 3., 3., 0., 0., 0.],  // mkfifo
    [20., 15., 4., 12., 3., 0.7, 6., 5., 2., 1., 3., 2., 2., 1., 1., 0.7, 0.5, 3., 3., 0., 0., 0.],  // chmod
    [20., 15., 4., 12., 3., 0.7, 6., 5., 2., 1., 3., 2., 2., 1., 1., 0.7, 0.5, 3., 3., 0., 0., 0.],  // set_mtime
    [20., 15., 4., 12., 3., 0.7, 6., 5., 2., 1., 3., 2., 2., 1., 1., 0.7, 0.5, 3., 3., 0., 0., 0.],  // set_xattr
    [20., 15., 4., 12., 3., 0.7, 6., 5., 2., 1., 3., 2., 2., 1., 1., 0.7, 0.5, 3., 3., 0., 0., 0.],  // chown
    [20., 15., 4., 12., 3., 0.7, 6., 5., 2., 1., 3., 2., 2., 1., 1., 0.7, 0.5, 3., 3., 0., 0., 0.],  // mknod
];

/// The default transition table, built from [`DEFAULT_TABLE`].
pub fn default_weights() -> Weights {
    OpKind::ALL
        .iter()
        .map(|&from| {
            let row = OpKind::ALL
                .iter()
                .map(|&to| (to, DEFAULT_TABLE[from.index()][to.index()]))
                .collect();
            (from, row)
        })
        .collect()
}

/// Build the effective config from defaults, `base`, `file` and `sets`.
pub fn load(
    file: Option<&Path>,
    base: Option<&Config>,
    sets: &[(String, String)],
) -> Result<Config, ConfigError> {
    let defaults = serde_yaml_ng::to_value(Config::default())?;
    let mut merged = defaults.clone();
    if let Some(b) = base {
        merge(&mut merged, serde_yaml_ng::to_value(b)?);
    }
    if let Some(path) = file {
        let text = std::fs::read_to_string(path)?;
        let v: Value = serde_yaml_ng::from_str(&text)?;
        // An empty file parses as null: nothing to merge.
        if !v.is_null() {
            check_keys(&defaults, &v, "")?;
            merge(&mut merged, v);
        }
    }
    for (key, raw) in sets {
        let v: Value = serde_yaml_ng::from_str(raw)?;
        let nested = set_value(&defaults, key, v)?;
        check_keys(&defaults, &nested, "")?;
        merge(&mut merged, nested);
    }
    serde_yaml_ng::from_value(merged).map_err(map_unknown_field)
}

/// Copy every flag the user gave into `cfg`.
pub fn apply_flags(cfg: &mut Config, args: &RunArgs) {
    if let Some(v) = &args.out_dir {
        cfg.out_dir = Some(v.clone());
    }
    if let Some(v) = &args.prefix {
        cfg.prefix = Some(v.clone());
    }
    if let Some(v) = args.max_total_size {
        cfg.max_total_size = v;
    }
    if let Some(v) = args.max_file_size {
        cfg.max_file_size = v;
    }
    if let Some(v) = args.max_depth {
        cfg.max_depth = v;
    }
    if let Some(v) = args.max_path_len {
        cfg.max_path_len = v;
    }
    if let Some(v) = args.remove_tree_max_entries {
        cfg.remove_tree_max_entries = v;
    }
    if let Some(v) = args.fsync {
        cfg.fsync = v;
    }
    if let Some(v) = args.checkpoint_steps {
        cfg.checkpoint_steps = v;
    }
}

/// Recursively merge `over` into `into`: mappings merge key by key, any
/// other pair is replaced.
fn merge(into: &mut Value, over: Value) {
    match (into, over) {
        (Value::Mapping(a), Value::Mapping(b)) => {
            for (k, v) in b {
                match a.get_mut(&k) {
                    Some(slot) => merge(slot, v),
                    None => {
                        a.insert(k, v);
                    }
                }
            }
        }
        (slot, v) => *slot = v,
    }
}

fn key_name(k: &Value) -> String {
    match k {
        Value::String(s) => s.clone(),
        other => serde_yaml_ng::to_string(other)
            .map(|s| s.trim().to_string())
            .unwrap_or_default(),
    }
}

fn join(prefix: &str, key: &str) -> String {
    if prefix.is_empty() {
        key.to_string()
    } else {
        format!("{prefix}.{key}")
    }
}

/// Reject any key in `v` that is not in the default config at the same
/// position, naming its full dotted path.
fn check_keys(defaults: &Value, v: &Value, prefix: &str) -> Result<(), ConfigError> {
    if let (Value::Mapping(d), Value::Mapping(m)) = (defaults, v) {
        for (k, sub) in m {
            let path = join(prefix, &key_name(k));
            match d.get(k) {
                Some(dsub) => check_keys(dsub, sub, &path)?,
                None => return Err(ConfigError::UnknownKey(path)),
            }
        }
    }
    Ok(())
}

/// Turn `a.b.c` = `v` into `{a: {b: {c: v}}}`, requiring the path to exist
/// in the default config.
fn set_value(defaults: &Value, key: &str, v: Value) -> Result<Value, ConfigError> {
    let parts: Vec<&str> = key.split('.').collect();
    let mut node = defaults;
    for part in &parts {
        node = node
            .as_mapping()
            .and_then(|m| m.get(*part))
            .ok_or_else(|| ConfigError::UnknownKey(key.to_string()))?;
    }
    Ok(parts.iter().rev().fold(v, |acc, part| {
        let mut m = Mapping::new();
        m.insert(Value::String(part.to_string()), acc);
        Value::Mapping(m)
    }))
}

/// Surface serde's `unknown field` error as [`ConfigError::UnknownKey`].
fn map_unknown_field(e: serde_yaml_ng::Error) -> ConfigError {
    let msg = e.to_string();
    msg.split_once("unknown field `")
        .and_then(|(_, rest)| rest.split_once('`'))
        .map(|(name, _)| ConfigError::UnknownKey(name.to_string()))
        .unwrap_or(ConfigError::Yaml(e))
}

fn check_probability(name: &str, p: f64) -> Result<(), ConfigError> {
    if (0.0..=1.0).contains(&p) {
        Ok(())
    } else {
        Err(ConfigError::Invalid(format!(
            "{name} must be a probability in [0, 1], got {p}"
        )))
    }
}

fn check_weight(name: &str, w: f64) -> Result<(), ConfigError> {
    if w.is_finite() && w >= 0.0 {
        Ok(())
    } else {
        Err(ConfigError::Invalid(format!(
            "{name} must be a finite weight >= 0, got {w}"
        )))
    }
}

impl Config {
    /// Check ranges and the shape of the transition table.
    pub fn validate(&self) -> Result<(), ConfigError> {
        let invalid = |m: String| Err(ConfigError::Invalid(m));
        if self.max_file_size.0 > self.max_total_size.0 {
            return invalid(format!(
                "max_file_size ({}) exceeds max_total_size ({})",
                self.max_file_size, self.max_total_size
            ));
        }
        if self.max_path_len > 4096 {
            return invalid(format!(
                "max_path_len must be <= 4096, got {}",
                self.max_path_len
            ));
        }
        if self.max_depth < 1 {
            return invalid("max_depth must be >= 1".into());
        }
        if self.checkpoint_steps < 1 {
            return invalid("checkpoint_steps must be >= 1".into());
        }

        let sd = &self.size_distribution;
        for (name, p) in [
            ("size_distribution.empty", sd.empty),
            ("size_distribution.small.weight", sd.small.weight),
            ("size_distribution.medium.weight", sd.medium.weight),
            ("size_distribution.large_weight", sd.large_weight),
            ("content.pattern_fraction", self.content.pattern_fraction),
            ("symlink.file", self.symlink.file),
            ("symlink.dir", self.symlink.dir),
            ("symlink.dangling", self.symlink.dangling),
            ("symlink.absolute", self.symlink.absolute),
            ("chmod.special_weight", self.chmod.special_weight),
            ("mtime.recent", self.mtime.recent),
            ("mtime.distant", self.mtime.distant),
            ("mtime.future", self.mtime.future),
            ("locality.same_dir", self.locality.same_dir),
            ("locality.parent", self.locality.parent),
            ("locality.anywhere", self.locality.anywhere),
            ("mknod.char_fraction", self.mknod.char_fraction),
        ] {
            check_probability(name, p)?;
        }

        let c = &self.names.classes;
        for (name, w) in [
            ("names.classes.plain", c.plain),
            ("names.classes.spaces", c.spaces),
            ("names.classes.dotfile", c.dotfile),
            ("names.classes.unicode", c.unicode),
            ("names.classes.long", c.long),
            ("names.classes.awkward", c.awkward),
        ] {
            check_weight(name, w)?;
        }

        for prev in OpKind::ALL {
            let Some(row) = self.weights.get(&prev) else {
                return invalid(format!("weights is missing row {}", prev.name()));
            };
            let mut any = false;
            for next in OpKind::ALL {
                let Some(&w) = row.get(&next) else {
                    return invalid(format!(
                        "weights.{} is missing column {}",
                        prev.name(),
                        next.name()
                    ));
                };
                check_weight(&format!("weights.{}.{}", prev.name(), next.name()), w)?;
                any |= w > 0.0;
            }
            if !any {
                return invalid(format!("weights.{} has no non-zero weight", prev.name()));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::{Cli, Command, RunArgs, Size};
    use crate::kind::OpKind;
    use clap::Parser;
    use std::io::Write;

    fn write_tmp(s: &str) -> tempfile::TempPath {
        let mut f = tempfile::NamedTempFile::new().unwrap();
        f.write_all(s.as_bytes()).unwrap();
        f.into_temp_path()
    }

    fn parse_run_args(args: &[&str]) -> RunArgs {
        let argv = ["filegen", "run"].iter().chain(args.iter());
        match Cli::try_parse_from(argv).unwrap().command {
            Command::Run(a) => a,
        }
    }

    #[test]
    fn default_config_round_trips_through_yaml() {
        let d = Config::default();
        let s = serde_yaml_ng::to_string(&d).unwrap();
        assert_eq!(serde_yaml_ng::from_str::<Config>(&s).unwrap(), d);
        assert!(s.contains("max_total_size: 25G"));
        // serde_yaml_ng quotes digit-only strings with single quotes.
        assert!(s.contains("- '0644'"), "{s}");
    }

    #[test]
    fn mode_parses_octal_strings() {
        assert_eq!(
            serde_yaml_ng::from_str::<Mode>("\"0644\"").unwrap(),
            Mode(0o644)
        );
        assert_eq!(
            serde_yaml_ng::from_str::<Mode>("'644'").unwrap(),
            Mode(0o644)
        );
        assert_eq!(
            serde_yaml_ng::from_str::<Mode>("\"4755\"").unwrap(),
            Mode(0o4755)
        );
        assert_eq!(Mode(0o4755).to_string(), "4755");
        assert!(serde_yaml_ng::from_str::<Mode>("\"0648\"").is_err());
        assert!(serde_yaml_ng::from_str::<Mode>("\"64\"").is_err());
    }

    #[test]
    fn default_config_validates() {
        Config::default().validate().unwrap();
    }

    #[test]
    fn partial_file_merges_over_defaults() {
        let f = write_tmp("max_file_size: 1M\nnames:\n  classes:\n    unicode: 0\n");
        let c = load(Some(&f), None, &[]).unwrap();
        assert_eq!(c.max_file_size.0, 1 << 20);
        assert_eq!(c.names.classes.unicode, 0.0);
        assert_eq!(c.names.classes.plain, 70.0);
        assert_eq!(c.max_total_size.0, 25 << 30);
    }

    #[test]
    fn set_overrides_file_and_parses_yaml_values() {
        let f = write_tmp("content:\n  pattern_fraction: 0.5\n");
        let c = load(
            Some(&f),
            None,
            &[
                ("content.pattern_fraction".into(), "0.2".into()),
                ("names.extensions".into(), "[a, b]".into()),
            ],
        )
        .unwrap();
        assert_eq!(c.content.pattern_fraction, 0.2);
        assert_eq!(c.names.extensions, vec!["a", "b"]);
    }

    #[test]
    fn base_applies_before_file() {
        let base = Config {
            max_depth: 5,
            fsync: true,
            ..Config::default()
        };
        let f = write_tmp("max_depth: 7\n");
        let c = load(Some(&f), Some(&base), &[]).unwrap();
        assert_eq!(c.max_depth, 7);
        assert!(c.fsync);
    }

    #[test]
    fn config_unknown_key_is_error() {
        let f = write_tmp("max_total_sise: 1G\n");
        let e = load(Some(&f), None, &[]).unwrap_err().to_string();
        assert!(e.contains("max_total_sise"), "{e}");
        let f = write_tmp("names:\n  clases: {}\n");
        assert!(load(Some(&f), None, &[])
            .unwrap_err()
            .to_string()
            .contains("clases"));
    }

    #[test]
    fn config_set_unknown_key_is_error() {
        let e = load(None, None, &[("locality.nearby".into(), "1".into())])
            .unwrap_err()
            .to_string();
        assert!(e.contains("locality.nearby"));
    }

    #[test]
    fn validate_rejects_bad_values() {
        let mut c = Config::default();
        c.locality.same_dir = 1.5;
        assert!(c.validate().is_err());
        let mut c = Config::default();
        c.max_file_size = Size(c.max_total_size.0 + 1);
        assert!(c.validate().is_err());
        let mut c = Config::default();
        c.weights.get_mut(&OpKind::Mkdir).unwrap().clear();
        assert!(c.validate().is_err());
    }

    #[test]
    fn flags_override_everything() {
        let args = RunArgs {
            max_depth: Some(3),
            fsync: Some(true),
            ..parse_run_args(&["--steps", "1", "--seed", "1", "/x"])
        };
        let mut c = Config::default();
        apply_flags(&mut c, &args);
        assert_eq!(c.max_depth, 3);
        assert!(c.fsync);
    }
}
