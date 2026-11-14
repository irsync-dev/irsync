# Wireshark and irsync captures

`IRSYNC_CAPTURE=<file>` makes irsync write every protocol message to a
pcapng file. It opens in stock Wireshark, with no plugin. Each packet is one
`irsync.capture.v1.Record` (`proto/irsync/capture/v1/record.proto`), which
Wireshark decodes with its built-in protobuf dissector. Fake addresses: the
daemon is `fd69:7273::1:0:0`, clients are `fd69:7273::<connection>`.

## Files

- `open-capture.sh FILE` starts `wireshark` with the two settings below and
  the `proto/` directory found relative to the script. Nothing is saved.
  On macOS, `wireshark` may not be on `PATH`; use
  `/Applications/Wireshark.app/Contents/MacOS/Wireshark` (put it on `PATH`
  or edit the script).
- `profile/` is a Wireshark configuration profile for those who want the
  settings kept. In Wireshark: Configuration Profiles, Import, From
  Directory, pick `profile/`. Then edit the one line in
  Preferences, Protocols, Protobuf, Protobuf search paths so it points to
  `proto/` in your clone (the shipped line is `~/irsync/proto`, which
  Wireshark does not expand; use the full path).

The two settings, also usable with `tshark`. The search path must be
absolute: Wireshark does not resolve a relative one against the current
directory.

```sh
-o protobuf.default_type:irsync.capture.v1.Record
-o 'uat:protobuf_search_paths:"/full/path/to/irsync/proto","TRUE"'
```

## tshark on Ubuntu

Ubuntu ships an AppArmor profile for `tshark` (`/etc/apparmor.d/tshark`)
that lets it read `/tmp` but not your home directory. With the clone in your
home, tshark cannot read `proto/`, says nothing, and shows every packet as
`Message: <UNKNOWN>`. `sudo journalctl -k | grep 'profile="tshark"'` shows
the denials. Copy the protos out first:

```sh
cp -r proto /tmp/irsync-proto
```

and give `/tmp/irsync-proto` as the search path. The Wireshark GUI has no
such profile and reads `proto/` in place.

## Checking the layout by hand

The test in `crates/irsync/tests/capture.rs` runs tshark by itself when it is
installed, and fails unless every packet decodes as a `Record`. To get a
capture to look at:

```sh
IRSYNC_KEEP_CAPTURE=/tmp/cap cargo test -q -p irsync --test capture
```

then, from the repository root (use `/tmp/irsync-proto` instead of
`$PWD/proto` on Ubuntu, see above):

```sh
tshark -r /tmp/cap/c.pcapng -o protobuf.default_type:irsync.capture.v1.Record \
  -o "uat:protobuf_search_paths:\"$PWD/proto\",\"TRUE\"" -V | grep -m3 'irsync.v1.Hello'
tshark -r /tmp/cap/c.pcapng -o protobuf.default_type:irsync.capture.v1.Record \
  -o "uat:protobuf_search_paths:\"$PWD/proto\",\"TRUE\"" -q -z flow,any,network
```

The first prints `Message: irsync.v1.Hello` lines. The second draws rows
between `fd69:7273::1:0:0` and a `fd69:7273::<connection>` address, each
labelled with the record's fields, such as `side=SIDE_CLIENT
direction=DIRECTION_SENT stream=STREAM_KIND_CONTROL ... hello`.

Checked with tshark 4.6.4: every message kind decodes, and the flow graph
shows both sides.
