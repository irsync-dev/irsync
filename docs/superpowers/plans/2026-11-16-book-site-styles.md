# The book takes the site's styles: Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** `site/tokens.json` holds the landing page's design and both the landing page and the book read it; the landing page renders pixel-identical, the book looks like the site.

**Architecture:** `site/tools/gen_tokens.py` turns `tokens.json` into `--ir-*` CSS custom properties in two forms: one for the site (light on `:root`, dark under `prefers-color-scheme`), one for the book (keyed on mdbook's theme classes). `site/tailwind.css` imports the site form instead of its hand-written `--site-*` block. `book/theme/irsync.css` maps mdbook's variables onto the same tokens and loads the site's Barlow files.

**Tech Stack:** Python 3 (stdlib `unittest`), Tailwind CSS v4 + daisyUI (pinned in `site/package-lock.json`), mdbook v0.5.4, headless Google Chrome for the screenshot check.

**Spec:** `docs/superpowers/specs/2026-11-16-book-site-styles-design.md`

## Global Constraints

- The landing page (`index.html`, `404.html`) must render pixel-identical before and after, in light and dark, at 390, 768 and 1280px wide. Any difference blocks the change.
- No design value is written in two places. Every colour, font stack, type size and radius lives in `site/tokens.json` only.
- `tokens.json` is edited in this repo; it is no longer a copy of the design system's file.
- The book's picker offers exactly Auto, Light, Dark. Auto follows the system and is the default.
- Book-only colours (`warn-ink`, `danger-ink`, `aqua-soft`, `warn-soft`, `danger-soft`) are at least 4.5:1 as text against the grounds they're used on, in both modes.
- No font files are copied into the book. The book loads the three Barlow files from the site's top-level `fonts/`.
- The screenshot script lives in the scratchpad and is never committed. `<scratchpad>` below means `/tmp/claude-1000/-home-mike-work-irsync/b5979e75-3346-427b-80da-37cd0501cec8/scratchpad`.
- Change only `site/`, `book/`, `.gitignore` and `book/src/contributing/website.md`. No Rust changes, so `tools/ci.sh check` is not needed. The site checks are: `python3 -m unittest discover -s site/tools -p 'test_*.py'`, `site/build.sh`, `python3 site/tools/check.py target/site`.
- Commit messages: a short summary line, a blank line, a brief why. No attribution trailers of any kind.

## Review Focus

1. **A reader with a stored old theme** (`mdbook-theme` = `rust`, `coal` or `ayu` in localStorage from an earlier visit) should still get a correct light or dark page, not mdbook's original colours. Test: Task 4, Step 6.
2. **A reader whose browser font size isn't 16px** should get the book at the same relative size as the site. That means the type scale stays in `rem` and the book scales it for mdbook's 62.5% root, not px. Test: Task 4, Step 2 (`test_book_scales_rem_sizes`).
3. **The dark-mode switch in the screenshot check doesn't take effect**, so "identical" only proves light mode. Test: Task 1, Step 3 (light and dark shots must differ).
4. **Web fonts not loaded when the shot is taken**, so the before and after shots both show fallback fonts and hide a font regression. Test: Task 1, Step 3 (the shot waits for `document.fonts.ready`, and a check looks for Barlow in `document.fonts`).
5. **Low-contrast text in the book's notes, warnings and search matches** (the reason for the book-only colours). Test: Task 2, Step 1 (`test_contrast`).

---

## File map

| File | Change |
|---|---|
| `site/tokens.json` | Rewritten: the site's palette, book-only colours, four font stacks, type scale, two radii. |
| `site/tools/gen_tokens.py` | New token shape; site form when no selectors are given, book form with them. |
| `site/tools/test_gen_tokens.py` | Rewritten for the new shape and both forms; adds a contrast test. |
| `site/build.sh` | Generates `site/tokens.css` before Tailwind runs. |
| `.gitignore` | Adds `/site/tokens.css`. |
| `site/tailwind.css` | Imports `tokens.css`; loses the `--site-*` blocks and the inline error colours; `@theme` and daisyUI refer to `--ir-*`. |
| `book/theme/irsync.css` | Rewritten against the new tokens; `@font-face` for Barlow. |
| `book/theme/picker.js` | New: trims the theme picker to Auto, Light, Dark. |
| `book/book.toml` | Adds `additional-js`; sets the light and dark themes. |
| `book/src/contributing/website.md` | Drops the "verbatim copy" rule; describes the shared tokens and fonts. |

---

### Task 1: Baseline of the current site and the screenshot script

No commit. This captures "before" from the branch as it is now (only the design doc changed since `main`).

**Files:**
- Create: `<scratchpad>/shots.py` (not in the repo)

**Interfaces:**
- Produces: `python3 <scratchpad>/shots.py SITE_DIR OUT_DIR` — serves `SITE_DIR` over HTTP on a free local port and writes `OUT_DIR/{index,404}-{light,dark}-{390,768,1280}.png` (12 files). Exits non-zero if any shot fails, if a light/dark pair is identical, or if Barlow is not loaded.
- Produces: `python3 <scratchpad>/shots.py --compare A_DIR B_DIR` — compares every PNG by name, pixel for pixel; prints `identical: 12/12` and exits 0, or names each file that differs and exits 1.
- Produces: `target/site-baseline/` (the built site) and `<scratchpad>/baseline/` (12 PNGs, plus a copy of `style.css`).

- [ ] **Step 1: Build the baseline site**

Run: `site/build.sh target/site-baseline`
Expected: exits 0; `target/site-baseline/style.css` and `target/site-baseline/fonts/barlow-latin-400-normal.woff2` exist.

- [ ] **Step 2: Write `shots.py`**

Drive `/usr/bin/google-chrome --headless=new` through the DevTools protocol (Python stdlib only: `subprocess`, `http.server` in a thread, `urllib`, and a minimal websocket client, or `websocket-client` if it is already installed; no npm or pip installs into the repo). Per shot:
- `Emulation.setDeviceMetricsOverride` with the width, `deviceScaleFactor: 1`, and a height taken from `document.documentElement.scrollHeight` after load (full page);
- `Emulation.setEmulatedMedia` with `features: [{name: "prefers-color-scheme", value: "light"|"dark"}]`;
- wait for `document.fonts.ready`, then check `[...document.fonts].some(f => f.family.includes("Barlow") && f.status === "loaded")`; fail if false;
- `Page.captureScreenshot` with `captureBeyondViewport: true`, format png.

Compare with raw PNG bytes after decoding (stdlib `zlib` decode of IDAT is fine) or with Pillow if installed; byte-equal PNG files also count as identical.

- [ ] **Step 3: Capture the baseline and check the script works**

Run: `python3 <scratchpad>/shots.py target/site-baseline <scratchpad>/baseline && cp target/site-baseline/style.css <scratchpad>/baseline/`
Expected: 12 PNGs. The script itself has already failed if any light/dark pair is the same or Barlow didn't load. Open one light and one dark desktop shot and look at them: both themes and Barlow headings should be visible.

- [ ] **Step 4: Check the comparison catches a difference**

Run: `python3 <scratchpad>/shots.py --compare <scratchpad>/baseline <scratchpad>/baseline`
Expected: `identical: 12/12`.
Then copy the baseline to `<scratchpad>/tamper/`, overwrite `index-light-390.png` with `index-dark-390.png`, and compare again.
Expected: exit 1, naming `index-light-390.png`.

---

### Task 2: New tokens and generator

After this task the book build still runs but looks wrong, because `irsync.css` still uses the old names. Task 4 fixes that. The site doesn't read the tokens yet.

**Files:**
- Modify: `site/tokens.json` (full rewrite)
- Modify: `site/tools/gen_tokens.py`
- Test: `site/tools/test_gen_tokens.py` (full rewrite)

**Interfaces:**
- Produces: `tokens_css(tokens: dict, light: str | None = None, dark: str | None = None) -> str` in `site/tools/gen_tokens.py`.
  - Both `None` (site form): `:root { <light colours>; <plain tokens> }`, then `@media (prefers-color-scheme: dark) { :root { <dark colours> } }`.
  - Both given (book form): `:root, <light> { <light colours>; <plain tokens> }`, then `<dark> { <dark colours> }`, then `@media (prefers-color-scheme: dark) { :root:not(<light>) { <dark colours> } }` (no-JS fallback).
  - Theme values are looked up by id `"light"` / `"dark"`, not by position.
  - Giving only one of `light` / `dark` raises `ValueError`.
- Produces: CLI `gen_tokens.py TOKENS -o OUT [--light SEL --dark SEL]`.
- Produces: these custom properties, which Tasks 3 and 4 use:
  - colours: `--ir-ground`, `--ir-raised`, `--ir-rule`, `--ir-ink`, `--ir-muted`, `--ir-aqua`, `--ir-aqua-ink`, `--ir-on-aqua`, `--ir-signal`, `--ir-on-signal`, `--ir-term`, `--ir-term-ink`, `--ir-term-muted`, `--ir-error`, `--ir-on-error`; book-only `--ir-warn-ink`, `--ir-danger-ink`, `--ir-aqua-soft`, `--ir-warn-soft`, `--ir-danger-soft`
  - fonts: `--ir-font-display`, `--ir-font-condensed`, `--ir-font-serif`, `--ir-font-mono`
  - type: `--ir-text-<name>`, `--ir-text-<name>-line`, and `--ir-text-<name>-tracking` where given, for `lede`, `body`, `small`, `label`, `heading`, `display`
  - radius: `--ir-radius-sm`, `--ir-radius-md`

**`tokens.json` shape.** Keep the top-level layout (`name`, `version` bumped to 2, `color.themes`, `color.tokens`, `type`, `radius`), drop `spacing` and `shadow`. Each colour token is `{name, value, usage}`, where `value` is a string (same in both modes) or `{light, dark}`. Book-only tokens say "Book only; the landing page does not use it." in `usage`. `type.families` holds the four stacks. `type.scale` is a list of `{name, size, lineHeight, letterSpacing?, usage}`. The `{alias}` syntax and `_resolve` can go: nothing uses it now.

Values, copied from `site/tailwind.css` as it stands:

| token | light | dark |
|---|---|---|
| ground | `#f2f5f6` | `#0e1920` |
| raised | `#ffffff` | `#15242d` |
| rule | `#ccd6db` | `#263943` |
| ink | `#13222c` | `#e2ebee` |
| muted | `#4f616c` | `#96a9b3` |
| aqua | `#00a6b4` | `#2cc4d1` |
| aqua-ink | `#00707b` | `#5ed6e0` |
| on-aqua | `#ffffff` | `#0e1920` |
| signal | `#f6c700` (both) | |
| on-signal | `#13222c` (both) | |
| term | `#13222c` | `#081116` |
| term-ink | `#dde7eb` (both) | |
| term-muted | `#8da3ae` | `#7f949f` |
| error | `#c8322b` (both) | |
| on-error | `#ffffff` (both) | |

Book-only values are yours to pick, in the site's hues (yellow near signal's hue, red near error's, aqua near aqua's), so that `test_contrast` passes.

Font stacks, verbatim from `@theme`:
- display: `Barlow, Bahnschrift, "DIN Alternate", "Franklin Gothic Medium", sans-serif`
- condensed: `"Barlow Semi Condensed", Bahnschrift, "DIN Alternate", "Arial Narrow", sans-serif`
- serif: `Charter, "Bitstream Charter", "Sitka Text", Cambria, serif`
- mono: `ui-monospace, "SF Mono", Menlo, Consolas, "DejaVu Sans Mono", monospace`

Type scale, verbatim: lede `1.3125rem`/`1.5`; body `1.0625rem`/`1.6`; small `0.9375rem`/`1.55`; label `1rem`/`1.25`; heading `1.75rem`/`1.15`; display `clamp(2rem, 4vw, 3rem)`/`1.05`/tracking `-0.01em`.

Radius: `sm` `2px`, `md` `4px`.

- [ ] **Step 1: Write the failing tests**

Replace `site/tools/test_gen_tokens.py`. Tests (load the real `tokens.json`, as now):

```python
def test_site_form_light_on_root(self):
    css = tokens_css(TOKENS)
    root = css[css.index(":root {"):].split("}")[0]
    self.assertIn("--ir-ground: #f2f5f6;", root)
    self.assertIn("--ir-signal: #f6c700;", root)          # plain value, in the root block
    self.assertIn("--ir-font-display: Barlow, Bahnschrift,", root)
    self.assertIn("--ir-text-body: 1.0625rem;", root)
    self.assertIn("--ir-text-display: clamp(2rem, 4vw, 3rem);", root)
    self.assertIn("--ir-text-display-tracking: -0.01em;", root)
    self.assertIn("--ir-radius-md: 4px;", root)

def test_site_form_dark_under_media_query(self):
    css = tokens_css(TOKENS)
    media = css[css.index("@media (prefers-color-scheme: dark)"):]
    self.assertIn(":root {", media)
    self.assertIn("--ir-ground: #0e1920;", media)
    self.assertNotIn("--ir-signal", media)                # plain tokens appear once
    self.assertNotIn("html.", css)

def test_book_form(self):
    css = tokens_css(TOKENS, "html.light, html.rust", "html.coal, html.navy, html.ayu")
    first = css[css.index(":root, html.light, html.rust {"):].split("}")[0]
    self.assertIn("--ir-ground: #f2f5f6;", first)
    dark = css[css.index("html.coal, html.navy, html.ayu {"):].split("}")[0]
    self.assertIn("--ir-ground: #0e1920;", dark)
    self.assertIn(":root:not(html.light, html.rust)", css)

def test_one_selector_only_is_an_error(self):
    with self.assertRaises(ValueError):
        tokens_css(TOKENS, "html.light", None)

def test_every_value_matches_tailwind_today(self):
    # the 15 site colours, both modes, exactly as in the table above
    ...

def test_contrast(self):
    # WCAG relative-luminance contrast, both modes, each pair >= 4.5:
    # ink, muted, aqua-ink, warn-ink, danger-ink on ground and on raised;
    # aqua-ink on aqua-soft; warn-ink on warn-soft; danger-ink on danger-soft;
    # ink on aqua-soft, warn-soft and danger-soft (search marks, footnotes, hover).
    ...
```

`test_every_value_matches_tailwind_today` holds the table above as a literal dict, so a later edit to `tokens.json` that would change the site fails loudly. The contrast helper goes in the test file; the generator doesn't need it.

- [ ] **Step 2: Run the tests and see them fail**

Run: `python3 -m unittest discover -s site/tools -p 'test_gen_tokens.py' -v`
Expected: FAIL: `KeyError`/missing `--ir-ground`, and `tokens_css()` missing arguments.

- [ ] **Step 3: Rewrite `site/tokens.json`** with the shape and values above.

- [ ] **Step 4: Update `gen_tokens.py`** to the `tokens_css` signature and CLI above. Keep `HEADER` and `_block`.

- [ ] **Step 5: Run all site tool tests**

Run: `python3 -m unittest discover -s site/tools -p 'test_*.py'`
Expected: OK. If `test_contrast` fails on a site colour (not a book-only one), stop and report it: site colours can't change. (Checked while planning: ink, muted and aqua-ink pass on ground and raised in both modes, lowest 5.32.)

- [ ] **Step 6: Commit**

```bash
git add site/tokens.json site/tools/gen_tokens.py site/tools/test_gen_tokens.py
git commit -m "Put the landing page's design in tokens.json

tokens.json held an older palette that only the book used. It now holds
the site's colours, fonts and type scale, plus a few book-only colours,
so both can read one source."
```

---

### Task 3: The landing page reads the generated tokens

**Files:**
- Modify: `site/build.sh`
- Modify: `site/tailwind.css`
- Modify: `.gitignore`

**Interfaces:**
- Consumes: `gen_tokens.py` CLI and the `--ir-*` names from Task 2.
- Produces: `site/tokens.css` (generated, git-ignored), imported by `tailwind.css`.

- [ ] **Step 1: Generate the site form in `build.sh`**

Next to the existing book call: `tokens -o "$root/site/tokens.css"` (no selectors). The book call becomes `tokens --light "html.light, html.rust" --dark "html.coal, html.navy, html.ayu" -o "$root/book/theme/tokens.css"`. Both run before Tailwind and mdbook. Add `/site/tokens.css` to `.gitignore` next to `/book/theme/tokens.css`.

- [ ] **Step 2: Rewire `tailwind.css`**

- Add `@import "./tokens.css";` (where Tailwind accepts it: next to `@import "tailwindcss" source(none);`).
- Delete the `:root { color-scheme…; --site-* }` block and its `@media (prefers-color-scheme: dark)` block, except `color-scheme: light dark;`, which stays in a small `:root` rule (it isn't a token).
- In the daisyUI theme: `var(--site-X)` → `var(--ir-X)` (`site-ground` → `ir-ground`, and so on); `--color-error: var(--ir-error)`; `--color-error-content: var(--ir-on-error)`; `--radius-selector` and `--radius-field: var(--ir-radius-sm)`; `--radius-box: var(--ir-radius-md)`.
- In `@theme`: the four `--font-*` and every `--text-*` (size, `--line-height`, `--letter-spacing`) become `var(--ir-…)`; the `--color-*` lines refer to `--ir-*`.
- In `:focus-visible`: `var(--site-aqua-ink)` → `var(--ir-aqua-ink)`.
- Update the header comment: the palette, fonts and type scale come from `tokens.json`; the description of the look stays.

If Tailwind or daisyUI needs a literal value somewhere (for example a plugin that computes from the colour), keep the `var()` out of that spot and say so in the commit message; the pixel check decides.

- [ ] **Step 3: Check no design value is left in `tailwind.css`**

Run: `grep -nE '#[0-9a-fA-F]{3,8}\b|[0-9.]+rem|Barlow,|Charter|ui-monospace' site/tailwind.css`
Expected: only the `@font-face` blocks (font family names and file URLs) and the header comment.

- [ ] **Step 4: Build and compare**

Run: `site/build.sh && python3 <scratchpad>/shots.py target/site <scratchpad>/after && python3 <scratchpad>/shots.py --compare <scratchpad>/baseline <scratchpad>/after`
Expected: `identical: 12/12`.

- [ ] **Step 5: Read the CSS diff**

Run: `diff <(tr ';{}' '\n\n\n' < <scratchpad>/baseline/style.css) <(tr ';{}' '\n\n\n' < target/site/style.css)`
Expected: differences are only renamed variables, `var()` indirections and where the custom properties are declared. Note any other difference in the commit message with why it doesn't change rendering. If the screenshots differ, find the cause before going on; don't adjust the baseline.

- [ ] **Step 6: Run the site checks**

Run: `python3 site/tools/check.py target/site`
Expected: no output, exit 0.

- [ ] **Step 7: Commit**

```bash
git add .gitignore site/build.sh site/tailwind.css
git commit -m "Build the landing page's colours and fonts from tokens.json

The values were written straight into tailwind.css, which is how the
book fell behind. Screenshots of both pages, light and dark, at three
widths are pixel-identical before and after."
```

---

### Task 4: The book's stylesheet, fonts and picker

**Files:**
- Modify: `book/theme/irsync.css` (rewrite)
- Create: `book/theme/picker.js`
- Modify: `book/book.toml`
- Test: `site/tools/test_book_theme.py` (new)

**Interfaces:**
- Consumes: the `--ir-*` names from Task 2; the book form of `tokens.css` from Task 3's `build.sh`.
- Produces: nothing other tasks use.

**Styling decisions** (spec, plus calls made while planning):
- mdbook's root font size is 62.5%, so in the book `1rem` = 10px. Declare `--ir-book-rem: 1.6` on `:root` in `irsync.css` and size text as `calc(var(--ir-text-body) * var(--ir-book-rem))`. Don't convert to px: readers' font-size settings must keep working.
- Body: serif, `text-body` size and line height. `h1`: display font, `text-heading` size ×1.6 (the site's `display` clamp is for one headline, too big for every page title); `h2` uses `text-lede` ×1.6, `h3` `text-label` ×1.6, both display font, semibold. Sidebar, menu bar, search bar and picker: display font. Inline and block code: mono at 0.875em, like the site.
- Colours: `--bg` ground; `--fg` ink; sidebar raised; `--links` and `--sidebar-active` aqua-ink; rules, table borders and header rule; `--quote-bg` raised; `--theme-hover`, `--search-mark-bg`, `--footnote-highlight` aqua-soft; note/tip blockquote colour aqua-ink; important muted; warning warn-ink; caution danger-ink. Highlighting: keywords aqua-ink, strings and literals warn-ink, comments muted italic, deletions danger-ink, the rest ink.
- Links underlined, 1px, offset 3px, 2px on hover (as on the site). Focus: `outline: 2px solid var(--ir-aqua-ink); outline-offset: 3px` (as on the site).
- Keep from the current file: the light/dark `--color-scheme` mapping, the scrolling `pre` and tables, the highlight-specificity approach, `!important` on code fonts.
- Fonts: three `@font-face` rules (same family names, weights and `font-display: fallback` as `tailwind.css`) with `src: url("../../fonts/<file>.woff2")`. The built CSS sits at `book/theme/irsync-<hash>.css`, so `../../fonts/` is the site's `fonts/`. Say so in a comment.
- Picker: `book.toml` gets `additional-js = ["theme/picker.js"]`; `default-theme = "light"` and `preferred-dark-theme = "navy"` stay. `picker.js` removes the `<li>` holding `#mdbook-theme-rust`, `#mdbook-theme-coal` and `#mdbook-theme-ayu`, and sets `#mdbook-theme-navy`'s text to `Dark`. A stored `rust`/`coal`/`ayu` keeps working through the token selectors; no migration.

- [ ] **Step 1: Write the failing tests**

`site/tools/test_book_theme.py`, reading the source files (no build needed):

```python
def test_no_old_token_names(self):
    # irsync.css uses no --ir-surface, --ir-accent, --ir-warn(?!-), --ir-danger(?!-),
    # --ir-line, --ir-space-, --ir-focus-ring, --ir-font-sans
def test_every_ir_var_is_defined(self):
    # every var(--ir-X) in irsync.css is either declared in irsync.css
    # (--ir-book-rem) or produced by tokens_css(TOKENS, "a", "b")
def test_book_scales_rem_sizes(self):
    # every var(--ir-text-*) that isn't a -line or -tracking appears inside
    # calc(... * var(--ir-book-rem))
def test_barlow_from_site_fonts(self):
    # three url("../../fonts/barlow…woff2") and no other url()
def test_picker_script_is_loaded(self):
    # book.toml additional-js contains "theme/picker.js"; picker.js mentions
    # mdbook-theme-rust, -coal, -ayu, -navy and "Dark"
```

- [ ] **Step 2: Run them and see them fail**

Run: `python3 -m unittest discover -s site/tools -p 'test_book_theme.py' -v`
Expected: FAIL on old names, missing `--ir-book-rem`, missing fonts and picker.

- [ ] **Step 3: Rewrite `irsync.css`, add `picker.js`, update `book.toml`** per the decisions above. Update `irsync.css`'s header comment (mdbook v0.5.4).

- [ ] **Step 4: Run the tests**

Run: `python3 -m unittest discover -s site/tools -p 'test_*.py'`
Expected: OK.

- [ ] **Step 5: Build, check, and confirm the site is still identical**

Run: `site/build.sh && python3 site/tools/check.py target/site && python3 <scratchpad>/shots.py target/site <scratchpad>/after && python3 <scratchpad>/shots.py --compare <scratchpad>/baseline <scratchpad>/after`
Expected: `check.py` silent; `identical: 12/12`.

- [ ] **Step 6: Look at the book**

Take shots (reuse `shots.py`'s helpers; a `--pages` option or a copy of the script is fine) of `book/introduction.html`, `book/reference/configuration.html` (tables, code), and a page with a warning or caution note (`grep -l 'blockquote-tag-warning\|blockquote-tag-caution' target/site/book/**/*.html`), at 390 and 1280, in four states: Auto+light system, Auto+dark system, and with localStorage `mdbook-theme` set to `rust` and to `ayu`.
Expected: Barlow headings, serif body, site colours. The `rust` shot matches the light look, `ayu` the dark look. The open picker shows Auto, Light, Dark only. Save the shots in `<scratchpad>/book/` for the user to look at.

- [ ] **Step 7: Commit**

```bash
git add book/theme/irsync.css book/theme/picker.js book/book.toml site/tools/test_book_theme.py
git commit -m "Give the book the landing page's look

Same colours, Barlow headings and serif text as the site, from the same
tokens. The theme picker offers Auto, Light and Dark only; Auto follows
the system setting."
```

---

### Task 5: Contributor docs

**Files:**
- Modify: `book/src/contributing/website.md`

- [ ] **Step 1: Update the page**

- The file table row for `site/tokens.json`: the design of the landing page and the book.
- "Updating the design tokens": `tokens.json` is edited here (drop "verbatim copy" and "do not edit it by hand"). The build writes two CSS files from it: `site/tokens.css` (imported by `tailwind.css`) and `book/theme/tokens.css`, both git-ignored. Every colour, font stack, type size and radius on both comes from it. Some colours are book-only. `test_gen_tokens.py` pins the site's values, so changing a site colour means changing that test too, on purpose. Drop "The landing page no longer uses these tokens…".
- "The landing page design": where it lists colours and fonts, say they come from `tokens.json`.
- Building/fonts: the book loads the site's Barlow files from `fonts/`; no font files are in the book; `mdbook serve` on its own falls back to system fonts. The "No fonts but Barlow" bullet in the checker section stays true; reword if it says the book uses system fonts.
- The theme picker: Auto, Light, Dark, trimmed by `book/theme/picker.js`.

- [ ] **Step 2: Build and check**

Run: `site/build.sh && python3 site/tools/check.py target/site`
Expected: silent, exit 0 (the page's links and anchors still resolve).

- [ ] **Step 3: Commit**

```bash
git add book/src/contributing/website.md
git commit -m "Describe tokens.json as the shared source of the design"
```

---

### Task 6: Final check

- [ ] **Step 1: Everything the site workflow runs, from clean**

Run: `rm -rf target/site site/tokens.css book/theme/tokens.css && python3 -m unittest discover -s site/tools -p 'test_*.py' && site/build.sh && python3 site/tools/check.py target/site`
Expected: tests OK, build OK, check silent. (Tests must not depend on generated files.)

- [ ] **Step 2: The pixel check, one last time**

Run: `python3 <scratchpad>/shots.py target/site <scratchpad>/final && python3 <scratchpad>/shots.py --compare <scratchpad>/baseline <scratchpad>/final`
Expected: `identical: 12/12`.

- [ ] **Step 3: Report** the result of both, and point the user at `<scratchpad>/book/` for the book shots.
