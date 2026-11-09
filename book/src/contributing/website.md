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
| `site/wrangler.jsonc` | The settings both Cloudflare Workers are deployed with. Not deployed. |
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
(`crates/`, `proto/`, `docs/`, `.githooks/`, `tools/`). It runs the tool
tests, builds the site, runs the checker, keeps `target/site` as the run
artifact `site`, and deploys it to `irsync-dev`. If a check fails, nothing is
deployed.

The `promote` job runs only when the workflow is started by hand on `main`.
It downloads the `site` artifact of the newest successful push run on
`main`, deploys it to `irsync-pub` and writes its `BUILD` to the job
summary.

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

The job promotes the newest CI build, not whatever dev.irsync.org serves at
that moment. The two differ only if someone deployed staging by hand with
`site/deploy.sh staging` after the last CI build; the `BUILD` comparison
above shows it. Push, or re-run the newest staging run, to bring staging
back to a CI build before promoting.

Artifacts expire after the repository's retention period (90 days by
default). When the newest staging build is older than that, the job fails
with "no successful staging run on main" or cannot download the build; push,
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

`staging` refuses a working tree with uncommitted or untracked changes. It
builds `target/site/`, writes the `HEAD` commit to `target/site/BUILD`,
builds irsync (`cargo build -p irsync`), runs the tool tests and the checker,
and refuses to deploy when either fails. Then it deploys to `irsync-dev` and
copies the build to `target/site-staging/`.

`promote` deploys `target/site-staging/` to `irsync-pub` and prints its
`BUILD`. It refuses without a `target/site-staging/BUILD`, and when that
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

There is no staging-only `robots.txt`. Promotion uploads staging's files
unchanged, and identical files cannot carry a different `robots.txt` per
host. A `Disallow: /` would not help either: a crawler that may not fetch a
page never sees its `noindex` header, and can still list the URL. The one
`robots.txt` allows everything.
