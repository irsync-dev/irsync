# First-use experience: site, book and two CLI hints

Date: 2026-11-10

## Why

The website (irsync.org, built from `site/` and `book/`) still tells the
setup story from before SSH onboarding. A newcomer who follows a link sees a
terminal that starts with `irsync host add myserver <64 hex chars>`, an ID
that comes out of nowhere, and an install section that says to install on
both machines, which is no longer needed when `ssh myserver` works. The one
real first-run failure seen so far is a copy mistake the tool does nothing to
catch: in manual setup, the user ran `irsync id show` on the server, got the
server's *client* ID, gave it to the client as the server's ID, and the
connection timed out with nothing pointing at the cause.

The goal is a first visit, install and first sync that work on the first try
for the target reader.

## Who it is for

An rsync user who follows a link (a post, a chat message, a README). They
know `-av`, trailing slashes and ssh, and decide within a minute or two
whether irsync is worth trying. People sent to the site directly get the same
page; designing for the rsync user covers them.

## Goals

- The home page shows, near the top, both ways to set up: the one-command
  ssh way and the manual way, as two tabs.
- A visitor knows before installing what they need and that irsync is alpha.
- The install step works for someone without Rust, and says what to expect.
- The tutorial gives a short, complete first success along the ssh path,
  with no contradictions.
- Copying the wrong ID in manual setup is hard to do, and when it happens
  the error says what to check.

## Non-goals

- Release binaries or an install script. Install stays `cargo install`;
  binaries come with the beta.
- Redesigning the site's look, tokens or build. Use the existing styles.
- Rewriting reference or explanation pages beyond what the changes below
  touch.

## Anti-goals

- Adding JavaScript to the site. It has none today; the tabs must work
  without it.
- A first example that needs an ID the reader doesn't have yet.
- Hiding the alpha status in small print.

## What to build

### 1. Home page (`site/index.html`, `site/style.css`)

Top to bottom:

1. **Alpha notice**, above the headline, in the site's warning colours
   (`--ir-warn`, `--ir-warn-soft`). Content: irsync is alpha software; the
   command line and the network protocol may change before the beta (and so
   versions may not talk to each other); keep another copy of anything you
   sync with it. Exact wording is the builder's.
2. **Headline and intro**, rewritten for the rsync reader. Fix the broken
   sentence "irsync sessions are using multiplexed over QUIC" and use a real
   dash in the headline. Say early that the flags and the trailing-slash rule
   are rsync's. Keep the two claims: gets through NATs and firewalls without
   port forwarding (via iroh), and a transfer outlives the connection.
3. **What you need**, one short line or list: Linux or macOS on both ends;
   Rust, to build it; ssh access to the server (optional).
4. **Buttons**: Install irsync, and Your first sync (instead of "Read the
   documentation").
5. **Two tabs**, "With ssh" (selected by default) and "Without ssh", each
   with a terminal example and one line above and below.
   - *With ssh*: "If `ssh myserver` logs you in, install on the client
     only." The command `irsync -av ~/Photos/ myserver:/srv/photos/`, then the
     setup lines it prints (taken from the tutorial's real output), then a
     couple of file names. Caption: later runs go over iroh and don't use ssh.
   - *Without ssh*: "Without ssh, or to share only named folders: install on
     both machines and exchange two IDs once." The commands, each marked
     `client$` or `server$`: `irsync id show` (client), `irsync export add
     photos:/srv/photos` (server, prints the server's ID), `irsync grant add
     photos <client-id>`, `irsync daemon`, `irsync host add myserver
     <server-id>` (client), `irsync -av ~/Photos/ myserver:photos/`. Every ID
     placeholder must have a visible source in the example. Update the
     comments if the `id show` output changes (section 4).
   - Tabs are pure CSS (radio inputs and labels, or an equivalent), work by
     keyboard, and show a focus ring.
6. **The live progress block**, kept from today, below the tabs and shared by
   both. Caption: either way, the push looks like this. Trim the caption's
   current wording to fit.
7. Three cards, "If you know rsync", the book map, footer: as today.
8. **Install section**: irsync builds from source with `cargo`; if you have
   no Rust, get it from rustup.rs first (show the rustup command); the build
   takes a few minutes. Then the `cargo install` line. Then: "Release binaries
   will be provided once the beta starts." Replace "Install it on both
   machines" with: with ssh the client is enough, and the first sync copies
   irsync to the server when both run the same OS and CPU. When they differ,
   install irsync on both machines and use the manual setup (the "Without
   ssh" tab). Don't suggest installing on the server by hand and re-running
   the ssh setup: that doesn't work (see 4c). Keep the fileset paragraph.

A mockup of items 1 to 8 was agreed in the brainstorm (tabs version):
`docs/superpowers/specs/2026-11-10-first-use/home-mockup.html`. It loads the
site's styles from `target/site/`, so run `site/build.sh` before opening it.
Follow its layout and tab structure; its text is a draft, and the yellow
dashed "mockup" label is not part of the page.

`site/404.html`: rename the nav link "book" to "documentation" to match the
home page.

### 2. README

Same alpha notice and install wording as the home page (Rust needed, rustup,
build time, binaries at beta). Its ssh-first example stays. Update the manual
example's comment "the second line is its EndpointId" to match the new
`id show` output.

### 3. Book

- **Introduction**: add the alpha notice and the install wording.
- **Tutorial (`tutorial/first-sync.md`)**:
  - Fix the opening: irsync is needed on the client; on the server too only
    when you skip ssh. Point to the install section of the home page or the
    introduction, not to the GitHub README.
  - Part one, the ssh way, as a complete short first success: the one
    command and what it prints; run it again and nothing is sent; Ctrl-C a
    run and resume it; pull back. Keep the paragraph on what happens later
    (the daemon after a reboot, the 15-second check).
  - Move the ssh extras out of the tutorial into a how-to page (new, or a
    section of an existing one; builder's call): `--ssh` to re-run setup
    (for example after upgrading the client), `--no-remote-dht`, `--rsh` /
    `IRSYNC_RSH`, and `daemon.immediate = false` before the first sync. The
    tutorial keeps one line pointing there.
  - Part two, "Set the server up by hand": the current steps 1 to 10,
    adjusted so steps already shown in part one (resume, pull) are not
    repeated in full. In the step where the server's ID is copied, say plainly
    that it is the *daemon's* ID, printed by `export add` and by `irsync id
    show --daemon`, and that the server's own `irsync id show` is the wrong
    one.
- **New page: Troubleshooting** (`how-to/` or `reference/`, builder's call;
  linked from the tutorial and the introduction). Each entry: the message
  you see, the cause, the fix. Start with:
  - *The client times out on a host added by hand*: most likely the client
    was given the server's client ID instead of its daemon ID. Fix: `irsync
    id show --daemon` on the server, `irsync host add` with that.
  - *ssh setup stops because the server's OS or CPU differs*: install
    irsync on both machines and set the server up by hand (tutorial part
    two). Release binaries at beta will remove this step.
  - *"rejected this client's key"*: no grant for this client; how to grant,
    and why ssh never fixes it.
  - *Slow transfer, `[relay]` in the progress line*: hole punching failed;
    link to Connections / direct-and-relay.
  Only list failures whose messages exist in the code; quote them exactly.
- Update every page that describes `id show` output (management reference,
  files-and-state, run-the-daemon, tutorial) to match section 4.

### 4. CLI changes (`crates/irsync`)

Three small changes, no new features.

**a. `irsync id show` labels its output.** Today the default prints the
client's public key line and `EndpointId` with no label, so on a server it
is easy to take it for the server's ID. After the change:

- The default output says it is the client's identity, in the same
  `client:` / `daemon:` labelling that `--all` already uses (my call: one
  style for all three forms). `--daemon` labels its lines `daemon:`.
- When the default runs on a machine that also has a daemon identity, it
  adds a note (on stderr, so stdout stays clean) that the daemon has its own
  ID, that clients need that one, and how to print it (`irsync id show
  --daemon`).
- No backward compatibility is needed (no users yet), but update every test
  and doc that reads the old format.
- Exact line format is the builder's call, as long as copying the key line
  into an `authorized_keys`-style file stays easy and the client/daemon
  distinction can't be missed.

**b. A hint when a hand-added host doesn't answer.** For a host without ssh
setup (an alias with no `ssh` entry, or a raw `EndpointId`), the client gives
up after about 15 seconds with a plain error (`main.rs`, the
`Recheck::NotSsh` path). Add a hint to that error: if you copied this ID by
hand, it must be the server's daemon ID; run `irsync id show --daemon` on
the server and compare. Hosts set up over ssh don't get it (they re-check
over ssh already).

**c. The "different OS or CPU" message points to the manual setup.**
Today `ensure_installed` in `crates/irsync/src/ssh/client.rs` says "install
irsync on {dest} by hand". That leads nowhere: ssh setup only accepts its
own copy at `~/.local/share/irsync/bin/irsync` with exactly the client's
build, so a `cargo install` on the server (into `~/.cargo/bin`) changes
nothing and the next run fails the same way. Change the message to say:
install irsync on both machines and set the server up by hand, with a
pointer to the tutorial's manual part. Making ssh setup accept an irsync on
the server's `PATH` was considered and left for later; release binaries at
beta are the real fix.

All three changes get tests in the existing style.

## Constraints

- The site check (`site/tools/check.py`) checks links and that flags named on
  the site exist in the built `irsync`; it must keep passing. Site tool tests
  and `site/build.sh` run in CI.
- No new runtime dependencies for the site; no web fonts.
- Example names: client `myclient`, server `myserver`.

## Left to the builder

Wording throughout; the troubleshooting page's location; whether the ssh
extras get a new how-to page or a section; exact `id show` line format within
the rules above; how the tabs are built in CSS.
