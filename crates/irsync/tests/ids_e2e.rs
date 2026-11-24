//! Owner and group names cross the wire (`FileEntry.user/group`): a receiver
//! maps them to its own ids, so a peer's different numbering is no change;
//! `--numeric-ids` keeps the numbers as sent.

mod common;

use std::fs;
use std::sync::Arc;
use std::time::Duration;

use common::{Flow, Harness};
use irsync::cli::{Args, SyncOptions};
use irsync::platform::{egid, euid, group_name, mode_of, mtime_of, owner_of, user_name};
use irsync::session::Event;
use irsync_proto::v1::{file_entry::Kind, FileEntry};

fn opts(extra: &[&str]) -> SyncOptions {
    let mut v = vec!["irsync", "-a", "-i"];
    v.extend_from_slice(extra);
    v.extend_from_slice(&["src/", "host:m/"]);
    SyncOptions::from_args(&Args::try_parse_from(v).unwrap())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn peer_ids_are_mapped_by_name_unless_numeric_ids() {
    tokio::time::timeout(Duration::from_secs(30), async {
        let (uid, gid) = (euid(), egid());
        let user = user_name(uid).unwrap();
        let group = group_name(gid).unwrap();
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
                mode: mode_of(&md),
                mtime_sec: mtime_of(&md).0,
                mtime_nsec: mtime_of(&md).1,
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

/// A chunk list carries the owner and group the file was hashed under,
/// with their names: the receiver maps the names as for a listing entry.
/// The client's list says a stranger's group number with the name of a
/// group of ours: mapped by name, the copy gets our group; with
/// `--numeric-ids` the stranger's number, which we may not set.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn chunk_list_ids_are_mapped_by_name_unless_numeric_ids() {
    use irsync::session::StreamKind;
    use irsync_proto::v1::control::Msg;
    tokio::time::timeout(Duration::from_secs(30), async {
        let egid = irsync::platform::egid();
        let Some(g2) = irsync::fs::meta::supplementary_groups()
            .into_iter()
            .find(|&g| g != egid)
        else {
            eprintln!("skipped: this user is in only one group");
            return;
        };
        let name = group_name(g2).unwrap();
        let stranger = g2.wrapping_add(100_000);
        for (extra, mapped) in [(&[][..], true), (&["--numeric-ids"][..], false)] {
            let h = Harness::new().await;
            let src = h.src(Flow::Push).join("f");
            fs::write(&src, b"content").unwrap();
            irsync::platform::testutil::chown(&src, None, Some(g2)).unwrap();
            let mut local = h.local_side();
            let name = name.clone();
            local.hooks.tamper_frames = Some(Arc::new(move |kind, msg| match (kind, msg) {
                (StreamKind::Control, Msg::ChunkListPart(mut p)) if p.first == 0 => {
                    assert_eq!((p.gid, p.group.as_str()), (g2, name.as_str()));
                    p.gid = stranger;
                    vec![Msg::ChunkListPart(p)]
                }
                (_, m) => vec![m],
            }));
            let out = h.run_side(&opts(extra), Flow::Push, local).await.unwrap();
            assert!(out.errors.is_empty(), "{extra:?}: {:?}", out.errors);
            let gid = owner_of(&fs::metadata(h.dst(Flow::Push).join("f")).unwrap()).1;
            assert_eq!(gid == g2, mapped, "{extra:?}: group {gid}");
        }
    })
    .await
    .expect("test timed out after 30 s");
}
