# The ssh install gives the same result as install.sh

Issue: https://github.com/irsync-dev/irsync/issues/28

## Intent

A first sync over ssh installs irsync on the server. Today that install
differs from the one `install.sh` makes:

| | `install.sh` | first sync over ssh |
|---|---|---|
| Where | `~/.local/bin` | `~/.local/share/irsync/bin` |
| Programs | `irsync` and `fileset` | `irsync` only |
| Man pages | yes | no |
| `PATH` advice | printed, per shell | none |

So a user who logs in to the server afterwards and types `irsync` or
`fileset` gets "command not found". Two rounds of user feedback reported
this, the second also for dash, fish and nushell, where the advice the
installer printed did not help.

After this change both installs put the same files in the same places and
give the same `PATH` advice, because the same script does both.

## Goals

- After an ssh install the server has `~/.local/bin/irsync`,
  `~/.local/bin/fileset` and the three man pages under
  `~/.local/share/man`, exactly as after `install.sh`.
- One script, `irsync-installer.sh`, places the files and prints the `PATH`
  advice for both installs.
- The `PATH` advice is right for the user's login shell, including dash,
  fish and nushell.
- The server still needs no internet access (decision R44).
- `~/.local/share/irsync/bin` is no longer used. A user whose service file
  still points there is told.
- `--no-install` and a client config item turn the ssh install off.

## Non-goals

- Editing shell startup files, on either install. The installer prints the
  line to add and edits nothing. (The issue assumed the old dist installer,
  which did edit them. Our own installer replaced it and deliberately does
  not. "Same behavior as `install.sh`" therefore means: print.)
- Editing the user's systemd or launchd files. We only warn.
- Finding service files under other names or in other places than the two
  the onboarding prints.
- Updating a reachable older daemon without going through ssh (still open
  in `TODO.md`).
- Signing release files (still open in `TODO.md`).

## Anti-goals

- A second install routine in Rust that can drift from the installer
  again. That drift is the bug.
- An ssh install that needs `xz`, `sha256sum`, `curl` or internet on the
  server.
- Deleting a binary that a service file still starts.

## Constraints

- The installer is POSIX `sh` and must keep working under dash and
  BusyBox `sh`.
- Decision R44: the server runs the new binary with `--version` and the
  old one is replaced only if it reports the client's exact build. This
  check stays. Before it passes nothing is installed or replaced; the only
  thing the server's home may hold is the short-lived work directory of
  section 5.
- A development build has no release to download. A FreeBSD server has no
  release tarball either.
- The installer's existing behavior for `install.sh` users (download,
  checksum, flags, `--uninstall`) must not change, except for the better
  `PATH` check below.

## Approaches

**A. Run the release installer on the server, fed a local tarball.** The
client downloads the tarball and the installer, copies both over, and the
installer reads the tarball from a directory (today's test-only hook).
Identical result, but the server needs `tar` with xz and `sha256sum`, so
Alpine and BusyBox servers fail, and development builds need a second
install path anyway.

**B. No installer; irsync places the files itself.** Extend today's
upload to send all files and write the `PATH` advice in Rust. The server
needs only `sh`, but there are two install routines again.

**C. Give the installer an "install from this unpacked directory" mode.**
The client prepares the files, sends them with the installer script, and
the server runs the script on them. **Picked.** One script does the
placing, the write checks and the `PATH` advice for both installs; the
server needs only `sh` and plain `tar`; development builds use the same
path. The cost is a second installer mode to test.

## Design

### 1. The installer gets a "from directory" mode

`sh irsync-installer.sh --from DIR` installs from an unpacked directory
instead of downloading:

- `DIR` has the tarball's layout: `irsync`, `fileset`, `man/man1/irsync.1`,
  `man/man1/fileset.1`, `man/man5/irsync.toml.5`.
- No download, no checksum, no `tar`, no platform check. The caller
  vouches for the files.
- `irsync` is required. `fileset` and the man pages are installed when
  present and skipped when not; the script says what it skipped. (In the
  normal download mode all five stay required, as today.)
- Everything else is shared with the download mode: the check that every
  target directory is writable before anything is placed, the
  copy-then-rename placing, the `PATH` check and advice, the closing
  lines.
- `--from` cannot be combined with `--uninstall`.

### 2. A better `PATH` check, for both modes

Today the installer looks at its own `PATH`. That is wrong when the user's
shell is a different one, and nearly always wrong under `ssh host cmd`,
which does not run a login shell.

New: the installer also asks the login shell (`$SHELL`) whether it finds
`irsync`. It starts that shell the way a new terminal would, as an
interactive login shell, with stdin from `/dev/null` and stderr dropped.
For sh, dash, bash, zsh, ksh and fish that is `$SHELL -l -i -c 'command -v
irsync'`; nushell needs its own form.

It must be interactive: the advice names `~/.zshrc` and `~/.bashrc`, which
a plain `-l -c` shell does not read (or leaves early). A user who follows
the advice and runs the installer again must then be told nothing.

`irsync` counts as found if either check finds the copy just installed in
`~/.local/bin`: the login-shell probe, or today's look at the installer's
own `PATH`. Advice is printed only when both miss. If the shell is
unknown, missing, or the probe fails, only today's check applies.

Advice when not found: as today per shell, plus a nushell entry (the line
to add to its config file). The Linux wording "log out and in again
first" stays, since the login-shell probe already covers the case where a
new login fixes it: if the probe finds `irsync`, nothing is printed.

*My call:* probing the login shell rather than only improving the text,
probing it interactively, and "either check is enough". The issue asked
for the probe ("checks whether the user's login shell will find irsync").
Risk: a login shell with a slow or noisy startup file. Only the last line
of the probe's output is read, and only as a path; anything else is
ignored.

### 3. The installer script is built into irsync

The irsync binary carries the installer template
(`packaging/installer/irsync-installer.sh.in`) with its version filled in.
The ssh install sends that copy. It never downloads the installer, so the
script always matches the client's build, and development builds have it
too.

The checksum placeholders are not used in `--from` mode. How they are
filled in the built-in copy is left to the builder, as long as the
download mode of that copy cannot install an unchecked tarball (for
example: fill them with a value that can never match).

### 4. What the client sends

The client builds a directory in the tarball's layout, then streams it and
the installer to the server in one ssh call as a plain, uncompressed tar.

| This build | Server platform | Files come from |
|---|---|---|
| Release build | has a release tarball (Linux x86_64/aarch64, macOS aarch64) | the release tarball for this build's tag, downloaded and checked on the client as today. All five files. |
| Any build | same OS and CPU, and no tarball applies (development build; FreeBSD) | this machine: the running `irsync`; `fileset` if it sits beside it; the man pages if present. |
| Anything else | | fails with today's messages. |

"Man pages if present" on this machine means: first `man/` beside the
binary (the tarball layout), then `../share/man` relative to the binary's
directory (the layout of `~/.local`, Homebrew and `/usr`). All three pages
or none. *My call* on the two places.

Changed from today: a release build now downloads the tarball even when
the server has the same OS and CPU. It is the only reliable source for
`fileset` and the man pages.

Fallback, same OS and CPU only (*my call*): when the download fails (no
internet on the client, or no such release: a clean build from source
counts as a release build, see `TODO.md`), or the server rejects the
tarball's `irsync` as another build, the client says so in one line and
installs from this machine instead (row 2). So every same-platform setup
that works today keeps working. With another OS or CPU there is no
fallback, as today.

### 5. What happens on the server

One ssh call, in this order:

1. Unpack the stream into a fresh work directory under the home directory
   (not `/tmp`, which hardened servers mount noexec), outside
   `~/.local/bin` and `~/.local/share/man`. Check that all bytes arrived
   (today's size check, or an equivalent). *My call* on the place; the
   exact path is the builder's. Parent directories created for it are
   removed again if the install is rejected and they are empty.
2. Run the unpacked `irsync --version`. It must print the client's exact
   build. Otherwise remove the temporary directory and report, with
   today's three distinct outcomes: cut short, another version, does not
   run.
3. Run the installer with `--from` on that directory.
4. Remove the temporary directory, on every exit.

Nothing is installed or replaced before step 3. This also closes the two
open `TODO.md` items about the upload command (the empty directory after a
rejected install, the leftover temp file after a failed step).

The installer's messages go to stderr and reach the user's terminal
through ssh.

The probe before the install reads the version of `~/.local/bin/irsync`.
If that already is the client's exact build, nothing is installed, as
today. If it is another version, including one the user installed with
`install.sh`, it is replaced. `ssh-setup` and the daemon are started from
`~/.local/bin/irsync`.

### 6. The old directory

`irsync ssh-setup`, on the server, after the install:

- Looks at `~/.config/systemd/user/irsync.service` and
  `~/Library/LaunchAgents/org.irsync.daemon.plist`. If one mentions
  `.local/share/irsync/bin`, the setup report carries a warning. The
  client's summary prints it: which file, that the service still starts
  an old irsync, and the new path to put in
  (`~/.local/bin/irsync`).
- If such a file points at the old binary: the old binary stays.
- If none does: the old binary `~/.local/share/irsync/bin/irsync` is
  removed, and the `bin` directory too if it is then empty. The summary
  says so.

**A daemon still running from the old binary.** Today `ssh-setup` only
restarts a daemon started from its own file (`setup.rs`: `held == exe`);
any other is "foreign" and left alone. After the move, every existing
server's daemon runs from the old path, so without a rule here it would
stay on the old build for good. Decided:

- No service file points at the old binary: `ssh-setup` treats that
  daemon as its own. Stop it, start the new one from `~/.local/bin`, then
  remove the old binary. Reported as a restart, as today.
- A service file points at the old binary: the daemon is the service's.
  Leave it alone (reported as foreign, as today) and print the warning.
  Stopping it would fight systemd or launchd.

The service snippets the onboarding prints, the book
(`how-to/run-the-daemon.md`, `how-to/ssh-setup.md`,
`tutorial/first-sync.md`, `reference/files-and-state.md`) and the sample
output switch to `~/.local/bin/irsync`.

### 7. The onboarding summary

The summary line names the new place and what was installed, for example:

```
irsync:   installed irsync 0.2.0 (1a2b3c4) in ~/.local/bin, with fileset and the manual pages (release files for linux aarch64)
```

and for a development build without the extras:

```
irsync:   installed irsync 0.2.0-dev (1a2b3c4+) in ~/.local/bin (no fileset, no manual pages: this is a development build)
```

Exact wording is the builder's. The `PATH` advice, when needed, comes
from the installer right above it.

### 8. `--no-install`

The flag was first decided when the install was going to edit startup
files. With no editing left, its meaning is, as decided: copy nothing to
the server.

- With `--no-install`, the ssh setup uses the irsync already at
  `~/.local/bin/irsync` on the server.
- If there is none, or it is not the client's exact build, the setup
  fails and says to install that version on the server or drop the flag.
  Nothing is changed.
- *My call:* the old-directory warning of section 6 still runs, but under
  `--no-install` nothing is deleted. A daemon running from the old path
  is still restarted from `~/.local/bin`, since that is setup, not
  install.

Client config item: a boolean with the same meaning, default off, in the
client's config file; the flag overrides it. Name and table are the
builder's, following how the file names its other ssh-related settings.
Documented in `reference/command-line.md` and the config reference (which
feeds `irsync.toml(5)`).

## Tests

- Installer (`packaging/tests/test_installer.py`): `--from` with all five
  files; with only `irsync`; missing `irsync`; unwritable target; the
  login-shell probe with a fake `$SHELL` that finds, does not find, fails;
  nushell advice. Under dash as well as the default `sh`.
- ssh client (`crates/irsync/tests/ssh_client.rs`, `onboard_e2e.rs`):
  files land in `~/.local/bin` and `~/.local/share/man`; the three
  rejected outcomes leave the home directory untouched; development build
  with and without `fileset` beside it; `--no-install` with a matching,
  a differing and a missing server copy.
- Setup: the warning for each of the two service files; old binary kept
  when referenced, removed when not.
- The man pages are built from the reference chapters, so
  `python3 -m unittest discover -s tools/tests` runs too.

## Left to the builder

- How the template gets into the binary and how its placeholders are
  filled.
- The exact remote command, its exit codes, and the stream format details.
- The nushell probe command and the exact nushell advice line.
- The new lines of the setup report and their parsing.
- Message wording.
- `docs/IRSYNC_DECISIONS.md` (amend R44), `docs/SUPPLY_CHAIN_DECISIONS.md`
  if the built-in installer changes anything there, and `TODO.md`.
