//! Writes `irsync.toml` from the config message.
//!
//! The writer has one form, whatever the file looked like when it was
//! read: the top-level keys, then each table under its own header in a
//! fixed order, each key in the order of the message's fields, and every
//! comment where [`super::comments`] reads it back from. It writes only
//! what the message sets.

use super::clients::RIGHTS;
use super::duration;
use crate::authz::Access;
use irsync_proto::config as pb;
use std::collections::BTreeMap;
use std::time::Duration;
use toml_edit::{Key, Value};

/// The text of `irsync.toml` for `msg`. What it writes reads back into
/// the same message, as long as the tail of the table written last has
/// no blank line: the first blank line after the last key starts the
/// footer.
pub fn write(msg: &pb::Config) -> String {
    let mut top = Keys::new(&msg.key_comments);
    top.int("config_version", msg.config_version);
    top.bool("unsafe_config_allowed", msg.unsafe_config_allowed);
    top.string("identity", msg.identity.as_deref());
    top.string("state_dir", msg.state_dir.as_deref());
    let mut top = top.lines;
    top.extend(msg.tail.iter().cloned());

    let mut sections = vec![msg.header.clone(), top];
    if let Some(c) = &msg.client {
        sections.push(table(&["client"], &c.comment, &c.tail, client_table(c)));
    }
    if let Some(d) = &msg.daemon {
        sections.push(table(&["daemon"], &d.comment, &d.tail, daemon(d)));
        if let Some(x) = &d.discovery {
            sections.push(table(
                &["daemon", "discovery"],
                &x.comment,
                &x.tail,
                discovery(x),
            ));
        }
    }
    if let Some(t) = &msg.telemetry {
        sections.push(table(&["telemetry"], &t.comment, &t.tail, telemetry(t)));
    }
    for s in &msg.servers {
        sections.push(table(&["servers", &s.name], &s.comment, &s.tail, server(s)));
    }
    for c in &msg.clients {
        sections.push(table(
            &["clients", &c.name],
            &c.comment,
            &c.tail,
            client_entry(c),
        ));
    }
    for e in &msg.exports {
        sections.push(table(&["exports", &e.name], &e.comment, &e.tail, export(e)));
    }
    sections.push(msg.footer.clone());

    let parts: Vec<String> = sections
        .into_iter()
        .filter(|s| !s.is_empty())
        .map(|s| s.join("\n"))
        .collect();
    if parts.is_empty() {
        return String::new();
    }
    // One blank line between the parts.
    parts.join("\n\n") + "\n"
}

/// A table under its header: the lines above it, the header with its
/// trailing comment, the keys and the tail. Nothing for a table with no
/// key and no comment.
fn table(path: &[&str], comment: &Option<pb::Comment>, tail: &[String], keys: Keys) -> Vec<String> {
    let comment = comment.clone().unwrap_or_default();
    if keys.lines.is_empty()
        && comment.above.is_empty()
        && comment.trailing.is_empty()
        && tail.is_empty()
    {
        return Vec::new();
    }
    let name: Vec<String> = path.iter().map(|p| key(p)).collect();
    let mut lines = comment.above;
    lines.push(with_trailing(
        format!("[{}]", name.join(".")),
        &comment.trailing,
    ));
    lines.extend(keys.lines);
    lines.extend(tail.iter().cloned());
    lines
}

fn client_table(c: &pb::Client) -> Keys<'_> {
    let mut k = Keys::new(&c.key_comments);
    k.int("parallel", c.parallel);
    k.duration("reconnect", c.reconnect.as_ref());
    k.int("dial_retries", c.dial_retries);
    k.bool("dht", c.dht);
    k.bool("ssh_install", c.ssh_install);
    k.bool("mdns", c.mdns);
    k.string("relay", c.relay.as_deref());
    k.string("relay_mode", c.relay_mode.and_then(relay_mode));
    k.string("congestion", c.congestion.and_then(congestion));
    k.string(
        "pq_key_exchange",
        c.pq_key_exchange.and_then(pq_key_exchange),
    );
    k
}

fn daemon(d: &pb::Daemon) -> Keys<'_> {
    let mut k = Keys::new(&d.key_comments);
    k.string("identity", d.identity.as_deref());
    k.duration("idle_timeout", d.idle_timeout.as_ref());
    k.duration(
        "dormant_session_timeout",
        d.dormant_session_timeout.as_ref(),
    );
    k.int(
        "max_dormant_sessions_per_key",
        d.max_dormant_sessions_per_key,
    );
    k.array("listen", &d.listen, d.listen_set);
    k.bool("portmap", d.portmap);
    k.string("relay", d.relay.as_deref());
    k.string("relay_mode", d.relay_mode.and_then(relay_mode));
    k.string("congestion", d.congestion.and_then(congestion));
    k.string(
        "pq_key_exchange",
        d.pq_key_exchange.and_then(pq_key_exchange),
    );
    k.string("debug_addr", d.debug_addr.as_deref());
    k.bool("allow_all_paths", d.allow_all_paths);
    k.bool("allow_root", d.allow_root);
    k
}

fn discovery(d: &pb::Discovery) -> Keys<'_> {
    let mut k = Keys::new(&d.key_comments);
    k.bool("dht", d.dht);
    k.bool("dns", d.dns);
    k.bool("mdns", d.mdns);
    k
}

fn telemetry(t: &pb::Telemetry) -> Keys<'_> {
    let mut k = Keys::new(&t.key_comments);
    k.string("otlp_endpoint", t.otlp_endpoint.as_deref());
    k.string("otlp_protocol", t.otlp_protocol.and_then(otlp_protocol));
    k
}

fn server(s: &pb::ServerEntry) -> Keys<'_> {
    let mut k = Keys::new(&s.key_comments);
    k.string("endpoint_id", non_empty(&s.endpoint_id).map(id).as_deref());
    k.string("relay", s.relay.as_deref());
    k.string("relay_mode", s.relay_mode.and_then(relay_mode));
    k.string("congestion", s.congestion.and_then(congestion));
    k.string(
        "pq_key_exchange",
        s.pq_key_exchange.and_then(pq_key_exchange),
    );
    k.array("address", &s.address, s.address_set);
    k.int("parallel", s.parallel);
    k.duration("reconnect", s.reconnect.as_ref());
    k.int("dial_retries", s.dial_retries);
    k.bool("dht", s.dht);
    k.bool("mdns", s.mdns);
    k.string("ssh", s.ssh.as_deref());
    k.array("ssh_aliases", &s.ssh_aliases, s.ssh_aliases_set);
    k
}

fn client_entry(c: &pb::ClientEntry) -> Keys<'_> {
    let mut k = Keys::new(&c.key_comments);
    k.string("client_id", non_empty(&c.client_id).map(id).as_deref());
    for right in RIGHTS {
        k.string(right.key(), right.get(c).map(Access::as_str));
    }
    k
}

/// An id of the message: empty means missing.
fn non_empty(bytes: &[u8]) -> Option<&[u8]> {
    (!bytes.is_empty()).then_some(bytes)
}

fn export(e: &pb::Export) -> Keys<'_> {
    let mut k = Keys::new(&e.key_comments);
    k.string("path", Some(&e.path));
    k.bool("read_only", e.read_only);
    k.bool("warm_index", e.warm_index);
    k.bool("fsync", e.fsync);
    k.bool("auto", e.auto);
    k.array("rw", &e.rw, e.rw_set);
    k.array("ro", &e.ro, e.ro_set);
    k
}

/// The lines of the keys of one table, each with its comments.
struct Keys<'a> {
    comments: &'a BTreeMap<String, pb::Comment>,
    lines: Vec<String>,
}

impl<'a> Keys<'a> {
    fn new(comments: &'a BTreeMap<String, pb::Comment>) -> Self {
        Keys {
            comments,
            lines: Vec::new(),
        }
    }

    fn bool(&mut self, name: &str, v: Option<bool>) {
        if let Some(v) = v {
            self.put(name, Value::from(v).to_string());
        }
    }

    fn int(&mut self, name: &str, v: Option<u32>) {
        if let Some(v) = v {
            self.put(name, Value::from(i64::from(v)).to_string());
        }
    }

    fn string(&mut self, name: &str, v: Option<&str>) {
        if let Some(v) = v {
            self.put(name, quoted(v));
        }
    }

    fn duration(&mut self, name: &str, v: Option<&prost_types::Duration>) {
        if let Some(d) = v {
            self.put(name, quoted(&duration_text(d)));
        }
    }

    /// A string array: on one line, or one entry per line when an entry
    /// has a comment.
    fn array(&mut self, name: &str, entries: &[pb::Entry], set: bool) {
        if !set && entries.is_empty() {
            return;
        }
        let commented = entries.iter().any(|e| {
            e.comment
                .as_ref()
                .is_some_and(|c| !c.above.is_empty() || !c.trailing.is_empty())
        });
        if !commented {
            let values: Vec<String> = entries.iter().map(|e| quoted(&e.value)).collect();
            self.put(name, format!("[{}]", values.join(", ")));
            return;
        }
        let comment = self.comment(name);
        self.lines.extend(comment.above.iter().cloned());
        self.lines.push(format!("{} = [", key(name)));
        for e in entries {
            let c = e.comment.clone().unwrap_or_default();
            self.lines
                .extend(c.above.iter().map(|l| format!("    {l}")));
            self.lines.push(with_trailing(
                format!("    {},", quoted(&e.value)),
                &c.trailing,
            ));
        }
        self.lines
            .push(with_trailing("]".to_owned(), &comment.trailing));
    }

    /// `name = value`, with the key's comments.
    fn put(&mut self, name: &str, value: String) {
        let comment = self.comment(name);
        self.lines.extend(comment.above.iter().cloned());
        self.lines.push(with_trailing(
            format!("{} = {value}", key(name)),
            &comment.trailing,
        ));
    }

    fn comment(&self, name: &str) -> pb::Comment {
        self.comments.get(name).cloned().unwrap_or_default()
    }
}

fn with_trailing(line: String, trailing: &str) -> String {
    if trailing.is_empty() {
        line
    } else {
        format!("{line} {trailing}")
    }
}

/// A key or table name, quoted when TOML needs it.
pub(crate) fn key(name: &str) -> String {
    Key::new(name).display_repr().into_owned()
}

/// A string value, quoted as TOML writes it.
pub(crate) fn quoted(s: &str) -> String {
    Value::from(s).to_string()
}

/// An id as `EndpointId` prints it; bytes that are no id, in hex, which
/// the reader then refuses.
pub(crate) fn id(bytes: &[u8]) -> String {
    <[u8; 32]>::try_from(bytes)
        .ok()
        .and_then(|b| iroh::EndpointId::from_bytes(&b).ok())
        .map_or_else(
            || bytes.iter().map(|b| format!("{b:02x}")).collect(),
            |id| id.to_string(),
        )
}

/// A duration as the file spells it (`90s`, `30m`).
pub(crate) fn duration_text(d: &prost_types::Duration) -> String {
    let secs = u64::try_from(d.seconds).unwrap_or(0);
    let nanos = u32::try_from(d.nanos).unwrap_or(0);
    duration::format(Duration::new(secs, nanos))
}

pub(crate) fn relay_mode(m: i32) -> Option<&'static str> {
    match pb::RelayMode::try_from(m).ok()? {
        pb::RelayMode::Default => Some("default"),
        pb::RelayMode::Disabled => Some("disabled"),
        pb::RelayMode::Unspecified => None,
    }
}

pub(crate) fn congestion(c: i32) -> Option<&'static str> {
    match pb::Congestion::try_from(c).ok()? {
        pb::Congestion::Bbr => Some("bbr"),
        pb::Congestion::Cubic => Some("cubic"),
        pb::Congestion::Unspecified => None,
    }
}

pub(crate) fn pq_key_exchange(p: i32) -> Option<&'static str> {
    match pb::PqKeyExchange::try_from(p).ok()? {
        pb::PqKeyExchange::Prefer => Some("prefer"),
        pb::PqKeyExchange::Require => Some("require"),
        pb::PqKeyExchange::Unspecified => None,
    }
}

pub(crate) fn otlp_protocol(p: i32) -> Option<&'static str> {
    match pb::OtlpProtocol::try_from(p).ok()? {
        pb::OtlpProtocol::Grpc => Some("grpc"),
        pb::OtlpProtocol::Http => Some("http"),
        pb::OtlpProtocol::Unspecified => None,
    }
}
