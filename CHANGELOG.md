# Changelog

What changed in each irsync release, for people who run it: what is new,
what to do when upgrading, and what was fixed. The newest release is on top.
Each section is also the top of that release's notes on GitHub, where the
full list of changes follows it.

## v0.1.0-beta.7

A release about safety and a cleaner setup. A review of everything the
other side of a sync can send led to many fixes, every connection offers a
key exchange that holds against a future quantum computer, and the new
`irsync pair` is now the only command that uses ssh. The configuration and
the command line were cleaned up, so read "Upgrading" first.

### Upgrading

- The protocol changed. An older irsync cannot talk to this one, so update
  the client and every server together.
- `irsync.toml` has a new layout, and irsync refuses a file from beta.6 at
  the first line it no longer knows. Change it by hand, with the
  configuration reference at hand:
  - `[hosts.NAME]` is split into `[servers.NAME]`, the machines you
    connect to, and `[clients.NAME]`, the machines your daemon lets in.
    Grants name clients. The file starts with `config_version = 2`. [#202]
  - `[client]` is now `[defaults]`, and the top-level `identity` moves
    into it.
  - `[all]` and `[immediate]` are now `all_exports` and `all_paths` on
    the client, and `[daemon] immediate` is now `allow_all_paths`. [#284]
  - One `relay = "default" | "disabled" | "URL"` replaces `relay` and
    `relay_mode`. `dht`, `dns` and `mdns` are plain keys in `[daemon]`,
    `[defaults]` and each server, and `[daemon.discovery]` is gone.
  - The export setting `warm_index` is now `index_at_start`.
- The first command that changes `irsync.toml` writes it in a fixed order.
  Comments stay, blank lines do not.
- irsync refuses a config file, a private key or a passphrase file that
  other users can read or write, or that belongs to another user, as sshd
  does. `chmod 600` fixes it. `unsafe_config_allowed = true` at the top of
  `irsync.toml` turns the check off.
- A sync no longer uses ssh. `irsync pair myserver` sets a server up: it
  installs irsync there, lets your client in, starts the daemon as a user
  service and saves the server. A sync to a name that is not a saved server
  stops and points to `irsync pair`. Run `irsync pair` once for each server
  you set up before, to update it. [#277]
- Commands and flags that changed:
  - `irsync host` is gone. Use `irsync server` and `irsync client`.
  - `--ssh`, `--rsh`, `--no-install` and `--no-remote-dht` left the sync.
    `irsync pair` has `--rsh`, `--no-ssh-install` and `--server-no-dht`.
  - `--timeout SECS` is now `--idle-timeout DUR`.
  - `--relay default|disabled|URL` replaces `--no-relay` and the old
    `--relay URL`, and `server add --relay-mode` is gone.
  - `export add --ro` is now `--read-only`.
  - `irsync export update` is now `irsync index --export NAME`.
  - `grant add` and `grant remove` no longer take `all` or `immediate`.
    Use `irsync client set`.
- Every mistake on the command line exits with 2. Some used to exit with 1.
- A push of one file onto a symlink is refused when the link has an
  absolute target or leads out of the export.
- A client with a grant for only one of two exports of the same directory
  can no longer use the other export's name.
- `fileset` refuses a manifest written by an older version. Make it again
  with `fileset init`.

### New

- `irsync pair` ends by reaching the daemon over iroh, the way a sync
  will, and says so when it cannot. [#312]
- `irsync remote myserver` manages a server's exports, clients and grants
  from your client, with no shell on the server. The server gives a client
  that right with `irsync client set myclient admin=rw`, or `admin=ro` to
  only read. `irsync pair` gives it to your client. [#203]
- `irsync client set` sets a client's `all_exports`, `all_paths` and
  `admin`. `all_paths = "ro"` is new: read any path, write none. [#284]
- `irsync export set`, `irsync export rename` and `export add --fsync`.
  `export add` also changes an export that is already there.
- `--identity` works on every command, and `-q` quiets the management
  commands.
- Every connection offers the hybrid key exchange `X25519MLKEM768` first.
  A recorded connection between two irsync of this version stays safe from
  a future quantum computer. `pq_key_exchange = "require"`, or
  `--pq-key-exchange require`, refuses a connection without it. `-vv`
  shows which key exchange a connection got. [#240] [#241] [#255]
- A chunk that a dropped connection left half done continues where it
  stopped. On a link that drops more often than one chunk can cross, a
  transfer now finishes. [#281]
- A big push over a link that drops often keeps going, as long as each
  connection moves data. [#271]
- The daemon setting `max_dormant_sessions_per_key`, default `25`, limits
  the interrupted sessions one client key may leave in an export.
- A run with several sources names each session in the progress block, and
  the name stays above its finished lines. [#112]
- `fileset init --store redb` keeps the manifest in one small file, for a
  tree that travels. It can be read on media that cannot be written to.
  [#192] [#206]
- `fileset` runs on Windows.
- The book has a page on the threat model. Security problems can be
  reported privately on the repository's Security tab.
- The landing page has the version switcher, and the book of the
  development version moved to dev.irsync.org. [#288] [#318]

### Fixed

- irsync checks what the other side of a sync sends much more strictly:
  - A receiver places only the one file of a single-file sync, and nothing
    that its own filter rules exclude.
  - Files are read and written through directories opened without
    following symlinks, so a sync stays inside the export and the
    destination even when a directory is swapped for a symlink.
  - A sender learns nothing about the old copy of a file it updates.
  - A session holds a limited number of filter rules, paths and hard-link
    checks.
  - Text from the other side is printed with control characters and
    characters that change the direction of text escaped, and a log entry
    stays on one line.
  - An export's name counts only for a client with a grant for it.
- Partial files, the index, the daemon's log and a new config file can be
  read by their owner only. Saving the config keeps its permissions.
- `fileset` trusts a manifest that came with a tree less: it refuses one
  that is a symlink or holds links, views or triggers.
- `--reconnect` counts an outage from the first dropped connection, not
  from the start of the run. A long first connection that drops is always
  dialled again. [#223]
- On a lossy link, the daemon's last answer, such as a refusal, reaches the
  client. [#298]
- A daemon's refusal no longer looks like an empty list of exports, and a
  refused listing no longer prints an error from iroh first. [#256]
- The progress block:
  - A file that is fully sent and waits for the other side no longer shows
    as stalled. [#120]
  - On a pull, a file gets its row when its data arrives, not while it
    waits its turn. [#114]
  - The rows show the files that are moving, not the largest ones, and the
    last line says how many hidden rows are stalled. [#121]
- `irsync show` no longer creates a daemon key, `irsync config get` prints
  the default of every key, and help, errors and the book were corrected
  in many places.

### Behind the scenes

- irsync builds on Windows, but does not run there yet. `tools/ci.sh
  typecheck` checks every platform from one machine.
- `pbwalk` shows where every byte of a protocol capture goes. [#231]
- aws-lc-rs replaces ring, iroh comes from upstream instead of a fork, and
  `cargo deny` refuses a new duplicate crate version. OpenSSF Scorecard
  checks the repository. [#124] [#209]
- Release notes live in `CHANGELOG.md`, and every release line gets a
  release branch. [#322]
- Several flaky tests are fixed.

## v0.1.0-beta.6

irsync and the install script now check a release's signature before they
install it. The progress block and the summary are right after a resume,
and the code is checked by more tools.

### New

- `--bwlimit` now limits a pull too, not only a push. [#111]
- irsync and the install script check the signature of a release before
  they install it. The install script never installs an unsigned release.
  [#89]
- The website holds the documentation once per release channel, and the
  book's header switches between them. The homepage shows the current
  versions. [#105] [#123]
- The man pages name the authors and the license.

### Fixed

- After a resume, the progress block and the summary report the right
  totals, percent and number of created files. [#26] [#103] [#133] [#139]
- A failed session says "failed" in the progress block, not "done". [#116]
- After a push of several sources, the finished lines of the last source
  stay on screen too. [#27]

### Behind the scenes

- The code that reads data from a peer is fuzzed every night.
- CodeQL, Miri and zizmor check the code and the workflows, and shellcheck
  checks every shell script.

## v0.1.0-beta.5

irsync now runs on FreeBSD, and the first sync to a new server installs
irsync there the same way the install script does. Releases are signed
from this one on.

### Upgrading

- The host setting `ssh_also` is now called `ssh_aliases`. Rename it in
  `irsync.toml`.
- An ssh setup now installs irsync on the server into `~/.local/bin`. A
  server set up by an older version is moved there from
  `~/.local/share/irsync/bin` the next time it is set up. [#28]

### New

- FreeBSD: irsync builds and passes its tests there, and a FreeBSD client
  can set up a FreeBSD server over ssh. There is no FreeBSD download yet,
  so you build it yourself.
- `irsync sync` and `irsync list` are commands of their own. The short
  form without a command name still works. [#62]
- `-v`, `-q`, `--config` and `--passphrase-file` work on every command. [#31]
- `irsync id generate --passphrase-file` writes an encrypted key. Where
  `--passphrase-file` is given, irsync refuses a key that is not
  encrypted.
- The progress block shows the current transfer rate, not only the
  average. [#25]
- The first sync to a server saves its host name as an alias, so the
  name you typed keeps working. The server grants access to the client by
  its host name too. [#33]
- A remote host can be an IPv6 address in brackets.
- `--no-install` and the `ssh_install` setting stop irsync from installing
  itself on a server.
- `irsync host add --client-id` adds a host that only connects to you.
- Every release is signed with minisign, on a machine that is not part of
  the build. The book explains how to check a download. [#89]

### Behind the scenes

- The developer setup script downloads its tools, with checksums, instead
  of compiling them. [#35]
- A release is built as a draft and published only after it is signed.

## v0.1.0-beta.4

The first release that calls itself a beta. The configuration moves into
one file, man pages arrive, and irsync has its own installer.

### Upgrading

- `hosts.toml` and `exports.toml` are no longer read. Everything lives in
  `irsync.toml`. Move what you still keep in the two old files there.
- The daemon refuses to run as root unless you opt in. An ssh setup
  refuses a root account as well. [#34]
- irsync writes host aliases, not raw IDs, into the grant lists whenever it
  saves the configuration. [#38]

### New

- Man pages: `irsync(1)`, `irsync.toml(5)` and one for fileset. They come
  with the tarballs, the Homebrew formula and the deb and rpm packages. [#51]
- A new install script, written for irsync. It also installs the man
  pages, and it works under Rosetta.
- `irsync host rename` renames a host and updates its grants. [#37]
- `irsync id list` lists the keys. A management command given alone, like
  `irsync host`, lists. [#32]
- `irsync host remove` refuses to remove a host that grants still name.
- `fileset list` prints what a manifest holds. [#29]
- `fileset -v` prints one line per entry, and the spinner shows the
  hashing speed. [#30]

### Fixed

- A hostile include or exclude pattern can no longer keep the matcher busy
  for a long time.

### Behind the scenes

- The website and the book share one design.
- Releases are cut from release branches, and the tag is created last, so a
  failed release leaves no tag behind.
- CI runs the same script a developer runs locally.

## v0.1.0-beta.3

Only the release process changed. irsync itself is the same as in beta.2.

### Behind the scenes

- A release starts with a pull request that changes the version.
- Every file of a release has a build attestation, not just the archives.

## v0.1.0-beta.2

This release makes the protocol visible, for debugging, and lets the first
sync install irsync on a server that runs another system than yours.

### Upgrading

- The way messages are framed on the wire changed, and the protocol
  version went up. An older irsync cannot talk to this one, so update
  both sides together.

### New

- `-vvvvv` prints every protocol message, on the client and on the
  daemon.
- irsync can write what it sends and receives as a capture file for
  Wireshark. The book has a page on debugging the protocol with it.
- An ssh setup works when the server has another operating system or CPU.
  irsync downloads the matching release, checks it and installs it there.
- The daemon counts `-v` the same way the client does.

### Fixed

- `--address` uses only the address you gave.
- The install script refuses downloads over plain HTTP.

### Behind the scenes

- Release files carry build attestations. The book explains how to verify
  a release.
- Every GitHub Action is pinned to a commit, and new dependencies are
  checked for known problems and licenses.

## v0.1.0-beta.1

The source of a sync can now be a single file.

### New

- Push or pull one file, not only a directory. It resumes like any other
  transfer, and the result is checked against rsync's.

### Fixed

- A push onto a symlink that points to a directory is refused.
- The first contact with a server retries when the connection drops, and
  `--timeout` limits how long it waits.

## v0.1.0-alpha.4

A release about trust in the result: irsync is now tested while the source
changes during the sync.

### Upgrading

- A sync prints nothing by default unless a file fails, like rsync. The
  live progress block now needs `--progress` or `-P`.
- `-v` lists the files, as rsync's `-v` does. The summary of a session
  moved to `-vv`.

### Fixed

- A file is placed with the permissions and owner it was checked under.
- Names of hard links are checked before they are linked.
- On macOS, the destination is kept when the source is missing.

### Behind the scenes

- The stress tests can write to the source while a sync runs, and judge
  every file at the destination against the history of the source.
- The Rust version is pinned, and CI runs the tests with cargo-nextest.

## v0.1.0-alpha.3

The first release published on GitHub. It carries the same irsync as
alpha.1, with updated dependencies.

### Behind the scenes

- Dependencies updated.
- CI builds the Linux release binaries on every change.
- A test makes sure existing keys keep their ID.

## v0.1.0-alpha.2

Tagged but never published. It fixed the lock file that kept alpha.1 from
building as a release.

## v0.1.0-alpha.1

The first tagged version, never published. It holds everything built up to
then, so this section describes what irsync was at the start.

### New

- **Sync over iroh.** irsync copies directory trees like rsync, with
  rsync's flags, over encrypted connections that need no open port.
  Without a direct path, a relay carries the data. `--address` connects
  directly on networks with no internet.
- **Resume.** A transfer survives a dropped connection and a restart of
  either side. It picks up where it stopped.
- **Fast transfers.** Files are cut into chunks, and only chunks the other
  side lacks are sent, on many streams at once. Renamed and copied files
  are found and not sent again.
- **What rsync users expect.** Deletion modes, include and exclude rules,
  hard links, sparse files, special files, owners and groups, `--inplace`,
  `--append`, `--bwlimit`, dry runs and itemized output.
- **Checked results.** irsync notices a source file that changes while it
  is read. `--paranoid` hashes everything a second time before it places
  a file.
- **Set up over ssh.** The first sync to a new server installs irsync
  there over ssh, starts the daemon and saves the host. After that, ssh
  is no longer needed.
- **Keys and access.** Each side has an Ed25519 key, and an existing ssh
  key works. A daemon serves named exports, and each one lists who may
  read or write it. `irsync host`, `export`, `grant`, `id` and `config`
  manage all of it in `irsync.toml`.
- **Progress and numbers.** A live progress block on a terminal, a time
  and resource summary with `-v`, and metrics and traces over
  OpenTelemetry.
- **fileset.** A separate tool that records a tree in a manifest and
  reports what was added, deleted or changed since.
- **Installing.** An install script, deb and rpm packages with signed
  repositories, and Homebrew.
- **Documentation.** The website and the book: a tutorial, how-to guides,
  the reference and how irsync works.

### Behind the scenes

- Every sync mode is compared with GNU rsync.
- A stress harness changes a tree at random, syncs it and compares both
  sides. A second one does the same over a simulated bad network with
  loss, dead zones and handovers.
