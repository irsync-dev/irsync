# Set a server up over ssh

When `ssh myserver` logs you in to the server, the first sync to
`myserver:PATH` sets the server up: it installs irsync there, grants this
client access, starts the daemon and saves `myserver` as a host alias.
[Your first sync](../tutorial/first-sync.md#the-quick-way-over-ssh) walks
through it. This page covers the options.

## Run the setup again

Pass `--ssh` to go through ssh on purpose, for example to update irsync on
the server after you updated the client:

```sh
irsync -av --ssh ~/Photos/ myserver:/srv/photos/
```

It runs the same setup for a saved host, then syncs.

Without `--ssh`, a sync goes through ssh again only when the daemon does not
answer for about 15 seconds: then irsync says
`myserver did not answer; checking it over ssh`, starts the daemon again and
carries on. A daemon that answers but refuses the client is never "fixed"
over ssh: that refusal is on purpose. The check never adds back an immediate
grant you removed on the server either; the sync then stops with the same
"rejected this client's key" message.

## Keep the daemon out of the public DHT

The daemon publishes its addresses in the mainline DHT, so later runs find it
without ssh even when its IP address changes. To keep it out of that public
table, pass `--no-remote-dht`:

```sh
irsync -av --no-remote-dht ~/Photos/ myserver:/srv/photos/
```

The setup then writes `dht = false` under `[daemon.discovery]` on the server.
The daemon can still be found through DNS and the relay.

## Use another ssh command

Use `--rsh` or `IRSYNC_RSH` for another ssh command:

```sh
irsync -av --rsh "ssh -p 2222" ~/Photos/ myserver:/srv/photos/
```

ssh is used only for the setup, never for the data.

## Share only named directories

By default the setup lets the client reach any path your account on the
server can (an [immediate grant](../reference/configuration.md#immediate-grants)).
If the client should not reach every path, set this on the server before the
first sync:

```sh
irsync config set daemon.immediate false
```

The setup then grants nothing, and you grant exports by hand as in
[Your first sync](../tutorial/first-sync.md#3-grant-the-client-access).

## A server with another OS or CPU

The setup copies the client's own binary, so it works only when both machines
run the same OS and CPU. Otherwise it stops with:

```text
irsync: myserver is not a saved alias; setting it up over ssh
irsync: "myserver" is not an alias in /Users/you/.config/irsync/irsync.toml and not an EndpointId; setting it up over ssh failed: myserver runs linux aarch64, this irsync is macos aarch64, so it cannot copy itself there. Install irsync on both machines and set the server up by hand: https://irsync.org/book/tutorial/first-sync.html#set-the-server-up-by-hand
```

Install irsync on the server yourself and
[set the server up by hand](../tutorial/first-sync.md#set-the-server-up-by-hand).
Installing it on the server does not make the ssh setup work: the setup only
uses its own copy of the client's exact build. Release binaries, planned for
the beta, will remove this step.

## Keep the daemon running

A daemon started over ssh does not survive a reboot on its own; the next sync
restarts it over ssh. To make it a user service, see
[Run the daemon](run-the-daemon.md#keep-a-daemon-started-over-ssh-running).
