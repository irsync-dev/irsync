//! Resuming a receiver from a complete stored plan: no listings are
//! exchanged; the planned directories are recreated and every file not yet
//! placed is requested again.

use std::collections::HashSet;

use irsync_proto::v1::{control::Msg, plan_item::Action, PlanItem, SessionPlan};

use super::super::engine::blocking;
use super::super::planner::{mkdir_replacing, DirEntry, PlannerReport};
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
            self.register(e.file_id, path.clone(), entry, action, None);
            self.ctx.hooks.event("plan", &path);
            self.out.send(Msg::Plan(PlanItem {
                file_id: e.file_id,
                path: path.as_bytes().to_vec(),
                action: action as i32,
            }));
            requested += 1;
        }
        tracing::info!(requested, "resuming the stored plan");
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
        });
        self.peer_listing_done = true;
        self.peer_listing_incomplete = plan.peer_listing_incomplete;
        self.plan_complete = true;
    }
}
