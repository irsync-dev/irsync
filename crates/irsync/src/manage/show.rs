//! The report printed by `irsync show`.

use crate::config::clients::Clients;
use crate::config::exports::Exports;
use crate::config::file::Config;
use crate::config::paths::Paths;
use iroh::EndpointId;
use std::fmt::Write;
use std::path::Path;

fn file_line(label: &str, path: &Path) -> String {
    let missing = if path.exists() { "" } else { " (missing)" };
    format!("{label}: {}{missing}\n", path.display())
}

/// An entry of an `rw`/`ro` list: a raw id as is, a known client name
/// with its id, anything else flagged as unknown.
fn entry_text(entry: &str, clients: &Clients) -> String {
    if crate::identity::parse_endpoint_id(entry).is_ok() {
        entry.to_string()
    } else if let Some(c) = clients.get(entry) {
        format!("{entry} ({})", c.client_id)
    } else {
        format!("{entry} (unknown client)")
    }
}

/// The `    rw: ...` and `    ro: ...` lines of one grant table, each only
/// when its list is non-empty. The one spelling of a grant line, shared by
/// `show`, `export list` and `grant list`.
pub fn grant_lines(out: &mut String, rw: &[String], ro: &[String], clients: &Clients) {
    for (label, list) in [("rw", rw), ("ro", ro)] {
        if !list.is_empty() {
            let items: Vec<String> = list.iter().map(|e| entry_text(e, clients)).collect();
            let _ = writeln!(out, "    {label}: {}", items.join(", "));
        }
    }
}

/// One block per export: `name: path`, then `    read_only`,
/// `    warm_index` and `    fsync` when set, then its grant lines.
pub fn export_blocks(exports: &Exports, clients: &Clients) -> String {
    let mut out = String::new();
    for (name, e) in &exports.exports {
        let _ = writeln!(out, "{name}: {}", e.path.display());
        if e.read_only {
            out.push_str("    read_only\n");
        }
        if e.warm_index {
            out.push_str("    warm_index\n");
        }
        if e.fsync {
            out.push_str("    fsync\n");
        }
        grant_lines(&mut out, &e.rw, &e.ro, clients);
    }
    out
}

/// The report: the config file, identities (`client` is the id and the
/// public key line), servers and clients (`server_lines` and
/// `client_lines`, what `server list` and `client list` print) and
/// exports, each section under its name. `daemon` is none when this
/// machine has no daemon key.
pub fn report(
    paths: &Paths,
    config: &Config,
    server_lines: &[String],
    client_lines: &[String],
    client: (&EndpointId, &str),
    daemon: Option<&EndpointId>,
) -> String {
    let clients = &config.clients;
    let exports = &config.exports;
    let mut out = file_line("config", &paths.config);
    let _ = writeln!(out, "client identity: {}\n{}", client.0, client.1);
    for (title, lines) in [("servers:", server_lines), ("clients:", client_lines)] {
        out.push_str(title);
        out.push('\n');
        for line in lines {
            let _ = writeln!(out, "{line}");
        }
    }
    out.push_str("exports:\n");
    match daemon {
        Some(id) => {
            let _ = writeln!(out, "daemon identity: {id}");
        }
        None => out.push_str("daemon identity: none (irsync id generate --daemon makes one)\n"),
    }
    out.push_str(&export_blocks(exports, clients));
    out
}
