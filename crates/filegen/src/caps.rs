//! What the machine and the target directory allow, beyond what every
//! platform has: found once, at the start of a run.

use crate::config::Weights;
use crate::kind::OpKind;
use crate::platform::Platform;
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

/// The kinds that cannot run on `platform` with `caps`, in
/// [`OpKind::ALL`] order. On Unix with every capability, none.
pub fn disabled(platform: Platform, caps: Caps) -> Vec<OpKind> {
    OpKind::ALL
        .into_iter()
        .filter(|&kind| match kind {
            OpKind::Mkfifo | OpKind::Chmod | OpKind::SetXattr | OpKind::Chown | OpKind::Mknod => {
                platform == Platform::Windows
            }
            OpKind::Symlink | OpKind::RepointSymlink | OpKind::DeleteSymlink => !caps.symlinks,
            OpKind::PunchHole => !caps.sparse,
            _ => false,
        })
        .collect()
}

/// A copy of `weights` with the weight of every transition into a
/// disabled kind set to 0.
pub fn without(weights: &Weights, disabled: &[OpKind]) -> Weights {
    let mut out = weights.clone();
    for row in out.values_mut() {
        for kind in disabled {
            if let Some(w) = row.get_mut(kind) {
                *w = 0.0;
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::default_weights;

    #[test]
    fn nothing_is_disabled_on_unix() {
        assert!(disabled(Platform::Unix, Caps::ALL).is_empty());
    }

    #[test]
    fn windows_disables_the_posix_kinds() {
        assert_eq!(
            disabled(Platform::Windows, Caps::ALL),
            [
                OpKind::Mkfifo,
                OpKind::Chmod,
                OpKind::SetXattr,
                OpKind::Chown,
                OpKind::Mknod
            ]
        );
    }

    #[test]
    fn missing_capabilities_disable_their_kinds() {
        let no_links = Caps {
            symlinks: false,
            sparse: true,
        };
        let d = disabled(Platform::Windows, no_links);
        for k in [
            OpKind::Symlink,
            OpKind::RepointSymlink,
            OpKind::DeleteSymlink,
        ] {
            assert!(d.contains(&k), "{k:?}");
        }
        assert!(!d.contains(&OpKind::PunchHole));
        assert_eq!(d.len(), 8);
        let no_sparse = Caps {
            symlinks: true,
            sparse: false,
        };
        let d = disabled(Platform::Windows, no_sparse);
        assert!(d.contains(&OpKind::PunchHole));
        assert_eq!(d.len(), 6);
        // The order is OpKind::ALL's.
        assert!(d.windows(2).all(|w| w[0].index() < w[1].index()));
    }

    #[test]
    fn without_zeroes_only_the_columns() {
        let w = default_weights();
        let off = [OpKind::Mkfifo, OpKind::PunchHole];
        let z = without(&w, &off);
        for (from, row) in &w {
            for (to, &v) in row {
                let got = z[from][to];
                if off.contains(to) {
                    assert_eq!(got, 0.0, "{from:?}->{to:?}");
                } else {
                    assert_eq!(got, v, "{from:?}->{to:?}");
                }
            }
        }
    }

    #[test]
    fn log_values() {
        let caps = |symlinks, sparse| Caps { symlinks, sparse };
        assert_eq!(Caps::ALL.log_value(), "symlinks,sparse");
        assert_eq!(caps(true, false).log_value(), "symlinks");
        assert_eq!(caps(false, true).log_value(), "sparse");
        assert_eq!(caps(false, false).log_value(), "-");
    }
}
