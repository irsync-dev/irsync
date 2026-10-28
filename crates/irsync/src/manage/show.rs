//! The report printed by `irsync show`.

use crate::config::exports::Exports;
use crate::config::file::Config;
use crate::config::hosts::Hosts;
use crate::config::paths::Paths;
use iroh::EndpointId;
use std::fmt::Write;
use std::path::Path;

fn file_line(label: &str, path: &Path) -> String {
    let missing = if path.exists() { "" } else { " (missing)" };
    format!("{label}: {}{missing}\n", path.display())
}

/// An entry of an `rw`/`ro` list: a raw id as is, a known alias with its id,
/// anything else flagged as an unknown alias.
fn entry_text(entry: &str, hosts: &Hosts) -> String {
    if crate::identity::parse_endpoint_id(entry).is_ok() {
        entry.to_string()
    } else if let Some(h) = hosts.get(entry) {
        format!("{entry} ({})", h.endpoint_id)
    } else {
        format!("{entry} (unknown alias)")
    }
}

/// The `    rw: ...` and `    ro: ...` lines of one grant table, each only
/// when its list is non-empty. The one spelling of a grant line, shared by
/// `show`, `export list` and `grant list`.
pub fn grant_lines(out: &mut String, rw: &[String], ro: &[String], hosts: &Hosts) {
    for (label, list) in [("rw", rw), ("ro", ro)] {
        if !list.is_empty() {
            let items: Vec<String> = list.iter().map(|e| entry_text(e, hosts)).collect();
            let _ = writeln!(out, "    {label}: {}", items.join(", "));
        }
    }
}

/// One block per export: `name: path`, then `    read_only` and
/// `    warm_index` when set, then its grant lines.
pub fn export_blocks(exports: &Exports, hosts: &Hosts) -> String {
    let mut out = String::new();
    for (name, e) in &exports.exports {
        let _ = writeln!(out, "{name}: {}", e.path.display());
        if e.read_only {
            out.push_str("    read_only\n");
        }
        if e.warm_index {
            out.push_str("    warm_index\n");
        }
        grant_lines(&mut out, &e.rw, &e.ro, hosts);
    }
    out
}

/// `all:` and its grant lines, or nothing when `[all]` grants nobody.
pub fn all_block(exports: &Exports, hosts: &Hosts) -> String {
    let mut out = String::new();
    if !exports.rw.is_empty() || !exports.ro.is_empty() {
        out.push_str("all:\n");
        grant_lines(&mut out, &exports.rw, &exports.ro, hosts);
    }
    out
}

/// The report: the config file, identities (`client` is the id and the
/// public key line), hosts (`host_lines`, what `host list` prints) and
/// exports. The hosts and exports lines name the file that holds each
/// table, `(also read)` when it is a split file beside the config.
pub fn report(
    paths: &Paths,
    config: &Config,
    host_lines: &[String],
    client: (&EndpointId, &str),
    daemon: &EndpointId,
) -> String {
    let hosts = &config.hosts;
    let exports = &config.exports;
    let owner = |label: &str, path: &Path, split: bool| {
        let suffix = if split { " (also read)" } else { "" };
        format!("{label}: {}{suffix}\n", path.display())
    };
    let mut out = file_line("config", &paths.config);
    let _ = writeln!(out, "client identity: {}\n{}", client.0, client.1);
    out.push_str(&owner("hosts", &hosts.path, config.split_hosts.is_some()));
    for line in host_lines {
        let _ = writeln!(out, "{line}");
    }
    out.push_str(&owner(
        "exports",
        &exports.path,
        config.split_exports.is_some(),
    ));
    let _ = writeln!(out, "daemon identity: {daemon}");
    out.push_str(&export_blocks(exports, hosts));
    out.push_str(&all_block(exports, hosts));
    out
}
