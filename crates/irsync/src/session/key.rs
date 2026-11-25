//! The session key: what makes two runs the same session.

use iroh::EndpointId;

use super::{single, SyncHello};
use crate::fs::paths::RelPath;
use crate::location::Location;
use crate::options::{DeleteMode, FilterRule, SyncOptions};
use crate::platform::os_to_bytes;

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
    let mut v = Vec::new();
    match l {
        Location::Local {
            path,
            contents_only,
        } => {
            let canon = crate::platform::native_path(path)
                .and_then(std::fs::canonicalize)
                .unwrap_or_else(|_| path.clone());
            v.extend_from_slice(b"local\0");
            v.push(*contents_only as u8);
            v.extend_from_slice(&os_to_bytes(canon.as_os_str()));
        }
        // The host string is a server name; the daemon id passed alongside identifies it.
        Location::Remote {
            path,
            contents_only,
            ..
        } => {
            v.extend_from_slice(b"remote\0");
            v.push(*contents_only as u8);
            v.extend_from_slice(path);
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
            p.dirs,
            p.links,
            p.perms,
            p.times,
            p.group,
            p.owner,
            p.devices,
            p.specials,
            p.hard_links,
            p.sparse,
        ]
        .iter()
        .enumerate()
        .fold(0u16, |acc, (i, &b)| acc | ((b as u16) << i));
        field(&mut h, b"preserve", &bits.to_le_bytes());
        field(&mut h, b"checksum", &[opts.checksum as u8]);
        field(&mut h, b"paranoid", &[opts.paranoid as u8]);
        field(&mut h, b"update", &[opts.update_only as u8]);
        field(&mut h, b"inplace", &[opts.inplace as u8]);
        field(&mut h, b"append", &[opts.append as u8]);
        let delete = match opts.delete {
            DeleteMode::Off => 0u8,
            DeleteMode::Before => 1,
            DeleteMode::During => 2,
            DeleteMode::After => 3,
        };
        field(&mut h, b"delete", &[delete, opts.delete_excluded as u8]);
        // A dry run's record must never be resumed by a real run (or back).
        field(&mut h, b"dry-run", &[opts.dry_run as u8]);
        for r in &opts.filters {
            match r {
                FilterRule::Include(s) => field(&mut h, b"include", s.as_bytes()),
                FilterRule::Exclude(s) => field(&mut h, b"exclude", s.as_bytes()),
                FilterRule::IncludeFrom(p) => {
                    field(&mut h, b"include-from", &os_to_bytes(p.as_os_str()))
                }
                FilterRule::ExcludeFrom(p) => {
                    field(&mut h, b"exclude-from", &os_to_bytes(p.as_os_str()))
                }
            }
        }
        SessionKey(h.finalize().as_bytes().to_vec())
    }

    /// The daemon's key for a session: the client's identity plus the
    /// resolved place (`export` and `rel`, so every spelling of it resumes
    /// the same session) and the flags.
    pub fn for_daemon(
        client: EndpointId,
        export: &str,
        rel: &RelPath,
        hello: &SyncHello,
        single: Option<&single::SingleFile>,
    ) -> SessionKey {
        let (direction, req) = (hello.direction, &hello.req);
        let mut h = blake3::Hasher::new();
        field(&mut h, b"irsync-daemon-session-key", b"v1");
        field(&mut h, b"client", client.as_bytes());
        field(&mut h, b"export", export.as_bytes());
        field(&mut h, b"path", rel.as_bytes());
        field(
            &mut h,
            b"flags",
            &[direction as u8, req.contents_only as u8],
        );
        field(
            &mut h,
            b"receiver",
            &[
                req.delete_mode as u8,
                req.delete_excluded as u8,
                req.dry_run as u8,
                req.update_only as u8,
                req.checksum as u8,
                req.inplace as u8,
                req.append as u8,
            ],
        );
        field(&mut h, b"sender", &[req.paranoid as u8]);
        if let Some(s) = single {
            field(&mut h, b"single", s.local.as_bytes());
        }
        SessionKey(h.finalize().as_bytes().to_vec())
    }

    /// This key for a session over one file of the root, named `name` here.
    pub fn with_single(self, name: &[u8]) -> SessionKey {
        let mut h = blake3::Hasher::new();
        field(&mut h, b"irsync-session-key", &self.0);
        field(&mut h, b"single", name);
        SessionKey(h.finalize().as_bytes().to_vec())
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }

    pub fn into_bytes(self) -> Vec<u8> {
        self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::options::Direction;
    use irsync_proto::v1::SyncRequest;

    #[test]
    fn daemon_session_key_is_by_resolved_place() {
        let c = iroh::SecretKey::from_bytes(&[1; 32]).public();
        let rel = RelPath::new(b"2024").unwrap();
        let h = |path: &[u8]| {
            SyncHello::new(
                Direction::ClientSends,
                SyncRequest {
                    path: path.to_vec(),
                    ..SyncRequest::default()
                },
            )
        };
        assert_eq!(
            SessionKey::for_daemon(c, "photos", &rel, &h(b"photos/2024"), None),
            SessionKey::for_daemon(c, "photos", &rel, &h(b"/srv/photos/2024"), None)
        );
        assert_ne!(
            SessionKey::for_daemon(c, "photos", &rel, &h(b"photos/2024"), None),
            SessionKey::for_daemon(c, "srv", &rel, &h(b"photos/2024"), None)
        );
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
        assert_ne!(base, k(&args(&["-n"])));
        assert_ne!(base, k(&args(&["--exclude", "*.o"])));
        let other = Location::parse("h:m/sub").unwrap();
        assert_ne!(base, SessionKey::new(id, &src, &other, &args(&[])));
    }
}
