# irsync

[![CI](https://github.com/irsync-dev/irsync/actions/workflows/ci.yml/badge.svg?branch=main)](https://github.com/irsync-dev/irsync/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)
![Status: alpha](https://img.shields.io/badge/status-alpha-orange.svg)

irsync is a one-way directory synchroniser in the mould of rsync, rebuilt for
today's networks. It runs on both ends of a transfer and connects through
[iroh](https://iroh.computer), so two machines find each other and traverse
NATs and firewalls without port forwarding, dynamic DNS or SSH tunnels. The
daemon side is reached by its iroh identity, not by an address.

irsync takes rsync's common flags, but it is not a drop-in replacement: it
has its own protocol, so both ends must run irsync, and some rsync options,
such as `-X`, `-A` and `-z`, are missing. See
[Known limitations](https://irsync.org/book/reference/limitations.html).

Traffic is encrypted end to end, and each side is known by its key. The client
only connects to the daemon key it was given, and the daemon only lets in
client keys it has granted; see
[Identity and trust](https://irsync.org/book/explanation/identity-and-trust.html).
When no direct path can be found, traffic goes through a relay, by default one
of the public relays run by n0, the developers of iroh. A relay cannot read
the data. You can run your own, or refuse relays with `--no-relay`; see
[Connect directly or through a relay](https://irsync.org/book/how-to/direct-and-relay.html).

> **Alpha.** irsync is alpha software. The command line and the network
> protocol may change before the beta, and versions may then not talk to each
> other. Keep another copy of anything you sync with it.

It is designed to fill whatever bandwidth the link has, and to survive a bad
one. A sync session outlives any single network connection: when hotel wifi
drops, the laptop sleeps or the process is killed, the next run of the same
command resumes where it stopped; when the interrupted run had finished
listing and no deletion is asked for, it does so without re-walking and
re-comparing the whole tree. Many files are in flight at once, scanning runs
alongside the transfer, and a persistent index makes repeat runs over a large,
mostly unchanged tree cheap.

## Install

irsync builds from source with Rust's `cargo` and needs Rust 1.96 or newer.
If you have no Rust yet, or an older one, get it from
[rustup.rs](https://rustup.rs) first. The build takes a few minutes.

```sh
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh   # only if you have no Rust
cargo install --git https://github.com/irsync-dev/irsync irsync
```

Release binaries will be provided once the beta starts.

Linux and macOS (x86_64 and aarch64) are supported, as client and as daemon.
With ssh, the client is enough: the first sync copies irsync to the server when
both run the same OS and CPU. When they differ, install irsync on both machines
and set the server up by hand.

The workspace also holds `fileset`, an independent tool that records the
checksums and metadata of a directory tree and later verifies the tree against
them, for example after a transfer or to find bitrot:

```sh
cargo install --git https://github.com/irsync-dev/irsync fileset
```

## Example

Push a photo library from a client to a server you can already reach with
`ssh myserver`:

```sh
irsync -av ~/Photos/ myserver:/srv/photos/
```

The first time, irsync sets the server up over SSH: it copies itself there
(when the server runs the same OS and CPU), gives this client's key access,
starts the daemon and saves `myserver` as a host alias. It prints what it did.
Later runs connect over iroh directly and do not use SSH. If the daemon stops
answering, irsync waits about 15 seconds, then checks it over SSH and starts it
again.

Without SSH, set the server up by hand:

```sh
irsync id show                             # client: the last line is its EndpointId
irsync export add photos:/srv/photos       # server: prints the daemon's EndpointId
irsync grant add photos <client-id>        # server: let the client's key in
irsync daemon                              # server: serve until interrupted
irsync -av ~/Photos/ <server-id>:photos/   # client: push
```

## Documentation

The project's website is [irsync.org](https://irsync.org), and the book is at
https://irsync.org/book/. Start with
[Your first sync](https://irsync.org/book/tutorial/first-sync.html), look up
flags in the [command-line reference](https://irsync.org/book/reference/command-line.html),
and see [CONTRIBUTING.md](CONTRIBUTING.md) to work on irsync itself. If a
first sync goes wrong, see
[Troubleshoot a first sync](https://irsync.org/book/how-to/troubleshooting.html).
Before relying on it, read the
[Known limitations](https://irsync.org/book/reference/limitations.html):
Windows is not supported, extended attributes and ACLs are not copied, and
relayed connections are much slower than direct ones.

`fileset` shares no code with irsync; see
its [reference](https://irsync.org/book/reference/fileset.html).

## Getting help

Ask questions and report bugs in
[GitHub Issues](https://github.com/irsync-dev/irsync/issues). Report security
problems privately to security@irsync.org instead; see [SECURITY.md](SECURITY.md).

## License

irsync is licensed under the MIT License; see [LICENSE](LICENSE).
