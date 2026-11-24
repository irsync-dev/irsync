//! A per-field byte account of protobuf messages.

#![forbid(unsafe_code)]

pub mod account;
pub mod render;
pub mod schema;
pub mod totals;
pub mod walk;

use std::io::{self, Write};

use anyhow::{bail, Context as _};
use prost_reflect::MessageDescriptor;

pub use account::{raw, unwrap, Entry, Kind, Prefix};
pub use clap::Parser;
pub use schema::Schema;
pub use walk::{own_bytes, walk, Field, Sizes, Value, MAX_DEPTH};

use render::Place;
use totals::Totals;

#[derive(clap::Parser, Debug, Clone)]
#[command(
    name = "pbwalk",
    about = "List every field of a protobuf capture with its size"
)]
pub struct Args {
    /// The capture file (pcapng, exported PDUs).
    pub file: std::path::PathBuf,
    /// Load every .proto under DIR instead of the built-in protos; may be
    /// given more than once.
    #[arg(long = "proto-path", value_name = "DIR")]
    pub proto_path: Vec<std::path::PathBuf>,
    /// The type of each packet.
    #[arg(
        long = "type",
        value_name = "NAME",
        default_value = "irsync.capture.v1.Record"
    )]
    pub type_name: String,
    /// Show each packet as it is in the file, wrapper included.
    #[arg(long)]
    pub no_unwrap: bool,
    /// How a message is framed on the wire.
    #[arg(long, value_enum, default_value = "varint")]
    pub prefix: Prefix,
    /// Only records whose wrapper field F prints as V; may be given more
    /// than once.
    #[arg(long = "where", value_name = "F=V")]
    pub filters: Vec<String>,
    /// Only the messages numbered N, or N to M (the `#N` that starts each
    /// block); may be given more than once.
    #[arg(long = "message", value_name = "N[-M]", value_parser = parse_numbers)]
    pub numbers: Vec<(u64, u64)>,
    /// Only messages of type NAME, such as DataChunk; may be given more
    /// than once.
    #[arg(long = "message-type", value_name = "NAME")]
    pub message_types: Vec<String>,
    /// Wrapper fields to break the totals down by.
    #[arg(long, value_name = "F,...", value_delimiter = ',')]
    pub group_by: Vec<String>,
    /// In the tables by message, by field and by --group-by field, show
    /// only the N largest rows and fold the rest into one.
    #[arg(long, value_name = "N", value_parser = clap::value_parser!(u64).range(1..))]
    pub top: Option<u64>,
    /// Skip the per-message listing.
    #[arg(long, conflicts_with = "no_totals")]
    pub totals_only: bool,
    /// Skip the totals.
    #[arg(long)]
    pub no_totals: bool,
    /// Print long byte strings, repeated fields and header values in full.
    #[arg(long)]
    pub full: bool,
    /// Put each field's bytes in front of its line, in hex and as
    /// characters; the first 48 columns of the output are then the bytes.
    #[arg(short = 'C', long, conflicts_with = "totals_only")]
    pub canonical_hex_dump: bool,
}

#[derive(Default, Debug, PartialEq, Eq)]
pub struct Summary {
    /// Packets read from the file.
    pub packets: u64,
    /// Wire messages (or, with `--no-unwrap`, packets) that were picked:
    /// that passed `--where`, `--message` and `--message-type`.
    pub messages: u64,
    /// Their sizes on the wire, without prefixes.
    pub wire_bytes: u64,
    /// Their length prefixes.
    pub prefix_bytes: u64,
    /// Picked records whose numbers do not add up.
    pub mismatches: u64,
    /// The file ended in the middle of a block.
    pub truncated: bool,
    /// Why reading stopped early, when it stopped on anything other than
    /// the file ending in the middle of a block: the reader's own message.
    pub stopped: Option<String>,
}

/// Reads the capture from `input`, writes the listing and totals to `out`.
/// Err for anything that stops the run before a packet is read (bad
/// arguments, bad protos, not a capture). A closed `out` ends the run
/// quietly with Ok.
pub fn run(
    args: &Args,
    input: impl std::io::Read,
    out: &mut impl std::io::Write,
) -> anyhow::Result<Summary> {
    let schema = if args.proto_path.is_empty() {
        Schema::builtin()
    } else {
        Schema::from_dirs(&args.proto_path)?
    };
    let wrapper = schema.message(&args.type_name)?;
    if args.no_unwrap
        && !(args.filters.is_empty() && args.group_by.is_empty() && args.message_types.is_empty())
    {
        bail!(
            "--where, --group-by and --message-type need the wrapper unwrapped; drop --no-unwrap"
        );
    }
    for name in &args.message_types {
        check_message_type(&wrapper, name).with_context(|| format!("--message-type {name}"))?;
    }
    let filters = args
        .filters
        .iter()
        .map(|f| {
            let (name, value) = f
                .split_once('=')
                .with_context(|| format!("--where {f}: want FIELD=VALUE"))?;
            check_header_field(&wrapper, name).with_context(|| format!("--where {f}"))?;
            Ok((name, value))
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    for name in &args.group_by {
        check_header_field(&wrapper, name).with_context(|| format!("--group-by {name}"))?;
    }
    let reader = match pbcap::Reader::new(input) {
        Ok(reader) => reader,
        Err(pbcap::Error::Truncated) => {
            bail!("not a capture: the file ends before its first interface block")
        }
        Err(e) => return Err(e.into()),
    };

    let mut summary = Summary::default();
    let pass = Pass {
        args,
        wrapper: &wrapper,
        filters: &filters,
    };
    match pass.run(reader, out, &mut summary) {
        Ok(()) => Ok(summary),
        Err(e) if e.kind() == io::ErrorKind::BrokenPipe => Ok(summary),
        Err(e) => Err(anyhow::Error::new(e).context("cannot write the output")),
    }
}

/// Fails unless `name` is a wrapper field the header lists, naming those
/// that are.
fn check_header_field(wrapper: &MessageDescriptor, name: &str) -> anyhow::Result<()> {
    // The header fields are those `header_value` knows on an empty record.
    let empty = unwrap(wrapper, &[], Prefix::None);
    if empty.header_value(wrapper, name).is_some() {
        return Ok(());
    }
    let known: Vec<String> = wrapper
        .fields()
        .map(|f| f.name().to_owned())
        .filter(|f| empty.header_value(wrapper, f).is_some())
        .collect();
    bail!(
        "{} has no header field {name}; it has: {}",
        wrapper.full_name(),
        known.join(", ")
    )
}

/// Fails unless `name` is the type of a message the wrapper can carry,
/// naming those it can.
fn check_message_type(wrapper: &MessageDescriptor, name: &str) -> anyhow::Result<()> {
    let known: Vec<String> = wrapper
        .oneofs()
        .flat_map(|o| o.fields().collect::<Vec<_>>())
        .filter_map(|f| f.kind().as_message().map(|m| m.name().to_owned()))
        .collect();
    if known.iter().any(|k| k == name) {
        return Ok(());
    }
    bail!(
        "{} carries no message of type {name}; it carries: {}",
        wrapper.full_name(),
        known.join(", ")
    )
}

/// `N` or `N-M`, as the first and last number, both kept.
fn parse_numbers(s: &str) -> Result<(u64, u64), String> {
    let number = |n: &str| {
        n.trim()
            .parse::<u64>()
            .map_err(|_| format!("{n:?} is not a message number"))
    };
    let (first, last) = match s.split_once('-') {
        Some((a, b)) => (number(a)?, number(b)?),
        None => (number(s)?, number(s)?),
    };
    if first > last {
        return Err(format!("{s}: the range ends before it starts"));
    }
    Ok((first, last))
}

/// `printed` is `want`, or `want` followed by `(...)`: so `SIDE_CLIENT`
/// matches `SIDE_CLIENT(1)`.
fn matches(printed: &str, want: &str) -> bool {
    printed == want
        || printed
            .strip_prefix(want)
            .is_some_and(|rest| rest.starts_with('(') && rest.ends_with(')'))
}

/// One pass over the capture.
struct Pass<'a> {
    args: &'a Args,
    wrapper: &'a MessageDescriptor,
    filters: &'a [(&'a str, &'a str)],
}

impl Pass<'_> {
    fn run<R: io::Read>(
        &self,
        reader: pbcap::Reader<R>,
        out: &mut impl Write,
        summary: &mut Summary,
    ) -> io::Result<()> {
        let pool = self.wrapper.parent_pool();
        let top = self
            .args
            .top
            .map(|n| usize::try_from(n).unwrap_or(usize::MAX));
        let mut totals = Totals::new(&self.args.group_by, self.wrapper, top);
        let mut first_ts = None;
        // Whether anything came before the totals.
        let mut listed = false;
        for packet in reader {
            let packet = match packet {
                Ok(packet) => packet,
                Err(e) => {
                    match e {
                        pbcap::Error::Truncated => summary.truncated = true,
                        _ => summary.stopped = Some(e.to_string()),
                    }
                    let text = format!(
                        "! stopped reading after {} packets: {e}",
                        render::grouped(summary.packets)
                    );
                    render::line(out, self.args.canonical_hex_dump, &[], &text)?;
                    listed = true;
                    break;
                }
            };
            summary.packets += 1;
            let first = *first_ts.get_or_insert(packet.ts_micros);
            let entry = if self.args.no_unwrap {
                raw(self.wrapper, &packet.body)
            } else {
                unwrap(self.wrapper, &packet.body, self.args.prefix)
            };
            if !self.passes(&entry, summary.packets) {
                continue;
            }
            if entry.kind != Kind::Event {
                summary.messages += 1;
                summary.wire_bytes = summary.wire_bytes.saturating_add(entry.wire_len);
                summary.prefix_bytes = summary.prefix_bytes.saturating_add(entry.prefix);
            }
            if render::is_mismatch(&entry) {
                summary.mismatches += 1;
            }
            if !self.args.totals_only {
                let place = Place {
                    number: summary.packets,
                    micros: i128::from(packet.ts_micros) - i128::from(first),
                    packet: &packet,
                };
                let listing = render::Listing {
                    full: self.args.full,
                    hex: self.args.canonical_hex_dump,
                };
                render::entry(out, &place, &entry, pool, listing)?;
                listed = true;
            }
            totals.add(&entry, self.wrapper);
        }
        if !self.args.no_totals {
            if self.args.canonical_hex_dump {
                // Clear of the hex columns, so that cutting them out of
                // the whole output gives the bytes and nothing else.
                totals.write(&mut render::Indented::new(out), summary, listed)?;
            } else {
                totals.write(out, summary, listed)?;
            }
        }
        out.flush()
    }

    /// Whether the entry numbered `number` is picked by `--message`,
    /// `--message-type` and `--where`.
    fn passes(&self, entry: &Entry, number: u64) -> bool {
        let args = self.args;
        (args.numbers.is_empty() || args.numbers.iter().any(|&(a, b)| (a..=b).contains(&number)))
            && (args.message_types.is_empty() || args.message_types.contains(&entry.type_name))
            && self.filters.iter().all(|(name, want)| {
                entry
                    .header_value(self.wrapper, name)
                    .is_some_and(|v| matches(&v, want))
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use irsync_proto::capture::v1::{record::Msg, ContentSummary, Record, StreamEvent, Trimmed};
    use irsync_proto::v1;
    use prost::Message as _;
    use render::hex_blocks;
    use std::net::Ipv6Addr;

    const A: Ipv6Addr = Ipv6Addr::new(0xfd69, 0x7273, 0, 0, 0, 0, 0, 1);
    const B: Ipv6Addr = Ipv6Addr::new(0xfd69, 0x7273, 0, 0, 0, 0, 0, 2);

    /// A capture file holding `records`, one packet each, 12.345 ms apart.
    fn capture(records: &[Record]) -> Vec<u8> {
        let bodies: Vec<Vec<u8>> = records.iter().map(Record::encode_to_vec).collect();
        capture_bodies(&bodies)
    }

    /// [`capture`] for packets given as their bytes.
    fn capture_bodies(bodies: &[Vec<u8>]) -> Vec<u8> {
        let mut f = pbcap::write::section_header();
        f.extend(pbcap::write::interface(
            pbcap::LINKTYPE_WIRESHARK_UPPER_PDU,
            0,
        ));
        for (i, body) in bodies.iter().enumerate() {
            let pdu = pbcap::write::exported_pdu("irsync", A, B, 1, 1, body);
            f.extend(pbcap::write::packet(1_000_000 + i as u64 * 12_345, &pdu));
        }
        f
    }

    fn args(extra: &[&str]) -> Args {
        Args::parse_from(["pbwalk", "c.pcapng"].iter().chain(extra))
    }

    fn run_on(extra: &[&str], file: &[u8]) -> (Summary, String) {
        let mut out = Vec::new();
        let s = run(&args(extra), file, &mut out).unwrap();
        (s, String::from_utf8(out).unwrap())
    }

    fn hello() -> v1::Hello {
        v1::Hello {
            protocol_version: 5,
            path: b"m".to_vec(),
            ..Default::default()
        }
    }

    fn hello_ack() -> v1::HelloAck {
        v1::HelloAck {
            session_id: vec![7; 16],
            protocol_version: 5,
            ..Default::default()
        }
    }

    /// A wire message record with `encoded_len` set from the message.
    fn message(side: i32, direction: i32, msg: Msg, len: usize) -> Record {
        Record {
            side,
            direction,
            stream: 1,
            connection: 1,
            encoded_len: len as u32,
            msg: Some(msg),
            ..Default::default()
        }
    }

    fn hello_record(side: i32, direction: i32) -> Record {
        let n = hello().encoded_len();
        message(side, direction, Msg::Hello(hello()), n)
    }

    fn event_record() -> Record {
        Record {
            side: 1,
            direction: 1,
            stream: 1,
            msg: Some(Msg::StreamEvent(StreamEvent {
                kind: 1,
                detail: String::new(),
            })),
            ..Default::default()
        }
    }

    /// A DataChunk of 262,144 + 19 bytes on the wire whose data was cut.
    fn cut_chunk_record() -> Record {
        Record {
            side: 1,
            direction: 1,
            stream: 3,
            encoded_len: 262_163,
            trimmed: Some(Trimmed {
                original_len: 262_163,
                dropped_entries: 0,
                content: vec![ContentSummary {
                    len: 262_144,
                    blake3_prefix: vec![1; 8],
                }],
            }),
            msg: Some(Msg::DataChunk(v1::DataChunk::default())),
            ..Default::default()
        }
    }

    fn varint_len(n: u64) -> u64 {
        prost::encoding::encoded_len_varint(n) as u64
    }

    #[test]
    fn listing_and_summary_for_a_small_session() {
        let a = hello().encoded_len() as u64;
        let b = hello_ack().encoded_len() as u64;
        let c = 262_163;
        let p = varint_len(a) + varint_len(b) + varint_len(c);
        let records = [
            hello_record(1, 1),
            message(1, 2, Msg::HelloAck(hello_ack()), b as usize),
            event_record(),
            cut_chunk_record(),
        ];
        let (s, out) = run_on(&[], &capture(&records));
        assert_eq!(
            s,
            Summary {
                packets: 4,
                messages: 3,
                wire_bytes: a + b + c,
                prefix_bytes: p,
                mismatches: 0,
                truncated: false,
                stopped: None,
            }
        );
        for want in [
            "Hello",
            "protocol_version = 5",
            "cut content",
            "StreamEvent  event",
            "DataChunk (cut content)",
            "#2 0.012345 ",
            "side=SIDE_CLIENT(1)",
        ] {
            assert!(out.contains(want), "no {want:?} in\n{out}");
        }
        let direction = out.split("By direction").nth(1).expect(&out);
        let sum_line = direction
            .lines()
            .find(|l| l.trim_start().starts_with("sum"))
            .expect(direction);
        let total = grouped(a + b + c + p);
        assert!(sum_line.contains(&total), "{sum_line:?} lacks {total}");
        assert!(!out.contains("DOES NOT ADD UP"), "{out}");
    }

    /// The thousands separator, as the output prints it.
    fn grouped(n: u64) -> String {
        render::grouped(n)
    }

    #[test]
    fn where_filters_and_matches_defaults() {
        let records = [hello_record(1, 1), hello_record(2, 2)];
        let file = capture(&records);
        let (s, out) = run_on(&["--where", "side=SIDE_CLIENT"], &file);
        assert_eq!((s.packets, s.messages), (2, 1));
        assert!(out.contains("#1 "), "{out}");
        assert!(!out.contains("#2 "), "{out}");
        let (s2, _) = run_on(&["--where", "side=SIDE_DAEMON(2)"], &file);
        assert_eq!(s2.messages, 1);
        let (s3, _) = run_on(&["--where", "side=SIDE"], &file);
        assert_eq!(s3.messages, 0);

        let mut unset = hello_record(1, 0);
        unset.direction = 0;
        let file = capture(&[unset, hello_record(1, 1)]);
        let (s, _) = run_on(&["--where", "direction=DIRECTION_UNSPECIFIED"], &file);
        assert_eq!(s.messages, 1);
        let (s, _) = run_on(
            &[
                "--where",
                "direction=DIRECTION_SENT",
                "--where",
                "side=SIDE_CLIENT",
            ],
            &file,
        );
        assert_eq!(s.messages, 1);
    }

    #[test]
    fn where_on_an_unknown_field_is_an_error() {
        let file = capture(&[hello_record(1, 1)]);
        for bad in ["no_such=1", "encoded_len=3", "side"] {
            let mut out = Vec::new();
            let err = run(&args(&["--where", bad]), &file[..], &mut out).unwrap_err();
            assert!(out.is_empty());
            let text = format!("{err:#}");
            assert!(text.contains("--where"), "{text}");
        }
        let mut out = Vec::new();
        let err = run(&args(&["--group-by", "hello"]), &file[..], &mut out).unwrap_err();
        assert!(format!("{err:#}").contains("hello"), "{err:#}");
    }

    #[test]
    fn group_by_adds_a_table() {
        let file = capture(&[hello_record(1, 1), event_record()]);
        let (_, out) = run_on(&["--group-by", "stream,side"], &file);
        let table = out.split("By stream").nth(1).expect(&out);
        assert!(table.contains("STREAM_KIND_CONTROL(1)"), "{out}");
        assert!(out.contains("By side"), "{out}");
        let (_, plain) = run_on(&[], &file);
        assert!(!plain.contains("By stream"), "{plain}");
    }

    #[test]
    fn group_by_direction_adds_nothing() {
        let file = capture(&[hello_record(1, 1), hello_record(2, 2)]);
        let (_, plain) = run_on(&[], &file);
        let (_, grouped) = run_on(&["--group-by", "direction"], &file);
        assert_eq!(grouped, plain);
        let (_, out) = run_on(&["--group-by", "direction,stream"], &file);
        assert_eq!(out.matches("By direction").count(), 1, "{out}");
        assert!(out.contains("By stream"), "{out}");
        // Every table but the direction one ends in `total`.
        let stream = out.split("By stream").nth(1).expect(&out);
        assert!(
            stream.lines().any(|l| l.trim_start().starts_with("total")),
            "{stream}"
        );
    }

    #[test]
    fn unknown_direction_has_its_own_row() {
        let n = hello().encoded_len() as u64;
        let table = |direction: i32| {
            let (_, out) = run_on(&[], &capture(&[hello_record(1, direction)]));
            out.split("By direction").nth(1).expect(&out).to_string()
        };
        let row = |table: &str, key: &str| {
            table
                .lines()
                .find(|l| l.trim_start().starts_with(key))
                .unwrap_or_else(|| panic!("no {key} row in\n{table}"))
                .split_whitespace()
                .skip(1)
                .collect::<Vec<_>>()
                .join(" ")
        };
        let whole = format!("1 {} B 100.0%", n + 1);
        // A direction this build does not know.
        let t = table(7);
        assert_eq!(row(&t, "other"), whole);
        assert!(!t.contains("unspecified"), "{t}");
        // No direction at all.
        let t = table(0);
        assert_eq!(row(&t, "unspecified"), whole);
        assert!(!t.contains("other"), "{t}");
        // Both absent when every direction is known; sent and received stay.
        let t = table(1);
        assert!(!t.contains("other"), "{t}");
        assert!(!t.contains("unspecified"), "{t}");
        assert_eq!(row(&t, "received"), "0 0 B 0.0%");
    }

    #[test]
    fn totals_only_and_no_totals() {
        let file = capture(&[hello_record(1, 1)]);
        let (s, out) = run_on(&["--totals-only"], &file);
        assert_eq!(s.messages, 1);
        assert!(!out.contains("protocol_version = 5"), "{out}");
        assert!(out.contains("By message"), "{out}");
        let (s, out) = run_on(&["--no-totals"], &file);
        assert_eq!(s.messages, 1);
        assert!(out.contains("protocol_version = 5"), "{out}");
        assert!(!out.contains("By message"), "{out}");
        assert!(Args::try_parse_from(["pbwalk", "f", "--totals-only", "--no-totals"]).is_err());
    }

    /// The numbers of the listing's blocks: the `N` of each `#N` line.
    fn blocks(out: &str) -> Vec<u64> {
        out.lines()
            .filter_map(|l| l.strip_prefix('#')?.split(' ').next()?.parse().ok())
            .collect()
    }

    #[test]
    fn message_picks_by_number() {
        let file = capture(&[hello_record(1, 1), cut_chunk_record(), hello_record(1, 1)]);
        let (s, out) = run_on(&["--message", "2"], &file);
        assert_eq!((s.packets, s.messages), (3, 1));
        assert_eq!(blocks(&out), [2], "{out}");
        // The totals cover only what was picked.
        let by_message = out.split("By message").nth(1).expect(&out);
        assert!(!by_message.contains("Hello"), "{by_message}");
        let (s, out) = run_on(&["--message", "2-3"], &file);
        assert_eq!(s.messages, 2);
        assert_eq!(blocks(&out), [2, 3], "{out}");
        let (s, _) = run_on(&["--message", "3", "--message", "1"], &file);
        assert_eq!(s.messages, 2);
        let (s, _) = run_on(&["--message", "7-9"], &file);
        assert_eq!((s.packets, s.messages), (3, 0));
        // With other filters, a message keeps its number.
        let (s, _) = run_on(&["--message", "3", "--message-type", "Hello"], &file);
        assert_eq!(s.messages, 1);
        let (s, _) = run_on(&["--no-unwrap", "--message", "1"], &file);
        assert_eq!(s.messages, 1);
        for bad in ["x", "1-", "-2", "3-1", "1-2-3"] {
            let parsed = Args::try_parse_from(["pbwalk", "f", "--message", bad]);
            assert!(parsed.is_err(), "{bad}");
        }
    }

    #[test]
    fn message_type_picks_by_type() {
        let file = capture(&[hello_record(1, 1), cut_chunk_record(), hello_record(1, 1)]);
        let (s, out) = run_on(&["--message-type", "DataChunk"], &file);
        assert_eq!(s.messages, 1);
        assert_eq!(blocks(&out), [2], "{out}");
        let (s, _) = run_on(&["--message-type", "Hello"], &file);
        assert_eq!(s.messages, 2);
        let (s, _) = run_on(
            &["--message-type", "Hello", "--message-type", "DataChunk"],
            &file,
        );
        assert_eq!(s.messages, 3);
        let mut out = Vec::new();
        let err = run(&args(&["--message-type", "Helo"]), &file[..], &mut out).unwrap_err();
        let text = format!("{err:#}");
        assert!(
            text.contains("--message-type Helo") && text.contains("Hello"),
            "{text}"
        );
        let err = run(
            &args(&["--no-unwrap", "--message-type", "Hello"]),
            &file[..],
            &mut out,
        )
        .unwrap_err();
        assert!(format!("{err:#}").contains("--no-unwrap"), "{err:#}");
    }

    #[test]
    fn totals_only_starts_with_the_totals() {
        let (_, out) = run_on(&["--totals-only"], &capture(&[hello_record(1, 1)]));
        assert!(out.starts_with("Totals: "), "{out:?}");
        // After a listing, a blank line keeps the two apart.
        let (_, out) = run_on(&[], &capture(&[hello_record(1, 1)]));
        assert!(out.contains("\n\nTotals: "), "{out}");
    }

    #[test]
    fn top_folds_the_rest_of_a_table() {
        let file = capture(&[hello_record(1, 1), cut_chunk_record()]);
        let table = |out: &str, title: &str| -> Vec<String> {
            let rest = out.split(&format!("\n{title}\n")).nth(1).expect(out);
            rest.lines()
                .skip(1)
                .take_while(|l| !l.is_empty())
                // The key column narrows when long rows are folded away.
                .map(|l| l.split_whitespace().collect::<Vec<_>>().join(" "))
                .collect()
        };
        let (_, whole) = run_on(&["--totals-only"], &file);
        let (_, top) = run_on(&["--totals-only", "--top", "1"], &file);
        let all = table(&whole, "By field");
        // At least three rows and the total, so that two or more fold.
        assert!(all.len() >= 4, "{whole}");
        let short = table(&top, "By field");
        assert_eq!(short.len(), 3, "{top}");
        assert_eq!(short[0], all[0]);
        let folded = format!("({} more rows)", all.len() - 2);
        assert!(short[1].starts_with(&folded), "{top}");
        assert_eq!(short[2], all[all.len() - 1], "the total stays the same");
        // The fixed tables are left whole.
        assert_eq!(table(&top, "Overhead"), table(&whole, "Overhead"));
        // A table no longer than N is not folded.
        let (_, wide) = run_on(&["--totals-only", "--top", "1000"], &file);
        assert_eq!(table(&wide, "By field"), all);
        assert!(Args::try_parse_from(["pbwalk", "f", "--top", "0"]).is_err());
    }

    #[test]
    fn full_prints_long_values() {
        let long = v1::Hello {
            protocol_version: 5,
            path: vec![b'a'; 100],
            ..Default::default()
        };
        let n = long.encoded_len();
        let file = capture(&[message(1, 1, Msg::Hello(long), n)]);
        let (_, out) = run_on(&[], &file);
        assert!(out.contains("… (100 bytes)"), "{out}");
        assert!(out.contains(&format!("\"{}\"", "a".repeat(32))), "{out}");
        assert!(!out.contains(&"a".repeat(33)), "{out}");
        let (_, out) = run_on(&["--full"], &file);
        assert!(out.contains(&format!("\"{}\"", "a".repeat(100))), "{out}");
        assert!(!out.contains("(100 bytes)"), "{out}");
    }

    #[test]
    fn no_unwrap_lists_the_wrapper() {
        let records = [hello_record(1, 1), event_record()];
        let bodies: u64 = records.iter().map(|r| r.encoded_len() as u64).sum();
        let (s, out) = run_on(&["--no-unwrap"], &capture(&records));
        assert!(out.contains("side = SIDE_CLIENT(1)"), "{out}");
        assert!(out.contains("encoded_len ="), "{out}");
        assert_eq!((s.messages, s.wire_bytes, s.prefix_bytes), (2, bodies, 0));
        let mut out = Vec::new();
        let file = capture(&records);
        assert!(run(
            &args(&["--no-unwrap", "--where", "side=SIDE_CLIENT"]),
            &file[..],
            &mut out
        )
        .is_err());
        assert!(run(
            &args(&["--no-unwrap", "--group-by", "side"]),
            &file[..],
            &mut out
        )
        .is_err());
    }

    #[test]
    fn mismatch_is_counted_and_shown() {
        let n = hello().encoded_len();
        let small = message(1, 1, Msg::Hello(hello()), n - 1);
        let (s, out) = run_on(&[], &capture(&[small, hello_record(1, 1)]));
        assert_eq!(s.mismatches, 1);
        assert!(out.contains("DOES NOT ADD UP"), "{out}");
    }

    #[test]
    fn undecodable_body_is_a_mismatch() {
        let mut out = Vec::new();
        let mut f = pbcap::write::section_header();
        f.extend(pbcap::write::interface(252, 0));
        let pdu = pbcap::write::exported_pdu("irsync", A, B, 1, 1, &[0xff; 8]);
        f.extend(pbcap::write::packet(1, &pdu));
        let s = run(&args(&[]), &f[..], &mut out).unwrap();
        assert_eq!(s.mismatches, 1);
        let out = String::from_utf8(out).unwrap();
        assert!(
            out.contains(
                "DOES NOT ADD UP: the record does not decode as an envelope: \
                 failed to decode Protobuf message: invalid varint; \
                 the record ends in bytes that do not parse"
            ),
            "{out}"
        );
    }

    #[test]
    fn truncated_file_lists_what_it_read() {
        let records = [hello_record(1, 1), hello_record(2, 2), hello_record(1, 1)];
        let whole = capture(&records);
        let last = pbcap::write::packet(
            0,
            &pbcap::write::exported_pdu("irsync", A, B, 1, 1, &records[2].encode_to_vec()),
        )
        .len();
        let cut = &whole[..whole.len() - last / 2];
        let (s, out) = run_on(&[], cut);
        assert_eq!((s.packets, s.truncated, s.stopped), (2, true, None));
        assert_eq!(s.messages, 2);
        assert!(out.contains("By message"), "{out}");
        assert!(out.contains("cut off"), "{out}");
    }

    /// A capture whose third block, where the first packet should be, has
    /// two lengths that differ.
    fn malformed_third_block() -> Vec<u8> {
        let mut f = capture(&[]);
        f.extend_from_slice(&6u32.to_le_bytes());
        f.extend_from_slice(&16u32.to_le_bytes());
        f.extend_from_slice(&[0; 4]);
        f.extend_from_slice(&20u32.to_le_bytes());
        f
    }

    #[test]
    fn malformed_block_stops_without_cut_off() {
        let (s, out) = run_on(&[], &malformed_third_block());
        assert!(!s.truncated);
        assert_eq!(
            s.stopped.as_deref(),
            Some("not a capture file this tool reads: a block's two lengths differ")
        );
        assert!(out.contains("By message"), "{out}");
        assert!(!out.contains("cut off"), "{out}");
    }

    #[test]
    fn empty_capture_prints_zero_totals() {
        let (s, out) = run_on(&[], &capture(&[]));
        assert_eq!(s, Summary::default());
        assert!(out.contains("By message"), "{out}");
        let total = out
            .lines()
            .find(|l| l.trim_start().starts_with("total"))
            .expect(&out);
        assert!(total.trim_end().ends_with('-'), "{total:?}");
    }

    #[test]
    fn events_only_capture() {
        let (s, out) = run_on(&[], &capture(&[event_record(), event_record()]));
        assert_eq!((s.packets, s.messages, s.wire_bytes), (2, 0, 0));
        assert_eq!(s.mismatches, 0);
        assert!(out.contains("StreamEvent  event"), "{out}");
        assert!(out.contains("By message"), "{out}");
        // Never on the wire, so its fields carry no sizes.
        let kind = out.lines().find(|l| l.contains("kind = OPENED(1)"));
        assert_eq!(kind, Some("  kind = OPENED(1)"), "{out}");
    }

    #[test]
    fn long_header_values_are_shortened() {
        let mut r = hello_record(1, 1);
        r.local_endpoint_id = vec![0xab; 32];
        let file = capture(&[r]);
        let whole = "ab".repeat(32);
        let (_, out) = run_on(&[], &file);
        let short = format!("local_endpoint_id={}…", "ab".repeat(8));
        assert!(out.contains(&short), "{out}");
        assert!(!out.contains(&whole), "{out}");
        let (_, out) = run_on(&["--full"], &file);
        assert!(out.contains(&format!("local_endpoint_id={whole}")), "{out}");
        let (s, _) = run_on(&["--where", &format!("local_endpoint_id={whole}")], &file);
        assert_eq!(s.messages, 1);
    }

    #[test]
    fn unknown_payload_type_is_labelled() {
        let mut body = Record {
            direction: 1,
            encoded_len: 5,
            ..Default::default()
        }
        .encode_to_vec();
        body.extend_from_slice(&[0xea, 0x01, 5, 1, 2, 3, 4, 5]);
        let mut f = pbcap::write::section_header();
        f.extend(pbcap::write::interface(252, 0));
        let pdu = pbcap::write::exported_pdu("irsync", A, B, 1, 1, &body);
        f.extend(pbcap::write::packet(1, &pdu));
        let (s, out) = run_on(&[], &f);
        assert_eq!((s.messages, s.wire_bytes, s.mismatches), (1, 5, 0));
        assert!(out.contains("5 bytes, type not in the protos"), "{out}");
        assert!(out.contains("#29 (type not in the protos)"), "{out}");
    }

    #[test]
    fn unknown_payload_cut_rows_use_the_type_label() {
        let mut body = Record {
            direction: 1,
            encoded_len: 15,
            trimmed: Some(Trimmed {
                original_len: 15,
                dropped_entries: 2,
                content: Vec::new(),
            }),
            ..Default::default()
        }
        .encode_to_vec();
        // Field 29, 5 bytes: a type these protos do not have.
        body.extend_from_slice(&[0xea, 0x01, 5, 1, 2, 3, 4, 5]);
        let mut f = capture(&[]);
        let pdu = pbcap::write::exported_pdu("irsync", A, B, 1, 1, &body);
        f.extend(pbcap::write::packet(1, &pdu));
        let (s, out) = run_on(&["--totals-only"], &f);
        assert_eq!((s.wire_bytes, s.mismatches), (15, 0));
        let fields = out.split("By field").nth(1).expect(&out);
        let row = fields
            .lines()
            .find(|l| l.contains("(cut, not itemised)"))
            .expect(fields);
        assert_eq!(
            row.split("  ").find(|p| !p.is_empty()).map(str::trim),
            Some("#29 (type not in the protos) (cut, not itemised)"),
            "{row:?}"
        );
    }

    #[test]
    fn control_characters_are_escaped() {
        let hello = v1::Hello {
            protocol_version: 5,
            path: b"a\nb".to_vec(),
            ..Default::default()
        };
        let n = hello.encoded_len();
        let mut r = message(1, 1, Msg::Hello(hello), n);
        r.session_id = vec![0xab];
        let (_, out) = run_on(&[], &capture(&[r]));
        // Not text: hex, on one line.
        assert!(out.contains("path = 610a62"), "{out}");
        assert!(out.contains("session_id=ab"), "{out}");
    }

    #[test]
    fn tcpdump_capture_is_refused() {
        let mut f = pbcap::write::section_header();
        f.extend(pbcap::write::interface(1, 0));
        let mut out = Vec::new();
        let err = run(&args(&[]), &f[..], &mut out).unwrap_err();
        assert!(format!("{err:#}").contains("link type 1"), "{err:#}");
        assert!(out.is_empty());
        assert!(run(&args(&[]), &b""[..], &mut out).is_err());
    }

    /// Takes 100 bytes, then reports a closed pipe.
    struct ClosesAfter100(usize);

    impl std::io::Write for ClosesAfter100 {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            if self.0 >= 100 {
                return Err(std::io::ErrorKind::BrokenPipe.into());
            }
            let n = buf.len().min(100 - self.0);
            self.0 += n;
            Ok(n)
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn closed_output_ends_quietly() {
        let file = capture(&[hello_record(1, 1), hello_record(2, 2), event_record()]);
        let mut out = ClosesAfter100(0);
        assert!(run(&args(&[]), &file[..], &mut out).is_ok());
    }

    #[test]
    fn proto_path_replaces_the_builtin() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../proto");
        let file = capture(&[hello_record(1, 1), event_record(), cut_chunk_record()]);
        let (s1, builtin) = run_on(&[], &file);
        let (s2, loaded) = run_on(&["--proto-path", dir.to_str().unwrap()], &file);
        assert_eq!(s1, s2);
        assert_eq!(builtin, loaded);
        let mut out = Vec::new();
        assert!(run(&args(&["--type", "no.such.Type"]), &file[..], &mut out).is_err());
    }

    /// The records every hex test runs over, each with the bytes of the
    /// message or event inside it as they were built: short and long
    /// fields, a nested message, an event, a message with nothing in the
    /// file, a payload of a type the protos lack (field 29, 20 bytes), a
    /// packed field of over 16 bytes, and a message with 20 bytes at its
    /// end that do not parse.
    fn hex_records() -> Vec<(Vec<u8>, Vec<u8>)> {
        // A record holding `payload` as field `number`, written by hand.
        fn by_hand(number: u8, payload: &[u8]) -> Vec<u8> {
            let mut record = Record {
                direction: 1,
                encoded_len: payload.len() as u32,
                ..Default::default()
            }
            .encode_to_vec();
            record.extend([number << 3 | 2, 0x01, payload.len() as u8]);
            record.extend(payload);
            record
        }
        let ack = v1::HelloAck {
            build: Some(v1::BuildInfo {
                version: "0.1.0".into(),
                commit: "abc".into(),
                dirty: None,
            }),
            ..hello_ack()
        };
        let event = StreamEvent {
            kind: 1,
            detail: String::new(),
        };
        let unknown: Vec<u8> = (100..120).collect();
        let need = v1::Control {
            msg: Some(v1::control::Msg::Need(v1::NeedChunks {
                file_id: 1,
                chunk_idx: (0..20).collect(),
                ..Default::default()
            })),
        };
        let mut junk_tail = hello().encode_to_vec();
        junk_tail.extend([0x0b; 20]); // group tags: they do not parse
        vec![
            (hello_record(1, 1).encode_to_vec(), hello().encode_to_vec()),
            (
                message(1, 2, Msg::HelloAck(ack.clone()), ack.encoded_len()).encode_to_vec(),
                ack.encode_to_vec(),
            ),
            (event_record().encode_to_vec(), event.encode_to_vec()),
            (cut_chunk_record().encode_to_vec(), Vec::new()),
            (by_hand(29, &unknown), unknown),
            (
                message(1, 1, Msg::Control(need.clone()), need.encoded_len()).encode_to_vec(),
                need.encode_to_vec(),
            ),
            (by_hand(20, &junk_tail), junk_tail),
        ]
    }

    /// A capture of [`hex_records`].
    fn hex_file() -> Vec<u8> {
        let bodies: Vec<Vec<u8>> = hex_records().into_iter().map(|(r, _)| r).collect();
        capture_bodies(&bodies)
    }

    #[test]
    fn hex_reassembles_each_block() {
        let (bodies, payloads): (Vec<_>, Vec<_>) = hex_records().into_iter().unzip();
        let file = hex_file();
        // As it is in the file: each block is the packet.
        let (_, out) = run_on(&["-C", "--no-unwrap", "--no-totals"], &file);
        assert_eq!(hex_blocks(&out), bodies);
        // Unwrapped: each block is the message as it was built.
        let (_, out) = run_on(&["-C", "--no-totals"], &file);
        assert_eq!(hex_blocks(&out), payloads);
        assert!(out.contains("bytes not decodable"), "{out}");
        assert!(out.contains("type not in the protos"), "{out}");
        assert!(out.contains("chunk_idx = [0, 1, 2"), "{out}");
        // --full changes the text only.
        let (_, full) = run_on(&["-C", "--no-totals", "--full"], &file);
        assert_eq!(hex_blocks(&full), payloads);
    }

    /// What `cut -c1-48` keeps of the output, without the blanks.
    fn cut_hex(out: &str) -> String {
        out.lines()
            .flat_map(|l| l.chars().take(render::HEX_WIDTH))
            .filter(|c| !c.is_whitespace())
            .collect()
    }

    #[test]
    fn totals_stay_out_of_the_hex_column() {
        let file = hex_file();
        let (_, listing) = run_on(&["-C", "--no-totals"], &file);
        let (_, both) = run_on(&["-C"], &file);
        assert!(both.contains("By message"), "{both}");
        // Cutting the hex column gives the same with the totals as without.
        assert_eq!(cut_hex(&both), cut_hex(&listing));
        // The output is that without -C, moved right; empty lines stay empty.
        let (_, plain) = run_on(&[], &file);
        let stripped: Vec<String> = both
            .lines()
            .filter(|l| l.chars().count() > render::DUMP_WIDTH)
            .map(|l| l.chars().skip(render::DUMP_WIDTH).collect())
            .collect();
        let want: Vec<&str> = plain.lines().filter(|l| !l.is_empty()).collect();
        assert_eq!(stripped, want);
        assert_eq!(
            both.lines().filter(|l| l.is_empty()).count(),
            plain.lines().filter(|l| l.is_empty()).count()
        );
    }

    #[test]
    fn hex_columns_shift_the_listing_and_nothing_else() {
        let file = hex_file();
        let (_, plain) = run_on(&["--no-totals"], &file);
        let (_, dumped) = run_on(&["-C", "--no-totals"], &file);
        // Drop the continuation lines, which are shorter than the columns,
        // and the columns from the rest: the listing without -C.
        let stripped: Vec<String> = dumped
            .lines()
            .filter(|l| l.chars().count() > render::DUMP_WIDTH)
            .map(|l| l.chars().skip(render::DUMP_WIDTH).collect())
            .collect();
        assert_eq!(stripped, plain.lines().collect::<Vec<_>>());
        assert!(dumped.lines().all(|l| !l.ends_with(' ')));
        // A 16-byte session_id with tag and length is 18 bytes: two lines.
        assert!(
            dumped.contains(&format!("\n{:<48}  |..|\n", "07 07")),
            "{dumped}"
        );
    }

    #[test]
    fn stopped_line_keeps_the_columns_clear() {
        // Two records, the second cut in half, as in
        // truncated_file_lists_what_it_read.
        let records = [hello_record(1, 1), hello_record(2, 2)];
        let whole = capture(&records);
        let last = pbcap::write::packet(
            0,
            &pbcap::write::exported_pdu("irsync", A, B, 1, 1, &records[1].encode_to_vec()),
        )
        .len();
        let cut = &whole[..whole.len() - last / 2];
        let (s, out) = run_on(&["-C", "--no-totals"], cut);
        assert!(s.truncated);
        let line = out
            .lines()
            .find(|l| l.contains("! stopped reading"))
            .unwrap();
        assert!(
            line.starts_with(&" ".repeat(render::DUMP_WIDTH)),
            "{line:?}"
        );
        assert_eq!(hex_blocks(&out).len(), 1);
    }

    #[test]
    fn hex_needs_a_listing() {
        assert!(Args::try_parse_from(["pbwalk", "-C", "--totals-only", "c"]).is_err());
        let args = Args::try_parse_from(["pbwalk", "--canonical-hex-dump", "c"]).unwrap();
        assert!(args.canonical_hex_dump);
    }
}
