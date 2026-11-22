//! The totals printed after the listing: by message, by field, overhead,
//! by direction and by each `--group-by` field.

use std::collections::BTreeMap;
use std::io::{self, Write};

use pbcap::v1::Direction;
use prost_reflect::MessageDescriptor;

use crate::render::{field_name, grouped, is_unknown_type, plural, type_label};
use crate::{Entry, Field, Kind, Summary, Value};

/// The envelope's direction field.
const DIRECTION: u32 = 2;

/// A count and a number of bytes.
#[derive(Default, Clone, Copy)]
struct Tally {
    count: u64,
    bytes: u64,
}

impl Tally {
    fn add(&mut self, bytes: u64) {
        self.count = self.count.saturating_add(1);
        self.bytes = self.bytes.saturating_add(bytes);
    }
}

fn add(sum: &mut u64, n: u64) {
    *sum = sum.saturating_add(n);
}

/// Everything the tables need, kept as packets go by.
#[derive(Default)]
pub struct Totals {
    by_message: BTreeMap<String, Tally>,
    events: BTreeMap<String, u64>,
    by_field: BTreeMap<String, u64>,
    prefixes: u64,
    tags: u64,
    lens: u64,
    values: u64,
    undecodable: u64,
    missing: u64,
    sent: Tally,
    received: Tally,
    unspecified: Tally,
    /// A direction number the envelope does not know.
    other: Tally,
    groups: Vec<(String, BTreeMap<String, Tally>)>,
}

impl Totals {
    /// Totals with one extra table per wrapper field in `group_by`, but
    /// none for the direction field: the By direction table shows it.
    pub fn new(group_by: &[String], wrapper: &MessageDescriptor) -> Totals {
        Totals {
            groups: group_by
                .iter()
                .filter(|f| {
                    wrapper
                        .get_field_by_name(f)
                        .is_none_or(|d| d.number() != DIRECTION)
                })
                .map(|f| (f.clone(), BTreeMap::new()))
                .collect(),
            ..Totals::default()
        }
    }

    /// Counts one entry that passed `--where`.
    pub fn add(&mut self, entry: &Entry, wrapper: &MessageDescriptor) {
        let name = type_label(entry);
        if entry.kind == Kind::Event {
            add(self.events.entry(name).or_default(), 1);
            return;
        }
        self.by_message.entry(name).or_default().add(entry.wire_len);

        let with_prefix = entry.wire_len.saturating_add(entry.prefix);
        match entry.direction {
            Some(Direction::Sent) => self.sent.add(with_prefix),
            Some(Direction::Received) => self.received.add(with_prefix),
            Some(Direction::Unspecified) => self.unspecified.add(with_prefix),
            None => self.other.add(with_prefix),
        }
        for (field, table) in &mut self.groups {
            let key = entry.header_value(wrapper, field).unwrap_or_default();
            table.entry(key).or_default().add(with_prefix);
        }

        add(&mut self.prefixes, entry.prefix);
        if is_unknown_type(entry) {
            for f in &entry.fields {
                add(&mut self.values, f.sizes.total());
                self.path(type_label(entry), f.sizes.total());
            }
        } else {
            self.fields(&entry.type_name, &entry.fields);
        }

        let cut = [
            ("cut content", entry.cut_content),
            ("cut, not itemised", entry.cut_other),
            ("message not captured", entry.not_captured),
            ("not in the capture, unexplained", entry.unexplained),
        ];
        for (label, bytes) in cut {
            if bytes > 0 {
                add(&mut self.missing, bytes);
                let path = if entry.type_name.is_empty() {
                    format!("({label})")
                } else {
                    format!("{} ({label})", type_label(entry))
                };
                self.path(path, bytes);
            }
        }
    }

    fn path(&mut self, path: String, bytes: u64) {
        add(self.by_field.entry(path).or_default(), bytes);
    }

    /// A leaf's path gets its whole size; a message's only its tag and
    /// length, the rest going to its fields.
    fn fields(&mut self, parent: &str, fields: &[Field]) {
        for f in fields {
            match &f.value {
                Value::Undecodable(_) => {
                    add(&mut self.undecodable, f.sizes.total());
                    self.path(format!("{parent} (not decodable)"), f.sizes.total());
                }
                Value::Message { fields, .. } => {
                    let path = format!("{parent}.{}", field_name(f));
                    add(&mut self.tags, f.sizes.tag);
                    add(&mut self.lens, f.sizes.len);
                    self.path(path.clone(), f.sizes.tag.saturating_add(f.sizes.len));
                    self.fields(&path, fields);
                }
                _ => {
                    add(&mut self.tags, f.sizes.tag);
                    add(&mut self.lens, f.sizes.len);
                    add(&mut self.values, f.sizes.value);
                    self.path(format!("{parent}.{}", field_name(f)), f.sizes.total());
                }
            }
        }
    }

    /// Writes the tables.
    pub fn write(&self, out: &mut impl Write, summary: &Summary) -> io::Result<()> {
        let events: u64 = self.events.values().sum();
        writeln!(out)?;
        writeln!(
            out,
            "Totals: {} and {} of {} read; {} B on the wire, length prefixes ({} B) included",
            plural(summary.messages, "message", "messages"),
            plural(events, "event", "events"),
            plural(summary.packets, "packet", "packets"),
            grouped(summary.wire_bytes.saturating_add(summary.prefix_bytes)),
            grouped(summary.prefix_bytes),
        )?;
        if summary.mismatches > 0 {
            writeln!(
                out,
                "Records that do not add up: {} (marked DOES NOT ADD UP); the totals count what was listed for them",
                grouped(summary.mismatches),
            )?;
        }

        let rows = |m: &BTreeMap<String, Tally>| -> Vec<Row> {
            m.iter()
                .map(|(k, t)| Row::counted(k.escape_debug().to_string(), *t))
                .collect()
        };
        table(
            out,
            "By message",
            "type",
            true,
            TOTAL,
            rows(&self.by_message),
        )?;
        if !self.events.is_empty() {
            writeln!(out)?;
            writeln!(out, "Events (never on the wire)")?;
            for (name, count) in &self.events {
                writeln!(out, "  {name:<30} {:>10}", grouped(*count))?;
            }
        }
        let fields = self
            .by_field
            .iter()
            .map(|(k, b)| Row::bytes(k.escape_debug().to_string(), *b))
            .collect();
        table(out, "By field", "path", false, TOTAL, fields)?;

        let mut overhead = vec![
            Row::bytes("length prefixes".into(), self.prefixes),
            Row::bytes("tags".into(), self.tags),
            Row::bytes("length bytes".into(), self.lens),
            Row::bytes("values".into(), self.values),
            Row::bytes("not in the capture".into(), self.missing),
        ];
        if self.undecodable > 0 {
            overhead.push(Row::bytes("not decodable".into(), self.undecodable));
        }
        table(out, "Overhead", "", false, TOTAL, overhead)?;

        let mut directions = vec![
            Row::counted("sent".into(), self.sent),
            Row::counted("received".into(), self.received),
            Row::counted("unspecified".into(), self.unspecified),
        ];
        if self.other.count > 0 {
            directions.push(Row::counted("other".into(), self.other));
        }
        table(
            out,
            "By direction (with prefixes)",
            "direction",
            true,
            SUM,
            directions,
        )?;

        for (field, values) in &self.groups {
            let title = format!("By {} (with prefixes)", field.escape_debug());
            table(out, &title, field, true, TOTAL, rows(values))?;
        }
        Ok(())
    }
}

struct Row {
    key: String,
    count: u64,
    bytes: u64,
}

impl Row {
    fn counted(key: String, t: Tally) -> Row {
        Row {
            key,
            count: t.count,
            bytes: t.bytes,
        }
    }

    fn bytes(key: String, bytes: u64) -> Row {
        Row {
            key,
            count: 0,
            bytes,
        }
    }
}

/// The last line's label: `total` for a table of the whole, `sum` for the
/// direction table, whose two directions may count one message twice.
const TOTAL: &str = "total";
const SUM: &str = "sum";

/// A table sorted by bytes, largest first, with a share of its own total
/// and a last line labelled `total_label`.
fn table(
    out: &mut impl Write,
    title: &str,
    key_head: &str,
    counted: bool,
    total_label: &str,
    mut rows: Vec<Row>,
) -> io::Result<()> {
    rows.sort_by(|a, b| b.bytes.cmp(&a.bytes).then_with(|| a.key.cmp(&b.key)));
    let count: u64 = rows.iter().fold(0u64, |sum, r| sum.saturating_add(r.count));
    let bytes: u64 = rows.iter().fold(0u64, |sum, r| sum.saturating_add(r.bytes));
    let width = rows
        .iter()
        .map(|r| r.key.chars().count())
        .chain([key_head.len(), total_label.len(), 24])
        .max()
        .unwrap_or(24);

    writeln!(out)?;
    writeln!(out, "{title}")?;
    let line = |out: &mut dyn Write, key: &str, n: Option<u64>, b: u64| -> io::Result<()> {
        write!(out, "  {key:<width$}")?;
        if counted {
            write!(out, " {:>10}", grouped(n.unwrap_or(0)))?;
        }
        writeln!(out, " {:>15} B  {:>6}", grouped(b), share(b, bytes))
    };
    if counted {
        writeln!(
            out,
            "  {key_head:<width$} {:>10} {:>17}  {:>6}",
            "count", "bytes", "share"
        )?;
    } else {
        writeln!(out, "  {key_head:<width$} {:>17}  {:>6}", "bytes", "share")?;
    }
    for r in &rows {
        line(out, &r.key, Some(r.count), r.bytes)?;
    }
    line(out, total_label, Some(count), bytes)
}

/// `b` as a share of `total`, or `-` when the total is 0.
fn share(b: u64, total: u64) -> String {
    if total == 0 {
        return "-".to_owned();
    }
    let percent = b as f64 * 100.0 / total as f64;
    if b > 0 && percent < 0.05 {
        "<0.1%".to_owned()
    } else {
        format!("{percent:.1}%")
    }
}
