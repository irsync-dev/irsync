//! Owner and group names cross the wire (`FileEntry.user/group`): a receiver
//! maps them to its own ids, so a peer's different numbering is no change;
//! `--numeric-ids` keeps the numbers as sent.

mod common;

use std::fs;
use std::os::unix::fs::MetadataExt;
use std::sync::Arc;
use std::time::Duration;

use common::{Flow, Harness};
use irsync::cli::{Args, SyncOptions};
use irsync::session::Event;
use irsync_proto::v1::{file_entry::Kind, FileEntry};
use nix::unistd::{getegid, geteuid, Gid, Group, Uid, User};

fn opts(extra: &[&str]) -> SyncOptions {
    let mut v = vec!["irsync", "-a", "-i"];
    v.extend_from_slice(extra);
    v.extend_from_slice(&["src/", "host:m/"]);
    SyncOptions::from_args(&Args::try_parse_from(v).unwrap())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn peer_ids_are_mapped_by_name_unless_numeric_ids() {
    tokio::time::timeout(Duration::from_secs(30), async {
        let (uid, gid) = (geteuid().as_raw(), getegid().as_raw());
        let user = User::from_uid(Uid::from_raw(uid)).unwrap().unwrap().name;
        let group = Group::from_gid(Gid::from_raw(gid)).unwrap().unwrap().name;
        for (extra, numbers_as_sent) in [(&[][..], false), (&["--numeric-ids"][..], true)] {
            let h = Harness::new().await;
            // The destination already holds `ghost`, owned by us. The pushing
            // client lists it with our names but a stranger's numbers.
            let dst = h.export.path().join("ghost");
            fs::create_dir(&dst).unwrap();
            let md = fs::metadata(&dst).unwrap();
            let ghost = FileEntry {
                path: b"ghost".to_vec(),
                kind: Kind::Directory as i32,
                mode: md.mode(),
                mtime_sec: md.mtime(),
                mtime_nsec: md.mtime_nsec() as u32,
                uid: uid.wrapping_add(1000),
                gid: gid.wrapping_add(1000),
                user: user.clone(),
                group: group.clone(),
                ..FileEntry::default()
            };
            let mut local = h.local_side();
            local.hooks.inject_listing = Some(Arc::new(move |e: &FileEntry| {
                if e.path.is_empty() {
                    vec![ghost.clone()]
                } else {
                    Vec::new()
                }
            }));
            let mut rx = local.hooks.bus.subscribe();
            let out = h.run_side(&opts(extra), Flow::Push, local).await.unwrap();
            assert!(out.errors.is_empty(), "{extra:?}: {:?}", out.errors);
            let mut codes = Vec::new();
            while let Ok(e) = rx.try_recv() {
                if let Event::Itemized(p, c) = e {
                    if p.as_bytes() == b"ghost" {
                        codes.push(c);
                    }
                }
            }
            let owner_differs = codes.iter().any(|c| c.contains('o') || c.contains('g'));
            assert_eq!(
                owner_differs, numbers_as_sent,
                "{extra:?}: itemized {codes:?}"
            );
        }
    })
    .await
    .expect("test timed out after 30 s");
}
