# irsync website and book: design

Date: 2026-11-08

## Intent

irsync is about to be shown to people outside its author's head. The first
readers are close friends and peers with deep unix knowledge who come from
rsync and distributed systems. Once their feedback is in, irsync will be
announced on social media and spread by word of mouth.

The website serves both waves. It has two parts:

- a landing page at `irsync.org/` that says what irsync is and why it
  exists, and sends the reader into the book;
- a book at `irsync.org/book/`, built with mdbook and organised by the
  Diátaxis framework (tutorials, how-to guides, reference, explanation),
  which becomes the single home of irsync's user and contributor
  documentation.

The first readers will judge irsync on its technical claims. The site
earns their trust by being precise and honest, including about where
rsync is still ahead.

## Goals

- A landing page that makes the case for irsync in the first minute to
  someone who knows rsync, and links every claim to the book page that
  backs it.
- One book holding everything a user or contributor needs, describing
  irsync as it is now.
- Each existing user or contributor document moved into the book, not
  copied. After the move there is exactly one place for each fact.
- A staging site (`dev.irsync.org`) that tracks `main` automatically, and a
  production site (`irsync.org`) that changes only on a deliberate promote.

## Non-goals

- Release notes, a changelog, a blog or news section.
- Binary downloads or packages. Installing stays `cargo install` from the
  source.
- Search engine work, analytics, or any tracking.
- A logo or new visual identity. The design system says there is no mark
  yet; the name is set in plain type.

## Anti-goals

These make the site a failure even if it builds and deploys:

- Marketing gloss: superlatives, exclamation marks, claims without a number
  or a link to the mechanism. The first readers will discount the whole site.
- A claim the code does not support. When the code and a document disagree,
  the code wins and the book says what the code does.
- Duplicated documentation: the same fact in the book and in a file in the
  repo, which then drift apart.
- History in the book. Decision records, superpowers specs and plans, review
  narratives ("written on ... against commit ..., resolved ...") stay where
  they are and are not reproduced verbatim.

## Sources and authority

All content comes from this repository. When in doubt, the code is the
source of truth, then the documentation in the repository.

The visual design comes from the irsync design system
(https://claude.ai/artifact/GRRG3LBn57iSDvG9m3CrN1): its colour, type,
spacing, radius and shadow tokens, its focus ring, its terminal-output
conventions (status glyphs, progress line), and its rules on accent use
(one accent per view, for bytes in flight, the primary action and links).
The design system is used for the visual design only. Its voice and
terminology rules are not adopted where they conflict with the code: the
book says *client* and *daemon* (as `irsync daemon` and `[client]` do), and
`EndpointId` (as the CLI does), not *source/destination peer* or *node id*.

Known disagreements to settle against the code while writing (examples, not
an exhaustive list):

- The README gives `--parallel` a default of 32 files in flight;
  `docs/IRSYNC_VS_RSYNC.md` says files in flight go up to 4096.
- The exact layout of the live progress block shown on the landing page.

## The landing page

Direction A ("terminal first") was chosen, with the rsync comparison added
further down. From top to bottom:

1. A top bar: the name `irsync` in plain type; links to the book, the source
   (`https://github.com/irsync-dev/irsync`) and install.
2. The headline "rsync, rebuilt for today's networks" and the README's
   opening paragraph (one-way synchroniser, both ends, iroh, NAT traversal
   without port forwarding, dynamic DNS or SSH tunnels).
3. One primary action, "Read the book" (accent), and a quiet "Install".
4. A terminal frame showing a real session from the quick start: adding a
   host alias, `irsync home:` listing exports, and a push with the live
   progress block as irsync draws it.
5. Three claims, each a card linking into the book: no port forwarding
   (identity, not address; pinned keys), survives a bad link (sessions
   outlive connections; the next run resumes), fills the link (many files
   in flight, scanning alongside transfer, persistent index).
6. "If you know rsync": a short table of differences drawn from
   `docs/IRSYNC_VS_RSYNC.md` (sections 2 and 4), verified against the code.
   It deliberately includes at least one row where rsync is ahead
   (compression: rsync has `-z`, irsync has none). Links to the full
   comparison in the book.
7. Install: the `cargo install` line against the GitHub repository, and the
   supported platforms (Linux and macOS, x86_64 and aarch64, client and
   daemon). `fileset` is mentioned as an optional, independent tool.
8. The book's four Diátaxis sections as a map, each with one line saying
   what is in it.

The mockup shown during brainstorming is at
`.superpowers/brainstorm/*/content/landing-v2.html` (not committed). It is
a layout reference; its copy is not final and its rsync column must be
checked.

The page works at phone width (single column, no horizontal scroll, the
terminal frame scrolls inside itself), and in both the dark (primary) and
light themes of the design system, following `prefers-color-scheme`.

## The book

Built with mdbook. Themed from the same design system tokens as the landing
page, so the two read as one site: same colours, type, spacing and code
styling; dark first, light as well.

Parts, in this order. Within each part the four Diátaxis kinds stay
separate; a page is one kind.

- **Introduction**: what irsync is, who it is for, how the book is organised.
- **Tutorial**: your first sync, a laptop and a home server, pushing and
  pulling a photo library (from the README quick start).
- **How-to guides**: run the daemon system-wide, under systemd, under
  launchd; change exports and reload settings while it runs; connect by
  direct address or through a self-hosted relay; seed the index from an
  existing tree (`irsync index`); clean up (`irsync gc`); collect telemetry
  (local collector, OpenTelemetry export); verify a transfer with `fileset`.
- **Reference**: the command line (rsync-compatible flags, irsync-specific
  flags, subcommands); remote addresses and their resolution order;
  `irsync.toml` and `hosts.toml`; metrics and spans; the wire protocol;
  `fileset` commands, output and exit codes; known limitations.
- **Explanation**: sessions and resume; identity and trust (pinned
  EndpointIds, no trust-on-first-use, grants and nested exports); how
  connections are found (iroh, relays, DHT, discovery); the transfer
  algorithm compared with rsync, including where rsync is ahead; receiving a
  changed file.
- **Contributing**: setting up a clone and the checks before pushing;
  testing; `filegen`; `irsync-stress`; `irsync-netstress` (its use, its
  reference, and how it works); bug-hunting campaigns; performance tuning.

This outline is a draft. The builder firms it up against the code and the
moved material, keeping the parts and the Diátaxis split.

### What moves into the book

Moved means: the content goes into book pages (rewritten where needed to
describe the current state and to fit one Diátaxis kind), the original file
is deleted, and every link to it in the repository is updated.

| Today | Into the book as |
| --- | --- |
| `README.md` (everything but a short pitch) | tutorial, reference (flags, addresses, subcommands), how-to, known limitations |
| `docs/daemon.md` | how-to (daemon), reference (files, state directory), explanation (relays, DHT, discovery) |
| `docs/protocol.md` | reference (wire protocol), explanation (sessions and resume) |
| `docs/telemetry.md` | how-to (collect telemetry), reference (metrics, spans) |
| `docs/telemetry/*.yaml`, `prometheus.yml` | stay as files; the how-to links to or includes them |
| `docs/DELTA_RECEIVE.md` | explanation |
| `docs/IRSYNC_VS_RSYNC.md` | explanation, rewritten to describe the current algorithm; the review history (dated findings, "Resolved" notes, ranked remedies) is not carried over |
| `DEVELOPMENT.md` | contributing |
| `crates/fileset/README.md` | `fileset` how-to and reference |
| `crates/filegen/README.md` | contributing |
| `crates/stress/README.md` | contributing |
| `crates/netstress/README.md` | contributing |
| `docs/NETSTRESS_HOW_IT_WORKS.md` | contributing (explanation) |
| `docs/STRESS_BUG_HUNTING.md` | contributing (how-to) |

`README.md` stays, shortened to a pitch, the install line and a link to
irsync.org and the book. Crate READMEs are shortened the same way (one
paragraph and a link) rather than deleted, because crates.io and code hosts
show them.

### What stays where it is

`docs/*_DECISIONS.md`, everything under `docs/superpowers/`, and `TODO.md`.
The book may link to the decision records from a contributing page for
readers who want the history, but does not reproduce them.

Content in the stay-put files that describes current behaviour and exists
nowhere else may be restated in the book in present tense; the stay-put
file is not edited.

## Repository layout and build

- `site/`: the landing page source.
- `book/`: the mdbook source (`book.toml`, `src/`, the theme).
- One build produces a single output directory with the landing page at
  `/` and the book at `/book/`.
- The theme is generated from the design system's `tokens.json`, shared
  by the landing page and the book. The design system publishes
  `project/tokens.json`; a copy is vendored into the repository (for
  example `site/tokens.json`) and the CSS custom properties are generated
  from it, so a token change is one file replaced and a rebuild. Both type
  families are system font stacks: no font files ship and nothing is
  loaded from a third party.

The site is static. No server code.

## Deployment

Cloudflare Pages, as two Pages projects, one per target. `irsync.org` is
registered with Cloudflare and its DNS is there, so both projects and
their custom domains are created as the first deployment step (by the
author; quick to do):

- `dev.irsync.org`: staging. Every push to `main` updates it automatically.
- `irsync.org`: production. Changes only when someone promotes the current
  staging build by hand.

The repository is currently hosted only at `git.wildpaner.net` and will move
to `https://github.com/irsync-dev/irsync`. Until the move, the build and
both deploys run from a developer machine with `wrangler`, as documented
commands; the automatic staging deploy starts with the move. After the
move, a GitHub Actions workflow builds on each push to `main` and deploys
to staging; promotion to production stays a manual step (a manually
triggered workflow, or the documented command).

Promotion ships exactly what staging serves: the staging build's output
is kept (as a workflow artifact, or locally before the move) and the same
files are uploaded to production, not rebuilt.

`dev.irsync.org` is public but not indexed: every response carries
`X-Robots-Tag: noindex` and its `robots.txt` disallows everything.
Production is indexed normally.

All install instructions and source links point at the GitHub repository.
The repository is published on GitHub before the site goes to
production, so those links are live from the first public day. The doc
move may land on `main` (and staging) before then.

## Decided by the author

- Audience: rsync- and distributed-systems-literate peers first, social
  media and word of mouth later.
- Two parts: landing page at `/`, mdbook at `/book/`, Diátaxis.
- Landing page direction A, with the rsync comparison further down.
- Visual design from the irsync design system; content from the repository
  only; code over documentation when they disagree.
- The book describes the current state; history stays in place.
- User and contributor documentation moves into the book; no duplication.
- Hosting on Cloudflare Pages: `irsync.org` production, `dev.irsync.org`
  staging; `main` deploys to staging automatically, production is promoted
  by hand.
- Source at `https://github.com/irsync-dev/irsync`, published there before
  the site goes to production.
- Two Cloudflare Pages setups, one for `irsync.org` and one for
  `dev.irsync.org`; the domain is already on Cloudflare.

## Calls made during design (not yet confirmed by the author)

- Source layout `site/` and `book/`, one combined build output.
- `wrangler` deploys from a developer machine until the GitHub move.
- The book's part order and the draft chapter list above.
- Crate READMEs shortened to a paragraph and a link, not deleted.
- The `docs/telemetry/` config files stay as files.
- `tokens.json` vendored into the repo and the theme generated from it.
- Staging deploys are manual commands until the GitHub move, automatic
  after.
- Promotion re-uploads staging's exact build output; no rebuild.
- `dev.irsync.org` public with `noindex`, not behind access control.

## Left to the builder

- The landing page's final copy, within the rules above.
- The exact chapter list and page boundaries.
- How the theme is produced from the tokens.
- The build tooling around mdbook (plain `mdbook build`, a Makefile, a
  script).
- Checking every claim and number against the code, and resolving the
  disagreements listed under Sources and authority.
