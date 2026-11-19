# Release versions on the homepage

## Intent

The homepage shows which irsync versions are current, one line per release
channel (stable, beta, alpha). It is for repeat users: they look at the
version and the date and decide whether it is worth installing again.

The site updates on its own when a release is built. It lands on
dev.irsync.org automatically. Promoting to irsync.org stays a manual step,
done after the release is published.

## Goals

- Show the newest version of each channel that is not obsolete, with its
  date.
- Include the release that is being built right now, before it is published.
- No hand edits to the site for a release cut from `main`.
- The install command, the "beta" badge and the beta warning follow the
  same data, so they cannot drift from the versions shown.

## Non-goals

- No change list or "What's changed" link on the homepage.
- No hint to run `irsync --version`.
- The book, the README and `site/install.sh` do not change. (`install.sh`
  keeps picking the release itself, from GitHub, when it runs.)
- Automatic refresh for patch releases cut from `release-X.Y` (see
  [Patch releases](#patch-releases)).

## Anti-goals

- irsync.org must never change without the manual promote.
- The page must not look up anything in the browser. What is deployed
  already holds the versions.
- The build must not need the network or a GitHub token to work out the
  versions.

## Which versions are shown

Three channels, by the shape of the version: `X.Y.Z` is stable,
`X.Y.Z-beta.N` is beta, `X.Y.Z-alpha.N` is alpha. Anything else is ignored.

Versions are ordered as in semver: by `X.Y.Z` first; for the same `X.Y.Z`,
alpha is older than beta, beta is older than stable; then by `N`.

1. Take the newest version of each channel (the "current" stable, beta and
   alpha). A channel may have none.
2. The stable is shown if there is one.
3. The beta is shown only if it is newer than the current stable (or there
   is no stable).
4. The alpha is shown only if it is newer than the current stable and
   newer than the current beta, whichever exist. This compares against the
   current beta even when that beta is itself hidden.

The **main version** is the first shown one in the order stable, beta,
alpha. It is the one the install command installs.

Examples:

| Current stable / beta / alpha | Shown | Main |
| --- | --- | --- |
| none / 0.1.0-beta.5 / 0.1.0-alpha.4 (today) | beta | beta |
| 0.1.0 / 0.1.0-beta.7 / 0.1.0-alpha.4 | stable | stable |
| 0.1.0 / 0.2.0-beta.1 / 0.2.0-alpha.3 | stable, beta | stable |
| 0.1.2 / 0.2.0-beta.1 / 0.3.0-alpha.1 | stable, beta, alpha | stable |
| 0.1.0 / 0.1.0-beta.7 / 0.2.0-alpha.1 | stable, alpha | stable |
| none / none / 0.1.0-alpha.4 | alpha | alpha |
| none / none / none | nothing | none |

## What the page shows

Decided with a clickable demo. In the "Get started" column, on the line of
the "Install" heading, the shown versions sit at the right, one per line,
right-aligned. The first line shares its baseline with the word "Install";
the others stack below it, above the command strip:

```
(1) Install                          Stable [0.1.2] · 1 Feb 2027
                                       Beta [0.2.0-beta.1] · 15 Jan 2027
                                      Alpha [0.3.0-alpha.1] · 10 Feb 2027
    $ curl ... https://irsync.org/install.sh | sh              [Copy]
```

- Each line is: channel label, version, `·`, date. Order: stable, beta,
  alpha. No word "Latest".
- Date format: `19 Nov 2026` (day, short English month, year).
- The whole line is in Barlow Semi Condensed, at the `text-small` size, in
  the muted colour. The label is semibold; the version and the date are
  regular weight. All three parts sit on one baseline. The version is not
  in the monospace font and not at the smaller `code` size.
- **The version is a button** that copies the install command of its
  channel: the full curl line, ending in `| sh` for stable,
  `| sh -s -- --beta` for beta, `| sh -s -- --alpha` for alpha. It says
  "Copied" for a moment, like the existing Copy buttons. It has a tooltip
  that says what it copies. It looks like a button, not like text: a small
  outlined box around the version (daisyUI `btn btn-xs` with the page's
  rule colour as border, as the demo had it), with the version in the
  line's font and size. "Copied" replaces the version inside the button
  for about 1.5 seconds. Without JavaScript or without clipboard access
  the version is plain text; the page is otherwise the same.
- **The install command** in the strip installs the main version: it ends
  in `| sh` when the main version is stable, else in `| sh -s -- --beta`
  or `| sh -s -- --alpha`.
- **The badge** next to "irsync" in the header shows only when there is no
  stable. It reads "beta" or "alpha", the channel of the main version.
- **The yellow note** under the steps ("Beta. The command line and the
  protocol may still change ...") shows only when there is no stable. Its
  label is "Beta." or "Alpha.".
- With nothing released: no version lines, no badge, no note, and the
  command ends in `| sh`. (Cannot happen any more; it only has to not
  break.)

## Approaches

**A. Look up the GitHub release list when the site builds.** The gate and
the resume run would each start a site build, the first one passing the
version being built. Works for every branch, and has the real publish
date. But the build needs the network and a token, `site.yml` needs a new
"build for a release" start next to "promote", and promote has to accept
those builds.

**B. A versions file in the repo**, written by the bump script. Simple and
offline, but a patch release's bump goes to `release-X.Y`, so the file on
`main` needs a second PR, and an abandoned release leaves a wrong file.

**C. Look it up in the browser.** Ruled out: it changes irsync.org without
the manual promote, cannot show a release that is still building, and
needs JavaScript.

**D. Read git (chosen).** Git already carries both signals:

- A `v*` tag exists exactly for a published release: GitHub creates it on
  publish, and the tag ruleset stops anyone moving or deleting one.
- After the bump PR is merged, the version in `Cargo.toml` is ahead of the
  newest tag. That is the release being built.

So the build needs no network, the page is the same before and after
publishing (one build is enough), and for a release from `main` the
trigger is just the bump PR's merge. Chosen for that, accepting that the
date is the release commit's date and not the publish time.

## How it is built

### Where the versions come from

The candidates are:

- every `v*` tag in the checkout whose name is a release version, and
- the `[workspace.package]` version in `Cargo.toml` at `HEAD`, and at the
  tip of every `origin/release-X.Y` branch the checkout knows about.

The rules above run over all candidates together. A `Cargo.toml` version
that equals a tag adds nothing. The two old stray tags (`v0.1.0-alpha.1`,
`v0.1.0-alpha.2`) need no special case: they are ordinary old alphas.

The date of a tagged version is the committer date of the commit the tag
points at (for the old annotated tags too: the commit, not the tag
object). The date of an untagged `Cargo.toml` version is the committer
date of the commit that brought that version onto the branch. The point is
that the date does not move when later commits land, and is the same or
nearly the same once the tag exists. A date is the calendar day recorded
in the commit, in the commit's own time zone, as `git log` shows it
(`%cs`). Today that gives 19 Nov 2026 for `v0.1.0-beta.5`.

### The build step

- A new tool in `site/tools/` works out the shown versions and writes them
  into the built `index.html`. `site/build.sh` runs it. The logic that
  picks versions is a pure function with unit tests that cover the table
  above; reading git is a thin layer around it, tested against a throwaway
  repository.
- `site/index.html` stays a complete page that can be opened as it is. It
  marks the places the build fills in: the version lines, the end of the
  install command, the badge, the note. How it marks them is the
  builder's call.
- If the checkout has no tags (a shallow clone), the build fails with a
  message that says to fetch tags. It does not quietly build a page with
  no versions.
- The copy behaviour of the version buttons goes into the page's existing
  inline script, next to the Copy buttons.
- Tailwind keeps only classes it sees in `site/index.html` and
  `site/404.html`. The classes of the generated lines have to be visible
  to it.
- Barlow Semi Condensed ships in semibold only today. Add the regular
  (400) Latin face: its `@font-face` rule, the file in the build's copy
  list, and the allow-list in `check.py` and its tests. No preload.

### The workflow

- `site.yml`'s staging job checks out with the full history and tags
  (`fetch-depth: 0`), and also runs for a push to `main` that changes
  `Cargo.toml`. The bump PR's merge is such a push, so dev.irsync.org shows
  the new version from then on.
- Nothing changes in the release gate, `sign-release.sh`, the resume run
  or the promote job.
- `site/deploy.sh staging` builds from the local checkout, so it shows
  what the local tags say. Its docs say to `git fetch --tags` first.

### Patch releases

A patch is cut from `release-X.Y`. Its bump lands there, and a push there
cannot deploy the site: the deploy tokens open only on `main`. The build
already reads the `release-X.Y` tips, so the next site build on `main`
picks a patch up, before or after it is published. What is missing is the
automatic trigger. That is left for later, before the first patch release:
the gate would start a site build on `main`. Until then
`releasing.md` says that after a patch release the site refreshes with the
next site build on `main`.

### Docs

- `book/src/contributing/website.md`: the new tool, where the versions
  come from, the new font face, the tags a build needs.
- `book/src/contributing/releasing.md`: the site follows the bump merge;
  promote after publishing; the patch-release gap.

## Constraints

- All of `AGENTS.md`: a branch and a PR, `tools/ci.sh check` before the
  push.
- The site tool tests run with `python3 -m unittest discover -s site/tools
  -p 'test_*.py'`; the staging job runs them before it deploys.
- PR CI does not run the site tool tests; only `site.yml` does, after the
  merge. Run them locally before the push. A failure after the merge
  blocks the staging deploy.
- `site/tools/test_build.py` runs `build.sh` in a copy of the tree with no
  git. The new step comes after the mdbook version check, or that test
  changes with it.
- `site/build.sh` is also run by `site/serve.sh` on every change; the new
  step must be quick.

## Known trade-offs, accepted

- dev.irsync.org shows a new version from the bump merge on, before it is
  published. Promoting before publishing would advertise a version that
  cannot be installed yet. The manual promote is the guard.
- A bump that is abandoned stays on dev until the next bump.
- The date can be a day early when signing happens the day after the bump.

## Calls made by the writer, not by Mike

- With a stable out, the whole yellow note goes, including its sentence on
  Windows, extended attributes and ACLs. The "Known limitations" page still
  says it.
- The label is semibold and the version and date regular, which needs the
  extra font file.
- A build without tags fails instead of showing nothing.
- The date is the commit's own calendar day, not UTC.
- The branch carries two small edits Mike asked for on the side: the
  sentence "In scripts, `irsync sync -av …` is the same command with its
  name spelled out" is removed from `site/index.html` and from
  `README.md`. They go in as their own first commit of this PR. They are
  the only README change.
- The patch-release trigger is deferred; the build reads the release
  branches already.

## Left to the builder

- The name and shape of the new tool, and how `index.html` marks the
  filled-in places.
- How the Cargo.toml version's commit date is found.
- The exact markup and classes, as long as the result matches the sketch
  and the rules above.
- How the tests fake a repository.
