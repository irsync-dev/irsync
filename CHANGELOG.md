# Changelog

What changed in each irsync release, for people who run it: what is new,
what to do when upgrading, and what was fixed. The newest release is on top.
Each section is also the top of that release's notes on GitHub, where the
full list of changes follows it.

The sections up to v0.1.0-beta.6 were written afterwards, from the history
of the code. Their GitHub releases do not carry them.

## v0.1.0-beta.6

### New

- `--bwlimit` now limits a pull too, not only a push.
- irsync and the install script check the signature of a release before
  they install it. The install script never installs an unsigned release.
- The website holds the documentation once per release channel, and the
  book's header switches between them. The homepage shows the current
  versions.
- The man pages name the authors and the license.

### Fixed

- After a resume, the progress block and the summary report the right
  totals, percent and number of created files.
- A failed session says "failed" in the progress block, not "done".

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
  `~/.local/share/irsync/bin` the next time it is set up.

### New

- FreeBSD: irsync builds and passes its tests there, and a FreeBSD client
  can set up a FreeBSD server over ssh. There is no FreeBSD download yet,
  so you build it yourself.
- `irsync sync` and `irsync list` are commands of their own. The short
  form without a command name still works.
- `-v`, `-q`, `--config` and `--passphrase-file` work on every command.
- `irsync id generate --passphrase-file` writes an encrypted key. Where
  `--passphrase-file` is given, irsync refuses a key that is not
  encrypted.
- The progress block shows the current transfer rate, not only the average.
- The first sync to a server saves its host name as an alias, so the
  name you typed keeps working. The server grants access to the client by
  its host name too.
- A remote host can be an IPv6 address in brackets.
- `--no-install` and the `ssh_install` setting stop irsync from installing
  itself on a server.
- `irsync host add --client-id` adds a host that only connects to you.
- Every release is signed with minisign, on a machine that is not part of
  the build. The book explains how to check a download.

### Behind the scenes

- The developer setup script downloads its tools, with checksums, instead
  of compiling them.
- A release is built as a draft and published only after it is signed.

## v0.1.0-beta.4

The first release that calls itself a beta. The configuration moves into
one file, man pages arrive, and irsync has its own installer.

### Upgrading

- `hosts.toml` and `exports.toml` are no longer read. Everything lives in
  `irsync.toml`. Move what you still keep in the two old files there.
- The daemon refuses to run as root unless you opt in. An ssh setup
  refuses a root account as well.
- irsync writes host aliases, not raw IDs, into the grant lists whenever it
  saves the configuration.

### New

- Man pages: `irsync(1)`, `irsync.toml(5)` and one for fileset. They come
  with the tarballs, the Homebrew formula and the deb and rpm packages.
- A new install script, written for irsync. It also installs the man
  pages, and it works under Rosetta.
- `irsync host rename` renames a host and updates its grants.
- `irsync id list` lists the keys. A management command given alone, like
  `irsync host`, lists.
- `irsync host remove` refuses to remove a host that grants still name.
- `fileset list` prints what a manifest holds. `fileset -v` prints one
  line per entry, and the spinner shows the hashing speed.

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
