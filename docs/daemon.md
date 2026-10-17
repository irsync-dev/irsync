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

The `User=` account needs read access to the module paths (write access for
read-write modules), to the identity and `authorized_keys` files, and write
access to `state_dir`. Then `systemctl enable --now irsync` and
`journalctl -u irsync -f`.

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

## State directory and garbage collection

`state_dir` (default `~/.local/state/irsync`) holds one SQLite index per module
under `index/`: cached file hashes, partial-file progress and the daemon's
session records. It is safe to delete; the next sync rebuilds it, at the cost
of re-hashing and of losing resumable sessions.

Interrupted transfers leave `.irsync.<name>.part` files next to their
destinations. The daemon checks each module once an hour (first check a minute
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
