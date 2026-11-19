# A version switcher in the book's top bar

Issue: #123. Builds on the books per release channel (#105,
`2026-11-19-docs-per-channel-design.md`).

## Why

The site holds the documentation once per release channel: `/book/`,
`/stable/`, `/beta/`, `/alpha/` and `/dev/`. A reader in one book has no
way to reach the others except going back to the landing page or editing
the address. Someone on a beta who lands in `/book/` (the stable book, once
there is one) should get to the beta book in one click, and stay on the
page they were reading.

## What the reader gets

A button on the right of the top bar of every book, before the print and
GitHub buttons. It shows the channel of the book the reader is in, and
opens a list:

```
☰ 🖌 🔍        irsync Documentation        Beta ▾  🖨 GitHub
                                        ┌──────────────────────┐
                                        │ Beta  0.1.0-beta.5 ✓ │
                                        │ Alpha 0.1.0-alpha.4  │
                                        │ Dev   unreleased     │
                                        └──────────────────────┘
```

- **The list:** Stable, Beta, Alpha, each with its exact version, then
  Dev. Only the channels that have a book are listed; today there is no
  stable. Dev shows `unreleased` where the others show a version.
- **`/book/` is not listed.** It is a copy of one of the channel books (the
  main version's). A reader in `/book/` sees that channel on the button and
  ticked in the list. With no release at all, `/book/` is the working tree:
  then Dev.
- **The current book is ticked**, and its name is on the button.
- **Switching keeps the page.** Choosing an entry opens the same page in
  that book. If that book does not have the page (an older release may lack
  a newer page), it opens that book's title page instead.
- **The title** in the top bar becomes `irsync Documentation` in every
  book, with no version: the button and the title page say which version
  it is. The homepage already links to the book as "Documentation". The
  title still links to the landing page (`book/theme/home-link.js`). It is
  also in the browser tab: "Your first sync - irsync Documentation".
- **The title page** keeps its sentence naming the exact version (#105).

## Approaches

1. **Everything written in at build time (chosen).** The build already
   builds all five books together. It writes into each book the list of
   books and the pages each one has. The switcher works from that alone:
   no requests, nothing that depends on the server, testable in Python
   like the rest of the site tools. Costs a page list in each book (about
   50 pages per book, a few KB).
2. **The browser asks.** The page fetches `/VERSIONS` for the list, and on
   a click checks whether the target page exists. Books stay unchanged, but
   every page makes an extra request, each switch waits, and it breaks
   wherever the site is not served from the root.
3. **A mix:** list at build time, page checked on click. The wait of 2
   without saving much over 1.

## Decided

- Approach 1.
- Placement B: a separate button on the right; the title keeps its home
  link.
- Title `irsync Documentation`.
- Dev is listed, `/book/` is not, switching keeps the page (all from the
  user, recorded in #123). Falling back to the title page for a missing
  page is the agreed addition to the issue.

## Constraints

- **The book in `book/` stays plain mdbook.** `mdbook serve book` and
  `mdbook build book` must keep working with no generated file present.
  There, the switcher does not appear. The list exists only in the
  working copies the site build makes under `target/site-books/`.
- **Nothing under `book/` is changed by the build** (as in #105).
  Generated files go into the working copies.
- **Every book gets the same list**, whatever its version: the list is
  today's, from the same build. An old release's book shows today's newer
  versions.
- **The menu is written into the built pages (changed while planning).**
  After mdbook has built all five books, a build step adds the switcher's
  markup to every page that has the top bar, with each link worked out for
  that page: the same page in the other book if that book has the file,
  else its title page (`index.html`). The pages each book has are the HTML
  files mdbook wrote, so nothing guesses mdbook's file names, and the
  build's link check (`check.py`) already checks every one of these links.
  The design first had the lists written before mdbook runs and resolved in
  the browser; this does the same at build time with less code in the
  browser and no page lists shipped.
- **Real links.** The entries are `<a href>` links, so open-in-new-tab and
  copy-link work. The links are relative (`../beta/…`), so a copy of the
  site served below the root still works, as do the other links in the
  books.
- **The theme picker's look and behaviour.** The list opens and closes like
  mdbook's theme list: the same popup styling, closes on Escape and on a
  click elsewhere, keyboard reachable, `aria-haspopup` and
  `aria-expanded` on the button.
- **Same fragment.** When the page exists in the other book, any `#anchor`
  in the address is kept.
- **Light and dark.** The button and list use the design tokens, as the
  rest of the book theme does.
- **Small screens.** At phone width the top bar has to fit; the button may
  shrink to the channel name only (it already is), and must not push the
  title off the bar.

## Changes

- `book/book.toml`: title `irsync Documentation`.
- `site/tools/label_book.py`: stops rewriting the title (it no longer
  holds a version). Keeps `site-url`, the edit link and the title page
  sentence.
- New: the switcher's behaviour in `book/theme/switcher.js` (listed in
  `additional-js`: open, close, keep the `#anchor`), styles in
  `irsync.css`. On a page without the switcher's markup it does nothing.
- New: `site/tools/switcher.py`, which reads `VERSIONS` and adds the
  markup to the built pages.
- `site/build.sh`: runs it once all books are built.
- Tests for the new step and the label change, in `site/tools/`.
- `book/src/contributing/website.md`: "The books" and "The book's look"
  describe the switcher and the new title; the top-bar line that names the
  version goes.

## Non-goals

- No switcher on the landing page.
- No list of older releases: one book per channel, as today.
- No request to the server, and no change to `VERSIONS` or the promote.
- No change to which books exist or what goes into them, besides the title.

## Settled after the builder check (my calls)

- **The ticked entry in `/book/` is a link to the channel's own book**
  (`../beta/…`), like every other entry. All entries are real links.
- **Pages outside `SUMMARY.md`.** `print.html` is in every book's list, so
  switching from the print page keeps it. `404.html` is not a page to
  switch to: from there, an entry opens the other book's title page. Draft
  chapters (a `SUMMARY.md` entry with no file) make no page and stay out of
  the list.
- **A release that is being built** (`unpublished` in `VERSIONS`) has a
  book on staging, so it is listed like any other. The promote already
  refuses such a build.
- **`label_book.py`** no longer looks for the line `title = "irsync"`: it
  neither checks nor changes the title.

## Left to the builder

- File names, and how the data is handed to the script.
- The exact markup and styles, within the constraints.
- How the switcher's behaviour is tested beyond the Python tests (the site
  has no JavaScript test setup; a headless Chrome check, as used for the
  landing page buttons, is enough).
