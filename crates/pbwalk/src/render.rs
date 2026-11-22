//! The listing: one block per packet, each field on a line with its size.

use std::fmt::Write as _;
use std::io::{self, Write};

use prost_reflect::DescriptorPool;

use crate::account::hex;
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

/// Width of the hex column.
pub const HEX_WIDTH: usize = 48;
/// Width of both columns and the gaps after them; the text starts here.
pub const DUMP_WIDTH: usize = 70;
/// Bytes on one line of the hex column.
const DUMP_BYTES: usize = 16;

/// How the listing is printed.
#[derive(Clone, Copy, Default, Debug)]
pub struct Listing {
    /// Long values in full.
    pub full: bool,
    /// Each line's bytes in front of it, in hex and as characters.
    pub hex: bool,
}

/// Up to 16 bytes as `hexdump -C` prints them, without the offset: the
/// hex padded to [`HEX_WIDTH`], two spaces, the characters between `|`.
/// Nothing after the closing `|`.
fn dump_line(chunk: &[u8]) -> String {
    let mut out = String::with_capacity(DUMP_WIDTH);
    for (i, b) in chunk.iter().enumerate() {
        if i > 0 {
            out.push(' ');
        }
        if i == DUMP_BYTES / 2 {
            out.push(' ');
        }
        let _ = write!(out, "{b:02x}");
    }
    let _ = write!(out, "{:pad$}  |", "", pad = HEX_WIDTH - out.len());
    // A printable ASCII character as itself, anything else as a dot.
    out.extend(chunk.iter().map(|&b| match b {
        0x20..=0x7e => char::from(b),
        _ => '.',
    }));
    out.push('|');
    out
}

/// One line of the listing: `text`, with `bytes` in front of it when
/// `hex`. More than 16 bytes continue on lines of their own.
pub(crate) fn line(out: &mut impl Write, hex: bool, bytes: &[u8], text: &str) -> io::Result<()> {
    if !hex {
        return writeln!(out, "{text}");
    }
    let mut chunks = bytes.chunks(DUMP_BYTES);
    let first = chunks.next().map(dump_line).unwrap_or_default();
    writeln!(out, "{first:DUMP_WIDTH$}{text}")?;
    for chunk in chunks {
        writeln!(out, "{}", dump_line(chunk))?;
    }
    Ok(())
}

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
    listing: Listing,
) -> io::Result<()> {
    let Listing { full, hex } = listing;
    let size = match entry.kind {
        Kind::Event => "event".to_owned(),
        Kind::Message if entry.prefix > 0 => format!(
            "{} B + {} B prefix",
            grouped(entry.wire_len),
            grouped(entry.prefix)
        ),
        Kind::Message | Kind::Raw => format!("{} B", grouped(entry.wire_len)),
    };
    let first = format!("{}  {}  {size}", place_text(place), type_label(entry));
    line(out, hex, &[], &first)?;
    if !entry.header.is_empty() {
        let header: Vec<String> = entry
            .header
            .iter()
            .map(|(k, v)| format!("{}={}", k.escape_debug(), header_text(v, full)))
            .collect();
        line(out, hex, &[], &format!("   {}", header.join(" ")))?;
    }

    // An event was never on the wire: its fields are listed without sizes,
    // which would only be the bytes it takes in the file.
    let style = Style {
        pool,
        full,
        hex,
        sized: entry.kind != Kind::Event,
    };
    for f in &entry.fields {
        if is_unknown_type(entry) && f.name.is_none() {
            if let Value::Bytes(raw) = f.value {
                let label = format!(
                    "{} bytes, type not in the protos",
                    grouped(raw.len() as u64)
                );
                let text = if style.sized {
                    row(2, &label, f.sizes.total(), None)
                } else {
                    format!("  {label}")
                };
                line(out, hex, f.own, &text)?;
                continue;
            }
        }
        field(out, f, 0, &style)?;
    }

    if entry.cut_content > 0 || entry.cut_pieces > 0 {
        let pieces = plural(entry.cut_pieces, "piece", "pieces");
        let text = row(2, "cut content", entry.cut_content, Some(pieces));
        line(out, hex, &[], &text)?;
    }
    if entry.cut_other > 0 || entry.dropped_entries > 0 {
        let dropped = (entry.dropped_entries > 0).then(|| {
            format!(
                "{} dropped",
                plural(entry.dropped_entries, "entry", "entries")
            )
        });
        let text = row(2, "cut, not itemised", entry.cut_other, dropped);
        line(out, hex, &[], &text)?;
    }
    if entry.not_captured > 0 {
        let text = row(2, "message not captured", entry.not_captured, None);
        line(out, hex, &[], &text)?;
    }
    if entry.unexplained > 0 {
        let text = row(
            2,
            "not in the capture, unexplained",
            entry.unexplained,
            None,
        );
        line(out, hex, &[], &text)?;
    }
    if is_mismatch(entry) {
        let text = format!("  DOES NOT ADD UP: {}", mismatch_text(entry));
        line(out, hex, &[], &text)?;
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

pub(crate) fn plural(n: u64, one: &str, many: &str) -> String {
    format!("{} {}", grouped(n), if n == 1 { one } else { many })
}

/// How fields are printed.
struct Style<'p> {
    pool: &'p DescriptorPool,
    /// Long values in full.
    full: bool,
    /// With each line's bytes in front of it.
    hex: bool,
    /// With their sizes.
    sized: bool,
}

/// One field's row, then its children's.
fn field(out: &mut impl Write, f: &Field, depth: usize, style: &Style) -> io::Result<()> {
    let Style {
        pool,
        full,
        hex,
        sized,
    } = *style;
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
    let text = if !sized {
        format!("{:indent$}{label}", "")
    } else if matches!(f.value, Value::Undecodable(_)) {
        row(indent, &label, f.sizes.total(), None)
    } else {
        row(indent, &label, f.sizes.total(), Some(sizes_text(&f.sizes)))
    };
    line(out, hex, f.own, &text)?;
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

/// A row's text: its label, its size and what the size is made of.
fn row(indent: usize, label: &str, bytes: u64, detail: Option<String>) -> String {
    let used = indent + label.chars().count();
    let pad = LABEL_WIDTH.saturating_sub(used);
    let mut text = format!(
        "{:indent$}{label}{:pad$} {:>SIZE_WIDTH$} B",
        "",
        "",
        grouped(bytes)
    );
    if let Some(detail) = detail {
        let _ = write!(text, "  ({detail})");
    }
    text
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

    #[test]
    fn dump_layout() {
        assert_eq!(dump_line(&[0x10, 0x05]), format!("{:<48}  |..|", "10 05"));
        let sixteen: Vec<u8> = (0x41..0x51).collect();
        assert_eq!(
            dump_line(&sixteen),
            "41 42 43 44 45 46 47 48  49 4a 4b 4c 4d 4e 4f 50  |ABCDEFGHIJKLMNOP|"
        );
        assert_eq!(dump_line(&sixteen).chars().count(), DUMP_WIDTH - 2);

        let text = |bytes: &[u8]| {
            let mut out = Vec::new();
            line(&mut out, true, bytes, "  x = 1").unwrap();
            String::from_utf8(out).unwrap()
        };
        // No bytes, fewer than 16, exactly 16: one line, text in column 71.
        for n in [0, 2, 16] {
            let out = text(&sixteen[..n]);
            assert_eq!(out.lines().count(), 1, "{n}");
            assert_eq!(&out[DUMP_WIDTH..], "  x = 1\n", "{n}");
        }
        // 17: the text on the first line, one byte alone on the second.
        let seventeen = [&sixteen[..], &[0x51]].concat();
        let out = text(&seventeen);
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines.len(), 2);
        assert_eq!(&lines[0][DUMP_WIDTH..], "  x = 1");
        assert_eq!(lines[1], format!("{:<48}  |Q|", "51"));
        assert!(out.lines().all(|l| !l.ends_with(' ')));
        // Without hex the line is the text.
        let mut plain = Vec::new();
        line(&mut plain, false, &seventeen, "  x = 1").unwrap();
        assert_eq!(plain, b"  x = 1\n");
    }

    #[test]
    fn dump_characters() {
        // printable, space, control, 0x7f, above ASCII, and the bar itself
        assert_eq!(
            dump_line(&[b'a', b' ', 0x1b, 0x7f, 0x80, 0xff, b'|', b'~']),
            format!("{:<48}  |a ....|~|", "61 20 1b 7f 80 ff 7c 7e")
        );
    }
}
