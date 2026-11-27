//! The check that keeps a request from reaching another export's directory
//! through an outer export, whatever the spelling (#295).
//!
//! The daemon chooses the export for a request by comparing path
//! components byte for byte ([`crate::resolve::containing`]). On a volume
//! that ignores case (the macOS default, a casefolded directory on Linux)
//! another spelling of an inner export's path matches only the outer
//! export, and the file system then opens the inner export's directory.
//! The check here catches that by identity: it walks the request from the
//! chosen export's root, one directory at a time, and compares each
//! directory's (device, inode) with the roots of the other exports this
//! peer has a grant for. Names stay exact; identity catches every other
//! spelling that reaches the same directory.

use std::io;
use std::path::{Path, PathBuf};

use crate::fs::paths::RelPath;

/// Another export a request may reach, as [`reached_export`] compares it:
/// the identity (device, inode) of the deepest directory of its configured
/// path that exists (its own root when that exists), and the names of the
/// path below that directory (none when the root exists).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OtherRoot {
    pub name: String,
    pub ancestor: (u64, u64),
    pub rest: Vec<Vec<u8>>,
    /// Whether `rest` is compared ignoring case: unless `ancestor` is
    /// known to keep case apart ([`crate::platform::Dir::ignores_case`]),
    /// a name made there in another case may be the export's root.
    pub fold: bool,
}

impl OtherRoot {
    /// [`OtherRoot`] of the export `name` at `path`, looked at now: `path`
    /// itself, opened as the daemon opens an export root to serve it
    /// (`Dir::open_root_nofollow`), then each shorter prefix of it, until
    /// one opens. The shorter prefixes are directories on the way to a path
    /// the administrator configured, so they are opened following links
    /// (`Dir::open_root`): a symlink among the parents is the
    /// administrator's choice, and it is the real directory behind it, with
    /// its identity, that a walk could meet.
    ///
    /// A prefix that does not exist moves on to the next shorter one. A
    /// shorter prefix that is not a directory (a file) means the export
    /// cannot exist below it: `None`, as when not even the path's root
    /// exists. The export's own root being a file, or a link the serving
    /// open refuses, is `None` too: neither a walk nor a session passes
    /// through a file or a link, so there is nothing to keep a request
    /// away from, and refusing would let whoever put it there lock out
    /// every request. Any other failure (permission denied, an I/O error)
    /// is an error, logged here with the export's name and path, and the
    /// caller refuses the request.
    fn find(name: &str, path: &Path) -> io::Result<Option<OtherRoot>> {
        use crate::platform::{cause, os_to_bytes, Cause, Dir};
        for (i, ancestor) in path.ancestors().enumerate() {
            if ancestor.as_os_str().is_empty() {
                break;
            }
            let opened = if i == 0 {
                Dir::open_root_nofollow(ancestor)
            } else {
                Dir::open_root(ancestor)
            };
            match opened.and_then(|d| d.stat_self().map(|st| (d, st))) {
                Ok((d, st)) => {
                    let rest: Vec<Vec<u8>> = path
                        .strip_prefix(ancestor)
                        .map_err(io::Error::other)?
                        .components()
                        .map(|c| os_to_bytes(c.as_os_str()))
                        .collect();
                    let fold = !rest.is_empty() && d.ignores_case() != Some(false);
                    return Ok(Some(OtherRoot {
                        name: name.to_string(),
                        ancestor: (st.dev, st.ino),
                        rest,
                        fold,
                    }));
                }
                Err(e) if cause(&e) == Cause::NotThere => {}
                Err(e) if i > 0 && cause(&e) == Cause::NotADirectory => return Ok(None),
                Err(e)
                    if i == 0 && matches!(cause(&e), Cause::NotADirectory | Cause::LinkRefused) =>
                {
                    return Ok(None)
                }
                Err(e) => {
                    tracing::warn!(
                        export = %name,
                        path = %path.display(),
                        "checking for an inner export: opening {}: {e}",
                        ancestor.display()
                    );
                    return Err(e);
                }
            }
        }
        Ok(None)
    }
}

/// The other export a request reaches, if one does: the decision of
/// [`inner_export`], on what it found. `walk` holds the identities of the
/// directories on the way from the chosen export's root down the request,
/// in order: `walk[0]` the root itself, `walk[i]` the directory
/// `comps[i - 1]` names; it ends at the first component that is not
/// there. The request reaches `other` when the walk meets its `ancestor`
/// at some position `i` and the components after it, `comps[i..]`, begin
/// with its `rest`, compared ignoring case ([`same_ignoring_case`])
/// unless the directory is known to keep case apart (`fold`): a
/// directory that exists is judged by identity, a name not made yet by its
/// spelling. A request with fewer components left than `rest` is an
/// ancestor of that export's root, not inside it. At the root itself
/// (`i == 0`) an empty `rest` means the other export has this very
/// directory as its root (two exports of one directory, which the resolver
/// chooses between by access): that is not one export inside the other.
/// Only with `twin_exempt`: a walk from a volume's root (an automatic
/// export, see [`auto_export_reaches`]) has no such exemption.
pub(crate) fn reached_export<'a>(
    walk: &[(u64, u64)],
    comps: &[&[u8]],
    others: &'a [OtherRoot],
    twin_exempt: bool,
) -> Option<&'a str> {
    for (i, id) in walk.iter().enumerate() {
        let left = comps.get(i..).unwrap_or_default();
        for o in others {
            let inside = o.ancestor == *id
                && !(twin_exempt && i == 0 && o.rest.is_empty())
                && o.rest.len() <= left.len()
                && o.rest.iter().zip(left).all(|(r, c)| {
                    if o.fold {
                        same_ignoring_case(r, c)
                    } else {
                        r == c
                    }
                });
            if inside {
                return Some(&o.name);
            }
        }
    }
    None
}

/// What [`reached_export`] compares for each of `others` (name,
/// configured path).
fn other_roots(others: &[(String, PathBuf)]) -> io::Result<Vec<OtherRoot>> {
    let mut roots = Vec::new();
    for (name, path) in others {
        roots.extend(OtherRoot::find(name, path)?);
    }
    Ok(roots)
}

/// The name of another export of `others` (name, configured path) that
/// the request `rel` in the export at `export_root` reaches, if one does,
/// whatever the spelling. For each other export, [`OtherRoot::find`]
/// looks up the deepest directory of its path that exists, now; then the
/// walk goes from `export_root` (opened as `prepare_root` opens it) down
/// `rel`, one component at a time from the directory above without
/// following a link, as a session opens it, and notes each directory's
/// identity; [`reached_export`] decides. Nothing is kept from one request
/// to the next.
///
/// The walk ends at the first component that is not there yet (a push
/// makes it), is not a directory, or is a link (the session goes through
/// none of these). A missing export root ends it too: the session reports
/// that itself. Any other error is returned: the caller refuses the
/// request, so a check that cannot finish never lets one through. Cost,
/// per request: for each other export one open and one stat, and one more
/// of each for each missing name at the end of its path; then one open and
/// one stat for the root and for each component of `rel` that exists.
pub(crate) fn inner_export(
    export_root: &Path,
    rel: &RelPath,
    others: &[(String, PathBuf)],
) -> io::Result<Option<String>> {
    use crate::platform::{cause, Cause, Dir};
    // With no component only the root is met, and it is no other
    // export's inside (see `reached_export`).
    if rel.as_bytes().is_empty() || others.is_empty() {
        return Ok(None);
    }
    let roots = other_roots(others)?;
    if roots.is_empty() {
        return Ok(None);
    }
    let comps: Vec<&[u8]> = rel.components().collect();
    let ends = |e: &io::Error| {
        matches!(
            cause(e),
            Cause::NotThere | Cause::NotADirectory | Cause::LinkRefused
        )
    };
    let mut dir = match Dir::open_root_nofollow(export_root) {
        Ok(d) => d,
        Err(e) if cause(&e) == Cause::NotThere => return Ok(None),
        Err(e) => return Err(e),
    };
    let st = dir.stat_self()?;
    let mut walk = vec![(st.dev, st.ino)];
    for c in &comps {
        dir = match dir.open_dir(c) {
            Ok(d) => d,
            Err(e) if ends(&e) => break,
            Err(e) => return Err(e),
        };
        let st = dir.stat_self()?;
        walk.push((st.dev, st.ino));
    }
    Ok(reached_export(&walk, &comps, &roots, true).map(str::to_string))
}

/// The deepest part of `base` that exists, resolved to its real place
/// (`canonicalize`, as the automatic route resolves its root: links the
/// administrator placed followed), and the rest of `base` below it, the
/// names not made yet, as typed. A part that is not there, or runs through
/// a file, moves on to the next shorter one; any other error is returned
/// (the caller refuses). `None`: nothing of `base` exists.
pub(crate) fn real_place(base: &Path) -> io::Result<Option<(PathBuf, PathBuf)>> {
    use crate::platform::{cause, Cause};
    for ancestor in base.ancestors() {
        if ancestor.as_os_str().is_empty() {
            break;
        }
        match std::fs::canonicalize(ancestor) {
            Ok(real) => {
                let missing = base.strip_prefix(ancestor).map_err(io::Error::other)?;
                return Ok(Some((real, missing.to_path_buf())));
            }
            Err(e) if matches!(cause(&e), Cause::NotThere | Cause::NotADirectory) => {}
            Err(e) => {
                tracing::warn!(
                    path = %base.display(),
                    "checking for an inner export: resolving {}: {e}",
                    ancestor.display()
                );
                return Err(e);
            }
        }
    }
    Ok(None)
}

/// For an automatic export (one made for, or matched by, a request by
/// absolute path): the name of another export of `others` (name,
/// configured path) at, above, or on the way to `base` joined with `rel`,
/// whatever the spelling. An automatic export is placed where the request
/// asks, so a granted export there must be met even when the request
/// spells its path otherwise; the walk therefore starts at the file
/// system's root, not at the automatic export's.
///
/// First the deepest part of `base` that exists is resolved to its real
/// place ([`real_place`]). Then the walk goes from `/` through `Dir`
/// down the real names, the names of `base` not made yet, and `rel`,
/// noting each directory's identity ([`walk_resolved`]). Then
/// [`reached_export`] decides, without the exemption for an export of the
/// walk's first directory. Nothing is made here, and it runs before the
/// automatic route makes a directory or adds an export.
pub(crate) fn auto_export_reaches(
    base: &Path,
    rel: &RelPath,
    others: &[(String, PathBuf)],
) -> io::Result<Option<String>> {
    use crate::platform::{os_to_bytes, Dir};
    use std::path::Component;
    if others.is_empty() {
        return Ok(None);
    }
    let Some((real, missing)) = real_place(base)? else {
        return Ok(None);
    };
    let roots = other_roots(others)?;
    if roots.is_empty() {
        return Ok(None);
    }
    let mut volume = PathBuf::new();
    let mut names: Vec<Vec<u8>> = Vec::new();
    for c in real.components() {
        match c {
            Component::Prefix(_) | Component::RootDir => volume.push(c.as_os_str()),
            Component::Normal(n) => names.push(os_to_bytes(n)),
            Component::CurDir | Component::ParentDir => {
                return Err(io::Error::other("a resolved path holds . or .."))
            }
        }
    }
    let existing = names.len();
    for c in missing.components() {
        if let Component::Normal(n) = c {
            names.push(os_to_bytes(n));
        }
    }
    names.extend(rel.components().map(<[u8]>::to_vec));
    let comps: Vec<&[u8]> = names.iter().map(Vec::as_slice).collect();
    let dir = Dir::open_root_nofollow(&volume)?;
    let st = dir.stat_self()?;
    let mut walker = RealWalker {
        dir,
        volume: &volume,
        names: &comps,
    };
    let walk = walk_resolved((st.dev, st.ino), comps.len(), existing, &mut walker)?;
    Ok(reached_export(&walk, &comps, &roots, false).map(str::to_string))
}

/// What one step of [`walk_resolved`] found at the next name.
#[derive(Debug)]
pub(crate) enum Step {
    /// A directory, with its identity: the walk goes on in it.
    Opened((u64, u64)),
    /// The name is not there (not made yet).
    NotThere,
    /// Not a directory (a file): nothing below it can exist.
    NotADirectory,
    /// A link the no-follow open refuses.
    Link,
    /// Any other error.
    Failed(io::Error),
}

/// The two moves of [`walk_resolved`]: open name `i` below the directory
/// the walk is in, without following a link; or open the path up to and
/// including name `i` by path, following, and go on there.
pub(crate) trait Walker {
    fn child(&mut self, i: usize) -> Step;
    fn follow(&mut self, i: usize) -> io::Result<(u64, u64)>;
}

/// The identities met walking `n` names down from a directory with
/// identity `first`, of which the first `existing` are a resolved path's
/// existing part (`canonicalize`: no symlink is left in it).
///
/// - A directory: its identity is noted and the walk goes on.
/// - A link among the resolved existing names can only be something
///   swapped in since: that prefix is opened by path, following
///   ([`Walker::follow`]), its identity noted, and the walk goes on below
///   it. If that open fails, the walk fails.
/// - The walk ends, legitimately, at a name that is not there (not made
///   yet, or removed since: nothing below it exists either), at a file
///   (nothing below it can exist), and at a link past the resolved part
///   (a session does not pass one).
/// - Any other error fails the walk; the caller refuses the request.
pub(crate) fn walk_resolved(
    first: (u64, u64),
    n: usize,
    existing: usize,
    w: &mut dyn Walker,
) -> io::Result<Vec<(u64, u64)>> {
    let mut walk = vec![first];
    for i in 0..n {
        match w.child(i) {
            Step::Opened(id) => walk.push(id),
            Step::NotThere | Step::NotADirectory => break,
            Step::Link if i < existing => walk.push(w.follow(i)?),
            Step::Link => break,
            Step::Failed(e) => return Err(e),
        }
    }
    Ok(walk)
}

/// [`Walker`] over the real file system: `Dir` below `volume`'s root.
struct RealWalker<'a> {
    dir: crate::platform::Dir,
    volume: &'a Path,
    names: &'a [&'a [u8]],
}

impl Walker for RealWalker<'_> {
    fn child(&mut self, i: usize) -> Step {
        use crate::platform::{cause, Cause};
        let opened = self
            .dir
            .open_dir(self.names[i])
            .and_then(|d| d.stat_self().map(|st| (d, (st.dev, st.ino))));
        match opened {
            Ok((d, id)) => {
                self.dir = d;
                Step::Opened(id)
            }
            Err(e) => match cause(&e) {
                Cause::NotThere => Step::NotThere,
                Cause::NotADirectory => Step::NotADirectory,
                Cause::LinkRefused => Step::Link,
                _ => Step::Failed(e),
            },
        }
    }

    fn follow(&mut self, i: usize) -> io::Result<(u64, u64)> {
        use crate::platform::{bytes_to_os, Dir};
        let mut path = self.volume.to_path_buf();
        for n in &self.names[..=i] {
            path.push(bytes_to_os(n)?);
        }
        let opened = Dir::open_root(&path).and_then(|d| d.stat_self().map(|st| (d, st)));
        match opened {
            Ok((d, st)) => {
                self.dir = d;
                Ok((st.dev, st.ino))
            }
            Err(e) => {
                tracing::warn!(
                    "checking for an inner export: opening {}: {e}",
                    path.display()
                );
                Err(e)
            }
        }
    }
}

/// Whether two names (stored bytes) differ only in case, at least as
/// loosely as a case-insensitive file system folds them: two characters are
/// the same when they are equal, or their uppercase is, or their lowercase
/// is, each taken only where it is a single character (`ß`, whose
/// uppercase is `SS`, stays as it is). A name that is WTF-8 but not UTF-8
/// (it holds an unpaired surrogate) is compared per UTF-16 unit, each unit
/// that is a character by itself as above, a surrogate exactly. Bytes that
/// are not WTF-8 are compared exactly.
fn same_ignoring_case(a: &[u8], b: &[u8]) -> bool {
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

/// Two characters are the same ignoring case: equal, or equal in their
/// single-character uppercase, or in their single-character lowercase.
fn same_char(x: char, y: char) -> bool {
    let upper = |c: char| single(c.to_uppercase(), c);
    let lower = |c: char| single(c.to_lowercase(), c);
    x == y || upper(x) == upper(y) || lower(x) == lower(y)
}

#[cfg(test)]
mod tests {
    use super::{reached_export, OtherRoot};

    /// A [`super::Walker`] that plays back `steps` for the names in order,
    /// and answers `follow` with `followed` (counting the calls).
    struct Script {
        steps: Vec<Option<super::Step>>,
        followed: Vec<std::io::Result<(u64, u64)>>,
        follows: Vec<usize>,
    }

    impl super::Walker for Script {
        fn child(&mut self, i: usize) -> super::Step {
            self.steps[i].take().expect("a name asked for twice")
        }
        fn follow(&mut self, i: usize) -> std::io::Result<(u64, u64)> {
            self.follows.push(i);
            self.followed.remove(0)
        }
    }

    /// The walk over a resolved path: a link among its existing names is
    /// opened following, and the walk goes on below it; where it cannot be
    /// opened, the walk fails (the request is refused). Which ends are
    /// legitimate.
    #[test]
    fn a_walk_goes_on_through_a_link_in_the_resolved_part_and_fails_closed() {
        use super::{walk_resolved, Step};
        const VOL: (u64, u64) = (1, 1);
        const MNT: (u64, u64) = (2, 5);
        const E: (u64, u64) = (2, 7);
        let run =
            |steps: Vec<Step>, existing: usize, followed: Vec<std::io::Result<(u64, u64)>>| {
                let n = steps.len();
                let mut w = Script {
                    steps: steps.into_iter().map(Some).collect(),
                    followed,
                    follows: Vec::new(),
                };
                let r = walk_resolved(VOL, n, existing, &mut w);
                (r, w.follows)
            };
        // `/mnt/e/x`: `mnt` is opened following; the walk goes on and
        // meets E's root.
        let (r, follows) = run(
            vec![Step::Link, Step::Opened(E), Step::NotThere],
            2,
            vec![Ok(MNT)],
        );
        let walk = r.unwrap();
        assert_eq!(walk, [VOL, MNT, E]);
        assert_eq!(follows, [0]);
        let others = [OtherRoot {
            name: "e".into(),
            ancestor: E,
            rest: vec![],
            fold: false,
        }];
        let comps: Vec<&[u8]> = vec![b"mnt", b"E", b"x"];
        assert_eq!(reached_export(&walk, &comps, &others, false), Some("e"));
        // E's root missing: met by its parent and the name.
        let others = [OtherRoot {
            name: "e".into(),
            ancestor: MNT,
            rest: vec![b"ro".to_vec()],
            fold: true,
        }];
        let (r, _) = run(vec![Step::Link, Step::NotThere], 1, vec![Ok(MNT)]);
        let walk = r.unwrap();
        let comps: Vec<&[u8]> = vec![b"mnt", b"RO"];
        assert_eq!(reached_export(&walk, &comps, &others, false), Some("e"));
        // The followed link cannot be opened: the walk fails.
        let (r, follows) = run(
            vec![Step::Link, Step::Opened(E)],
            2,
            vec![Err(std::io::Error::other("denied"))],
        );
        assert_eq!(r.unwrap_err().to_string(), "denied");
        assert_eq!(follows, [0]);
        // Any other error fails it.
        let (r, _) = run(
            vec![Step::Opened(MNT), Step::Failed(std::io::Error::other("io"))],
            2,
            vec![],
        );
        assert_eq!(r.unwrap_err().to_string(), "io");
        // Legitimate ends: a name not there, a file, a link past the
        // resolved part (not followed).
        for (steps, existing) in [
            (vec![Step::Opened(MNT), Step::NotThere, Step::Opened(E)], 3),
            (
                vec![Step::Opened(MNT), Step::NotADirectory, Step::Opened(E)],
                3,
            ),
            (vec![Step::Opened(MNT), Step::Link, Step::Opened(E)], 1),
        ] {
            let (r, follows) = run(steps, existing, vec![]);
            assert_eq!(r.unwrap(), [VOL, MNT]);
            assert!(follows.is_empty());
        }
    }

    const ROOT: (u64, u64) = (1, 1);
    const PARENT: (u64, u64) = (1, 2);
    const DEEPER: (u64, u64) = (1, 3);

    fn other(name: &str, ancestor: (u64, u64), rest: &[&str]) -> OtherRoot {
        OtherRoot {
            name: name.to_string(),
            ancestor,
            rest: rest.iter().map(|r| r.as_bytes().to_vec()).collect(),
            fold: true,
        }
    }

    /// [`other`] below a directory known to keep case apart.
    fn exact(name: &str, ancestor: (u64, u64), rest: &[&str]) -> OtherRoot {
        OtherRoot {
            fold: false,
            ..other(name, ancestor, rest)
        }
    }

    /// The decision on what the walk met: a directory that exists counts by
    /// identity, a name not made yet by its spelling ignoring case.
    #[test]
    fn a_request_reaches_another_export_by_identity_then_by_name() {
        // `walk` as the walk would note it, `comps` the request.
        type Case<'a> = (&'a [(u64, u64)], &'a [&'a str], OtherRoot, Option<&'a str>);
        let cases: Vec<Case> = vec![
            // The other export's root exists (`rest` empty): met at 1.
            (&[ROOT, PARENT], &["p"], other("e", PARENT, &[]), Some("e")),
            (&[ROOT, PARENT], &["P"], other("e", PARENT, &[]), Some("e")),
            (
                &[ROOT, PARENT, DEEPER],
                &["p", "x"],
                other("e", PARENT, &[]),
                Some("e"),
            ),
            // Not met: another directory, or not there (the walk ended).
            (&[ROOT, DEEPER], &["q"], other("e", PARENT, &[]), None),
            (&[ROOT], &["p"], other("e", PARENT, &[]), None),
            // The same inode on another device is another directory.
            (&[ROOT, (2, 2)], &["p"], other("e", PARENT, &[]), None),
            // The chosen root is the other export's root too: two exports
            // of one directory, not one inside the other.
            (&[ROOT], &[], other("twin", ROOT, &[]), None),
            (&[ROOT, PARENT], &["p"], other("twin", ROOT, &[]), None),
            // The other root is missing right below the chosen root.
            (
                &[ROOT],
                &["RO", "x"],
                other("ro", ROOT, &["ro"]),
                Some("ro"),
            ),
            (&[ROOT], &["Ro"], other("ro", ROOT, &["ro"]), Some("ro")),
            (&[ROOT], &["ro"], other("ro", ROOT, &["ro"]), Some("ro")),
            (&[ROOT], &["rw", "x"], other("ro", ROOT, &["ro"]), None),
            (&[ROOT], &["ro2"], other("ro", ROOT, &["ro"]), None),
            // Missing below an existing parent, which is reached in any
            // spelling: judged by identity, then names.
            (
                &[ROOT, PARENT],
                &["PHOTOS", "RO", "x"],
                other("ro", PARENT, &["ro"]),
                Some("ro"),
            ),
            (
                &[ROOT, PARENT],
                &["PHOTOS", "other"],
                other("ro", PARENT, &["ro"]),
                None,
            ),
            // Two names missing: fewer left is an ancestor of the root.
            (
                &[ROOT, PARENT],
                &["p", "B"],
                other("ro", PARENT, &["b", "ro"]),
                None,
            ),
            (
                &[ROOT, PARENT],
                &["p"],
                other("ro", PARENT, &["b", "ro"]),
                None,
            ),
            (
                &[ROOT, PARENT],
                &["p", "B", "RO"],
                other("ro", PARENT, &["b", "ro"]),
                Some("ro"),
            ),
            (
                &[ROOT, PARENT],
                &["p", "b", "Ro", "x"],
                other("ro", PARENT, &["b", "ro"]),
                Some("ro"),
            ),
            (
                &[ROOT, PARENT],
                &["p", "c", "ro"],
                other("ro", PARENT, &["b", "ro"]),
                None,
            ),
            // Case folded also where the full uppercase is several
            // characters.
            (&[ROOT], &["ᾈ"], other("g", ROOT, &["ᾀ"]), Some("g")),
            // The configured spelling differs in case from the request.
            (&[ROOT], &["ro"], other("ro", ROOT, &["RO"]), Some("ro")),
            // Below a directory that keeps case apart, another case is
            // another name: only the exact one is the export's root.
            (
                &[ROOT],
                &["ro", "x"],
                exact("ro", ROOT, &["ro"]),
                Some("ro"),
            ),
            (&[ROOT], &["RO", "x"], exact("ro", ROOT, &["ro"]), None),
            (
                &[ROOT, PARENT],
                &["p", "b", "Ro"],
                exact("ro", PARENT, &["b", "ro"]),
                None,
            ),
            // An existing root is met by identity, whatever the case.
            (&[ROOT, PARENT], &["P"], exact("e", PARENT, &[]), Some("e")),
        ];
        for (walk, comps, o, want) in cases {
            let c: Vec<&[u8]> = comps.iter().map(|c| c.as_bytes()).collect();
            let others = [o];
            assert_eq!(
                reached_export(walk, &c, &others, true),
                want,
                "walk {walk:?} comps {comps:?} other {:?}",
                others[0]
            );
        }
        // Several others: the first the walk meets is named.
        let others = [other("deep", DEEPER, &[]), other("near", ROOT, &["p"])];
        let c: Vec<&[u8]> = vec![b"P", b"x"];
        assert_eq!(
            reached_export(&[ROOT, PARENT, DEEPER], &c, &others, true),
            Some("near")
        );
        // Bytes that are not UTF-8 are compared exactly.
        let mut b = other("b", ROOT, &[]);
        b.rest = vec![b"r\xff".to_vec()];
        assert_eq!(
            reached_export(&[ROOT], &[b"r\xff"], std::slice::from_ref(&b), true),
            Some("b")
        );
        b.rest = vec![b"R\xff".to_vec()];
        assert_eq!(
            reached_export(&[ROOT], &[b"r\xff"], std::slice::from_ref(&b), true),
            None
        );
    }
}
