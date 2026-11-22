# pbwalk Hex Dump Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** `pbwalk -C` puts each field's bytes in front of its line, in hex and as characters, so that the hex of one block reassembles to the message as the capture holds it.

**Architecture:** The walker records, for every field, the slice of bytes that belongs to that field's own line. The printer writes that slice in two fixed-width columns in front of every line of the listing. Nothing else in `pbwalk` changes.

**Tech Stack:** Rust, the existing `pbwalk` crate (`clap`, `prost-reflect`, `pbcap`). No new dependency.

**Spec:** `docs/superpowers/specs/2026-11-22-pbwalk-hex-dump-design.md`. Read it first. Issue: #235.

## Global Constraints

- Option names: `-C` and `--canonical-hex-dump`.
- Hex column: 48 characters. 16 bytes as two lowercase hex digits each, one space between bytes, one more after the eighth.
- Character column: two spaces after the hex column, 18 characters wide. `|`, one character per byte, `|`. Bytes 0x20 to 0x7e print as themselves, every other byte as `.`. The closing `|` comes right after the last character.
- Two spaces, then today's line. The text starts in column 71, so the two columns and their gaps take 70 characters.
- A line with more than 16 bytes continues on following lines that hold the two columns only.
- No line ends in spaces.
- Every line of the listing has the 70 characters, empty where the line has no bytes. This includes the `! stopped reading` line.
- The hex of one block, in order, is that block's message as the file holds it. No byte twice, none missing. `--full` does not affect the hex.
- Without `-C`, the output does not change by one byte.
- `-C` with `--totals-only` is refused by the argument parser.
- Every `unsafe` block needs a `// SAFETY:` comment; `pbwalk` has `#![forbid(unsafe_code)]`, keep it.
- Commit messages: a short summary line, a blank line, a plain-English why. No attribution trailers of any kind.
- Before pushing: `tools/ci.sh check`. After touching `fuzz/`: `tools/ci.sh fuzz-build` too.

## Review Focus

Inputs the spec implies but does not spell out a test for. Each has a test in the task named.

1. **A field of exactly 16 bytes.** One line, no empty continuation line after it. (Task 2, `dump_layout`.)
2. **A truncated capture under `-C`.** The `! stopped reading` line must start after column 70, or `cut -c1-48` picks up its text. (Task 2, `stopped_line_keeps_the_columns_clear`.)
3. **A message with no bytes in the file**, such as a `DataChunk` whose content was cut. Its block has no hex at all and still reassembles to the (empty) payload. (Task 2, `hex_reassembles_each_block`.)
4. **Bytes `pbwalk` cannot split, longer than 16**: an undecodable tail and a payload of unknown type. Several lines, all bytes there. (Task 1, `own_bytes_of_malformed_input`; Task 2, `hex_reassembles_each_block`.)
5. **A packed repeated field longer than 16 bytes.** One row, its tag, length and all elements over several lines. (Task 1, `own_bytes_cover_a_packed_field`.)

## File Structure

- `crates/pbwalk/src/walk.rs`: `Field::own`, `own_bytes`.
- `crates/pbwalk/src/account.rs`: the unknown-type row sets `own`.
- `crates/pbwalk/src/render.rs`: the two columns; `Listing` replaces the `full: bool` parameter.
- `crates/pbwalk/src/lib.rs`: the option; the `! stopped reading` line.
- `fuzz/fuzz_targets/pbwalk.rs`: the walker invariant; `-C` in the runs.
- `crates/irsync/tests/capture.rs`: the round trip over a real session.
- `book/src/contributing/protocol-debugging.md`: the "See the bytes" section.

---

### Task 1: The walker keeps each field's own bytes

**Files:**
- Modify: `crates/pbwalk/src/walk.rs` (`Field`, `walk_at`, `field_at`, tests)
- Modify: `crates/pbwalk/src/account.rs:148` (the unknown-type row) and its test `unknown_payload_is_shown_as_its_number`
- Modify: `crates/pbwalk/src/lib.rs` (re-export)
- Modify: `fuzz/fuzz_targets/pbwalk.rs`

**Interfaces:**
- Produces, in `walk.rs`:

  ```rust
  pub struct Field<'a> {
      // ... existing fields ...
      /// The bytes of this field's own line: its tag and length for a
      /// [`Value::Message`], whose fields carry the rest; else the whole
      /// field.
      pub own: &'a [u8],
  }

  /// Every field's own bytes, depth first: the input of the walk.
  pub fn own_bytes(fields: &[Field]) -> Vec<u8>
  ```

  `pub use walk::own_bytes` in `lib.rs`.

- [ ] **Step 1: Write the failing tests** in `walk.rs`'s test module.

  Add `own` to the three `Field { .. }` literals in `scalars_and_bytes`-style tests and to the `undecodable` helper (`own: bytes`). For the `Hello` literal the values are `own: &bytes[0..2]` and `own: &bytes[2..5]`.

  ```rust
  #[test]
  fn own_bytes_are_the_input() {
      let bytes = v1::Hello {
          protocol_version: 5,
          path: b"m".into(),
          build: Some(v1::BuildInfo { version: "0.1.0".into(), commit: "abc".into(), dirty: Some(false) }),
          ..Default::default()
      }
      .encode_to_vec();
      let fields = walk(&desc("irsync.v1.Hello"), &bytes);
      assert_eq!(own_bytes(&fields), bytes);
      let build = fields.iter().find(|f| f.name.as_deref() == Some("build")).unwrap();
      // A nested message's own bytes are its tag and length only.
      assert_eq!(build.own.len() as u64, build.sizes.tag + build.sizes.len);
      let path = fields.iter().find(|f| f.name.as_deref() == Some("path")).unwrap();
      assert_eq!(path.own.len() as u64, path.sizes.total());
  }

  #[test]
  fn own_bytes_of_malformed_input() {
      let d = desc("irsync.v1.Hello");
      let mut tail = vec![0x08, 0x05]; // protocol_version = 5
      tail.extend([0x0b; 20]); // then a group tag: 20 bytes that do not parse
      for bytes in [&[0x0b, 0x0c][..], &[0x02, 0x00], &[0x08, 0x05, 0x22, 0x09, 0x61], &tail] {
          assert_eq!(own_bytes(&walk(&d, bytes)), bytes, "{bytes:x?}");
      }
  }

  #[test]
  fn own_bytes_cover_a_packed_field() {
      // NeedChunks.chunk_idx is packed; 20 elements are over 16 bytes.
      let need = v1::NeedChunks { file_id: 1, chunk_idx: (0..20).collect(), ..Default::default() };
      let bytes = need.encode_to_vec();
      let fields = walk(&desc("irsync.v1.NeedChunks"), &bytes);
      let packed = fields.iter().find(|f| matches!(f.value, Value::Packed(_))).unwrap();
      assert!(packed.own.len() > 16);
      assert_eq!(packed.own.len() as u64, packed.sizes.total());
      assert_eq!(own_bytes(&fields), bytes);
  }

  #[test]
  fn own_bytes_of_the_session_capture() {
      let file = std::fs::read(concat!(
          env!("CARGO_MANIFEST_DIR"),
          "/../../fuzz/seeds/pbwalk/session.pcapng"
      ))
      .unwrap();
      let d = desc("irsync.capture.v1.Record");
      let mut packets = 0;
      for packet in pbcap::Reader::new(&file[..]).unwrap() {
          let body = packet.unwrap().body;
          assert_eq!(own_bytes(&walk(&d, &body)), body);
          packets += 1;
      }
      assert!(packets > 0);
  }
  ```

  In `account.rs`, `unknown_payload_is_shown_as_its_number` gains:

  ```rust
  assert_eq!(e.fields[0].own, &[1, 2, 3, 4, 5]);
  ```

- [ ] **Step 2: Run them to see them fail**

  Run: `cargo test -q -p pbwalk --lib`
  Expected: does not compile: `Field` has no field `own`, no function `own_bytes`.

- [ ] **Step 3: Implement.**

  - `field_at`: `own` is `&rest[..tag + len]` when the value is `Value::Message`, else `&rest[..tag + len + raw.len()]`. A message nested deeper than `MAX_DEPTH` is a `Value::Bytes` and so takes the whole field.
  - `walk_at`'s undecodable field: `own: rest`.
  - `account.rs:148`: `own: raw`.
  - `own_bytes`: a field's `own`, then, for a `Value::Message`, its fields' in order.

- [ ] **Step 4: Run the tests**

  Run: `cargo test -q -p pbwalk`
  Expected: all pass.

- [ ] **Step 5: The fuzz target checks the invariant.** In `fuzz/fuzz_targets/pbwalk.rs`, before the existing loop:

  ```rust
  // The walker gives every byte to exactly one field, whatever the input.
  let record = pbwalk::Schema::builtin()
      .message("irsync.capture.v1.Record")
      .unwrap();
  assert_eq!(pbwalk::own_bytes(&pbwalk::walk(&record, data)), data);
  ```

  Update the comment above the target to say so.

  Run: `tools/ci.sh fuzz-build`
  Expected: `ci.sh: passed: fuzz-build`.

- [ ] **Step 6: Commit**

  ```bash
  git add crates/pbwalk fuzz/fuzz_targets/pbwalk.rs
  git commit -m "Keep each field's own bytes in pbwalk's walker" \
    -m "The hex column prints them. A nested message keeps only its tag and length, so the fields' bytes put together are the input, which the tests and the fuzz target check."
  ```

---

### Task 2: The two columns and the option

**Files:**
- Modify: `crates/pbwalk/src/render.rs`
- Modify: `crates/pbwalk/src/lib.rs` (`Args`, the call to `render::entry`, the `! stopped reading` line, tests)

**Interfaces:**
- Consumes: `Field::own` from Task 1.
- Produces, in `render.rs`:

  ```rust
  /// Width of the hex column.
  pub const HEX_WIDTH: usize = 48;
  /// Width of both columns and the gaps after them; the text starts here.
  pub const DUMP_WIDTH: usize = 70;

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
  fn dump_line(chunk: &[u8]) -> String

  /// One line of the listing: `text`, with `bytes` in front of it when
  /// `hex`. More than 16 bytes continue on lines of their own.
  pub(crate) fn line(out: &mut impl Write, hex: bool, bytes: &[u8], text: &str) -> io::Result<()>

  pub fn entry(out: &mut impl Write, place: &Place, entry: &Entry, pool: &DescriptorPool, listing: Listing) -> io::Result<()>
  ```

- Produces, in `lib.rs`'s `Args`:

  ```rust
  /// Put each field's bytes in front of its line, in hex and as characters.
  #[arg(short = 'C', long, conflicts_with = "totals_only")]
  pub canonical_hex_dump: bool,
  ```

- [ ] **Step 1: Write the failing unit tests** in `render.rs`:

  ```rust
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
  ```

- [ ] **Step 2: Write the failing tests** in `lib.rs`'s test module. They share one helper:

  ```rust
  /// The blocks of a `-C --no-totals` listing: each one's hex column,
  /// put back into bytes. A block starts at a line with nothing in its
  /// first DUMP_WIDTH columns and `#` after them.
  fn hex_blocks(out: &str) -> Vec<Vec<u8>> {
      let mut blocks: Vec<Vec<u8>> = Vec::new();
      for l in out.lines() {
          let cols: Vec<char> = l.chars().collect();
          let dump: String = cols.iter().take(render::DUMP_WIDTH).collect();
          if dump.trim().is_empty() {
              if cols.get(render::DUMP_WIDTH) == Some(&'#') {
                  blocks.push(Vec::new());
              }
              continue;
          }
          let hex: String = cols.iter().take(render::HEX_WIDTH).filter(|c| !c.is_whitespace()).collect();
          let bytes = (0..hex.len()).step_by(2).map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap());
          blocks.last_mut().expect("hex before the first block").extend(bytes);
      }
      blocks
  }
  ```

  ```rust
  /// The records every hex test runs over: short and long fields, a
  /// nested message, an event, a message with nothing in the file, and
  /// a payload of a type the protos lack (field 29, 20 bytes).
  fn hex_records() -> Vec<Vec<u8>>   // each one an encoded Record
  ```

  Build it from `hello_record(1, 1)`, a `HelloAck` record with `build` set, `event_record()`, `cut_chunk_record()`, and the unknown-type body as `account.rs`'s test `unknown_payload_is_shown_as_its_number` builds it, with 20 payload bytes. `capture` takes `&[Record]` today; add a sibling `capture_bodies(&[Vec<u8>]) -> Vec<u8>` that writes bodies as given and make `capture` call it.

  ```rust
  #[test]
  fn hex_reassembles_each_block() {
      let bodies = hex_records();
      let file = capture_bodies(&bodies);
      // As it is in the file: each block is the packet.
      let (_, out) = run_on(&["-C", "--no-unwrap", "--no-totals"], &file);
      assert_eq!(hex_blocks(&out), bodies);
      // Unwrapped: each block is the record's payload.
      let (_, out) = run_on(&["-C", "--no-totals"], &file);
      let payloads: Vec<Vec<u8>> = bodies.iter().map(|b| payload_of(b)).collect();
      assert_eq!(hex_blocks(&out), payloads);
      assert!(payloads.iter().any(Vec::is_empty), "the cut DataChunk has no bytes");
      // --full changes the text only.
      let (_, full) = run_on(&["-C", "--no-totals", "--full"], &file);
      assert_eq!(hex_blocks(&full), payloads);
  }
  ```

  `payload_of(body)`: the value bytes of the first field numbered 20 or more in `body`, found by walking it with the `Record` descriptor (`f.own[(f.sizes.tag + f.sizes.len) as usize..]` plus `own_bytes` of its fields for a message; the `raw` of a `Value::Unknown`/`Value::Bytes`), or empty when there is none.

  ```rust
  #[test]
  fn hex_columns_shift_the_listing_and_nothing_else() {
      let file = capture_bodies(&hex_records());
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
      assert!(dumped.contains(&format!("\n{:<48}  |..|\n", "07 07")), "{dumped}");
  }

  #[test]
  fn stopped_line_keeps_the_columns_clear() {
      // Two records, the second cut in half, as in
      // truncated_file_lists_what_it_read.
      let file: Vec<u8> = /* capture(&[hello_record(1, 1), hello_record(2, 2)]) less half its last packet */;
      let (s, out) = run_on(&["-C", "--no-totals"], &file);
      assert!(s.truncated);
      let line = out.lines().find(|l| l.contains("! stopped reading")).unwrap();
      assert!(line.starts_with(&" ".repeat(render::DUMP_WIDTH)), "{line:?}");
      assert_eq!(hex_blocks(&out).len(), 1);
  }

  #[test]
  fn hex_needs_a_listing() {
      assert!(Args::try_parse_from(["pbwalk", "-C", "--totals-only", "c"]).is_err());
      assert!(Args::try_parse_from(["pbwalk", "--canonical-hex-dump", "c"]).unwrap().canonical_hex_dump);
  }
  ```

- [ ] **Step 3: Run them to see them fail**

  Run: `cargo test -q -p pbwalk --lib`
  Expected: does not compile: no `dump_line`, `line`, `DUMP_WIDTH`, `canonical_hex_dump`.

- [ ] **Step 4: Implement.**

  - `dump_line` and `line` as specified. `line` with `hex` and no bytes writes `DUMP_WIDTH` spaces, then the text.
  - `entry`, `field` and `row` write every line through `line`. `row` builds its text first. Bytes per line: a field's `own`; the row for a payload of unknown type, its field's `own`; every other line (the block's two first lines, the cut and missing rows, `DOES NOT ADD UP`), none. Events print their fields' bytes too.
  - `Style` carries `hex`; `entry` takes `Listing` in place of `full: bool`.
  - `lib.rs`: the option; `Listing { full: self.args.full, hex: self.args.canonical_hex_dump }`; the `! stopped reading` line goes through `render::line` with no bytes.

- [ ] **Step 5: Run the tests**

  Run: `cargo test -q -p pbwalk`
  Expected: all pass, the existing ones unchanged.

- [ ] **Step 6: The fuzz target runs with `-C` too.** In `fuzz/fuzz_targets/pbwalk.rs`, the list of extra arguments becomes `[&[][..], &["--no-unwrap"][..], &["-C"][..], &["-C", "--no-unwrap"][..]]`.

  Run: `tools/ci.sh fuzz-build`
  Expected: `ci.sh: passed: fuzz-build`.

- [ ] **Step 7: Look at it.**

  Run: `cargo run -q -p pbwalk -- -C --no-totals fuzz/seeds/pbwalk/session.pcapng | head -40`
  Expected: the columns line up, and
  `cargo run -q -p pbwalk -- -C --no-totals --no-unwrap fuzz/seeds/pbwalk/session.pcapng | cut -c1-48 | xxd -r -p | wc -c`
  prints the sum of the packet sizes that the same command without `-C` and with `--totals-only` reports as bytes read.

- [ ] **Step 8: Commit**

  ```bash
  git add crates/pbwalk fuzz/fuzz_targets/pbwalk.rs
  git commit -m "Add pbwalk -C: each field's bytes in hex and as characters" \
    -m "Two columns in front of every line of the listing, laid out as hexdump -C does. The hex of a block, cut out and turned back into bytes, is the message as the capture holds it."
  ```

---

### Task 3: A real session, and the book

**Files:**
- Modify: `crates/irsync/tests/capture.rs` (`check_with_pbwalk`)
- Modify: `book/src/contributing/protocol-debugging.md`

**Interfaces:**
- Consumes: `-C`, `pbwalk::render::{HEX_WIDTH, DUMP_WIDTH}` from Task 2.

- [ ] **Step 1: Write the check.** `check_with_pbwalk(path, records)` gains a second run over the same capture, with `["pbwalk", "-C", "--no-unwrap", "--no-totals", path]`:

  ```rust
  let bodies: Vec<Vec<u8>> = pbcap::Reader::new(&std::fs::read(path).unwrap()[..])
      .unwrap()
      .map(|p| p.unwrap().body)
      .collect();
  assert_eq!(hex_blocks(&out), bodies, "the hex column is not the capture");
  ```

  and a third with `["pbwalk", "-C", "--no-totals", path]`: as many blocks as records, and each block's bytes are a contiguous part of its packet's body (`body.windows(n).any(|w| w == block)`, or the block is empty).

  `hex_blocks` is the helper of Task 2, copied into this file with a comment naming its twin in `crates/pbwalk/src/lib.rs`.

- [ ] **Step 2: Run it**

  Run: `cargo test -q -p irsync --test capture`
  Expected: pass. (Task 2 is in; this step adds coverage over a real push, with cut content and events.)

- [ ] **Step 3: The book.** In `book/src/contributing/protocol-debugging.md`:

  - In the "More views" list, a last bullet: `-C` puts each field's bytes in front of its line; see [See the bytes](#see-the-bytes).
  - A new section `### See the bytes`, between "More views" and "Other protocols". It says, in the chapter's voice:
    - what `-C` / `--canonical-hex-dump` adds: the bytes of each field in hex, then the same bytes as characters, with `.` for anything that is not a printable ASCII character;
    - an example of four or five lines, taken from a real run in Step 4 of this task, that includes a field longer than 16 bytes and a nested message;
    - that a nested message's line holds only its tag and length, so no byte appears twice;
    - how to get the bytes back:

      ```sh
      cargo run -p pbwalk -- -C --no-totals ~/push.pcapng | cut -c1-48 | xxd -r -p > messages.bin
      ```

      and that this gives every message of the capture one after the other, so `--where` is the way to narrow it;
    - the two limits: the bytes are the message as captured, so file content and dropped entries are not there; and the length prefix is not in the hex, because it is not in the file.

- [ ] **Step 4: Take the example from a real run**

  Run: `cargo run -q -p pbwalk -- -C --no-totals fuzz/seeds/pbwalk/session.pcapng | sed -n 1,30p`
  Paste lines of a `HelloAck` (its `session_id` is 18 bytes) into the section, shortening the long text lines with `…` where the chapter's other examples do.

- [ ] **Step 5: Build the book's checks**

  Run: `python3 -m unittest discover -s tools/tests` (the chapter is not a man page source, so this must pass unchanged) and `mdbook build book` if `mdbook` is installed.
  Expected: pass; the new anchor resolves.

- [ ] **Step 6: The full check**

  Run: `tools/ci.sh check`
  Expected: `ci.sh: passed: check`.

- [ ] **Step 7: Commit**

  ```bash
  git add crates/irsync/tests/capture.rs book/src/contributing/protocol-debugging.md
  git commit -m "Check pbwalk's hex against a real session, and document -C" \
    -m "The capture test now puts the hex column back together and compares it with the file. The book gets a section on the option and its two limits."
  ```
