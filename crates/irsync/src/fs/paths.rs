//! Byte-exact relative paths with component-wise ordering.

use std::cmp::Ordering;
use std::ffi::{OsStr, OsString};
use std::path::Path;

use crate::platform::Platform;

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

    /// The path as a relative path of this system, with its separator
    /// between the components (`\` on Windows; on Unix the bytes as they
    /// are). On Unix any bytes are a name, so this cannot fail there.
    pub fn to_os(&self) -> OsString {
        crate::platform::bytes_to_os(&native_separators(&self.0, Platform::CURRENT))
            .expect("a relative path is a name of this system")
    }

    /// The canonical relative path that `rel`, a relative path of this
    /// system, names: each separator of the system becomes `/` (see
    /// [`slash_separators`]), then the bytes are checked as [`RelPath::new`]
    /// checks them. On Unix that is `RelPath::new` of the bytes as they are.
    pub fn from_os(rel: &OsStr) -> Result<RelPath, InvalidPath> {
        RelPath::new(&slash_separators(rel))
    }
}

/// The bytes of `rel`, a path of this system, with `/` between the
/// components, as a path inside a tree is stored and sent: on Windows each
/// `\` becomes `/` (a name there can hold neither); on Unix the bytes as
/// they are. Nothing else is checked.
pub fn slash_separators(rel: &OsStr) -> Vec<u8> {
    slashes_for(crate::platform::os_to_bytes(rel), Platform::CURRENT)
}

/// [`slash_separators`] for `platform`'s separators.
fn slashes_for(mut bytes: Vec<u8>, platform: Platform) -> Vec<u8> {
    for b in &mut bytes {
        if platform.is_separator(*b) {
            *b = b'/';
        }
    }
    bytes
}

/// Stored bytes with `platform`'s separator in place of each `/`.
fn native_separators(bytes: &[u8], platform: Platform) -> Vec<u8> {
    let sep = platform.separator();
    bytes
        .iter()
        .map(|&b| if b == b'/' { sep } else { b })
        .collect()
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

/// A path of this system (its stored bytes, [`crate::platform::os_to_bytes`])
/// as it is shown in a message: see [`escape_os_path_on`]. A path inside a
/// tree (a [`RelPath`]) is shown with [`escape_path`] instead.
pub fn escape_os_path(p: &[u8]) -> String {
    escape_os_path_on(p, Platform::CURRENT)
}

/// A path of `platform` as it is shown in a message.
///
/// On Unix this is [`escape_path`]: a backslash is an ordinary byte of a
/// name there, so it is escaped with the rest, and the output cannot be
/// mistaken for another path.
///
/// On Windows a backslash is the separator and no name can hold one, so it
/// is shown as it is (`C:\Users\d`). What may not be shown as it is gets a
/// form in angle brackets, which no Windows name can hold either (`<` and
/// `>` are refused in names), so it cannot be read as part of a name: a
/// control or direction-changing character as `<U+001B>`, an unpaired
/// surrogate (stored as WTF-8) as `<U+D800>`, and a byte that is neither
/// UTF-8 nor WTF-8 as `<0xFF>`.
pub fn escape_os_path_on(p: &[u8], platform: Platform) -> String {
    if platform == Platform::Unix {
        return escape_path(p);
    }
    let mut out = String::new();
    let mut rest = p;
    while !rest.is_empty() {
        let (good, bad) = match std::str::from_utf8(rest) {
            Ok(s) => (s, 0),
            Err(e) => (
                // Valid by construction.
                std::str::from_utf8(&rest[..e.valid_up_to()]).unwrap_or(""),
                e.error_len().unwrap_or(rest.len() - e.valid_up_to()),
            ),
        };
        for c in good.chars() {
            if hidden(c) {
                out.push_str(&format!("<U+{:04X}>", c as u32));
            } else {
                out.push(c);
            }
        }
        rest = &rest[good.len()..];
        if bad == 0 {
            continue;
        }
        // WTF-8's unpaired surrogate: ED A0..=BF 80..=BF.
        match rest {
            [0xed, b1 @ 0xa0..=0xbf, b2 @ 0x80..=0xbf, ..] => {
                let unit = 0xd000 | (u32::from(b1 & 0x3f) << 6) | u32::from(b2 & 0x3f);
                out.push_str(&format!("<U+{unit:04X}>"));
                rest = &rest[3..];
            }
            [b, ..] => {
                out.push_str(&format!("<0x{b:02X}>"));
                rest = &rest[1..];
            }
            [] => {}
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
    fn separators_are_the_platforms_on_the_way_out_and_slashes_on_the_way_in() {
        let v = |s: &str| s.as_bytes().to_vec();
        // Unix: the bytes as they are, both ways; a backslash is a name's.
        assert_eq!(native_separators(b"a/b/c", Platform::Unix), v("a/b/c"));
        assert_eq!(slashes_for(v(r"a/b\c"), Platform::Unix), v(r"a/b\c"));
        // Windows: `\` out; `\` and `/` both in.
        assert_eq!(native_separators(b"a/b/c", Platform::Windows), v(r"a\b\c"));
        assert_eq!(slashes_for(v(r"a\b/c"), Platform::Windows), v("a/b/c"));
        // A separator never hides in a longer UTF-8 sequence.
        assert_eq!(
            slashes_for(v("\u{5c5c}\\x"), Platform::Windows),
            v("\u{5c5c}/x")
        );
        assert_eq!(native_separators(b"", Platform::Windows), v(""));
    }

    #[test]
    fn a_relative_path_of_this_system_becomes_a_rel_path_and_back() {
        let os = |s: &str| std::ffi::OsString::from(s);
        let p = RelPath::from_os(&os("d/b.txt")).unwrap();
        assert_eq!(p.as_bytes(), b"d/b.txt");
        assert_eq!(
            std::path::Path::new("root").join(p.to_os()),
            std::path::Path::new("root").join("d").join("b.txt")
        );
        assert!(RelPath::from_os(&os("a/../b")).is_err());
        assert!(RelPath::from_os(&os("a//b")).is_err());
        if Platform::CURRENT == Platform::Windows {
            assert_eq!(
                RelPath::from_os(&os(r"d\e\f")).unwrap().as_bytes(),
                b"d/e/f"
            );
            assert_eq!(p.to_os(), os(r"d\b.txt"));
        } else {
            assert_eq!(RelPath::from_os(&os(r"d\e")).unwrap().as_bytes(), br"d\e");
            assert_eq!(p.to_os(), os("d/b.txt"));
        }
    }

    #[test]
    fn a_path_of_the_system_shows_its_separators_on_windows_and_is_escaped_on_unix() {
        let path = br"C:\Users\d/x";
        assert_eq!(escape_os_path_on(path, Platform::Unix), escape_path(path));
        assert_eq!(
            escape_os_path_on(path, Platform::Unix),
            r"C:\134Users\134d/x"
        );
        assert_eq!(escape_os_path_on(path, Platform::Windows), r"C:\Users\d/x");
        // What cannot be shown as it is, in a form no Windows name holds.
        assert_eq!(
            escape_os_path_on(b"C:\\a\x1b[2Jb", Platform::Windows),
            r"C:\a<U+001B>[2Jb"
        );
        assert_eq!(
            escape_os_path_on("C:\\photo\u{202e}gpj.exe".as_bytes(), Platform::Windows),
            r"C:\photo<U+202E>gpj.exe"
        );
        assert_eq!(
            escape_os_path_on(b"C:\\caf\xed\xa0\x80\\x", Platform::Windows),
            r"C:\caf<U+D800>\x"
        );
        assert_eq!(
            escape_os_path_on(b"C:\\a\xff\xe9", Platform::Windows),
            r"C:\a<0xFF><0xE9>"
        );
        assert_eq!(
            escape_os_path_on("C:\\caf\u{e9}".as_bytes(), Platform::Windows),
            "C:\\caf\u{e9}"
        );
        assert_eq!(escape_os_path(b"/srv/x"), "/srv/x");
    }

    #[test]
    fn parent_drops_the_last_component() {
        assert_eq!(RelPath::new(b"a/b/c").unwrap().parent().as_bytes(), b"a/b");
        assert_eq!(RelPath::new(b"a").unwrap().parent(), RelPath::root());
        assert_eq!(RelPath::root().parent(), RelPath::root());
    }
}
