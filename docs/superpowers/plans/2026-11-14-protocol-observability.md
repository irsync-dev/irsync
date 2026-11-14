# Protocol Observability Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let contributors read and watch irsync's protocol: varint frame
prefixes, a text trace of every message, a pcapng capture that opens in
stock Wireshark, TLS key logging, and a "Networking" book page with a
how-to.

**Architecture:** A new `net::tap` module observes every frame at the
framing layer. A per-connection `ConnTap` hands out per-stream
`StreamTap`s, which the stream's reader or writer passes to
`write_msg_tapped` / `read_msg_tapped`. When a tap is on, each message
goes to the text trace (a `tracing` event under `irsync::wire`) and/or
to a process-wide capture recorder, which wraps it in a capture-only
protobuf `Record` and writes it as an exported-PDU packet to a pcapng
file on a background thread. When both are off, a tap is a `None` and
costs a branch.

**Tech Stack:** Rust, prost 0.14, protox, prost-reflect 0.16 (already in
the dependency tree through protox), tracing, iroh (pinned fork), pcapng
written by hand, Wireshark/tshark for checks.

**Spec:** `docs/superpowers/specs/2026-11-14-protocol-observability-design.md`

## Global Constraints

- No backward compatibility: `PROTOCOL_VERSION` goes from 4 to 5, no compatibility path.
- `MAX_FRAME` stays 16 MiB and is checked before any allocation for the body.
- Bytes on the wire stay as they are apart from the prefix: no envelope message on the wire.
- Off means free: no allocation, no encoding, no context gathering when neither the trace nor the capture is on.
- Trace context is "for free" only: no extra lookups, stat calls or path resolution.
- File content never appears in the trace or the capture: `DataChunk.data` and `DeltaOp.literal` become a length and a short hash.
- Wireshark refuses a packet over 256 KiB (262,144 bytes); a capture packet never exceeds that.
- The capture opens in stock Wireshark with no plugin and no permanent setup.
- Capture file mode 0600; turning it on logs a warning naming the file.
- `SSLKEYLOGFILE` is honoured in every build, with a warning naming the file.
- Docs write home paths as `~/…`, example hosts `myclient`/`myserver`, roles client/server.
- Commit messages: a short summary line, a blank line, a brief plain-English why. No attribution trailers of any kind.
- Before each commit: `cargo test -q --workspace --all-features --no-fail-fast`; before the last one, the full regression from `AGENTS.md`.

## Review Focus

1. **A run with `-vvv` and no `RUST_LOG`.** `-vvv` sets `irsync=trace`, which would switch the wire trace on and flood the log. Expected: the wire trace stays off unless `irsync::wire` is named. Pinned in Task 4 (`verbose_defaults_keep_wire_trace_off`).
2. **A process that dies mid-session.** Expected: the capture holds every record up to shortly before the end and opens in Wireshark. Pinned in Task 5 (`records_reach_disk_without_close`).
3. **Non-UTF-8 file names in the text trace.** Expected: printed as hex, no panic, no lossy text. Pinned in Task 4 (`non_utf8_path_prints_as_hex`).
4. **A session resumed on a new connection.** Expected: the new connection gets its own fake address, and both carry the same `session_id`. Pinned in Task 3 (`second_connection_gets_new_address`).
5. **A daemon serving many sessions at once.** Expected: records from concurrent streams never interleave inside a packet; the file stays well-formed. Pinned in Task 5 (`concurrent_writers_produce_valid_file`).

---

## File Structure

| File | Responsibility |
| --- | --- |
| `crates/irsync/src/net/frame.rs` | Varint framing; untapped and tapped read/write; raw body read |
| `proto/irsync/capture/v1/record.proto` | The capture-only `Record` message |
| `crates/irsync-proto/build.rs`, `src/lib.rs` | Compile the capture proto; expose the descriptor set |
| `crates/irsync/src/net/tap/mod.rs` | `ConnTap`, `StreamTap`, `WireMsg`, the on/off decision |
| `crates/irsync/src/net/tap/text.rs` | Readable one-line rendering of any message |
| `crates/irsync/src/net/tap/scrub.rs` | Drop file content; trim to fit 256 KiB |
| `crates/irsync/src/net/tap/pcapng.rs` | pcapng and exported-PDU encoding |
| `crates/irsync/src/net/tap/capture.rs` | Recorder: file open/lock, background writer, timestamps, fake addresses |
| `crates/irsync/src/net/endpoint.rs` | Key log switch |
| `crates/irsync/tests/capture.rs` | End-to-end capture test (own binary) |
| `tools/wireshark/` | Profile files and the open command |
| `book/src/explanation/networking.md`, `book/src/contributing/protocol-debugging.md` | Docs |

---

### Task 1: Varint frame prefix

**Files:**
- Modify: `crates/irsync/src/net/frame.rs`
- Modify: `crates/irsync/src/session/engine.rs:516-527` (remove `read_frame`), its callers in `engine.rs` and `crates/irsync/src/session/single.rs:17,171`
- Modify: `crates/irsync-proto/src/lib.rs` (`PROTOCOL_VERSION = 5`)
- Modify: `crates/irsync/tests/net.rs:79,121`, `crates/irsync/tests/single_file.rs:810` (hand-built frames)
- Modify: `book/src/reference/protocol.md` (Framing row, version 5)

**Interfaces:**
- Produces: `pub async fn read_body<R: AsyncRead + Unpin>(r: &mut R) -> io::Result<Option<Vec<u8>>>` in `net::frame` (one frame's body; `None` on clean EOF). `write_msg` and `read_msg` keep their signatures. `pub fn encode_prefix(len: usize, out: &mut Vec<u8>)` for tests that hand-build frames.

- [ ] **Step 1: Rewrite the tests in `frame.rs` for the varint prefix**

Keep `zero_length_body_round_trips` and `write_refuses_oversize_message`. Change the others and add:

```rust
#[tokio::test] async fn prefix_is_protobuf_varint() {
    // A 300-byte body has the prefix AC 02.
    let msg = HelloAck { session_id: vec![7; 297], ..Default::default() };
    assert_eq!(msg.encoded_len(), 300);
    let mut out = Vec::new();
    write_msg(&mut out, &msg).await.unwrap();
    assert_eq!(&out[..2], &[0xAC, 0x02]);
    assert_eq!(out.len(), 302);
}
#[tokio::test] async fn max_frame_prefix_then_eof_is_unexpected_eof() {
    let mut p = Vec::new(); encode_prefix(MAX_FRAME, &mut p);
    assert_eq!(read_after::<Hello>(&p).await.unwrap_err().kind(), io::ErrorKind::UnexpectedEof);
}
#[tokio::test] async fn prefix_over_max_frame_is_invalid_data() {
    let mut p = Vec::new(); encode_prefix(MAX_FRAME + 1, &mut p);
    assert_eq!(read_after::<Hello>(&p).await.unwrap_err().kind(), io::ErrorKind::InvalidData);
}
#[tokio::test] async fn eof_inside_prefix_is_unexpected_eof() {
    assert_eq!(read_after::<Hello>(&[0x80]).await.unwrap_err().kind(), io::ErrorKind::UnexpectedEof);
}
#[tokio::test] async fn overlong_prefix_is_invalid_data() {
    // Six continuation bytes: more than a u32 length can need.
    assert_eq!(read_after::<Hello>(&[0x80; 6]).await.unwrap_err().kind(), io::ErrorKind::InvalidData);
}
#[tokio::test] async fn undecodable_body_is_invalid_data() {
    assert_eq!(read_after::<HelloAck>(&[3, 0x0a, 5, 0]).await.unwrap_err().kind(), io::ErrorKind::InvalidData);
}
#[tokio::test] async fn read_body_returns_raw_bytes() {
    let mut buf = Vec::new();
    write_msg(&mut buf, &HelloAck { resumed: true, ..Default::default() }).await.unwrap();
    let body = read_after_body(&buf).await.unwrap().unwrap();
    assert_eq!(HelloAck::decode(&body[..]).unwrap().resumed, true);
}
```

(`read_after_body` is the `read_body` twin of the existing `read_after` helper.)

- [ ] **Step 2: Run them and see them fail**

Run: `cargo test -q -p irsync --lib net::frame`
Expected: FAIL (wrong prefix bytes; `encode_prefix`, `read_body` undefined).

- [ ] **Step 3: Implement**

`write_msg` writes `encode_prefix(body.len())` then the body in one `write_all` of a single buffer (`prost::encoding::encode_varint`). `read_body` reads the varint one byte at a time: clean EOF before the first byte is `Ok(None)`; EOF after it is `UnexpectedEof`; a fifth byte with the continuation bit, or a value over `MAX_FRAME`, is `InvalidData`. Then the body as today (grow as bytes arrive). `read_msg` is `read_body` plus decode. Replace `engine::read_frame` with `frame::read_body` (an `Ok(None)` there becomes `UnexpectedEof`, "end of stream before reply"). Update the hand-built frames in the two test files to use `encode_prefix`. Grep `crates/` for other 4-byte assumptions (`to_be_bytes`, `+ 4`, "4-byte") and fix any that concern frames; the budgets in `session/limits.rs` count body bytes, check and leave a note in the commit if unchanged.

- [ ] **Step 4: Bump the version and update the book**

`PROTOCOL_VERSION = 5`. In `protocol.md`: `PROTOCOL_VERSION` row says `5`; Framing row: "A protobuf varint length (at most 4 bytes for `MAX_FRAME`), then that many bytes of one protobuf message: protobuf's standard length-delimited form."

- [ ] **Step 5: Run the workspace tests**

Run: `cargo test -q --workspace --all-features --no-fail-fast`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git commit -am "Frame messages with a varint length prefix

Protobuf's standard length-delimited form saves bytes and matches what
protobuf tooling expects. Protocol version 5; no compatibility path."
```

---

### Task 2: The capture `Record` proto

**Files:**
- Create: `proto/irsync/capture/v1/record.proto`
- Modify: `crates/irsync-proto/build.rs`, `crates/irsync-proto/src/lib.rs`
- Test: `crates/irsync-proto/src/lib.rs` (unit test)

**Interfaces:**
- Produces: `irsync_proto::capture::v1::{Record, record::Msg, Side, Direction, StreamKind, StreamEvent, stream_event::Kind, PathChange, Trimmed, ContentSummary}`; `irsync_proto::FILE_DESCRIPTOR_SET: &[u8]` (encoded `FileDescriptorSet` for both packages).

- [ ] **Step 1: Write the proto**

```proto
syntax = "proto3";
package irsync.capture.v1;

import "irsync/v1/common.proto";
import "irsync/v1/control.proto";
import "irsync/v1/data.proto";

// One capture packet. Never sent on the wire.
message Record {
  Side side = 1;                  // the side that recorded it
  Direction direction = 2;
  StreamKind stream = 3;
  uint64 quic_stream_id = 4;
  bytes session_id = 5;           // empty until known
  optional uint64 file_id = 6;
  uint32 connection = 7;          // order of first appearance in this file, from 1
  bytes local_endpoint_id = 8;
  bytes remote_endpoint_id = 9;
  uint32 encoded_len = 10;        // the message's size on the wire, without prefix
  Trimmed trimmed = 11;           // set when anything was dropped
  oneof msg {
    irsync.v1.Hello hello = 20;
    irsync.v1.HelloAck hello_ack = 21;
    irsync.v1.Error error = 22;
    irsync.v1.ExportList export_list = 23;
    irsync.v1.PathInfo path_info = 24;
    irsync.v1.Control control = 25;
    irsync.v1.ListingStreamHeader listing_stream_header = 26;
    irsync.v1.DataStreamHeader data_stream_header = 27;
    irsync.v1.DataChunk data_chunk = 28;
    StreamEvent stream_event = 40;
    PathChange path_change = 41;
  }
}
enum Side { SIDE_UNSPECIFIED = 0; SIDE_CLIENT = 1; SIDE_DAEMON = 2; }
enum Direction { DIRECTION_UNSPECIFIED = 0; DIRECTION_SENT = 1; DIRECTION_RECEIVED = 2; }
enum StreamKind { STREAM_KIND_UNSPECIFIED = 0; STREAM_KIND_CONTROL = 1; STREAM_KIND_LISTING = 2; STREAM_KIND_DATA = 3; }
message StreamEvent {
  enum Kind { KIND_UNSPECIFIED = 0; OPENED = 1; FINISHED = 2; RESET = 3; }
  Kind kind = 1;
  string detail = 2;              // the error, for RESET
}
message PathChange { string remote = 1; } // the selected path as iroh prints it: ip:port or relay URL
message ContentSummary { uint32 len = 1; bytes blake3_prefix = 2; } // 8 bytes
message Trimmed {
  uint32 original_len = 1;        // the message's encoded size before trimming
  uint32 dropped_entries = 2;     // repeated entries removed to fit
  repeated ContentSummary content = 3; // file bytes removed, in message order
}
```

Check each imported message name against `proto/irsync/v1/*.proto` and adjust the imports to where each is defined.

- [ ] **Step 2: Write the failing test**

```rust
#[test]
fn descriptor_set_has_both_packages() {
    let fds = prost_types::FileDescriptorSet::decode(FILE_DESCRIPTOR_SET).unwrap();
    let pkgs: Vec<_> = fds.file.iter().filter_map(|f| f.package.clone()).collect();
    assert!(pkgs.iter().any(|p| p == "irsync.v1"));
    assert!(pkgs.iter().any(|p| p == "irsync.capture.v1"));
}
```

(Add `prost-types` as a dev-dependency if it isn't reachable; it is a prost companion crate.)

- [ ] **Step 3: Run it and see it fail**

Run: `cargo test -q -p irsync-proto`
Expected: FAIL to compile (`FILE_DESCRIPTOR_SET` undefined).

- [ ] **Step 4: Implement**

`build.rs` collects `.proto` files from `irsync/v1` and `irsync/capture/v1`, compiles them together, writes the encoded `fds` to `$OUT_DIR/descriptor.bin`, then generates code. `lib.rs` adds `pub mod capture { pub mod v1 { include!(…"/irsync.capture.v1.rs"); } }` and `pub static FILE_DESCRIPTOR_SET: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/descriptor.bin"));`.

- [ ] **Step 5: Run the tests**

Run: `cargo test -q -p irsync-proto`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git commit -m "Add the capture-only Record message

Every capture packet is one Record, so Wireshark needs a single message
type to decode a whole capture."
```

---

### Task 3: Taps and the on/off decision

**Files:**
- Create: `crates/irsync/src/net/tap/mod.rs`; register in `crates/irsync/src/net/mod.rs`
- Modify: `crates/irsync/src/net/frame.rs` (tapped variants)
- Modify: `crates/irsync/Cargo.toml` (`prost-reflect = "0.16"` in the workspace and here)

**Interfaces:**
- Consumes: Task 1's `read_body`, Task 2's `Record` types.
- Produces:
  - `pub trait WireMsg: prost::Message + Default + Clone { fn to_record(&self) -> record::Msg; fn file_id(&self) -> Option<u64> { None } }`, implemented for `Hello, HelloAck, Error, ExportList, PathInfo, Control, ListingStreamHeader, DataStreamHeader, DataChunk`. `Control::file_id` returns the inner message's `file_id` where it has one.
  - `#[derive(Clone, Default)] pub struct ConnTap(Option<Arc<ConnInner>>)` with `pub fn of(conn: &iroh::endpoint::Connection) -> ConnTap` (off unless `tap::wire_trace_on()` or a recorder exists; side from `conn.side()`: QUIC client = `SIDE_CLIENT`, server = `SIDE_DAEMON`), `pub fn set_session(&self, id: &[u8])`, accessors `side() -> Side`, `connection() -> u32`, `session() -> Option<[u8; 16]>` (used by tests and the recorder), `pub fn stream(&self, kind: StreamKind, quic_stream_id: u64) -> StreamTap` (records `OPENED`).
  - `#[derive(Clone, Default)] pub struct StreamTap(Option<Arc<StreamInner>>)` with `pub fn off() -> Self`, `pub fn is_on(&self) -> bool`, `pub fn sent<M: WireMsg>(&self, m: &M, encoded_len: usize)`, `pub fn received<M: WireMsg>(&self, m: &M, encoded_len: usize)`, `pub fn finished(&self)` (sent side), `pub fn reset(&self, why: &str)`. A `DataStreamHeader` seen on a stream sets that stream's file id for later `DataChunk`s.
  - `pub(crate) fn wire_trace_on() -> bool` = `tracing::enabled!(target: "irsync::wire", tracing::Level::TRACE)`.
  - Internal sink: `ConnInner`/`StreamInner` call `text::emit(&ctx, &record)` (Task 4) when the trace is on and `capture::submit(record)` (Task 5) when a recorder exists. Until those tasks land, both are stubs.
  - In `frame.rs`: `write_msg_tapped<M: WireMsg, W>(w, m: &M, tap: &StreamTap)`, `read_msg_tapped<M: WireMsg, R>(r, tap: &StreamTap) -> io::Result<Option<M>>` (records `FINISHED` on clean EOF, `RESET` on error). Only when `tap.is_on()` is `to_record` called.
  - Connection numbering: `ConnTap::of` gives each new connection the next number, from 1, process-wide.

- [ ] **Step 1: Write the failing tests** (in `tap/mod.rs`; use `tests::loopback_conn_pair()` built like `crates/irsync/tests/common::connect_pair`, or a small helper here)

```rust
#[tokio::test] async fn off_when_nothing_is_on() {
    let (c, _s) = loopback_conn_pair().await;
    assert!(!ConnTap::of(&c).stream(StreamKind::Control, 0).is_on());
}
#[tokio::test] async fn sides_follow_quic_roles() {
    let _on = force_on_for_test();                 // turns taps on without a recorder
    let (c, s) = loopback_conn_pair().await;
    assert_eq!(ConnTap::of(&c).side(), Side::Client);
    assert_eq!(ConnTap::of(&s).side(), Side::Daemon);
}
#[tokio::test] async fn second_connection_gets_new_address() {
    let _on = force_on_for_test();
    let (a, _) = loopback_conn_pair().await;
    let (b, _) = loopback_conn_pair().await;
    let (ta, tb) = (ConnTap::of(&a), ConnTap::of(&b));
    ta.set_session(&[1; 16]); tb.set_session(&[1; 16]);
    assert_ne!(ta.connection(), tb.connection());
    assert_eq!(ta.session(), tb.session());
}
#[tokio::test] async fn data_chunk_inherits_header_file_id() {
    let (tap, records) = collecting_stream_tap(StreamKind::Data, 2); // test sink
    tap.sent(&DataStreamHeader { file_id: 12, ..Default::default() }, 5);
    tap.sent(&DataChunk::default(), 0);
    assert_eq!(records.lock().unwrap()[2].file_id, Some(12)); // [0] is OPENED
}
#[tokio::test] async fn tapped_read_records_finish() {
    let (tap, records) = collecting_stream_tap(StreamKind::Control, 0);
    let mut empty: &[u8] = &[];
    assert!(read_msg_tapped::<Control, _>(&mut empty, &tap).await.unwrap().is_none());
    assert_eq!(last_event(&records), Some(stream_event::Kind::Finished));
}
```

`force_on_for_test`, `collecting_stream_tap` and `last_event` are `#[cfg(test)]` helpers in this module.

- [ ] **Step 2: Run them and see them fail**

Run: `cargo test -q -p irsync --lib net::tap`
Expected: FAIL to compile.

- [ ] **Step 3: Implement `tap/mod.rs` and the tapped frame functions** as specified in Interfaces.

- [ ] **Step 4: Run the tests**

Run: `cargo test -q -p irsync --lib net::tap net::frame`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git commit -m "Add per-connection and per-stream taps on frames

The framing layer can now report each message with its context. A tap
that is off is a None and costs one branch."
```

---

### Task 4: The text trace

**Files:**
- Create: `crates/irsync/src/net/tap/text.rs`
- Modify: `crates/irsync/src/main.rs:86-103` (verbose filters)

**Interfaces:**
- Consumes: Task 2's `FILE_DESCRIPTOR_SET`, Task 3's sink hook.
- Produces: `pub(crate) fn render(msg: &prost_reflect::DynamicMessage) -> String`; `pub(crate) fn emit(record: &Record)` which logs one `tracing::trace!(target: "irsync::wire", side, dir, stream, quic_stream, conn, session, file, "{rendered message}")`. Session id prints as 32 hex digits, `-` until known.

Rendering rules: `Name { field: value, … }` on one line, omitting fields at their default; nested messages the same; repeated fields `[a, b]` in full; enums by name; `Hash.bytes` as hex; bytes fields `data` (in `DataChunk`) and `literal` (in `DeltaOp`) as `<N bytes, blake3 XXXXXXXXXXXXXXXX>` (first 8 bytes of the BLAKE3 hash, hex); any other bytes field as a quoted string when it is valid UTF-8 with no control characters, else `0x` hex.

- [ ] **Step 1: Write the failing tests**

```rust
#[test] fn renders_need_chunks() {
    let c = Control { msg: Some(Msg::NeedChunks(NeedChunks { file_id: 12, chunk_idx: vec![0, 1, 5] })) };
    assert_eq!(render(&dyn_of(&c)), "Control { need_chunks: NeedChunks { file_id: 12, chunk_idx: [0, 1, 5] } }");
}
#[test] fn data_bytes_never_print() {
    let d = DataChunk { data: vec![b'x'; 1000], ..Default::default() };
    let s = render(&dyn_of(&d));
    assert!(s.contains("<1000 bytes, blake3 "), "{s}");
    assert!(!s.contains("xxxx"), "{s}");
}
#[test] fn literal_bytes_never_print() { /* same, for a DeltaOp literal inside DataChunk.ops */ }
#[test] fn non_utf8_path_prints_as_hex() {
    let e = ListingEntry { entry: Some(FileEntry { path: vec![0x66, 0xff], ..Default::default() }) };
    let c = Control { msg: Some(Msg::Listing(e)) };
    assert!(render(&dyn_of(&c)).contains("path: 0x66ff"));
}
#[test] fn verbose_defaults_keep_wire_trace_off() {
    for v in 0..=4 {
        assert!(!filter_enables_wire(client_log_default(v, false)), "client -v×{v}");
        assert!(!filter_enables_wire(daemon_log_default(v, false)), "daemon -v×{v}");
    }
}
```

`dyn_of` encodes a message and decodes it as a `DynamicMessage` from the pool; `filter_enables_wire` builds an `EnvFilter` and checks `irsync::wire` at TRACE. Match the field names to the real `ListingEntry`/`FileEntry` definitions. The last test lives in `main.rs`'s tests.

- [ ] **Step 2: Run them and see them fail**

Run: `cargo test -q -p irsync --bins --lib text verbose_defaults`
Expected: FAIL.

- [ ] **Step 3: Implement** `render` over `prost_reflect` (descriptor pool built once from `FILE_DESCRIPTOR_SET` in a `OnceLock`), `emit`, and hook `emit` into the Task 3 sink. Append `,irsync::wire=off` to the client's `debug,irsync=trace` and the daemon's `trace` defaults.

- [ ] **Step 4: Run the tests**

Run: `cargo test -q -p irsync --bins --lib text verbose_defaults`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git commit -m "Print every protocol message under RUST_LOG=irsync::wire=trace

One line per message sent or received, with side, stream and session.
File content prints as its length and a short hash. -vvv leaves it off."
```

---

### Task 5: Capture file writer

**Files:**
- Create: `crates/irsync/src/net/tap/scrub.rs`, `crates/irsync/src/net/tap/pcapng.rs`, `crates/irsync/src/net/tap/capture.rs`

**Interfaces:**
- Consumes: Task 2 types, Task 3's sink hook.
- Produces:
  - `scrub::scrub(record: &mut Record)`: replaces `DataChunk.data` and every `DeltaOp.literal` with empty bytes, appending a `ContentSummary` each to `record.trimmed`. `scrub::fit(record: &mut Record, max: usize)`: while the encoded record exceeds `max`, removes entries from the end of the largest repeated field in the message (for `ChunkBasis`, `weak` and the matching 8-byte slices of `strong` together), counting them in `trimmed.dropped_entries`; sets `trimmed.original_len` whenever anything was dropped.
  - `pcapng::section_header() -> Vec<u8>`, `pcapng::interface(linktype: u16 = 252, snaplen: u32 = 0) -> Vec<u8>`, `pcapng::packet(ts_micros: u64, payload: &[u8]) -> Vec<u8>` (Enhanced Packet Block, little-endian, padded to 4).
  - `pcapng::exported_pdu(src: Ipv6Addr, dst: Ipv6Addr, src_port: u32, dst_port: u32, body: &[u8]) -> Vec<u8>`: TLVs, each `u16` tag BE, `u16` length BE, value padded with zeros to 4 bytes (length counts the padding): `12` (`DISSECTOR_NAME`) = `"protobuf"`, `22`/`23` (`IPV6_SRC`/`DST`), `24` (`PORT_TYPE`) = `3` (UDP) as `u32` BE, `25`/`26` (`SRC_PORT`/`DST_PORT`) as `u32` BE, `33` (`COL_PROT_TEXT`) = `"irsync"`, then `0`/`0` end, then `body`.
  - `capture::Recorder` and `capture::submit(record: Record)`. The recorder opens lazily on first use when `IRSYNC_CAPTURE` is set. The open logic: create or open the file with mode 0600 without truncating; `File::try_lock()`; if it is locked, use `<stem>.<pid>.pcapng` next to it; truncate; log `warn!("writing a protocol capture to {path}; it holds paths and file names")`. Records go over a channel to one `std::thread`, which buffers and flushes whenever the channel is empty, and writes the section header and interface first.
  - Timestamps: `base_wall + (tokio::time::Instant::now() - base_tokio)`, both bases taken when the recorder opens, so netsim's paused clock shows simulated time.
  - Fake addresses: daemon `fd69:7273::ffff`, client `fd69:7273::<connection>` (connection number in the last 32 bits). Source is the address of the side that sent the message: for `sent`, the recording side; for `received`, the peer. Ports: both `(quic_stream_id % 65535) + 1`. Session filtering uses the `Record.session_id` field, not the address, because the address must be fixed before the session id is known.
  - Path changes: when a recorder exists, `ConnTap::of` spawns a task on `conn.paths_stream()` that submits a `PathChange` record (stream kind unspecified, port 0) for each new selected path.

- [ ] **Step 1: Write the failing tests**

```rust
// scrub.rs
#[test] fn data_chunk_keeps_no_content() {
    let mut r = record_with(Msg::DataChunk(DataChunk { data: vec![1; 4096], ..Default::default() }));
    scrub(&mut r);
    assert!(data_of(&r).is_empty());
    assert_eq!(r.trimmed.unwrap().content[0].len, 4096);
}
#[test] fn big_chunk_list_fits_and_says_so() {
    let mut r = record_with(control(Msg::ChunkListPart(part_with_records(32_768))));
    let before = r.encoded_len();
    fit(&mut r, 262_144);
    assert!(r.encoded_len() <= 262_144);
    let t = r.trimmed.unwrap();
    assert_eq!(t.original_len as usize, before);
    assert!(t.dropped_entries > 0);
}
#[test] fn small_message_is_untouched() { /* fit leaves a Heartbeat as is, trimmed None */ }

// pcapng.rs
#[test] fn exported_pdu_tags_are_padded() {
    let p = exported_pdu(A, B, 1, 2, b"xy");
    assert_eq!(&p[..4], &[0, 12, 0, 8]);            // "protobuf" is already 8
    assert_eq!(&p[4..12], b"protobuf");
    assert!(p.ends_with(&[0, 0, 0, 0, b'x', b'y']));
}
#[test] fn packet_block_lengths_match() { /* EPB total length appears at both ends, payload padded to 4 */ }

// capture.rs
#[test] fn locked_file_falls_back_to_pid_name() { /* hold try_lock on f.pcapng, open recorder at f.pcapng, its path is f.<pid>.pcapng */ }
#[test] fn file_is_owner_only() { /* mode & 0o777 == 0o600 */ }
#[test] fn records_reach_disk_without_close() {
    // submit 3 records, wait until idle, read the file without dropping the recorder: 3 EPBs
}
#[test] fn concurrent_writers_produce_valid_file() {
    // 8 threads × 500 records; parse the file block by block: 4000 EPBs, every Record decodes
}
#[test] fn addresses_and_ports() {
    assert_eq!(client_addr(3), "fd69:7273::3".parse::<Ipv6Addr>().unwrap());
    assert_eq!(port_for(0), 1);
    assert_eq!(port_for(65_535), 1);
}
```

Tests that open a recorder build one with `Recorder::open(path)` directly, not through the environment variable.

- [ ] **Step 2: Run them and see them fail**

Run: `cargo test -q -p irsync --lib net::tap`
Expected: FAIL.

- [ ] **Step 3: Implement** `scrub.rs`, `pcapng.rs`, `capture.rs` and hook `capture::submit` (after `scrub` and `fit(…, 262_144 - 128)`, leaving room for the TLVs and block headers) into the Task 3 sink.

- [ ] **Step 4: Run the tests**

Run: `cargo test -q -p irsync --lib net::tap`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git commit -m "Write protocol captures as pcapng for Wireshark

IRSYNC_CAPTURE names the file. Each message is one exported-PDU packet
that Wireshark hands straight to its protobuf decoder, with fake
addresses per connection and ports per stream."
```

---

### Task 6: Tap every stream, end to end

**Files:**
- Modify: every production frame site: `crates/irsync/src/daemon.rs:485-560,923`, `crates/irsync/src/session/engine.rs:530-570,690-700,1121,1334`, `crates/irsync/src/session/single.rs:160-175`, `crates/irsync/src/session/listing_stream.rs:30-50,131,261`, `crates/irsync/src/session/control.rs:237-262,350-362` (and the callers that pass streams in), `crates/irsync/src/send.rs:514-600`, `crates/irsync/src/session/recv_io.rs:1215-1400`
- Create: `crates/irsync/tests/capture.rs`

**Interfaces:**
- Consumes: Tasks 3–5.
- Where streams are opened or accepted, build `ConnTap::of(&conn).stream(kind, u64::from(stream.id()))` and pass the `StreamTap` alongside the stream to its writer or reader; use `write_msg_tapped` / `read_msg_tapped` there. Call `tap.finished()` where a send stream is finished. Call `set_session` on the client when `HelloAck` arrives and on the daemon when it admits the session. For the `Hello` reply read with `read_body`, call `tap.received(&ack_or_error, body.len())` after decoding. After this task, `grep -n "write_msg(\|read_msg::<\|read_msg(" crates/irsync/src` shows only `frame.rs` and test code.

- [ ] **Step 1: Write the failing end-to-end test** (`tests/capture.rs`, one test in the file so the environment variable is set before anything starts)

```rust
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn push_capture_has_both_sides_of_every_message() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("c.pcapng");
    std::env::set_var("IRSYNC_CAPTURE", &path);
    let (out, _h) = common::sync_trees(|s| write_files(s, 3, 300_000), |_| {}, &common::archive_opts(), Flow::Push).await;
    assert!(out.errors.is_empty());
    let recs = read_capture(&path);              // parse pcapng, strip exported-PDU TLVs, decode Record
    let sent = recs.iter().filter(|r| r.direction() == Direction::Sent && r.msg.is_some()).count();
    let recv = recs.iter().filter(|r| r.direction() == Direction::Received && !is_event(r)).count();
    assert_eq!(sent, recv);                      // in-process: each message twice
    assert!(recs.iter().any(|r| matches!(r.msg, Some(Msg::Hello(_))) && r.side() == Side::Client));
    assert!(recs.iter().any(|r| matches!(r.msg, Some(Msg::HelloAck(_))) && r.side() == Side::Daemon));
    let chunks: Vec<_> = recs.iter().filter_map(|r| match &r.msg { Some(Msg::DataChunk(d)) => Some((r, d)), _ => None }).collect();
    assert!(!chunks.is_empty());
    assert!(chunks.iter().all(|(r, d)| d.data.is_empty() && r.file_id.is_some()));
    assert!(recs.iter().filter(|r| r.msg.is_some() && !is_event(r)).skip(2).all(|r| r.session_id.len() == 16));
}
```

- [ ] **Step 2: Run it and see it fail**

Run: `cargo test -q -p irsync --test capture`
Expected: FAIL (empty or missing capture).

- [ ] **Step 3: Wire the taps** as listed in Interfaces.

- [ ] **Step 4: Run it, then the workspace**

Run: `cargo test -q -p irsync --test capture && cargo test -q --workspace --all-features --no-fail-fast`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git commit -m "Tap every protocol stream

Every message on the control, listing and data streams now reaches the
trace and the capture, including the reply to Hello."
```

---

### Task 7: Key log

**Files:**
- Modify: `crates/irsync/src/net/endpoint.rs:322-390`

**Interfaces:**
- Produces: `fn keylog_wanted(var: Option<&OsStr>) -> bool` (true for a non-empty value). `build` calls `builder.keylog(true)` and logs `warn!("writing TLS secrets to {file} (SSLKEYLOGFILE)")` when it returns true, on both the netsim and the normal path.

- [ ] **Step 1: Write the failing test**

```rust
#[test] fn keylog_follows_the_variable() {
    assert!(!keylog_wanted(None));
    assert!(!keylog_wanted(Some(OsStr::new(""))));
    assert!(keylog_wanted(Some(OsStr::new("/tmp/k.log"))));
}
```

- [ ] **Step 2: Run it and see it fail**

Run: `cargo test -q -p irsync --lib keylog`
Expected: FAIL.

- [ ] **Step 3: Implement**

- [ ] **Step 4: Run it, then check by hand that a key log is written**

Run: `cargo test -q -p irsync --lib keylog`, then a local sync between two `irsync` processes with `SSLKEYLOGFILE=$(mktemp)` set on one side.
Expected: PASS; the file holds `CLIENT_TRAFFIC_SECRET_0` lines. If `tshark` is installed, also capture with `tshark -i lo -w x.pcapng` during the sync and check `tshark -r x.pcapng -o tls.keylog_file:<file> -Y quic.stream` lists decrypted stream frames. Record what worked (and what didn't, e.g. multipath paths) in the commit message.

- [ ] **Step 5: Commit**

```bash
git commit -m "Write TLS secrets when SSLKEYLOGFILE is set

Lets Wireshark decrypt a real QUIC capture of irsync. A warning names
the file so a variable left set elsewhere doesn't go unnoticed."
```

---

### Task 8: Wireshark check and profile

**Files:**
- Create: `tools/wireshark/open-capture.sh`, `tools/wireshark/profile/preferences`, `tools/wireshark/profile/protobuf_search_paths`, `tools/wireshark/README.md`
- Modify: `crates/irsync/tests/capture.rs` (tshark check)

**Interfaces:**
- Produces: `tools/wireshark/open-capture.sh FILE` runs
  `wireshark -o protobuf.default_type:irsync.capture.v1.Record -o 'uat:protobuf_search_paths:"<repo>/proto","TRUE"' FILE`
  (the repo found relative to the script). `TSHARK_ARGS` in the test uses the same two `-o` options.

- [ ] **Step 1: Check by hand that the layout works** (this is the spec's early check)

With `tshark` installed and a capture from `tests/capture.rs` (keep it with `IRSYNC_KEEP_CAPTURE`, or run a local sync with `IRSYNC_CAPTURE` set), run:

```sh
tshark -r c.pcapng -o protobuf.default_type:irsync.capture.v1.Record \
  -o 'uat:protobuf_search_paths:"proto","TRUE"' -V | grep -m3 'irsync.v1.Hello'
tshark -r c.pcapng -o … -o … -q -z flow,any,network
```

Expected: the first shows decoded `Hello` fields; the second shows rows between `fd69:7273::1` and `fd69:7273::ffff`. If the flow graph is empty, switch the container to fake UDP over IPv6 (link type 101, raw IP) with `-o 'uat:protobuf_udp_message_types:"1-65535","irsync.capture.v1.Record"'`, as the spec's fallback, and adjust `pcapng.rs` and the script.

- [ ] **Step 2: Add the tshark check to `tests/capture.rs`**

After the existing assertions: if `tshark` is on `PATH`, run it with `TSHARK_ARGS -T fields -e frame.number -Y 'protobuf'` and assert the number of lines equals the number of records; without `tshark`, skip silently (as `rsync_compare.rs` does without rsync).

- [ ] **Step 3: Write the profile and script**; README says what each file is and how to import the profile (Wireshark: Configuration Profiles → Import).

- [ ] **Step 4: Run**

Run: `cargo test -q -p irsync --test capture`
Expected: PASS (with and without tshark on `PATH`).

- [ ] **Step 5: Commit**

```bash
git commit -m "Add a Wireshark profile and an open-capture script

One command opens a capture in stock Wireshark with nothing configured
permanently; the profile is there for those who want it kept."
```

---

### Task 9: Docs

**Files:**
- Create: `book/src/explanation/networking.md`, `book/src/contributing/protocol-debugging.md`
- Modify: `book/src/SUMMARY.md` (Networking right after `connections.md`; the how-to among the contributing pages), `book/src/explanation/connections.md` (link to Networking instead of repeating it), `book/src/reference/protocol.md` (link to Networking from the top)

- [ ] **Step 1: Write `networking.md`, title "Networking"**

Bottom-up, one section per layer, each saying what iroh provides, the standard behind it with links, and where irsync's code touches it (file paths): finding the other side (endpoint IDs as Ed25519 public keys; DNS/pkarr, mainline DHT, mDNS lookups; relays), getting through NAT (hole punching, relay fallback), QUIC (RFC 9000) with TLS 1.3 (RFC 8446) inside it (RFC 9001), raw public keys (RFC 7250), ALPN `irsync/1` (RFC 7301), streams and flow control, the multipath extension (`draft-ietf-quic-multipath`), and irsync's layer (varint-framed protobuf, the three stream kinds, close codes; link to `protocol.md`). A stack diagram (SVG or ASCII inside the page). Finish with a walk through one real push: run a local sync of two small files with `RUST_LOG=irsync::wire=trace` and quote the trimmed trace, explaining each step.

- [ ] **Step 2: Write `protocol-debugging.md`**

Sections: the text trace (`RUST_LOG=irsync::wire=trace`, what each field means, sample line); the capture (`IRSYNC_CAPTURE`, what's in a `Record`, trimming, fake addresses and ports, the `tools/wireshark/open-capture.sh` command, useful display filters such as `pbf.irsync.capture.v1.Record.session_id` (check the real field names in Wireshark), the flow graph); the key log (`SSLKEYLOGFILE`, what Wireshark shows, that relayed traffic stays opaque); safety (captures and key logs hold paths and secrets). Screenshots of the flow graph: take them on a machine with Wireshark; if you can't, add the `tshark -z flow` text output instead and list "add screenshots" for Mike in the hand-off.

- [ ] **Step 3: Build the book**

Run: `mdbook build book`
Expected: no errors, no broken links reported.

- [ ] **Step 4: Run the full regression** (see `AGENTS.md`)

Expected: all pass, fmt and both clippy runs clean.

- [ ] **Step 5: Commit**

```bash
git commit -m "Add the Networking page and a protocol debugging how-to

Contributors get a bottom-up view of the network stack, tied to iroh and
the RFCs, and instructions for the trace, the capture and the key log."
```
