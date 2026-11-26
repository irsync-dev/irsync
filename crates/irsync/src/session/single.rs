//! Placement rules for a single file as the source of a copy, following
//! rsync: where the file lands depends on what the destination is.

use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::Context;
use iroh::endpoint::Connection;
use irsync_proto::v1::{hello, hello_reply, path_info, Probe};

use irsync_proto::v1::{error::Code, SyncRequest};

use super::engine::{prepare_root, read_hello_reply, Missing, TransportError};
use super::SyncHello;
use crate::fs::paths::RelPath;
use crate::net::frame::write_msg_tapped;
use crate::net::tap::{ConnTap, StreamKind as TapStream};
use crate::options::Direction;
use crate::platform::{bytes_to_os, os_to_bytes};

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
    pub(crate) fn local_of<'a>(&'a self, rel: &'a RelPath) -> &'a RelPath {
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
    /// The daemon's destination is a symlink to a directory: it neither
    /// replaces the link nor writes through it.
    #[error("{} is a symlink to a directory", .0.display())]
    SymlinkToDirectory(PathBuf),
    /// The daemon's destination is a symlink that leads out of the export,
    /// or through another symlink: the daemon does not look where it
    /// leads, so it cannot tell a file from a directory there.
    #[error("{} is a symlink the daemon does not follow", .0.display())]
    SymlinkNotFollowed(PathBuf),
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
            (parent, os_to_bytes(name))
        }
        None => (PathBuf::from("."), os_to_bytes(path.as_os_str())),
    }
}

/// What is at a single file's destination, or at the file's name inside
/// it, as far as placing the file needs to know.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum At {
    Missing,
    Dir,
    /// A file, or anything else that is not a directory (a symlink that
    /// is not looked through counts here, as a file to replace).
    Other,
    /// A symlink to a directory (the daemon refuses it).
    LinkToDir,
    /// A symlink the daemon does not follow (see [`link_leads_to`]).
    LinkNotFollowed,
}

/// How a side looks at what is where a single file may land.
trait Lookup {
    /// What is at the destination itself.
    fn dest(&self) -> io::Result<At>;
    /// What is at `name` inside the destination, once that is a directory.
    fn child(&self, name: &[u8]) -> io::Result<At>;
}

/// rsync's rule for a single file named `src_name` and the destination
/// `dest`, with `look` telling what is there: into the destination when
/// it is a directory (or missing, with a trailing slash), else as the
/// destination itself. Nothing is written through a link.
fn place(
    look: &impl Lookup,
    dest: &Path,
    contents_only: bool,
    src_name: &[u8],
) -> Result<FilePlace, PlaceError> {
    let in_dir = match look.dest() {
        Ok(At::Dir) => true,
        Ok(At::LinkToDir) => return Err(PlaceError::SymlinkToDirectory(dest.to_path_buf())),
        Ok(At::LinkNotFollowed) => return Err(PlaceError::SymlinkNotFollowed(dest.to_path_buf())),
        Ok(At::Other) if contents_only => {
            return Err(PlaceError::NotADirectory(dest.to_path_buf()))
        }
        Ok(At::Other) => false,
        Ok(At::Missing) => contents_only,
        Err(e) => return Err(PlaceError::Io(dest.to_path_buf(), e)),
    };
    if !in_dir {
        let (root, name) = split_last(dest);
        return Ok(FilePlace { root, name });
    }
    let name = bytes_to_os(src_name).map_err(|e| PlaceError::Io(dest.to_path_buf(), e))?;
    let target = dest.join(name);
    match look.child(src_name) {
        Ok(At::Dir) => return Err(PlaceError::IsADirectory(target)),
        Ok(_) => {}
        Err(e) => return Err(PlaceError::Io(target, e)),
    }
    Ok(FilePlace {
        root: dest.to_path_buf(),
        name: src_name.to_vec(),
    })
}

/// The kind of what `meta` describes, a missing entry as [`At::Missing`].
fn at_of(meta: io::Result<std::fs::Metadata>) -> io::Result<At> {
    match meta {
        Ok(m) if m.is_dir() => Ok(At::Dir),
        Ok(_) => Ok(At::Other),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(At::Missing),
        Err(e) => Err(e),
    }
}

/// The client looks with the plain file system and follows a symlink at
/// the destination (a dangling one is a missing destination).
struct Followed<'a>(&'a Path);

impl Lookup for Followed<'_> {
    fn dest(&self) -> io::Result<At> {
        at_of(std::fs::metadata(self.0))
    }

    fn child(&self, name: &[u8]) -> io::Result<At> {
        at_of(std::fs::symlink_metadata(self.0.join(bytes_to_os(name)?)))
    }
}

/// Decide where a single file named `src_name` lands for destination
/// `dest` on the client, which follows a symlink at `dest`. A missing
/// root directory is not checked here; callers create it.
pub fn place_file(
    dest: &Path,
    contents_only: bool,
    src_name: &[u8],
) -> Result<FilePlace, PlaceError> {
    place(&Followed(dest), dest, contents_only, src_name)
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
/// as [`SessionRefused`], with the codes a pull of `path` would get. The
/// wait for the reply ends after `timeout` (`--idle-timeout`; none or zero:
/// no limit), as for a session's Hello.
pub async fn probe(
    conn: &Connection,
    path: &[u8],
    timeout: Option<Duration>,
) -> anyhow::Result<path_info::Kind> {
    let hello = super::hello(hello::Request::Probe(Probe {
        path: path.to_vec(),
    }));
    let (mut send, mut recv) = conn.open_bi().await.context("open control stream")?;
    let tap = ConnTap::of(conn).stream(TapStream::Control, u64::from(send.id()));
    write_msg_tapped(&mut send, &hello, &tap)
        .await
        .map_err(|e| TransportError(format!("sending Hello: {e}")))?;
    send.finish()?;
    tap.finished();
    match read_hello_reply(&mut recv, &tap, timeout).await? {
        hello_reply::Reply::PathInfo(info) => {
            path_info::Kind::try_from(info.kind).context("unknown kind of path in the reply")
        }
        _ => anyhow::bail!("the daemon's reply does not fit a probe"),
    }
}

// The daemon's side of a single-file session.

/// A single-file session at `rel` in the export at `export_root`: the
/// directory that holds the file (export-relative) and the file's names.
/// A push places the file by rsync's rules ([`place_file_beneath`]); a pull sends
/// the file `rel` names, under the receiver's name `hello.req.name`.
/// Nothing is looked at through a symlink: the directories above the file
/// are checked as `prepare_root` checks a session root, first.
pub(crate) fn place_single(
    export_root: &Path,
    rel: &RelPath,
    hello: &SyncHello,
) -> Result<(RelPath, SingleFile), (Code, String)> {
    if hello.direction == Direction::DaemonSends {
        return source_single(export_root, rel, &hello.req);
    }
    let hello = &hello.req;
    // A missing directory is created later, by the session.
    prepare_root(export_root, &rel.parent(), Missing::Allow).map_err(bad_session_path)?;
    let dest = if rel.as_bytes().is_empty() {
        export_root.to_path_buf()
    } else {
        export_root.join(rel.to_os())
    };
    let placed = place_file_beneath(export_root, rel, &dest, hello.contents_only, &hello.name)
        .map_err(|e| match e {
            PlaceError::SymlinkToDirectory(_) | PlaceError::SymlinkNotFollowed(_) => {
                (Code::BadPath, format!("bad session path: {e}"))
            }
            e => (Code::BadPath, e.to_string()),
        })?;
    let root = placed
        .root
        .strip_prefix(export_root)
        .ok()
        .and_then(|r| RelPath::new(&crate::platform::os_to_bytes(r.as_os_str())).ok());
    let name = RelPath::new(&placed.name)
        .ok()
        .filter(|_| is_one_component(&placed.name));
    match (root, name) {
        (Some(root), Some(name)) => Ok((
            root,
            SingleFile {
                wire: name.clone(),
                local: name,
            },
        )),
        _ => Err((Code::BadPath, "invalid path".into())),
    }
}

/// The daemon looks from the export's directory with no symlink followed
/// on the way, so a directory above the destination that was swapped for a
/// link says nothing about what the link leads to. A link at the
/// destination itself is worked out only inside the export
/// ([`link_leads_to`]).
struct Beneath<'a> {
    export_root: &'a Path,
    rel: &'a RelPath,
}

impl Lookup for Beneath<'_> {
    fn dest(&self) -> io::Result<At> {
        use crate::platform::Kind as FileKind;
        if self.rel.as_bytes().is_empty() {
            // The export's own directory.
            return Ok(At::Dir);
        }
        match crate::fs::safe::Beneath::new(self.export_root, self.rel.clone()).lstat() {
            Ok(st) if st.kind == FileKind::Dir => Ok(At::Dir),
            // Only decides; nothing is written through the link.
            Ok(st) if st.kind == FileKind::Symlink => {
                Ok(match link_leads_to(self.export_root, self.rel) {
                    LinkTo::Directory => At::LinkToDir,
                    LinkTo::NotFollowed => At::LinkNotFollowed,
                    LinkTo::Other => At::Other,
                })
            }
            Ok(_) => Ok(At::Other),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(At::Missing),
            Err(e) => Err(e),
        }
    }

    fn child(&self, name: &[u8]) -> io::Result<At> {
        use crate::platform::Kind as FileKind;
        let rel = self
            .rel
            .join(name)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
        match crate::fs::safe::Beneath::new(self.export_root, rel).lstat() {
            Ok(st) if st.kind == FileKind::Dir => Ok(At::Dir),
            Ok(_) => Ok(At::Other),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(At::Missing),
            Err(e) => Err(e),
        }
    }
}

/// [`place_file`] for the daemon: where a single file named `src_name`
/// lands for the destination `rel` in the export at `export_root` (`dest`
/// is the same place as a path, for the result and for messages), looked
/// at as [`Beneath`] says.
fn place_file_beneath(
    export_root: &Path,
    rel: &RelPath,
    dest: &Path,
    contents_only: bool,
    src_name: &[u8],
) -> Result<FilePlace, PlaceError> {
    place(&Beneath { export_root, rel }, dest, contents_only, src_name)
}

/// What the symlink at a single file's destination leads to, as far as the
/// daemon looks.
#[derive(Debug, PartialEq, Eq)]
enum LinkTo {
    /// A directory in the export.
    Directory,
    /// A file in the export, or a name in it that is not there: the link
    /// is a file to replace.
    Other,
    /// Out of the export, or through another symlink. What is there is
    /// not looked at, so the answer is the same whatever it is.
    NotFollowed,
}

/// Where the symlink at `rel` in the export at `export_root` leads. Its
/// target is read as text and worked out inside the export: an absolute
/// target, or one that climbs out with `..`, is not followed, and neither
/// is one whose way passes another symlink. A client can put a link at the
/// destination itself, so looking at what an outside link leads to would
/// tell it what is at a path it may not see.
fn link_leads_to(export_root: &Path, rel: &RelPath) -> LinkTo {
    use crate::fs::safe::{open_parent_beneath, Beneath};
    use crate::platform::Kind as FileKind;
    let Ok((dir, name)) = open_parent_beneath(export_root, rel) else {
        return LinkTo::NotFollowed;
    };
    let Ok(target) = dir.read_link(&name) else {
        return LinkTo::NotFollowed;
    };
    let target = &target[..];
    if target.is_empty() || target[0] == b'/' || target.contains(&0) {
        return LinkTo::NotFollowed;
    }
    let parent = rel.parent();
    let mut parts: Vec<&[u8]> = parent.components().collect();
    for c in target.split(|&b| b == b'/') {
        match c {
            b"" | b"." => {}
            b".." => {
                if parts.pop().is_none() {
                    return LinkTo::NotFollowed;
                }
            }
            c => parts.push(c),
        }
    }
    if parts.is_empty() {
        // The export's own directory.
        return LinkTo::Directory;
    }
    let Ok(to) = RelPath::new(&parts.join(&b'/')) else {
        return LinkTo::NotFollowed;
    };
    match Beneath::new(export_root, to).lstat() {
        Ok(st) if st.kind == FileKind::Dir => LinkTo::Directory,
        Ok(st) if st.kind == FileKind::Symlink => LinkTo::NotFollowed,
        Ok(_) => LinkTo::Other,
        Err(e) if e.kind() == io::ErrorKind::NotFound => LinkTo::Other,
        Err(_) => LinkTo::NotFollowed,
    }
}

/// The pull half of [`place_single`]: `rel` must name an existing entry
/// that is not a directory. Its directory becomes the session root.
fn source_single(
    export_root: &Path,
    rel: &RelPath,
    hello: &SyncRequest,
) -> Result<(RelPath, SingleFile), (Code, String)> {
    let parent = rel.parent();
    let dir = prepare_root(export_root, &parent, Missing::Refuse).map_err(bad_session_path)?;
    let invalid = || (Code::BadPath, "invalid name".to_string());
    let Some(name) = rel.components().last() else {
        return Err((Code::BadPath, PlaceError::IsADirectory(dir).to_string()));
    };
    let file = dir.join(crate::platform::bytes_to_os(name).map_err(|_| invalid())?);
    // Looked at from the export's directory with no symlink followed: the
    // directories above may have changed since they were checked.
    match crate::fs::safe::Beneath::new(export_root, rel.clone()).lstat() {
        Ok(st) if st.kind == crate::platform::Kind::Dir => {
            return Err((Code::BadPath, PlaceError::IsADirectory(file).to_string()))
        }
        Ok(_) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            let e = io::Error::new(e.kind(), format!("{} does not exist", file.display()));
            return Err(bad_session_path(e));
        }
        Err(e) => return Err((Code::BadPath, PlaceError::Io(file, e).to_string())),
    }
    let wire = RelPath::new(&hello.name).map_err(|_| invalid())?;
    let local = RelPath::new(name).map_err(|_| invalid())?;
    Ok((parent, SingleFile { wire, local }))
}

/// What `rel` is in the export at `export_root`, for a probe. Like a
/// pull, it never looks through a symlink: a parent that is one, or is
/// missing, makes the path missing. The last component is not followed.
pub(crate) fn probe_kind(export_root: &Path, rel: &RelPath) -> path_info::Kind {
    use path_info::Kind;
    if prepare_root(export_root, &rel.parent(), Missing::Refuse).is_err() {
        return Kind::Missing;
    }
    if rel.as_bytes().is_empty() {
        return Kind::Directory;
    }
    // From the export's directory, with no symlink followed on the way.
    match crate::fs::safe::Beneath::new(export_root, rel.clone()).lstat() {
        Ok(st) if st.kind == crate::platform::Kind::Dir => Kind::Directory,
        Ok(_) => Kind::File,
        Err(_) => Kind::Missing,
    }
}

/// The reply `serve_daemon` gives for a session root it cannot use.
fn bad_session_path(e: io::Error) -> (Code, String) {
    (Code::BadPath, format!("bad session path: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_probe_looks_at_nothing_through_a_symlink() {
        use path_info::Kind;
        let export = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::create_dir(export.path().join("d")).unwrap();
        std::fs::write(export.path().join("d/f"), b"x").unwrap();
        std::fs::write(outside.path().join("f"), b"x").unwrap();
        crate::platform::testutil::symlink(outside.path(), &export.path().join("link")).unwrap();
        let kind = |p: &str| probe_kind(export.path(), &RelPath::new(p.as_bytes()).unwrap());
        assert_eq!(kind(""), Kind::Directory);
        assert_eq!(kind("d"), Kind::Directory);
        assert_eq!(kind("d/f"), Kind::File);
        assert_eq!(kind("d/none"), Kind::Missing);
        // The link itself is an entry; nothing behind it is.
        assert_eq!(kind("link"), Kind::File);
        assert_eq!(kind("link/f"), Kind::Missing);
    }

    /// Where a pushed single file lands is decided from the export's
    /// directory, with no symlink followed on the way to the destination.
    #[test]
    fn a_single_file_is_placed_without_looking_through_a_symlink() {
        let export = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::create_dir(export.path().join("d")).unwrap();
        std::fs::write(export.path().join("d/old"), b"x").unwrap();
        std::fs::create_dir(outside.path().join("sub")).unwrap();
        std::fs::write(outside.path().join("old"), b"x").unwrap();
        crate::platform::testutil::symlink(outside.path(), &export.path().join("link")).unwrap();
        let place = |p: &str, contents_only: bool| {
            let rel = RelPath::new(p.as_bytes()).unwrap();
            let dest = export.path().join(p);
            place_file_beneath(export.path(), &rel, &dest, contents_only, b"f.iso")
        };
        // Into a directory, onto a file, onto a name that is not there.
        let p = place("d", false).unwrap();
        assert_eq!(
            (p.root, p.name),
            (export.path().join("d"), b"f.iso".to_vec())
        );
        for name in ["d/old", "d/new"] {
            let p = place(name, false).unwrap();
            assert_eq!(p.root, export.path().join("d"));
            assert_eq!(p.name, name.as_bytes()[2..].to_vec());
        }
        assert!(matches!(
            place("d/old", true),
            Err(PlaceError::NotADirectory(_))
        ));
        // Behind the link, a directory, a file and a missing name all get
        // the same answer: none is looked at.
        for name in ["link/sub", "link/old", "link/new"] {
            for contents_only in [false, true] {
                match place(name, contents_only) {
                    Err(PlaceError::Io(_, e)) => {
                        assert_eq!(e.kind(), io::ErrorKind::InvalidInput, "{name}: {e}")
                    }
                    other => panic!("{name}: {other:?}"),
                }
            }
        }
    }

    /// Where a link at a single file's destination leads is worked out
    /// inside the export only.
    #[test]
    fn a_link_at_the_destination_is_followed_only_inside_the_export() {
        let export = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(export.path().join("d/sub")).unwrap();
        std::fs::write(export.path().join("d/file"), b"x").unwrap();
        std::fs::create_dir(outside.path().join("dir")).unwrap();
        let link = |name: &str, to: &std::path::Path| {
            crate::platform::testutil::symlink(to, &export.path().join(name)).unwrap();
            link_leads_to(export.path(), &RelPath::new(name.as_bytes()).unwrap())
        };
        let p = std::path::Path::new;
        assert_eq!(link("a", p("d/sub")), LinkTo::Directory);
        assert_eq!(link("d/b", p("sub")), LinkTo::Directory);
        assert_eq!(link("d/c", p("../d/./sub/")), LinkTo::Directory);
        assert_eq!(link("d/e", p("..")), LinkTo::Directory);
        assert_eq!(link("f", p("d/file")), LinkTo::Other);
        assert_eq!(link("g", p("d/none")), LinkTo::Other);
        // Out of the export: absolute, or climbing out, whatever is there.
        assert_eq!(link("h", &outside.path().join("dir")), LinkTo::NotFollowed);
        assert_eq!(link("i", &outside.path().join("none")), LinkTo::NotFollowed);
        assert_eq!(link("j", p("../x")), LinkTo::NotFollowed);
        assert_eq!(link("d/k", p("../../x")), LinkTo::NotFollowed);
        // An absolute link back into the export is not worked out either.
        assert_eq!(link("l", &export.path().join("d")), LinkTo::NotFollowed);
        // Through, or onto, another link.
        assert_eq!(link("m", p("a")), LinkTo::NotFollowed);
        assert_eq!(link("n", p("a/x")), LinkTo::NotFollowed);
    }

    /// On the daemon a symlink at `T` to a file, or a dangling one, is a
    /// file to replace: nothing is written through it.
    #[test]
    fn symlink_dest_is_a_file_on_daemon() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("real"), b"x").unwrap();
        for (t, to) in [("T", "real"), ("D", "nowhere")] {
            crate::platform::testutil::symlink(std::path::Path::new(to), &d.path().join(t))
                .unwrap();
            let rel = RelPath::new(t.as_bytes()).unwrap();
            let dest = d.path().join(t);
            let p = place_file_beneath(d.path(), &rel, &dest, false, b"f.iso").unwrap();
            assert_eq!((p.root, p.name), split_last(&dest));
        }
    }

    /// On the daemon a symlink at `T` to a directory is refused, with or
    /// without a trailing slash: neither replacing the link nor writing into
    /// its target is what the user meant.
    #[test]
    fn symlink_to_dir_dest_is_refused_on_daemon() {
        let d = tempfile::tempdir().unwrap();
        std::fs::create_dir(d.path().join("real")).unwrap();
        let t = d.path().join("T");
        crate::platform::testutil::symlink(std::path::Path::new("real"), &t).unwrap();
        let rel = RelPath::new(b"T").unwrap();
        for contents_only in [false, true] {
            match place_file_beneath(d.path(), &rel, &t, contents_only, b"f.iso") {
                Err(e @ PlaceError::SymlinkToDirectory(_)) => assert_eq!(
                    e.to_string(),
                    format!("{} is a symlink to a directory", t.display())
                ),
                other => panic!("unexpected {other:?}"),
            }
        }
    }
}
