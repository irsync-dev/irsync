# Release Versions on the Homepage Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** The homepage shows the current version of each release channel, worked out from git when the site builds, and follows a release without hand edits.

**Architecture:** A new Python tool, `site/tools/versions.py`, reads the `v*` tags and the `Cargo.toml` version of `HEAD` and the `origin/release-X.Y` branches, picks what to show, and fills marked places in `site/index.html` while `site/build.sh` copies it. `site.yml` fetches tags and also runs on a `Cargo.toml` change, so the bump PR's merge refreshes dev.irsync.org. `sign-release.sh --publish` ends by printing the promote command.

**Tech Stack:** Python 3 standard library and `unittest`, git, POSIX sh, Tailwind CSS 4 with daisyUI, GitHub Actions, bash.

**Spec:** `docs/superpowers/specs/2026-11-19-site-release-versions-design.md`. Read it first; the plan argues from it.

## Global Constraints

- Work on branch `site-release-versions` (it exists, with the spec). One PR against `main`. No attribution trailers in commits or the PR.
- Commit messages: a short summary line, a blank line, then a brief plain-English reason if it isn't obvious.
- No network and no GitHub token to work out versions. The browser looks nothing up.
- irsync.org changes only through the manual promote. The promote job, the release gate and the resume run do not change.
- The README, the book's user chapters and `site/install.sh` do not change (apart from Task 1's sentence removal).
- Channels by shape: `X.Y.Z` stable, `X.Y.Z-beta.N` beta, `X.Y.Z-alpha.N` alpha; anything else is ignored.
- Date format on the page: `19 Nov 2026`. A date is the commit's own calendar day (`%cs`), not UTC.
- Install command endings: `sh` (stable, or nothing released), `sh -s -- --beta`, `sh -s -- --alpha`.
- PR CI does not run the site tool tests. Run `python3 -m unittest discover -s site/tools -p 'test_*.py'` locally before every commit that touches `site/`.
- Docs follow the book's plain style: short sentences, no jargon.

## Review Focus

1. A shallow clone with no tags: the build must fail and say to fetch tags, not deploy a page with no versions. (Task 3)
2. Tags that look like versions but are not releases (`v1.2`, `v0.1.0-rc.1`, `v0.1.0-beta`, `vnext`): ignored, no crash. (Task 2, Task 3)
3. Numbers compare as numbers: `0.1.0-beta.10` is newer than `0.1.0-beta.9`, `0.10.0` newer than `0.9.0`. (Task 2)
4. A commit made just after midnight in `+01:00`: the page shows that commit's own day, not the UTC day before. (Task 3)
5. An old `origin/release-X.Y` whose `Cargo.toml` is missing, has no workspace version, or holds an older patch than the current stable: skipped or outranked, no crash and no wrong line. (Task 3)
6. A marker missing from `site/index.html` after someone edits the page: the build fails and names the marker. (Task 4)

## File Structure

| File | Change | Responsibility |
| --- | --- | --- |
| `site/tools/versions.py` | create | Pick the shown versions (`parse`, `pick`), read them from git (`from_git`), fill the page (`render`), CLI |
| `site/tools/test_versions.py` | create | Tests for all of the above |
| `site/index.html` | modify | Markers, the version container, the version copy buttons |
| `site/tailwind.css` | modify | `@source` for the tool, the regular condensed font face |
| `site/build.sh` | modify | Run the tool for `index.html`; copy the tenth font |
| `site/tools/test_check.py` | modify | The new font file is allowed |
| `.github/workflows/site.yml` | modify | Full fetch, `Cargo.toml` path |
| `.github/scripts/sign-release.sh`, `.github/scripts/tests/test_release_scripts.py` | modify | Print the promote command after publishing |
| `book/src/contributing/website.md`, `releasing.md` | modify | Docs |

---

### Task 1: Commit the two sentence removals

The working tree already holds two edits Mike asked for: the sentence about `irsync sync -av …` "with its name spelled out" is gone from `site/index.html` and `README.md`.

- [ ] **Step 1: Check the diff is only that**

Run: `git diff --stat`
Expected: `README.md` and `site/index.html`, 1 insertion and 6 deletions in total.

- [ ] **Step 2: Commit**

```bash
git add README.md site/index.html
git commit -m "Drop the 'irsync sync' aside from the homepage and the README" \
  -m "It took space next to the first command and said nothing a new user needs there."
```

---

### Task 2: Pick the versions to show

**Files:**
- Create: `site/tools/versions.py`
- Test: `site/tools/test_versions.py`

**Interfaces:**
- Produces:
  - `parse(text: str) -> tuple[int, int, int, int, int] | None`: the sort key `(x, y, z, rank, n)` with rank 0 alpha, 1 beta, 2 stable and `n` 0 for stable; `None` when `text` is not exactly `X.Y.Z`, `X.Y.Z-beta.N` or `X.Y.Z-alpha.N`. No leading `v`.
  - `channel(text: str) -> str`: `"stable"`, `"beta"` or `"alpha"` for a valid version.
  - `Shown = namedtuple("Shown", "channel version date")`, `date` an ISO string `YYYY-MM-DD`.
  - `pick(candidates: dict[str, str]) -> list[Shown]`: version text to ISO date in, the shown versions out, in the order stable, beta, alpha. The first is the main version. Keys that `parse` rejects are ignored.

- [ ] **Step 1: Write the failing tests**

```python
import unittest
import versions
from versions import Shown

D = "2026-11-19"

class ParseTests(unittest.TestCase):
    def test_shapes(self):
        self.assertEqual(versions.parse("1.2.3"), (1, 2, 3, 2, 0))
        self.assertEqual(versions.parse("0.1.0-beta.5"), (0, 1, 0, 1, 5))
        self.assertEqual(versions.parse("0.1.0-alpha.4"), (0, 1, 0, 0, 4))

    def test_not_a_release(self):
        for t in ["1.2", "v1.2.3", "0.1.0-rc.1", "0.1.0-beta", "0.1.0-beta.x",
                  "next", "", "1.2.3 ", "1.2.3-beta.1.2"]:
            self.assertIsNone(versions.parse(t), t)

    def test_numbers_compare_as_numbers(self):
        self.assertGreater(versions.parse("0.1.0-beta.10"), versions.parse("0.1.0-beta.9"))
        self.assertGreater(versions.parse("0.10.0"), versions.parse("0.9.0"))
        self.assertGreater(versions.parse("0.1.0"), versions.parse("0.1.0-beta.99"))
        self.assertGreater(versions.parse("0.1.0-beta.1"), versions.parse("0.1.0-alpha.99"))

class PickTests(unittest.TestCase):
    def shown(self, *vs):
        return [(s.channel, s.version) for s in versions.pick({v: D for v in vs})]

    def test_spec_table(self):
        self.assertEqual(self.shown("0.1.0-beta.5", "0.1.0-alpha.4"), [("beta", "0.1.0-beta.5")])
        self.assertEqual(self.shown("0.1.0", "0.1.0-beta.7", "0.1.0-alpha.4"), [("stable", "0.1.0")])
        self.assertEqual(self.shown("0.1.0", "0.2.0-beta.1", "0.2.0-alpha.3"),
                         [("stable", "0.1.0"), ("beta", "0.2.0-beta.1")])
        self.assertEqual(self.shown("0.1.2", "0.2.0-beta.1", "0.3.0-alpha.1"),
                         [("stable", "0.1.2"), ("beta", "0.2.0-beta.1"), ("alpha", "0.3.0-alpha.1")])
        self.assertEqual(self.shown("0.1.0", "0.1.0-beta.7", "0.2.0-alpha.1"),
                         [("stable", "0.1.0"), ("alpha", "0.2.0-alpha.1")])
        self.assertEqual(self.shown("0.1.0-alpha.4"), [("alpha", "0.1.0-alpha.4")])
        self.assertEqual(self.shown(), [])

    def test_alpha_is_held_against_a_hidden_beta(self):
        # beta 0.2.0-beta.1 is hidden by stable 0.2.0, and still hides the alpha.
        self.assertEqual(self.shown("0.2.0", "0.2.0-beta.1", "0.2.0-alpha.9"), [("stable", "0.2.0")])

    def test_newest_of_each_channel_and_junk_ignored(self):
        self.assertEqual(self.shown("0.1.0-beta.9", "0.1.0-beta.10", "0.1.0-rc.1", "next"),
                         [("beta", "0.1.0-beta.10")])

    def test_keeps_the_date(self):
        self.assertEqual(versions.pick({"1.0.0": "2027-03-03"}), [Shown("stable", "1.0.0", "2027-03-03")])
```

- [ ] **Step 2: Run them and see them fail**

Run: `python3 -m unittest discover -s site/tools -p 'test_versions.py'`
Expected: an import error for `versions`.

- [ ] **Step 3: Implement `parse`, `channel`, `Shown` and `pick` in `site/tools/versions.py`**

Start the file with a docstring that says what the tool does and where the rules come from (the spec's "Which versions are shown"). `pick` takes the newest version per channel, then shows the stable, the beta if it is newer than the stable, and the alpha if it is newer than the stable and the beta, whether or not that beta is shown.

- [ ] **Step 4: Run the tests**

Run: `python3 -m unittest discover -s site/tools -p 'test_versions.py'`
Expected: OK.

- [ ] **Step 5: Commit**

```bash
git add site/tools/versions.py site/tools/test_versions.py
git commit -m "Add the rules that pick the versions the homepage shows"
```

---

### Task 3: Read the versions from git

**Files:**
- Modify: `site/tools/versions.py`
- Test: `site/tools/test_versions.py`

**Interfaces:**
- Consumes: `parse` from Task 2.
- Produces:
  - `class VersionsError(Exception)`.
  - `from_git(repo: pathlib.Path) -> dict[str, str]`: version text to ISO date, ready for `pick`.

What `from_git` collects:

- Every tag `vVERSION` whose `VERSION` parses, dated with the committer date of the commit it points at. One call does it for lightweight and annotated tags: `git for-each-ref --format='%(refname:short) %(*committerdate:short)%(committerdate:short)' 'refs/tags/v*'` (for an annotated tag the plain field is empty and the starred one is the commit's; for a lightweight tag the other way round).
- If no tag parses, raise `VersionsError("no release tags in this checkout; run: git fetch --tags")`.
- The `version` in the `[workspace.package]` section of `Cargo.toml`, read with `git show REF:Cargo.toml`, for `HEAD` and for every ref `refs/remotes/origin/release-X.Y` (digits only). A ref with no `Cargo.toml`, no such section or a version that does not parse is skipped. A version that already has a tag is skipped: the tag's date stands.
- The date of such an untagged version: `git log -1 --first-parent --format=%cs -S'version = "VERSION"' REF -- Cargo.toml`. `--first-parent` makes git see the bump PR's merge commit, which is the commit the tag will land on. If that prints nothing, use the date of `REF` itself.

- [ ] **Step 1: Write the failing tests**

Add a helper that builds a throwaway repository in a temp directory: `git init -q -b main`, a local `user.name` and `user.email`, and a `commit(cargo_version, when)` that writes `Cargo.toml` as `[workspace.package]\nversion = "…"\n` and commits with `GIT_AUTHOR_DATE` and `GIT_COMMITTER_DATE` set to `when`. Run git with `GIT_CONFIG_GLOBAL=/dev/null` and `GIT_CONFIG_SYSTEM=/dev/null` so the developer's settings (signing, hooks) stay out.

```python
class FromGitTests(RepoCase):
    def test_tags_and_dates(self):
        self.commit("0.1.0-alpha.4", "2026-11-13T12:00:00+00:00"); self.git("tag", "v0.1.0-alpha.4")
        self.commit("0.1.0-beta.5", "2026-11-19T00:30:00+01:00"); self.git("tag", "v0.1.0-beta.5")
        self.assertEqual(versions.from_git(self.repo),
                         {"0.1.0-alpha.4": "2026-11-13", "0.1.0-beta.5": "2026-11-19"})  # not 11-18 (UTC)

    def test_annotated_tag_uses_the_commit_date(self):
        self.commit("0.1.0-beta.1", "2026-11-14T12:00:00+00:00")
        self.git("tag", "-a", "-m", "irsync 0.1.0-beta.1", "v0.1.0-beta.1",
                 env={"GIT_COMMITTER_DATE": "2026-12-01T12:00:00+00:00"})
        self.assertEqual(versions.from_git(self.repo), {"0.1.0-beta.1": "2026-11-14"})

    def test_junk_tags_are_ignored(self):
        self.commit("0.1.0-beta.1", "2026-11-14T12:00:00+00:00")
        for t in ["v0.1.0-beta.1", "v1.2", "v0.1.0-rc.1", "vnext", "other"]:
            self.git("tag", t)
        self.assertEqual(list(versions.from_git(self.repo)), ["0.1.0-beta.1"])

    def test_no_tags_fails_and_says_what_to_do(self):
        self.commit("0.1.0-beta.1", "2026-11-14T12:00:00+00:00")
        with self.assertRaisesRegex(versions.VersionsError, "git fetch --tags"):
            versions.from_git(self.repo)

    def test_release_being_built_on_head(self):
        self.commit("0.1.0-beta.5", "2026-11-19T12:00:00+00:00"); self.git("tag", "v0.1.0-beta.5")
        self.commit("0.1.0-beta.6", "2026-11-25T12:00:00+00:00")   # the bump
        self.commit("0.1.0-beta.6", "2026-11-27T12:00:00+00:00")   # a later commit, same version
        self.assertEqual(versions.from_git(self.repo)["0.1.0-beta.6"], "2026-11-25")

    def test_bump_merged_through_a_pull_request(self):
        # The date is the merge commit's, as the tag will sit on it.
        self.commit("0.1.0-beta.5", "2026-11-19T12:00:00+00:00"); self.git("tag", "v0.1.0-beta.5")
        self.git("switch", "-q", "-c", "bump")
        self.commit("0.1.0-beta.6", "2026-11-24T12:00:00+00:00")
        self.git("switch", "-q", "main")
        self.git("merge", "-q", "--no-ff", "-m", "Merge bump", "bump",
                 env={"GIT_COMMITTER_DATE": "2026-11-25T12:00:00+00:00",
                      "GIT_AUTHOR_DATE": "2026-11-25T12:00:00+00:00"})
        self.assertEqual(versions.from_git(self.repo)["0.1.0-beta.6"], "2026-11-25")

    def test_release_branches(self):
        self.commit("1.2.0", "2027-03-03T12:00:00+00:00"); self.git("tag", "v1.2.0")
        base = self.git("rev-parse", "HEAD")
        self.commit("1.2.1", "2027-03-10T12:00:00+00:00")
        self.git("update-ref", "refs/remotes/origin/release-1.2", "HEAD")
        self.git("update-ref", "refs/remotes/origin/release-v0.1.0-beta.3", "HEAD")  # not a release branch
        self.git("reset", "-q", "--hard", base)
        self.git("rm", "-q", "Cargo.toml"); self.git("commit", "-q", "-m", "no cargo")
        self.git("update-ref", "refs/remotes/origin/release-1.1", "HEAD")           # nothing to read
        self.git("reset", "-q", "--hard", base)
        self.assertEqual(versions.from_git(self.repo), {"1.2.0": "2027-03-03", "1.2.1": "2027-03-10"})

    def test_real_checkout(self):
        got = versions.from_git(ROOT)
        self.assertEqual(got["0.1.0-beta.5"], "2026-11-19")
```

`test_real_checkout` is skipped (`self.skipTest`) when `git -C ROOT tag -l v0.1.0-beta.5` prints nothing, so a shallow CI checkout elsewhere does not fail.

- [ ] **Step 2: Run them and see them fail**

Run: `python3 -m unittest discover -s site/tools -p 'test_versions.py'`
Expected: the new tests fail, `from_git` is missing.

- [ ] **Step 3: Implement `VersionsError` and `from_git`**

Run git with `subprocess.run([...], capture_output=True, text=True)` and `-C repo`. A git that fails where it must not (the tag listing) raises `VersionsError` with git's message.

- [ ] **Step 4: Run the tests**

Run: `python3 -m unittest discover -s site/tools -p 'test_versions.py'`
Expected: OK.

- [ ] **Step 5: Commit**

```bash
git add site/tools/versions.py site/tools/test_versions.py
git commit -m "Read the released and in-progress versions from git" \
  -m "A v* tag exists exactly for a published release, and a Cargo.toml version ahead of the tags is the release being built."
```

---

### Task 4: Fill the page at build time

**Files:**
- Modify: `site/tools/versions.py`, `site/index.html`, `site/build.sh`, `site/tailwind.css`
- Test: `site/tools/test_versions.py`

**Interfaces:**
- Consumes: `Shown`, `pick`, `from_git`, `VersionsError`.
- Produces:
  - `render(html: str, shown: list[Shown]) -> str`.
  - CLI: `python3 site/tools/versions.py REPO IN_HTML OUT_HTML`, exit 1 with `versions.py: MESSAGE` on stderr for a `VersionsError`.
  - In the built page: each version is `<span class="version" data-tail="TAIL">VERSION</span>`; the command's ending sits in an element with `id="install-tail"`. Task 5's script relies on both.

**The markers.** A region is `<!-- versions:NAME -->` … `<!-- /versions:NAME -->`. `render` replaces what is between them and removes the marker comments themselves:

| Name | Where in `site/index.html` | Filled with |
| --- | --- | --- |
| `lines` | Empty region inside a new `<p>` on the Install heading's row (below) | One line per shown version |
| `tail` | Around the text `sh -s -- --beta` in the install command | `sh`, `sh -s -- --beta` or `sh -s -- --alpha`, by the main version |
| `prerelease` | Twice: around the whole badge `<span>` in the header, and around the whole yellow note `<p role="note">` | Kept when the main version is a beta or alpha; emptied when it is stable or nothing is shown |
| `channel` | Inside the badge, around `beta` | `beta` or `alpha` |
| `Channel` | Inside the note's label, around `Beta` (the full stop stays outside) | `Beta` or `Alpha` |

`render` raises `VersionsError("site/index.html: marker NAME not found")` when `lines`, `tail`, `prerelease`, `channel` or `Channel` does not occur at least once.

**The Install heading's row** replaces the bare `<h3 id="install" …>`:

```html
<div class="flex flex-wrap items-baseline justify-between gap-x-4">
  <h3 id="install" class="font-condensed text-label leading-7">Install</h3>
  <p class="flex flex-col items-end text-small text-muted"><!-- versions:lines --><!-- /versions:lines --></p>
</div>
```

**One line**, a constant in `versions.py` (the label capitalised, the date as `19 Nov 2026` from a fixed list of English month names, not the locale):

```html
<span class="inline-flex items-baseline gap-x-1.5 font-condensed font-normal whitespace-nowrap"><strong class="font-semibold">Beta</strong> <span class="version" data-tail="sh -s -- --beta">0.1.0-beta.5</span> <span>· 19 Nov 2026</span></span>
```

The install command's ending becomes `<span class="whitespace-nowrap" id="install-tail"><!-- versions:tail -->sh -s -- --beta<!-- /versions:tail --></span>`.

- [ ] **Step 1: Write the failing tests**

```python
INDEX = (ROOT / "site" / "index.html").read_text()

class RenderTests(unittest.TestCase):
    def page(self, *vs):
        return versions.render(INDEX, versions.pick({v: "2026-11-19" for v in vs}))

    def test_beta_only(self):
        out = self.page("0.1.0-beta.5", "0.1.0-alpha.4")
        self.assertIn('<strong class="font-semibold">Beta</strong> '
                      '<span class="version" data-tail="sh -s -- --beta">0.1.0-beta.5</span> '
                      '<span>· 19 Nov 2026</span>', out)
        self.assertEqual(out.count('class="version"'), 1)
        self.assertIn('id="install-tail">sh -s -- --beta</span>', out)
        self.assertIn(">beta</span", out)        # the badge
        self.assertIn("Beta.</strong>", out)     # the note
        self.assertNotIn("versions:", out)       # no marker survives

    def test_stable_with_newer_beta_and_alpha(self):
        out = self.page("0.1.2", "0.2.0-beta.1", "0.3.0-alpha.1")
        order = [out.index(v) for v in ('>0.1.2<', '>0.2.0-beta.1<', '>0.3.0-alpha.1<')]
        self.assertEqual(order, sorted(order))
        self.assertIn('data-tail="sh">0.1.2<', out)
        self.assertIn('data-tail="sh -s -- --alpha">0.3.0-alpha.1<', out)
        self.assertIn('id="install-tail">sh</span>', out)
        self.assertNotIn('role="note"', out)
        self.assertNotIn("bg-signal", out)       # the badge is gone

    def test_alpha_only(self):
        out = self.page("0.1.0-alpha.4")
        self.assertIn('id="install-tail">sh -s -- --alpha</span>', out)
        self.assertIn(">alpha</span", out)
        self.assertIn("Alpha.</strong>", out)

    def test_nothing_released(self):
        out = self.page()
        self.assertNotIn('class="version"', out)
        self.assertIn('id="install-tail">sh</span>', out)
        self.assertNotIn('role="note"', out)

    def test_single_digit_day(self):
        out = versions.render(INDEX, versions.pick({"1.0.0": "2027-03-03"}))
        self.assertIn("· 3 Mar 2027", out)

    def test_missing_marker_fails(self):
        broken = INDEX.replace("<!-- versions:tail -->", "")
        with self.assertRaisesRegex(versions.VersionsError, "marker tail not found"):
            versions.render(broken, [])

class CliTests(RepoCase):
    def test_writes_the_page(self):
        self.commit("0.1.0-beta.5", "2026-11-19T12:00:00+00:00"); self.git("tag", "v0.1.0-beta.5")
        out = self.repo / "out.html"
        r = subprocess.run([sys.executable, TOOL, self.repo, ROOT / "site/index.html", out],
                           capture_output=True, text=True)
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertIn(">0.1.0-beta.5<", out.read_text())

    def test_no_tags_exits_1_and_writes_nothing(self):
        self.commit("0.1.0-beta.5", "2026-11-19T12:00:00+00:00")
        out = self.repo / "out.html"
        r = subprocess.run([sys.executable, TOOL, self.repo, ROOT / "site/index.html", out],
                           capture_output=True, text=True)
        self.assertEqual(r.returncode, 1)
        self.assertIn("versions.py: no release tags", r.stderr)
        self.assertFalse(out.exists())
```

- [ ] **Step 2: Run them and see them fail**

Run: `python3 -m unittest discover -s site/tools -p 'test_versions.py'`
Expected: the new tests fail.

- [ ] **Step 3: Add the markers and the heading row to `site/index.html`**

As in the table and the HTML above. Nothing else on the page changes in this task.

- [ ] **Step 4: Implement `render` and the CLI in `versions.py`**

- [ ] **Step 5: Wire it into the build**

In `site/build.sh`, take `index.html` out of the `for f in …; do cp` list and, right after that loop, run:

```sh
# The landing page, with the release versions from git written in.
python3 "$root/site/tools/versions.py" "$root" "$root/site/index.html" "$out/index.html"
```

In `site/tailwind.css`, after `@source "./404.html";`, add `@source "./tools/versions.py";` with a comment that the version lines' classes live there.

- [ ] **Step 6: Run the tests and a real build**

Run: `python3 -m unittest discover -s site/tools -p 'test_*.py'`
Expected: OK (all site tool tests, `test_build.py` included).

Run: `site/build.sh && grep -c 'class="version"' target/site/index.html && grep -c 'versions:' target/site/index.html`
Expected: `1`, then `0` (grep exits 1 on the second; that is the pass).

Run: `cargo build -p irsync && python3 site/tools/check.py --irsync target/debug/irsync target/site`
Expected: no output, exit 0.

- [ ] **Step 7: Commit**

```bash
git add site/tools/versions.py site/tools/test_versions.py site/index.html site/build.sh site/tailwind.css
git commit -m "Show the current release versions on the homepage" \
  -m "The build writes them into the page from git. The install command, the beta badge and the beta note follow the same data."
```

---

### Task 5: The look: font weight and copy buttons

**Files:**
- Modify: `site/tailwind.css`, `site/build.sh`, `site/index.html`, `site/tools/test_check.py`

**Interfaces:**
- Consumes: `span.version[data-tail]` and `#install-tail` from Task 4.

- [ ] **Step 1: Write the failing font test**

In `site/tools/test_check.py`, in the test that writes the allowed fonts (around line 66), also write `fonts/barlow-semi-condensed-latin-400-normal.woff2` and keep its "no problems" assertion. `check.py`'s pattern already allows any weight, so this passes at once; it pins that the file may ship. Run it:

Run: `python3 -m unittest discover -s site/tools -p 'test_check.py'`
Expected: OK.

- [ ] **Step 2: Ship the regular condensed face**

In `site/tailwind.css`, add next to the existing Barlow Semi Condensed rule:

```css
@font-face {
  font-family: "Barlow Semi Condensed";
  font-style: normal;
  font-weight: 400;
  font-display: fallback;
  src: url("fonts/barlow-semi-condensed-latin-400-normal.woff2") format("woff2");
}
```

In `site/build.sh`, add `barlow-semi-condensed/files/barlow-semi-condensed-latin-400-normal.woff2` to the font copy list and correct the comment above it (labels in semibold, the version lines in regular). No preload link.

- [ ] **Step 3: Turn the versions into copy buttons**

In the inline script at the end of `site/index.html`, inside the existing `if (navigator.clipboard) { … }`, after the loop over `.cmd`: for each `.version`, replace the span with a `<button type="button">` that

- shows the version, and has `title` `Copy the install command for VERSION`;
- has the classes `btn btn-xs h-auto border-base-300 px-2 py-0.5 [font:inherit] text-base-content`;
- on click copies the install command's text with the ending swapped: the `textContent` of the `.copy` element that holds `#install-tail`, where the tail's text is replaced by the button's `data-tail`. Build it from a clone of that element, so the page itself does not change;
- then shows `Copied` for 1500 ms and goes back to the version, as the Copy buttons do.

Update the comment above the script: it also turns each version into such a button, and without script the versions are plain text.

- [ ] **Step 4: Build and check**

Run: `site/build.sh && ls target/site/fonts/*.woff2 | wc -l`
Expected: `10`.

Run: `grep -c 'font:inherit' target/site/style.css`
Expected: `1` (Tailwind kept the button's classes).

Run: `python3 -m unittest discover -s site/tools -p 'test_*.py' && python3 site/tools/check.py --irsync target/debug/irsync target/site`
Expected: OK, then no output.

- [ ] **Step 5: Look at it**

Run `site/serve.sh` and open `http://localhost:9999/`. Check against the spec's sketch: the line `Beta [0.1.0-beta.5] · 19 Nov 2026` is at the right of the "Install" line, on its baseline, label, button and date on one baseline, in the condensed font; clicking the button copies the full curl line ending in `sh -s -- --beta`. If you cannot open a browser, say so in the hand-back; do not claim it was checked.

- [ ] **Step 6: Commit**

```bash
git add site/tailwind.css site/build.sh site/index.html site/tools/test_check.py
git commit -m "Make each version a button that copies its install command" \
  -m "Adds the regular weight of Barlow Semi Condensed for the version and the date."
```

---

### Task 6: Build the site when a release is bumped

**Files:**
- Modify: `.github/workflows/site.yml`

- [ ] **Step 1: Change the staging job**

- Under `on.push.paths`, add `- Cargo.toml`.
- Give the staging job's `actions/checkout` step `with: fetch-depth: 0`, with a comment: the build reads the release tags and the `release-X.Y` branches.
- Extend the comment at the top of the file: a version bump is such a push, so staging shows a release from its bump on.

The promote job stays as it is.

- [ ] **Step 2: Check the workflows**

Run: `tools/ci.sh workflows`
Expected: passes (the workflow policy tests and actionlint). If a policy test pins `site.yml`'s paths or checkout, update that test to the new values in the same commit.

- [ ] **Step 3: Commit**

```bash
git add .github/workflows/site.yml
git commit -m "Rebuild the site when the version in Cargo.toml changes" \
  -m "The merge of a bump PR now refreshes dev.irsync.org. The build needs the tags, so the job fetches the full history."
```

---

### Task 7: `sign-release.sh --publish` prints the promote command

**Files:**
- Modify: `.github/scripts/sign-release.sh` (the end of the file, and step 10 of the header comment)
- Test: `.github/scripts/tests/test_release_scripts.py`

- [ ] **Step 1: Write the failing tests**

In `test_signs_uploads_publishes_and_resumes` (it runs `sign-release.sh --publish` to the end), add:

```python
self.assertIn("To put v1.3.0 on irsync.org: check that https://dev.irsync.org/ shows it, then run:\n"
              "  gh workflow run site.yml --ref main\n", r.stdout)
self.assertTrue(r.stdout.endswith("  gh workflow run site.yml --ref main\n"))
```

with the version that test uses in place of `1.3.0`, and its own name for the result in place of `r`. In `test_without_publish_stops_after_upload` and in `test_no_resume_unless_immutable`, add `self.assertNotIn("irsync.org", r.stdout)`.

- [ ] **Step 2: Run them and see them fail**

Run: `python3 -m unittest discover -s .github/scripts/tests`
Expected: the `--publish` test fails on the new assertion.

- [ ] **Step 3: Print the two lines**

After the last `echo "Started release-resume.yml …"`:

```bash
echo "To put $tag on irsync.org: check that https://dev.irsync.org/ shows it, then run:"
echo "  gh workflow run site.yml --ref main"
```

Add to step 10 of the header comment that it then prints how to promote the site.

- [ ] **Step 4: Run the tests**

Run: `python3 -m unittest discover -s .github/scripts/tests`
Expected: OK.

- [ ] **Step 5: Commit**

```bash
git add .github/scripts/sign-release.sh .github/scripts/tests/test_release_scripts.py
git commit -m "sign-release.sh: say how to promote the site after publishing"
```

---

### Task 8: Docs, full regression, pull request

**Files:**
- Modify: `book/src/contributing/website.md`, `book/src/contributing/releasing.md`

- [ ] **Step 1: `website.md`**

- Layout table: `site/tools/` also holds `versions.py` (the release versions on the landing page).
- A new section "The release versions", after "The landing page design": what the page shows and the rules (short, with the spec's example table), that the build takes them from the `v*` tags and from `Cargo.toml` on `HEAD` and the `origin/release-X.Y` branches, the date it uses, the markers in `site/index.html` and that the build fails when one is missing or the checkout has no tags (`git fetch --tags`).
- "Building": the fonts are now ten files; the regular Barlow Semi Condensed is for the version lines.
- "The landing page design", Type: Barlow Semi Condensed ships in semibold and regular.
- "Deploying from GitHub": the staging job also runs on a `Cargo.toml` change and fetches the full history.
- "Deploying from a developer machine": run `git fetch --tags` first; the page shows what the local tags say.

- [ ] **Step 2: `releasing.md`**

- In "Cutting a release", after step 3: merging the bump rebuilds the site, and dev.irsync.org shows the new version from then on, before it is published.
- Step 5: with `--publish` the script ends by printing how to promote the site; do it once dev.irsync.org shows the version (link to `website.md#promoting-to-irsyncorg`).
- "Signing the release", item 7: the same, in one clause.
- A short note for patch releases: the bump lands on `release-X.Y`, which does not rebuild the site; it picks the patch up with the next site build on `main`.

- [ ] **Step 3: Build and check the book**

Run: `site/build.sh && python3 site/tools/check.py --irsync target/debug/irsync target/site`
Expected: no output.

- [ ] **Step 4: Commit**

```bash
git add book/src/contributing/website.md book/src/contributing/releasing.md
git commit -m "Document the release versions on the homepage"
```

- [ ] **Step 5: Full regression**

Run: `tools/ci.sh check` and `python3 -m unittest discover -s site/tools -p 'test_*.py'`
Expected: both pass. Report failures with their output; do not push over them.

- [ ] **Step 6: Push and open the pull request**

```bash
git push -u origin site-release-versions
gh pr create --base main --title "Show the current release versions on the homepage" --body-file <file>
```

The body, in plain words: what the page now shows and the rules in two lines; that versions come from git at build time; that a bump merge refreshes dev.irsync.org and promote stays manual; the `sign-release.sh` hint; the side change from Task 1; what was tested, and that the page's look was or was not checked in a browser. Add `Refs #105` (the per-channel docs issue builds on this). No `Closes` line: there is no issue for this work.
