# Reloading the daemon's settings

Date: 2026-10-29. Status: implemented 2026-10-29 (no separate plan; one
commit series on `main`).

## Intent

The daemon reads `irsync.toml` afresh on every connection, so exports,
grants and host aliases change without a restart. The `[daemon]` table was
read once at startup: a new `listen` port, a relay, a discovery toggle or
a new `idle_timeout` meant stopping the daemon, which ends every session
in progress. This gives the daemon the reload every long-running service
has: `SIGHUP` re-reads the file and applies the `[daemon]` settings,
without touching the sessions that are running.

## Goals

- `kill -HUP` (so `systemctl reload`) makes the daemon re-read
  `irsync.toml` and apply the `[daemon]` table: `listen`, `relay`,
  `relay_mode`, `portmap`, `discovery`, `identity`, `idle_timeout`,
  `dormant_session_timeout`.
- Sessions in progress are not interrupted: they finish on the connection
  they started on.
- A reload that cannot be applied (the file does not parse, a port cannot
  be bound, the identity cannot be read) leaves the daemon running as it
  was, with a warning naming the problem.
- The daemon says what it is reachable at after a reload, the way it does
  at startup.

## Non-goals

- Applying `state_dir` without a restart: open indexes live under it. A
  changed `state_dir` is logged at reload and applied at the next start.
- Watching the file for changes. A reload is an explicit request, as with
  every other daemon; the per-connection read of exports and grants stays.
- Changing a running session's idle timeout; the new values apply to
  sessions accepted after the reload.

## Design

A new endpoint, then a retired one. On `SIGHUP` the daemon loads the
config, keeps a `--listen` given on the command line over the file's
value, loads (or generates) the identity the file names, resolves the
listen sockets and binds a new iroh endpoint with the new relay,
discovery and port-mapping settings, exactly as at startup. The new
endpoint takes over accepting. The old one is retired: a task refuses
whatever still arrives on it (a dialer with a stale address gets a clean
refusal instead of a hang), and closes it once the connections it accepted
have all ended, or at shutdown. Each endpoint counts the connection
handlers it spawned, so "all ended" is exact.

The identity is whatever the file names. A reload that changes the
daemon's endpoint id is applied and logged at `warn`, since clients then
need the new id.

`idle_timeout` and `dormant_session_timeout` live in shared state the
connection handlers and the collection tasks read when they need them, so
the new values apply from the reload on without rebuilding anything.

Startup and reload share one function that builds the endpoint from the
settings, and one announcement of the result: `endpoint id:` and
`listening on:` lines on stdout, printed again after every successful
reload.

The reload source is a `Reloader` the binary hands to the daemon: the
config paths, the passphrase source, the command-line `listen` override, a
`Notify` the `SIGHUP` handler signals, and the announcement callback.
Tests hand in their own.

## Tests

- In process: a daemon with a config file on disk is reloaded; a client
  reaches it at the new address; a connection accepted before the reload
  still completes an exchange afterwards; once that connection ends the
  old address refuses; a reload with a broken config keeps the current
  endpoint serving.
- Binary: `kill -HUP` makes `irsync daemon` print a new `listening on:`
  line, and a sync against the new address works.
