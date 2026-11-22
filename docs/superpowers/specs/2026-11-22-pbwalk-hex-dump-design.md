# pbwalk: each field's bytes in hex

Date: 2026-11-22. Status: waiting for approval. Issue: #235.

A small change to `pbwalk` (design:
`docs/superpowers/specs/2026-11-22-pbwalk-design.md`).

## Intent

`pbwalk` lists every field of every message with its size, but not the
bytes. Mike wants the bytes next to each field, so that a field can be
checked against what is really in the capture.

The rule that defines the feature, in Mike's words: "If I would cut just
the hexdump columns and reassemble them to binary format again, I would
get a byte-identical version of the binary message."

## What it looks like

A new option, `-C` / `--canonical-hex-dump`, adds a column in front of
every line of the listing:

```text
                                                  #4 0.009941 fd69:7273::1:0:0 → fd69:7273::1 port 1  HelloAck  47 B + 1 B prefix
                                                     side=SIDE_CLIENT(1) direction=DIRECTION_RECEIVED(2) …
0a 10 53 f5 97 76 5a f6  db 17 4b 0d 9e 4e e1 12    session_id = 53f597765af6db174b0d9e4ee112ff28           18 B  (tag 1, len 1, value 16)
ff 28
10 05                                               protocol_version = 5                                     2 B  (tag 1, value 1)
1a 19                                               build: BuildInfo                                        27 B  (tag 1, len 1, value 25)
0a 0c 30 2e 31 2e 30 2d  62 65 74 61 2e 36            version = "0.1.0-beta.6"                              14 B  (tag 1, len 1, value 12)
```

(The tag bytes in this sketch are made up.)

- The column is 48 characters wide: 16 bytes as two hex digits each, one
  space between bytes and one more after the eighth, as `hexdump -C`
  prints them. Two spaces follow, then the line as it is printed today.
- A field with fewer than 16 bytes is padded with spaces, so the text
  lines up.
- A field with more than 16 bytes continues on the following lines, which
  hold hex only.
- There are no offsets and no ASCII column: `cut -c1-48 | xxd -r -p` turns
  a block back into bytes.

## Goals

- **Nothing twice, nothing missing.** The hex of one block, put together
  in order, is the block's message byte for byte, as the capture file
  holds it. A nested message's line shows only its own tag and length; its
  children show the rest.
- **The hex is always complete.** `--full` and the shortening of long
  values apply to the text only.
- **Every line of the listing has the column.** Lines without bytes leave
  it empty: a block's first two lines, the "cut content", "cut, not
  itemised", "message not captured" and "unexplained" rows, and the "DOES
  NOT ADD UP" line. Otherwise cutting the column would pick up text.
- **The hex shows what the walker decoded.** Each line's bytes are the
  bytes the walker assigned to that field, not bytes counted off by the
  printer. A test that reassembles the column then checks the walker too.

## Non-goals

- The length prefix. It is not in the capture; `pbwalk` works it out. The
  column holds only bytes that are in the file.
- The wrapper record's own fields in the default view. They are the
  block's second line and get no hex. With `--no-unwrap` the block is the
  whole packet, and so is its hex.
- Getting back the message as it was on the wire when the capture cut
  something. File content and dropped entries are not in the file. The
  hex gives the message as captured.
- Offsets, an ASCII column, colours, a way to pick one message (#233,
  item 6).

## Anti-goals

- Hex that could disagree with the decoding next to it, or that is
  byte-identical only because the printer copied the input through.
- Any change to the output without `-C`.

## Approaches

1. **The walker records each field's own bytes (chosen).** Each field
   keeps the slice the walker assigned to it: tag and length for a nested
   message, the whole field for anything else. The printer prints that
   slice. The round trip then tests the walker.
2. **The printer counts along the message.** No walker change: the
   printer takes the next N bytes for each row from the sizes it has.
   Less code, but the result is byte-identical even when the walker
   assigned bytes to the wrong field, so it proves nothing.
3. **A separate dump mode** beside the listing. Does not fit "prefix
   every field".

Mike picked 1.

## Decided

- The option's names: `-C` and `--canonical-hex-dump`.
- 16 bytes per line; pad short fields; continue long ones on the next
  line.
- No duplication; nested message lines carry their own bytes.
- Approach 1.

## Claude's calls

Mike has not seen these as decisions; change them if they are wrong.

- **Events show their bytes too.** An event was never on the wire, but
  its fields are bytes in the file, and "every field line has its bytes"
  is the simpler rule. (In the conversation I said events would be
  empty; this is a change.) Events still show no sizes.
- **`-C` with `--totals-only` is refused** by the argument parser, as
  `--totals-only` with `--no-totals` is. There is no listing to add the
  column to.
- **Continuation lines have no trailing spaces.**
- **The unknown-type row** ("N bytes, type not in the protos") and the
  "bytes not decodable" row show all their bytes: they are one row for
  bytes `pbwalk` cannot split further.
- **A packed repeated field is one line** with its tag, length and every
  element, as it is one row today.

## What changes

- `crates/pbwalk/src/walk.rs`: `Field` gains the slice of its own bytes.
  Invariant: the slices of a walk, depth first, are the input in order.
- `crates/pbwalk/src/account.rs`: the row it builds for a payload of
  unknown type carries the payload's bytes.
- `crates/pbwalk/src/render.rs`: the column, on every line of a block.
- `crates/pbwalk/src/lib.rs`: the option.
- `book/src/contributing/protocol-debugging.md`: the option under "More
  views", with a short example, the `cut | xxd` line, and the two limits
  (as captured, no prefix).

## Tests

- Walker: for the real session capture in `fuzz/seeds/pbwalk/` and for
  malformed input, the fields' slices put together equal the input. The
  fuzz target checks the same.
- Output: run with `-C --no-unwrap` over the session capture, cut the
  column out of each block, and compare with the packet's bytes from
  `pbcap`. The same in the default view against each record's payload.
- Layout: a field of fewer than 16 bytes, of exactly 16, of 17, and an
  empty column; the text starts in the same place on each.
- Without `-C` the output of the existing tests does not change.

## Left to the builder

- The names of the new field and helpers, and how the printer threads
  the column through `row` and the plain `writeln!` lines.
- Whether the output test parses the column itself or shells out.
