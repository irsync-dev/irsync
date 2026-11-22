//! The listing: one block per packet, each field on a line with its size.

use std::fmt::Write as _;
use std::io::{self, Write};

use prost_reflect::DescriptorPool;

use crate::{Entry, Field, Kind, Sizes, Value};

/// Field labels are padded to this many columns, so sizes line up.
const LABEL_WIDTH: usize = 48;
/// Sizes are right-aligned in this many columns.
const SIZE_WIDTH: usize = 11;
/// Without `--full`, byte strings longer than this are shortened.
const SHORT_BYTES: usize = 32;
/// Without `--full`, header values longer than this are shortened.
const SHORT_HEADER: usize = 32;
/// Without `--full`, packed fields with more elements than this are shortened.
const SHORT_VALUES: usize = 8;

/// `n` with `,` between groups of three digits: 262,144.
pub fn grouped(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// The name an entry's type goes by in the listing and the totals.
pub fn type_label(entry: &Entry) -> String {
    if entry.type_name.is_empty() {
        return match entry.kind {
            Kind::Event => "(empty record)".to_owned(),
            _ if entry.not_captured > 0 => "(message not captured)".to_owned(),
            _ => "(no message)".to_owned(),
        };
    }
    if is_unknown_type(entry) {
        return format!("{} (type not in the protos)", entry.type_name);
    }
    entry.type_name.clone()
}

/// A payload whose type the protos do not know: named by its field number.
pub fn is_unknown_type(entry: &Entry) -> bool {
    entry.kind != Kind::Raw && entry.type_name.starts_with('#')
}

/// Whether the entry's numbers are wrong: a problem found while
/// unwrapping, or rows that do not sum to its size on the wire.
pub fn is_mismatch(entry: &Entry) -> bool {
    entry.mismatch.is_some() || !entry.adds_up()
}

/// Where a packet sits in the file, for its first line.
pub struct Place<'p> {
    /// From 1.
    pub number: u64,
    /// Since the first packet of the file.
    pub micros: i128,
    pub packet: &'p pbcap::Packet,
}

/// The packet's number, time, addresses and port: `#6 0.012345 a → b port 1`.
fn place_text(place: &Place) -> String {
    let sign = if place.micros < 0 { "-" } else { "" };
    let abs = place.micros.unsigned_abs();
    let mut s = format!(
        "#{} {sign}{}.{:06}",
        place.number,
        abs / 1_000_000,
        abs % 1_000_000
    );
    let p = place.packet;
    if let (Some(src), Some(dst)) = (p.src(), p.dst()) {
        let _ = write!(s, " {src} → {dst}");
    }
    match (p.src_port(), p.dst_port()) {
        (Some(a), Some(b)) if a == b => {
            let _ = write!(s, " port {a}");
        }
        (Some(a), Some(b)) => {
            let _ = write!(s, " port {a} → {b}");
        }
        _ => {}
    }
    s
}

/// Writes one entry's block.
pub fn entry(
    out: &mut impl Write,
    place: &Place,
    entry: &Entry,
    pool: &DescriptorPool,
    full: bool,
) -> io::Result<()> {
    let size = match entry.kind {
        Kind::Event => "event".to_owned(),
        Kind::Message if entry.prefix > 0 => format!(
            "{} B + {} B prefix",
            grouped(entry.wire_len),
            grouped(entry.prefix)
        ),
        Kind::Message | Kind::Raw => format!("{} B", grouped(entry.wire_len)),
    };
    writeln!(out, "{}  {}  {size}", place_text(place), type_label(entry))?;
    if !entry.header.is_empty() {
        let header: Vec<String> = entry
            .header
            .iter()
            .map(|(k, v)| format!("{}={}", k.escape_debug(), header_text(v, full)))
            .collect();
        writeln!(out, "   {}", header.join(" "))?;
    }

    // An event was never on the wire: its fields are listed without sizes,
    // which would only be the bytes it takes in the file.
    let style = Style {
        pool,
        full,
        sized: entry.kind != Kind::Event,
    };
    for f in &entry.fields {
        if is_unknown_type(entry) && f.name.is_none() {
            if let Value::Bytes(raw) = f.value {
                let label = format!(
                    "{} bytes, type not in the protos",
                    grouped(raw.len() as u64)
                );
                if style.sized {
                    row(out, 2, &label, f.sizes.total(), None)?;
                } else {
                    writeln!(out, "  {label}")?;
                }
                continue;
            }
        }
        field(out, f, 0, &style)?;
    }

    if entry.cut_content > 0 || entry.cut_pieces > 0 {
        let pieces = plural(entry.cut_pieces, "piece", "pieces");
        row(out, 2, "cut content", entry.cut_content, Some(pieces))?;
    }
    if entry.cut_other > 0 || entry.dropped_entries > 0 {
        let dropped = (entry.dropped_entries > 0).then(|| {
            format!(
                "{} dropped",
                plural(entry.dropped_entries, "entry", "entries")
            )
        });
        row(out, 2, "cut, not itemised", entry.cut_other, dropped)?;
    }
    if entry.not_captured > 0 {
        row(out, 2, "message not captured", entry.not_captured, None)?;
    }
    if entry.unexplained > 0 {
        row(
            out,
            2,
            "not in the capture, unexplained",
            entry.unexplained,
            None,
        )?;
    }
    if is_mismatch(entry) {
        writeln!(out, "  DOES NOT ADD UP: {}", mismatch_text(entry))?;
    }
    Ok(())
}

/// A header value, escaped; shortened unless `full`. `--where` still
/// matches the whole value.
fn header_text(value: &str, full: bool) -> String {
    if full || value.chars().count() <= SHORT_HEADER {
        return value.escape_debug().to_string();
    }
    let head: String = value.chars().take(SHORT_HEADER / 2).collect();
    format!("{}…", head.escape_debug())
}

/// What is wrong with an entry's numbers.
fn mismatch_text(entry: &Entry) -> String {
    let mut parts = Vec::new();
    if let Some(m) = &entry.mismatch {
        parts.push(m.escape_debug().to_string());
    }
    if !entry.adds_up() {
        let listed: u128 = [
            entry.present,
            entry.cut_content,
            entry.cut_other,
            entry.not_captured,
            entry.unexplained,
        ]
        .iter()
        .map(|&n| u128::from(n))
        .sum();
        parts.push(format!(
            "the rows sum to {listed} B, the message is {} B on the wire",
            entry.wire_len
        ));
    }
    parts.join("; ")
}

fn plural(n: u64, one: &str, many: &str) -> String {
    format!("{} {}", grouped(n), if n == 1 { one } else { many })
}

/// How fields are printed.
struct Style<'p> {
    pool: &'p DescriptorPool,
    /// Long values in full.
    full: bool,
    /// With their sizes.
    sized: bool,
}

/// One field's row, then its children's.
fn field(out: &mut impl Write, f: &Field, depth: usize, style: &Style) -> io::Result<()> {
    let Style { pool, full, sized } = *style;
    let name = field_name(f);
    let indent = 2 + 2 * depth;
    let label = match &f.value {
        Value::Scalar(text) => format!("{name} = {}", text.escape_debug()),
        Value::Bytes(raw) => format!("{name} = {}", bytes_text(raw, full)),
        Value::Message { type_name, .. } => format!("{name}: {}", local_name(pool, type_name)),
        Value::Packed(elements) => format!("{name} = {}", packed_text(elements, full)),
        Value::Unknown { wire_type, .. } => match &f.name {
            None => format!("{name} ({})", wire_type_name(*wire_type)),
            Some(_) => format!(
                "{name} ({}, not its declared type)",
                wire_type_name(*wire_type)
            ),
        },
        Value::Undecodable(raw) => {
            format!("{} bytes not decodable", grouped(raw.len() as u64))
        }
    };
    if !sized {
        writeln!(out, "{:indent$}{label}", "")?;
    } else if matches!(f.value, Value::Undecodable(_)) {
        row(out, indent, &label, f.sizes.total(), None)?;
    } else {
        row(
            out,
            indent,
            &label,
            f.sizes.total(),
            Some(sizes_text(&f.sizes)),
        )?;
    }
    if let Value::Message { fields, .. } = &f.value {
        for child in fields {
            field(out, child, depth + 1, style)?;
        }
    }
    Ok(())
}

/// The field's name, or `#N` when the type has no such field.
pub fn field_name(f: &Field) -> String {
    match &f.name {
        Some(name) => name.clone(),
        None => format!("#{}", f.number),
    }
}

/// `tag 1, len 1, value 15`; the length only for a length-delimited field.
fn sizes_text(sizes: &Sizes) -> String {
    if sizes.len > 0 {
        format!(
            "tag {}, len {}, value {}",
            grouped(sizes.tag),
            grouped(sizes.len),
            grouped(sizes.value)
        )
    } else {
        format!("tag {}, value {}", grouped(sizes.tag), grouped(sizes.value))
    }
}

fn row(
    out: &mut impl Write,
    indent: usize,
    label: &str,
    bytes: u64,
    detail: Option<String>,
) -> io::Result<()> {
    let used = indent + label.chars().count();
    let pad = LABEL_WIDTH.saturating_sub(used);
    write!(
        out,
        "{:indent$}{label}{:pad$} {:>SIZE_WIDTH$} B",
        "",
        "",
        grouped(bytes)
    )?;
    if let Some(detail) = detail {
        write!(out, "  ({detail})")?;
    }
    writeln!(out)
}

/// A nested message's type without its package: `Hello.Preserve`.
fn local_name<'n>(pool: &DescriptorPool, full_name: &'n str) -> &'n str {
    pool.get_message_by_name(full_name)
        .and_then(|m| {
            let package = m.package_name();
            if package.is_empty() {
                None
            } else {
                full_name
                    .strip_prefix(package)
                    .and_then(|rest| rest.strip_prefix('.'))
            }
        })
        .unwrap_or(full_name)
}

/// Text in quotes when it is UTF-8 without control characters, else hex;
/// shortened unless `full`.
fn bytes_text(raw: &[u8], full: bool) -> String {
    let short = !full && raw.len() > SHORT_BYTES;
    let text = std::str::from_utf8(raw)
        .ok()
        .filter(|s| !s.chars().any(char::is_control));
    let mut out = match text {
        Some(s) => {
            let mut end = if short { SHORT_BYTES } else { s.len() };
            while !s.is_char_boundary(end) {
                end -= 1;
            }
            format!("\"{}\"", s[..end].escape_debug())
        }
        None => hex(if short { &raw[..SHORT_BYTES] } else { raw }),
    };
    if short {
        let _ = write!(out, " … ({} bytes)", grouped(raw.len() as u64));
    }
    out
}

fn packed_text(elements: &[String], full: bool) -> String {
    let short = !full && elements.len() > SHORT_VALUES;
    let shown = if short {
        &elements[..SHORT_VALUES]
    } else {
        elements
    };
    let mut out = format!("[{}", shown.join(", "));
    if short {
        let _ = write!(out, ", … ({} values)", grouped(elements.len() as u64));
    }
    out.push(']');
    out
}

fn wire_type_name(wire_type: u8) -> String {
    match wire_type {
        0 => "varint".to_owned(),
        1 => "fixed64".to_owned(),
        2 => "length-delimited".to_owned(),
        5 => "fixed32".to_owned(),
        n => format!("wire type {n}"),
    }
}

fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(out, "{b:02x}");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grouping() {
        assert_eq!(grouped(0), "0");
        assert_eq!(grouped(999), "999");
        assert_eq!(grouped(1000), "1,000");
        assert_eq!(grouped(262_144), "262,144");
        assert_eq!(grouped(u64::MAX), "18,446,744,073,709,551,615");
    }

    #[test]
    fn bytes_as_text_or_hex() {
        assert_eq!(bytes_text(b"m", false), "\"m\"");
        assert_eq!(bytes_text(b"", false), "\"\"");
        assert_eq!(bytes_text(b"a\"b", false), "\"a\\\"b\"");
        assert_eq!(bytes_text(b"a\x1bb", false), "611b62");
        assert_eq!(bytes_text(&[0xff, 0], false), "ff00");
        let long = "é".repeat(20); // 40 bytes; 32 is a char boundary
        assert_eq!(
            bytes_text(long.as_bytes(), false),
            format!("\"{}\" … (40 bytes)", "é".repeat(16))
        );
        let odd = format!("a{}", "é".repeat(20)); // byte 32 is inside a char
        assert_eq!(
            bytes_text(odd.as_bytes(), false),
            format!("\"a{}\" … (41 bytes)", "é".repeat(15))
        );
        assert_eq!(bytes_text(&[0xab; 40], false).matches("ab").count(), 32);
    }

    #[test]
    fn packed_is_shortened() {
        let v: Vec<String> = (1..=10).map(|n| n.to_string()).collect();
        assert_eq!(
            packed_text(&v, false),
            "[1, 2, 3, 4, 5, 6, 7, 8, … (10 values)]"
        );
        assert_eq!(packed_text(&v[..8], false), "[1, 2, 3, 4, 5, 6, 7, 8]");
        assert_eq!(packed_text(&v, true).matches(", ").count(), 9);
    }
}
