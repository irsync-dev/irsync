# Change exports and settings

A running daemon picks up most changes without a restart. Exports, grants and
aliases apply from the next connection. The `[daemon]` settings apply when you
send it SIGHUP. Only a few settings wait for the next start.

Run the commands on the daemon's machine, as the daemon's account and with the
daemon's `--config` (see [Run the daemon](run-the-daemon.md#run-it-as-its-own-account)).

## Exports, grants and aliases

Edit them with the [management commands](../reference/management.md), or by
hand:

```sh
irsync export add music:/srv/music
irsync grant add music myclient
irsync grant remove photos phone
```

Nothing else is needed. The daemon reads `irsync.toml`, and any `hosts.toml`
or `exports.toml` beside it, afresh on every accepted connection, once the
handshake has proved the peer's `EndpointId`. It takes the aliases, exports
and grants from that read.

- A `grant remove` or `export remove` does not end a session already in
  progress. It refuses new connections only.
- If the file cannot be read or parsed, in any table, the daemon logs the
  error at `warn` with the file and the line, rejects that connection and
  keeps running. It does not fall back to an earlier copy. A missing file
  counts as empty.
- A daemon with no exports starts, logs a warning naming the config file, and
  serves nothing until an export exists.

At startup, by contrast, a file that does not parse stops the daemon. The
management commands check the file before they write it, so an edit made with
them never leaves it unparsable; an edit by hand can.

## When a client is refused

The daemon refuses a client in one of two ways:

- A key that no grant names is closed on before the daemon reads any request.
- A key with grants that asks for a path outside what it may use is refused
  with `no access to PATH`. The answer is the same whether the path is
  outside every export or missing.

Either way the client says that the daemon rejected its key for that address,
and prints its own `EndpointId` with what to run on the daemon's machine:

```sh
irsync grant list <id>
irsync grant add EXPORT <id>
irsync export add NAME:PATH
```

`irsync grant list <id>` shows what the key may reach. The daemon's log shows
`rejecting unauthorized key` or `rejecting session`, with the first ten hex
digits of the client's `EndpointId`.

How a path finds its export, and why a grant on an outer export reaches the
exports inside it, is in
[Remote addresses](../reference/remote-addresses.md#exports-decide-access).

## Reload the daemon settings

1. Change a `[daemon]` key:

   ```sh
   irsync config set daemon.idle_timeout 1h
   ```

2. Send the daemon SIGHUP:

   ```sh
   kill -HUP <pid>
   ```

   Under the systemd unit from [Run the daemon](run-the-daemon.md#under-systemd),
   `systemctl reload irsync` does the same.

The daemon re-reads the config file and applies `listen`, `relay`,
`relay_mode`, `congestion`, `portmap`, the `[daemon.discovery]` table,
`identity`, `idle_timeout` and `dormant_session_timeout`:

- It binds a new endpoint with the new network settings and prints
  `endpoint id:` and `listening on:` again.
- Sessions in progress finish on the connection they started on. A client
  that dials an address the daemon has given up is refused.
- `idle_timeout` applies to the sessions accepted from then on.
- A `--listen` given on the command line stays in force over the file's
  `listen`.
- A changed `identity` is applied and logged at `warn`, since clients need the
  new `EndpointId`.

A reload that cannot be applied, because the file does not parse, a port
cannot be bound or the key cannot be read, is logged at `warn`, and the daemon
keeps running as it was.

## What waits for a restart

| Change | When it applies |
| --- | --- |
| `state_dir` | At the next start. A reload logs that it changed. |
| `debug_addr` | At the next start. |
| `warm_index` on an export the daemon has already opened | At the next start. |
| Removing an export | At once for new connections; the export's index stays open, and its hourly collection keeps running, until the next start. |

The daemon opens an export's index, and starts its hourly collection and its
`warm_index` warm-up, the first time the export is used: at startup for the
exports already in the file, and at the first connection naming an export
added later. Exports with the same path share one index.

## See also

- [Configuration](../reference/configuration.md#daemon): every `[daemon]` key.
- [Seed the index](seed-the-index.md): `warm_index` and `irsync export update`.
