# The daemon refuses to run as root

Date: 2026-11-16

Issue: #34

## Intent

A daemon that receives a push applies what the client sends: owner, group,
permission bits, device nodes. As root, any client with write access to an
export can create setuid files owned by root and device nodes. With an
immediate grant, which ssh setup gives by default, that client can also
read and write every file on the machine.

Daemon mode has not had a security audit. Until it has, running it as root
must be a deliberate choice by the person who owns the server, never
something that happens because they logged in as root.

Today nothing stops it. The only trace is a warning line in the ssh setup
summary.

## Goals

- `irsync daemon` refuses to start as root unless the server's owner has
  opted in.
- ssh setup never starts a daemon as root.
- The refusal says why and what to do instead.

## Non-goals

- Dropping privileges to another account after start.
- Linux capabilities: a non-root daemon that was given `CAP_CHOWN` or
  similar is not detected.
- Anything other than the daemon. A sync run as root on the client side,
  and `gc`, `index` and the management commands, stay as they are.
- Keeping daemons that run as root today working. The next start of such a
  daemon fails until its owner opts in. There are no users to protect yet.

## Anti-goals

- A client must never be able to switch root on for a server. The opt-in
  lives on the server only: its command line or its config file.
- ssh setup must not install or change anything on a server before it
  refuses.

## Approaches

1. **Check in the `irsync daemon` command.** Chosen. It is the only way a
   user starts a daemon, and ssh setup starts the daemon through the same
   command. Code that uses the daemon as a library, as the in-process tests
   do, is not affected.
2. **Check inside the daemon library.** Rejected: it guards against callers
   that do not exist, and every in-process test that starts a daemon would
   fail when the suite runs as root (for example in a container).
3. **Let ssh setup proceed as root when the server's config has the opt-in
   key.** Rejected: irsync would have to be installed on the server before
   the config could be read, and the simple rule "never as root over ssh"
   is easier to trust.

## Design

### `irsync daemon`

Decided: the daemon exits with an error when it runs as root, unless one of
two things is set on the server:

- the flag `--run-as-root-i-know-what-i-am-doing` on `irsync daemon`
- a key in the config file

My calls:

- The key is `allow_root = true` under `[daemon]`, default `false`.
- "Root" means the effective user id is 0, so a start through `sudo` or a
  setuid binary counts.
- The check comes first: before the identity is loaded or created, before
  any socket is opened, before the lock file is taken.
- The exit code is 1, as for other start failures.
- When root is allowed, the daemon logs a warning at every start.

The refusal:

```
irsync daemon: refusing to run as root: the daemon has not had a security audit,
  and as root any client with write access can create setuid files and device nodes.
  Run it as an unprivileged account. To run as root anyway, pass
  --run-as-root-i-know-what-i-am-doing or set `allow_root = true` under [daemon].
```

### ssh setup

Decided: a first sync to a root account is refused, always. The opt-in on
the server does not change that.

The ssh probe already reports the user id (`id -u`) before anything is
installed. When it is 0, irsync stops there. This covers a first sync,
`--ssh`, `--no-remote-dht`, and the automatic recheck of a saved host whose
`ssh` destination is a root account.

```
irsync: root@myserver is a root account, and irsync does not set up a daemon as root.
  Use another account on myserver, or set the daemon up by hand:
  https://irsync.org/book/how-to/run-the-daemon.html
```

My calls:

- The summary line "warning: the daemon runs as root and this client can
  reach every path on HOST" can no longer be reached. It, its test and the
  `root` field it reads are removed.
- The systemd and launchd snippets printed after setup are unchanged. They
  are never printed for root, so they do not mention the flag.

### Docs

- `book/src/how-to/run-the-daemon.md`, "Do not run it as root": say the
  daemon now refuses, name the flag and the key, and replace the paragraph
  about the ssh setup warning with the refusal.
- `book/src/reference/configuration.md`: the `allow_root` row.
- `book/src/reference/command-line.md`: the flag.
- `book/src/tutorial/first-sync.md`: one sentence where the ssh way is
  introduced, that it needs a non-root account.

## Tests

- As root without opt-in: exit 1, the message above, and nothing created
  (no identity, no lock file).
- As root with the flag, and with the key: starts, and logs the warning.
- Not as root: unchanged; no warning.
- ssh setup with a probe that reports user id 0: refused, nothing run on
  the server after the probe, no host alias saved.
- The recheck of a saved host through a root ssh destination: refused the
  same way.

The suite does not run as root on CI. The tests that need the daemon to see
user id 0 need a way to pretend, behind the existing `test-hooks` feature;
how is left to the builder.

## Left to the builder

- How the tests make the daemon believe it is root.
- Where the check sits in `run_daemon`, as long as it is before the first
  side effect.
- The exact wording of the start-up warning.
