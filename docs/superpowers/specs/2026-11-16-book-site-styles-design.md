# The book takes the site's styles

## Why

The landing page got its own design in `5ab3e87` ("Give the landing page
its own design"): a cool navy and aqua palette, a yellow "signal" colour,
Barlow headings, a serif for running text. Those values were written
straight into `site/tailwind.css`. The book still gets its look from
`site/tokens.json`, which holds the older warm grey-green palette and system
sans fonts. The two now look like different projects.
`book/src/contributing/website.md` already says the book is meant to take
over the site's design later. This is that change.

The fix should also stop it happening again: one file holds the design, and
both the site and the book read it.

## Goals

- `site/tokens.json` is the one source of the design for both the landing
  page and the book: colours (light and dark), font stacks, type scale,
  radius and the other tokens either one uses.
- The landing page reads its colours and fonts from the CSS generated from
  `tokens.json`, not from values written into `tailwind.css`.
- **The landing page does not change at all.** `index.html` and `404.html`
  render pixel-identical before and after, in light and dark.
- The book looks like the site: same colours, Barlow for headings and UI,
  the serif for running text, monospace for code.
- The book follows the system's light or dark setting by default. Its theme
  picker offers Auto, Light and Dark (see "Theme picker" below).

## Non-goals

- No redesign. The site's design is the target as it stands today; nothing
  on the landing page is improved or tidied in passing.
- No change to the book's content or structure.
- No pixel-identity check in CI. It's a one-off check for this change.

## Anti-goals

- Any visible change to the landing page, however small, is a failure,
  even if the code is cleaner.
- Two copies of the same value (one in `tokens.json`, one in a stylesheet)
  is a failure: that is how they drifted.

## Decisions made by the user

- `tokens.json` becomes the shared source for both.
- `tokens.json` is edited in this repo. The rule that it is a verbatim copy
  of an outside design system's `project/tokens.json` is dropped, and
  `website.md` is updated to say so.
- The book follows the system's light or dark setting. The picker offers
  Auto (follow the system, the default), Light and Dark.
- The book may use a few colours the site doesn't: text-safe yellow and red
  and pale tints (see "Book-only colours").
- The site must be pixel-identical after the change.

## The shape of the change

### `site/tokens.json`

Rewrite it to hold the site's design as found in `site/tailwind.css` today:

- the colours from the `--site-*` block: ground, raised, rule, ink, muted,
  aqua, aqua-ink, on-aqua, signal, on-signal, term, term-ink, term-muted, in
  light and dark. Where the site uses one value in both modes (signal,
  on-signal, term-ink), it is one plain value. The error colour, now written
  inline in the daisyUI theme (`#c8322b` on `#ffffff`), moves in too.
- the four font stacks: display (Barlow), condensed (Barlow Semi
  Condensed), serif, mono.
- the type scale from `@theme`: lede, body, small, label, heading, display
  (sizes, line heights, letter spacing).
- radius values matching what the daisyUI theme uses (2px, 4px).
- the `usage` notes, rewritten to describe the new values.

The old grey-green tokens are removed. Spacing and shadow tokens stay only
if something still uses them (builder's call; see below).

#### Book-only colours

The site's signal yellow and error red only work where the site uses them
(yellow as a fill under dark text). As text, yellow on the light ground is
about 1.5:1, and the red on the dark ground about 3.4:1. The site also has
no pale tints, which the book uses for search matches, footnote highlights
and menu hover. So `tokens.json` gets a few book-only entries, each with
light and dark values and a `usage` note saying the site doesn't use them:

- a warn-text yellow, readable as text (at least 4.5:1) on ground and raised;
- a danger-text red, the same;
- pale tints behind aqua, yellow and red (aqua-soft, warn-soft,
  danger-soft), with their matching text colour readable on them.

Exact values are the builder's call, in the site's hues. The site's own
error colour (`#c8322b`) stays as it is for the site.

### `site/tools/gen_tokens.py`

It already turns `tokens.json` into CSS custom properties. It needs to:

- handle the new token shape (whatever it ends up being);
- write the site's form, where dark comes from
  `@media (prefers-color-scheme: dark)` on `:root`, light being the default;
- keep writing the book's form, keyed on mdbook's theme classes, with the
  system setting deciding when the reader hasn't picked.

Its tests (`site/tools/test_gen_tokens.py`) are updated to match.

### The landing page

`site/build.sh` generates the site's token CSS before running Tailwind.
`tailwind.css` imports it and loses its hand-written `--site-*` block,
`:root` dark override and inline error colours. Everything else in
`tailwind.css` (the daisyUI theme mapping, `@theme`, base styles,
components) keeps working off the generated variables. Whether `@theme`
references the generated variables or is itself generated is the builder's
call, as long as nothing is written down twice.

### The book

- `book/theme/irsync.css` maps mdbook's variables onto the new tokens, as it
  does now with the old ones. Body text in the serif at the site's body
  size; headings, the sidebar, menus and the search bar in Barlow; code in
  monospace.
- Book-only parts with no match on the site (code highlighting, tables,
  search results, sidebar, notes/warnings blockquotes, footnotes) are
  styled from the new palette. Yellow (signal) stays reserved for warnings,
  as on the site. Aqua-ink is the link and accent colour, as on the site.
- Fonts: the book uses the same three Barlow files the site ships in the
  top-level `fonts/` (`@font-face` with a relative URL from the book's CSS).
  No font files are copied into the book. mdbook's own web fonts stay out,
  as now. The book's `@font-face` block is hand-written in its stylesheet
  (builder's call, agreed: these are file paths, not design values, so they
  don't go in `tokens.json`). `check.py`'s "no fonts but Barlow" rule should keep passing
  unchanged; if it needs a tweak, keep its intent (no font files except
  the site's Barlow). A local `mdbook serve` preview has no `fonts/`
  directory and falls back to the next fonts in the stack; that's fine.

### Theme picker

mdbook (v0.5.4 here) shows Auto, Light, Rust, Coal, Navy, Ayu. Auto follows
the system setting and is what a first visit gets.

The picker keeps **Auto, Light and Dark**. Rust, Coal and Ayu are hidden.
The dark theme is shown with the label "Dark". `book.toml` sets
`default-theme` and `preferred-dark-theme` so Auto means "the site's light
or dark, by system setting". A reader who has a stored choice of one of the
removed themes (from an earlier visit) still gets a correct page: every
mdbook theme class maps onto the light or dark tokens, as today.

### Docs

`book/src/contributing/website.md`: drop the "verbatim copy" rule, say
`tokens.json` is the source for both the landing page and the book and is
edited here, and update the build and fonts sections to match.

## Checking the site is unchanged

Before changing anything, capture a baseline from the current `main`:

1. the compiled `target/site/style.css`;
2. full-page screenshots of `index.html` and `404.html`, in light and dark,
   at phone (390px), tablet (768px) and desktop (1280px) widths, using a
   headless browser serving `target/site/` locally, with web fonts loaded
   before the shot.

After the change, build again and repeat. The screenshots must match pixel
for pixel; any difference blocks the change. The `style.css` diff should
show only where values are defined, not different values; read it to
explain any difference. The screenshot script is a throwaway in the
scratchpad, not committed.

## Left to the builder

- Token names and the exact JSON shape, and the generated variable names.
- How `gen_tokens.py` is told which form to write (flags, or two outputs).
- Whether unused spacing and shadow tokens stay.
- The highlight colours and other book-only details, within the palette.
- How Rust/Coal/Ayu are hidden and "Navy" is renamed "Dark" (CSS, or an
  mdbook template override); which mdbook dark theme carries the dark
  tokens.
- The screenshot tool.
- Converting sizes for the book: mdbook sets the root font size to 62.5%
  (10px), so `rem` values from the site's type scale must be converted
  (to px, or rescaled) in the book's CSS.
- Testing: `site/tools` tests, `tools/ci.sh check` is not needed for a
  `site/`- and `book/`-only change, but the site workflow's checks
  (`site/build.sh`, `check.py`) must pass locally.
