//! Owner and group names (rsync's default id handling): the sender attaches
//! the names its ids have, the receiver maps them to its own ids when the
//! names exist there and keeps the numbers otherwise. `--numeric-ids` skips
//! the mapping. Lookups go through the system databases once per id or name.

use std::collections::HashMap;

use irsync_proto::v1::FileEntry;
use nix::unistd::{Gid, Group, Uid, User};

/// The names of this side's ids, for a listing.
#[derive(Default)]
pub struct IdNames {
    users: HashMap<u32, String>,
    groups: HashMap<u32, String>,
}

impl IdNames {
    pub fn new() -> IdNames {
        IdNames::default()
    }

    pub fn user(&mut self, uid: u32) -> &str {
        self.users.entry(uid).or_insert_with(|| {
            User::from_uid(Uid::from_raw(uid))
                .ok()
                .flatten()
                .map(|u| u.name)
                .unwrap_or_default()
        })
    }

    pub fn group(&mut self, gid: u32) -> &str {
        self.groups.entry(gid).or_insert_with(|| {
            Group::from_gid(Gid::from_raw(gid))
                .ok()
                .flatten()
                .map(|g| g.name)
                .unwrap_or_default()
        })
    }

    /// Fills `user` and `group` from the entry's ids.
    pub fn attach(&mut self, e: &mut FileEntry) {
        e.user = self.user(e.uid).to_string();
        e.group = self.group(e.gid).to_string();
    }
}

/// This side's ids for the names a peer sent.
#[derive(Default)]
pub struct IdMap {
    users: HashMap<String, Option<u32>>,
    groups: HashMap<String, Option<u32>>,
}

impl IdMap {
    pub fn new() -> IdMap {
        IdMap::default()
    }

    fn uid_of(&mut self, name: &str) -> Option<u32> {
        *self
            .users
            .entry(name.to_string())
            .or_insert_with(|| User::from_name(name).ok().flatten().map(|u| u.uid.as_raw()))
    }

    fn gid_of(&mut self, name: &str) -> Option<u32> {
        *self.groups.entry(name.to_string()).or_insert_with(|| {
            Group::from_name(name)
                .ok()
                .flatten()
                .map(|g| g.gid.as_raw())
        })
    }

    /// Replaces the entry's ids by the local ids of its names, where the
    /// names exist here; an unknown or empty name leaves the number as sent.
    pub fn localize(&mut self, e: &mut FileEntry) {
        if !e.user.is_empty() {
            if let Some(uid) = self.uid_of(&e.user) {
                e.uid = uid;
            }
        }
        if !e.group.is_empty() {
            if let Some(gid) = self.gid_of(&e.group) {
                e.gid = gid;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn me() -> (u32, u32) {
        (
            nix::unistd::geteuid().as_raw(),
            nix::unistd::getegid().as_raw(),
        )
    }

    #[test]
    fn names_are_attached_and_mapped_back() {
        let (uid, gid) = me();
        let mut names = IdNames::new();
        let mut e = FileEntry {
            uid,
            gid,
            ..FileEntry::default()
        };
        names.attach(&mut e);
        let user = User::from_uid(Uid::from_raw(uid)).unwrap().unwrap().name;
        assert_eq!(e.user, user);
        assert!(!e.group.is_empty());
        // The peer's numbers differ; the names bring them home.
        e.uid = uid.wrapping_add(1000);
        e.gid = gid.wrapping_add(1000);
        let mut map = IdMap::new();
        map.localize(&mut e);
        assert_eq!((e.uid, e.gid), (uid, gid));
    }

    #[test]
    fn unknown_or_missing_names_keep_the_numbers() {
        let mut map = IdMap::new();
        let mut e = FileEntry {
            uid: 4242,
            gid: 4343,
            user: "no-such-user-irsync-test".into(),
            group: "no-such-group-irsync-test".into(),
            ..FileEntry::default()
        };
        map.localize(&mut e);
        assert_eq!((e.uid, e.gid), (4242, 4343));
        e.user.clear();
        e.group.clear();
        map.localize(&mut e);
        assert_eq!((e.uid, e.gid), (4242, 4343));
    }

    #[test]
    fn an_id_without_a_name_sends_an_empty_name() {
        let mut names = IdNames::new();
        // Chosen to exist on no sane system.
        assert_eq!(names.user(0xfffe_fffe), "");
        assert_eq!(names.group(0xfffe_fffe), "");
    }
}
