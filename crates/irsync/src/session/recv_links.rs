//! Hard links (`-H`) on the receiver: recorded as the planner decides them
//! (and stored with the plan, so a resumed session places them too), then
//! placed in one pass once every regular file is placed or has failed — a
//! link's target may itself have been in flight until then.

use irsync_proto::v1::{
    control::Msg, file_entry::Kind, plan_item::Action, CheckLink, CheckLinksEnd, FileEntry,
    PlanEntry, SourceStat, Unlinked,
};

use super::super::engine::blocking;
use super::super::planner::{place_hardlink, Linked};
use super::{LinkCheck, Receiver};
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
            append_from: 0,
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

    /// A file was placed: with `-H`, one listed with more names than one
    /// is kept, with the stat it was hashed under, for the link check (and
    /// stored, for a later connection of the session).
    pub(super) fn note_placed(&mut self, path: RelPath, nlink: u32, hashed: Option<SourceStat>) {
        if !self.ctx.opts.check_links || nlink < 2 {
            return;
        }
        if let Some(stat) = hashed {
            self.persist.placed(&path, stat);
            self.placed_from.insert(path, stat);
        }
    }

    /// Sends a `CheckLink` for each link to be made to a file placed in
    /// this session, then `CheckLinksEnd`, from a task: one paced frame per
    /// link. A link to a file this session did not send (it was up to
    /// date) is not checked: both names held that content when listed.
    pub(super) fn ask_link_check(&mut self) {
        let asks: Vec<CheckLink> = self
            .links
            .iter()
            .filter(|l| !self.failed.contains(&l.target))
            .filter_map(|l| {
                Some(CheckLink {
                    path: l.path.as_bytes().to_vec(),
                    want: Some(*self.placed_from.get(&l.target)?),
                })
            })
            .collect();
        tracing::debug!(links = asks.len(), "asking the sender to check links");
        let out = self.out.clone();
        self.link_asker = Some(tokio::spawn(async move {
            for a in asks {
                out.send_paced(Msg::CheckLink(a)).await;
            }
            out.send_paced(Msg::CheckLinksEnd(CheckLinksEnd {})).await;
        }));
    }

    /// The sender found `u.path` is no longer the file it was to be linked
    /// to: it is not linked. If it is a regular file now (`u.entry`), it is
    /// planned as a file of its own; otherwise it fails in the link pass.
    pub(super) async fn on_unlinked(&mut self, u: Unlinked) -> anyhow::Result<()> {
        if self.link_check != LinkCheck::Asked {
            anyhow::bail!("protocol error: Unlinked outside the link check");
        }
        let Ok(path) = RelPath::new(&u.path) else {
            anyhow::bail!("protocol error: Unlinked with an invalid path");
        };
        // Only names this side asked about, each once.
        let Some(at) = self.links.iter().position(|l| l.path == path) else {
            anyhow::bail!("protocol error: Unlinked for a name not being linked");
        };
        match u.entry {
            Some(entry) if entry.kind() == Kind::Regular => {
                let l = self.links.swap_remove(at);
                if !self.replanned.insert(path.clone()) {
                    anyhow::bail!("protocol error: Unlinked for a path already sent again");
                }
                tracing::info!(?path, target = ?l.target, "no longer a hard link; sending it as a file");
                // Its stored plan entry is a link: done with, so a resumed
                // session does not link it after all.
                self.persist.done(l.file_id);
                self.plan_again(path, entry).await;
            }
            Some(_) => anyhow::bail!("protocol error: Unlinked with an entry of another kind"),
            None => {
                if !self.unlinked.insert(path) {
                    anyhow::bail!("protocol error: a second Unlinked for one name");
                }
            }
        }
        Ok(())
    }

    pub(super) fn on_links_checked(&mut self) -> anyhow::Result<()> {
        if self.link_check != LinkCheck::Asked {
            anyhow::bail!("protocol error: LinksChecked outside the link check");
        }
        self.link_check = LinkCheck::Done;
        self.link_asker = None;
        Ok(())
    }

    /// Places every recorded hard link. Runs once no file is in flight, so
    /// each target is final; a target that failed this session is not
    /// linked to (its old content would be). The planner only records links
    /// whose target it handled as a regular file, and the placement resolves
    /// both names without following symlinks. With `-H`, a name the sender
    /// found changed since its target was hashed (`Unlinked`) is not
    /// linked: it fails, and the destination keeps what it had.
    pub(super) async fn link_pass(&mut self) {
        let links = std::mem::take(&mut self.links);
        if links.is_empty() {
            return;
        }
        let ctx = self.ctx.clone();
        let failed = std::mem::take(&mut self.failed);
        let unlinked = std::mem::take(&mut self.unlinked);
        let link = ctx.opts.preserve.hard_links;
        let results = blocking(move || {
            Ok(links
                .into_iter()
                .map(|l| {
                    let res = if failed.contains(&l.target) {
                        Err(std::io::Error::other("hard link target failed"))
                    } else if unlinked.contains(&l.path) {
                        Err(std::io::Error::other(
                            "changed during the sync: no longer the file it was a link to",
                        ))
                    } else {
                        let apply = ctx.apply_opts(&l.entry);
                        place_hardlink(&ctx.root, &l.target, &l.path, link, &l.entry, &apply)
                    };
                    (l.file_id, l.path, l.target, res)
                })
                .collect::<Vec<_>>())
        })
        .await
        .unwrap_or_else(|e| vec![(0, RelPath::root(), RelPath::root(), Err(e))]);
        for (file_id, path, target, res) in results {
            match res {
                Ok(()) => {
                    self.persist.done(file_id);
                    self.ctx
                        .hooks
                        .bus
                        .emit(crate::session::Event::Hardlinked(path, target));
                }
                Err(e) => {
                    tracing::info!(?path, error = %e, "hard link failed");
                    let msg = format!("hard link: {e}");
                    self.ctx
                        .hooks
                        .bus
                        .emit(crate::session::Event::Failed(path.clone(), msg.clone()));
                    self.errors.push((path, msg));
                }
            }
        }
    }
}
