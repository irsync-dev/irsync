# SSH onboarding: first sync with only SSH access to the server

Date: 2026-11-10

## Why

Getting irsync working against a new server today takes five steps on two
machines (README "Example"): show the client's ID, add an export on the
server, grant the client's ID, start the daemon, then sync using the
server's ID. People have to copy identities between machines by hand and
declare exports before they can move a single file.

rsync over SSH has none of that: `rsync -av ~/Photos/ myserver:/srv/photos`
just works if you can `ssh myserver`. irsync should come much closer to that
experience.

## The experience we want

Starting point: irsync is installed on the client only, and the user can
`ssh myserver` (keys, `~/.ssh/config`, whatever they normally use).

```sh
irsync -av ~/Photos/ myserver:/srv/photos
```

The first time, irsync uses SSH to get the server ready, reports what it
did, and then syncs over iroh as usual. Every later run with `myserver:`
dials the daemon by its iroh ID directly, with no SSH, so it also works
from networks where SSH can't reach the server.

## Goals

1. **Install on the server (a).** If irsync is missing on the server and the
   server has the same OS and CPU architecture as the client, copy the
   client's own binary over SSH.
2. **Configure the server (b).** Over SSH: grant the client's key, start the
   daemon, read back the daemon's iroh ID. No hand-copying of identities.
3. **Immediate transfers (c).** No export is needed. `myserver:PATH` reaches
   any path the SSH user can access, like rsync: relative paths are under
   that user's home, absolute paths are as typed.
4. **Remember the server.** The first run saves a client host alias for
   `myserver` with the daemon's iroh ID. Later runs use the alias and never
   touch SSH unless something is wrong (see "Later runs").
5. **Just do it, then report.** The first run does not ask for
   confirmation. It makes the changes and prints a short summary of what it
   did on the server (installed binary at X, granted key Y, started daemon,
   saved alias `myserver`).

## Non-goals

- **Persistent services.** irsync does not install a systemd unit, launchd
  agent, cron entry or anything else that survives a reboot. It starts the
  daemon in the background and prints a short how-to for setting up a user
  service (systemd user unit on Linux, launchd agent on macOS) if the user
  wants the daemon to survive reboots.
- **Root.** Everything on the server happens as the SSH user. No sudo.
- **Our own SSH implementation.** irsync calls the system `ssh` binary, so
  `user@host`, `~/.ssh/config`, agents, jump hosts and the user's known-hosts
  behaviour all work as they do for rsync.
- **Backward compatibility.** Config files and the protocol may change
  freely (no users yet).

## Later item (after the release pipeline exists)

A separate workstream will build GitHub releases. There are no releases
today. Once they exist:

- When the server's OS or CPU differs from the client's, the client
  downloads the matching binary for **exactly the client's release tag**, or
  for `latest` if the client has no release tag (a dev build). The client
  downloads it, verifies it, and copies it to the server over SSH, so the
  server needs no internet access and the check happens on the trusted
  machine.
- Downloads must be protected against tampering in transit. The user asked
  for cryptographic checksums. Note: a checksum file served next to the
  binary only protects against corruption, because whoever can swap the
  binary can swap the checksum. A signature checked against a public key
  built into irsync (for example minisign) also protects against a tampered
  download. Pick the scheme with the release workstream.
- A dev client paired with a `latest` release server may not speak the same
  protocol. The resulting error must say so plainly.

Until then, if the server's OS or CPU differs from the client's and irsync
is not installed there, stop with a clear message saying what is needed
(install irsync on the server by hand), and do not change anything else.

## How it works

### Telling an alias from an SSH host

`Location::parse` already splits `HOST:PATH` the rsync way
(`crates/irsync/src/cli/location.rs`). For a remote `HOST`:

1. If `HOST` is a saved host alias or a literal EndpointId, behave as
   today: dial directly.
2. Otherwise, whenever the client has no iroh identity saved for `HOST`,
   treat it as an SSH destination (`myserver`, `user@myserver`) and run the
   first-run setup below. (User's rule: SSH is used whenever no identity is
   configured for the other host.)

### First run, over SSH

All steps run as the SSH user via the system `ssh`.

1. **Find or install irsync.** irsync keeps its own copy on the server in
   a folder of its own in the SSH user's home (for example
   `~/.local/share/irsync/bin/irsync`) and always uses that copy, never an
   irsync the user installed elsewhere. If that copy is missing or is not
   the client's exact version, check the server's OS and CPU (for example
   `uname -sm`). If they match the client's, copy the running client binary
   there. If they don't, stop (see "Later item"). Any other irsync on the
   server is left untouched (user's decision).
   Likewise, irsync only ever stops or restarts a daemon it started itself
   from its own copy; a daemon the user runs separately is left alone.
2. **Grant the client.** Add the client's EndpointId to the server's config
   with the new "any path" grant (see "Immediate mode on the daemon"). If
   the client has no identity yet, it creates one, as it does today when
   needed.
3. **Start the daemon** in the background if it is not running, detached so
   it survives the SSH session ending.
4. **Read the daemon's EndpointId** from the server.
5. **Save the alias** `myserver` → that EndpointId in the client's
   config, using the existing host-alias support (`config/hosts.rs`,
   `irsync host add`).
6. **Report** what was done, including the user-service how-to if the
   daemon was started.
7. **Sync** over iroh as a normal run with that alias.

Running the first run against a server that is already set up (irsync
installed, client already granted, daemon running) must change nothing and
still succeed. Each step checks before acting.

### Later runs

- The alias exists, so the client dials the daemon's iroh ID directly. No
  SSH.
- If the daemon does not answer, fall back to SSH to the same host and
  rerun the whole first-run setup (it changes nothing that is already in
  place), which starts the daemon again; then retry. If, over SSH, the
  daemon's EndpointId turns out to differ from the saved alias (for example
  the server was rebuilt), update the alias and say so.
- *Builder-check call:* the fallback is triggered only by "no answer" (and
  by having no identity for the host, as above). If the daemon answers and
  refuses the client (its grant was removed, or immediate mode is turned
  off), irsync stops with a clear error. It never re-adds a grant in
  response to a refusal, so a choice the server's owner made is not undone.
- Fall-back needs the SSH destination. The alias saved by the first run
  must remember it, so a later run knows where to SSH.

### Immediate mode on the daemon

Today a remote path must lie inside an export the peer is granted
(`resolve::resolve`, `authz::Grants`). The daemon's path resolution already
handles home-relative and absolute paths. What's new:

- A grant kind that allows a key **any path the daemon's user can access**,
  read and write, the same reach the SSH user has.
- **Immediate transfers keep everything exports give today**: resuming an
  interrupted run, the index that makes repeat syncs fast, and cleanup of
  half-written files. They get it by *being* exports (user's decision):
  when a client with the "any path" grant names a path that no export it
  may use covers, the daemon automatically adds an export for that path,
  granted to that client only, and the transfer runs against it like any
  other. Later transfers to the same path, or below it, reuse that export.
  *My call:* the daemon adds the export itself, not the client over SSH,
  so a later run to a new path still needs no SSH.
- A daemon setting that **rejects immediate mode**. When it is set, the
  "any path" grant is ignored, the first run's attempt to add one fails or
  is refused, and the client gets a clear error that points to exports as
  the way in.
- Existing exports and grants keep working alongside this.
- *Builder-check call:* when the server rejects immediate mode during a
  first run, the first run still saves the alias, reports that immediate
  mode is off on that server, and attempts the sync. It succeeds if an
  export the client is already granted covers the path; otherwise it fails
  with the error that points to exports.

## Constraints

- Linux and macOS, x86_64 and aarch64, as today.
- Only the system `ssh` (and whatever it needs, for example `scp` or piping
  the binary through `ssh`) is assumed on the client. On the server, only a
  POSIX shell and `uname`.
- Install location on the server must be in the SSH user's home (no root).
- With "reject immediate mode" set, the daemon neither honours the "any
  path" grant nor adds exports automatically.

## Left to the builder

- The exact SSH commands, and how the binary is copied (`scp`, or piping
  through `ssh` and `cat`).
- Where irsync is installed on the server (for example `~/.local/bin`), and
  how to find an existing one, given non-interactive SSH often has a minimal
  `PATH` (for example `~/.cargo/bin` missing).
- How the daemon is started in the background and where its log goes.
- How the "any path" grant and the "reject immediate mode" setting are
  spelled in the config and in `irsync grant`.
- How automatically added exports are named and marked as automatic, what
  happens when a new path contains an existing automatic export, and
  whether and when unused automatic exports and their indexes are cleaned
  up.
- How irsync tells its own daemon apart from one the user runs, and what
  happens if both want the same identity or port.
- How the client tells "daemon not running" from "network is slow", and how
  long it waits before falling back to SSH.
- How an SSH destination like `user@myserver` becomes an alias name, given
  the existing alias rules.
- The wording of the summary and the user-service how-to.
- Whether a client identity is created silently or with a one-line note.
- Book and README updates: the README example and "Your first sync" should
  lead with the SSH flow.
