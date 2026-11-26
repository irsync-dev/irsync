# Pair with a server over ssh

When `ssh myserver` logs you in to the server, `irsync pair myserver` sets
the server up: it installs irsync there, grants this client access, starts
the daemon and saves `myserver` as a server.
[Your first sync](../tutorial/first-sync.md#the-quick-way-over-ssh) walks
through it. This page covers the options.

`pair` is the only command that uses ssh. A sync, push, pull or listing
never does: it dials the daemon over iroh. A sync to a name irsync does not
know stops and tells you to run `irsync pair`.

## The connection check

Last, `pair` dials the new daemon over iroh, the way a sync will: by the
lookups, with no help from the ssh session. It asks the daemon for the
exports this client may use, so the check also shows that the grant works:

```text
irsync: checking that myserver answers over iroh
irsync: myserver answers over iroh
irsync: paired with myserver; syncs go over iroh, not ssh
```

A daemon that has just started is usually found within seconds. When it is
looked up in the DHT only, it can take a few seconds more. `pair` keeps
trying for up to 30 seconds. If the daemon cannot be found by then, `pair`
fails. The server stays set up and saved: run the same `irsync pair` again
to check once more. See
[Troubleshooting](troubleshooting.md#pair-could-not-reach-the-daemon-over-iroh).

The check takes the connection flags of a sync, such as `--relay`,
`--no-dht` and `--address`, and the server's and `[defaults]` settings.
The flags are used for the check only, not saved.

## The names pair saves

On the client, the server name depends on the host you gave `irsync pair`:

| You typed | Saved server name |
|-----------|-------------|
| `myserver` | `myserver` |
| `mike@myserver` | `mike@myserver`, and also `myserver` when no server has that name yet |
| `192.168.1.5` or `mike@192.168.1.5` | the server's own host name, for example `myserver` |
| `[fe80::1]` (an IPv6 address goes in brackets) | the server's own host name |

Every server keeps what you typed as its `ssh` value, so pairing again
logs in the same way.

- With `mike@myserver`, both names reach the same daemon. If `myserver` is
  a server already, it is left as it is: the first one wins.
- For an address, irsync asks the server for its host name and uses the part
  before the first `.`. If that name already belongs to another daemon, the
  name is `myserver-2`, then `myserver-3`.
- A server that gives no usable name (none, or `localhost`) keeps an IPv4
  address as its name. An IPv6 address cannot be a server name, so `pair`
  stops before it changes anything; give the server a host name or
  [add the server by hand](../tutorial/first-sync.md#set-the-server-up-by-hand).

Typing the address again finds the saved server by its `ssh` value, and
`irsync pair` says so:

```text
irsync: 192.168.1.5 is saved as myserver
```

A server that is already saved with a name to reach it by, such as
`ssh = "mike@myserver"`, keeps that name when you type its address: an
address can go stale. The address is saved beside it, in `ssh_aliases`, so
typing it again finds the same server. The same goes for a second spelling of
an address, such as `u@192.168.1.5` after `192.168.1.5`.

It checks the daemon saved under that name, wherever it is now. If the
address has since gone to another machine, that is not the machine you get.
First take the address off the old entry (`irsync server remove myserver`, or
change its `ssh` or `ssh_aliases` value in the config file), then run
`irsync pair` with the address again: the new machine is set up and saved
under its own name. Do not pair again before that: it would point the old
name at the new machine.

On the server, `pair` saves the client under the client's short host name
as a client, and gives it the `all_paths` and `admin` rights:

```toml
[clients.myclient]
client_id = "a3e4753e5ea74e64e745e9c4d10b0fd751dbe03049d742eb0ea545c89703dfba"
all_paths = "rw"
admin = "rw"
```

The `admin` right lets this client change the server later with
[`irsync remote`](remote-admin.md).

The name is a label. The daemon decides access by the `EndpointId` the name
stands for, never by what a client calls itself. A client the server already
has a name for keeps that name.

The two machines can sync in both directions under the same names. The server
may have a `[servers.myclient]` entry for the same machine: a server and a
client may share a name, and neither touches the other. When the server later
runs `irsync ... myclient:PATH` before it has paired with `myclient`, the
sync stops: `myclient` is saved as a client only, and this machine has never
dialled its daemon. Run `irsync pair myclient` on the server. It sets the
daemon up over ssh and saves the daemon's `endpoint_id` as
`[servers.myclient]`.

`pair` stops, and leaves the server's configuration and daemon as they
were, when the client's host name cannot be used there. The server checks
the name with the irsync that `pair` has just installed or updated
there; that install stays, and the message says so. The cases:

- The name already belongs to another client. This also happens when the
  same machine comes back with a new key, for example after a reinstall.
  The message names the entry and the command to run on the server if that
  client is gone: `irsync client remove myclient -f`. Else rename the client
  machine.
- Grants on the server still name `myclient`, and no `[clients.myclient]`
  says who that is. Those are left from a removed client, or they name a
  server. They are not handed to a new client of the same name. Take the name
  out of the lists the message names.
- The client has no usable host name of its own (none, or `localhost`).
  Give the machine a host name.

## Pair again

Run `pair` again on purpose, for example to update irsync on the server
after you updated the client, or to start a daemon that has stopped:

```sh
irsync pair myserver
```

For a saved server it runs the same setup again. It never adds back an
`all_paths` you removed on the server.

A sync never does this for you. When the daemon does not answer within
15 seconds, the sync stops and says `myserver did not answer`. It names what
to check on the server and the command to start the daemon again:
`irsync pair myserver`. The sync dials only once, unless you pass
`--dial-retries N` or set `dial_retries` in the server's table or in
`[defaults]`: then it tries N more times first. A daemon that answers but refuses the client is
"fixed" by no one: that refusal is on purpose, and the sync stops with the
"rejected this client's key" message.

## Keep the daemon out of the public DHT

The daemon publishes its addresses in the mainline DHT, so syncs find it
even when its IP address changes. To keep it out of that public
table, pass `--server-no-dht`:

```sh
irsync pair --server-no-dht myserver
```

`pair` then writes `dht = false` under `[daemon]` on the server.
The daemon can still be found through DNS and the relay.

## Use another ssh command

Use `--rsh` or `IRSYNC_RSH` for another ssh command:

```sh
irsync pair --rsh "ssh -p 2222" myserver
```

ssh is used only by `pair`, never for the data.

## Share only named directories

By default `pair` lets the client reach any path your account on the
server can (`all_paths = "rw"`, see [Rights on a client](../reference/configuration.md#rights-on-a-client)).
If the client should not reach every path, set this on the server before you
pair:

```sh
irsync config set daemon.allow_all_paths false
```

The daemon then ignores `all_paths`, and you grant exports by hand as in
[Your first sync](../tutorial/first-sync.md#3-grant-the-client-access).

## What gets installed, and from where

`pair` installs the same files as `install.sh`: `irsync` and `fileset` in
`~/.local/bin`, and the three manual pages under `~/.local/share/man`. It
runs the installer script on files the client sends. The installer never
edits your shell startup files. When the server's login shell does not find
`irsync`, `pair` shows the line to add.

A released irsync downloads the release files of its own version on the
client, for every server platform, and checks their SHA-256:

```text
irsync: downloading irsync v0.1.0-beta.1 for linux aarch64
irsync: set up myserver over ssh:
irsync:   installed irsync 0.1.0-beta.1 (ae5ebe5) in ~/.local/bin, with fileset and the manual pages (release files for linux aarch64)
```

If the download fails and the server has the same OS and CPU, irsync says so
and installs this machine's own files instead. Then `fileset` and the manual
pages come along only if they sit beside this machine's `irsync`:

```text
irsync:   installed irsync 0.1.0-beta.1 (ae5ebe5) in ~/.local/bin (no fileset, no manual pages: this machine has none beside its irsync)
```

The download happens on the client, which needs `curl` or `wget`, and a
`tar` that reads `.xz`. The client also checks that an irsync release key
signed the release's list of SHA-256 sums for exactly this version. The
server needs no internet access, but it needs `tar` to unpack the files it
is sent, and a home directory you can write to. Either way the server runs
the new binary with `--version` before it replaces anything, and keeps what
it had unless the binary reports the client's exact build.

Release files exist for Linux on x86_64 and arm64 and for macOS on Apple
Silicon. For any other server, and for a client built from source whose
server has another OS or CPU, `pair` stops and changes nothing. Then
install irsync on the server yourself and
[set the server up by hand](../tutorial/first-sync.md#set-the-server-up-by-hand).
Installing it on the server does not make `pair` work, unless you run
with `--no-ssh-install`: `pair` otherwise uses its own copy of the client's
exact build. See
[Troubleshooting](troubleshooting.md#pair-stops-the-server-runs-another-os-or-cpu)
for the messages.

To copy nothing to the server, use `--no-ssh-install` (or `ssh_install = false`
in the config file). `pair` then uses the irsync at `~/.local/bin/irsync`
there, and stops if it is missing or another version.

## Keep the daemon running

`pair` makes the daemon a user service, so it starts again after a reboot.
Pass `--no-service` to skip that. See
[Run the daemon](run-the-daemon.md#run-the-daemon-as-a-user-service).
