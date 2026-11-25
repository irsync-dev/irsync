# Product

<!-- impeccable:product-schema 1 -->

## Platform

web

## Users

Four groups, none ranked above the others:

- **rsync users with machines of their own.** They know rsync and want to
  sync between their laptop, home server and VPS, which sit behind NATs and
  firewalls.
- **People moving large trees over bad links.** Hotel wifi, mobile data,
  long-haul connections. They need a transfer to survive drops and pick up
  where it stopped.
- **People who need proof that a copy is exact.** Archives, backups, bitrot
  checks. They use `fileset` alongside irsync, or on its own to check copies
  made by rsync or rclone.
- **Admins.** They run the daemon on servers, grant client keys, and script
  syncs.

## Product Purpose

irsync is a one-way directory synchroniser in the mould of rsync, rebuilt
for today's networks. It runs on both ends of a transfer. It ships with
`fileset`, which checks that a copy is exact.

Success: a user installs it in one command, runs a sync that looks like the
rsync command they already know, and gets an exact copy, even across NATs
and dropped connections.

## Positioning

Three claims together, none leading:

1. **No open ports.** The other side is reached by its key, through NATs and
   firewalls, via iroh. No port forwarding, dynamic DNS or SSH tunnels. A
   relay is used only when no direct path exists, and cannot read the data.
2. **Survives network changes and outages.** A sync session outlives any
   single connection. Running the same command again resumes it.
3. **Correctness first.** A completed `irsync -a --delete` of a source that
   is not changing leaves an exact copy, and the user can check it with
   `fileset`.

## Operating Context

- A terminal. irsync takes rsync's common flags, so its commands look like
  rsync's. Install is `curl … | sh`; Homebrew, apt and dnf come from v0.1.0
  on.
- `irsync pair` sets up the server over ssh; syncs connect over iroh.
- Two web surfaces, built from this repository: the landing page at
  irsync.org, and the book (mdbook) once per release channel (`/book/`,
  `/stable/`, `/beta/`, `/alpha/`; `/dev/`, the book as on `main`, is on
  dev.irsync.org only). See
  `book/src/contributing/website.md`.

## Capabilities and Constraints

- Linux and macOS. Not Windows yet. No extended attributes, ACLs, `-X`,
  `-A` or `-z`. Both ends must run irsync: it is not a drop-in rsync
  replacement. The current list is `book/src/reference/limitations.md`.
- Release channels: stable, beta, alpha. There is no stable release yet;
  the first will be v0.1.0.
- The landing page shows the current versions from git at build time; it
  looks nothing up in the browser.

## Brand Commitments

- Name: `irsync`, always lower case. The companion tool is `fileset`.
- Voice: short sentences, plain words, for people who use irsync and have
  not read the code. Flags and config keys are written as the user types
  them.
- MIT licence. Built by Michael Wildpaner. Connections powered by iroh,
  credited by name.

## Evidence on Hand

- The book (`book/src/`): tutorial, how-tos, reference and explanation,
  including how correctness is verified
  (`book/src/explanation/correctness.md`) and the threat model.
- No evidence beyond that was confirmed. Do not invent benchmarks, user
  counts, testimonials or logos.

## Product Principles

1. **Beta honesty.** Until v0.1.0, every surface says plainly that irsync is
   beta: the command line and protocol may change, keep a backup, and what
   is not supported yet.
2. **Show the command.** A real, copyable command says more than a claim
   about it.
3. **Every claim links to its proof.** A promise on the landing page points
   to the book page that explains or verifies it.
4. **Familiar to rsync users.** Use rsync's words and flags where irsync
   shares them; say clearly where it differs.

## Accessibility & Inclusion

The site's own tests hold it to text contrast of at least 4.5:1, touch
targets of at least 44px, full keyboard use, and pages that work without
script (`site/tools/`).
