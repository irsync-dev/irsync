# irsync

[![CI](https://github.com/irsync-dev/irsync/actions/workflows/ci.yml/badge.svg?branch=main)](https://github.com/irsync-dev/irsync/actions/workflows/ci.yml)
[![OpenSSF Scorecard](https://api.scorecard.dev/projects/github.com/irsync-dev/irsync/badge)](https://scorecard.dev/viewer/?uri=github.com/irsync-dev/irsync)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)
![Status: beta](https://img.shields.io/badge/status-beta-orange.svg)

irsync is a one-way directory synchroniser in the mould of rsync, rebuilt for
today's networks. It runs on both ends of a transfer and connects through
[iroh](https://iroh.computer), so two machines find each other and traverse
NATs and firewalls without port forwarding, dynamic DNS or SSH tunnels. The
daemon side is reached by its iroh identity, not by an address.

irsync takes rsync's common flags, but it is not a drop-in replacement: it
has its own protocol, so both ends must run irsync, and some rsync options,
such as `-X`, `-A` and `-z`, are missing. See
[Known limitations](https://irsync.org/book/reference/limitations).

Traffic is encrypted end to end, and each side is known by its key. The client
only connects to the daemon key it was given, and the daemon only lets in
client keys it has granted; see
[Identity and trust](https://irsync.org/book/explanation/identity-and-trust).
When no direct path can be found, traffic goes through a relay, by default one
of the public relays run by n0, the developers of iroh. A relay cannot read
the data. You can run your own, or refuse relays with `--no-relay`; see
[Connect directly or through a relay](https://irsync.org/book/how-to/direct-and-relay).

> **Beta.** irsync is beta software. The command line and the network
> protocol may still change before the first stable release, and versions may
> then not talk to each other. Keep another copy of anything you sync with it.

It is designed to fill whatever bandwidth the link has, and to survive a bad
one. A sync session outlives any single network connection: when hotel wifi
drops, the laptop sleeps or the process is killed, the next run of the same
command resumes where it stopped; when the interrupted run had finished
listing and no deletion is asked for, it does so without re-walking and
re-comparing the whole tree. Many files are in flight at once, scanning runs
alongside the transfer, and a persistent index makes repeat runs over a large,
mostly unchanged tree cheap.

## Install

The install script puts `irsync` and `fileset` in `~/.local/bin` and the
man pages in `~/.local/share/man`. Until the first stable release, v0.1.0,
is out, add `--beta`:

```sh
curl --proto '=https' --tlsv1.2 -LsSf https://irsync.org/install.sh | sh -s -- --beta
```

Release binaries cover Linux on x86_64 and arm64, and macOS on Apple Silicon.
Homebrew, apt and dnf packages come from v0.1.0 on. Building from source,
removing irsync again, and which servers `irsync pair` can set up are in
[Install](https://irsync.org/book/introduction#install), in the book.

## Example

Push a photo library from a client to a server you can already reach with
`ssh myserver`. Set the server up once, then push:

```sh
irsync pair myserver
irsync -av ~/Photos/ myserver:/srv/photos/
```

The flags are rsync's, with `irsync` in place of `rsync`. `irsync list myserver` lists the exports the server shares with this client.

`irsync pair` sets the server up over SSH: it installs irsync there
(with fileset and the manual pages), gives this client's key access, starts the daemon as a user service and saves `myserver` as a server. It prints what it did.
Only `pair` uses SSH. Syncs connect over iroh directly. If the daemon stops
answering, a sync waits 15 seconds, then stops and says what to check.
`irsync pair myserver` starts it again.

Without SSH, set the server up by hand:

```sh
irsync id show                             # client: the last line is its EndpointId
irsync export add photos:/srv/photos       # server: prints the daemon's EndpointId
irsync grant add photos <client-id>        # server: let the client's key in
irsync daemon                              # server: serve until interrupted
irsync -av ~/Photos/ <server-id>:photos/   # client: push
```

## Verify a download

Every file on a release has a minisign signature (`FILE.minisig`), made with
a key that is kept offline. The public key:

```text
RWR6ngoFwyY7D9i4MAL2T4n1UkGwrCdSGA/SMowtANaffYWkTZdrAtAZ
```

Check a file with `minisign -Vm FILE -P <key>`, and make sure the trusted
comment reads `irsync vX.Y.Z` for your version. More ways to check, with
`gh attestation verify` too, are in
[Verify a download](https://irsync.org/book/how-to/verify-a-download).
Releases from before signing started have no signatures.

## Documentation

The project's website is [irsync.org](https://irsync.org), and the book is at
https://irsync.org/book/. Start with
[Your first sync](https://irsync.org/book/tutorial/first-sync), look up
flags in the [command-line reference](https://irsync.org/book/reference/command-line),
and see [CONTRIBUTING.md](CONTRIBUTING.md) to work on irsync itself. If a
first sync goes wrong, see
[Troubleshoot a first sync](https://irsync.org/book/how-to/troubleshooting).
Before relying on it, read the
[Known limitations](https://irsync.org/book/reference/limitations):
Windows is not supported, extended attributes and ACLs are not copied, and
relayed connections are much slower than direct ones.

`fileset` is a separate tool. It records and checks with code of its own, not
irsync's; only the wildcard matcher of `fileset list` is shared. Use it to verify
copies made by any tool and to find bitrot; see the
[introduction to fileset](https://irsync.org/book/fileset/introduction).

## Getting help

Ask questions and report bugs in
[GitHub Issues](https://github.com/irsync-dev/irsync/issues). Report security
problems privately to security@irsync.org instead; see [SECURITY.md](SECURITY.md).

## License

irsync is licensed under the MIT License; see [LICENSE](LICENSE).

## Authors

- Michael Wildpaner ([@lambdanature](https://github.com/lambdanature))
