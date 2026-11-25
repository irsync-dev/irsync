//! What one source's session is: a directory, or a single file placed by
//! rsync's rules.

use std::path::{Path, PathBuf};

use anyhow::bail;
use irsync_proto::v1::{path_info, SyncRequest};

use crate::fs::paths::escape_path;
use crate::session::single::{
    is_one_component, place_file, source_is_single, split_last, PlaceError,
};
use crate::session::SyncHello;

/// One source's session: the local root, and the `Hello` to send.
#[derive(Clone)]
pub(super) struct Plan {
    pub root: PathBuf,
    pub local_contents_only: bool,
    pub hello: SyncHello,
    /// The local name of the one file a single-file session copies.
    pub single: Option<Vec<u8>>,
}

/// A push of `path`: a file is pushed on its own from the directory that
/// holds it (a trailing slash on it is an error); anything else as `dir`.
/// Several sources always go into the destination as a directory.
pub(super) fn push_plan(
    dir: Plan,
    path: &Path,
    contents_only: bool,
    several: bool,
) -> anyhow::Result<Plan> {
    // The caller's message already starts with `path`.
    let single = source_is_single(path, contents_only).map_err(|e| match e {
        PlaceError::Io(_, e) => anyhow::Error::new(e),
        PlaceError::NotADirectory(_) => anyhow::anyhow!("not a directory"),
        e => e.into(),
    })?;
    if !single {
        return Ok(dir);
    }
    let (root, name) = split_last(path);
    let mut hello = dir.hello;
    hello.req = SyncRequest {
        single_file: true,
        name: name.clone(),
        contents_only: hello.req.contents_only || several,
        ..hello.req
    };
    Ok(Plan {
        root,
        local_contents_only: false,
        hello,
        single: Some(name),
    })
}

/// A pull of `rpath`, which the daemon says is `kind`, into `local`: a file
/// is placed by rsync's rules and pulled on its own; anything else as `dir`.
pub(super) fn pull_plan(
    dir: Plan,
    kind: path_info::Kind,
    local: &Path,
    contents_only: bool,
    rpath: &[u8],
) -> anyhow::Result<Plan> {
    if kind != path_info::Kind::File {
        return Ok(dir);
    }
    // The path as typed here: `/` separates, and on Windows `\` too.
    let basename = rpath
        .rsplit(|&b| crate::platform::Platform::CURRENT.is_separator(b))
        .next()
        .unwrap_or_default();
    if !is_one_component(basename) || basename == b"~" {
        bail!("cannot tell the file's name from {}", escape_path(rpath));
    }
    let placed = place_file(local, contents_only, basename)?;
    let mut hello = dir.hello;
    hello.req = SyncRequest {
        single_file: true,
        name: placed.name.clone(),
        contents_only: false,
        ..hello.req
    };
    Ok(Plan {
        root: placed.root,
        local_contents_only: false,
        hello,
        single: Some(placed.name),
    })
}
