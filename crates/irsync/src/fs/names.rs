//! The rule for names that arrive from a peer: whether one component can
//! exist on this kind of system. Applied where a name enters (a request's
//! path and name, a partial dir, each entry of a listing), before any join
//! or conversion to a name of the system, so nothing a peer sends can be
//! read by the system as more than one name (`..\x`, `C:\x`, `a:stream`).
//!
//! `/`, NUL, `.`, `..` and empty components are refused on every system by
//! [`RelPath`] itself; this adds what a platform cannot hold beyond that.

use crate::fs::paths::RelPath;
use crate::platform::Platform;

/// Why `name` (stored bytes, one component) cannot exist on `platform`, if
/// it cannot.
///
/// On Windows: bytes that are not WTF-8 (`"not a valid name on this
/// system"`), and any of `< > : " | ? * \` or a byte below 0x20 (`"holds a
/// character Windows does not allow"`). Names that are legal but awkward
/// (`CON`, a trailing dot or space) are allowed, as the design says.
///
/// On Unix every name is allowed: `None` for any bytes.
pub fn refused(name: &[u8], platform: Platform) -> Option<&'static str> {
    match platform {
        Platform::Unix => None,
        Platform::Windows => {
            if name.iter().any(|&b| b < 0x20 || b"<>:\"|?*\\".contains(&b)) {
                Some(NOT_ALLOWED)
            } else if crate::wtf8::decode(name).is_err() {
                Some(NOT_VALID)
            } else {
                None
            }
        }
    }
}

/// Why a symlink to `target` (from a peer's listing) is not created on
/// `platform`, if it is not.
///
/// On Windows a target that, with `/` read as `\` (as the link is made),
/// starts with two separators (`\\server\share`, `//server`, `\\?\…`,
/// `\\.\…`) or with `\??\` names a network or device path: following the
/// link would make this system open it. Drive-absolute (`C:\…`) and
/// relative targets are made as before.
///
/// On Unix every target is allowed: `None` for any bytes.
pub fn link_target_refused(target: &[u8], platform: Platform) -> Option<&'static str> {
    match platform {
        Platform::Unix => None,
        Platform::Windows => {
            let sep = |b: Option<&u8>| matches!(b, Some(b'\\' | b'/'));
            let two = sep(target.first()) && sep(target.get(1));
            let nt =
                sep(target.first()) && target.get(1..3) == Some(&b"??"[..]) && sep(target.get(3));
            (two || nt).then_some(NETWORK_LINK)
        }
    }
}

/// Whether two names (stored bytes) differ only in case, at least as
/// loosely as NTFS folds them: two characters are the same when they are
/// equal, or their uppercase is, or their lowercase is, each taken only
/// where it is a single character (`ß`, whose uppercase is `SS`, stays as
/// it is). The lowercase catches pairs whose full uppercase is several
/// characters but which NTFS still folds, such as Greek letters with
/// ypogegrammeni (`ᾀ` and `ᾈ`). A name that is WTF-8 but not UTF-8 (it
/// holds an unpaired surrogate) is compared per UTF-16 unit, as NTFS does:
/// each unit that is a character by itself as above, a surrogate exactly.
/// Bytes that are not WTF-8 are compared exactly.
pub fn same_ignoring_case(a: &[u8], b: &[u8]) -> bool {
    match (std::str::from_utf8(a), std::str::from_utf8(b)) {
        (Ok(a), Ok(b)) => {
            a.chars().count() == b.chars().count()
                && a.chars().zip(b.chars()).all(|(x, y)| same_char(x, y))
        }
        (Ok(_), Err(_)) | (Err(_), Ok(_)) => false,
        (Err(_), Err(_)) => match (crate::wtf8::decode(a), crate::wtf8::decode(b)) {
            (Ok(a), Ok(b)) => {
                a.len() == b.len()
                    && a.iter().zip(&b).all(|(&x, &y)| match (unit(x), unit(y)) {
                        (Some(x), Some(y)) => same_char(x, y),
                        _ => x == y,
                    })
            }
            _ => a == b,
        },
    }
}

/// A key for [`same_ignoring_case`]: two names that are the same ignoring
/// case have equal keys. The key may be coarser (two names with equal keys
/// are not always the same), so a match found by key is confirmed with
/// [`same_ignoring_case`]. Each character becomes [`fold`]ed; a name that
/// is WTF-8 but not UTF-8 is keyed per UTF-16 unit, a surrogate kept as it
/// is; bytes that are not WTF-8 are their own key. A leading tag keeps the
/// three kinds apart (no two names of different kinds are the same).
pub fn case_key(name: &[u8]) -> Vec<u8> {
    let mut key = Vec::with_capacity(name.len() + 1);
    if let Ok(s) = std::str::from_utf8(name) {
        key.push(0);
        let mut buf = [0u8; 4];
        for c in s.chars() {
            key.extend_from_slice(fold(c).encode_utf8(&mut buf).as_bytes());
        }
    } else if let Ok(units) = crate::wtf8::decode(name) {
        key.push(1);
        for u in units {
            let k = unit(u).map_or(u32::from(u), |c| u32::from(fold(c)));
            key.extend_from_slice(&k.to_le_bytes());
        }
    } else {
        key.push(2);
        key.extend_from_slice(name);
    }
    key
}

/// A UTF-16 unit as the character it is by itself; `None` for a surrogate.
fn unit(u: u16) -> Option<char> {
    char::from_u32(u32::from(u))
}

/// `c`'s case mapping (`to_uppercase` or `to_lowercase`) where it is a
/// single character, else `c`.
fn single(mut it: impl Iterator<Item = char>, c: char) -> char {
    match (it.next(), it.next()) {
        (Some(one), None) => one,
        _ => c,
    }
}

fn upper(c: char) -> char {
    single(c.to_uppercase(), c)
}

fn lower(c: char) -> char {
    single(c.to_lowercase(), c)
}

/// Two characters are the same ignoring case: equal, or equal in their
/// single-character uppercase, or in their single-character lowercase.
fn same_char(x: char, y: char) -> bool {
    x == y || upper(x) == upper(y) || lower(x) == lower(y)
}

/// The folded form of `c` that [`case_key`] uses: the lowercase of its
/// uppercase. `same_char(x, y)` implies `fold(x) == fold(y)`: a test over
/// every character checks `fold(c) == fold(upper(c)) == fold(lower(c))`,
/// so two characters with one uppercase (or one lowercase) fold alike.
pub(crate) fn fold(c: char) -> char {
    lower(upper(c))
}

/// The message for a link target that names a network or device path.
pub const NETWORK_LINK: &str = "a link to a network or device path is not created on Windows";

/// The message for a character Windows does not allow in a name.
pub const NOT_ALLOWED: &str = "holds a character Windows does not allow";
/// The message for bytes that are no name of this system.
pub const NOT_VALID: &str = "not a valid name on this system";

/// [`refused`] for the first component of `path` it refuses, on `platform`.
pub fn refused_path(path: &RelPath, platform: Platform) -> Option<&'static str> {
    path.components().find_map(|c| refused(c, platform))
}

/// `bytes` from a peer as a path inside a tree: a canonical [`RelPath`]
/// whose every component exists on this system. On Unix that is
/// `RelPath::new(bytes)`. `Err` holds why not.
pub fn peer_path(bytes: &[u8]) -> Result<RelPath, &'static str> {
    let p = RelPath::new(bytes).map_err(|_| "invalid relative path")?;
    match refused_path(&p, Platform::CURRENT) {
        Some(why) => Err(why),
        None => Ok(p),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_refuses_what_ntfs_cannot_hold() {
        let w = Platform::Windows;
        for bad in [&b"a:b"[..], b"a*b", b"a\x01", b"a\xff", b"a\\b"] {
            assert!(refused(bad, w).is_some(), "{bad:?}");
            assert_eq!(refused(bad, Platform::Unix), None, "{bad:?}");
        }
        for good in [&b"CON"[..], b"trail.", b"a b", "\u{e9}".as_bytes()] {
            assert_eq!(refused(good, w), None, "{good:?}");
            assert_eq!(refused(good, Platform::Unix), None, "{good:?}");
        }
        for c in br#"<>:"|?*\"# {
            assert_eq!(refused(&[b'x', *c], w), Some(NOT_ALLOWED), "{c}");
        }
        assert_eq!(refused(b"a\xff", w), Some(NOT_VALID));
        // An unpaired surrogate is a name there (WTF-8).
        assert_eq!(refused(b"a\xed\xa0\x80", w), None);
    }

    #[test]
    fn case_only_differences() {
        assert!(same_ignoring_case(b"A", b"a"));
        assert!(same_ignoring_case("É".as_bytes(), "é".as_bytes()));
        assert!(same_ignoring_case(b"ReadOnly", b"rEADoNLY"));
        assert!(!same_ignoring_case(b"a", b"b"));
        assert!(!same_ignoring_case("ß".as_bytes(), b"SS"));
        assert!(!same_ignoring_case(b"a", b"aa"));
        // Full uppercase of several characters, one simple case pair: NTFS
        // folds them, so they are the same here too.
        assert!(same_ignoring_case("ᾀ".as_bytes(), "ᾈ".as_bytes()));
        assert!(same_ignoring_case("ᾳ".as_bytes(), "ᾼ".as_bytes()));
        assert!(same_ignoring_case("xᾀy".as_bytes(), "XᾈY".as_bytes()));
        for n in [&b"x"[..], "ß".as_bytes(), b"a\xff"] {
            assert!(same_ignoring_case(n, n), "{n:?}");
        }
        assert!(!same_ignoring_case(b"a\xff", b"A\xff"));
        // An unpaired surrogate (WTF-8): the other units fold, NTFS does.
        assert!(same_ignoring_case(b"A\xed\xa0\x80", b"a\xed\xa0\x80"));
        assert!(!same_ignoring_case(b"a\xed\xa0\x80", b"a\xed\xa0\x81"));
        assert!(!same_ignoring_case(b"a\xed\xa0\x80", b"a"));
    }

    /// Every character folds as its uppercase and its lowercase do, so two
    /// characters `same_char` takes for one have one fold.
    #[test]
    fn fold_agrees_with_the_comparison_for_every_character() {
        for c in (0..=0x10FFFF).filter_map(char::from_u32) {
            assert_eq!(fold(c), fold(upper(c)), "{c:?} {:X}", c as u32);
            assert_eq!(fold(c), fold(lower(c)), "{c:?} {:X}", c as u32);
        }
    }

    /// Names that are the same ignoring case have one key.
    #[test]
    fn the_case_key_matches_the_comparison() {
        let pairs: [(&[u8], &[u8]); 7] = [
            (b"A", b"a"),
            ("É".as_bytes(), "é".as_bytes()),
            (b"ReadOnly", b"rEADoNLY"),
            ("ᾀ".as_bytes(), "ᾈ".as_bytes()),
            ("xᾀy".as_bytes(), "XᾈY".as_bytes()),
            (b"A\xed\xa0\x80", b"a\xed\xa0\x80"),
            ("\u{212A}".as_bytes(), b"k"),
        ];
        for (a, b) in pairs {
            assert!(same_ignoring_case(a, b), "{a:?} {b:?}");
            assert_eq!(case_key(a), case_key(b), "{a:?} {b:?}");
        }
        for (a, b) in [
            (&b"a"[..], &b"b"[..]),
            ("ß".as_bytes(), b"SS"),
            (b"a\xff", b"A\xff"),
            (b"a\xed\xa0\x80", b"a"),
        ] {
            assert_ne!(case_key(a), case_key(b), "{a:?} {b:?}");
        }
    }

    #[test]
    fn windows_makes_no_link_to_a_network_or_device_path() {
        let w = Platform::Windows;
        for bad in [
            &br"\\server\share\x"[..],
            b"//server/share",
            br"\/server",
            br"/\server",
            br"\\?\C:\x",
            br"\\.\pipe\x",
            br"\\127.0.0.1\C$\x",
            br"\??\C:\x",
            b"/??/C:/x",
        ] {
            assert_eq!(link_target_refused(bad, w), Some(NETWORK_LINK), "{bad:?}");
            assert_eq!(link_target_refused(bad, Platform::Unix), None, "{bad:?}");
        }
        for good in [
            &br"C:\x"[..],
            b"C:/x",
            br"..\y",
            b"../y",
            b"y",
            br"\x",
            b"/etc/hosts",
            br"\?",
            b"??/x",
        ] {
            assert_eq!(link_target_refused(good, w), None, "{good:?}");
            assert_eq!(link_target_refused(good, Platform::Unix), None, "{good:?}");
        }
    }

    #[test]
    fn a_path_is_refused_by_its_first_bad_component() {
        let p = RelPath::new(br"ok/a:b/c\d").unwrap();
        assert_eq!(refused_path(&p, Platform::Windows), Some(NOT_ALLOWED));
        assert_eq!(refused_path(&p, Platform::Unix), None);
        assert_eq!(
            refused_path(&RelPath::new(b"a/b").unwrap(), Platform::Windows),
            None
        );
    }

    #[test]
    fn a_peer_path_is_relative_and_holds_only_names_of_this_system() {
        for bad in [&b"/x"[..], b"..", b"a/../b", b"a//b"] {
            assert!(peer_path(bad).is_err(), "{bad:?}");
        }
        assert_eq!(peer_path(b"a/b").unwrap().as_bytes(), b"a/b");
        for bad in [&br"..\x"[..], br"..\..\x", br"\x", br"C:\x", br"a\..\..\x"] {
            assert_eq!(
                peer_path(bad).is_err(),
                Platform::CURRENT == Platform::Windows,
                "{bad:?}"
            );
        }
    }
}
