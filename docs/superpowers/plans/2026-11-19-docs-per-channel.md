# Documentation per Release Channel Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** The site publishes the book five times (`/book/`, `/stable/`, `/beta/`, `/alpha/`, `/dev/`), each labelled with its version, and irsync.org can never show a version that is not published.

**Architecture:** `site/tools/versions.py` learns which commit each version's pages come from and lists the books to build. `site/build.sh` builds each book from that commit's `book/src` with today's theme and mdbook, after a new tool `site/tools/book.py` has set the title, the address and the version line in a working copy. The build records what it built in a `VERSIONS` file; the promote job and `site/deploy.sh promote` refuse a build with an unpublished version; `sign-release.sh --publish` re-runs the staging build once the tag exists.

**Tech Stack:** Python 3 standard library and `unittest`, git, POSIX sh, mdbook 0.5.2, Tailwind CSS 4, GitHub Actions, bash.

**Spec:** `docs/superpowers/specs/2026-11-19-docs-per-channel-design.md`. Read it first; the plan argues from it. It builds on `docs/superpowers/specs/2026-11-19-site-release-versions-design.md`.

## Global Constraints

- Work on branch `docs-per-channel` (it exists, with the spec). One PR against `main` with `Closes #105`. No attribution trailers in commits or the PR.
- Commit messages: a short summary line, a blank line, then a brief plain-English reason if it isn't obvious.
- No network and no GitHub to find or fetch a version's pages. Pages come from git: committed content only for a version, the working tree only for `/dev/`.
- Old pages are built with today's `book/book.toml`, `book/theme/` and mdbook. Nothing is taken from a tag but `book/src`.
- The repository's files and the working tree are never edited by the build: the version line and the settings go into copies under `target/`.
- Paths and their order everywhere: `book`, `stable`, `beta`, `alpha`, `dev`.
- Titles: `irsync VERSION`; in `/dev/`: `irsync unreleased`.
- Title page sentences, exact: `This is the documentation of irsync VERSION.` and `This is the documentation of the unreleased development version of irsync.`
- `X-Robots-Tag: noindex` on `/stable/*`, `/beta/*`, `/alpha/*`, `/dev/*` on every host; never on `/book/*` or `/` on irsync.org.
- The release gate, the resume run and `site.yml`'s triggers do not change.
- PR CI does not run the site tool tests or `check.py`. Before every commit that touches `site/`: `python3 -m unittest discover -s site/tools -p 'test_*.py'`. Before the push also `site/build.sh` and `check.py`.
- `tools/ci.sh workflows` needs PyYAML. If the system `python3` has none, make a throwaway venv with `pyyaml` and put it first on `PATH`; do not install into the system Python.
- Docs follow the book's plain style: short sentences, no jargon.

## Review Focus

1. A version whose commit has no `book/src`: the build stops and names the version; it does not publish an empty or wrong book. (Task 3)
2. An old `introduction.md` that does not start with a `# ` heading: `book.py` stops and says so; it does not drop the version line silently. (Task 2)
3. A release being built on `origin/release-X.Y`, not on `HEAD`: its pages come from that branch's tip, and it is `unpublished`. (Task 1)
4. `VERSIONS` missing, empty or with a malformed line when promoting: refused, with a message; never treated as "all published". (Task 5)
5. The newest staging run still going when `sign-release.sh` tries to re-run it, or when someone promotes: the script says what to do and still exits 0; the promote stops instead of shipping the run before it. (Tasks 5, 6)
6. A version text in a title or a page: it only ever comes from `parse`-valid versions, so it cannot carry TOML or Markdown syntax; `book.py` rejects anything else. (Task 2)

## File Structure

| File | Change | Responsibility |
| --- | --- | --- |
| `site/tools/versions.py` | modify | Where each version's pages come from; the list of books; the `VERSIONS` check; the linked version lines |
| `site/tools/book.py` | create | Prepare one book's working copy: settings and version line |
| `site/tools/test_versions.py`, `test_book.py` | modify, create | Their tests |
| `site/build.sh` | modify | Build every book, write `VERSIONS` |
| `site/_headers`, `site/tools/test_headers.py` | modify | noindex for the four unindexed books |
| `site/index.html`, `CONTRIBUTING.md` | modify | Contributor guide links go to `/dev/` |
| `.github/workflows/site.yml`, `site/deploy.sh` | modify | Promote refuses an unpublished build |
| `.github/scripts/sign-release.sh`, `tests/fake_gh.py`, `tests/test_release_scripts.py`, `tests/test_workflow_policy.py` | modify | Re-run staging after publishing; policy test for the promote job |
| `book/src/contributing/website.md`, `releasing.md`, `AGENTS.md` | modify | Docs |

---

### Task 1: Which commit each version comes from, and the list of books

**Files:**
- Modify: `site/tools/versions.py`, `site/build.sh` (the one call)
- Test: `site/tools/test_versions.py`

**Interfaces:**
- Consumes: `parse`, `channel`, `pick`, `CHANNELS`, `VersionsError`, `render`, and the test helper `RepoCase` (all exist).
- Produces:
  - `Release = namedtuple("Release", "date commit published")`: `commit` a full 40-character hash, `published` a bool.
  - `releases(repo: Path) -> dict[str, Release]`: what `from_git` collects today, plus where it came from. A tagged version: the commit the tag points at, `published=True`. An untagged `Cargo.toml` version: the tip commit of the ref it was read from (`HEAD` or `refs/remotes/origin/release-X.Y`), `published=False`. Raises as `from_git` does.
  - `from_git(repo)` stays, as `{v: r.date for v, r in releases(repo).items()}`.
  - `newest(versions: Iterable[str]) -> dict[str, str]`: channel to its newest version, no hiding. `pick` uses it.
  - `Book = namedtuple("Book", "path version commit state")`; `state` is `"published"`, `"unpublished"` or `"dev"`. For a `dev` book `version` and `commit` are `None`.
  - `books(found: dict[str, Release]) -> list[Book]`: in the order `book`, `stable`, `beta`, `alpha`, `dev`. `book` repeats the main version's entry (`pick(...)[0]`); with no version at all it is `Book("book", None, None, "dev")`. A channel with no version has no entry. `dev` is always last: `Book("dev", None, None, "dev")`.
  - `books_text(books) -> str`: one line per book, `PATH VERSION COMMIT STATE\n`, with `-` for a `None`. This is the `VERSIONS` file.
  - CLI, replacing the positional form, with `argparse` subcommands:
    - `versions.py render REPO IN_HTML OUT_HTML` (what the tool does today)
    - `versions.py books REPO` prints `books_text`
    - (Task 5 adds `promotable`.)
    Exit 1 with `versions.py: MESSAGE` on stderr for a `VersionsError`, as today.

- [ ] **Step 1: Write the failing tests**

```python
class ReleasesTests(RepoCase):
    def test_tagged_and_being_built(self):
        self.commit("0.1.0-beta.5", "2026-11-19T12:00:00+00:00"); self.git("tag", "v0.1.0-beta.5")
        tagged = self.git("rev-parse", "HEAD")
        self.commit("0.1.0-beta.6", "2026-11-25T12:00:00+00:00")
        self.commit("0.1.0-beta.6", "2026-11-27T12:00:00+00:00")
        tip = self.git("rev-parse", "HEAD")
        got = versions.releases(self.repo)
        self.assertEqual(got["0.1.0-beta.5"], versions.Release("2026-11-19", tagged, True))
        # The pages come from the tip; the date stays the bump's.
        self.assertEqual(got["0.1.0-beta.6"], versions.Release("2026-11-25", tip, False))
        self.assertEqual(versions.from_git(self.repo),
                         {"0.1.0-beta.5": "2026-11-19", "0.1.0-beta.6": "2026-11-25"})

    def test_annotated_tag_gives_the_commit_not_the_tag_object(self):
        self.commit("0.1.0-beta.1", "2026-11-14T12:00:00+00:00")
        self.git("tag", "-a", "-m", "x", "v0.1.0-beta.1")
        self.assertEqual(versions.releases(self.repo)["0.1.0-beta.1"].commit, self.git("rev-parse", "HEAD"))

    def test_patch_being_built_on_a_release_branch(self):
        self.commit("1.2.0", "2027-03-03T12:00:00+00:00"); self.git("tag", "v1.2.0")
        base = self.git("rev-parse", "HEAD")
        self.commit("1.2.1", "2027-03-10T12:00:00+00:00")
        branch_tip = self.git("rev-parse", "HEAD")
        self.git("update-ref", "refs/remotes/origin/release-1.2", "HEAD")
        self.git("reset", "-q", "--hard", base)
        self.assertEqual(versions.releases(self.repo)["1.2.1"],
                         versions.Release("2027-03-10", branch_tip, False))


R = versions.Release

class BooksTests(unittest.TestCase):
    def test_today(self):
        found = {"0.1.0-beta.5": R("2026-11-19", "b" * 40, True),
                 "0.1.0-alpha.4": R("2026-11-13", "a" * 40, True)}
        self.assertEqual(versions.books(found), [
            versions.Book("book", "0.1.0-beta.5", "b" * 40, "published"),
            versions.Book("beta", "0.1.0-beta.5", "b" * 40, "published"),
            versions.Book("alpha", "0.1.0-alpha.4", "a" * 40, "published"),   # obsolete, still a book
            versions.Book("dev", None, None, "dev")])

    def test_stable_is_the_main_book_and_old_channels_stay(self):
        found = {"1.0.0": R("2027-03-03", "c" * 40, True),
                 "0.9.0-beta.2": R("2027-01-01", "b" * 40, True),
                 "1.1.0-alpha.1": R("2027-04-01", "a" * 40, False)}
        got = versions.books(found)
        self.assertEqual([b.path for b in got], ["book", "stable", "beta", "alpha", "dev"])
        self.assertEqual(got[0].version, "1.0.0")
        self.assertEqual(got[2].version, "0.9.0-beta.2")
        self.assertEqual(got[3].state, "unpublished")

    def test_no_version_at_all(self):
        self.assertEqual(versions.books({}), [versions.Book("book", None, None, "dev"),
                                              versions.Book("dev", None, None, "dev")])

    def test_text(self):
        found = {"0.1.0-beta.5": R("2026-11-19", "b" * 40, True)}
        self.assertEqual(versions.books_text(versions.books(found)),
                         f"book 0.1.0-beta.5 {'b' * 40} published\n"
                         f"beta 0.1.0-beta.5 {'b' * 40} published\n"
                         "dev - - dev\n")

class NewestTests(unittest.TestCase):
    def test_per_channel_without_hiding(self):
        self.assertEqual(versions.newest(["1.0.0", "0.9.0-beta.2", "0.9.0-beta.10", "0.1.0-alpha.4", "junk"]),
                         {"stable": "1.0.0", "beta": "0.9.0-beta.10", "alpha": "0.1.0-alpha.4"})
```

Change `CliTests.run_tool` to call `versions.py render REPO IN OUT`, and add:

```python
    def test_books_prints_the_list(self):
        self.commit("0.1.0-beta.5", "2026-11-19T12:00:00+00:00"); self.git("tag", "v0.1.0-beta.5")
        sha = self.git("rev-parse", "HEAD")
        r = subprocess.run([sys.executable, str(TOOL), "books", str(self.repo)], capture_output=True, text=True)
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(r.stdout, f"book 0.1.0-beta.5 {sha} published\nbeta 0.1.0-beta.5 {sha} published\ndev - - dev\n")
```

- [ ] **Step 2: Run them and see them fail**

Run: `python3 -m unittest discover -s site/tools -p 'test_versions.py'`
Expected: the new tests fail (`releases`, `books`, `newest` missing; the CLI rejects `render`).

- [ ] **Step 3: Implement**

Refactor `from_git` into `releases`. The tag listing gains the commit: add `%(*objectname)%(objectname)` to the `for-each-ref` format; that pair gives the commit for a lightweight tag but the tag object's own hash first for an annotated one, so resolve annotated tags with the starred field when it is not empty. An untagged version's commit is `git rev-parse REF^{commit}`.

- [ ] **Step 4: Update the build's call**

In `site/build.sh`: `python3 "$root/site/tools/versions.py" render "$root" "$root/site/index.html" "$out/index.html"`.

- [ ] **Step 5: Run the tests and a build**

Run: `python3 -m unittest discover -s site/tools -p 'test_*.py'`
Expected: OK.

Run: `python3 site/tools/versions.py books .`
Expected: four lines: `book` and `beta` with `0.1.0-beta.5`, `alpha` with `0.1.0-alpha.4`, all `published`, then `dev - - dev`.

Run: `site/build.sh && grep -c 'class="version"' target/site/index.html`
Expected: `1`.

- [ ] **Step 6: Commit**

```bash
git add site/tools/versions.py site/tools/test_versions.py site/build.sh
git commit -m "Work out which commit each version's documentation comes from"
```

---

### Task 2: Prepare one book's working copy

**Files:**
- Create: `site/tools/book.py`
- Test: `site/tools/test_book.py`

**Interfaces:**
- Consumes: `versions.parse` (import it; both files are in `site/tools/`).
- Produces:
  - `class BookError(Exception)`.
  - `prepare(work: Path, path: str, version: str | None) -> None`. `work` holds `book.toml` and `src/`. `version` is `None` for an unreleased book.
  - CLI: `python3 site/tools/book.py WORK PATH VERSION`, with `-` for no version. Exit 1 with `book.py: MESSAGE` on stderr for a `BookError`.

What `prepare` does:

1. Rejects a `path` not in `book`, `stable`, `beta`, `alpha`, `dev`, and a `version` that `versions.parse` rejects: `BookError`.
2. In `work/book.toml`, by whole-line replacement; each line must be found exactly once, else `BookError` naming the setting:
   - `title = "irsync"` becomes `title = "irsync VERSION"`, or `title = "irsync unreleased"`.
   - `site-url = "/book/"` becomes `site-url = "/PATH/"`.
   - the `edit-url-template = …` line is removed when `version` is not `None`, and kept otherwise.
3. In `work/src/introduction.md`: the first line must start with `# `, else `BookError("PATH: introduction.md does not start with a heading")`. After it, insert a blank line and the sentence:
   - released: `This is the documentation of irsync VERSION.`
   - unreleased, `path == "dev"`: `This is the documentation of the unreleased development version of irsync. [Released documentation](../book/).`
   - unreleased, any other path: the same sentence without the link.
   A missing `introduction.md` is a `BookError` that names the path.

- [ ] **Step 1: Write the failing tests**

```python
import shutil, subprocess, sys, tempfile, unittest
from pathlib import Path
import book

ROOT = Path(__file__).resolve().parent.parent.parent
TOOL = ROOT / "site" / "tools" / "book.py"

class PrepareTests(unittest.TestCase):
    def setUp(self):
        self.work = Path(tempfile.mkdtemp(prefix="site-book-"))
        self.addCleanup(shutil.rmtree, self.work, ignore_errors=True)
        shutil.copy(ROOT / "book" / "book.toml", self.work / "book.toml")   # the real settings
        (self.work / "src").mkdir()
        (self.work / "src" / "introduction.md").write_text("# Introduction\n\n`irsync` is a synchroniser.\n")

    def toml(self): return (self.work / "book.toml").read_text()
    def intro(self): return (self.work / "src" / "introduction.md").read_text()

    def test_released(self):
        book.prepare(self.work, "beta", "0.1.0-beta.5")
        self.assertIn('title = "irsync 0.1.0-beta.5"\n', self.toml())
        self.assertIn('site-url = "/beta/"\n', self.toml())
        self.assertNotIn("edit-url-template", self.toml())
        self.assertEqual(self.intro(), "# Introduction\n\nThis is the documentation of irsync 0.1.0-beta.5.\n\n"
                                       "`irsync` is a synchroniser.\n")

    def test_dev(self):
        book.prepare(self.work, "dev", None)
        self.assertIn('title = "irsync unreleased"\n', self.toml())
        self.assertIn('site-url = "/dev/"\n', self.toml())
        self.assertIn("edit-url-template", self.toml())
        self.assertIn("\n\nThis is the documentation of the unreleased development version of irsync. "
                      "[Released documentation](../book/).\n\n", self.intro())

    def test_unreleased_main_book_has_no_link_to_itself(self):
        book.prepare(self.work, "book", None)
        self.assertIn('site-url = "/book/"\n', self.toml())
        self.assertIn("development version of irsync.\n", self.intro())
        self.assertNotIn("Released documentation", self.intro())

    def test_rest_of_the_settings_is_untouched(self):
        before = self.toml()
        book.prepare(self.work, "dev", None)
        changed = [l for l in before.splitlines() if l not in self.toml().splitlines()]
        self.assertEqual(changed, ['title = "irsync"', 'site-url = "/book/"'])

    def test_refuses(self):
        cases = [("docs", "1.0.0", "path"), ("beta", "1.0.0; rm", "version"), ("beta", "v1.0.0", "version")]
        for path, version, word in cases:
            with self.assertRaisesRegex(book.BookError, word):
                book.prepare(self.work, path, version)

    def test_introduction_without_a_heading(self):
        (self.work / "src" / "introduction.md").write_text("irsync is…\n")
        with self.assertRaisesRegex(book.BookError, "beta: introduction.md does not start with a heading"):
            book.prepare(self.work, "beta", "0.1.0-beta.5")

    def test_no_introduction(self):
        (self.work / "src" / "introduction.md").unlink()
        with self.assertRaisesRegex(book.BookError, "beta"):
            book.prepare(self.work, "beta", "0.1.0-beta.5")

    def test_setting_missing_from_book_toml(self):
        (self.work / "book.toml").write_text('[book]\ntitle = "other"\n')
        with self.assertRaisesRegex(book.BookError, "title"):
            book.prepare(self.work, "beta", "0.1.0-beta.5")

    def test_cli(self):
        r = subprocess.run([sys.executable, str(TOOL), str(self.work), "alpha", "0.1.0-alpha.4"],
                           capture_output=True, text=True)
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertIn('title = "irsync 0.1.0-alpha.4"', self.toml())
        r = subprocess.run([sys.executable, str(TOOL), str(self.work), "nope", "-"], capture_output=True, text=True)
        self.assertEqual(r.returncode, 1)
        self.assertIn("book.py: ", r.stderr)
```

- [ ] **Step 2: Run them and see them fail**

Run: `python3 -m unittest discover -s site/tools -p 'test_book.py'`
Expected: an import error for `book`.

- [ ] **Step 3: Implement `site/tools/book.py`**

Read and write with `encoding="utf-8"`. Start the file with a docstring that says what a working copy is and why the build never edits `book/` itself.

- [ ] **Step 4: Run the tests**

Run: `python3 -m unittest discover -s site/tools -p 'test_*.py'`
Expected: OK.

- [ ] **Step 5: Commit**

```bash
git add site/tools/book.py site/tools/test_book.py
git commit -m "Add the tool that labels a book with its version"
```

---

### Task 3: Build the five books

**Files:**
- Modify: `site/build.sh`, `site/_headers`, `site/tools/test_headers.py`, `site/index.html`, `CONTRIBUTING.md`

**Interfaces:**
- Consumes: `versions.py books REPO` (Task 1), `book.py WORK PATH VERSION` (Task 2).
- Produces: `target/site/{book,stable,beta,alpha,dev}/` as the versions allow, and `target/site/VERSIONS` (the output of `versions.py books`). Task 4's links and Task 5's check rely on both.

- [ ] **Step 1: Write the failing header test**

Read `site/tools/test_headers.py` first and use its helper that resolves the headers of a URL. Replace `test_noindex_only_for_staging_hosts` by tests that say:

- on `https://irsync.org`: `/`, `/book/`, `/book/introduction.html`, `/install.sh` have no `X-Robots-Tag`;
- on `https://irsync.org`: `/stable/x.html`, `/beta/`, `/alpha/print.html`, `/dev/contributing/setup.html` have `X-Robots-Tag: noindex`;
- on `https://dev.irsync.org` and on a `workers.dev` host: every path is `noindex`, as now.

Run: `python3 -m unittest discover -s site/tools -p 'test_headers.py'`
Expected: the irsync.org `/beta/` case fails.

- [ ] **Step 2: Add the rules to `site/_headers`**

Four path rules, `/stable/*`, `/beta/*`, `/alpha/*`, `/dev/*`, each with `X-Robots-Tag: noindex`, and a comment: only `/book/` is indexed; the others are a copy, an old channel or unreleased.

Run the header tests. Expected: OK.

- [ ] **Step 3: Build every book in `site/build.sh`**

Replace the single `mdbook build "$root/book" -d "$out/book"` block (with its `rm -rf "$out/book/fonts"` and `strip_font_link.py` lines) by a loop over the lines of `versions.py books "$root"`. Write that output to `"$out/VERSIONS"` first; if the tool fails the build stops (`set -e`; do not pipe the tool straight into the loop, a failure there would be lost). For each line `PATH VERSION COMMIT STATE`:

1. `work="$root/target/site-books/$PATH"`; empty it; copy `book/book.toml` and `book/theme` into it (after the tokens step, so `theme/tokens.css` is there).
2. The pages: for `STATE` `dev`, copy the working tree's `book/src`. Otherwise `git -C "$root" archive "$COMMIT" book/src | tar -x -C "$work.pages"` and move its `book/src` to `$work/src`; if the commit has no `book/src`, stop with `no book/src in $VERSION ($COMMIT)`.
3. `python3 "$root/site/tools/book.py" "$work" "$PATH" "$VERSION"`.
4. `mdbook build "$work" -d "$out/$PATH"`, then `rm -rf "$out/$PATH/fonts"` and `strip_font_link.py "$out/$PATH"`.

Update the comment at the top of the file (the landing page at `/`, the books at `/book/`, `/stable/` …). `target/site-books/` is under `target/`, which the script already requires for its output; it is not under `book/` or `site/`, so `site/serve.sh` does not watch it.

- [ ] **Step 4: Move the contributor guide links to `/dev/`**

- `site/index.html`, footer: `book/contributing/setup.html` becomes `dev/contributing/setup.html`.
- `CONTRIBUTING.md`: its three `https://irsync.org/book/contributing/…` links become `https://irsync.org/dev/contributing/…`.

- [ ] **Step 5: Build, look, check**

Run: `site/build.sh && cat target/site/VERSIONS && ls target/site`
Expected: `VERSIONS` has the four lines of Task 1; the directories `book`, `beta`, `alpha`, `dev` exist and `stable` does not.

Run: `grep -o '<h1 class="menu-title">[^<]*' target/site/{book,beta,alpha,dev}/index.html`
Expected: `irsync 0.1.0-beta.5` twice, `irsync 0.1.0-alpha.4`, `irsync unreleased`.

Run: `grep -c 'This is the documentation of irsync 0.1.0-alpha.4' target/site/alpha/introduction.html; grep -c 'edit/main' target/site/dev/introduction.html target/site/book/introduction.html`
Expected: `1`; then `dev` at least 1 and `book` 0.

Run: `git status --short`
Expected: nothing under `book/` (the build did not touch the repository's pages).

Run: `cargo build -p irsync && python3 site/tools/check.py --irsync target/debug/irsync target/site`
Expected: no output. If it reports a link in `alpha/` that the alpha.4 pages really lack, that is a finding to report, not something to patch in the old pages.

Run: `python3 -m unittest discover -s site/tools -p 'test_*.py'`
Expected: OK.

- [ ] **Step 6: Prove the missing-link stop**

Temporarily change one homepage link in `site/index.html` to `book/does-not-exist.html`, run `site/build.sh` and `check.py`.
Expected: `check.py` prints a line naming `does-not-exist.html` and exits 1. Undo the change (`git checkout site/index.html` is wrong here, it would drop Step 4; edit the link back) and rebuild.

- [ ] **Step 7: Commit**

```bash
git add site/build.sh site/_headers site/tools/test_headers.py site/index.html CONTRIBUTING.md
git commit -m "Publish the book per release channel" \
  -m "/book/ is the main version, /stable/, /beta/ and /alpha/ the newest of each channel, /dev/ is main. Each is built from its version's pages with today's theme. Only /book/ is indexed."
```

---

### Task 4: Link the version lines to their book and release

**Files:**
- Modify: `site/tools/versions.py` (the `_LINE` constant and `render`)
- Test: `site/tools/test_versions.py`

One version line becomes:

```html
<span class="inline-flex items-baseline gap-x-1.5 font-condensed font-normal whitespace-nowrap"><strong class="font-semibold"><a class="text-muted" href="beta/">Beta</a></strong> <span class="version" data-tail="sh -s -- --beta">0.1.0-beta.5</span> <span>· <a class="text-muted" href="https://github.com/irsync-dev/irsync/releases/tag/v0.1.0-beta.5">19 Nov 2026</a></span></span>
```

- [ ] **Step 1: Change the tests first**

In `RenderTests`, replace the assertions on the old line markup (`test_beta_only`, `test_stable_with_newer_beta_and_alpha`) by the new markup, and add:

```python
    def test_links(self):
        out = self.page("0.1.2", "0.2.0-beta.1")
        self.assertIn('<a class="text-muted" href="stable/">Stable</a>', out)
        self.assertIn('<a class="text-muted" href="beta/">Beta</a>', out)
        self.assertIn('href="https://github.com/irsync-dev/irsync/releases/tag/v0.1.2">19 Nov 2026</a>', out)
        self.assertIn('href="https://github.com/irsync-dev/irsync/releases/tag/v0.2.0-beta.1">', out)
```

Run: `python3 -m unittest discover -s site/tools -p 'test_versions.py'`
Expected: these fail on the old markup.

- [ ] **Step 2: Change `_LINE` and its `format` call**

- [ ] **Step 3: Run the tests, build, check**

Run: `python3 -m unittest discover -s site/tools -p 'test_*.py' && site/build.sh && python3 site/tools/check.py --irsync target/debug/irsync target/site`
Expected: OK, then no output (the link `beta/` resolves to the built book).

Run: `grep -c 'text-muted' target/site/style.css`
Expected: at least 1.

- [ ] **Step 4: Look at it**

Render the built page in headless Chrome as in the last change (serve `target/site` on localhost, `--screenshot`), and look: the label and the date are muted, underlined links on the same baseline as the version button. If you cannot, say so in the hand-back.

- [ ] **Step 5: Commit**

```bash
git add site/tools/versions.py site/tools/test_versions.py
git commit -m "Link each version line to its documentation and its release page"
```

---

### Task 5: Promote refuses an unpublished version

**Files:**
- Modify: `site/tools/versions.py`, `.github/workflows/site.yml`, `site/deploy.sh`
- Test: `site/tools/test_versions.py`, `.github/scripts/tests/test_workflow_policy.py`

**Interfaces:**
- Consumes: the `VERSIONS` format of Task 1.
- Produces:
  - `unpublished(text: str) -> list[str]`: the versions of the lines whose state is `unpublished`, without repeats, in file order. Raises `VersionsError` for an empty text, a line that does not have four fields, a path or state it does not know, or a text with no `dev` line.
  - CLI: `versions.py promotable FILE`. Exit 0 and no output when nothing is unpublished. Exit 1 with a message on stderr otherwise:
    - unpublished: `versions.py: this build holds 0.1.0-beta.6, which is not published. Publish the release, re-run the staging build, then promote.`
    - missing file: `versions.py: FILE is missing: this build was made before VERSIONS existed. Re-run the staging build.`
    - malformed: `versions.py: FILE: MESSAGE`

- [ ] **Step 1: Write the failing tests**

```python
SHA = "b" * 40

class PromotableTests(unittest.TestCase):
    def test_all_published(self):
        self.assertEqual(versions.unpublished(f"book 0.1.0-beta.5 {SHA} published\nbeta 0.1.0-beta.5 {SHA} published\ndev - - dev\n"), [])

    def test_names_the_unpublished_once(self):
        text = (f"book 0.1.0-beta.6 {SHA} unpublished\nbeta 0.1.0-beta.6 {SHA} unpublished\n"
                f"alpha 0.1.0-alpha.4 {SHA} published\ndev - - dev\n")
        self.assertEqual(versions.unpublished(text), ["0.1.0-beta.6"])

    def test_refuses_what_it_cannot_read(self):
        for text in ["", "\n", "book 1.0.0 published\ndev - - dev\n", f"book 1.0.0 {SHA} maybe\ndev - - dev\n",
                     f"docs 1.0.0 {SHA} published\ndev - - dev\n", f"book 1.0.0 {SHA} published\n"]:
            with self.assertRaises(versions.VersionsError, msg=repr(text)):
                versions.unpublished(text)

    def run_cli(self, text):
        d = Path(tempfile.mkdtemp(prefix="site-promotable-")); self.addCleanup(shutil.rmtree, d, ignore_errors=True)
        f = d / "VERSIONS"
        if text is not None:
            f.write_text(text)
        return subprocess.run([sys.executable, str(TOOL), "promotable", str(f)], capture_output=True, text=True)

    def test_cli(self):
        r = self.run_cli(f"book 0.1.0-beta.5 {SHA} published\ndev - - dev\n")
        self.assertEqual((r.returncode, r.stdout, r.stderr), (0, "", ""))
        r = self.run_cli(f"book 0.1.0-beta.6 {SHA} unpublished\ndev - - dev\n")
        self.assertEqual(r.returncode, 1)
        self.assertIn("this build holds 0.1.0-beta.6, which is not published", r.stderr)
        r = self.run_cli(None)
        self.assertEqual(r.returncode, 1)
        self.assertIn("is missing", r.stderr)
        r = self.run_cli("garbage\n")
        self.assertEqual(r.returncode, 1)
```

In `test_workflow_policy.py`, next to `test_cloudflare_pub_in_site_env`, add `test_promote_checks_the_versions_before_it_deploys`: in `site.yml`'s `promote` job, a step whose `run` holds `versions.py promotable` comes before the step that holds `secrets.CLOUDFLARE_API_TOKEN_PUB`, and that step's `if` names `inputs.allow-unpublished`. Use the file's own helpers (`load_workflows`, `jobs`).

- [ ] **Step 2: Run them and see them fail**

Run: `python3 -m unittest discover -s site/tools -p 'test_versions.py'` and `python3 -m unittest discover -s .github/scripts/tests -p 'test_workflow_policy.py'`
Expected: the new tests fail.

- [ ] **Step 3: Implement `unpublished` and the `promotable` subcommand**

- [ ] **Step 4: Change the promote job in `.github/workflows/site.yml`**

- `on.workflow_dispatch` gets an input `allow-unpublished`: `type: boolean`, `default: false`, description `Promote although the build holds a version that is not published (an abandoned bump, or a fix that cannot wait)`.
- The sparse checkout also fetches `site/tools/versions.py`.
- "Download the newest staging build": take the newest push run on `main` whatever its state (`--limit 1 --json databaseId,status,conclusion`, without `--status success`). Stop with `the newest staging run on main is not finished and green (STATUS/CONCLUSION): wait for it, or fix it, then promote` unless it is `completed` and `success`. Keep the "no staging run" error.
- A new step after the download and before the deploy, `Refuse a version that is not published`, with `if: ${{ !inputs.allow-unpublished }}`, running `python3 site/tools/versions.py promotable site-out/VERSIONS`.
- Extend the comment at the top of the file: promote refuses a build with an unpublished version.

- [ ] **Step 5: The same in `site/deploy.sh promote`**

Usage becomes `site/deploy.sh staging|promote [--allow-unpublished]` (the flag only with `promote`). Before `worker_deploy "$staging" irsync-pub`, unless the flag is given, run `python3 "$root/site/tools/versions.py" promotable "$staging/VERSIONS"`. Update the header comment.

- [ ] **Step 6: Run everything**

Run: `python3 -m unittest discover -s site/tools -p 'test_*.py'`, `tools/ci.sh workflows`, and `sh -n site/deploy.sh`
Expected: OK; `ci.sh: passed: workflows`; no output.

Run: `site/build.sh && python3 site/tools/versions.py promotable target/site/VERSIONS; echo $?`
Expected: `0`.

- [ ] **Step 7: Commit**

```bash
git add site/tools/versions.py site/tools/test_versions.py .github/workflows/site.yml site/deploy.sh .github/scripts/tests/test_workflow_policy.py
git commit -m "Refuse to promote a site that shows an unpublished version" \
  -m "A build made between a bump and its publish names a version nobody can install, and its book may not match the release. Promote also stops when the newest staging run is not finished and green, instead of shipping the one before it."
```

---

### Task 6: `sign-release.sh --publish` re-runs the staging build

**Files:**
- Modify: `.github/scripts/sign-release.sh`, `.github/scripts/tests/fake_gh.py`
- Test: `.github/scripts/tests/test_release_scripts.py`

After `Started release-resume.yml …`, the script:

1. Looks up the newest push run of `site.yml` on `main`: `gh run list --workflow site.yml --branch main --event push --limit 1 --json databaseId --jq '.[0].databaseId // empty'`.
2. Re-runs it: `gh run rerun ID`.
3. On success prints `Rebuilding the staging site (run ID), so it shows $tag as published.`
4. If the lookup finds nothing or either command fails, prints, and goes on (the release is published; the script still exits 0):

   ```
   Could not rebuild the staging site. When its newest run on main is finished, re-run it:
     gh run rerun "$(gh run list --workflow site.yml --branch main --event push --limit 1 --json databaseId --jq '.[0].databaseId')"
   ```

5. Ends, in both cases, with:

   ```
   To put $tag on irsync.org: wait for that run, check that https://dev.irsync.org/ shows it, then run:
     gh workflow run site.yml --ref main
   ```

- [ ] **Step 1: Teach `fake_gh.py` the two commands**

Read how it records calls and handles `workflow run`. Add `run list` (prints the state's `site_run`, default `7001`; prints nothing when the state says `no_site_run`) and `run rerun ID` (records the call; fails when the state says `site_rerun_fails`). Follow the file's own way of reading state and failing.

- [ ] **Step 2: Write the failing tests**

- `test_signs_uploads_publishes_and_resumes`: the last call is now `["run", "rerun", "7001"]` and comes after the resume call (replace the `calls[-1]` assertion); stdout holds `Rebuilding the staging site (run 7001), so it shows v1.2.3 as published.` and ends with the two lines of point 5 for `v1.2.3`.
- New `test_publish_survives_a_failed_site_rerun`: with `site_rerun_fails`, exit code 0, stdout holds `Could not rebuild the staging site.` and still ends with the promote lines.
- New `test_publish_survives_no_site_run`: with `no_site_run`, the same, and no `run rerun` call was made.
- `test_without_publish_stops_after_upload` and `test_no_resume_unless_immutable`: no `["run", …]` call at all (`self.calls("run") == []`); keep their `assertNotIn("irsync.org", …)`.

Run: `python3 -m unittest discover -s .github/scripts/tests`
Expected: the changed and new tests fail.

- [ ] **Step 3: Change the script**

Also update step 10 of its header comment: it re-runs the staging site build and prints how to promote.

- [ ] **Step 4: Run the tests**

Run: `python3 -m unittest discover -s .github/scripts/tests` and `tools/ci.sh workflows`
Expected: OK; passed.

- [ ] **Step 5: Commit**

```bash
git add .github/scripts/sign-release.sh .github/scripts/tests/fake_gh.py .github/scripts/tests/test_release_scripts.py
git commit -m "sign-release.sh: rebuild the staging site after publishing" \
  -m "Once the tag exists, a site build reads every book from its tag. Without this, staging kept the book it built from the moving branch, and a patch release never reached the site."
```

---

### Task 7: Docs, full regression, pull request

**Files:**
- Modify: `book/src/contributing/website.md`, `book/src/contributing/releasing.md`, `AGENTS.md`

- [ ] **Step 1: `website.md`**

- Opening and Layout: the site has a landing page and five books; `site/tools/book.py`.
- A new section "The books", after "The release versions": the table of the five paths; that a channel's book stays although the homepage hides the channel; that each is built from its version's `book/src` with today's theme and mdbook, in a working copy under `target/site-books/`; what the build adds (title, address, the sentence on the title page, "Edit this page" only in `/dev/`); that `/dev/` is the working tree; the `VERSIONS` file and its four fields.
- A short section "Linking from the landing page": general links go to `book/…`; a link to a page the released book lacks fails the check; link to `dev/…` until the release has it; the contributor guide is linked at `dev/`.
- "The release versions": the label links to the channel's book and the date to the release page.
- "The checker": links are checked in all five books; flags only against the working tree's pages.
- "Promoting to irsync.org" and "Deploying from a developer machine": the refusal, its message, `allow-unpublished` (`gh workflow run site.yml --ref main -f allow-unpublished=true`, `site/deploy.sh promote --allow-unpublished`), and that promote stops while the newest staging run is going or red (this replaces the sentence that it takes the newest successful one).
- "Indexing": only `/book/` and the landing page are indexed on irsync.org; the four path rules.

- [ ] **Step 2: `releasing.md`**

- "Cutting a release": after the bump merge, dev.irsync.org shows the new version and its book, built from the branch. Publishing re-runs the staging build, which then reads the book from the tag. The last step: wait for that run, look at dev.irsync.org, promote.
- Remove the paragraph that a patch release waits for the next site build on `main`: publishing rebuilds staging for every release.
- "Signing the release", item 7: it re-runs the staging site build; what to do when that fails (the command the script prints).
- "If a release fails": a promote is refused between the bump and the publish; for an abandoned bump use `allow-unpublished`, or land the next bump.

- [ ] **Step 3: `AGENTS.md`**

Where it says the contributor guide is the book under `book/src/contributing/`, nothing changes (that is the source). Only add, in "Tests", one line: the site builds five books; see `book/src/contributing/website.md`.

- [ ] **Step 4: Build and check the book**

Run: `site/build.sh && python3 site/tools/check.py --irsync target/debug/irsync target/site`
Expected: no output.

- [ ] **Step 5: Commit**

```bash
git add book/src/contributing/website.md book/src/contributing/releasing.md AGENTS.md
git commit -m "Document the books per release channel"
```

- [ ] **Step 6: Full regression**

Run: `tools/ci.sh check`, `tools/ci.sh workflows` and `python3 -m unittest discover -s site/tools -p 'test_*.py'`
Expected: all pass. Report failures with their output; do not push over them.

- [ ] **Step 7: Push and open the pull request**

```bash
git push -u origin docs-per-channel
gh pr create --base main --title "Publish the documentation per release channel" --body-file <file>
```

The body, in plain words: the five paths and which book each holds; that old pages get today's look; the version label; the homepage links; that only `/book/` is indexed; the promote refusal with its way out, and that `sign-release.sh --publish` rebuilds staging; what was tested and what was only looked at. End with `Closes #105`.

After the merge, the first staging run is the real proof: check that `https://dev.irsync.org/beta/`, `/alpha/` and `/dev/` exist, that `/stable/` answers 404, and that `https://dev.irsync.org/VERSIONS` lists them.
