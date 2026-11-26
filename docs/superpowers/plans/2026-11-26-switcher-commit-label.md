# The Dev book's commit in the version switcher: implementation plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** the switcher's Dev entry shows `irsync@SHORTSHA` instead of `unreleased`.

**Architecture:** `switcher.py` already parses `VERSIONS` (`PATH VERSION COMMIT STATE` per line). It keeps the commit of each line in its `Entry`, and `menu` labels an entry with no version by that commit.

**Tech Stack:** Python 3 (standard library), unittest.

**Spec:** `docs/superpowers/specs/2026-11-26-switcher-commit-label-design.md`

## Global Constraints

- Label, exact: `irsync@` followed by the first 7 characters of the 40-character commit, e.g. `irsync@a3cdcd6`. `irsync` is fixed text.
- Commit field `-` on a working-tree line: the label stays `unreleased`.
- Release entries keep their version; the button text (`Dev`) and `label_book.py` do not change.
- Run the site tool tests from `site/tools`: `cd site/tools && python3 -m unittest test_switcher -v`.

## Review Focus

- A `VERSIONS` from an older build with `dev - - dev`: Dev shows `unreleased`, no crash. (Test in Task 1.)
- The landing page menu (`apply` → `landing`) shows the same label as the book menus. (Covered: both use `menu`; checked by `ApplyTests.test_landing_page` in Task 1.)
- A release entry never shows a commit, even though its line has one. (Test in Task 1.)

---

### Task 1: Label the Dev entry with its commit

**Files:**
- Modify: `site/tools/switcher.py` (`Entry`, `_lines`, `entries`, `ticked`, `menu`; module docstring)
- Test: `site/tools/test_switcher.py`
- Docs: `book/src/contributing/website.md` ("The books", the paragraph starting "Once mdbook has built all of them"), `docs/superpowers/specs/2026-11-19-book-version-switcher-design.md` (the sketch line `Dev   unreleased` and "Dev shows `unreleased`")

**Interfaces:**
- Produces: `Entry = namedtuple("Entry", "path label version commit")`. `commit` is the line's 40-character commit, or `None` when the field is `-`. `version` stays `None` for a working-tree book.
- `_lines(versions_text) -> {path: (version | None, commit | None)}`; `ticked` reads the version from it.
- `menu` signature unchanged.

- [ ] **Step 1: Write the failing tests** in `test_switcher.py`

  - `EntriesTests.test_entries`: expectations get the commit: `Entry("beta", "Beta", "0.1.0-beta.5", SHA)`, `Entry("alpha", "Alpha", "0.1.0-alpha.4", SHA)`, `Entry("dev", "Dev", None, SHA)`; stable `Entry("stable", "Stable", "1.0.0", SHA)`; `NO_RELEASE` → `[Entry("dev", "Dev", None, SHA)]`.
  - `MenuTests.test_page_in_a_subdirectory`: replace `assertIn("unreleased", menu)` with `assertIn(">Dev <span class=\"irsync-book-version\">irsync@bbbbbbb</span>", menu)` and `assertNotIn("unreleased", menu)`.
  - New `MenuTests.test_release_entries_show_their_version_not_a_commit`: the beta entry's text is `Beta <span class="irsync-book-version">0.1.0-beta.5</span>`; `menu.count("irsync@")` is 1.
  - New `MenuTests.test_dev_without_a_commit_is_unreleased`: with `TODAY.replace(f"dev - {SHA} main", "dev - - dev")`, the menu contains `Dev <span class="irsync-book-version">unreleased</span>` and no `irsync@`.
  - `ApplyTests.test_apply_over_a_small_site`: also assert `irsync@bbbbbbb` is in a built dev page. `ApplyTests.test_landing_page`: also assert it is in the landing page.

- [ ] **Step 2: Run them to see them fail**

  Run: `cd site/tools && python3 -m unittest test_switcher -v`
  Expected: the tests above FAIL (Entry has 3 fields; menu shows `unreleased`).

- [ ] **Step 3: Implement in `switcher.py`**

  `Entry` and `_lines` as in Interfaces (`-` → `None` for both fields). In `menu`, the detail text is `e.version`, else `f"irsync@{e.commit[:7]}"` when `e.commit`, else `"unreleased"`, HTML-escaped as now. Update the `Entry` comment and the module docstring: Dev shows the commit it was built from.

- [ ] **Step 4: Run the tests**

  Run: `cd site/tools && python3 -m unittest discover -v`
  Expected: all PASS.

- [ ] **Step 5: Update the docs**

  `website.md`: in the paragraph above, "then Dev" becomes "then Dev with the commit it was built from (`irsync@a3cdcd6`), so a glance at dev.irsync.org tells whether a merge is live". Switcher spec: the sketch line becomes `│ Dev   irsync@a3cdcd6 │` and the sentence notes the change and points to `2026-11-26-switcher-commit-label-design.md`.

- [ ] **Step 6: Build the site and look**

  Run: `site/build.sh`, then `grep -o 'irsync@[0-9a-f]*' target/site/dev/index.html | head -1`.
  Expected: `irsync@` + the first 7 characters of `git rev-parse HEAD`.

- [ ] **Step 7: Commit**

  ```bash
  git add site/tools/switcher.py site/tools/test_switcher.py book/src/contributing/website.md docs/superpowers/specs/2026-11-19-book-version-switcher-design.md
  git commit -m "Show the Dev book's commit in the version switcher" -m "Instead of 'unreleased', so a glance at dev.irsync.org tells whether a merge has been deployed."
  ```
