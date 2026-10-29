//! The receiver's deletion passes (`--delete*`): run under the session root
//! before the transfers (once both listings have ended), per directory as
//! the planner leaves it, or after the transfers; refused when a listing
//! was incomplete (so far, for a pass during the run) unless
//! `--ignore-errors`.

use tracing::Instrument;

use super::Receiver;
use crate::cli::DeleteMode;
use crate::delete::DeletePass;
use crate::fs::paths::RelPath;
use crate::telemetry::PhaseKind;

impl Receiver {
    /// Runs a deletion pass under the session root: refused when
    /// `incomplete` (a listing was incomplete) unless `--ignore-errors`.
    /// Returns the paths deleted.
    pub(super) async fn run_deletes(
        &mut self,
        mut pass: DeletePass,
        incomplete: bool,
    ) -> Vec<RelPath> {
        let spent = self.persist.deleted();
        pass.set_spent(spent);
        pass.set_activity(self.ctx.activity.clone());
        pass.set_meter(self.ctx.meter.clone());
        let busy = self.ctx.busy.clone();
        let bus = self.ctx.hooks.bus.clone();
        // Progress counts the session's deletions so far; a pass during the
        // run (`--delete-during`) has no total, the final pass gives it.
        let is_final = pass.mode() != DeleteMode::During || self.report.is_some();
        pass.set_progress(std::sync::Arc::new(move |done, total| {
            let (done, total) = (spent + done, spent + total);
            busy.deleted
                .store(done, std::sync::atomic::Ordering::Relaxed);
            busy.delete_total
                .store(total, std::sync::atomic::Ordering::Relaxed);
            bus.emit(crate::session::Event::Delete {
                party: crate::session::Party::Local,
                done,
                total: is_final.then_some(total),
            });
        }));
        let phase = self.ctx.phase(PhaseKind::Delete);
        phase.span().record("mode", format!("{:?}", pass.mode()));
        let executed = pass
            .execute(self.ctx.root.clone(), incomplete)
            .instrument(phase.span().clone())
            .await;
        drop(phase);
        match executed {
            Ok(rep) => {
                for p in &rep.deleted {
                    self.ctx.hooks.event("delete", p);
                    let bus = &self.ctx.hooks.bus;
                    bus.emit(crate::session::Event::Itemized(
                        p.clone(),
                        crate::plan::DELETING.into(),
                    ));
                    bus.emit(crate::session::Event::Deleted(p.clone()));
                }
                self.persist.add_deleted(rep.deleted.len() as u64);
                self.max_delete_reached |= rep.stopped_at_max;
                self.errors.extend(rep.errors);
                for p in &rep.would_delete {
                    self.ctx
                        .hooks
                        .bus
                        .emit(crate::session::Event::WouldDelete(p.clone()));
                }
                self.would_delete.extend(rep.would_delete);
                self.deleted.extend(rep.deleted.iter().cloned());
                rep.deleted
            }
            Err(refused) => {
                if !self.delete_refused {
                    self.delete_refused = true;
                    self.errors.push((RelPath::root(), refused.to_string()));
                }
                Vec::new()
            }
        }
    }

    /// Runs a waiting `--delete-before` pass once the sender's listing has ended.
    pub(super) async fn maybe_delete_before(&mut self) {
        if !self.peer_listing_done {
            return;
        }
        if let Some((pass, reply)) = self.pending_before.take() {
            self.await_own_walk().await;
            let incomplete = self.peer_listing_incomplete || self.local_listing_incomplete;
            let deleted = self.run_deletes(pass, incomplete).await;
            let _ = reply.send(deleted);
        }
    }
}

/// At most this many deleted (or would-delete) paths travel in `SessionDone`.
pub(super) const MAX_DONE_PATHS: usize = 1000;

pub(super) fn capped_paths(v: &[RelPath]) -> Vec<Vec<u8>> {
    v.iter()
        .take(MAX_DONE_PATHS)
        .map(|p| p.as_bytes().to_vec())
        .collect()
}
