# irsync

irsync is a one-way directory synchroniser in the mould of rsync, rebuilt for
today's networks. It runs on both ends of a transfer and connects through
[iroh](https://iroh.computer), so two machines find each other and traverse
NATs and firewalls without port forwarding, dynamic DNS or SSH tunnels. The
daemon side is reached by its iroh identity, not by an address.

It is designed to fill whatever bandwidth the link has, and to survive a bad
one. A sync session outlives any single network connection: when hotel wifi
drops, the laptop sleeps or the process is killed, the next run of the same
command resumes where it stopped, without re-walking and re-comparing the
whole tree. Many files are in flight at once, scanning runs alongside the
transfer, and a persistent index makes repeat runs over a large, mostly
unchanged tree cheap.

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

Push a photo library from a laptop to a home server:

```sh
irsync id show                             # laptop: the second line is its EndpointId
irsync export add photos:/srv/photos       # server: prints the daemon's EndpointId
irsync grant add photos <laptop-id>        # server: let the laptop's key in
irsync daemon                              # server: serve until interrupted
irsync -av ~/Photos/ <server-id>:photos/   # laptop: push
```

## Documentation

The book is at https://irsync.org/book/. Start with
[Your first sync](https://irsync.org/book/tutorial/first-sync.html), look up
flags in the [command-line reference](https://irsync.org/book/reference/command-line.html),
and see [Contributing](https://irsync.org/book/contributing/setup.html) to work
on irsync itself.

`fileset` shares no code with irsync; see
[crates/fileset/README.md](crates/fileset/README.md).

## License

irsync is licensed under the MIT License; see [LICENSE](LICENSE).
