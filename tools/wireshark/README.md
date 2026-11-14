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

The two settings, also usable with `tshark`:

```sh
-o protobuf.default_type:irsync.capture.v1.Record
-o 'uat:protobuf_search_paths:"<repo>/proto","TRUE"'
```

## Checking the layout by hand

The test in `crates/irsync/tests/capture.rs` runs tshark by itself when it is
installed. To get a capture to look at:

```sh
IRSYNC_KEEP_CAPTURE=~/cap cargo test -q -p irsync --test capture
```

then, from the repository root, with the capture at `~/cap/c.pcapng`:

```sh
tshark -r ~/cap/c.pcapng -o protobuf.default_type:irsync.capture.v1.Record \
  -o 'uat:protobuf_search_paths:"proto","TRUE"' -V | grep -m3 'irsync.v1.Hello'
tshark -r ~/cap/c.pcapng -o protobuf.default_type:irsync.capture.v1.Record \
  -o 'uat:protobuf_search_paths:"proto","TRUE"' -q -z flow,any,network
```

Expect decoded `Hello` fields from the first command, and rows between
`fd69:7273::1:0:0` and a `fd69:7273::<connection>` address from the second.

This has not been run yet (the machine it was written on had no Wireshark).
If the flow graph comes out empty, the fallback is to write fake UDP over
IPv6 instead of exported PDUs, and to map the port with
`-o 'uat:protobuf_udp_message_types:"<port>","irsync.capture.v1.Record"'`.
