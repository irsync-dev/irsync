//! `irsync remote SERVER COMMAND`: the client side of the admin service.
//!
//! Each call dials the server, sends an ADMIN Hello and then one change and
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
use irsync::manage::{self, PeerSpec};
use irsync::net::frame::{read_body, write_msg_tapped};
use irsync::net::tap::{ConnTap, StreamKind, StreamTap};
use irsync::session::engine::SessionRefused;
use irsync_proto::config as pb;
use irsync_proto::v1::{
    admin_reply, admin_request, change::Op, error::Code, hello::Direction, read, AdminReply,
    AdminRequest, Change, ClientAdd, ClientRemove, ClientRename, ClientSet, Commit, Error,
    ExportAdd, ExportRemove, GrantAdd, GrantRemove, Hello, Read, ReadConfig, ReadView, View,
};
use irsync_proto::PROTOCOL_VERSION;
use prost::Message;

use crate::Connected;

/// How long [`converse`] waits for the server's answer.
const REPLY_WAIT: Duration = Duration::from_secs(60);

/// What one call asks the server.
pub enum RemoteRequest {
    /// One change, then a commit.
    Change(Change),
    /// The exports, the clients and the daemon's facts.
    View,
    /// `config get KEY`, or with an empty key `config list`.
    Config(String),
}

/// What the server answered.
pub enum RemoteOutcome {
    /// The change was applied: the lines the local command prints to stderr.
    Applied(Vec<String>),
    View(View),
    Config(Vec<String>),
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
                refuse_expanded_home(server, &a.target, dirs::home_dir().as_deref())?;
                change(Op::ExportAdd(ExportAdd {
                    target: a.target.clone(),
                    read_only: a.ro,
                    warm_index: a.warm_index,
                }))
            }
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
    match PeerSpec::parse(id, &clients, &servers) {
        Ok(PeerSpec::Id { id, .. }) => Ok(id),
        _ => Err(irsync::identity::parse_endpoint_id(id)
            .map_or_else(anyhow::Error::from, |_| anyhow::anyhow!("not an ID"))
            .context(format!("ID {id:?}"))),
    }
}

/// Sends `req` to the server of `c` and returns its answer. A refusal, a
/// NACK or a failure is the error.
pub async fn run_remote(c: &Connected, req: RemoteRequest) -> anyhow::Result<RemoteOutcome> {
    let server = c.alias.clone().unwrap_or_else(|| c.target.id.to_string());
    let client = c.endpoint.id();
    let conn = crate::dial(c).await?;
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
    let hello = Hello {
        protocol_version: PROTOCOL_VERSION,
        direction: Direction::Admin as i32,
        ..Hello::default()
    };
    let (what, requests) = match req {
        RemoteRequest::Change(change) => (
            None,
            vec![
                admin_request::Msg::Change(change),
                admin_request::Msg::Commit(Commit {}),
            ],
        ),
        RemoteRequest::View => (
            Some(false),
            vec![admin_request::Msg::Read(Read {
                what: Some(read::What::View(ReadView {})),
            })],
        ),
        RemoteRequest::Config(key) => (
            Some(true),
            vec![admin_request::Msg::Read(Read {
                what: Some(read::What::Config(ReadConfig { key })),
            })],
        ),
    };
    // A daemon that refuses answers without waiting for the requests, and
    // may be gone when they are written: the answer, not the write, says why.
    let sent = async {
        write_msg_tapped(&mut send, &hello, &tap).await?;
        for msg in requests {
            write_msg_tapped(&mut send, &AdminRequest { msg: Some(msg) }, &tap).await?;
        }
        send.finish()?;
        tap.finished();
        Ok::<(), anyhow::Error>(())
    }
    .await;
    let Ok(read) = tokio::time::timeout(REPLY_WAIT, read_body(&mut recv)).await else {
        bail!(
            "{server} did not answer within {} seconds",
            REPLY_WAIT.as_secs()
        );
    };
    let body = match read {
        Ok(Some(body)) => body,
        read => {
            sent.context("sending the request")?;
            match read {
                Err(e) => return Err(e).context("reading the reply"),
                _ => bail!("the server closed the stream without an answer"),
            }
        }
    };
    decode_reply(server, &body, what, &tap)
}

/// The outcome of the reply `body` to a request; `what` says which
/// (`None`: a change, `Some(false)`: a view, `Some(true)`: config lines).
/// The reply is reported to `tap` as received: as an `AdminReply`, or as
/// an `Error` when that is what the daemon sent.
fn decode_reply(
    server: &str,
    body: &[u8],
    what: Option<bool>,
    tap: &StreamTap,
) -> anyhow::Result<RemoteOutcome> {
    let msg = match AdminReply::decode(body) {
        Ok(reply @ AdminReply { msg: Some(_) }) => {
            tap.received(&reply, body.len());
            reply.msg.expect("checked above")
        }
        // A daemon that does not know the admin service answers with a
        // plain Error, which does not decode as a reply.
        _ => match Error::decode(body) {
            Ok(e) => {
                tap.received(&e, body.len());
                admin_reply::Msg::Refused(e)
            }
            Err(_) => {
                tap.reset("malformed reply");
                bail!("malformed reply from {server}")
            }
        },
    };
    match (msg, what) {
        (admin_reply::Msg::Ack(ack), None) => Ok(RemoteOutcome::Applied(ack.results)),
        (admin_reply::Msg::Nack(n), None) => bail!("{}", n.reason),
        (admin_reply::Msg::View(v), Some(false)) => Ok(RemoteOutcome::View(v)),
        (admin_reply::Msg::Config(t), Some(true)) => Ok(RemoteOutcome::Config(t.lines)),
        (admin_reply::Msg::Refused(e), _) => Err(refusal(server, e)),
        _ => bail!("{server} sent an answer that does not fit the request"),
    }
}

/// The error for a refusal: the plain words for the two the user can act on,
/// the daemon's own text otherwise.
fn refusal(server: &str, e: Error) -> anyhow::Error {
    match Code::try_from(e.code) {
        Ok(Code::Unauthorized) => anyhow::anyhow!(
            "{server} does not let this client manage it \
             (it needs admin = \"rw\" or \"ro\" in its [clients] entry)"
        ),
        Ok(Code::VersionMismatch) => match daemon_version(&e.message) {
            Some(n) if n < PROTOCOL_VERSION => anyhow::anyhow!(
                "{server} runs an older irsync without remote admin: update it there"
            ),
            _ => SessionRefused {
                code: Code::VersionMismatch,
                message: e.message,
            }
            .into(),
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
                warm_index: e.warm_index.then_some(true),
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

/// Prints `outcome` the way the local command does; the exit code.
pub fn print(shape: &Shape, server: &str, outcome: RemoteOutcome) -> anyhow::Result<i32> {
    match outcome {
        RemoteOutcome::Applied(results) => {
            for line in results.iter().flat_map(|r| r.split('\n')) {
                eprintln!("{}", escape_text(line));
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

    fn version_error(n: u32) -> Error {
        Error {
            code: Code::VersionMismatch as i32,
            message: format!("protocol version {PROTOCOL_VERSION} unsupported (daemon speaks {n})"),
        }
    }

    #[test]
    fn an_older_daemon_is_told_to_update() {
        let e = refusal("myserver", version_error(PROTOCOL_VERSION - 1));
        assert_eq!(
            e.to_string(),
            "myserver runs an older irsync without remote admin: update it there"
        );
    }

    /// The bytes an older daemon sends, decoded as a reply is.
    #[test]
    fn an_older_daemons_version_error_decodes_to_update_it() {
        for what in [None, Some(false), Some(true)] {
            let body = version_error(PROTOCOL_VERSION - 1).encode_to_vec();
            let Err(e) = decode_reply("myserver", &body, what, &StreamTap::off()) else {
                panic!("a version error decoded as an answer");
            };
            assert_eq!(
                e.to_string(),
                "myserver runs an older irsync without remote admin: update it there"
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
        let e = Error {
            code: Code::Unauthorized as i32,
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
