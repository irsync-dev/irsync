//! The report printed by `irsync show`.

use crate::config::exports::Exports;
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

/// The report: files, identities (`client` is the id and the public key
/// line), hosts and exports. Missing files are flagged; aliases resolve
/// through `hosts`.
pub fn report(
    paths: &Paths,
    hosts: &Hosts,
    exports: &Exports,
    client: (&EndpointId, &str),
    daemon: &EndpointId,
) -> String {
    let mut out = file_line("config", &paths.config);
    let _ = writeln!(out, "client identity: {}\n{}", client.0, client.1);
    out.push_str(&file_line("hosts", &paths.hosts));
    for (alias, h) in &hosts.entries {
        let _ = writeln!(out, "    {alias}: {}", h.endpoint_id);
    }
    out.push_str(&file_line("exports", &paths.exports));
    let _ = writeln!(out, "daemon identity: {daemon}");
    out.push_str(&export_blocks(exports, hosts));
    out.push_str(&all_block(exports, hosts));
    out
}
