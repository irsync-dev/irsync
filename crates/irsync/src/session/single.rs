//! Placement rules for a single file as the source of a copy, following
//! rsync: where the file lands depends on what the destination is.

use std::ffi::OsStr;
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use anyhow::Context;
use iroh::endpoint::Connection;
use irsync_proto::chunk::CHUNK_PARAMS;
use irsync_proto::v1::{error::Code, hello, path_info, Error, Hello, PathInfo};
use irsync_proto::PROTOCOL_VERSION;
use prost::Message;

use super::engine::{read_frame, SessionRefused, TransportError};
use crate::build::BuildInfo;
use crate::fs::paths::RelPath;
use crate::net::frame::write_msg;

/// A one-file transfer. Each name is exactly one path component. `wire` is
/// the receiver's name for the file, `local` is this side's name; they are
/// equal on the receiver.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SingleFile {
    pub wire: RelPath,
    pub local: RelPath,
}

impl SingleFile {
    /// This side's name for the listing path `rel`: the wire name maps to
    /// the local one, any other path is itself.
    pub fn local_of<'a>(&'a self, rel: &'a RelPath) -> &'a RelPath {
        if *rel == self.wire {
            &self.local
        } else {
            rel
        }
    }
}

/// Where a single file goes: the directory to use as the session root, and
/// the file's name inside it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FilePlace {
    pub root: PathBuf,
    pub name: Vec<u8>,
}

#[derive(Debug, thiserror::Error)]
pub enum PlaceError {
    #[error("{}: not a directory", .0.display())]
    NotADirectory(PathBuf),
    #[error("{}: is a directory", .0.display())]
    IsADirectory(PathBuf),
    #[error("{}: {}", .0.display(), .1)]
    Io(PathBuf, io::Error),
}

/// True for a non-empty name with no `/` and no NUL that is not `.` or `..`.
pub fn is_one_component(name: &[u8]) -> bool {
    !name.is_empty() && name != b"." && name != b".." && !name.contains(&b'/') && !name.contains(&0)
}

/// The parent (`.` for a bare name) and the last component of `path`.
pub fn split_last(path: &Path) -> (PathBuf, Vec<u8>) {
    match path.file_name() {
        Some(name) => {
            let parent = match path.parent() {
                Some(p) if !p.as_os_str().is_empty() => p.to_path_buf(),
                _ => PathBuf::from("."),
            };
            (parent, name.as_bytes().to_vec())
        }
        None => (PathBuf::from("."), path.as_os_str().as_bytes().to_vec()),
    }
}

/// Decide where a single file named `src_name` lands for destination `dest`.
/// `follow` resolves a symlink at `dest` (the client does, the daemon does
/// not). A missing root directory is not checked here; callers create it.
pub fn place_file(
    dest: &Path,
    contents_only: bool,
    src_name: &[u8],
    follow: bool,
) -> Result<FilePlace, PlaceError> {
    let meta = if follow {
        std::fs::metadata(dest)
    } else {
        std::fs::symlink_metadata(dest)
    };
    let in_dir = match meta {
        Ok(m) if m.is_dir() => true,
        Ok(_) => {
            if contents_only {
                return Err(PlaceError::NotADirectory(dest.to_path_buf()));
            }
            false
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => contents_only,
        Err(e) => return Err(PlaceError::Io(dest.to_path_buf(), e)),
    };
    if !in_dir {
        let (root, name) = split_last(dest);
        return Ok(FilePlace { root, name });
    }
    let target = dest.join(OsStr::from_bytes(src_name));
    match std::fs::symlink_metadata(&target) {
        Ok(m) if m.is_dir() => return Err(PlaceError::IsADirectory(target)),
        Ok(_) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(PlaceError::Io(target, e)),
    }
    Ok(FilePlace {
        root: dest.to_path_buf(),
        name: src_name.to_vec(),
    })
}

/// Whether `path` names a single file (not a directory). A missing path is
/// not single, so today's error path stays. With `contents_only` (trailing
/// slash) a non-directory is an error.
pub fn source_is_single(path: &Path, contents_only: bool) -> Result<bool, PlaceError> {
    match std::fs::metadata(path) {
        Ok(m) if m.is_dir() => Ok(false),
        Ok(_) if contents_only => Err(PlaceError::NotADirectory(path.to_path_buf())),
        Ok(_) => Ok(true),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(PlaceError::Io(path.to_path_buf(), e)),
    }
}

/// Asks the daemon what `path` (as typed, without a trailing slash) is,
/// before the client picks a local root for a pull. A refusal comes back
/// as [`SessionRefused`], with the codes a pull of `path` would get.
pub async fn probe(conn: &Connection, path: &[u8]) -> anyhow::Result<path_info::Kind> {
    let hello = Hello {
        protocol_version: PROTOCOL_VERSION,
        direction: hello::Direction::Probe as i32,
        chunk_min: CHUNK_PARAMS.min,
        chunk_avg: CHUNK_PARAMS.avg,
        chunk_max: CHUNK_PARAMS.max,
        build: Some(BuildInfo::current().to_wire()),
        path: path.to_vec(),
        ..Hello::default()
    };
    let (mut send, mut recv) = conn.open_bi().await.context("open control stream")?;
    write_msg(&mut send, &hello)
        .await
        .map_err(|e| TransportError(format!("sending Hello: {e}")))?;
    send.finish()?;
    let body = read_frame(&mut recv)
        .await
        .map_err(|e| TransportError(format!("reading PathInfo: {e}")))?;
    // Both replies decode as either message (field 1 is a varint in each).
    // A PathInfo never holds field 2; the daemon's Error always has a
    // message there.
    if let Ok(e) = Error::decode(&body[..]) {
        if !e.message.is_empty() {
            return Err(SessionRefused {
                code: Code::try_from(e.code).unwrap_or(Code::Unknown),
                message: e.message,
            }
            .into());
        }
    }
    let info = PathInfo::decode(&body[..]).context("malformed reply to Hello")?;
    path_info::Kind::try_from(info.kind).context("unknown kind of path in the reply")
}
