# Introduction

`irsync` is a one-way directory synchroniser in the mould of rsync, rebuilt for
today's networks. It runs on both ends of a transfer and connects through
[iroh](https://iroh.computer), so two machines find each other and traverse
NATs and firewalls without port forwarding, dynamic DNS or SSH tunnels. The
daemon side is reached by its iroh identity, not by an address.

It is designed to fill whatever bandwidth the link has, and to survive a bad
one. A sync session outlives any single network connection: when hotel wifi
drops, the laptop sleeps or the process is killed, the next run of the same
command resumes where it stopped; when the interrupted run had finished
listing and no deletion is asked for, it does so without re-walking and
re-comparing the whole tree. Many files are in flight at once, scanning runs
alongside the transfer, and a persistent index makes repeat runs over a large,
mostly unchanged tree cheap.

Correctness comes first. [How we verify irsync's correctness](explanation/correctness.md)
states what a completed sync promises, and how you can check it yourself.

> **Beta.** irsync is beta software. The command line and the network
> protocol may still change before the first stable release, and versions may
> then not talk to each other. Keep another copy of anything you sync with it.

## Install

Pick one way. Every package contains both `irsync` and `fileset`.

The install script puts both in `~/.local/bin` and the man pages in
`~/.local/share/man`:

```sh
curl --proto '=https' --tlsv1.2 -LsSf https://irsync.org/install.sh | sh
```

It installs the newest stable release. To try a beta, add `--beta` straight
after `sh -s --`. `--alpha` takes the newest release of any kind:

```sh
curl --proto '=https' --tlsv1.2 -LsSf https://irsync.org/install.sh | sh -s -- --beta
```

The first stable release will be v0.1.0. Until it is out, add `--beta`.

The script checks the installer's signature before it runs it, with irsync
or [minisign](https://jedisct1.github.io/minisign/) if either is installed.
If neither is, it warns and goes on. Add `--require-signature` (next to
`--beta`, before any other argument) to make it stop instead. Releases before
v0.1.0-beta.5 have no signatures, so the script never installs them.

Then `man irsync`, `man fileset` and `man irsync.toml` work. The script
never edits your shell startup files. If `~/.local/bin` is not on your
`PATH`, it says how to add it; on Linux, logging out and in again is
usually enough. To remove what it installed (your config
and state stay), run it with `--uninstall`. Drop `--beta` here too once
v0.1.0 is out:

```sh
curl --proto '=https' --tlsv1.2 -LsSf https://irsync.org/install.sh | sh -s -- --beta --uninstall
```

The Homebrew, apt and dnf packages install the man pages too. `cargo
install` installs none.

Homebrew (Linux and macOS), from v0.1.0 on:

```sh
brew install irsync-dev/tap/irsync
```

Debian, Ubuntu, Fedora and RHEL, from v0.1.0 on: the apt and dnf setup
commands are on [dist.irsync.org](https://dist.irsync.org).

You can also download a tarball from the
[GitHub Releases page](https://github.com/irsync-dev/irsync/releases).

For anything else, build from source with Rust's `cargo`. If you have no
Rust yet, or an older one, get it from [rustup.rs](https://rustup.rs) first. The build takes a few minutes.

```sh
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh   # only if you have no Rust
cargo install --git https://github.com/irsync-dev/irsync irsync
```

Release binaries cover Linux on x86_64 and arm64, and macOS on Apple Silicon.
Other Linux and macOS machines can build from source. All of these are
supported, as client and as daemon.

With ssh, the client is enough: `irsync pair` installs irsync, fileset and
the manual pages on the server. A released irsync downloads the release files
for the server's platform on this machine, whatever that platform is. If the
download fails and the server has the same OS and CPU, it sends this machine's
own files instead. The server needs no internet access. A build from source
sends only its own files, so it can set up only a server with the same OS and
CPU. For another server, and for a platform with no release files (an Intel
Mac, for example), install irsync on both machines and set the server up by
hand ([Your first sync](tutorial/first-sync.md#set-the-server-up-by-hand)).

## Quick start

If `ssh myserver` logs you in to the server, irsync needs to be installed on
the client only. Set the server up once, then push to any path your account
there can reach:

```sh
irsync pair myserver
irsync -av ~/Photos/ myserver:/srv/photos/
```

`irsync pair` sets the server up over ssh: it installs irsync there, grants
this client access to any path (`all_paths = "rw"`), starts the daemon as a user
service and saves `myserver` as a server. It is the only command that uses
ssh. Syncs connect over iroh.

To share only named directories instead, set the server up by hand: export a
directory, grant the client access to it and start the daemon.
[Your first sync](tutorial/first-sync.md) walks through both ways.

## Who this book is for

People who copy large directory trees between a client and a server, both
machines they control, and who want a transfer to finish after the network has
failed along the way. If you know rsync, you know most of the command line:
irsync accepts many of rsync's flags, some with a different meaning, and adds
its own; the [command-line reference](reference/command-line.md) lists them,
and [Known limitations](reference/limitations.md#rsync-options-irsync-does-not-have)
lists the rsync options it does not have.

## How the book is organised

The pages come in four kinds, and each kind answers a different question.

- **Tutorial.** One guided run from nothing to a first transfer, for someone
  who has not used irsync before.
- **How-to guides.** Short recipes for a task you already have in mind:
  troubleshooting a first sync, pairing with a server over ssh, running the
  daemon, changing exports, choosing a connection path, cleaning up,
  measuring a transfer.
- **Reference.** Every flag, command, configuration key, file and metric, and
  the wire protocol, stated as the code has it.
- **Explanation.** Why irsync behaves as it does: how we verify its correctness,
  sessions and resume, identity and trust, connections, and the transfer
  algorithm.

A separate part, **fileset**, covers a companion tool that checks with code of
its own, not irsync's. It checks a copy against a recorded manifest, whichever tool made the
copy, and finds bitrot. Start at [Introduction to fileset](fileset/introduction.md).

A last part, **Contributing**, is for people who work on irsync itself:
setting up a checkout, running the tests, the stress and network-stress
tools, and how this website is built.

The source is at [github.com/irsync-dev/irsync](https://github.com/irsync-dev/irsync).

## Get help

- **A first sync fails.** [Troubleshoot a first sync](how-to/troubleshooting.md)
  goes through the common causes and their fixes.
- **Questions and bugs.** Ask a question or report a bug in
  [GitHub Issues](https://github.com/irsync-dev/irsync/issues). For a bug,
  say which version runs on each machine (`irsync --version`), which OS, the
  command you ran and what it printed.
- **Security problems.** Do not open a public issue. Email
  [security@irsync.org](mailto:security@irsync.org) instead; the
  [security policy](https://github.com/irsync-dev/irsync/blob/main/SECURITY.md)
  says what to include.

## License

irsync, and the tools in its repository, are MIT licensed; see the
[LICENSE](https://github.com/irsync-dev/irsync/blob/main/LICENSE) file.
