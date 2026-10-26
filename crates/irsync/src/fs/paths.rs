//! Byte-exact relative paths with component-wise ordering.

use std::cmp::Ordering;
use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

/// Error for bytes that are not a canonical relative path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvalidPath;

impl std::fmt::Display for InvalidPath {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("invalid relative path")
    }
}

impl std::error::Error for InvalidPath {}

fn valid_name(c: &[u8]) -> bool {
    !c.is_empty() && c != b"." && c != b".." && !c.contains(&b'/') && !c.contains(&0)
}

/// A canonical path relative to a root: no leading slash, `/`-separated raw
/// bytes, no empty, `.` or `..` components, no NUL; the root is the empty path.
/// Canonical form makes derived `Eq`/`Hash` consistent with the component-wise `Ord`.
#[derive(Clone, PartialEq, Eq, Hash, Default)]
pub struct RelPath(Vec<u8>);

/// The escaped path in quotes (`"a/b\\012"`), not the byte list, so that
/// `?path` in a log line reads as a path.
impl std::fmt::Debug for RelPath {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "\"{}\"", escape_path(&self.0))
    }
}

impl RelPath {
    pub fn root() -> RelPath {
        RelPath(Vec::new())
    }

    pub fn new(bytes: &[u8]) -> Result<RelPath, InvalidPath> {
        if !bytes.is_empty() && !bytes.split(|&b| b == b'/').all(valid_name) {
            return Err(InvalidPath);
        }
        Ok(RelPath(bytes.to_vec()))
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }

    pub fn components(&self) -> impl Iterator<Item = &[u8]> {
        self.0.split(|&b| b == b'/').filter(|c| !c.is_empty())
    }

    /// The path without its last component; the root for a top-level entry.
    pub fn parent(&self) -> RelPath {
        match self.0.iter().rposition(|&b| b == b'/') {
            Some(i) => RelPath(self.0[..i].to_vec()),
            None => RelPath::root(),
        }
    }

    pub fn join(&self, name: &[u8]) -> Result<RelPath, InvalidPath> {
        if !valid_name(name) {
            return Err(InvalidPath);
        }
        if self.0.is_empty() {
            return Ok(RelPath(name.to_vec()));
        }
        let mut v = Vec::with_capacity(self.0.len() + 1 + name.len());
        v.extend_from_slice(&self.0);
        v.push(b'/');
        v.extend_from_slice(name);
        Ok(RelPath(v))
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

/// Lexical containment check over raw wire bytes: true if `rel` is a canonical
/// relative path (no absolute, `..`, `.`, empty components or NUL). `_root` is
/// reserved; symlink escapes are not checked here.
pub fn is_contained(_root: &Path, rel: &[u8]) -> bool {
    RelPath::new(rel).is_ok()
}

/// A path as rsync prints it: bytes that are not valid UTF-8, control
/// characters and backslashes become `\ooo` (octal); never fails.
fn octal(out: &mut String, b: u8) {
    out.push_str(&format!("\\{b:03o}"));
}

pub fn escape_path(p: &[u8]) -> String {
    let mut out = String::new();
    let mut rest = p;
    while !rest.is_empty() {
        let (good, bad) = match std::str::from_utf8(rest) {
            Ok(s) => (s, 0),
            Err(e) => {
                let (g, _) = rest.split_at(e.valid_up_to());
                // `g` is valid by construction.
                (
                    std::str::from_utf8(g).unwrap_or(""),
                    e.error_len().unwrap_or(rest.len() - g.len()),
                )
            }
        };
        for c in good.chars() {
            if c.is_control() || c == '\\' {
                // Control characters and `\` are all ASCII or C1 (2 bytes).
                let mut buf = [0u8; 4];
                c.encode_utf8(&mut buf)
                    .bytes()
                    .for_each(|b| octal(&mut out, b));
            } else {
                out.push(c);
            }
        }
        rest = &rest[good.len()..];
        rest[..bad].iter().for_each(|&b| octal(&mut out, b));
        rest = &rest[bad..];
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_shows_the_escaped_path_not_the_bytes() {
        assert_eq!(
            format!("{:?}", RelPath::new(b"a/b\n").unwrap()),
            r#""a/b\012""#
        );
        assert_eq!(
            format!("{:?}", RelPath::new("caf\u{e9}".as_bytes()).unwrap()),
            "\"caf\u{e9}\""
        );
        assert_eq!(format!("{:?}", RelPath::new(b"\xff").unwrap()), r#""\377""#);
        assert_eq!(format!("{:?}", RelPath::root()), r#""""#);
    }

    #[test]
    fn parent_drops_the_last_component() {
        assert_eq!(RelPath::new(b"a/b/c").unwrap().parent().as_bytes(), b"a/b");
        assert_eq!(RelPath::new(b"a").unwrap().parent(), RelPath::root());
        assert_eq!(RelPath::root().parent(), RelPath::root());
    }
}
