//! The sender role: walks and streams its listing, then answers the
//! receiver: `PlanItem` → digest (lookahead) → `ChunkList`; `NeedChunks` →
//! data streams. A file counts as done only at `FileAck DONE`. A file that
//! changed while being sent, or whose whole-file hash did not match at the
//! receiver, is re-stat'ed, re-digested and offered once more.

use std::collections::HashMap;
use std::io;
use std::sync::Arc;
use std::time::Duration;

use anyhow::bail;
use irsync_proto::chunk::FileDigest;
use irsync_proto::v1::{
    control::Msg, file_ack::Status, file_entry::Kind, ChunkList, FileAck, Hash, NeedChunks,
    PlanEntry, PlanItem,
};
use tokio::sync::mpsc;

use super::control::{spawn_listing, Ev, Listed, ListingSpec, Out, Persister};
use super::engine::{blocking, Ctx, Events};
use super::receiver::msg_name;
use super::Outcome;
use crate::fs::meta::lstat_entry_with_ctime;
use crate::fs::paths::RelPath;
use crate::scan::ScanItem;
use crate::send::{Sender, DEFAULT_SPLIT_THRESHOLD};

/// Re-offers per file after a change or a hash mismatch.
const MAX_RETRIES: u32 = 1;

/// Completions of the sender's tasks, delivered to its main loop.
pub(super) enum Done {
    Digested {
        file_id: u64,
        gen: u32,
        result: io::Result<FileDigest>,
    },
    Sent {
        file_id: u64,
        gen: u32,
        /// Bytes sent on success.
        result: io::Result<u64>,
    },
}

struct SFile {
    path: RelPath,
    /// Bumped on every re-offer; older task results are ignored.
    gen: u32,
    retries: u32,
    digest: Option<Arc<FileDigest>>,
    /// A `ChunkList` is out and its `NeedChunks` not yet seen.
    awaiting_need: bool,
    /// Acknowledged or given up: nothing more happens for this file.
    terminal: bool,
    #[cfg(feature = "test-hooks")]
    hooked: bool,
}

struct SenderRole {
    ctx: Arc<Ctx>,
    out: Out,
    events: mpsc::UnboundedSender<Ev>,
    sender: Arc<Sender>,
    listed: Listed,
    files: HashMap<u64, SFile>,
    files_done: u64,
    bytes: u64,
    errors: Vec<(RelPath, String)>,
    persist: Persister,
}

impl SenderRole {
    fn spawn_digest(&self, file_id: u64, gen: u32, path: RelPath, force: bool) {
        let (ctx, sender, events) = (self.ctx.clone(), self.sender.clone(), self.events.clone());
        tokio::spawn(async move {
            let result = async {
                // Re-stat: the digest must describe the file as it is now.
                let (root, rel) = (ctx.root.clone(), path.clone());
                let (entry, ctime) = blocking(move || lstat_entry_with_ctime(&root, &rel)).await?;
                if entry.kind() != Kind::Regular {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "no longer a regular file",
                    ));
                }
                let item = ScanItem { path, entry, ctime };
                if force {
                    ctx.forced.digest(&item).await
                } else {
                    sender.lookahead_digest(&item).await
                }
            }
            .await;
            let _ = events.send(Ev::Send(Done::Digested {
                file_id,
                gen,
                result,
            }));
        });
    }

    /// Gives up on the file and tells the receiver to stop waiting for it.
    fn source_error(&mut self, file_id: u64, msg: String) {
        let Some(f) = self.files.get_mut(&file_id) else {
            return;
        };
        if f.terminal {
            return;
        }
        f.terminal = true;
        f.digest = None;
        tracing::warn!(path = ?f.path, error = %msg, "cannot send file");
        self.errors.push((f.path.clone(), msg.clone()));
        self.out.send(Msg::Ack(FileAck {
            file_id,
            status: Status::SourceError as i32,
            message: msg,
        }));
    }

    /// Offers the file again with a fresh stat and a forced re-read, or gives
    /// up once the retry is spent.
    fn retry(&mut self, file_id: u64, why: String) {
        let Some(f) = self.files.get_mut(&file_id) else {
            return;
        };
        if f.retries >= MAX_RETRIES {
            self.source_error(file_id, format!("{why} (after retry)"));
            return;
        }
        tracing::info!(path = ?f.path, reason = %why, "re-sending file");
        f.retries += 1;
        f.gen += 1;
        f.digest = None;
        f.awaiting_need = false;
        let (gen, path) = (f.gen, f.path.clone());
        self.spawn_digest(file_id, gen, path, true);
    }

    fn on_plan(&mut self, p: PlanItem) -> anyhow::Result<()> {
        let file_id = p.file_id;
        if self.files.contains_key(&file_id) {
            bail!("protocol error: duplicate file id {file_id}");
        }
        let path = RelPath::new(&p.path).unwrap_or_default();
        self.files.insert(
            file_id,
            SFile {
                path: path.clone(),
                gen: 0,
                retries: 0,
                digest: None,
                awaiting_need: false,
                terminal: false,
                #[cfg(feature = "test-hooks")]
                hooked: false,
            },
        );
        self.persist.plan(PlanEntry {
            file_id,
            path: p.path,
            action: p.action,
            size: 0,
            whole_hash: None,
        });
        // Only paths this side listed as regular files are ever read: a peer
        // cannot ask for something outside the walk (e.g. through a symlink).
        let listed = self
            .listed
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .contains(&path);
        if !listed || path.as_bytes().is_empty() {
            self.source_error(file_id, "not a listed regular file".into());
            return Ok(());
        }
        self.spawn_digest(file_id, 0, path, false);
        Ok(())
    }

    fn on_digested(&mut self, file_id: u64, gen: u32, result: io::Result<FileDigest>) {
        let Some(f) = self.files.get_mut(&file_id) else {
            return;
        };
        if f.terminal || f.gen != gen {
            return;
        }
        match result {
            Ok(d) => {
                self.out.send(Msg::ChunkList(ChunkList {
                    file_id,
                    size: d.size,
                    whole_hash: Some(Hash {
                        bytes: d.hash.to_vec(),
                    }),
                    chunks: d.chunks.iter().map(|&c| c.into()).collect(),
                }));
                f.digest = Some(Arc::new(d));
                f.awaiting_need = true;
            }
            Err(e) => self.source_error(file_id, e.to_string()),
        }
    }

    fn on_need(&mut self, n: NeedChunks) {
        let file_id = n.file_id;
        let Some(f) = self.files.get_mut(&file_id) else {
            tracing::warn!(file_id, "NeedChunks for an unknown file");
            return;
        };
        if f.terminal || !f.awaiting_need {
            return;
        }
        f.awaiting_need = false;
        if n.chunk_idx.is_empty() {
            return; // The receiver has everything; wait for its ack.
        }
        let Some(digest) = f.digest.clone() else {
            return;
        };
        #[cfg(feature = "test-hooks")]
        let hook = if f.hooked {
            None
        } else {
            f.hooked = true;
            self.ctx.hooks.on_first_chunk_sent.clone()
        };
        let (gen, path, sender, events) = (
            f.gen,
            f.path.clone(),
            self.sender.clone(),
            self.events.clone(),
        );
        let idxs = n.chunk_idx;
        tokio::spawn(async move {
            let bytes: u64 = idxs
                .iter()
                .filter_map(|&i| digest.chunks.get(i as usize))
                .map(|c| u64::from(c.len))
                .sum();
            #[cfg(feature = "test-hooks")]
            let result = match hook {
                Some(h) if idxs.len() > 1 => {
                    match sender
                        .send_chunks(file_id, &path, &digest, &idxs[..1])
                        .await
                    {
                        Ok(()) => {
                            h(&path).await;
                            sender
                                .send_chunks(file_id, &path, &digest, &idxs[1..])
                                .await
                        }
                        Err(e) => Err(e),
                    }
                }
                _ => sender.send_chunks(file_id, &path, &digest, &idxs).await,
            };
            #[cfg(not(feature = "test-hooks"))]
            let result = sender.send_chunks(file_id, &path, &digest, &idxs).await;
            let _ = events.send(Ev::Send(Done::Sent {
                file_id,
                gen,
                result: result.map(|()| bytes),
            }));
        });
    }

    fn on_sent(&mut self, file_id: u64, gen: u32, result: io::Result<u64>) {
        let Some(f) = self.files.get_mut(&file_id) else {
            return;
        };
        match result {
            Ok(b) => self.bytes += b,
            Err(e) => {
                if f.terminal || f.gen != gen {
                    return;
                }
                if e.kind() == io::ErrorKind::InvalidData {
                    // The file changed under us; never send stale data.
                    self.retry(file_id, e.to_string());
                } else {
                    self.source_error(file_id, e.to_string());
                }
            }
        }
    }

    fn on_ack(&mut self, a: FileAck) {
        let file_id = a.file_id;
        let Some(f) = self.files.get_mut(&file_id) else {
            tracing::warn!(file_id, "FileAck for an unknown file");
            return;
        };
        if f.terminal {
            return;
        }
        match Status::try_from(a.status) {
            Ok(Status::Done) => {
                f.terminal = true;
                f.digest = None;
                self.files_done += 1;
                self.persist.done(file_id);
            }
            Ok(Status::HashMismatch) => self.retry(file_id, "whole-file hash mismatch".into()),
            Ok(Status::WriteError) => {
                f.terminal = true;
                f.digest = None;
                let msg = format!("receiver: {}", a.message);
                tracing::warn!(path = ?f.path, error = %msg, "file failed");
                self.errors.push((f.path.clone(), msg));
            }
            Ok(Status::SourceError) | Err(_) => {
                tracing::warn!(file_id, status = a.status, "unexpected FileAck status");
            }
        }
    }
}

/// Runs the sender role until the receiver's `SessionDone`.
pub(super) async fn run(
    ctx: Arc<Ctx>,
    out: Out,
    events_tx: mpsc::UnboundedSender<Ev>,
    mut events: Events,
) -> anyhow::Result<Outcome> {
    let hashed_before = ctx.files_hashed();
    let root_ok = std::fs::metadata(&ctx.root).is_ok_and(|m| m.is_dir());
    if !root_ok {
        bail!("source {} is not a directory", ctx.root.display());
    }
    let persist = Persister::start(ctx.index.clone(), ctx.session_id, ctx.key.clone()).await;
    let listed: Listed = Arc::default();
    let listing = spawn_listing(
        ListingSpec {
            root: ctx.root.clone(),
            filters: ctx.filters.clone(),
            hasher: ctx.hasher.clone(),
            skip: None,
        },
        out.clone(),
        None,
        Some(listed.clone()),
    );
    let sender = Arc::new(Sender::new(
        ctx.conn.clone(),
        ctx.session_id,
        ctx.root.clone(),
        ctx.hasher.clone(),
        ctx.opts.parallel,
        DEFAULT_SPLIT_THRESHOLD,
        ctx.opts.bwlimit,
    ));
    let mut s = SenderRole {
        ctx: ctx.clone(),
        out: out.clone(),
        events: events_tx,
        sender,
        listed,
        files: HashMap::new(),
        files_done: 0,
        bytes: 0,
        errors: Vec::new(),
        persist,
    };
    let mut tick = tokio::time::interval(Duration::from_secs(2));
    let res: anyhow::Result<()> = loop {
        let ev = tokio::select! {
            ev = events.recv() => ev,
            _ = tick.tick() => {
                s.persist.bytes(s.bytes);
                s.persist.maybe_flush().await;
                continue;
            }
        };
        let Some(ev) = ev else {
            break Err(anyhow::anyhow!("event channel closed"));
        };
        let step = match ev {
            Ev::Send(Done::Digested {
                file_id,
                gen,
                result,
            }) => {
                s.on_digested(file_id, gen, result);
                Ok(())
            }
            Ev::Send(Done::Sent {
                file_id,
                gen,
                result,
            }) => {
                s.on_sent(file_id, gen, result);
                Ok(())
            }
            Ev::Ctrl(Msg::Plan(p)) => s.on_plan(p),
            Ev::Ctrl(Msg::Need(n)) => {
                s.on_need(n);
                Ok(())
            }
            Ev::Ctrl(Msg::Ack(a)) => {
                s.on_ack(a);
                Ok(())
            }
            Ev::Ctrl(Msg::ListingEnd(_)) => Ok(()),
            Ev::Ctrl(Msg::Done(d)) => {
                tracing::debug!(files = d.files, bytes = d.bytes, "receiver done");
                break Ok(());
            }
            Ev::Ctrl(Msg::Error(e)) => Err(anyhow::anyhow!("peer error: {}", e.message)),
            Ev::Ctrl(m) => Err(anyhow::anyhow!(
                "protocol error: unexpected {} from the receiver",
                msg_name(&m)
            )),
            Ev::PeerEof => Err(anyhow::anyhow!(
                "the receiver closed the control stream before the session ended"
            )),
            Ev::PeerFailed(e) | Ev::WriterFailed(e) => Err(anyhow::anyhow!("control stream: {e}")),
            Ev::Recv(_) => Ok(()),
        };
        if let Err(e) = step {
            break Err(e);
        }
    };
    s.persist.bytes(s.bytes);
    s.persist.flush().await;
    if let Err(e) = res {
        listing.abort();
        return Err(e);
    }
    out.finish();
    Ok(Outcome {
        session_id: ctx.session_id,
        files_transferred: s.files_done,
        bytes_transferred: s.bytes,
        files_skipped: 0,
        files_hashed: ctx.files_hashed() - hashed_before,
        errors: s.errors,
    })
}
