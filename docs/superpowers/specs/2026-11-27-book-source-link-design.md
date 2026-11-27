# Where each book was built from, in its title

## Why

A reader of the book should see which source the book they read was built
from, and get there in one click: for the Dev book, the commit; for a
release, its release branch and its tag. Issue #331. It needs the release
branches of #322.

## What the reader sees

```
Dev:      irsync Documentation (main@b5618f4)
Release:  irsync Documentation (release-0.1@v0.1.0-beta.7)
```

- Two links, the `@` and the brackets plain text:
  - Dev: the branch links to `tree/BRANCH`, the short commit to `commit/FULLSHA`.
  - Release books (/book/, /stable/, /beta/, /alpha/): `release-X.Y` links
    to `tree/release-X.Y`, the version's tag `vVERSION` to `tree/vVERSION`.
- The Dev book's branch is the one checked out for the build (the site
  workflow builds `main`). With no branch checked out, only the commit shows.
- `irsync Documentation` keeps linking to the home page.
- The bracket is smaller and muted, has no mark, and is hidden on a narrow
  screen with the rest of the title.
- No commit known: no bracket. A commit that is not a full 40-character
  hash, or a branch name with characters outside `A-Z a-z 0-9 . _ / -`, is
  refused.

## Approaches

- **Chosen: a script per book copy.** `site/tools/label_book.py` writes
  `theme/source.js` (the parts and their links) into the build copy and
  loads it before `book/theme/home-link.js`, which shows it. `book/` is
  not changed, and a bare `mdbook serve` shows the title as today.
- Rejected: putting it in the book's title in `book.toml`. mdbook writes
  the title as plain text, so it could not hold the links, and every page
  title would carry it.
- Rejected: `irsync@...` as the label. The repository name never changes
  and says nothing; the branch does.

## How

`site/build.sh` passes each book's version, commit and branch (from the
`VERSIONS` list) to `label_book.py`. Tests: `site/tools/test_label_book.py`
and `site/tools/test_book_theme.py`. `book/src/contributing/website.md`
says what the title shows.
