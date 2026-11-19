# Book Version Switcher Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Every book on the site has a button in its top bar that switches to the same page in another release channel's book, and the book's title becomes `irsync Documentation`.

**Architecture:** mdbook builds the five books as today. Then a new tool, `site/tools/switcher.py`, reads the build's `VERSIONS` and writes the switcher's markup into every built page that has the top bar, each link worked out at build time (same page if the other book has that file, else its `index.html`). A small script in the theme, `book/theme/switcher.js`, opens and closes the list and keeps the `#anchor`. The title change is in `book/book.toml`; `site/tools/label_book.py` stops touching the title.

**Tech Stack:** Python 3 standard library and `unittest`, POSIX sh, mdbook 0.5.2, plain JavaScript and CSS in the book theme, headless Chrome for the browser check.

**Spec:** `docs/superpowers/specs/2026-11-19-book-version-switcher-design.md`. Read it first. It builds on `docs/superpowers/specs/2026-11-19-docs-per-channel-design.md`.

## Global Constraints

- Branch `book-version-switcher` from an up-to-date `main`; its first commit holds the spec and this plan. One PR against `main` with `Closes #123`. No attribution trailers in commits or the PR.
- Commit messages: a short summary line, a blank line, then a brief plain-English reason if it isn't obvious.
- Title, exact: `irsync Documentation`.
- The list's order: `stable`, `beta`, `alpha`, `dev`; only paths present in `VERSIONS`. `book` is never an entry.
- Labels, exact: `Stable`, `Beta`, `Alpha`, `Dev`. Dev's detail text, where the others show their version: `unreleased`.
- In `/book/` the ticked entry is the channel of the book's version (`versions.channel(version)`), or `dev` when the `book` line has no version.
- Links are relative to the page. Never an absolute `/path`.
- `book/` stays plain mdbook: `mdbook build book` works and shows no switcher. Nothing under `book/` is written by the build.
- PR CI does not run the site tool tests or `check.py`. Before every commit that touches `site/` or `book/theme/`: `python3 -m unittest discover -s site/tools -p 'test_*.py'`. Before the push also `site/build.sh` and `python3 site/tools/check.py --irsync target/debug/irsync target/site`.

## Review Focus

1. A page the other book lacks (an older release without a newer chapter): the link opens that book's `index.html`, and no `#anchor` is added. Pinned in Task 2 (`test_missing_page_goes_to_the_title_page`) and the Task 3 browser check.
2. The "not found" page (`404.html`, served at any address under `<base href="/PATH/">`): every entry opens the other book's `index.html`, and the links resolve under the base. Pinned in Task 2 (`test_not_found_page_goes_to_title_pages`).
3. Pages in subdirectories (`tutorial/first-sync.html`): the `../` depth is right from both a channel book and `/book/`. Pinned in Task 2 (`test_page_in_a_subdirectory`) and by `check.py` over the real build.
4. `/book/` ticks the channel it copies, and Dev when there is no release. Pinned in Task 2 (`test_ticked`).
5. A plain `mdbook build book` (no `VERSIONS`, no post-processing): no button, no script error. Pinned in the Task 3 browser check.

---

### Task 1: The title is `irsync Documentation`

**Files:**
- Modify: `book/book.toml` (`title`)
- Modify: `site/tools/label_book.py` (`_settings`, module docstring)
- Test: `site/tools/test_label_book.py`
- Modify: `book/src/contributing/website.md` ("The books": the first bullet under `label_book.py`; "The book's look": `("irsync")` after "The book's title in the menu bar")

**Interfaces:**
- Produces: `label_book.prepare(work, path, version)` unchanged in signature; it no longer reads or writes the `title` line.

- [ ] **Step 1: Change the tests**

In `test_label_book.py`:
- `test_released` and `test_dev`: replace the title assertions with `self.assertIn('title = "irsync Documentation"\n', self.toml())`.
- `test_rest_of_the_settings_is_untouched`: expect `changed == ['site-url = "/book/"']` for `prepare(self.work, "dev", None)`.
- `test_refuses`: after the refused calls assert `self.assertIn('site-url = "/book/"\n', self.toml())` (nothing written).
- `test_setting_missing_from_book_toml`: write `'[book]\ntitle = "irsync Documentation"\n'`, expect `BookError` matching `"site-url"`.
- `test_cli`, `test_cli_dash_is_no_version`: assert `'site-url = "/alpha/"'` and `'site-url = "/dev/"'`.
- New `test_title_is_left_alone`: write a `book.toml` whose title is `title = "Something else"` plus the real `site-url` and `edit-url-template` lines; `prepare(self.work, "beta", "0.1.0-beta.5")` succeeds and the title line is unchanged.

- [ ] **Step 2: Run, watch them fail**

Run: `cd site/tools && python3 -m unittest test_label_book -v`
Expected: FAIL (the title is still rewritten; the setup copies today's `book/book.toml` with `title = "irsync"`).

- [ ] **Step 3: Implement**

`book/book.toml`: `title = "irsync Documentation"`. `label_book._settings(text, path, version)`: drop the title lookup and rewrite; keep `site-url` and `edit-url-template`. Update the module docstring's list (the title bullet goes).

- [ ] **Step 4: Run all site tool tests**

Run: `python3 -m unittest discover -s site/tools -p 'test_*.py'`
Expected: OK.

- [ ] **Step 5: Docs and commit**

`website.md`: the `label_book.py` list loses the title bullet; "The book's look" names the title as `irsync Documentation`.

```bash
git add book/book.toml site/tools/label_book.py site/tools/test_label_book.py book/src/contributing/website.md
git commit -m "Call the book irsync Documentation

The version moves out of the title into the switcher (#123), so the
title no longer changes per book."
```

### Task 2: `switcher.py` writes the menu into the built pages

**Files:**
- Create: `site/tools/switcher.py`
- Test: `site/tools/test_switcher.py`

**Interfaces:**
- Consumes: `versions.unpublished(text)` (validates a `VERSIONS` text, raises `versions.VersionsError`), `versions.channel(version)`.
- Produces:
  - `class SwitcherError(Exception)`
  - `Entry = namedtuple("Entry", "path label version")`; `version` is `None` for dev.
  - `entries(versions_text: str) -> list[Entry]`
  - `ticked(versions_text: str, path: str) -> str`: the entry path ticked in book `path`.
  - `menu(entries: list[Entry], here: str, page: str, pages_of: dict[str, set[str]]) -> str`: the markup for one page. `page` is the page's path inside its book (posix, e.g. `tutorial/first-sync.html`); `pages_of` maps an entry path to the set of HTML files of that book.
  - `insert(html: str, menu_html: str) -> str`
  - `apply(site_dir: Path) -> None`: reads `site_dir/VERSIONS`, rewrites every page of every book listed there.
  - CLI: `switcher.py SITE_DIR`; exit 1 with `switcher.py: MESSAGE` on stderr on any `SwitcherError`.
  - Markup ids (Task 3's script and styles rely on them): button `id="irsync-book-toggle"` with `aria-haspopup="true"`, `aria-expanded="false"`, `aria-controls="irsync-book-list"`, text = ticked entry's label; list `<ul id="irsync-book-list" role="menu">`; each entry `<li role="none"><a role="menuitem" href="…">LABEL <span class="irsync-book-version">VERSION-or-unreleased</span></a></li>`; the ticked entry's `<a>` has `aria-current="true"`; an entry that keeps the page has the attribute `data-same-page`.

Fixtures used below (`SHA = "b" * 40`):

```python
TODAY = (f"book 0.1.0-beta.5 {SHA} published\nbeta 0.1.0-beta.5 {SHA} published\n"
         f"alpha 0.1.0-alpha.4 {SHA} published\ndev - {SHA} main\n")
NO_RELEASE = f"book - {SHA} main\ndev - {SHA} main\n"
```

- [ ] **Step 1: Write the failing tests**

- `test_entries`: `entries(TODAY) == [Entry("beta", "Beta", "0.1.0-beta.5"), Entry("alpha", "Alpha", "0.1.0-alpha.4"), Entry("dev", "Dev", None)]`; with a `stable 1.0.0 …` line added after `book`, `Stable` comes first; `entries(NO_RELEASE) == [Entry("dev", "Dev", None)]`.
- `test_ticked`: `ticked(TODAY, "book") == "beta"`, `ticked(TODAY, "alpha") == "alpha"`, `ticked(TODAY, "dev") == "dev"`, `ticked(NO_RELEASE, "book") == "dev"`.
- `test_bad_versions_file`: `entries("nonsense\n")` raises `SwitcherError`.
- `test_page_in_a_subdirectory`: `menu(entries(TODAY), "beta", "tutorial/first-sync.html", {"beta": {"index.html", "tutorial/first-sync.html"}, "alpha": {"index.html", "tutorial/first-sync.html"}, "dev": {"index.html", "tutorial/first-sync.html"}})` contains `href="../../alpha/tutorial/first-sync.html"` and `href="../../dev/tutorial/first-sync.html"` and `href="../../beta/tutorial/first-sync.html"`; three `data-same-page`; the button reads `Beta`; the beta `<a>` carries `aria-current="true"`; the dev entry shows `unreleased`.
- `test_missing_page_goes_to_the_title_page`: same call with `"alpha": {"index.html"}`: alpha's href is `../../alpha/index.html` and that `<a>` has no `data-same-page`.
- `test_not_found_page_goes_to_title_pages`: `menu(entries(TODAY), "beta", "404.html", {p: {"index.html", "404.html"} for p in ("beta", "alpha", "dev")})`: hrefs `../beta/index.html`, `../alpha/index.html`, `../dev/index.html`; no `data-same-page`.
- `test_insert`: `insert('<div class="right-buttons">\n<a href="print.html">', "<M>")` puts `<M>` right after the opening tag; a page without `class="right-buttons"` comes back unchanged; a page with two raises `SwitcherError`.
- `test_apply_over_a_small_site`: in a temp dir write `VERSIONS` = `TODAY`, and for each of `book`, `beta`, `alpha`, `dev` an `index.html`, `404.html` and `toc.html` (the last without a top bar); `beta`, `dev` and `book` also get `tutorial/first-sync.html`. Each page with a top bar is `<div class="right-buttons"></div>`. After `apply(dir)`: `book/tutorial/first-sync.html` links `../../alpha/index.html` and `../../beta/tutorial/first-sync.html`, with `Beta` ticked; `toc.html` files are unchanged; `check.check_links(dir)` returns `[]`.
- `test_cli_error`: running `switcher.py` on a dir without `VERSIONS` exits 1 and stderr starts with `switcher.py: `.

- [ ] **Step 2: Run, watch them fail**

Run: `cd site/tools && python3 -m unittest test_switcher -v`
Expected: ERROR, `No module named 'switcher'`.

- [ ] **Step 3: Implement `site/tools/switcher.py`**

Validate the text with `versions.unpublished` and turn `VersionsError` into `SwitcherError`. A link from page `page` is `"../" * (page.count("/") + 1) + f"{target}/{same or 'index.html'}"`, where `same` is `page` if `page != "404.html"` and `page in pages_of[target]`. `pages_of` in `apply` comes from `rglob("*.html")` of each book's output dir. Escape text with `html.escape`. Read and write UTF-8.

- [ ] **Step 4: Run, watch them pass, then all site tool tests**

Run: `cd site/tools && python3 -m unittest test_switcher -v && cd ../.. && python3 -m unittest discover -s site/tools -p 'test_*.py'`
Expected: OK.

- [ ] **Step 5: Commit**

```bash
git add site/tools/switcher.py site/tools/test_switcher.py
git commit -m "Add switcher.py: the version menu for the built books"
```

### Task 3: The button in the books, and the build runs the tool

**Files:**
- Create: `book/theme/switcher.js`
- Modify: `book/book.toml` (`additional-js`: append `"theme/switcher.js"`)
- Modify: `book/theme/irsync.css` (button and list)
- Modify: `site/build.sh` (after the books loop: `python3 "$root/site/tools/switcher.py" "$out"`)
- Test: `site/tools/test_book_theme.py` (`test_switcher_script_is_loaded`, like `test_picker_script_is_loaded`)
- Modify: `book/src/contributing/website.md` ("The books" and "The book's look")

**Interfaces:**
- Consumes: the markup ids and attributes from Task 2.

- [ ] **Step 1: Failing test**

`test_switcher_script_is_loaded`: `book.toml`'s `additional-js` lists `theme/switcher.js` and the file exists.

Run: `cd site/tools && python3 -m unittest test_book_theme -v`
Expected: FAIL.

- [ ] **Step 2: Implement**

- `switcher.js`: if there is no `#irsync-book-toggle`, return. The button toggles the list and `aria-expanded`. Escape closes and puts focus back on the button; a click outside closes. On a click on an entry with `data-same-page`, add `location.hash` to its address.
- `irsync.css`: the button looks like the top bar's other controls (Barlow, the bar's text colour, a small down arrow). The list takes the theme list's colours (`--theme-popup-bg`, `--theme-popup-border`), opens under the button aligned to its right edge, shows the version in the muted colour, marks the ticked entry like the theme list's selected one. Hidden until opened. At 375 px wide the bar still shows the title.
- `build.sh`: the tool runs once after the loop; a failure stops the build (`set -e` already).

- [ ] **Step 3: Tests, build, check**

Run: `python3 -m unittest discover -s site/tools -p 'test_*.py' && site/build.sh && python3 site/tools/check.py --irsync target/debug/irsync target/site`
Expected: OK, the build succeeds, the check prints nothing and exits 0.

- [ ] **Step 4: Browser check (headless Chrome)**

Serve `target/site/` locally. Check, and note the result:
- `/beta/tutorial/first-sync.html#<an id on that page>`: the button reads `Beta`; clicking opens the list with Beta ticked; Escape closes it; the Dev entry leads to `/dev/tutorial/first-sync.html#<that id>`.
- `/book/` shows `Beta` on the button.
- `/beta/no-such-page.html` (the 404 page): every entry leads to a title page.
- Light and dark, and 375 px wide: screenshots; the title is not pushed off the bar.
- `mdbook build book -d target/plain-book`: its pages have no `irsync-book-toggle` and load without a script error.

- [ ] **Step 5: Docs and commit**

`website.md`: "The books" says the top bar has a switcher (what it lists, `/book/` not listed but its channel ticked, same page or title page) and that `site/tools/switcher.py` adds it after mdbook; "The book's look" describes the button.

```bash
git add book/theme/switcher.js book/book.toml book/theme/irsync.css site/build.sh site/tools/test_book_theme.py book/src/contributing/website.md
git commit -m "Switch between the books from the top bar

Closes the gap between the per-channel books: a reader can reach the
same page in another channel's book in one click."
```

### Task 4: Pull request

- [ ] Run the full set once more: site tool tests, `site/build.sh`, `check.py`.
- [ ] Push `book-version-switcher`, open the PR "Switch between documentation versions from the book header" with a short description and `Closes #123`.
