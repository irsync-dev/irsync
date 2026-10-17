//! The receiver's deletion passes (`--delete*`): run under the session root
//! before the transfers (once both listings have ended) or after them,
//! refused when a listing was incomplete unless `--ignore-errors`.

use super::Receiver;
use crate::delete::DeletePass;
use crate::fs::paths::RelPath;

impl Receiver {
    /// Runs a deletion pass under the session root: refused when either
    /// listing was incomplete (unless `--ignore-errors`). Returns the paths
    /// deleted.
    pub(super) async fn run_deletes(&mut self, mut pass: DeletePass) -> Vec<RelPath> {
        self.await_own_walk().await;
        pass.set_spent(self.persist.deleted());
        let incomplete = self.peer_listing_incomplete || self.local_listing_incomplete;
        match pass.execute(self.ctx.root.clone(), incomplete).await {
            Ok(rep) => {
                for p in &rep.deleted {
                    self.ctx.hooks.event("delete", p);
                }
                self.persist.add_deleted(rep.deleted.len() as u64);
                self.max_delete_reached |= rep.stopped_at_max;
                self.errors.extend(rep.errors);
                self.would_delete.extend(rep.would_delete);
                self.deleted.extend(rep.deleted.iter().cloned());
                rep.deleted
            }
            Err(refused) => {
                self.errors.push((RelPath::root(), refused.to_string()));
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
            let deleted = self.run_deletes(pass).await;
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
