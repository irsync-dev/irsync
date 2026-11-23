//! Byte-exact relative paths with component-wise ordering.

use std::cmp::Ordering;
use std::ffi::OsString;
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

    /// The path as a name of this system. On Unix any bytes are a name, so
    /// this cannot fail there.
    pub fn to_os(&self) -> OsString {
        crate::platform::bytes_to_os(&self.0).expect("a relative path is a name of this system")
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

/// Whether `c` must not reach a terminal or a log as it is: a control
/// character, or one of the invisible characters that change the direction
/// text is shown in (the bidi marks, embeddings, overrides and isolates),
/// which can make a name read as another, `photo<U+202E>gpj.exe` as
/// `photoexe.jpg`.
fn hidden(c: char) -> bool {
    c.is_control()
        || matches!(
            c as u32,
            0x061c | 0x200e | 0x200f | 0x202a..=0x202e | 0x2066..=0x2069
        )
}

/// Text from the other side (an error message, a name, a code) as it is
/// safe to print: control characters become `\ooo`, so it stays on one
/// line and cannot drive the terminal. Unlike a path, a backslash is left
/// as it is, so that ordinary messages read as written.
pub fn escape_text(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if hidden(c) {
            let mut buf = [0u8; 4];
            c.encode_utf8(&mut buf)
                .bytes()
                .for_each(|b| octal(&mut out, b));
        } else {
            out.push(c);
        }
    }
    out
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
            if hidden(c) || c == '\\' {
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
    fn escape_text_keeps_a_message_on_one_harmless_line() {
        assert_eq!(
            escape_text("plain: caf\u{e9} (os error 2)"),
            "plain: caf\u{e9} (os error 2)"
        );
        assert_eq!(escape_text(r"C:\dir"), r"C:\dir");
        assert_eq!(
            escape_text("a\x1b[2J\r\nb\x07\u{9b}c\x7f"),
            r"a\033[2J\015\012b\007\302\233c\177"
        );
    }

    #[test]
    fn direction_changing_characters_are_escaped() {
        // RIGHT-TO-LEFT OVERRIDE: shown raw, the name reads `photoexe.jpg`.
        assert_eq!(
            escape_path("photo\u{202e}gpj.exe".as_bytes()),
            r"photo\342\200\256gpj.exe"
        );
        assert_eq!(escape_text("a\u{202e}b"), r"a\342\200\256b");
        for c in [
            '\u{61c}', '\u{200e}', '\u{200f}', '\u{202a}', '\u{202d}', '\u{2066}', '\u{2069}',
        ] {
            let name = format!("a{c}b");
            assert!(!escape_path(name.as_bytes()).contains(c), "{:x}", c as u32);
            assert!(!escape_text(&name).contains(c), "{:x}", c as u32);
        }
        // Text in a right-to-left script is left as it is.
        let hebrew = "\u{5e9}\u{5dc}\u{5d5}\u{5dd}.txt";
        assert_eq!(escape_path(hebrew.as_bytes()), hebrew);
    }

    #[test]
    fn parent_drops_the_last_component() {
        assert_eq!(RelPath::new(b"a/b/c").unwrap().parent().as_bytes(), b"a/b");
        assert_eq!(RelPath::new(b"a").unwrap().parent(), RelPath::root());
        assert_eq!(RelPath::root().parent(), RelPath::root());
    }
}
