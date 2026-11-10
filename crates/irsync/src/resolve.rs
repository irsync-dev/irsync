//! What a remote `PATH` means on the daemon: an export name, a path under
//! the daemon's home, or an absolute path, in that order, then the export
//! that contains it.

use crate::authz::{Access, Grants};
use crate::config::exports::Export;
use crate::fs::paths::RelPath;
use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;
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
    /// The peer's access to the export; `read_only` is not applied here.
    pub access: Access,
}

/// The components of `path` with `.` and empty ones dropped; `..` and NUL
/// are errors. The first value says whether the path was absolute.
fn components(path: &[u8]) -> Result<(bool, Vec<&[u8]>), ResolveError> {
    if path.contains(&0) {
        return Err(ResolveError::BadPath);
    }
    let absolute = path.first() == Some(&b'/');
    let mut out = Vec::new();
    for c in path.split(|&b| b == b'/') {
        match c {
            b"" | b"." => {}
            b".." => return Err(ResolveError::BadPath),
            c => out.push(c),
        }
    }
    Ok((absolute, out))
}

fn join(base: PathBuf, parts: &[&[u8]]) -> PathBuf {
    let mut p = base;
    for c in parts {
        p.push(OsStr::from_bytes(c));
    }
    p
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
    let (absolute, parts) = components(path)?;
    if absolute {
        return Ok(Some((join(PathBuf::from("/"), &parts), None)));
    }
    if let Some(first) = parts.first() {
        if let Some((name, e)) = std::str::from_utf8(first)
            .ok()
            .and_then(|n| exports.get_key_value(n))
        {
            return Ok(Some((join(e.path.clone(), &parts[1..]), Some(name))));
        }
    }
    let Some(home) = home else {
        return Ok(None);
    };
    let rest = match parts.first() {
        Some(&b"~") => &parts[1..],
        _ => &parts[..],
    };
    Ok(Some((join(home.to_path_buf(), rest), None)))
}

/// Component-wise, with `.` and empty components dropped (an export path
/// may be written `/srv/photos/`).
fn parts_of(p: &Path) -> Vec<&[u8]> {
    p.as_os_str()
        .as_bytes()
        .split(|&b| b == b'/')
        .filter(|c| !c.is_empty() && *c != b".")
        .collect()
}

/// The export containing `abs` among those the peer may use: the longest
/// path wins; between equal paths `prefer` (the export a short name named)
/// wins, then the one with the best effective access (`read_only`
/// applied). Exports the peer has no grant for do not take part: a grant
/// on an outer export reaches everything below it, and the reply for a
/// path never depends on where ungranted exports sit. `None` when no
/// export the peer may use contains `abs`.
pub fn containing<'a>(
    abs: &Path,
    exports: &'a BTreeMap<String, Export>,
    grants: &Grants,
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
/// cannot be resolved or lies outside every export the peer may use.
pub fn resolve<'a>(
    path: &[u8],
    home: Option<&Path>,
    exports: &'a BTreeMap<String, Export>,
    grants: &Grants,
) -> Result<Option<Resolved<'a>>, ResolveError> {
    Ok(resolve_path(path, home, exports)?
        .and_then(|(abs, named)| containing(&abs, exports, grants, named)))
}
