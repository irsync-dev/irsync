# `irsync pair`: ssh only for setting a server up

Date: 2026-11-24. Status: draft for review.

## Why

Today a sync to an unknown host sets the server up over ssh, then syncs.
A sync to a saved host that does not answer quietly logs in over ssh and
restarts the daemon. That is not easy to see, and it suggests that irsync
keeps using ssh. It does not: ssh is used only for setup, and data always
goes over iroh.

## Goal

ssh is used by one command only, `irsync pair HOST`. A sync (push, pull,
list) never runs ssh.

Non-goals: changes to what the setup does on the server (install, grant,
daemon start, aliases), or to how a sync finds a daemon.

Anti-goal: a sync that fails without saying what to do next. Every place
that used to fall back to ssh now stops with a message that names the fix.

## Approaches

- **A. A new top-level command, `irsync pair HOST` (chosen).** It is easy
  to find in `--help`, sits next to `daemon`, `export` and `grant`,
  and keeps all the ssh code out of a sync.
- B. `irsync host pair HOST`. Fewer top-level commands, but the setup does
  much more than save a name (install, grant, daemon start), so `host`
  hides it.
- C. Keep the setup in a sync behind a flag (`--setup`). Smallest change,
  but setup and data stay on one command line, which is the confusion.

Name: `pair` was chosen over `connect` (suggests a lasting connection),
`setup` and `install` (both say too little about the trust step and read
oddly for a restart) and `associate` (same meaning, longer).

## Decided

- **`irsync pair HOST`** does what the first sync does today: install or
  update irsync on the server, grant this client, start the daemon, save
  the alias. `HOST` takes the forms the setup takes now: an alias, an ssh
  destination, `user@host`, an IP address, `[ipv6]`.
- **`pair` makes the daemon a user service** when the server has systemd
  (Linux) or launchd (macOS), so it survives reboots and logouts. This
  replaces today's background start plus printed instructions. On Linux it
  writes the unit from "Run the daemon", runs `systemctl --user enable
  --now irsync` and `loginctl enable-linger`. On macOS it writes the
  LaunchAgent plist and loads it. A daemon that irsync started in the
  background earlier is stopped first, so the service can take the lock.
- **Running it again** for a saved host does what `--ssh` does today
  (check, update, restart; never adds back an immediate grant the owner
  removed). This also covers a host saved as a client only (the reverse
  direction).
- **A sync to an unknown host stops** and says how to pair it, roughly:
  `myserver is not a saved host or an EndpointId; to set it up over ssh:
  irsync pair myserver`. Finding a saved host by its `ssh` value stays:
  it needs no ssh.
- **A sync to a saved host whose daemon does not answer stops** where it
  now switches to ssh. The message says how to check the daemon on the
  server (the service status, the log) and, when the host has an `ssh`
  value, to run `irsync pair myserver` to start it again.
- **Flags move from sync to `pair`:** `--rsh` (and `IRSYNC_RSH`),
  `--no-install` (and `ssh_install`), `--no-remote-dht`. `--ssh` is
  removed. No deprecation period: irsync has no users yet.

## My calls (correct me)

- `pair` only pairs; it does not sync. It accepts `myserver` and
  `myserver:` alike.
- `pair` ends with a line that says ssh is done, for example
  `irsync: paired with myserver; syncs go over iroh, not ssh`, and says
  how the daemon runs (as a systemd or launchd user service, or in the
  background until the next reboot).
- **No service manager, or it fails** (no systemd, no user session bus,
  linger refused): start the daemon in the background as today, warn, and
  print the manual steps. Without linger a systemd user service would stop
  when the ssh session ends, so linger counts as part of the service.
- `--no-service` skips the service and starts the daemon in the
  background, for people who run it their own way (a system unit, say).
- A service file irsync did not write is never overwritten: `pair` uses
  it if it starts `~/.local/bin/irsync daemon`, and otherwise warns and
  leaves it alone.
- Running `pair` again after an update restarts the service, so the new
  binary runs.
- `--no-remote-dht` becomes `--no-dht` on `pair`: there is only one
  remote side there. Its help says it is the server's DHT publishing.
- The daemon-not-answering stop applies before the first connection only.
  Redialling after a dropped connection mid-transfer is unchanged.
- The hidden server-side `ssh-setup` command stays as it is.

## Left to the builder

- The exact wording of the two stop messages, in the book's style.
- How to load the LaunchAgent over ssh when nobody is logged in on the
  Mac's screen (`launchctl bootstrap` into the `gui/UID` or `user/UID`
  domain). It must survive a reboot without a screen login; if it can't,
  that falls under "service fails" above.
- How the setup recognises a service file it wrote (a marker comment, say).
- Moving the setup code out of the sync path in `main.rs`
  (`recheck_over_ssh`, the `first || forced` branch) into the `pair`
  command.
- Updating the tests (`onboard_e2e`, `ssh_client`, `isolated_e2e`) and
  the docs that describe ssh in a sync: the `--help` quick start, the
  tutorial, "Set a server up over ssh" (to become "Pair with a server over
  ssh"), the command-line and remote-address references, troubleshooting,
  the threat model, "Run the daemon", and the man page sources.
