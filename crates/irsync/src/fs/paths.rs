//! Byte-exact relative paths with component-wise ordering.

use std::cmp::Ordering;
use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

/// A path relative to a root: no leading slash, `/`-separated raw bytes,
/// the root itself being the empty path.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub struct RelPath(pub Vec<u8>);

impl RelPath {
    pub fn components(&self) -> impl Iterator<Item = &[u8]> {
        self.0.split(|&b| b == b'/').filter(|c| !c.is_empty())
    }

    pub fn join(&self, name: &[u8]) -> RelPath {
        if self.0.is_empty() {
            return RelPath(name.to_vec());
        }
        let mut v = Vec::with_capacity(self.0.len() + 1 + name.len());
        v.extend_from_slice(&self.0);
        v.push(b'/');
        v.extend_from_slice(name);
        RelPath(v)
    }

    pub fn to_os(&self) -> &OsStr {
        OsStr::from_bytes(&self.0)
    }
}

impl Ord for RelPath {
    fn cmp(&self, other: &Self) -> Ordering {
        self.components().cmp(other.components())
    }
}

impl PartialOrd for RelPath {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// True if `root/rel` stays lexically inside `root`: rejects absolute paths
/// and any `..` component.
pub fn is_contained(_root: &Path, rel: &RelPath) -> bool {
    !rel.0.starts_with(b"/") && rel.components().all(|c| c != b"..")
}
