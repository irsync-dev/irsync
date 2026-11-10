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

## Quick start

If `ssh myserver` logs you in to the server, irsync needs to be installed on
the client only. Push to any path your account there can reach:

```sh
irsync -av ~/Photos/ myserver:/srv/photos/
```

The first run sets the server up over ssh: it installs irsync there, grants
this client access to any path (*immediate mode*), starts the daemon and saves
`myserver` as a host alias. Later runs connect over iroh without ssh.

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
  running the daemon, changing exports, choosing a connection path, cleaning
  up, measuring a transfer.
- **Reference.** Every flag, command, configuration key, file and metric, and
  the wire protocol, stated as the code has it.
- **Explanation.** Why irsync behaves as it does: sessions and resume,
  identity and trust, connections, and the transfer algorithm.

A fifth part, **Contributing**, is for people who work on irsync itself:
setting up a checkout, running the tests, the stress and network-stress
tools, and how this website is built.

The source is at [github.com/irsync-dev/irsync](https://github.com/irsync-dev/irsync).

## License

irsync, and the tools in its repository, are MIT licensed; see the
[LICENSE](https://github.com/irsync-dev/irsync/blob/main/LICENSE) file.
