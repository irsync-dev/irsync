//! What the machine and the target directory allow, beyond what every
//! platform has: found once, at the start of a run.

use serde::{Deserialize, Serialize};

/// The capabilities filegen found where it runs.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub struct Caps {
    /// This user may create symlinks.
    pub symlinks: bool,
    /// The filesystem takes sparse files (holes can be punched).
    pub sparse: bool,
}

impl Caps {
    /// Every capability: what Unix always has.
    pub const ALL: Caps = Caps {
        symlinks: true,
        sparse: true,
    };

    /// The capabilities for the log: their names joined with `,`, or `-`
    /// for none.
    pub fn log_value(self) -> String {
        let names: Vec<&str> = [("symlinks", self.symlinks), ("sparse", self.sparse)]
            .into_iter()
            .filter_map(|(name, on)| on.then_some(name))
            .collect();
        if names.is_empty() {
            "-".to_string()
        } else {
            names.join(",")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn log_values() {
        let caps = |symlinks, sparse| Caps { symlinks, sparse };
        assert_eq!(Caps::ALL.log_value(), "symlinks,sparse");
        assert_eq!(caps(true, false).log_value(), "symlinks");
        assert_eq!(caps(false, true).log_value(), "sparse");
        assert_eq!(caps(false, false).log_value(), "-");
    }
}
