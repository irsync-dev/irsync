# Your first sync

In this tutorial you push a photo library from a client to a server, resume
an interrupted push, and pull the library back. First the quick way, over
ssh; then the same setup by hand, for when ssh is not available or you want
to share only named directories.

You need `irsync` on the client (see [Install](../introduction.md#install)).
The quick way installs it on the server for you; for the setup by hand,
install it on the server too.

The client runs `irsync`. The server runs `irsync daemon`, the *daemon*,
which serves its directories. The two machines find each other by the
daemon's identity, an `EndpointId`, not by an address: you need no port
forwarding, dynamic DNS or SSH tunnel.

You write no configuration file by hand. Each machine generates its key the
first time a command needs it and says so on stderr:
`generated identity <path>`.

## The quick way: over ssh

If `ssh myserver` already logs you in to the server, one command on the
client sets it up, and then you can push:

```sh
irsync pair myserver
irsync -av ~/Photos/ myserver:/srv/photos/
```

`irsync pair` is the only command that uses ssh. It takes `myserver` as an
ssh destination and sets the server up. A sync never uses ssh. The account you
log in to must not be root: irsync refuses to set up a daemon as root.
`pair` says what it did:

```text
irsync: pairing with myserver over ssh
irsync: set up myserver over ssh:
irsync:   installed irsync 0.1.0 (1a2b3c4) in ~/.local/bin, with fileset and the manual pages
irsync:   generated the daemon identity
irsync:   granted this client (myclient, a3e4753e5e) access to any path
irsync:   started the daemon
irsync:   the daemon runs as a systemd user service (irsync.service), also after a reboot
irsync:   saved server myserver -> 4a29f16a4f
irsync: paired with myserver; syncs go over iroh, not ssh
```

In steps, it:

- installs irsync on the server, the same way `install.sh` does: `irsync`,
  `fileset` and the manual pages go to `~/.local/bin` and
  `~/.local/share/man`. The client sends the files; the installer script
  places them. A released irsync downloads the release files of its own
  version on the client, for any server platform, and checks them. If that
  download fails and the server has the same OS and CPU, it installs this
  machine's own files instead. The download needs `curl` or `wget`, and
  `tar`, on the client; the server needs no internet access. A build from
  source has no release to download, so it works only when the server has
  the same OS and CPU. Not every platform has a release (an Intel Mac has
  none). Otherwise irsync stops and says so; install irsync on both machines
  and [set the server up by hand](#set-the-server-up-by-hand). Add
  `--no-install` to copy nothing and use the irsync already at
  `~/.local/bin/irsync` there. The installer never edits your shell startup
  files. When the server's login shell does not find `irsync`, the installer
  prints the line to add to a startup file there, and you see it while
  `pair` runs;
- lets this client reach any path your account on the server can, read-write
  (an [immediate grant](../reference/configuration.md#immediate-grants)). The
  server saves the client under its host name, here `myclient`, as a
  `[clients.myclient]` entry, so its grants show that name instead of a long id. The first push to
  `/srv/photos` makes it an export, granted to this client only;
- starts the daemon and makes it a user service: a systemd service on Linux,
  a launchd agent on macOS. It keeps running and starts again after a reboot.
  On a Mac that happens once someone logs in there. Pass `--no-service` to
  start it in the background only, until the next reboot;
- saves `myserver` as a server on the client, with the daemon's
  `EndpointId` and `ssh = "myserver"`. For `mike@myserver` or an IP
  address, see
  [which names pair saves](../how-to/pair.md#the-names-pair-saves).

If the server has no systemd or launchd user session, or the service file
there is not one irsync wrote, `pair` starts the daemon in the background
and prints the steps to make it a service yourself (see
[Run the daemon](../how-to/run-the-daemon.md#run-the-daemon-as-a-user-service)).

The push runs over iroh. `-a` recurses into directories and keeps
symlinks, permissions, times, group, owner and device files; `-v` lists each
file. The trailing `/` on `~/Photos/` copies the directory's contents, as in
rsync. Add `-P` to see a live block at the bottom of the screen that shows
the transfer as it runs.

Syncs use the saved server and never touch ssh. If the daemon has stopped, a sync
waits 15 seconds for an answer, then stops and says
`myserver did not answer`. It names what to check on the server. To start the
daemon again, run `irsync pair myserver`. A sync to a name irsync does not
know stops too, and tells you to run `irsync pair`.

[Pair with a server over ssh](../how-to/pair.md) covers the options: pairing
again after you update irsync, keeping the daemon out of the public DHT with
`--no-dht`, another ssh command with `--rsh`, and limiting the client to
named exports.

### Run it again

```sh
irsync -av ~/Photos/ myserver:/srv/photos/
```

Nothing has changed, so nothing is sent.

### Interrupt a transfer and resume it

Add a few large files to `~/Photos`, start the push, and press Ctrl-C while
it runs. Then run the same command again:

```sh
irsync -av ~/Photos/ myserver:/srv/photos/
```

`irsync` finds the stopped session and says `resuming session <id> from
<time>`. Files that were partly sent continue from the last verified chunk.
The same happens after a dropped network connection, except that `irsync`
redials on its own, for up to 24 hours by default.

### Pull the library back

Pull it into a new directory on the client:

```sh
irsync -av myserver:/srv/photos/ ~/Photos-copy/
```

A pull is the same command with the remote side first. To see what a pull
would fetch without writing anything, give the remote side alone:

```sh
irsync myserver:/srv/photos/
```

That is the whole quick way. The rest of this tutorial sets a server up by
hand. It shows what the quick way does for you.

## Set the server up by hand

Here you export two directories under names, grant the client access to
them, and start the daemon yourself. Nothing goes through ssh. irsync must
be installed on both machines.

This part is an alternative to the quick way, not a next step. Follow it on a
server that was not set up over ssh: on one that was, a daemon is already
running, so step 4 would start a second one, and step 5 would replace the
`myserver` server that the quick way saved.

### 1. Find the client's EndpointId

On the client, print its identity:

```sh
irsync id show
```

Under the `client:` label, the first line is the public key in OpenSSH form,
the second the client's `EndpointId`, 64 hexadecimal characters:

```text
client:
ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIKPkdT5ep05k50XpxNELD9dR2+AwSddC6w6lRciXA9+6 irsync
a3e4753e5ea74e64e745e9c4d10b0fd751dbe03049d742eb0ea545c89703dfba
```

If the client already has an ssh key at `~/.ssh/id_ed25519`, `irsync` uses it
instead of generating one, and reads it without changing it.

Copy the last line. You paste it on the server in step 3.

### 2. Export two directories on the server

On the server, create the directories and export them under a name:

```sh
sudo mkdir -p /srv/photos /backup
sudo chown "$USER" /srv/photos /backup
irsync export add photos:/srv/photos
irsync export add backup:/backup
```

`irsync export add` writes `~/.config/irsync/irsync.toml` and prints the
daemon's `EndpointId` on stdout. Copy it. You paste it on the client in
step 5.

This is the ID clients connect to. `irsync id show --daemon` prints it again
later. Plain `irsync id show` on the server prints the server's *client*
identity, which is a different key: nothing answers to it, so a client given
that one waits, then gives up with a message that points at
`irsync id show --daemon`
(see [Troubleshoot a first sync](../how-to/troubleshooting.md#a-host-added-by-hand-never-answers)).

### 3. Grant the client access

Still on the server, give the client's `EndpointId` a readable name, as a client, and grant
it both exports. Replace `<client-id>` with the line you copied in step 1:

```sh
irsync client add myclient <client-id>
irsync grant add photos myclient
irsync grant add backup myclient
```

Each `grant add` gives read-write access. The daemon refuses every key that no
grant names, before it reads any request.

### 4. Start the daemon

Start the daemon in the foreground, in a terminal you leave open:

```sh
irsync daemon
```

It prints a banner, its `EndpointId` and the sockets it listens on, then logs
warnings to stderr:

```text
irsync daemon 0.1.0-beta.1 (a1b2c3d) started
  relay: default (n0)
  discovery: dns, dht, mdns
endpoint id: 4a29f16a4f80a3fd171ef5573c106dbda470ec24f5a97933752746b567ab9aff
listening on: 192.168.1.10:41235
```

The daemon publishes its addresses so that a client holding its `EndpointId`
can find it. Leave it running for the rest of the tutorial.

### 5. Name the server on the client

On the client, give the server's `EndpointId` the name `myserver`. Replace
`<server-id>` with the line you copied in step 2:

```sh
irsync server add myserver <server-id>
```

The client now pins this `EndpointId` for `myserver`. If anything else
answers, the connection fails; there is no trust-on-first-use prompt.

### 6. See what the server shares

Ask the daemon what this client may reach:

```sh
irsync myserver:
```

The daemon answers with one line per export, its name, its real path and your
access:

```text
backup /backup rw
photos /srv/photos rw
```

### 7. Push the library

Copy the contents of `~/Photos` into the `photos` export:

```sh
irsync -av ~/Photos/ myserver:photos/
```

Resuming and pulling work as in the quick way, with `myserver:photos/` in
place of `myserver:/srv/photos/`.

### 8. Push by real path

You can also name a path on the server directly. The path must lie inside an
export that the client has a grant for, here `backup`:

```sh
irsync -av ~/Documents/ myserver:/backup/myclient/Documents/
```

A path outside every export the client may use is refused as "no access",
whether it exists or not.

## What you did

- `irsync pair` set the server up over ssh and saved a server; every sync
  after that went over iroh.
- By hand, you copied two `EndpointId`s, one in each direction: the client's
  to the server, the daemon's to the client. That exchange is the whole of the
  trust setup.
- You pushed, resumed, listed and pulled with rsync's flags and its
  trailing-slash rule.

The name is optional: the raw `EndpointId` works in its place, as in
`irsync -av ~/Photos/ <server-id>:photos/`, and `irsync grant add photos
<client-id>` works without `client add`.

## Next steps

- [Troubleshoot a first sync](../how-to/troubleshooting.md): what the usual
  first-run errors mean.
- [Pair with a server over ssh](../how-to/pair.md): the options of the
  quick way.
- [Command line](../reference/command-line.md): every flag.
- [Remote addresses](../reference/remote-addresses.md): how the daemon
  resolves `myserver:photos`, `myserver:Pictures` and `myserver:/backup`.
- [Management commands](../reference/management.md): `server`, `client`,
  `export`, `grant`, `id` and `config`.
- [Seed the index](../how-to/seed-the-index.md): when the server already
  holds a copy that `irsync` did not make.
- [Run the daemon](../how-to/run-the-daemon.md): under systemd or launchd,
  as its own account.

To check that the copy arrived intact, see [Verify an irsync transfer](../fileset/verify-irsync.md).
