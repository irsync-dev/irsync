# Your first sync

In this tutorial you push a photo library from a client to a server, list
what the server shares, and pull the library back. You need `irsync`
installed on both machines (see the
[README](https://github.com/irsync-dev/irsync#install)) and a shell on each.

The client runs `irsync`. The server runs `irsync daemon`, the *daemon*,
which serves the directories you export. The two machines find
each other by their identity, an `EndpointId`, not by an address: you need no
port forwarding, dynamic DNS or SSH tunnel.

You write no configuration file by hand. Each machine generates its key the
first time a command needs it and says so on stderr:
`generated identity <path>`.

## 1. Find the client's EndpointId

On the client, print its identity:

```sh
irsync id show
```

The first line is the public key in OpenSSH form, the second the client's
`EndpointId`, 64 hexadecimal characters:

```text
ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIKPkdT5ep05k50XpxNELD9dR2+AwSddC6w6lRciXA9+6 irsync
a3e4753e5ea74e64e745e9c4d10b0fd751dbe03049d742eb0ea545c89703dfba
```

If the client already has an ssh key at `~/.ssh/id_ed25519`, `irsync` uses it
instead of generating one, and reads it without changing it.

Copy the second line. You paste it on the server in step 3.

## 2. Export two directories on the server

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

## 3. Grant the client access

Still on the server, give the client's `EndpointId` a readable name and grant
it both exports. Replace `<client-id>` with the line you copied in step 1:

```sh
irsync host add myclient <client-id>
irsync grant add photos myclient
irsync grant add backup myclient
```

Each `grant add` gives read-write access. The daemon refuses every key that no
grant names, before it reads any request.

## 4. Start the daemon

Start the daemon in the foreground, in a terminal you leave open:

```sh
irsync daemon
```

It prints its `EndpointId` and the sockets it listens on, then logs to stderr:

```text
endpoint id: 4a29f16a4f80a3fd171ef5573c106dbda470ec24f5a97933752746b567ab9aff
listening on: 192.168.1.10:41235
```

The daemon publishes its addresses so that a client holding its `EndpointId`
can find it. Leave it running for the rest of the tutorial.

## 5. Name the server on the client

On the client, give the server's `EndpointId` the name `myserver`. Replace
`<server-id>` with the line you copied in step 2:

```sh
irsync host add myserver <server-id>
```

The client now pins this `EndpointId` for `myserver`. If anything else
answers, the connection fails; there is no trust-on-first-use prompt.

## 6. See what the server shares

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

## 7. Push the library

Copy the contents of `~/Photos` into the `photos` export:

```sh
irsync -av ~/Photos/ myserver:photos/
```

`-a` recurses into directories and keeps symlinks, permissions, times, group,
owner and device files; `-v` lists each file. The trailing `/` on `~/Photos/` copies the directory's
contents; without it, `irsync` would create `photos/Photos`. On a terminal a
live block at the bottom of the screen shows the transfer as it runs.

Run the same command again. Nothing has changed, so nothing is sent.

## 8. Interrupt a transfer and resume it

Add a few large files to `~/Photos`, start the push, and press Ctrl-C while
it runs. Then run the same command again:

```sh
irsync -av ~/Photos/ myserver:photos/
```

`irsync` finds the stopped session and says `resuming session <id> from
<time>`. Files that were partly sent continue from the last verified chunk.
The same happens after a dropped network connection, except that `irsync`
redials on its own, for up to 24 hours by default.

## 9. Pull the library back

Pull the export into a new directory on the client:

```sh
irsync -av myserver:photos/ ~/Photos-copy/
```

A pull is the same command with the remote side first. To see what a pull
would fetch without writing anything, give the remote side alone:

```sh
irsync myserver:photos/
```

## 10. Push by real path

You can also name a path on the server directly. The path must lie inside an
export that the client has a grant for, here `backup`:

```sh
irsync -av ~/Documents/ myserver:/backup/myclient/Documents/
```

A path outside every export the client may use is refused as "no access",
whether it exists or not.

## What you did

- You copied two `EndpointId`s by hand, one in each direction. That exchange
  is the whole of the trust setup.
- You pushed, resumed, listed and pulled with rsync's flags and its
  trailing-slash rule.

The alias is optional: the raw `EndpointId` works in its place, as in
`irsync -av ~/Photos/ <server-id>:photos/`, and `irsync grant add photos
<client-id>` works without `host add`.

## Next steps

- [Command line](../reference/command-line.md): every flag.
- [Remote addresses](../reference/remote-addresses.md): how the daemon
  resolves `myserver:photos`, `myserver:Pictures` and `myserver:/backup`.
- [Management commands](../reference/management.md): `host`, `export`,
  `grant`, `id` and `config`.
- [Seed the index](../how-to/seed-the-index.md): when the server already
  holds a copy that `irsync` did not make.
- [Run the daemon](../how-to/run-the-daemon.md): under systemd or launchd,
  as its own account.
