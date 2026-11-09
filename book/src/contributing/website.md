# This website

irsync.org has two parts, built from this repository into one directory: a
landing page at `/` and this book at `/book/`.

## Layout

| Path | What it holds |
| --- | --- |
| `site/index.html`, `site/404.html`, `site/style.css`, `site/robots.txt`, `site/_headers` | The landing page, the page served for an unknown URL outside `/book/`, and the files served next to them, copied as they are. |
| `site/tokens.json` | The design tokens: colours, type, spacing, radius, shadow. |
| `site/tools/` | `gen_tokens.py` (tokens to CSS), `strip_font_link.py` (see [Building](#building)), `check.py` (the checker) and their tests. Not deployed. |
| `site/build.sh`, `site/serve.sh`, `site/deploy.sh` | Build, serve locally and deploy. Not deployed. |
| `book/` | The mdbook: `book.toml`, the pages in `book/src/`, theme overrides in `book/theme/`. |

## Updating the design tokens

`site/tokens.json` is a verbatim copy of the design system's
`project/tokens.json`. To take a new version, replace the file with the
design system's file; do not edit it by hand. The build turns it into CSS
custom properties named `--ir-*`, twice: `book/theme/tokens.css` (git-ignored)
keyed on mdbook's theme class, and `tokens.css` next to the landing page
keyed on its `data-theme` attribute. Every colour, font stack, spacing,
radius and shadow on the site comes from these properties.

## Building

The build needs mdbook and Python 3:

```sh
site/build.sh
```

It empties `target/site/`, generates the token CSS, builds the book into
`target/site/book/` and copies the landing page files into `target/site/`.
No font files ship: the site uses the system font stacks from the tokens. The
empty `book/theme/fonts/fonts.css` keeps mdbook from copying its web fonts,
and the build removes that stylesheet and its link from every book page.
`site/build.sh OUT` builds into `OUT` instead; `OUT` must be under `target/`,
because the build deletes it first. The check is on the resolved path, so a
`target/` that is a symlink to somewhere outside the checkout is refused too.

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
  External links are not fetched.
- **No fonts.** No font file and no `fonts/` directory is in the build.
- **Moved files.** Documents that moved into the book (the list is
  `MOVED_FILES` in `check.py`) are not referred to, by path or by bare name,
  from any tracked file once they are gone. `docs/superpowers/` is history
  and is skipped.
- **irsync flags.** With `--irsync`, every `--flag` on a documented `irsync`
  command line (in a code block, or an inline code span starting with
  `irsync `) and in the first column of the tables in
  [Command line](../reference/command-line.md) is one that the given binary
  lists in `--help`, of the top-level command or a subcommand. The book,
  `site/index.html` and `README.md` are checked.

## Serving locally

```sh
site/serve.sh
```

It builds the site, serves `target/site/` at `http://localhost:9999/` and
rebuilds whenever a file under `book/` or `site/` changes; open pages reload
on their own. `site/serve.sh PORT` serves on another port. It needs Node.js:
it runs `chokidar-cli` (the watcher) and `browser-sync` (the server) through
`npx`, at the versions pinned at its top. Ctrl-C stops both.

The local server ignores `_headers`; the headers apply on Cloudflare Pages
only.

## Staging and production

The site is served by two Cloudflare Pages projects, each with production
branch `main`:

| Project | Domain | Role |
| --- | --- | --- |
| `irsync-dev` | [dev.irsync.org](https://dev.irsync.org/) | Staging. Every deploy of `main` lands here. |
| `irsync-pub` | [irsync.org](https://irsync.org/) | Production. Changes only when a staging build is promoted. |

Promotion ships the files staging served: the staging build's output is
kept and uploaded to `irsync-pub` unchanged, nothing is rebuilt. Each build
carries a `BUILD` file at its root holding the commit it was built from, so
`https://irsync.org/BUILD` and `https://dev.irsync.org/BUILD` show what each
one serves.

### Before the repository moves to GitHub

Both deploys run from a developer machine with `site/deploy.sh`. It uses
`wrangler` through `npx` at the version pinned at its top, and takes the
credentials from `CLOUDFLARE_API_TOKEN` and `CLOUDFLARE_ACCOUNT_ID`, or from
`wrangler login`.

```sh
site/deploy.sh staging
site/deploy.sh promote
```

`staging` refuses a working tree with uncommitted or untracked changes. It
builds `target/site/`, writes the `HEAD` commit to `target/site/BUILD`,
builds irsync (`cargo build -p irsync`), runs the tool tests and the checker,
and refuses to deploy when either fails. Then it deploys to `irsync-dev` and
copies the build to `target/site-staging/`.
`promote` deploys `target/site-staging/` to `irsync-pub` and prints its
`BUILD`. It refuses without a `target/site-staging/BUILD`, and when that
`BUILD` differs from `https://dev.irsync.org/BUILD` or cannot be compared
with it: someone else may have deployed staging since, and promotion ships
only what staging serves.

### After the move

`.github/workflows/site.yml` takes over both deploys, and promotion goes
through it rather than `site/deploy.sh promote`, whose local copy may not be
the build staging serves. Its `staging` job runs on every push
to `main` that touches `book/`, `site/`, `README.md`, the workflow, or what
the checker reads beyond them (`crates/`, `proto/`, `docs/`, `.githooks/`,
`tools/`): it runs
the tool tests, builds, runs the checker, uploads `target/site` as the
artifact `site` and deploys it to `irsync-dev`. Its `promote` job runs only
when the workflow is started by hand on `main` (Actions, Site, Run workflow): it
downloads the `site` artifact of the newest successful push run on `main`,
deploys it to `irsync-pub` and writes its `BUILD` to the job summary. The
repository secrets `CLOUDFLARE_API_TOKEN` and `CLOUDFLARE_ACCOUNT_ID` hold
the credentials; only the two deploy steps see them. Artifacts expire after
the repository's retention period (90 days by default); a staging build older than that has to be rebuilt by a
new push before it can be promoted.

After the first deploy to each project, confirm that an unknown URL gets a
404 status and not the landing page:

```sh
curl -I https://dev.irsync.org/nope
```

The first line of the answer should read `HTTP/2 404`. Cloudflare Pages
serves `404.html` with that status; without one it would serve the landing
page with a 200 for every unknown path.

## Indexing

Staging is public but not indexed; production is indexed. Both serve the
same files, so the difference lives in `site/_headers`, which Cloudflare
applies per host: responses from `dev.irsync.org`, `*.pages.dev` and the
per-deployment preview hosts under it carry `X-Robots-Tag: noindex`, and
responses from `irsync.org` do not. `site/tools/test_headers.py` pins these
rules.

There is no staging-only `robots.txt`. Promotion uploads staging's files
unchanged, and identical files cannot carry a different `robots.txt` per
host. A `Disallow: /` would not help either: a crawler that may not fetch a
page never sees its `noindex` header, and can still list the URL. The one
`robots.txt` allows everything.
