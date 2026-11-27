//! `irsync remote SERVER COMMAND`: the client side of the admin service.
//!
//! Each call dials the server, sends an admin Hello and then one change and
//! a commit, or one read, and prints what the same command prints on the
//! server. It never sets a server up: the server must be saved or named by
//! its EndpointId, and its daemon must be running.

use std::path::Path;
use std::time::Duration;

use anyhow::{bail, Context};
use irsync::authz::Access;
use irsync::cli::{ClientCmd, GrantCmd, RemoteCmd, RemoteConfigCmd, RemoteExportCmd};
use irsync::config::clients::{Clients, RIGHTS};
use irsync::config::file::Config;
use irsync::config::servers::Servers;
use irsync::fs::paths::escape_text;
use irsync::manage::{self, ClientSpec};
use irsync::net::frame::{read_body, write_msg_tapped};
use irsync::net::tap::{ConnTap, StreamKind, StreamTap};
use irsync::session::engine::{read_hello_reply, SessionRefused};
use irsync_proto::config as pb;
use irsync_proto::v1::{
    admin_reply, admin_request, change::Op, error::Code, hello, hello_reply, read, AdminReply,
    AdminRequest, Change, ClientAdd, ClientRemove, ClientRename, ClientSet, Commit, ExportAdd,
    ExportRemove, ExportRename, ExportSet, GrantAdd, GrantRemove, IndexRun, OpenAdmin, Read,
    ReadConfig, ReadIndexing, ReadView, StartIndex, View,
};
use irsync_proto::PROTOCOL_VERSION;
use prost::Message;

use irsync::client::Connected;

/// How long [`converse`] waits for each answer of the server.
const REPLY_WAIT: Duration = Duration::from_secs(60);

/// How often `remote index NAME` asks the server how its run stands.
const POLL: Duration = Duration::from_secs(1);

/// The wait of `remote index NAME` was interrupted (Ctrl-C); the run goes
/// on on the server.
#[derive(Debug)]
pub struct Interrupted;

impl std::fmt::Display for Interrupted {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("interrupted")
    }
}

impl std::error::Error for Interrupted {}

/// What one call asks the server.
pub enum RemoteRequest {
    /// One change, then a commit.
    Change(Change),
    /// The exports, the clients and the daemon's facts.
    View,
    /// `config get KEY`, or with an empty key `config list`.
    Config(String),
    /// Start indexing an export, then wait until the run ends.
    StartIndex(String),
    /// The indexing runs.
    Indexing,
}

/// What the server answered.
pub enum RemoteOutcome {
    /// The change was applied: the lines the local command prints to stderr.
    Applied(Vec<String>),
    View(View),
    Config(Vec<String>),
    /// The run asked for, as it ended.
    Indexed(IndexRun),
    Indexing(Vec<IndexRun>),
}

/// How [`print`] shows the answer.
pub enum Shape {
    Applied,
    ExportList,
    GrantList(Option<String>),
    ClientList,
    Show,
    ConfigGet,
    ConfigList,
    Indexed,
    IndexList,
}

pub struct Plan {
    pub request: RemoteRequest,
    pub shape: Shape,
}

/// The request `cmd` stands for, checked as far as the client can before it
/// dials. `server` is the name as typed, for messages.
pub fn plan(server: &str, cmd: &RemoteCmd) -> anyhow::Result<Plan> {
    let change = |op| Plan {
        request: RemoteRequest::Change(Change { op: Some(op) }),
        shape: Shape::Applied,
    };
    let view = |shape| Plan {
        request: RemoteRequest::View,
        shape,
    };
    Ok(match cmd {
        RemoteCmd::Export(e) => match e.cmd.as_ref().unwrap_or(&RemoteExportCmd::List) {
            RemoteExportCmd::Add(a) => {
                refuse_expanded_home(
                    server,
                    &a.target,
                    irsync::config::paths::home_dir().as_deref(),
                )?;
                change(Op::ExportAdd(ExportAdd {
                    target: a.target.clone(),
                    read_only: a.read_only,
                    index_at_start: a.index_at_start,
                }))
            }
            RemoteExportCmd::Set { name, settings } => change(Op::ExportSet(ExportSet {
                name: name.clone(),
                settings: settings.clone(),
            })),
            RemoteExportCmd::Rename { old, new } => change(Op::ExportRename(ExportRename {
                old: old.clone(),
                new: new.clone(),
            })),
            RemoteExportCmd::Remove { name } => {
                change(Op::ExportRemove(ExportRemove { name: name.clone() }))
            }
            RemoteExportCmd::List => view(Shape::ExportList),
        },
        RemoteCmd::Grant(g) => match g.cmd.as_ref().unwrap_or(&GrantCmd::List { target: None }) {
            GrantCmd::Add { export, who, ro } => change(Op::GrantAdd(GrantAdd {
                export: export.clone(),
                who: key_line_of(who).unwrap_or_else(|| who.clone()),
                ro: *ro,
            })),
            GrantCmd::Remove { export, who } => change(Op::GrantRemove(GrantRemove {
                export: export.clone(),
                who: key_line_of(who).unwrap_or_else(|| who.clone()),
            })),
            GrantCmd::List { target } => view(Shape::GrantList(target.clone())),
        },
        RemoteCmd::Client(c) => match c.cmd.as_ref().unwrap_or(&ClientCmd::List) {
            ClientCmd::Add { name, id } => change(Op::ClientAdd(ClientAdd {
                name: name.clone(),
                id: client_id_of(id)?.as_bytes().to_vec(),
            })),
            ClientCmd::Remove { name, force } => change(Op::ClientRemove(ClientRemove {
                name: name.clone(),
                force: *force,
            })),
            ClientCmd::Rename { old, new } => change(Op::ClientRename(ClientRename {
                old: old.clone(),
                new: new.clone(),
            })),
            ClientCmd::Set { name, settings } => change(Op::ClientSet(ClientSet {
                name: name.clone(),
                settings: settings.clone(),
            })),
            ClientCmd::List => view(Shape::ClientList),
        },
        RemoteCmd::Show => view(Shape::Show),
        RemoteCmd::Config(c) => match c.cmd.as_ref().unwrap_or(&RemoteConfigCmd::List) {
            RemoteConfigCmd::Get { key } => {
                if key.is_empty() {
                    bail!("config get needs a key");
                }
                Plan {
                    request: RemoteRequest::Config(key.clone()),
                    shape: Shape::ConfigGet,
                }
            }
            RemoteConfigCmd::List => Plan {
                request: RemoteRequest::Config(String::new()),
                shape: Shape::ConfigList,
            },
        },
        RemoteCmd::Index { export: Some(name) } => Plan {
            request: RemoteRequest::StartIndex(name.clone()),
            shape: Shape::Indexed,
        },
        RemoteCmd::Index { export: None } => Plan {
            request: RemoteRequest::Indexing,
            shape: Shape::IndexList,
        },
    })
}

/// An `export add` path that starts with this machine's home directory is
/// the shell's doing: it expanded an unquoted `~`. The server would take it
/// as a path of its own, so it is refused before anything is dialled.
fn refuse_expanded_home(server: &str, target: &str, home: Option<&Path>) -> anyhow::Result<()> {
    let (_, raw) = manage::exports::split_target(target);
    let Some(home) = home.filter(|h| h.parent().is_some()) else {
        return Ok(());
    };
    let Ok(rest) = Path::new(raw).strip_prefix(home) else {
        return Ok(());
    };
    let tilde = if rest.as_os_str().is_empty() {
        "~".to_string()
    } else {
        format!("~/{}", rest.display())
    };
    let name = manage::exports::target_name(target);
    bail!(
        "{raw} looks like your own home: the shell expanded ~. Quote it: \
         irsync remote {server} export add '{name}:{tilde}'"
    )
}

/// The first line of the `.pub` file `who` names, or `who` itself when it
/// is a public key line: what the server reads as a key. `None` for a
/// client name or an EndpointId, which the server resolves itself. Only the
/// key line leaves this machine, never the file's other contents.
fn key_line_of(who: &str) -> Option<String> {
    let line = match std::fs::read_to_string(who) {
        Ok(text) => text
            .lines()
            .map(str::trim)
            .find(|l| !l.is_empty())?
            .to_string(),
        Err(_) => who.trim().to_string(),
    };
    ssh_key::PublicKey::from_openssh(&line).ok()?;
    Some(line)
}

/// The id of `client add NAME ID`: an EndpointId, a public key line or a
/// `.pub` file, read here.
fn client_id_of(id: &str) -> anyhow::Result<iroh::EndpointId> {
    let empty = Path::new("");
    let clients = Clients::from_message(&[], empty)?;
    let servers = Servers::from_message(&[], empty)?;
    match ClientSpec::parse(id, &clients, &servers) {
        Ok(ClientSpec::Id { id, .. }) => Ok(id),
        _ => Err(irsync::identity::parse_endpoint_id(id)
            .map_or_else(anyhow::Error::from, |_| anyhow::anyhow!("not an ID"))
            .context(format!("ID {id:?}"))),
    }
}

/// Sends `req` to the server of `c` and returns its answer. A refusal, a
/// NACK or a failure is the error.
pub async fn run_remote(c: &Connected, req: RemoteRequest) -> anyhow::Result<RemoteOutcome> {
    let server = c.name.clone().unwrap_or_else(|| c.target.id.to_string());
    let client = c.endpoint.id();
    let conn = irsync::client::dial(c).await?;
    let answer = converse(&conn, &server, req).await;
    let answer = answer.map_err(|e| {
        irsync::session::reconnect::explain(e, Some(&conn), &format!("{server}:"), &client)
    });
    conn.close(0u32.into(), b"done");
    answer
}

async fn converse(
    conn: &iroh::endpoint::Connection,
    server: &str,
    req: RemoteRequest,
) -> anyhow::Result<RemoteOutcome> {
    let (mut send, mut recv) = conn.open_bi().await.context("open control stream")?;
    let tap = ConnTap::of(conn).stream(StreamKind::Control, u64::from(send.id()));
    let hello = irsync::session::hello(hello::Request::Admin(OpenAdmin {}));
    let read_of = |what| admin_request::Msg::Read(Read { what: Some(what) });
    let (expect, requests) = match req {
        RemoteRequest::Change(change) => (
            Expect::Ack,
            vec![
                admin_request::Msg::Change(change),
                admin_request::Msg::Commit(Commit {}),
            ],
        ),
        RemoteRequest::View => (Expect::View, vec![read_of(read::What::View(ReadView {}))]),
        RemoteRequest::Config(key) => (
            Expect::Config,
            vec![read_of(read::What::Config(ReadConfig { key }))],
        ),
        RemoteRequest::StartIndex(export) => (
            Expect::IndexStarted,
            vec![admin_request::Msg::StartIndex(StartIndex { export })],
        ),
        RemoteRequest::Indexing => (
            Expect::Indexing,
            vec![read_of(read::What::Indexing(ReadIndexing {}))],
        ),
    };
    // A wait polls on the same stream: it stays open for writing.
    let keep_open = matches!(expect, Expect::IndexStarted);
    // A daemon that refuses answers without waiting for the requests, and
    // may be gone when they are written: the answer, not the write, says why.
    let sent = async {
        write_msg_tapped(&mut send, &hello, &tap).await?;
        for msg in requests {
            write_msg_tapped(&mut send, &AdminRequest { msg: Some(msg) }, &tap).await?;
        }
        if !keep_open {
            send.finish()?;
            tap.finished();
        }
        Ok::<(), anyhow::Error>(())
    }
    .await;
    let Ok(read) = tokio::time::timeout(REPLY_WAIT, read_answer(&mut recv, &tap)).await else {
        bail!(
            "{server} did not answer within {} seconds",
            REPLY_WAIT.as_secs()
        );
    };
    let body = match read {
        Ok(Some(body)) => body,
        Err(e) if e.is::<SessionRefused>() => {
            return Err(refusal(server, e.downcast().expect("checked above")));
        }
        read => {
            sent.context("sending the request")?;
            match read {
                Err(e) => return Err(e).context("reading the reply"),
                _ => bail!("the server closed the stream without an answer"),
            }
        }
    };
    Ok(match decode_reply(server, &body, &expect, &tap)? {
        admin_reply::Msg::Ack(ack) => RemoteOutcome::Applied(ack.results),
        admin_reply::Msg::View(v) => RemoteOutcome::View(v),
        admin_reply::Msg::Config(t) => RemoteOutcome::Config(t.lines),
        admin_reply::Msg::Indexing(i) => RemoteOutcome::Indexing(i.runs),
        admin_reply::Msg::IndexStarted(s) => {
            let mine = s
                .run
                .with_context(|| format!("{server} did not say which run it started"))?;
            let done = wait_for(server, &mut send, &mut recv, &tap, mine).await;
            let _ = send.finish();
            tap.finished();
            RemoteOutcome::Indexed(done?)
        }
        _ => bail!("{server} sent an answer that does not fit the request"),
    })
}

/// Asks every [`POLL`] how the run `mine` stands, until it has ended
/// (see [`wait_over`]); the run as it ended.
async fn wait_for(
    server: &str,
    send: &mut iroh::endpoint::SendStream,
    recv: &mut iroh::endpoint::RecvStream,
    tap: &StreamTap,
    mine: IndexRun,
) -> anyhow::Result<IndexRun> {
    let mut last = mine;
    if last.ended_at > 0 {
        return Ok(last);
    }
    loop {
        tokio::time::sleep(POLL).await;
        let ask = admin_request::Msg::Read(Read {
            what: Some(read::What::Indexing(ReadIndexing {})),
        });
        write_msg_tapped(send, &AdminRequest { msg: Some(ask) }, tap)
            .await
            .context("asking how the run stands")?;
        let Ok(body) = tokio::time::timeout(REPLY_WAIT, read_body(recv)).await else {
            bail!(
                "{server} did not answer within {} seconds",
                REPLY_WAIT.as_secs()
            );
        };
        let Some(body) = body.context("reading the reply")? else {
            bail!("the server closed the stream without an answer");
        };
        let admin_reply::Msg::Indexing(i) = decode_reply(server, &body, &Expect::Indexing, tap)?
        else {
            bail!("{server} sent an answer that does not fit the request");
        };
        if let Some(done) = wait_over(&last, &i.runs) {
            return Ok(done);
        }
        if let Some(r) = i
            .runs
            .iter()
            .find(|r| r.path == last.path && r.started_at == last.started_at)
        {
            last = r.clone();
        }
    }
}

/// Whether the wait for the run `mine` is over, from the runs the server
/// listed: the run as it ended, or, when the path shows another run (or
/// none), `mine` as last seen. `None` while it goes on.
fn wait_over(mine: &IndexRun, runs: &[IndexRun]) -> Option<IndexRun> {
    match runs.iter().find(|r| r.path == mine.path) {
        Some(r) if r.started_at == mine.started_at => (r.ended_at > 0).then(|| r.clone()),
        _ => Some(mine.clone()),
    }
}

/// Which reply a request wants.
enum Expect {
    Ack,
    View,
    Config,
    IndexStarted,
    Indexing,
}

/// The answer to the request: the daemon's reply to Hello, which must open
/// the admin session, then the body of its first `AdminReply`; `None` when
/// the stream ends before it. A refusal of the Hello is a [`SessionRefused`].
async fn read_answer(
    recv: &mut iroh::endpoint::RecvStream,
    tap: &StreamTap,
) -> anyhow::Result<Option<Vec<u8>>> {
    match read_hello_reply(recv, tap, None).await? {
        hello_reply::Reply::AdminReady(_) => {}
        _ => bail!("the reply to Hello does not open an admin session"),
    }
    Ok(read_body(recv).await?)
}

/// The reply `body` to a request that wants `expect`. A NACK or a refusal
/// is the error. The reply is reported to `tap` as received.
fn decode_reply(
    server: &str,
    body: &[u8],
    expect: &Expect,
    tap: &StreamTap,
) -> anyhow::Result<admin_reply::Msg> {
    let msg = match AdminReply::decode(body) {
        Ok(AdminReply { msg: Some(msg) }) => {
            tap.received(
                &AdminReply {
                    msg: Some(msg.clone()),
                },
                body.len(),
            );
            msg
        }
        _ => {
            tap.reset("malformed reply");
            bail!("malformed reply from {server}")
        }
    };
    use admin_reply::Msg as M;
    match (msg, expect) {
        (M::Nack(n), Expect::Ack) => bail!("{}", n.reason),
        (M::Refused(e), _) => Err(refusal(
            server,
            SessionRefused {
                code: e.code(),
                message: e.message,
            },
        )),
        (m @ M::Ack(_), Expect::Ack)
        | (m @ M::View(_), Expect::View)
        | (m @ M::Config(_), Expect::Config)
        | (m @ M::IndexStarted(_), Expect::IndexStarted)
        | (m @ M::Indexing(_), Expect::Indexing) => Ok(m),
        _ => bail!("{server} sent an answer that does not fit the request"),
    }
}

/// The error for a refusal: the plain words for the two the user can act on,
/// the daemon's own text otherwise.
fn refusal(server: &str, e: SessionRefused) -> anyhow::Error {
    match e.code {
        Code::Unauthorized => anyhow::anyhow!(
            "{server} does not let this client manage it \
             (it needs admin = \"rw\" or \"ro\" in its [clients] entry)"
        ),
        Code::VersionMismatch => match daemon_version(&e.message) {
            Some(n) if n < PROTOCOL_VERSION => {
                anyhow::anyhow!("{server} runs an older irsync: update it there")
            }
            _ => e.into(),
        },
        _ => anyhow::anyhow!("{}", e.message),
    }
}

/// N of "daemon speaks N" in a version refusal.
fn daemon_version(message: &str) -> Option<u32> {
    let rest = message.split("daemon speaks ").nth(1)?;
    let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
    digits.parse().ok()
}

/// A [`pb::Config`] with what `v` holds, so the printers of the local
/// commands run on it unchanged.
pub fn view_to_config(v: &View) -> pb::Config {
    let entries = |values: &[String]| {
        values
            .iter()
            .map(|value| pb::Entry {
                value: value.clone(),
                ..pb::Entry::default()
            })
            .collect()
    };
    pb::Config {
        exports: v
            .exports
            .iter()
            .map(|e| pb::Export {
                name: e.name.clone(),
                path: e.path.clone(),
                read_only: e.read_only.then_some(true),
                index_at_start: e.index_at_start.then_some(true),
                fsync: e.fsync.then_some(true),
                auto: e.is_auto.then_some(true),
                rw: entries(&e.rw),
                rw_set: !e.rw.is_empty(),
                ro: entries(&e.ro),
                ro_set: !e.ro.is_empty(),
                ..pb::Export::default()
            })
            .collect(),
        clients: v
            .clients
            .iter()
            .map(|c| {
                let mut entry = pb::ClientEntry {
                    name: c.name.clone(),
                    client_id: c.id.clone(),
                    ..pb::ClientEntry::default()
                };
                for right in RIGHTS {
                    if let Some(a) = c.rights.get(right.key()).and_then(|s| Access::parse(s)) {
                        right.set(&mut entry, Some(a));
                    }
                }
                entry
            })
            .collect(),
        ..pb::Config::default()
    }
}

/// One line of `remote index` with no name: how a run stands.
fn index_line(r: &IndexRun) -> String {
    let state = if r.ended_at == 0 {
        "running".to_string()
    } else if !r.error.is_empty() {
        format!("failed: {}", r.error)
    } else if r.incomplete {
        "incomplete".to_string()
    } else {
        "done".to_string()
    };
    let failed = if r.failed > 0 {
        format!(", {} failed", r.failed)
    } else {
        String::new()
    };
    let by = if r.at_start {
        " (index_at_start)".to_string()
    } else {
        format!(" (by {})", r.started_by)
    };
    format!(
        "{}: {}: {state}, {} file(s), {} hashed ({} bytes){failed}{by}",
        r.export, r.path, r.examined, r.hashed, r.bytes
    )
}

/// Prints the end of the run `r` on `server` as local `irsync index`
/// prints it, without the files that failed: the server's log names them.
/// The exit code: 1 when the run failed, 23 when files failed or the walk
/// was incomplete.
fn print_indexed(server: &str, r: &IndexRun, quiet: bool) -> i32 {
    let path = escape_text(&r.path);
    if !r.error.is_empty() {
        eprintln!(
            "irsync: indexing {path} on {server} failed: {}",
            escape_text(&r.error)
        );
        return 1;
    }
    if !quiet {
        println!(
            "{path}: {} file(s), {} hashed ({} bytes)",
            r.examined, r.hashed, r.bytes
        );
    }
    let mut code = 0;
    if r.failed > 0 {
        eprintln!(
            "irsync: {} file(s) could not be read; the log on {server} names them",
            r.failed
        );
        code = 23;
    }
    if r.incomplete {
        eprintln!("irsync: {server} could not list everything under {path}");
        code = 23;
    }
    code
}

/// Prints `outcome` the way the local command does; the exit code.
pub fn print(
    shape: &Shape,
    server: &str,
    outcome: RemoteOutcome,
    quiet: bool,
) -> anyhow::Result<i32> {
    match outcome {
        // What a change did: a status line, which `-q` leaves out.
        RemoteOutcome::Applied(_) if quiet => {}
        RemoteOutcome::Applied(results) => {
            for line in results.iter().flat_map(|r| r.split('\n')) {
                eprintln!("{}", escape_text(line));
            }
        }
        RemoteOutcome::Indexed(run) => return Ok(print_indexed(server, &run, quiet)),
        RemoteOutcome::Indexing(_) if quiet => {}
        RemoteOutcome::Indexing(runs) => {
            for run in &runs {
                println!("{}", escape_text(&index_line(run)));
            }
        }
        RemoteOutcome::Config(lines) => {
            if matches!(shape, Shape::ConfigGet) && lines.is_empty() {
                return Ok(1);
            }
            for line in lines {
                println!("{}", escape_text(&line));
            }
        }
        RemoteOutcome::View(v) => {
            let msg = view_to_config(&v);
            let config = Config::from_message(&msg, Path::new(server))
                .with_context(|| format!("the answer of {server}"))?;
            match shape {
                Shape::ExportList => {
                    print!(
                        "{}",
                        manage::show::export_blocks(&config.exports, &config.clients)
                    );
                }
                Shape::GrantList(target) => print!(
                    "{}",
                    crate::grant_list_text(
                        &config.exports,
                        &config.clients,
                        v.allow_all_paths,
                        target.as_deref(),
                        &format!("the config of {server}"),
                    )?
                ),
                Shape::ClientList => {
                    for line in manage::clients::client_list(&msg) {
                        println!("{}", escape_text(&line));
                    }
                }
                Shape::Show => {
                    match irsync::manage::id_of(&v.daemon_id) {
                        Some(id) => println!("daemon identity: {id}"),
                        None => println!("daemon identity: (unknown)"),
                    }
                    println!("clients:");
                    for line in manage::clients::client_list(&msg) {
                        println!("{}", escape_text(&line));
                    }
                    println!("exports:");
                    print!(
                        "{}",
                        manage::show::export_blocks(&config.exports, &config.clients)
                    );
                }
                _ => bail!("{server} sent an answer that does not fit the request"),
            }
        }
    }
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(path: &str) -> IndexRun {
        IndexRun {
            export: "photos".into(),
            path: path.into(),
            started_by: "laptop".into(),
            started_at: 100,
            examined: 4,
            hashed: 3,
            bytes: 30,
            ..IndexRun::default()
        }
    }

    #[test]
    fn an_index_line_says_how_the_run_stands() {
        let running = run("/srv/photos");
        assert_eq!(
            index_line(&running),
            "photos: /srv/photos: running, 4 file(s), 3 hashed (30 bytes) (by laptop)"
        );
        let done = IndexRun {
            ended_at: 200,
            failed: 2,
            ..run("/srv/photos")
        };
        assert_eq!(
            index_line(&done),
            "photos: /srv/photos: done, 4 file(s), 3 hashed (30 bytes), 2 failed (by laptop)"
        );
        let failed = IndexRun {
            ended_at: 200,
            error: "gone".into(),
            ..run("/srv/photos")
        };
        assert_eq!(
            index_line(&failed),
            "photos: /srv/photos: failed: gone, 4 file(s), 3 hashed (30 bytes) (by laptop)"
        );
        let at_start = IndexRun {
            ended_at: 200,
            incomplete: true,
            at_start: true,
            started_by: String::new(),
            ..run("/srv/photos")
        };
        assert_eq!(
            index_line(&at_start),
            "photos: /srv/photos: incomplete, 4 file(s), 3 hashed (30 bytes) (index_at_start)"
        );
    }

    #[test]
    fn a_replaced_run_ends_the_wait() {
        let mine = run("/srv/photos");
        // The same run, still going: wait on.
        assert_eq!(wait_over(&mine, &[run("/srv/a"), mine.clone()]), None);
        // The same run, ended: its last state.
        let ended = IndexRun {
            ended_at: 150,
            hashed: 4,
            ..mine.clone()
        };
        assert_eq!(wait_over(&mine, std::slice::from_ref(&ended)), Some(ended));
        // Another run of the path, started later: the wait is over, with
        // the last state seen of ours.
        let later = IndexRun {
            started_at: 160,
            started_by: "other".into(),
            ..run("/srv/photos")
        };
        assert_eq!(wait_over(&mine, &[later]), Some(mine.clone()));
    }

    fn version_error(n: u32) -> SessionRefused {
        SessionRefused {
            code: Code::VersionMismatch,
            message: format!("protocol version {PROTOCOL_VERSION} unsupported (daemon speaks {n})"),
        }
    }

    #[test]
    fn an_older_daemon_is_told_to_update() {
        let e = refusal("myserver", version_error(PROTOCOL_VERSION - 1));
        assert_eq!(
            e.to_string(),
            "myserver runs an older irsync: update it there"
        );
    }

    /// A refusal inside the admin session is read as one, for every request.
    #[test]
    fn a_refused_request_decodes_to_its_refusal() {
        for expect in [
            Expect::Ack,
            Expect::View,
            Expect::Config,
            Expect::IndexStarted,
            Expect::Indexing,
        ] {
            let body = AdminReply {
                msg: Some(admin_reply::Msg::Refused(irsync_proto::v1::Error {
                    code: Code::Unauthorized as i32,
                    message: "not allowed".into(),
                })),
            }
            .encode_to_vec();
            let Err(e) = decode_reply("myserver", &body, &expect, &StreamTap::off()) else {
                panic!("a refusal decoded as an answer");
            };
            assert!(
                e.to_string()
                    .starts_with("myserver does not let this client manage it"),
                "{e:#}"
            );
        }
    }

    #[test]
    fn another_version_mismatch_keeps_the_usual_message() {
        let e = refusal("myserver", version_error(PROTOCOL_VERSION + 1));
        assert!(e.downcast_ref::<SessionRefused>().is_some(), "{e:#}");
    }

    #[test]
    fn a_plain_refusal_has_no_hint_of_the_server() {
        let e = SessionRefused {
            code: Code::Unauthorized,
            message: "not allowed".to_string(),
        };
        assert!(refusal("myserver", e)
            .to_string()
            .starts_with("myserver does not let this client manage it"));
    }

    #[test]
    fn a_view_becomes_a_config_the_printers_read() {
        let v = View {
            exports: vec![irsync_proto::v1::ExportView {
                name: "photos".into(),
                path: "/srv/photos".into(),
                read_only: true,
                rw: vec!["bob".into()],
                ..Default::default()
            }],
            clients: vec![irsync_proto::v1::ClientView {
                name: "bob".into(),
                id: iroh::SecretKey::from_bytes(&[3; 32])
                    .public()
                    .as_bytes()
                    .to_vec(),
                rights: [("admin".to_string(), "ro".to_string())].into(),
            }],
            ..Default::default()
        };
        let msg = view_to_config(&v);
        let config = Config::from_message(&msg, Path::new("server")).unwrap();
        let text = manage::show::export_blocks(&config.exports, &config.clients);
        assert!(text.contains("photos: /srv/photos"), "{text}");
        assert!(text.contains("read_only"), "{text}");
        assert!(manage::clients::client_list(&msg)[0].contains("admin=ro"));
    }

    #[test]
    fn a_path_under_the_local_home_is_the_shells_doing() {
        let home = Path::new("/home/me");
        let e = refuse_expanded_home("myserver", "photos:/home/me/Photos", Some(home)).unwrap_err();
        assert_eq!(
            e.to_string(),
            "/home/me/Photos looks like your own home: the shell expanded ~. Quote it: \
             irsync remote myserver export add 'photos:~/Photos'"
        );
        assert!(refuse_expanded_home("s", "photos:~/Photos", Some(home)).is_ok());
        assert!(refuse_expanded_home("s", "photos:/srv/Photos", Some(home)).is_ok());
        assert!(refuse_expanded_home("s", "photos:/home/mee/Photos", Some(home)).is_ok());
    }
}
