//! What a remote `PATH` means on the daemon: an export name, a path under
//! the daemon's home, or an absolute path, in that order, then the export
//! that contains it.

use crate::authz::{Access, Permissions};
use crate::config::exports::Export;
use crate::fs::paths::RelPath;
use crate::platform::{bytes_to_os, os_to_bytes, Platform};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResolveError {
    /// A `..` component or a NUL byte.
    BadPath,
}

/// The export a resolved path falls in, and where below it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolved<'a> {
    pub name: &'a str,
    pub export: &'a Export,
    /// The remainder below the export's path.
    pub rel: RelPath,
    /// The client's access to the export; `read_only` is not applied here.
    pub access: Access,
}

/// Where a path starts, as [`split_path`] reads it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Start {
    /// A relative path.
    Relative,
    /// A path that starts with a separator: `/srv` on Unix; on Windows
    /// `\srv`, the root of the current drive.
    Root,
    /// Windows only: a drive (`C:`, its letter upper-cased) or a network
    /// share (`\\server\share`), with its root. The extended forms
    /// `\\?\C:\` and `\\?\UNC\server\share\` are the same places and give
    /// the same prefix.
    Prefix(Vec<u8>),
}

/// Where `path` starts and its components on `platform`, with `.` and
/// empty components dropped (`..` is kept, for the caller to judge).
///
/// On Unix only `/` separates, and a path is rooted when it starts with
/// one. On Windows `\` and `/` both separate, and a drive (a letter and
/// `:`, followed by a separator or nothing more) or a network share (two
/// separators, a server and a share) is a component of its own,
/// [`Start::Prefix`]. A drive-relative path (`C:x`) has no drive prefix:
/// its first component is `C:x`. Components are compared exactly (case
/// matters: an NTFS directory can be case-sensitive, so two spellings may
/// be two directories, and matching loosely could reach one that was not
/// exported); only a drive letter is upper-cased, as it never names a
/// directory.
pub(crate) fn split_path(path: &[u8], platform: Platform) -> (Start, Vec<&[u8]>) {
    let keep = |c: &&[u8]| !c.is_empty() && *c != b".";
    match platform {
        Platform::Unix => {
            let start = if path.first() == Some(&b'/') {
                Start::Root
            } else {
                Start::Relative
            };
            (start, path.split(|&b| b == b'/').filter(keep).collect())
        }
        Platform::Windows => {
            let sep = |b: &u8| platform.is_separator(*b);
            let (start, rest) = windows_start(path, &sep);
            (start, rest.split(sep).filter(keep).collect())
        }
    }
}

/// A drive at the start of `path` (a letter and `:`, then a separator or
/// the end): its prefix, upper-cased, and what follows it.
fn drive<'p>(path: &'p [u8], sep: &impl Fn(&u8) -> bool) -> Option<(Vec<u8>, &'p [u8])> {
    match path {
        [l, b':', rest @ ..] if l.is_ascii_alphabetic() && rest.first().is_none_or(sep) => {
            Some((vec![l.to_ascii_uppercase(), b':'], rest))
        }
        _ => None,
    }
}

/// [`split_path`]'s start of a Windows path, and the rest of the path.
fn windows_start<'p>(path: &'p [u8], sep: &impl Fn(&u8) -> bool) -> (Start, &'p [u8]) {
    if let Some((d, rest)) = drive(path, sep) {
        return (Start::Prefix(d), rest);
    }
    match path {
        [a, b, rest @ ..] if sep(a) && sep(b) => {
            let (first, after) = first_and_rest(rest, sep);
            if first != b"?" && first != b"." {
                return unc(rest, sep);
            }
            // `\\?\C:\`, `\\?\UNC\server\share\` or a device (`\\.\name`).
            if let Some((d, rest)) = drive(after, sep) {
                return (Start::Prefix(d), rest);
            }
            let (kind, after_kind) = first_and_rest(after, sep);
            if kind.eq_ignore_ascii_case(b"UNC") {
                return unc(after_kind, sep);
            }
            let mut p = br"\\".to_vec();
            p.extend_from_slice(first);
            p.push(b'\\');
            p.extend_from_slice(kind);
            (Start::Prefix(p), after_kind)
        }
        [a, ..] if sep(a) => (Start::Root, path),
        _ => (Start::Relative, path),
    }
}

/// The first component of `path` and what follows its separator.
fn first_and_rest<'p>(path: &'p [u8], sep: &impl Fn(&u8) -> bool) -> (&'p [u8], &'p [u8]) {
    match path.iter().position(sep) {
        Some(i) => (&path[..i], &path[i + 1..]),
        None => (path, &[]),
    }
}

/// `server`, `share` and the rest of a network path: `\\server\share`.
fn unc<'p>(path: &'p [u8], sep: &impl Fn(&u8) -> bool) -> (Start, &'p [u8]) {
    let (server, after) = first_and_rest(path, sep);
    let (share, rest) = first_and_rest(after, sep);
    let mut p = br"\\".to_vec();
    p.extend_from_slice(server);
    if !share.is_empty() {
        p.push(b'\\');
        p.extend_from_slice(share);
    }
    (Start::Prefix(p), rest)
}

/// The components of `path`, a path as a peer typed it, with `.` and empty
/// ones dropped; `..` and NUL are errors, and on Windows so is a `:` in a
/// component (a drive or a stream there, never a directory's name). The
/// first value says where the path starts.
fn components(path: &[u8], platform: Platform) -> Result<(Start, Vec<&[u8]>), ResolveError> {
    if path.contains(&0) {
        return Err(ResolveError::BadPath);
    }
    let (start, parts) = split_path(path, platform);
    let windows = platform == Platform::Windows;
    if parts
        .iter()
        .any(|c| *c == b".." || (windows && c.contains(&b':')))
    {
        return Err(ResolveError::BadPath);
    }
    Ok((start, parts))
}

/// The directory a path that starts at `start` is below: `/` on Unix; on
/// Windows `\` or the prefix and its root (`C:\`, `\\server\share\`).
/// `None` for a relative path.
fn root_of(start: &Start) -> Option<PathBuf> {
    let sep = Platform::CURRENT.separator();
    let bytes = match start {
        Start::Relative => return None,
        Start::Root => vec![sep],
        Start::Prefix(p) => [&p[..], &[sep]].concat(),
    };
    // Read from a name of this system, or ASCII.
    bytes_to_os(&bytes).ok().map(PathBuf::from)
}

/// `base` with `parts` appended; a part that is not a name of this system
/// is a bad path (never on Unix, where any bytes are a name).
fn join(base: PathBuf, parts: &[&[u8]]) -> Result<PathBuf, ResolveError> {
    let mut p = base;
    for c in parts {
        p.push(bytes_to_os(c).map_err(|_| ResolveError::BadPath)?);
    }
    Ok(p)
}

/// `path` as typed, to an absolute path: an export's directory when the
/// first component names one (that name is returned too), else under
/// `home` when relative (`~` as the first component is `home` too), else
/// as is. `Ok(None)` when the path is relative and there is no home.
pub fn resolve_path<'a>(
    path: &[u8],
    home: Option<&Path>,
    exports: &'a BTreeMap<String, Export>,
) -> Result<Option<(PathBuf, Option<&'a str>)>, ResolveError> {
    resolve_path_where(path, home, exports, |_, _| true)
}

/// [`resolve_path`], where a first component names an export only if
/// `usable` says so; any other name is a directory under `home`.
fn resolve_path_where<'a>(
    path: &[u8],
    home: Option<&Path>,
    exports: &'a BTreeMap<String, Export>,
    usable: impl Fn(&str, &Export) -> bool,
) -> Result<Option<(PathBuf, Option<&'a str>)>, ResolveError> {
    let (start, parts) = components(path, Platform::CURRENT)?;
    if start != Start::Relative {
        let root = root_of(&start).ok_or(ResolveError::BadPath)?;
        return Ok(Some((join(root, &parts)?, None)));
    }
    if let Some(first) = parts.first() {
        if let Some((name, e)) = std::str::from_utf8(first)
            .ok()
            .and_then(|n| exports.get_key_value(n))
            .filter(|(n, e)| usable(n, e))
        {
            return Ok(Some((join(e.path.clone(), &parts[1..])?, Some(name))));
        }
    }
    let Some(home) = home else {
        return Ok(None);
    };
    let rest = match parts.first() {
        Some(&b"~") => &parts[1..],
        _ => &parts[..],
    };
    Ok(Some((join(home.to_path_buf(), rest)?, None)))
}

/// Component-wise, with `.` and empty components dropped (an export path
/// may be written `/srv/photos/`), as [`split_path`] reads it. On Unix
/// that is the path split at each `/`, whether it starts with one or not.
/// On Windows a drive, a share or a leading separator is the first
/// component (`C:`, `\\server\share`, `\`), so paths on two drives never
/// share a prefix.
pub(crate) fn parts_of(p: &Path) -> Vec<Vec<u8>> {
    parts_for(&os_to_bytes(p.as_os_str()), Platform::CURRENT)
}

/// [`parts_of`] the bytes of a path on `platform`.
fn parts_for(path: &[u8], platform: Platform) -> Vec<Vec<u8>> {
    let (start, parts) = split_path(path, platform);
    let first = match (platform, start) {
        (Platform::Windows, Start::Root) => Some(vec![b'\\']),
        (Platform::Windows, Start::Prefix(p)) => Some(p),
        _ => None,
    };
    first
        .into_iter()
        .chain(parts.into_iter().map(<[u8]>::to_vec))
        .collect()
}

/// The export containing `abs` among those the client may use: the longest
/// path wins; between equal paths `prefer` (the export a short name named)
/// wins, then the one with the best effective access (`read_only`
/// applied). Exports the client has no grant for do not take part: a grant
/// on an outer export reaches everything below it, and the reply for a
/// path never depends on where ungranted exports sit. `None` when no
/// export the client may use contains `abs`.
pub fn containing<'a>(
    abs: &Path,
    exports: &'a BTreeMap<String, Export>,
    grants: &Permissions,
    prefer: Option<&str>,
) -> Option<Resolved<'a>> {
    let target = parts_of(abs);
    let rank = |a: Access, e: &Export| match a {
        Access::ReadWrite if !e.read_only => 2,
        _ => 1,
    };
    // (path length, named by the client, effective access), then the export.
    type Candidate<'a> = ((usize, bool, u8), &'a str, &'a Export, Access);
    let mut best: Option<Candidate<'a>> = None;
    for (name, export) in exports {
        let Some(access) = grants.access_to(name, export) else {
            continue;
        };
        let prefix = parts_of(&export.path);
        if prefix.len() > target.len() || prefix != target[..prefix.len()] {
            continue;
        }
        let key = (
            prefix.len(),
            prefer == Some(name.as_str()),
            rank(access, export),
        );
        if best.is_none_or(|(k, ..)| key > k) {
            best = Some((key, name, export, access));
        }
    }
    let ((len, ..), name, export, access) = best?;
    let rel = RelPath::new(&target[len..].join(&b'/')).expect("components are valid names");
    Some(Resolved {
        name,
        export,
        rel,
        access,
    })
}

/// Both steps for the daemon: `Err(BadPath)`, or `Ok(None)` when the path
/// cannot be resolved or lies outside every export the client may use.
///
/// A first component stands for an export only when the client has a grant
/// for that export. The name of any other export is read as a directory
/// under the home, as if the export were not there, also when the client
/// may use that export's directory through another export: what a path
/// means to a client never depends on exports it has no grant for.
pub fn resolve<'a>(
    path: &[u8],
    home: Option<&Path>,
    exports: &'a BTreeMap<String, Export>,
    grants: &Permissions,
) -> Result<Option<Resolved<'a>>, ResolveError> {
    let usable = |name: &str, e: &Export| grants.access_to(name, e).is_some();
    Ok(resolve_path_where(path, home, exports, usable)?
        .and_then(|(abs, named)| containing(&abs, exports, grants, named)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parts(p: &str, platform: Platform) -> Vec<String> {
        parts_for(p.as_bytes(), platform)
            .into_iter()
            .map(|c| String::from_utf8(c).unwrap())
            .collect()
    }

    #[test]
    fn unix_splits_at_each_slash_and_nothing_else() {
        let u = Platform::Unix;
        assert_eq!(parts("/srv/photos", u), ["srv", "photos"]);
        assert_eq!(parts("/srv//./photos/", u), ["srv", "photos"]);
        assert_eq!(parts("srv/photos", u), ["srv", "photos"]);
        assert_eq!(parts("/a/../b", u), ["a", "..", "b"]);
        assert_eq!(parts(r"C:\srv\photos", u), [r"C:\srv\photos"]);
        assert_eq!(parts("c:/x", u), ["c:", "x"]);
        assert_eq!(parts("/", u), Vec::<String>::new());
        assert_eq!(split_path(b"/x", u), (Start::Root, vec![&b"x"[..]]));
        assert_eq!(split_path(b"x", u).0, Start::Relative);
        assert_eq!(split_path(br"\\s\x", u).0, Start::Relative);
    }

    #[test]
    fn windows_splits_at_both_separators_with_the_drive_or_share_first() {
        let w = Platform::Windows;
        assert_eq!(parts(r"C:\srv\photos", w), ["C:", "srv", "photos"]);
        assert_eq!(parts(r"C:\srv\photos\T", w), ["C:", "srv", "photos", "T"]);
        assert_eq!(parts("c:/srv//./photos/", w), ["C:", "srv", "photos"]);
        assert_eq!(parts(r"C:\srv/photos", w), ["C:", "srv", "photos"]);
        assert_eq!(parts(r"C:\", w), ["C:"]);
        assert_eq!(parts("C:", w), ["C:"]);
        // The extended forms are the same places.
        assert_eq!(parts(r"\\?\C:\srv\photos", w), ["C:", "srv", "photos"]);
        assert_eq!(parts(r"\\server\share\x", w), [r"\\server\share", "x"]);
        assert_eq!(parts(r"//server/share/x", w), [r"\\server\share", "x"]);
        assert_eq!(
            parts(r"\\?\UNC\server\share\x", w),
            [r"\\server\share", "x"]
        );
        assert_eq!(parts(r"\\.\pipe\x", w), [r"\\.\pipe", "x"]);
        // Rooted on the current drive: not the same as any drive.
        assert_eq!(parts(r"\srv\photos", w), [r"\", "srv", "photos"]);
        assert_eq!(parts("/srv/photos", w), [r"\", "srv", "photos"]);
        // Drive-relative: no drive prefix.
        assert_eq!(parts(r"C:x\y", w), ["C:x", "y"]);
        // Case is kept, except the drive letter's.
        assert_eq!(parts(r"d:\Srv\photos", w), ["D:", "Srv", "photos"]);
        assert_ne!(parts(r"C:\srv", w), parts(r"C:\SRV", w));
        assert_eq!(parts("rel/a", w), ["rel", "a"]);
    }

    #[test]
    fn a_windows_request_path_is_absolute_by_its_drive_and_refuses_a_colon_in_a_name() {
        let w = Platform::Windows;
        let (start, parts) = components(br"C:\Users\d\T", w).unwrap();
        assert_eq!(start, Start::Prefix(b"C:".to_vec()));
        assert_eq!(parts, [&b"Users"[..], b"d", b"T"]);
        assert_eq!(components(b"m/T", w).unwrap().0, Start::Relative);
        assert_eq!(components(b"/srv", w).unwrap().0, Start::Root);
        for bad in [&b"C:x"[..], b"m/D:/x", b"m/f:stream", br"m\..\x"] {
            assert_eq!(components(bad, w), Err(ResolveError::BadPath), "{bad:?}");
        }
        // On Unix a colon is a name's.
        assert_eq!(
            components(b"m/D:/x", Platform::Unix).unwrap().1,
            [&b"m"[..], b"D:", b"x"]
        );
    }

    #[test]
    fn an_export_below_a_windows_root_contains_the_paths_below_it() {
        let w = Platform::Windows;
        let export = parts(r"C:\Users\d\srv\m", w);
        let target = parts(r"C:\Users\d\srv\m\T", w);
        assert_eq!(&target[..export.len()], &export[..]);
        let other_drive = parts(r"D:\Users\d\srv\m\T", w);
        assert_ne!(&other_drive[..export.len()], &export[..]);
    }

    /// The root a request path is joined below, on this system.
    #[test]
    fn a_request_path_joins_below_its_root() {
        let ex = BTreeMap::new();
        let abs = |p: &[u8]| resolve_path(p, None, &ex).unwrap().unwrap().0;
        if Platform::CURRENT == Platform::Windows {
            assert_eq!(abs(br"C:\a\b").as_os_str(), std::ffi::OsStr::new(r"C:\a\b"));
            assert_eq!(abs(b"c:/a/b").as_os_str(), std::ffi::OsStr::new(r"C:\a\b"));
            assert_eq!(abs(b"/a/b").as_os_str(), std::ffi::OsStr::new(r"\a\b"));
            assert_eq!(
                abs(br"\\srv\share\a").as_os_str(),
                std::ffi::OsStr::new(r"\\srv\share\a")
            );
        } else {
            assert_eq!(abs(b"/a//b/").as_os_str(), std::ffi::OsStr::new("/a/b"));
        }
    }
}
