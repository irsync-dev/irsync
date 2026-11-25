//! The client side of irsync: reaching a server ([`connect`]), running a
//! sync of one or more sources ([`sync`]) and listing a server's exports
//! ([`list_exports`]). The command line, logging and exit codes stay in
//! the binary.

mod connect;
mod plan;

use std::sync::Arc;
use std::time::Instant;

use anyhow::{bail, Context};
use iroh::EndpointAddr;
use irsync_proto::v1::{hello, ExportEntry, Hello, ListExports, SyncRequest};
use irsync_proto::PROTOCOL_VERSION;

use crate::build::BuildInfo;
use crate::config::settings::PqKeyExchange;
use crate::filter::FilterSet;
use crate::fs::paths::escape_text;
use crate::index::Index;
use crate::location::Location;
use crate::options::{Direction, SyncOptions};
use crate::session::reconnect::{
    explain, never_reached, probe_path, run_with_reconnect, ConnState, NeverReached, ReconnectOpts,
};
use crate::session::{
    resolve_roots, Event, EventBus, LocalSide, Outcome, SessionSummary, SyncHello,
};
use crate::telemetry::meter::Usage;
use connect::client_state_dir;
pub use connect::{client_identity, client_key, connect, dial, use_state_dir, Connected};
use plan::{pull_plan, push_plan, Plan};

/// Partial files and sessions untouched for this long are collected: as
/// long as a daemon keeps them by default.
pub const CLIENT_GC_AGE: std::time::Duration = crate::config::settings::DORMANT_SESSION_TIMEOUT;
/// A partial record untouched this long makes the next run's GC walk due.
pub const CLIENT_GC_STALE: std::time::Duration = std::time::Duration::from_secs(3600);

/// What a sync is run on: the operands, read, and as typed (for messages).
#[derive(Debug, Clone)]
pub struct Operands {
    pub srcs: Vec<Location>,
    pub dest: Location,
    pub direction: Direction,
    pub typed_srcs: Vec<String>,
    pub typed_dest: String,
}

/// What all sessions of one invocation did together.
pub struct Totals {
    pub outcome: Outcome,
    pub sessions: usize,
    /// Sessions that ended in an error (their sources did not sync).
    pub failed: usize,
}

/// Runs a sync: one session per source, each reconnecting and resuming as
/// needed. A source that fails is reported through `say` (a line for the
/// user, above a live progress block) and the rest go on.
pub async fn sync(
    operands: &Operands,
    opts: &SyncOptions,
    bus: &EventBus,
    say: &dyn Fn(&str),
) -> anyhow::Result<Totals> {
    let Operands {
        srcs,
        dest,
        direction,
        typed_srcs,
        typed_dest,
    } = operands;
    let direction = *direction;
    let remote = match direction {
        Direction::ClientSends => dest,
        Direction::DaemonSends => &srcs[0],
    };
    let Location::Remote { host, .. } = remote else {
        bail!("internal: no remote location");
    };
    // The operand as typed, for the no-access hint.
    let remote_text: String = match direction {
        Direction::ClientSends => typed_dest.clone(),
        Direction::DaemonSends => typed_srcs[0].clone(),
    };

    let conn = connect(host, opts, say).await?;
    let filters = match FilterSet::from_rules(&conn.opts.filters).context("reading filter rules") {
        Ok(f) => Arc::new(f),
        Err(e) => {
            conn.endpoint.close().await;
            return Err(e);
        }
    };

    // One session per source; several sources only when pushing.
    let pairs: Vec<(&Location, &Location)> = srcs.iter().map(|s| (s, dest)).collect();
    let mut total = Outcome::default();
    let (mut sessions, mut failed) = (0, 0);
    let of = pairs.len();
    for (index, (src, dst)) in pairs.into_iter().enumerate() {
        let operand_from = typed_srcs[index].clone();
        let operand_to = typed_dest.clone();
        bus.emit(Event::SessionStart {
            index,
            of,
            from: operand_from.clone(),
            to: operand_to.clone(),
        });
        let (local, remote) = match direction {
            Direction::ClientSends => (src, dst),
            Direction::DaemonSends => (dst, src),
        };
        let (
            Location::Local {
                path,
                contents_only: local_co,
            },
            Location::Remote {
                path: rpath,
                contents_only: remote_co,
                ..
            },
        ) = (local, remote)
        else {
            bail!("internal: unexpected location kinds");
        };
        let (root, name) = resolve_roots(direction, path, *local_co, rpath, *remote_co);
        let dir_plan = Plan {
            root,
            local_contents_only: *local_co,
            // The receiver-side options are filled in from `opts` by the session.
            hello: SyncHello::new(
                direction,
                SyncRequest {
                    path: rpath.clone(),
                    name,
                    contents_only: *remote_co,
                    ..SyncRequest::default()
                },
            ),
            single: None,
        };
        let several = of > 1;
        let ropts = ReconnectOpts {
            max: conn.dial.reconnect,
            dial_retries: conn.dial.dial_retries,
            source_index: index,
            first_dial_timeout: conn.first_dial(),
            require_pq: conn.dial.pq_key_exchange == PqKeyExchange::Require,
            ..ReconnectOpts::default()
        };
        sessions += 1;
        let started = Instant::now();
        let result = {
            let res = async {
                let plan = match direction {
                    Direction::ClientSends => {
                        push_plan(dir_plan.clone(), path, *local_co, several)?
                    }
                    // With a trailing slash the remote path must be a directory.
                    Direction::DaemonSends if !*remote_co => {
                        let (endpoint, target) = (&conn.endpoint, conn.target.clone());
                        let timeout = conn.opts.idle_timeout;
                        let kind =
                            probe_path(endpoint, target, rpath, &remote_text, ropts, timeout)
                                .await?;
                        pull_plan(dir_plan.clone(), kind, path, *local_co, rpath)?
                    }
                    Direction::DaemonSends => dir_plan.clone(),
                };
                session(
                    &conn.endpoint,
                    conn.target.clone(),
                    &remote_text,
                    direction,
                    plan,
                    &filters,
                    &conn.opts,
                    ropts,
                    bus,
                )
                .await
            }
            .await;
            // A server that was never reached (a refusal is final).
            match res {
                Err(e) if e.downcast_ref::<NeverReached>().is_some() => {
                    Err(never_reached(e, conn.paired()))
                }
                res => res,
            }
        };
        let mut summary = SessionSummary {
            index,
            of,
            verb: match direction {
                Direction::ClientSends => "push",
                Direction::DaemonSends => "pull",
            },
            from: operand_from,
            to: operand_to,
            reconnects: 0,
            conn: None,
            key_exchange: None,
            usage: Usage::default(),
            files_transferred: 0,
            files_hashed: 0,
            bytes_sent: 0,
            bytes_received: 0,
            failed: false,
        };
        let out = match result {
            Ok(out) => {
                summary.reconnects = out.reconnects;
                summary.conn = out.conn;
                summary.key_exchange = out.key_exchange.clone();
                summary.usage = out.usage.clone();
                summary.files_transferred = out.files_transferred;
                summary.files_hashed = out.files_hashed;
                summary.bytes_sent = out.bytes_sent;
                summary.bytes_received = out.bytes_received;
                bus.emit(Event::SessionEnd(Arc::new(summary)));
                out
            }
            Err(e) => {
                summary.usage.wall = started.elapsed();
                summary.failed = true;
                bus.emit(Event::SessionEnd(Arc::new(summary)));
                // Keep what earlier sources achieved; report this one and go on.
                // The error may carry the daemon's own words.
                let e = escape_text(&format!("{e:#}"));
                say(&format!("irsync: {}: {e}", path.display()));
                failed += 1;
                continue;
            }
        };
        total.absorb(out);
    }
    conn.endpoint.close().await;
    Ok(Totals {
        outcome: total,
        sessions,
        failed,
    })
}

/// One source: open the local index, then run the session, reconnecting and
/// resuming as needed.
#[allow(clippy::too_many_arguments)]
async fn session(
    endpoint: &iroh::Endpoint,
    target: EndpointAddr,
    remote: &str,
    direction: Direction,
    plan: Plan,
    filters: &Arc<FilterSet>,
    opts: &SyncOptions,
    ropts: ReconnectOpts,
    bus: &EventBus,
) -> anyhow::Result<Outcome> {
    let Plan {
        root,
        local_contents_only,
        hello,
        single,
    } = plan;
    let single_opts;
    let opts = match single {
        Some(_) => {
            single_opts = opts.without_deletion();
            &single_opts
        }
        None => opts,
    };
    let dry_run = opts.dry_run;
    let state = client_state_dir(opts);
    let index = tokio::task::spawn_blocking({
        let root = root.clone();
        move || -> anyhow::Result<_> {
            // A dry run creates nothing (the receiver lists a missing root as empty).
            if direction == Direction::DaemonSends && !dry_run {
                std::fs::create_dir_all(&root)
                    .with_context(|| format!("creating {}", root.display()))?;
            }
            Ok(Index::open(&state, &root)
                .with_context(|| format!("opening index for {}", root.display()))?
                .into_handle())
        }
    })
    .await??;
    // A single file's root is a directory that holds other things: no
    // housekeeping there.
    if !dry_run && single.is_none() {
        // Housekeeping for this root, at most daily unless partial records
        // have gone stale (an hour untouched) since; never fatal.
        match crate::gc::gc_if_due(index.clone(), &root, CLIENT_GC_AGE, CLIENT_GC_STALE).await {
            Ok(Some(r)) => tracing::debug!(
                parts_removed = r.parts_removed.len(),
                sessions_removed = r.sessions_removed,
                entries_removed = r.entries_removed,
                "gc finished"
            ),
            Ok(None) => tracing::debug!("gc not due"),
            Err(e) => tracing::debug!("gc failed: {e}"),
        }
    }
    let mut side = LocalSide::new(root, local_contents_only, index, filters.clone());
    if let Some(name) = single {
        side = side.with_single(name);
    }
    side.bus = bus.clone();
    #[cfg(feature = "test-hooks")]
    {
        side.hooks.no_change_checks = crate::session::no_change_checks_from_env();
    }
    let hello_for = |id: Option<[u8; 16]>| {
        let mut h = hello.clone();
        h.req.session_id = id.map(|i| i.to_vec()).unwrap_or_default();
        h
    };
    let target_id = target.id;
    let on_state = |s: ConnState| match s {
        ConnState::Connecting => tracing::info!(target = %target_id, "connecting"),
        ConnState::Direct => tracing::info!("connected directly"),
        ConnState::Relay => tracing::info!("connected through a relay"),
        ConnState::Reconnecting { attempt } => {
            tracing::info!(attempt, "reconnecting")
        }
    };
    run_with_reconnect(
        endpoint, target, remote, hello_for, side, opts, ropts, on_state,
    )
    .await
}

/// The exports this client may reach on the server `operand` (`HOST:`)
/// names. `say` is as for [`sync`].
pub async fn list_exports(
    operand: &str,
    opts: &SyncOptions,
    say: &dyn Fn(&str),
) -> anyhow::Result<Vec<ExportEntry>> {
    let Location::Remote { host, .. } = Location::parse(operand)? else {
        bail!("internal: a listing takes a remote operand");
    };
    let c = connect(&host, opts, say).await?;
    // Closed on every way out: an endpoint dropped open makes iroh log an
    // error line before irsync's own message (#256).
    let listed = list_exports_over(&c, operand).await;
    c.endpoint.close().await;
    listed
}

async fn list_exports_over(c: &Connected, operand: &str) -> anyhow::Result<Vec<ExportEntry>> {
    let hello = Hello {
        protocol_version: PROTOCOL_VERSION,
        build: Some(BuildInfo::current().to_wire()),
        request: Some(hello::Request::ListExports(ListExports {})),
        ..Hello::default()
    };
    let client = c.endpoint.id();
    let conn = dial(c).await?;
    let entries = crate::session::engine::list_exports(&conn, hello)
        .await
        .map_err(|e| explain(e, Some(&conn), operand, &client))?;
    conn.close(0u32.into(), b"done");
    Ok(entries)
}
