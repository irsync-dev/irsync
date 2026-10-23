# Running the irsync daemon

`irsync daemon` runs in the foreground and never daemonises itself; let
systemd or launchd supervise it. It reads `--config PATH`, else
`/etc/irsync/irsyncd.toml` if present, else `~/.config/irsync/irsyncd.toml`. See the
[README](../README.md) for the config file and `authorized_keys` format.

`irsync daemon --show-id` prints only the daemon's `EndpointId` on stdout and
exits; it generates the identity first if it does not exist yet.

## systemd (Linux)

`/etc/systemd/system/irsync.service`:

```ini
[Unit]
Description=irsync daemon
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
ExecStart=/usr/local/bin/irsync daemon --config /etc/irsync/irsyncd.toml
Restart=on-failure
RestartSec=5
User=irsync
Environment=RUST_LOG=info

[Install]
WantedBy=multi-user.target
```

The `User=` account needs read access to the export paths (write access for
read-write exports), to the identity and `authorized_keys` files, and write
access to `state_dir`. Then `systemctl enable --now irsync` and
`journalctl -u irsync -f`.

**Do not run the daemon as root.** A receiving daemon applies the archive
metadata the client sends (permissions, owner, group, device nodes): as root
that means any client with write access to an export can create setuid files
owned by root, files owned by other users, and device nodes in it. Run it as a
dedicated unprivileged account (`irsync` above) that owns the export trees;
ownership is then applied only where that account may (its own groups), as
rsync does without privilege.

## launchd (macOS)

`~/Library/LaunchAgents/net.irsync.daemon.plist` (use `/Library/LaunchDaemons`
for a system-wide daemon):

```xml
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key>
  <string>net.irsync.daemon</string>
  <key>ProgramArguments</key>
  <array>
    <string>/usr/local/bin/irsync</string>
    <string>daemon</string>
    <string>--config</string>
    <string>/usr/local/etc/irsync/irsyncd.toml</string>
  </array>
  <key>EnvironmentVariables</key>
  <dict>
    <key>RUST_LOG</key>
    <string>info</string>
  </dict>
  <key>RunAtLoad</key>
  <true/>
  <key>KeepAlive</key>
  <true/>
  <key>StandardErrorPath</key>
  <string>/usr/local/var/log/irsync.log</string>
</dict>
</plist>
```

Load it with `launchctl load ~/Library/LaunchAgents/net.irsync.daemon.plist`.

## Logging

Logs go to stderr (journald and the launchd `StandardErrorPath` capture it).
The default level is `info`; `-v` raises it to `debug`, `-vv` to `trace`, and
`RUST_LOG` (standard `tracing` filter syntax, e.g. `RUST_LOG=irsync=debug`)
overrides both.

## Idle connections

`idle_timeout` (default `"30m"`; a number of seconds or a duration string such
as `"90s"`, `"30m"`, `"2h"`) is how long an authenticated session may go
without activity before the daemon drops its connection; the client then
reconnects and resumes. Activity is any control message or file data on the
wire, and also local progress on either side: chunks written or copied,
hashing (bumped every 64 MiB of a large file), finalizing (the `sync` and
whole-file re-hash of a large `.part`), and each deletion. Set
`idle_timeout = "0"` to disable the limit.

A connection dropped while a large file is being finalized does not fail that
file: the resumed session waits up to 60 s for the earlier connection's
finalize to release the `.part` file before reporting it busy.

## State directory and garbage collection

`state_dir` (default `~/.local/state/irsync`) holds one SQLite index per export
under `index/`: cached file hashes, partial-file progress and the daemon's
session records. It is safe to delete; the next sync rebuilds it, at the cost
of re-hashing and of losing resumable sessions.

An export filled by something other than irsync (a prior rsync, a restore)
contributes nothing to rename detection and chunk reuse until its files are
hashed. `warm_index = true` on an export hashes them in the background when
the daemon starts (only files whose index row is not fresh are read, so a
restart is cheap); `irsync index --daemon [--config PATH] [EXPORT...]` does
the same on demand, and a session hashes destination files of the size of a
file it is about to fetch on its own.

Interrupted transfers leave `.irsync.<name>.part` files next to their
destinations. The daemon checks each export once an hour (first check a minute
after start) and walks it only when its last collection is a day old or
partial-file records older than `dormant_session_timeout` exist; it collects:

- partial-file records and `.irsync.*.part` files untouched for longer than
  `dormant_session_timeout`, unless a stored session still lists the file;
- dormant sessions not touched for `dormant_session_timeout`.

A failure to remove one file is logged and reported, and the pass carries on.
Partial-file records do not remember a `--partial-dir`: a stale record whose
`.part` file is not next to its destination is simply dropped (reported as a
record without a file), and the file itself, if it exists, is collected on a
later pass as an orphan by its age.

`dormant_session_timeout` defaults to 7 days (a number of seconds, or a
duration string as in the config). A live transfer is never affected: its
partial record is refreshed as chunks arrive and its session lists its files.
Only regular files named exactly `.irsync.<name>.part` are ever removed;
symlinks are never followed. Each run logs a summary at `info`. On a client,
`irsync gc` does the same for all local roots (add `-n` to only list).

## Relays, DHT and discovery

The daemon publishes its address so clients can find it by `EndpointId`:
`dns` uses n0's pkarr/DNS service, `dht` the mainline DHT, and `mdns` the local
network. Clients resolve with the same mechanisms (`--no-dht` or `Host` options
turn lookups off on their side).

Connections start through a relay and upgrade to a direct path when hole
punching succeeds. Relays are a fallback and much slower than a direct path.
Set `relay = "https://..."` to use your own relay, or `relay_mode =
"disabled"` for direct-only operation.

- Published DHT records carry relay-only addresses. With relays disabled they
  would carry none, so the daemon logs a warning if `dht` is on while
  `relay_mode = "disabled"`.
- Users behind a symmetric NAT (many mobile and carrier-grade NATs) on both
  ends cannot hole-punch and stay on the relay. For them we recommend a
  self-hosted relay near the daemon (iroh ships `iroh-relay`), set as `relay`
  in the daemon config and as `Relay` in the client's `Host` block (or
  `--relay`), instead of depending on the shared public relays.

### Isolated operation (localhost, LAN, no internet)

For two irsync processes on one machine or one LAN, or anywhere without
internet access, neither side needs relays or lookups. Note that `listen`
alone only pins sockets; it does not isolate the daemon. The isolated daemon
turns everything off explicitly:

```toml
listen = ["127.0.0.1:0"]   # at most one IPv4 and one IPv6 entry; port 0 = daemon picks
relay_mode = "disabled"
portmap = false            # no UPnP/PCP/NAT-PMP gateway probing
[discovery]
dht = false
dns = false
mdns = false
```

`--listen ADDR` on the command line replaces the config's `listen` list. On
startup the daemon prints its id and, with the real ports, one line per
bound socket, on stdout; a parent process reads these two kinds of line and
can dial at once:

```
endpoint id: <id>
listening on: 127.0.0.1:43210
```

The client dials directly when given at least one address, in the `Host`
block or with `--address ADDR` (repeatable; `ip:port`, `[ipv6]:port` or
`host:port`, hostnames resolved with the system resolver):

```
Host local
    EndpointId  <id>
    Address     127.0.0.1:43210
```

```sh
irsync -a --address localhost:43210 ./src <id>:m/
```

Giving an address turns the client's relay, n0 DNS, DHT, mDNS and port
mapper off: it talks to nothing but the addresses it was given. The daemon's
EndpointId is still required and still verified; `--address` together with
`--relay` is refused. iroh may still learn and probe further addresses of
the daemon itself over the connection; against a loopback daemon those are
loopback too.
