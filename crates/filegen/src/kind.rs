//! Operation kinds: the Markov-chain states and YAML config keys.

use serde::{Deserialize, Serialize};

/// Every operation the generator can perform, in spec order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OpKind {
    CreateFile,
    Append,
    Truncate,
    OverwriteRange,
    Rewrite,
    PunchHole,
    DeleteFile,
    Mkdir,
    Rmdir,
    RemoveTree,
    Rename,
    RenameOver,
    Symlink,
    RepointSymlink,
    DeleteSymlink,
    Hardlink,
    Mkfifo,
    Chmod,
    SetMtime,
    SetXattr,
    Chown,
    Mknod,
}

impl OpKind {
    pub const ALL: [OpKind; 22] = [
        OpKind::CreateFile,
        OpKind::Append,
        OpKind::Truncate,
        OpKind::OverwriteRange,
        OpKind::Rewrite,
        OpKind::PunchHole,
        OpKind::DeleteFile,
        OpKind::Mkdir,
        OpKind::Rmdir,
        OpKind::RemoveTree,
        OpKind::Rename,
        OpKind::RenameOver,
        OpKind::Symlink,
        OpKind::RepointSymlink,
        OpKind::DeleteSymlink,
        OpKind::Hardlink,
        OpKind::Mkfifo,
        OpKind::Chmod,
        OpKind::SetMtime,
        OpKind::SetXattr,
        OpKind::Chown,
        OpKind::Mknod,
    ];

    /// The snake_case name used in configs and logs.
    pub fn name(self) -> &'static str {
        match self {
            OpKind::CreateFile => "create_file",
            OpKind::Append => "append",
            OpKind::Truncate => "truncate",
            OpKind::OverwriteRange => "overwrite_range",
            OpKind::Rewrite => "rewrite",
            OpKind::PunchHole => "punch_hole",
            OpKind::DeleteFile => "delete_file",
            OpKind::Mkdir => "mkdir",
            OpKind::Rmdir => "rmdir",
            OpKind::RemoveTree => "remove_tree",
            OpKind::Rename => "rename",
            OpKind::RenameOver => "rename_over",
            OpKind::Symlink => "symlink",
            OpKind::RepointSymlink => "repoint_symlink",
            OpKind::DeleteSymlink => "delete_symlink",
            OpKind::Hardlink => "hardlink",
            OpKind::Mkfifo => "mkfifo",
            OpKind::Chmod => "chmod",
            OpKind::SetMtime => "set_mtime",
            OpKind::SetXattr => "set_xattr",
            OpKind::Chown => "chown",
            OpKind::Mknod => "mknod",
        }
    }

    /// Position in [`OpKind::ALL`].
    pub fn index(self) -> usize {
        self as usize
    }

    /// Whether the operation tends to add bytes to the tree.
    pub fn grows(self) -> bool {
        matches!(
            self,
            OpKind::CreateFile | OpKind::Append | OpKind::Rewrite | OpKind::Hardlink
        )
    }

    /// Whether the operation tends to remove bytes from the tree.
    pub fn shrinks(self) -> bool {
        matches!(
            self,
            OpKind::Truncate | OpKind::DeleteFile | OpKind::RemoveTree
        )
    }

    /// Kinds needing privileges or special support are off by default.
    pub fn default_enabled(self) -> bool {
        !matches!(self, OpKind::SetXattr | OpKind::Chown | OpKind::Mknod)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kind_names_round_trip_serde() {
        for k in OpKind::ALL {
            let s = serde_yaml_ng::to_string(&k).unwrap();
            assert_eq!(s.trim(), k.name());
            assert_eq!(serde_yaml_ng::from_str::<OpKind>(&s).unwrap(), k);
        }
        assert_eq!(OpKind::OverwriteRange.name(), "overwrite_range");
    }

    #[test]
    fn kind_classification() {
        assert!(OpKind::Append.grows());
        assert!(OpKind::Truncate.shrinks());
        assert!(!OpKind::Chmod.grows());
        assert!(!OpKind::SetXattr.default_enabled());
        assert!(OpKind::Mkfifo.default_enabled());
    }

    #[test]
    fn kind_index_matches_all_order() {
        for (i, k) in OpKind::ALL.iter().enumerate() {
            assert_eq!(k.index(), i);
        }
        assert!(!OpKind::Rewrite.shrinks());
        assert!(!OpKind::Chown.default_enabled() && !OpKind::Mknod.default_enabled());
    }
}
