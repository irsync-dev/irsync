//! Hard links (`-H`) on the receiver: recorded as the planner decides them
//! (and stored with the plan, so a resumed session places them too), then
//! placed in one pass once every regular file is placed or has failed — a
//! link's target may itself have been in flight until then.

use irsync_proto::v1::{plan_item::Action, FileEntry, PlanEntry};

use super::super::engine::blocking;
use super::super::planner::{place_hardlink, Linked};
use super::Receiver;
use crate::fs::paths::RelPath;

impl Receiver {
    /// A hard link the planner decided on: stored with the plan, placed later.
    pub(super) fn on_linked(&mut self, l: Linked) {
        self.persist.plan(PlanEntry {
            file_id: l.file_id,
            path: l.path.as_bytes().to_vec(),
            action: Action::Whole as i32,
            size: l.entry.size,
            whole_hash: None,
            entry: Some(l.entry.clone()),
        });
        self.links.push(l);
    }

    /// Takes a stored plan entry that is a hard link (resume); `false` if it is not one.
    pub(super) fn resume_link(&mut self, file_id: u64, path: &RelPath, entry: &FileEntry) -> bool {
        if entry.kind() != irsync_proto::v1::file_entry::Kind::Hardlink {
            return false;
        }
        match RelPath::new(&entry.link_target) {
            Ok(target) => self.links.push(Linked {
                file_id,
                path: path.clone(),
                entry: entry.clone(),
                target,
            }),
            Err(_) => self
                .errors
                .push((path.clone(), "hard link with an invalid target".into())),
        }
        true
    }

    /// Places every recorded hard link. Runs once no file is in flight, so
    /// each target is final; a target that failed this session is not
    /// linked to (its old content would be).
    pub(super) async fn link_pass(&mut self) {
        let links = std::mem::take(&mut self.links);
        if links.is_empty() {
            return;
        }
        let ctx = self.ctx.clone();
        let failed = std::mem::take(&mut self.failed);
        let link = ctx.opts.preserve.hard_links;
        let results = blocking(move || {
            Ok(links
                .into_iter()
                .map(|l| {
                    let res = if failed.contains(&l.target) {
                        Err(std::io::Error::other("hard link target failed"))
                    } else {
                        let apply = ctx.apply_opts(&l.entry);
                        let (target, full) = (ctx.full(&l.target), ctx.full(&l.path));
                        place_hardlink(&target, &full, link, &l.entry, &apply)
                    };
                    (l.file_id, l.path, res)
                })
                .collect::<Vec<_>>())
        })
        .await
        .unwrap_or_else(|e| vec![(0, RelPath::root(), Err(e))]);
        for (file_id, path, res) in results {
            match res {
                Ok(()) => self.persist.done(file_id),
                Err(e) => {
                    tracing::warn!(?path, error = %e, "hard link failed");
                    self.errors.push((path, format!("hard link: {e}")));
                }
            }
        }
    }
}
