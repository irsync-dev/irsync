# This website

irsync.org has two parts, built from this repository into one directory: a
landing page at `/` and this book. The book is there several times, once
per release channel: see [The books](#the-books).

## Layout

| Path | What it holds |
| --- | --- |
| `site/index.html`, `site/404.html`, `site/robots.txt`, `site/_headers`, `site/install.sh` | The landing page, the page served for an unknown URL outside `/book/`, and the files served next to them, copied as they are. |
| `site/tailwind.css` | The landing page's stylesheet: the Tailwind CSS and daisyUI source that the build compiles into `style.css`. |
| `site/package.json`, `site/package-lock.json` | The npm packages the stylesheet is built with, pinned. Not deployed. |
| `site/tokens.json` | The design tokens of the landing page and the book: colours, font stacks, text sizes, radius. |
| `site/tools/` | `gen_tokens.py` (tokens to CSS), `strip_font_link.py` (see [Building](#building)), `versions.py` (see [The release versions](#the-release-versions)), `label_book.py` (see [The books](#the-books)), `check.py` (the checker) and their tests. Not deployed. |
| `site/build.sh`, `site/serve.sh`, `site/deploy.sh` | Build, serve locally and deploy. Not deployed. |
| `site/wrangler.jsonc` | The settings both Cloudflare Workers are deployed with. Not deployed. |
| `book/` | The mdbook: `book.toml`, the pages in `book/src/`, theme overrides in `book/theme/` (`irsync.css`; `picker.js`, which trims the theme picker; `home-link.js`, which links the title to the landing page). |

## The design tokens

`site/tokens.json` holds the design of both the landing page and the book:
every colour (light and dark), font stack, text size and corner radius.
Edit it here. The build turns it into CSS custom properties named `--ir-*`,
in two git-ignored files:

- `site/tokens.css`, which `site/tailwind.css` imports: light on `:root`,
  dark under the system's dark setting.
- `book/theme/tokens.css`, keyed on the theme class mdbook puts on
  `<html>`: Light and Rust take the light values, Coal, Navy and Ayu the
  dark ones.

A few colours are for the book only (`warn-ink`, `danger-ink` and the
`-soft` tints); their `usage` says so. `site/tools/test_gen_tokens.py`
pins the landing page's colours and checks that text colours are readable
(4.5:1) on the grounds they are used on. Changing a landing page colour
means changing that test too, on purpose.

## The landing page design

The landing page is styled with [Tailwind CSS](https://tailwindcss.com/)
and [daisyUI](https://daisyui.com/), from `site/tailwind.css`. Its look comes
from network cabling: deep navy ink, aqua (the jacket colour of multimode
fibre) for the route through the steps and for every link and button, and
single-mode yellow kept for the beta warning only.

- **Colours** come from `site/tokens.json` (see [The design
  tokens](#the-design-tokens)), light first and swapped for dark by the
  system setting. One daisyUI theme, `irsync`, refers to them, so it needs
  no dark twin. Colours with no daisyUI name are Tailwind colours of their
  own: `muted`, `aqua`, `link`, `signal`, `on-signal` and the `term`
  colours of the command strips. Write `text-muted` or `bg-base-200`, not
  a raw colour.
- **Type** is three families, all shipped as web fonts, Latin only.
  [Barlow](https://github.com/jpt/barlow) (`font-display`), a typeface
  modelled on road signs, for the headline, buttons, the header and the
  footer, in regular (400) and semibold (600); its narrower Barlow Semi
  Condensed (`font-condensed`), in semibold for the labels in running
  text, at the size of the text around them ("No open ports.", "Beta."),
  and for the step headings, and in regular (400) for the release
  versions. [Inter](https://rsms.me/inter/)
  (`font-body`) for running text, and
  [JetBrains Mono](https://www.jetbrains.com/lp/mono/) (`font-mono`) for
  commands. The stacks and the text sizes are in `site/tokens.json`. Text
  sizes are the named steps `text-display`, `text-heading`, `text-lede`,
  `text-body`, `text-small` and `text-label`; inline code is 0.9 of the
  text around it (`code` in the scale, in `em`).
- **The route.** The steps under Get started are stations on one aqua line
  (the `station` utility); keep it for content that really is a sequence.

## The release versions

On the line of the "Install" heading, the landing page shows the current
irsync versions, one per release channel: the channel, the version and its
date. They are for someone who has irsync already and wants to know
whether to install again. Each version is a button that copies the install
command of its channel. The channel's name links to that channel's book
(`stable/`, `beta/`, `alpha/`), and the date to the release on GitHub.

Of each channel only the newest version counts. Then:

- The stable is shown.
- The beta is shown if it is newer than the stable.
- The alpha is shown if it is newer than the stable and the beta, even a
  beta that is itself not shown.

| Newest stable / beta / alpha | Shown |
| --- | --- |
| none / 0.1.0-beta.5 / 0.1.0-alpha.4 | beta |
| 0.1.0 / 0.1.0-beta.7 / 0.1.0-alpha.4 | stable |
| 0.1.0 / 0.2.0-beta.1 / 0.2.0-alpha.3 | stable, beta |
| 0.1.2 / 0.2.0-beta.1 / 0.3.0-alpha.1 | stable, beta, alpha |

The first one shown is the main version. The install command on the page
installs it: the command ends in `| sh` for a stable, and in
`| sh -s -- --beta` or `| sh -s -- --alpha` otherwise. The "beta" badge in
the header and the yellow note under the steps show only while there is no
stable release, and name the main version's channel.

`site/tools/versions.py` works this out when the site is built, from git.
The page looks nothing up in the browser.

- A published release is its `v*` tag. GitHub creates the tag when the
  release is published.
- A release that is being built is the version in `Cargo.toml`, on the
  checked-out commit and on every `origin/release-X.Y` branch, when no tag
  has it yet. So the site shows a release from the merge of its bump on.
- The date is the day of the commit the tag points at, as `git log` shows
  it. For a release that is being built, it is the day its version came
  onto the branch.

`site/index.html` marks the places the build fills in with comment pairs,
`<!-- versions:NAME -->` and `<!-- /versions:NAME -->`: `lines` (the
version lines), `tail` (the end of the install command), `prerelease` (the
badge and the note, dropped once there is a stable), and `channel` and
`Channel` (the channel's name inside those). Opened straight from the
repository, the page shows no versions. The markup of a version line is
in `versions.py`; `site/tailwind.css` reads its classes from there.

The build stops with an error when a marker is missing from the page, or
when the checkout has no release tags. Fetch them with
`git fetch --tags`.

## The books

The site holds the book five times, so that someone who runs a release
reads the documentation of that release:

| Path | Which book |
| --- | --- |
| `/book/` | The main version: the one the install command on the landing page installs |
| `/stable/` | The newest stable release |
| `/beta/` | The newest beta |
| `/alpha/` | The newest alpha |
| `/dev/` | The working tree: `main` on the site, your edits when you build locally |

A channel's book is there whenever the channel has a version, also when the
landing page hides that version as obsolete. A channel with no version has
no directory. `/book/` is a full copy of its channel's book, not a
redirect.

The versions come from git, as for the landing page (`versions.py`). Each
book is built from the pages (`book/src`) of its version's commit: the tag
of a published release, or the branch tip of a release that is being built.
Everything else is today's: `book/book.toml`, the theme, mdbook. So an old
book has the current look.

The build makes a working copy of each book under `target/site-books/` and
never changes `book/` itself. `site/tools/label_book.py` labels the copy:

- The title page says which version it documents, under its heading.
- "Edit this page" is only in `/dev/`. A released page cannot be edited at
  its tag.
- mdbook's `site-url` is the book's own path.

The build stops with an error when a version's commit has no `book/src`, or
when its title page does not start with a heading.

The build also writes `VERSIONS` next to `BUILD`: one line per book, with
its path, version, commit and `published` or `unpublished` (a release that
is being built). The `dev` line has no version (`-`). Its commit is the one
the site was built from, the same as in `BUILD`, and its last field is the
branch that was built, `main` on staging.
`https://dev.irsync.org/VERSIONS` shows what staging holds. The promote reads it (see
[Promoting to irsync.org](#promoting-to-irsyncorg)).

### Linking from the landing page

The landing page is built from `main`, and `/book/` from a release. Link
to `book/…` as before. If the released book does not have the page or the
heading yet, the checker fails and names the link. Then link to `dev/…`
until the release has it, or wait for the release.

The contributor guide is linked at `dev/`, from the landing page's footer
and from `CONTRIBUTING.md`: contributors work on `main`.

## Building

The build needs mdbook, Python 3 and Node.js with npm. It reads the release
versions from git, so it needs a checkout with the release tags. mdbook must be the
version CI builds the site with, so the pages come out the same everywhere;
the build stops on any other. `tools/dev-setup.sh` installs that version:

```sh
site/build.sh
```

It empties `target/site/`, generates the token CSS for both, builds the
books into `target/site/book/`, `target/site/beta/` and so on, copies the landing page files into `target/site/`,
writes the release versions into `index.html`
and compiles `site/tailwind.css` into `target/site/style.css`. Tailwind
keeps only the classes that `site/index.html`, `site/404.html` and the
version lines in `site/tools/versions.py` use. The
Tailwind CLI, daisyUI and the fonts (`@fontsource/barlow`,
`@fontsource/barlow-semi-condensed`, `@fontsource/inter` and
`@fontsource/jetbrains-mono`) come from `site/node_modules/` (git-ignored);
the build runs `npm ci` there first when it is missing or older than
`site/package-lock.json`. To move to newer versions, change them in
`site/package.json` and run `npm install` in `site/`, which updates the lock
file.

The build copies the WOFF2 files that the stylesheets name, ten in all,
and each family's licence (the SIL Open Font License, which must ship
with the fonts) into `target/site/fonts/`. They are the only font files on
the site. The book shares them: `book/theme/irsync.css` loads them from
`../../fonts/`, the site's `fonts/`. A bare `mdbook serve` has no `fonts/`
and falls back to the next faces in each stack. The empty
`book/theme/fonts/fonts.css` keeps mdbook from copying its own web fonts,
and the build removes that stylesheet and its link from every book page.
To use another weight or style, add its `@font-face` rule, the file to
the copy list in `site/build.sh`, and, for the landing page, a preload
link if it shows on the first screen.

`site/build.sh OUT` builds into `OUT` instead; `OUT` must be under `target/`,
because the build deletes it first. The check is on the resolved path, so a
`target/` that is a symlink to somewhere outside the checkout is refused too.

## The book's look

The book takes the landing page's design from the same tokens:
`book/theme/irsync.css` maps mdbook's variables onto them. Running text
is in Inter, headings and the book's controls (sidebar, menu bar, search)
in Barlow, code in JetBrains Mono; bold is semibold (600), the weight
that ships. mdbook sets the root font size to
62.5%, so the token sizes, in `rem`, are multiplied by `--ir-book-rem`
(1.6). Page titles use the landing page's `heading` size.

The theme picker offers Auto (follow the system setting, the default),
Light and Dark; `book/theme/picker.js` removes Rust, Coal and Ayu and
shows Navy as Dark. A reader who chose one of the removed themes earlier
still gets the light or dark look.

The book's title in the menu bar ("irsync Documentation", the same in every
book) links to the landing page at
`/`, as the landing page's own "irsync" links home; mdbook has no setting
for this, so `book/theme/home-link.js` adds the link. A click on the bar
around the title still scrolls to the top. A bare `mdbook serve` has no
landing page, so the link leads nowhere there.

## The checker

```sh
cargo build -p irsync
python3 site/tools/check.py --irsync target/debug/irsync target/site
python3 -m unittest discover -s site/tools -p 'test_*.py'
```

`check.py` prints one line per problem and exits non-zero if there is any.
It enforces four things:

- **Links and anchors.** Every relative `href` and `src` in the built HTML
  resolves to a file, and every `#fragment` to an `id` on the target page.
  External links are not fetched. This covers the landing page and all
  five books.
- **No fonts but the site's own.** No font file and no `fonts/` directory
  is in the build, except Barlow, Inter and JetBrains Mono in the
  top-level `fonts/`.
- **Moved files.** Documents that moved into the book (the list is
  `MOVED_FILES` in `check.py`) are not referred to, by path or by bare name,
  from any tracked file once they are gone. `docs/superpowers/` is history
  and is skipped.
- **irsync flags.** With `--irsync`, every `--flag` on a documented `irsync`
  command line (in a code block, or an inline code span starting with
  `irsync `) and in the first column of the tables in
  [Command line](../reference/command-line.md) is one that the given binary
  lists in `--help`, of the top-level command or a subcommand. A command
  that the help hides, such as `irsync verify-signature`, counts once a
  page uses it: the checker asks for that command's own `--help`. The book
  in the working tree (the source of `/dev/`), `site/index.html` and
  `README.md` are checked. A released book's flags are not: that would
  need that release's irsync.

## Serving locally

```sh
site/serve.sh
```

It builds the site, serves `target/site/` at `http://localhost:9999/` and
rebuilds whenever a file under `book/` or `site/` changes; open pages reload
on their own. `site/serve.sh PORT` serves on another port. It needs Node.js:
it runs `chokidar-cli` (the watcher) and `browser-sync` (the server) through
`npx`, at the versions pinned at its top. Ctrl-C stops both.

The local server ignores `_headers`; the headers apply on Cloudflare only.

## Staging and production

The site is served by two Cloudflare Workers that hold only static files, no
code. Both are deployed with the settings in `site/wrangler.jsonc`:

| Worker | Domain | Role |
| --- | --- | --- |
| `irsync-dev` | [dev.irsync.org](https://dev.irsync.org/) | Staging. Every deploy of `main` lands here. |
| `irsync-pub` | [irsync.org](https://irsync.org/) | Production. Changes only when a staging build is promoted. |

Promotion ships the files staging served: the staging build's output is
kept and uploaded to `irsync-pub` unchanged, nothing is rebuilt. Each build
carries a `BUILD` file at its root holding the commit it was built from, so
`https://irsync.org/BUILD` and `https://dev.irsync.org/BUILD` show what each
one serves.

Workers rather than Cloudflare Pages, because a Cloudflare API token can be
limited to a single Worker, while a Pages token can change every Pages
project in the account. Each Worker gets its own deploy token, so the token
that staging deploys with cannot touch production.

### One-time setup

A token limited to one Worker can deploy new files to it, but cannot create
the Worker or attach its domain. So these steps are done once by someone with
full access to the Cloudflare account (`npx wrangler login`), and
`site/wrangler.jsonc` leaves the domains out, so deploys never try to change
them.

1. Create both Workers by deploying to them once, from a checkout with a
   built site:

   ```sh
   site/build.sh
   npx wrangler deploy --config site/wrangler.jsonc --name irsync-dev --assets target/site
   npx wrangler deploy --config site/wrangler.jsonc --name irsync-pub --assets target/site
   ```

2. Attach the domains: in the dashboard, Workers & Pages, the Worker,
   Settings, Domains & Routes, Add, Custom domain: `dev.irsync.org` on
   `irsync-dev` and `irsync.org` on `irsync-pub`. Cloudflare creates the DNS
   records; `irsync.org` must be a zone in the same account.

3. Create two API tokens, each with the Workers role Editor on one Worker
   only: one for `irsync-dev`, one for `irsync-pub`. Editor can deploy new
   versions of that Worker and nothing else.

4. Store them, with the account ID (`npx wrangler whoami` prints it), as
   repository secrets:

   ```sh
   gh secret set CLOUDFLARE_API_TOKEN_DEV
   gh secret set CLOUDFLARE_API_TOKEN_PUB
   gh secret set CLOUDFLARE_ACCOUNT_ID
   ```

### Deploying from GitHub

This is the normal way. `.github/workflows/site.yml` has two jobs.

The `staging` job runs on every push to `main` that touches `book/`,
`site/`, `README.md`, the workflow, or what the checker reads beyond them
(`crates/`, `proto/`, `docs/`, `.githooks/`, `tools/`). It also runs when
`Cargo.toml` changes: the merge of a version bump is such a push, and
staging then shows the new version. It checks out the full history, for
the release tags the build reads. It runs the tool
tests, builds the site, runs the checker, keeps `target/site` as the run
artifact `site`, and deploys it to `irsync-dev`. If a check fails, nothing is
deployed.

The `promote` job runs only when the workflow is started by hand on `main`.
It takes the newest push run on `main` and stops if that run is still going
or failed: it does not fall back to the run before it. It downloads that
run's `site` artifact, refuses it if it holds a version that is not
published (see [Promoting to irsync.org](#promoting-to-irsyncorg)),
deploys it to `irsync-pub` and writes its `BUILD` to the job summary.

The repository secrets `CLOUDFLARE_API_TOKEN_DEV`, `CLOUDFLARE_API_TOKEN_PUB`
and `CLOUDFLARE_ACCOUNT_ID` hold the credentials; each deploy step sees only
its own Worker's token.

### Promoting to irsync.org

Look over [dev.irsync.org](https://dev.irsync.org/) first, then start the
`promote` job, either in the browser (Actions, Site, Run workflow) or with:

```sh
gh workflow run site.yml --ref main
```

To follow it, wait a few seconds for the run to appear, then:

```sh
gh run watch $(gh run list --workflow site.yml --event workflow_dispatch \
  --limit 1 --json databaseId --jq '.[0].databaseId')
```

When it has finished, check that irsync.org serves the staging build, still
answers an unknown URL with a 404, and is not marked `noindex`:

```sh
curl https://irsync.org/BUILD                    # the same commit as the next line
curl https://dev.irsync.org/BUILD
curl -sI https://irsync.org/nope | head -1       # HTTP/2 404
curl -sI https://irsync.org/ | grep -i x-robots  # prints nothing
```

The last check matters most: a `noindex` on irsync.org would drop the site
from search engines.

**A version that is not published.** Between the merge of a version bump
and the publishing of that release, staging shows a version nobody can
install yet, and its book is read from a branch that may still move. The
promote refuses such a build and names the version:

```
versions.py: this build holds 0.1.0-beta.6, which is not published. Publish the release, re-run the staging build, then promote.
```

`sign-release.sh --publish` re-runs the staging build for you. That run
reads every release from its tag. So the usual order is: publish, wait for
the staging run, look at dev.irsync.org, promote.

In that window nothing can be promoted, not even an unrelated fix. The way
out, for a fix that cannot wait or a bump that was abandoned:

```sh
gh workflow run site.yml --ref main -f allow-unpublished=true
```

A build from before `VERSIONS` existed is refused too. Re-run the newest
staging run and promote again.

The job promotes the newest CI build, not whatever dev.irsync.org serves at
that moment. The two differ only if someone deployed staging by hand with
`site/deploy.sh staging` after the last CI build; the `BUILD` comparison
above shows it. Push, or re-run the newest staging run, to bring staging
back to a CI build before promoting.

Artifacts expire after the repository's retention period (90 days by
default). When the newest staging build is older than that, the job fails
with "no staging run on main" or cannot download the build; push,
or re-run the newest staging run, and promote again.

### Rolling back irsync.org

Every deploy is kept as a version of the Worker. To serve an older one again,
open the dashboard, Workers & Pages, `irsync-pub`, Deployments, pick the
version and roll back to it; or, with the `irsync-pub` token or a full-access
login:

```sh
npx wrangler deployments list --name irsync-pub
npx wrangler rollback <version-id> --name irsync-pub
```

A rollback takes effect at once and does not touch staging. The next
promotion replaces it.

### Deploying from a developer machine

For when GitHub Actions is not available. `site/deploy.sh` runs the same
checks and deploys. It uses `wrangler` through `npx` at the version pinned
at its top, and takes the credentials from `CLOUDFLARE_API_TOKEN` and
`CLOUDFLARE_ACCOUNT_ID`, or from `wrangler login`. A token limited to one
Worker deploys only to that Worker: `staging` needs the `irsync-dev` token
and `promote` the `irsync-pub` one.

```sh
site/deploy.sh staging
site/deploy.sh promote
```

Run `git fetch --tags` first: the page shows the versions your checkout
knows.

`staging` refuses a working tree with uncommitted or untracked changes. It
builds `target/site/`, writes the `HEAD` commit to `target/site/BUILD`,
builds irsync (`cargo build -p irsync`), runs the tool tests and the checker,
and refuses to deploy when either fails. Then it deploys to `irsync-dev` and
copies the build to `target/site-staging/`.

`promote` deploys `target/site-staging/` to `irsync-pub` and prints its
`BUILD`. Like the job, it refuses a build that holds a version which is not
published; `site/deploy.sh promote --allow-unpublished` skips that. It
refuses without a `target/site-staging/BUILD`, and when that
`BUILD` differs from `https://dev.irsync.org/BUILD` or cannot be compared
with it: someone else may have deployed staging since, and promotion ships
only what staging serves. Run the checks under
[Promoting to irsync.org](#promoting-to-irsyncorg) afterwards.

### Checking a new Worker

After the first deploy to each Worker, confirm that an unknown URL gets a
404 status and not the landing page:

```sh
curl -I https://dev.irsync.org/nope
curl -I https://irsync.org/nope
```

The first line of each answer should read `HTTP/2 404`. With
`not_found_handling` set to `404-page` in `site/wrangler.jsonc`, Cloudflare
serves `404.html` with that status for any path that is not a file.

## Indexing

Staging is public but not indexed; production is indexed. Both serve the
same files, so the difference lives in `site/_headers`, which Cloudflare
applies per host: responses from `dev.irsync.org` carry
`X-Robots-Tag: noindex`, and responses from `irsync.org` do not.
`site/wrangler.jsonc` turns off the Workers' `workers.dev` hosts and preview
hosts; `_headers` marks those `noindex` too, in case one is turned on.
`site/tools/test_headers.py` pins these rules.

Of the five books only `/book/` is indexed. `/stable/`, `/beta/`,
`/alpha/` and `/dev/` carry `X-Robots-Tag: noindex` on every host: each is
a copy of `/book/`, an older channel or unreleased, and must not outrank
it in a search.

There is no staging-only `robots.txt`. Promotion uploads staging's files
unchanged, and identical files cannot carry a different `robots.txt` per
host. A `Disallow: /` would not help either: a crawler that may not fetch a
page never sees its `noindex` header, and can still list the URL. The one
`robots.txt` allows everything.
