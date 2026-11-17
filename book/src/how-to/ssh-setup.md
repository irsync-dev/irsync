# Set a server up over ssh

When `ssh myserver` logs you in to the server, the first sync to
`myserver:PATH` sets the server up: it installs irsync there, grants this
client access, starts the daemon and saves `myserver` as a host alias.
[Your first sync](../tutorial/first-sync.md#the-quick-way-over-ssh) walks
through it. This page covers the options.

## The names the setup saves

On the client, the alias depends on what you typed before the `:`:

| You typed | Saved alias |
|-----------|-------------|
| `myserver` | `myserver` |
| `mike@myserver` | `mike@myserver`, and also `myserver` when no alias has that name yet |
| `192.168.1.5` or `mike@192.168.1.5` | the server's own host name, for example `myserver` |
| `[fe80::1]` (an IPv6 address goes in brackets) | the server's own host name |

Every alias keeps what you typed as its `ssh` value, so a later check over
ssh logs in the same way.

- With `mike@myserver`, both names reach the same daemon. If `myserver` is
  an alias already, it is left as it is: the first one wins.
- For an address, irsync asks the server for its host name and uses the part
  before the first `.`. If that name already belongs to another daemon, the
  alias is `myserver-2`, then `myserver-3`.
- A server that gives no usable name (none, or `localhost`) keeps an IPv4
  address as its alias. An IPv6 address cannot be an alias, so the setup
  stops before it changes anything; give the server a host name or
  [add the host by hand](../tutorial/first-sync.md#set-the-server-up-by-hand).

Typing the address again later needs no ssh. irsync finds the saved host by
its `ssh` value and says so:

```text
irsync: 192.168.1.5 is saved as myserver
```

It dials the daemon saved under that name, wherever it is now. If the
address has since gone to another machine, that is not the machine you get.
First take the address off the old entry (`irsync host remove myserver`, or
change its `ssh` value in the config file), then sync to the address again:
the new machine is set up and saved under its own name. Do not use `--ssh`
for this: it would point the old alias at the new machine.

On the server, the setup saves the client under the client's short host name
and grants that name:

```toml
[hosts.myclient]
client_id = "a3e4753e5ea74e64e745e9c4d10b0fd751dbe03049d742eb0ea545c89703dfba"

[immediate]
rw = ["myclient"]
```

The name is a label. The daemon decides access by the `EndpointId` the name
stands for, never by what a client calls itself. A client the server already
has a name for keeps that name.

The two machines can sync in both directions under the same names. When the
server later runs `irsync ... myclient:PATH`, irsync sees that `myclient` is
saved as a client only, sets its daemon up over ssh, and adds the daemon's
`endpoint_id` to the same entry. The other way round works too: a client
called `myclient` joins a `[hosts.myclient]` entry that the server so far
only dials, as long as no grant names that entry.

The setup stops, and changes nothing on the server, when the client's host
name cannot be used there:

- Grants on the server name `[hosts.myclient]`, and it is not this client:
  that entry is a peer someone named by hand. Rename one of the two.
- The name already belongs to another client. This also happens when the
  same machine comes back with a new key, for example after a reinstall.
  The message names the entry and the command to run on the server if that
  client is gone: `irsync host remove myclient -f`, or, when the server also
  dials that machine under the name, `irsync config set
  hosts.myclient.client_id ID`, which replaces only the client id. Else
  rename the client machine.
- Grants on the server still name a client that has no `[hosts]` entry any
  more. Those are left from a removed client and are not handed to a new
  one of the same name. Take the name out of the lists the message names.
- The client has no usable host name of its own (none, or `localhost`).
  Give the machine a host name.

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

When both machines run the same OS and CPU, the setup copies the client's
own binary. When they differ, a released irsync downloads the release binary
of its own version for the server's platform, checks its SHA-256, and copies
that over:

```text
irsync: downloading irsync v0.1.0-beta.1 for linux aarch64
irsync: set up myserver over ssh:
irsync:   installed irsync 0.1.0-beta.1 (ae5ebe5) at ~/.local/share/irsync/bin/irsync (release binary for linux aarch64)
```

The download happens on the client, which needs `curl` or `wget`, and a
`tar` that reads `.xz`. The server needs no internet access. Either way the
server runs the new binary with `--version` before it replaces anything, and
keeps what it had unless the binary reports the client's exact build.

Release binaries exist for Linux on x86_64 and arm64 and for macOS on Apple
Silicon. For any other server, and for a client built from source, the setup
stops and changes nothing. Then install irsync on the server yourself and
[set the server up by hand](../tutorial/first-sync.md#set-the-server-up-by-hand).
Installing it on the server does not make the ssh setup work: the setup only
uses its own copy of the client's exact build. See
[Troubleshooting](troubleshooting.md#ssh-setup-stops-the-server-runs-another-os-or-cpu)
for the messages.

## Keep the daemon running

A daemon started over ssh does not survive a reboot on its own; the next sync
restarts it over ssh. To make it a user service, see
[Run the daemon](run-the-daemon.md#keep-a-daemon-started-over-ssh-running).
