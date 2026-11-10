# irsync

irsync is a one-way directory synchroniser in the mould of rsync, rebuilt for
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

## Install

```sh
cargo install --git https://github.com/irsync-dev/irsync irsync
```

Linux and macOS (x86_64 and aarch64) are supported, as client and as daemon.

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
answering, irsync checks it over SSH once and starts it again.

Without SSH, set the server up by hand:

```sh
irsync id show                             # client: the second line is its EndpointId
irsync export add photos:/srv/photos       # server: prints the daemon's EndpointId
irsync grant add photos <client-id>        # server: let the client's key in
irsync daemon                              # server: serve until interrupted
irsync -av ~/Photos/ <server-id>:photos/   # client: push
```

## Documentation

The book is at https://irsync.org/book/. Start with
[Your first sync](https://irsync.org/book/tutorial/first-sync.html), look up
flags in the [command-line reference](https://irsync.org/book/reference/command-line.html),
and see [Contributing](https://irsync.org/book/contributing/setup.html) to work
on irsync itself.

`fileset` shares no code with irsync; see
its [reference](https://irsync.org/book/reference/fileset.html).

## License

irsync is licensed under the MIT License; see [LICENSE](LICENSE).
