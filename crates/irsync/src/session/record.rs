//! A session as the index stores it, and the index scope a session uses.

use irsync_proto::v1::{SessionPlan, SessionProgress};
use prost::Message;

use crate::fs::paths::RelPath;
use crate::index::{IndexHandle, SessionRow};

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
            peer: self.plan.remote_id.clone(),
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
/// root (the export root on the daemon), so `key` maps session-relative paths
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
pub(super) fn join_rel(a: &RelPath, b: &RelPath) -> RelPath {
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

#[cfg(test)]
mod tests {
    use super::*;

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
                    entry: None,
                    append_from: 0,
                }],
                plan_complete: true,
                remote_id: vec![4; 32],
                ..SessionPlan::default()
            },
            progress: SessionProgress {
                done_file_ids: vec![1],
                bytes_transferred: 9,
                deleted_count: 3,
                placed: vec![irsync_proto::v1::PlacedStat {
                    path: b"a".to_vec(),
                    stat: Some(irsync_proto::v1::SourceStat {
                        size: 5,
                        ctime_sec: 6,
                        inode: 7,
                        ..Default::default()
                    }),
                }],
            },
            created_at: 1,
            updated_at: 2,
        };
        assert_eq!(SessionRecord::from_row(&rec.to_row()).unwrap(), rec);
    }
}
