//! Sync sessions: the session key, the persisted record, the local side of a
//! session, and its outcome. The protocol roles live in [`engine`].

mod control;
pub mod engine;
mod planner;
mod receiver;
mod sender_role;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use iroh::EndpointId;
use irsync_proto::v1::{Hello, SessionPlan, SessionProgress};
use prost::Message;

use crate::cli::location::Location;
use crate::cli::{DeleteMode, Direction, FilterRule, SyncOptions};
use crate::filter::FilterSet;
use crate::fs::paths::RelPath;
use crate::hasher::Hasher;
use crate::index::{IndexHandle, SessionRow};

/// Files hashed concurrently by a session's hasher.
pub const HASH_WORKERS: usize = 4;

/// Identity of a session: BLAKE3 over a canonical byte string of everything
/// that changes what the session does (daemon, both locations, and the
/// semantics-affecting flags). Equal tuples give equal keys.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionKey(Vec<u8>);

/// Appends one length-prefixed field to the canonical encoding.
fn field(h: &mut blake3::Hasher, tag: &[u8], v: &[u8]) {
    h.update(&(tag.len() as u64).to_le_bytes());
    h.update(tag);
    h.update(&(v.len() as u64).to_le_bytes());
    h.update(v);
}

fn location_bytes(l: &Location) -> Vec<u8> {
    use std::os::unix::ffi::OsStrExt;
    let mut v = Vec::new();
    match l {
        Location::Local {
            path,
            contents_only,
        } => {
            let canon = std::fs::canonicalize(path).unwrap_or_else(|_| path.clone());
            v.extend_from_slice(b"local\0");
            v.push(*contents_only as u8);
            v.extend_from_slice(canon.as_os_str().as_bytes());
        }
        // The host string is an alias; the daemon id passed alongside identifies it.
        Location::Remote {
            module,
            path,
            contents_only,
            ..
        } => {
            v.extend_from_slice(b"remote\0");
            v.push(*contents_only as u8);
            v.extend_from_slice(&(module.len() as u64).to_le_bytes());
            v.extend_from_slice(module.as_bytes());
            v.extend_from_slice(path.as_bytes());
        }
    }
    v
}

impl SessionKey {
    pub fn new(
        daemon: EndpointId,
        src: &Location,
        dst: &Location,
        opts: &SyncOptions,
    ) -> SessionKey {
        let mut h = blake3::Hasher::new();
        field(&mut h, b"irsync-session-key", b"v1");
        field(&mut h, b"daemon", daemon.as_bytes());
        field(&mut h, b"src", &location_bytes(src));
        field(&mut h, b"dst", &location_bytes(dst));
        let p = &opts.preserve;
        let bits = [
            p.recursive,
            p.links,
            p.perms,
            p.times,
            p.group,
            p.owner,
            p.devices,
            p.hard_links,
            p.sparse,
        ]
        .iter()
        .enumerate()
        .fold(0u16, |acc, (i, &b)| acc | ((b as u16) << i));
        field(&mut h, b"preserve", &bits.to_le_bytes());
        field(&mut h, b"checksum", &[opts.checksum as u8]);
        field(&mut h, b"update", &[opts.update_only as u8]);
        let delete = match opts.delete {
            DeleteMode::Off => 0u8,
            DeleteMode::Before => 1,
            DeleteMode::During => 2,
            DeleteMode::After => 3,
        };
        field(&mut h, b"delete", &[delete, opts.delete_excluded as u8]);
        for r in &opts.filters {
            use std::os::unix::ffi::OsStrExt;
            match r {
                FilterRule::Include(s) => field(&mut h, b"include", s.as_bytes()),
                FilterRule::Exclude(s) => field(&mut h, b"exclude", s.as_bytes()),
                FilterRule::IncludeFrom(p) => {
                    field(&mut h, b"include-from", p.as_os_str().as_bytes())
                }
                FilterRule::ExcludeFrom(p) => {
                    field(&mut h, b"exclude-from", p.as_os_str().as_bytes())
                }
            }
        }
        SessionKey(h.finalize().as_bytes().to_vec())
    }

    /// The daemon's key for a session: the client's identity plus what it asked for.
    pub fn for_daemon(client: EndpointId, hello: &Hello) -> SessionKey {
        let mut h = blake3::Hasher::new();
        field(&mut h, b"irsync-daemon-session-key", b"v1");
        field(&mut h, b"client", client.as_bytes());
        field(&mut h, b"module", hello.module.as_bytes());
        field(&mut h, b"path", &hello.path);
        field(
            &mut h,
            b"flags",
            &[hello.direction as u8, hello.contents_only as u8],
        );
        SessionKey(h.finalize().as_bytes().to_vec())
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }

    pub fn into_bytes(self) -> Vec<u8> {
        self.0
    }
}

/// A session as persisted in the index: plan and progress are protobuf
/// messages (`proto/irsync/v1/session.proto`) stored as opaque blobs.
#[derive(Debug, Clone, PartialEq)]
pub struct SessionRecord {
    pub id: [u8; 16],
    pub key: Vec<u8>,
    pub plan: SessionPlan,
    pub progress: SessionProgress,
    pub created_at: i64,
    pub updated_at: i64,
}

impl SessionRecord {
    pub fn to_row(&self) -> SessionRow {
        SessionRow {
            id: self.id,
            key: self.key.clone(),
            plan: self.plan.encode_to_vec(),
            progress: self.progress.encode_to_vec(),
            created_at: self.created_at,
            updated_at: self.updated_at,
        }
    }

    pub fn from_row(row: &SessionRow) -> Result<SessionRecord, prost::DecodeError> {
        Ok(SessionRecord {
            id: row.id,
            key: row.key.clone(),
            plan: SessionPlan::decode(&row.plan[..])?,
            progress: SessionProgress::decode(&row.progress[..])?,
            created_at: row.created_at,
            updated_at: row.updated_at,
        })
    }
}

/// The index a session uses, and where the session root sits below the root
/// the index was opened for. Index rows are always relative to that index
/// root (the module root on the daemon), so `key` maps session-relative paths
/// to index keys and `strip` maps index keys back.
#[derive(Clone)]
pub struct IndexScope {
    pub handle: IndexHandle,
    pub prefix: RelPath,
}

impl IndexScope {
    pub fn new(handle: IndexHandle, prefix: RelPath) -> IndexScope {
        IndexScope { handle, prefix }
    }

    /// The index key for a session-relative path.
    pub fn key(&self, rel: &RelPath) -> RelPath {
        join_rel(&self.prefix, rel)
    }

    /// The session-relative path of an index key, if it lies inside the session root.
    pub fn strip(&self, key: &RelPath) -> Option<RelPath> {
        let (k, p) = (key.as_bytes(), self.prefix.as_bytes());
        if p.is_empty() {
            return Some(key.clone());
        }
        match k.strip_prefix(p) {
            Some([]) => Some(RelPath::root()),
            Some([b'/', rest @ ..]) => RelPath::new(rest).ok(),
            _ => None,
        }
    }
}

/// `a/b` for two canonical relative paths (either may be the root).
fn join_rel(a: &RelPath, b: &RelPath) -> RelPath {
    match (a.as_bytes(), b.as_bytes()) {
        ([], _) => b.clone(),
        (_, []) => a.clone(),
        (x, y) => {
            let mut v = x.to_vec();
            v.push(b'/');
            v.extend_from_slice(y);
            RelPath::new(&v).expect("joining canonical paths stays canonical")
        }
    }
}

/// Called by the sender role after the first chunk of a path went out, before
/// the rest; lets a test change the file mid-transfer.
#[cfg(feature = "test-hooks")]
pub type FirstChunkHook =
    Arc<dyn Fn(&RelPath) -> futures::future::BoxFuture<'static, ()> + Send + Sync>;

/// Test-only hooks; empty in normal builds.
#[derive(Clone, Default)]
pub struct SideHooks {
    #[cfg(feature = "test-hooks")]
    pub on_first_chunk_sent: Option<FirstChunkHook>,
}

impl std::fmt::Debug for SideHooks {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SideHooks")
    }
}

/// This process's end of a session: the directory it reads or writes, with
/// its index and hasher. `filters` only shape the walk when this side sends;
/// the client's filters are not transmitted to the daemon in v1.
#[derive(Clone)]
pub struct LocalSide {
    pub root: PathBuf,
    /// rsync's trailing-slash rule as typed (part of the session key; the
    /// root itself is already resolved, see [`resolve_roots`]).
    pub contents_only: bool,
    pub index: IndexHandle,
    pub hasher: Arc<Hasher>,
    pub filters: Arc<FilterSet>,
    pub hooks: SideHooks,
}

impl LocalSide {
    pub fn new(
        root: PathBuf,
        contents_only: bool,
        index: IndexHandle,
        filters: Arc<FilterSet>,
    ) -> LocalSide {
        let hasher = Arc::new(Hasher::new(index.clone(), root.clone(), HASH_WORKERS));
        LocalSide {
            root,
            contents_only,
            index,
            hasher,
            filters,
            hooks: SideHooks::default(),
        }
    }
}

/// What a session did, from one side's point of view.
#[derive(Debug, Clone, Default)]
pub struct Outcome {
    pub session_id: [u8; 16],
    /// Files placed (receiver) or acknowledged as placed (sender).
    pub files_transferred: u64,
    /// File data bytes that crossed the network on data streams.
    pub bytes_transferred: u64,
    /// Regular files found up to date (known on the receiving side only).
    pub files_skipped: u64,
    /// Files this side's hashers had to read during the session.
    pub files_hashed: u64,
    pub errors: Vec<(RelPath, String)>,
    /// Per-file records the role still held when it finished (should be 0).
    #[cfg(feature = "test-hooks")]
    pub retained_files: usize,
}

impl Outcome {
    /// rsync's convention: 0 ok, 23 partial transfer due to error.
    pub fn exit_code(&self) -> i32 {
        if self.errors.is_empty() {
            0
        } else {
            23
        }
    }
}

/// Applies rsync's trailing-slash rule to the source: `src` (no slash) copies
/// the directory itself, so its name is appended on the destination side.
/// Returns the local root and the remote path to put in `Hello`.
pub fn resolve_roots(
    direction: Direction,
    local: &Path,
    local_contents_only: bool,
    remote: &RelPath,
    remote_contents_only: bool,
) -> (PathBuf, RelPath) {
    use std::os::unix::ffi::OsStrExt;
    match direction {
        Direction::ClientSends => {
            let name = (!local_contents_only)
                .then(|| local.file_name())
                .flatten()
                .and_then(|n| remote.join(n.as_bytes()).ok());
            (local.to_path_buf(), name.unwrap_or_else(|| remote.clone()))
        }
        Direction::DaemonSends => {
            let last = remote.components().last();
            let root = match last {
                Some(name) if !remote_contents_only => {
                    local.join(std::ffi::OsStr::from_bytes(name))
                }
                _ => local.to_path_buf(),
            };
            (root, remote.clone())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trailing_slash_rule() {
        let m = RelPath::new(b"dir").unwrap();
        let (l, r) = resolve_roots(Direction::ClientSends, Path::new("/x/a"), false, &m, false);
        assert_eq!((l, r.as_bytes()), (PathBuf::from("/x/a"), &b"dir/a"[..]));
        let (_, r) = resolve_roots(Direction::ClientSends, Path::new("/x/a"), true, &m, false);
        assert_eq!(r.as_bytes(), b"dir");
        let (l, _) = resolve_roots(Direction::DaemonSends, Path::new("/x"), false, &m, false);
        assert_eq!(l, PathBuf::from("/x/dir"));
        let (l, _) = resolve_roots(Direction::DaemonSends, Path::new("/x"), false, &m, true);
        assert_eq!(l, PathBuf::from("/x"));
        let (l, _) = resolve_roots(
            Direction::DaemonSends,
            Path::new("/x"),
            false,
            &RelPath::root(),
            false,
        );
        assert_eq!(l, PathBuf::from("/x"));
    }

    #[test]
    fn index_scope_maps_session_paths_to_index_keys() {
        let ix = crate::index::Index::open(
            tempfile::tempdir().unwrap().path(),
            tempfile::tempdir().unwrap().path(),
        )
        .unwrap()
        .into_handle();
        let r = |s: &[u8]| RelPath::new(s).unwrap();
        let s = IndexScope::new(ix.clone(), r(b"sub/d"));
        assert_eq!(s.key(&r(b"f")), r(b"sub/d/f"));
        assert_eq!(s.key(&RelPath::root()), r(b"sub/d"));
        assert_eq!(s.strip(&r(b"sub/d/x/y")), Some(r(b"x/y")));
        assert_eq!(s.strip(&r(b"sub/d")), Some(RelPath::root()));
        assert_eq!(s.strip(&r(b"sub/dd/x")), None);
        assert_eq!(s.strip(&r(b"other")), None);
        let root = IndexScope::new(ix, RelPath::root());
        assert_eq!(root.key(&r(b"f")), r(b"f"));
        assert_eq!(root.strip(&r(b"f")), Some(r(b"f")));
    }

    #[test]
    fn session_record_roundtrips_through_row() {
        use irsync_proto::v1::{plan_item::Action, PlanEntry};
        let rec = SessionRecord {
            id: [7; 16],
            key: vec![1, 2, 3],
            plan: SessionPlan {
                entries: vec![PlanEntry {
                    file_id: 1,
                    path: b"a/\xff".to_vec(),
                    action: Action::Delta as i32,
                    size: 9,
                    whole_hash: None,
                }],
            },
            progress: SessionProgress {
                done_file_ids: vec![1],
                bytes_transferred: 9,
            },
            created_at: 1,
            updated_at: 2,
        };
        assert_eq!(SessionRecord::from_row(&rec.to_row()).unwrap(), rec);
    }

    #[test]
    fn session_key_depends_on_semantic_flags_only() {
        let args = |extra: &[&str]| {
            let mut v = vec!["irsync", "-a"];
            v.extend_from_slice(extra);
            v.extend_from_slice(&["src/", "h:m/"]);
            SyncOptions::from_args(&crate::cli::Args::try_parse_from(v).unwrap())
        };
        let id = iroh::SecretKey::from_bytes(&[3; 32]).public();
        let src = Location::parse("/nonexistent/src/").unwrap();
        let dst = Location::parse("h:m/").unwrap();
        let k = |o: &SyncOptions| SessionKey::new(id, &src, &dst, o);
        let base = k(&args(&[]));
        assert_eq!(base, k(&args(&["-v", "--progress"])));
        assert_ne!(base, k(&args(&["-c"])));
        assert_ne!(base, k(&args(&["--delete"])));
        assert_ne!(base, k(&args(&["--exclude", "*.o"])));
        let other = Location::parse("h:m/sub").unwrap();
        assert_ne!(base, SessionKey::new(id, &src, &other, &args(&[])));
    }
}
