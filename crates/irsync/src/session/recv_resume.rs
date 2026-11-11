//! Resuming a receiver from a complete stored plan: no listings are
//! exchanged; the planned directories are recreated and every file not yet
//! placed is requested again.

use std::collections::HashSet;

use irsync_proto::v1::{control::Msg, plan_item::Action, PlanItem, SessionPlan};

use super::super::engine::blocking;
use super::super::planner::{mkdir_replacing, Deferred, DirEntry, PlannerReport};
use super::Receiver;
use crate::fs::paths::RelPath;

impl Receiver {
    /// Resumes from a complete stored plan: makes sure the planned
    /// directories exist and requests every file not yet placed.
    pub(super) async fn resume_plan(&mut self, plan: SessionPlan) {
        let dirs: Vec<RelPath> = plan
            .dirs
            .iter()
            .filter_map(|e| RelPath::new(&e.path).ok())
            .collect();
        let ctx = self.ctx.clone();
        let mkdir_errors = blocking(move || {
            Ok(dirs
                .into_iter()
                .filter_map(|d| {
                    mkdir_replacing(&ctx.full(&d))
                        .err()
                        .map(|e| (d, format!("mkdir: {e}")))
                })
                .collect::<Vec<_>>())
        })
        .await
        .unwrap_or_default();
        self.errors.extend(mkdir_errors);
        let mut requested = 0usize;
        let max_id = plan.entries.iter().map(|e| e.file_id).max().unwrap_or(0);
        // The files sent on this connection: the only ones the sender's
        // final re-check looks at again (it listed nothing).
        let mut regular = HashSet::new();
        for e in plan.entries {
            if self.persist.is_done(e.file_id) {
                continue;
            }
            let (Ok(path), Some(entry)) = (RelPath::new(&e.path), e.entry) else {
                let path = RelPath::new(&e.path).unwrap_or_default();
                tracing::warn!(?path, file_id = e.file_id, "unusable stored plan entry");
                self.errors
                    .push((path, "not resumed: unusable stored plan entry".into()));
                continue;
            };
            if self.resume_link(e.file_id, &path, &entry) {
                continue;
            }
            let action = Action::try_from(e.action).unwrap_or(Action::Whole);
            regular.insert(path.clone());
            self.queued.push_back((
                e.file_id,
                path,
                entry,
                action,
                e.append_from,
                Deferred::default(),
            ));
            requested += 1;
        }
        tracing::info!(requested, "resuming the stored plan");
        self.pump();
        self.report = Some(PlannerReport {
            skipped: plan.skipped,
            errors: plan
                .errors
                .into_iter()
                .map(|e| (RelPath::new(&e.path).unwrap_or_default(), e.message))
                .collect(),
            dirs: plan
                .dirs
                .into_iter()
                .filter_map(|entry| {
                    let path = RelPath::new(&entry.path).ok()?;
                    Some(DirEntry {
                        path,
                        entry,
                        apply: true,
                    })
                })
                .collect(),
            touched: HashSet::new(),
            deletes: None,
            listed: Default::default(),
            created: 0,
            max_id,
            regular: HashSet::new(),
            kept_links: Default::default(),
        });
        self.regular = regular;
        self.peer_listing_done = true;
        self.peer_listing_incomplete = plan.peer_listing_incomplete;
        self.plan_complete = true;
    }

    /// Requests queued files of a resumed plan while fewer than the
    /// in-flight cap are in flight (the planner's bound, for this path).
    pub(super) fn pump(&mut self) {
        while self.active < self.inflight_cap {
            let Some((file_id, path, entry, action, append_from, deferred)) =
                self.queued.pop_front()
            else {
                return;
            };
            self.register(file_id, path.clone(), entry, action, deferred, None);
            self.ctx.hooks.event("plan", &path);
            self.out.send(Msg::Plan(PlanItem {
                file_id,
                path: path.as_bytes().to_vec(),
                action: action as i32,
                append_from,
            }));
        }
    }
}
