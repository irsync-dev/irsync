# The Dev book's commit in the version switcher

Builds on the version switcher (#123, `2026-11-19-book-version-switcher-design.md`).

## Why

After a merge to `main`, dev.irsync.org is rebuilt. Today the switcher's
Dev entry says `unreleased`, so nothing on the page tells whether the new
build is live. Showing the commit the Dev book was built from answers that
at a glance: compare it with the merge commit.

## What changes

- The Dev entry shows `irsync@SHORTSHA` where it showed `unreleased`, for
  example `irsync@a3cdcd6`. SHORTSHA is the first 7 characters of the
  commit. `irsync` is a fixed word, not read from git.
- `/book/` before any release is built from the same commit, and its entry
  in the list is Dev, so it shows the same label.
- A `VERSIONS` file whose working-tree line has `-` for the commit (a
  build from before the line carried one) still shows `unreleased`.

## Non-goals

- No branch name: dev.irsync.org is always built from `main`.
- No mark for uncommitted changes: the deployed site is built from a commit.
- The button still reads `Dev`; the title page keeps its sentence about
  the unreleased development version; released books are unchanged.

## Approaches

1. **Read the commit from `VERSIONS` (chosen).** The build already writes
   the working tree's commit there, and `switcher.py` already reads the
   file. No new git calls.
2. **Ask git again in `switcher.py`.** Duplicates what `versions.py`
   already worked out, and could disagree with it.

## Changes

- `site/tools/switcher.py`: keep the commit of a working-tree line, and
  label the entry with it.
- `site/tools/test_switcher.py`: the new label and the fallback.
- The switcher spec and `book/src/contributing/website.md` where they say
  `unreleased` for the Dev entry.
