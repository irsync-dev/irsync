//! What this binary was built from: the package version, the git commit,
//! and whether the tree had uncommitted changes. The commit and dirty flag
//! come from `build.rs`, which asks git at compile time.

use std::fmt;
use std::sync::LazyLock;

use irsync_proto::v1;

/// vergen writes this placeholder instead of a value in idempotent mode.
const PLACEHOLDER: &str = "VERGEN_IDEMPOTENT_OUTPUT";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuildInfo {
    /// Package version, or "unknown" for a peer that sent none.
    pub version: String,
    /// Short commit hash, or "unknown" when not built from a checkout.
    pub commit: String,
    /// Uncommitted changes at build time; `None` when unknown.
    pub dirty: Option<bool>,
}

impl BuildInfo {
    /// The info compiled into this binary.
    pub fn current() -> Self {
        Self::from_parts(
            env!("CARGO_PKG_VERSION"),
            option_env!("VERGEN_GIT_SHA"),
            option_env!("VERGEN_GIT_DIRTY"),
        )
    }

    /// From the raw build-script variables; a missing or placeholder value
    /// is unknown.
    pub fn from_parts(version: &str, sha: Option<&str>, dirty: Option<&str>) -> Self {
        fn known(v: Option<&str>) -> Option<&str> {
            v.filter(|v| !v.is_empty() && *v != PLACEHOLDER)
        }
        BuildInfo {
            version: version.to_string(),
            commit: known(sha).unwrap_or("unknown").to_string(),
            dirty: known(dirty).map(|d| d == "true"),
        }
    }

    /// As sent in `Hello` and `HelloAck`.
    pub fn to_wire(&self) -> v1::BuildInfo {
        v1::BuildInfo {
            version: self.version.clone(),
            commit: self.commit.clone(),
            dirty: self.dirty,
        }
    }

    /// From a peer's `Hello` or `HelloAck`; an older peer sends none.
    pub fn from_wire(wire: Option<&v1::BuildInfo>) -> Self {
        let Some(w) = wire else {
            return BuildInfo::from_parts("unknown", None, None);
        };
        let or_unknown = |s: &str| if s.is_empty() { "unknown" } else { s }.to_string();
        BuildInfo {
            version: or_unknown(&w.version),
            commit: or_unknown(&w.commit),
            dirty: w.dirty,
        }
    }
}

impl BuildInfo {
    /// The dirty flag as a word: "true", "false" or "unknown".
    pub fn dirty_label(&self) -> &'static str {
        match self.dirty {
            Some(true) => "true",
            Some(false) => "false",
            None => "unknown",
        }
    }

    /// Announces this build at info; called once logging is up.
    pub fn log_startup(&self) {
        tracing::info!(
            version = %self.version,
            commit = %self.commit,
            dirty = %self.dirty_label(),
            "starting irsync"
        );
    }
}

impl fmt::Display for BuildInfo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} ({}", self.version, self.commit)?;
        if self.dirty == Some(true) {
            f.write_str(", dirty")?;
        }
        f.write_str(")")?;
        #[cfg(feature = "netsim")]
        f.write_str(" [netsim]")?;
        Ok(())
    }
}

/// `BuildInfo::current()` rendered once, for clap's `--version` and help.
pub static LONG_VERSION: LazyLock<String> = LazyLock::new(|| BuildInfo::current().to_string());
