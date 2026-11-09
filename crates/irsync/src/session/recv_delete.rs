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
                    // Its row would offer chunks nothing holds any more.
                    self.ctx.index().queue_forget(self.ctx.key(p));
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

/// And at most this many bytes of them: long paths would otherwise make
/// `SessionDone` hundreds of KB, which takes seconds on a slow link.
pub(super) const MAX_DONE_PATH_BYTES: usize = 64 * 1024;

/// The first paths of `v` within both caps, and how many were left out.
pub(super) fn capped_paths(v: &[RelPath]) -> (Vec<Vec<u8>>, u64) {
    let mut bytes = 0;
    let kept: Vec<Vec<u8>> = v
        .iter()
        .take(MAX_DONE_PATHS)
        .take_while(|p| {
            bytes += p.as_bytes().len();
            bytes <= MAX_DONE_PATH_BYTES
        })
        .map(|p| p.as_bytes().to_vec())
        .collect();
    let omitted = (v.len() - kept.len()) as u64;
    (kept, omitted)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths(n: usize, len: usize) -> Vec<RelPath> {
        (0..n)
            .map(|i| RelPath::new(format!("{i:0len$}").as_bytes()).unwrap())
            .collect()
    }

    #[test]
    fn short_paths_are_capped_by_count() {
        let (v, omitted) = capped_paths(&paths(MAX_DONE_PATHS + 5, 10));
        assert_eq!(v.len(), MAX_DONE_PATHS);
        assert_eq!(omitted, 5);
        let (v, omitted) = capped_paths(&paths(3, 10));
        assert_eq!((v.len(), omitted), (3, 0));
    }

    /// Long paths would make `SessionDone` hundreds of KB, which takes
    /// seconds on a slow link; they are capped by size as well.
    #[test]
    fn long_paths_are_capped_by_size() {
        let (v, omitted) = capped_paths(&paths(1000, 400));
        let bytes: usize = v.iter().map(Vec::len).sum();
        assert!(bytes <= MAX_DONE_PATH_BYTES, "{bytes}");
        assert_eq!(v.len(), MAX_DONE_PATH_BYTES / 400);
        assert_eq!(v.len() as u64 + omitted, 1000);
    }
}
