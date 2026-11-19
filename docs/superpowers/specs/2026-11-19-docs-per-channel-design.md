# The documentation per release channel

Issue: #105. Builds on
`2026-11-19-site-release-versions-design.md` (the release versions on the
homepage), which is merged.

## Intent

Someone who runs a released irsync should read the documentation of that
release. Today the site publishes one book, built from `main`, at `/book/`.
It describes code that may not be released yet: flags the reader does not
have, behaviour that has changed.

The site publishes the book five times instead: once for the main version,
once per release channel, and once from `main`.

## Goals

| Path | Which book |
| --- | --- |
| `/book/` | The main version: the one the homepage's install command installs (the stable if there is one, else the beta, else the alpha) |
| `/stable/` | The newest stable release |
| `/beta/` | The newest beta |
| `/alpha/` | The newest alpha |
| `/dev/` | `main`, unreleased |

- A channel's book is published whenever that channel has a version, even
  when it is older than the main version. (The homepage hides an obsolete
  beta or alpha; the books do not.) A channel with no version has no
  directory. Today: `/beta/` and `/alpha/` exist, `/stable/` does not, and
  `/book/` holds the beta.
- Every book says which version it documents.
- Only `/book/` is indexed by search engines.
- The homepage links into the right book.
- irsync.org still changes only through the manual promote.
- irsync.org never shows a version that is not published.

## Non-goals

- A switcher inside the book to jump between versions.
- Keeping more than the newest version of a channel.
- Redirects from old addresses. `/book/` keeps every address it has now,
  as long as the released book has the page.
- Checking the links in `README.md` and other files on GitHub against the
  released book.
- Man pages, the release tarballs, `site/install.sh`.

## Anti-goals

- A link on the homepage that leads nowhere in the published `/book/`.
- A book that claims a version it was not built from.
- A build that needs the network or GitHub to find or fetch a version's
  pages.

## Which version goes where

The versions come from git, as for the homepage (`site/tools/versions.py`):
a `v*` tag is a published release, and a `Cargo.toml` version ahead of the
tags, on `HEAD` or an `origin/release-X.Y` branch, is a release being
built.

- `/stable/`, `/beta/`, `/alpha/`: the newest version of that channel, with
  no hiding rule.
- `/book/`: the same book as the main version's channel. With no version
  at all (cannot happen any more), `/book/` is the book from `main`.
- `/dev/`: the working tree the build runs in. In CI that is `main`.
  Locally it includes uncommitted edits, so `site/serve.sh` keeps showing
  what you are writing.

A version's pages are read from the commit that carries it: the tag for a
published release; for a release being built, the commit `versions.py`
found its `Cargo.toml` version on (`HEAD` or the release branch's tip).
Committed content only, never the working tree.

## Approaches

**A. Released content, current look (chosen).** For each version the build
takes only the book's pages (`book/src`) from that version's commit and
builds them with today's `book.toml`, theme, tokens and mdbook. All books
look the same, one recipe covers every old and future version, and the
build can set the title, the address and the version line because it owns
the settings. The cost: old pages are rendered by newer tooling; a
breaking change there would show in the link check or by eye.

**B. Each tag builds itself**, whole, with its own recipe. Most faithful,
but `v0.1.0-alpha.4`'s site build is a different script, every tag may
want its own mdbook, tags are frozen so the version line and the edit
links would be patched in afterwards, and `/alpha/` would look like an
older site (its theme is 320 lines behind).

**C. Build each book once at release time and store it.** Ruled out: the
existing releases have no stored book, it needs GitHub at build time, and
the versions were decided to come from git.

## What each book shows

- **In the bar at the top of every page:** the title `irsync` followed by
  the version, for example `irsync 0.1.0-beta.5`. In `/dev/`:
  `irsync unreleased`. The title still links to the landing page
  (`book/theme/home-link.js`).
- **On the title page** (`introduction.html`), directly under the
  "Introduction" heading, one line: `This is the documentation of irsync
  0.1.0-beta.5.` In `/dev/`: `This is the documentation of the unreleased
  development version of irsync.`, followed by a link to `/book/` reading
  `Released documentation`.
- **"Edit this page"** (mdbook's `edit-url-template`) only in `/dev/`. A
  released page cannot be edited at its tag.
- The same look everywhere: the current theme.
- mdbook's own settings follow the path: `site-url` is `/book/`,
  `/stable/`, and so on, so each book's `404.html` and search work under
  its own address.

`/book/` is a full copy of its channel's book, built with `site-url`
`/book/`, not a redirect.

## The homepage

- Every general link goes to `/book/`, as now: "Documentation" in the
  header, the links in the text, the buttons.
- One exception: "Contributor guide" in the footer goes to
  `dev/contributing/setup.html`. Contributors work on `main`.
- On each version line by the "Install" heading:
  - the channel label ("Stable", "Beta", "Alpha") is a link to that
    channel's book: `stable/`, `beta/`, `alpha/`;
  - the date is a link to the release page,
    `https://github.com/irsync-dev/irsync/releases/tag/vX.Y.Z`. For a
    release still being built that page appears when it is published;
    until then only dev.irsync.org shows the link.
  - The version stays the copy button it is.
  - The links look like the page's other links in muted text: the line's
    colour, underlined. No new colours.
- `CONTRIBUTING.md` links to the contributor guide at
  `https://irsync.org/book/contributing/…`; those three links move to
  `https://irsync.org/dev/contributing/…`. `README.md` keeps its `/book/`
  links.

## A homepage link the released book lacks

The homepage is built from `main`; `/book/` from a release. A homepage
link to a book page or anchor that the released book does not have yet
fails the build, with a message that names the link. `site/tools/check.py`
already checks every relative link of the built site, so this needs no new
rule, only that the check runs over the built `/book/`. The author then
waits for the release or links to `dev/…` for now.

The same check runs over all five books' own links.

## Indexing

`site/_headers` gives `/stable/*`, `/beta/*`, `/alpha/*` and `/dev/*` the
header `X-Robots-Tag: noindex`, on every host. `/book/*` and the landing
page stay indexed on irsync.org. `site/tools/test_headers.py` pins this.

## The checks

- **Links and anchors:** all five books and the landing page, as built.
- **Fonts:** as now, over the whole built site.
- **irsync flags:** only the pages of the working tree (the source of
  `/dev/`), `site/index.html` and `README.md`, against the binary built
  from the same tree. A released book's flags are not checked: that would
  need that release's binary.
- **Moved files:** unchanged.

## How it is built

- `versions.py` already yields version, date and channel. It also has to
  yield the commit (or ref) a version's pages are read from. Its tests
  cover that with the throwaway repository they already use.
- `site/build.sh` builds each book into its directory in `target/site/`.
  How the pages of a commit are fetched (for example `git archive`), where
  the temporary trees live under `target/`, and how the per-book settings
  reach mdbook (environment overrides or a generated `book.toml`) are the
  builder's call.
- The version line on the title page is added to a copy of the pages, never
  to the repository's files and never to the working tree.
- If a version's commit has no `book/src` (cannot happen for today's tags)
  the build fails and names the version.
- The font clean-up (`strip_font_link.py`, removing each book's `fonts/`)
  applies to every book. The fonts stay in the one top-level `fonts/`;
  every book is one level below the root, so `../fonts/` style paths in the
  theme keep working. Check that, do not assume it.
- `site/serve.sh` rebuilds on every change. Five books must not make that
  painful: if a full build takes more than about ten seconds locally, the
  rebuild on change may build `/dev/` only and leave the other books from
  the first build. Builder's call, measured.
- `site.yml`'s triggers, the release gate and the resume run do not
  change. A release already rebuilds the site through its bump merge. The
  promote job and `sign-release.sh` change as the next section says.

## A book must come from its release

While a release is being built, its book is read from a branch tip. That
tip can move on before the release is published: another pull request
that touches the book merges, the site rebuilds, and the book that says
"0.1.0-beta.6" holds pages the release does not have. The tag then lands
on the earlier commit, and nothing rebuilds the site. Promoting that build
would put a wrongly labelled book on irsync.org. Decided: close this
fully, in three parts.

**1. The build records what it built.** A file `VERSIONS` at the root of
the built site, next to `BUILD`, one line per book: its path, the version,
the commit its pages were read from, and `published` (read from a tag) or
`unpublished` (read from a branch). `/dev/` is listed as `dev`, neither.
The homepage's version lines come from the same versions, so the file
covers them too. It is deployed like `BUILD`.

**2. `sign-release.sh --publish` rebuilds staging.** After it has
published the release and started the resume run, it re-runs the newest
push run of `site.yml` on `main` (`gh run rerun`). The tag exists by then,
so every book is read from its tag. This also refreshes the site after a
patch release cut from `release-X.Y`, which nothing did before. It then
prints the promote command as now, with a line that says to wait for that
run. If the re-run cannot be started (for example the newest run is still
going), the script says so and prints the command to do it by hand; it
does not fail the release, which is already published.

**3. Promote refuses a build with an unpublished version.** The promote
job reads `VERSIONS` from the build it is about to ship. If any line says
`unpublished`, it stops and names the version, and says to publish the
release and re-run the staging build first. So irsync.org never shows a
version nobody can install, in a book or on the homepage.

- The cost: between a bump merge and the publish, nothing can be
  promoted, not even an unrelated site fix.
- The way out, for a bump that is abandoned or a fix that cannot wait:
  starting the promote by hand with an input `allow-unpublished` set to
  true skips the refusal. It is off by default.
- The promote job takes the newest push run of `site.yml` on `main`
  whatever its state, and stops if that run is not a finished success.
  Today it takes the newest successful one, which would quietly ship an
  older build while a re-run is going.
- `site/deploy.sh promote` applies the same refusal to
  `target/site-staging/VERSIONS`, with a flag for the same way out.
- A build without a `VERSIONS` file (made before this change) is refused
  too, with a message to rebuild staging.

The tests of `sign-release.sh` cover the re-run and its failure; the
workflow policy tests and `site/deploy.sh`'s tests, where they exist,
cover the refusal.

## Docs

- `book/src/contributing/website.md`: the five books, where each comes
  from, what the build adds to them, the checks, the header rule, and how
  to link from the homepage to pages not yet released.
- `book/src/contributing/releasing.md`: after a release, `/book/` and the
  channel's book change with the site build; publishing re-runs that
  build; what to look at on dev.irsync.org before promoting; why a promote
  is refused and the way out. The note that a patch release waits for the
  next site build on `main` goes away.
- `AGENTS.md` and `CONTRIBUTING.md` where they name the published
  contributor guide.

## Constraints

- All of `AGENTS.md`: a branch, a PR that closes #105, `tools/ci.sh check`
  before the push.
- PR CI does not run the site tool tests or `check.py`. Run both locally
  before the push; a failure after the merge blocks the staging deploy.
- mdbook stays at the one version CI pins.
- Cloudflare Workers static assets: the whole site stays far below the
  limits (five books of about 75 files and 6 MB each).

## Known trade-offs, accepted

- `/book/` no longer follows `main`. A fix to the docs shows on `/dev/` at
  once and on `/book/` with the next release.
- Old pages get today's theme and mdbook.
- dev.irsync.org shows a release's book from its bump merge on, before it
  is published. irsync.org cannot: the promote refuses such a build.
- Nothing can be promoted between a bump merge and the publish, short of
  the `allow-unpublished` way out.
- `README.md` on `main` may link to a `/book/` page that only the next
  release has.

## Calls made by the writer, not by Mike

Mike agreed to these in the conversation as a list, not one by one:

- The version in the top bar of every page, besides the title page.
- "Edit this page" only in `/dev/`.
- The footer's "Contributor guide" link, and `CONTRIBUTING.md`'s links, go
  to `/dev/`.
- Flags are checked only for `/dev/`.
- `/book/` is a copy, not a redirect.

Not discussed, the writer's own:

- The `VERSIONS` file as the way the promote job learns what a build
  holds, and that it is public like `BUILD`.
- The `allow-unpublished` way out, and that a build without `VERSIONS` is
  refused.
- That a failed re-run in `sign-release.sh` is a message, not an error.
- That the promote job stops when the newest staging run is not a
  finished success.

- The exact sentences on the title page, and the "Released documentation"
  link in `/dev/`.
- The look of the two new links on the version lines.
- `/dev/` is the working tree, so local builds show uncommitted edits.
- With no version at all, `/book/` is the book from `main`.

## Left to the builder

- Everything named as the builder's call above.
- How the tests fake several versions' books without running mdbook five
  times in every test.
- The names of new files and functions.
