# Correctness and testing docs: design

Date: 2026-11-10

## Why

For a file synchronisation tool, correctness comes first and performance
second. The project already does a lot to show that irsync is correct: engine
tests with test hooks, a seeded random workload generator (filegen), an
independent verifier (fileset), a loopback stress loop, the same loop over a
simulated bad network (netstress), and long campaigns. The evidence is spread
over seven Contributing pages, though, and nothing makes the case to a reader.

This work makes the correctness story visible to two readers:

- **Advanced users** who want to be convinced before they trust irsync with
  their data.
- **Contributors** who want to add or change tests and need to know which
  tool covers what.

What a skeptical user should believe after reading: **every sync leaves the
destination exactly matching the source, and they have a second line of
defence in `fileset`, which lets them check independently whether a transfer
was correct.** (The user's words.) On the page, the claim is stated with its
conditions; see part 1.

A second goal comes out of that: **fileset becomes a first-class tool.** It is
useful on its own, without irsync, to verify transfers and backups made with
other tools (rsync, rclone, cp) and to check important data in storage for
bitrot. Today it is described as "Optional" and its docs assume irsync.

## Goals

1. A correctness page in the book's Explanation part that makes the case for
   the claim above.
2. fileset as a new top-level part of the book, usable by someone who never
   uses irsync.
3. A correctness sentence in the landing page's hero and a fileset section on
   the landing page (layout "B", chosen from mockups; details below).
4. A new page explaining how filegen's Markov chain works, next to "How
   netstress works".
5. The Contributing testing pages turned into one discoverable story, with
   "Testing" as the hub.

## Non-goals

- No rewrite of the existing netstress, stress or bug-hunting content. Its
  content is fine; it only needs to be findable and placed in the story.
- No new tests, harness features or filegen operations. This is docs and the
  landing page only.
- No promise of which filegen user journeys or filesystem features come next.
- filegen is not presented to users. It stays under Contributing.

## Anti-goals

- Overclaiming. The correctness page must not say more than the tests show.
  It names what the tests cannot see (from "What the loop can and cannot see"
  in `book/src/contributing/bug-hunting.md`) and links to
  `reference/limitations.md`.
- fileset reading as an irsync accessory. Its part of the book must work for
  an rsync or rclone user who has never heard of irsync.
- Copying content into several places. Each fact lives on one page, and other
  pages link to it. In particular, the "Bugs the harness has found" table stays
  on `contributing/stress.md`.

## Constraints and house style

- The book is mdBook (`book/`, built by `site/build.sh`, mdBook 0.5.2 in CI).
  It is organised by Diátaxis: tutorial, how-to, reference, explanation, plus
  Contributing.
- The house style for the docs is short plain sentences, home directories
  written as `~` (never `/home/you`), example roles client and server, and
  example hosts `myclient` and `myserver`.
- No backward compatibility is needed: there are no users yet, so pages may
  move. Links in the repo (README, `site/index.html`, other book pages) must
  be updated to the new paths. Whether to add mdBook redirects for moved
  pages is left to the builder (see "Left to the builder").

## The parts

### 1. Correctness page (Explanation)

A new page, `book/src/explanation/correctness.md`. The working title is "How
we know irsync is correct"; the builder may improve it. It is listed in
`SUMMARY.md` under Explanation.

Outline (proposed by Claude and agreed with the user):

1. **The claim, with its conditions.** A completed `-a --delete` sync of a
   source that is not changing leaves the destination exactly matching the
   source. (The user chose this framing.) Say what "exactly" covers:
   contents, names, types, modes, times, symlink targets, hard links, sparse
   holes, deletions. For each item, say what evidence backs it (user's
   choice, option "b" from the review):
   - fileset's default check: type, content, symlink target, device numbers,
     mode, mtime, size.
   - fileset with `--require nlink` or `--paranoid`: hard links (and the
     other fields the paranoid tier adds).
   - Engine tests and campaign rows only: sparse holes, which fileset does
     not record at all (checked with `du -s` in the `sparse` matrix row).

   Check every one of these against `book/src/reference/fileset.md`,
   `book/src/contributing/bug-hunting.md` and the code before writing it.
   Point to `reference/limitations.md` for the edges, such as flags and
   attributes irsync does not carry.
2. **Check it yourself with fileset.** This is the second line of defence. It
   is a separate tool that shares no code with irsync, and it is the same
   check the test harness uses. Say plainly what the default check covers,
   how to widen it (`--paranoid`, `--require nlink`), and that holes are not
   recorded. Link to the fileset part of the book.
3. **How we check it, layer by layer.** Each layer gets one short paragraph:
   what it catches, and a link to the detailed page.
   - Engine tests in `crates/irsync/tests/`, using the `test-hooks` event log
     to pin the order of steps.
   - The stress loop on loopback: filegen mutates, fileset records, irsync
     syncs, fileset checks.
   - The same loop over a simulated bad network (netstress): loss, delay,
     dead zones, handovers.
   - Campaigns: many seeds, the campaign matrix, paranoid checks.
4. **Evidence.** The loop finds real bugs. Summarise in a sentence or two and
   link to "Bugs the harness has found" on the stress page.
5. **Outside the claim, and what the tests cannot see.** An honest,
   user-level section. It covers two things.
   - **Cases the claim does not cover** (user's choice):
     - A sync without `-a` does not keep modes and times.
     - A sync without `--delete` leaves removed files in place.
     - An interrupted sync, or one ending with exit 23, is incomplete and may
       leave `.part` files. Running the same command again resumes it.
     - Files that change while the sync runs. Say what irsync does in that
       case, which needs research (see "Order of work").
     - After a power cut soon after a sync with `--fsync` off (the default),
       a file can keep its name, size and time but lose its content. See the
       `--fsync` row in `reference/command-line.md`.
   - **The three filters** (filegen must provoke the bug, fileset must
     observe it, a known bug must not hide it). Link to Hunting bugs for the
     detail.
6. **For contributors.** One line linking to the Testing hub.

The page is written for an advanced user, not a contributor. It avoids
internal names where it can, or explains them the first time they appear.

### 2. fileset as a top-level part of the book

fileset gets its own part in `SUMMARY.md`. It sits at the top level, next to
irsync's own parts, not nested inside them. The user decided this. It is
written so that someone who never uses irsync can follow it.

Use cases (the user's words): verify transfers and backups made with other
tools, such as rsync or rclone, and check important data sets in storage
systems for bitrot.

Proposed contents (agreed in shape; page titles are the builder's call):

- **Introduction to fileset.** What it does, that it shares no code with
  irsync, the core idea (record a manifest, check against it later), and how
  to install it.
- **Tutorial.** Record a manifest, copy the tree with a tool you already use
  (rsync), check the copy.
- **How-tos:**
  - Verify an rsync or rclone transfer.
  - Verify an irsync transfer (the existing
    `how-to/verify-with-fileset.md`, moved here).
  - Check data for bitrot over time, including on a schedule. The existing
    "Check a disk for bitrot later" section is the starting point.
  - Keep a manifest current as a tree changes (`update`, and why to `check`
    before `update` on a tree you suspect).
  - Check read-only media and snapshots (existing material).
- **Reference.** The existing `reference/fileset.md`, moved here.
- **Explanation.** What a manifest records, the comparison tiers (default,
  relaxed, paranoid), and what fileset cannot catch.

Everything else points into this part instead of describing fileset itself:
the landing page, `introduction.md` (including "How the book is organised"),
the irsync tutorial, the correctness page, the README, and the Contributing
pages.

Claude's call: the irsync tutorial and `introduction.md` get only a one-line
link to the fileset part. The tutorial gets no new step, because it was just
reworked.

### 3. Landing page (`site/index.html`), layout B

The user chose layout B from three mockups.

- **Hero.** A short correctness sentence after the opening paragraphs. Draft:
  "Correctness comes first: a completed sync leaves an exact copy, and you
  can check that yourself with fileset." It links to the correctness page,
  which states the conditions. The builder may refine the wording, but must
  keep it to one or two sentences and must not claim more than the
  correctness page does.
- **A fileset section of its own,** placed after the claim cards (before "If
  you know rsync"). It contains:
  - A heading that presents fileset as a tool in its own right. Draft:
    "fileset: check any copy, made by any tool".
  - One or two sentences: record a manifest, check a copy against it after
    rsync, rclone, cp or irsync, or months later to catch bitrot.
  - A short terminal example using rsync, styled like the existing terminals
    on the page. The example must be checked against fileset's real command
    line (`fileset init`, `fileset check --manifest …`).
  - Links to the fileset part of the book and to installing fileset.
- **Top nav.** Add a "fileset" link pointing at the fileset part of the book.
- **Install section.** Drop the word "Optional", reword the fileset paragraph
  so that fileset doesn't read as an afterthought, and fix its link to point
  at the new fileset pages.
- **"The book" map** further down the page: add an entry for the fileset part.

The page's existing style (tokens, cards, terminal blocks, light and dark
themes) is reused; no new visual language is introduced. The mockups showed
placement only.

### 4. How filegen works (Contributing)

A new page next to "How netstress works", for example
`book/src/contributing/filegen-internals.md`. filegen is internal: its job is
to validate and stress test irsync and fileset. It is not mentioned on the
landing page or in the user-facing parts of the book.

Contents:

- **Why a Markov chain.** Real filesystem use comes in patterns: create a file
  and then append to it, make a directory and then fill it, write and then
  rename. Purely independent random operations rarely build the situations
  where sync bugs hide. A chain over operation kinds, where the next kind
  depends on the previous one, produces those patterns while staying
  reproducible from a seed. (This framing is Claude's; it matches the code in
  `crates/filegen/src/chain.rs` and the user agreed with it.)
- **How it works.**
  - The 22×22 transition table: rows are the previous kind, columns the next.
  - How a kind is switched off (its column is zero in every row).
  - Locality: same directory, parent, or anywhere.
  - Size pressure above 80% of `max_total_size`, which damps growing kinds and
    boosts shrinking ones.
  - Sizes, names and content patterns.
  - The seeded generator and resume from state.
  
  Describe all of this as the code has it; read `crates/filegen/src/` rather
  than restating the design spec.
- **Tuning without code changes:** `--config`, `--filegen-set` (as
  `irsync-stress` and `crates/stress/tools/matrix.sh` use it).
- **Extending filegen. Left open on purpose.** filegen can be changed to
  cover new critical filesystem user journeys (sequences that matter to real
  users), and to cover filesystem-specific features. Say where each kind of
  change goes in the code: a new `OpKind`, its weights row and column, its
  config, its application in `run.rs`/`model.rs`. Say that a new behaviour
  is only useful if `fileset check` can observe its effect, so it may need a
  fileset change too. Name no specific journeys or features as planned.

The existing `contributing/filegen.md` stays the command and configuration
reference. It gets a short pointer to the new page, and the large weights
block can stay there.

### 5. Testing hub and sidebar

The content of the netstress, stress and bug-hunting pages stays as it is.
What changes is how a reader finds them and how they fit together.

- **`contributing/testing.md` becomes the hub.** It opens with the layers,
  from fast to slow:
  1. Engine tests (with `test-hooks`).
  2. The loopback stress loop.
  3. netstress.
  4. Campaigns.

  For each layer it says what it catches and when to reach for it, with a
  short guide that maps the kind of change to the test to add or run. Examples:
  - A change to the receiver or the planner gets an engine test in
    `crates/irsync/tests/`.
  - A change to anything that walks or writes the tree gets a stress run.
  - A change to sessions, resume or the transport gets a netstress run.
  - Anything risky gets a campaign row.

  The builder should check these against the code and the existing pages.
  The current content (the suite command, Cargo features, platform skips,
  the receiving-a-changed-file tests) stays below the overview.
- **Sidebar.** In `SUMMARY.md`, nest the testing pages under Testing:
  - filegen, and How filegen works
  - Stress testing
  - Hunting bugs
  - Network stress with netstress, and How netstress works

  Setup, Performance, Design records and This website stay at the top level
  of Contributing. The exact nesting is the builder's call, within mdBook's
  limits.
- **Cross-links.** Each testing page opens with one line saying where it fits
  and linking to the hub. The hub links to the user-facing correctness page,
  and the correctness page links back to the hub.
- **`introduction.md`.** Update "How the book is organised" to mention the
  fileset part and the correctness page.

## Order of work

1. Correctness page. Before writing it, research what irsync does when
   source files change while a sync runs. Read the sender and planner code
   and the existing tests and decision records (`docs/IRSYNC_DECISIONS.md`).
   Then state the behaviour as the code has it. If the behaviour is unclear
   or looks wrong, stop and report it instead of documenting a guess.
2. fileset part of the book, including moving the two existing fileset pages
   and updating every link to them.
3. Landing page.
4. How filegen works.
5. Testing hub, sidebar nesting and cross-links.

Each step should leave the book building cleanly (`mdbook build book`) with
no broken internal links.

## Left to the builder

- Exact page titles, file names and wording, within the house style.
- Whether moved pages get mdBook redirects (`[output.html.redirect]` in
  `book/book.toml`). There are no users yet, so this is optional.
- How to check links. If the project has no link checker, a local
  `mdbook build` plus a grep for the old paths is enough.
- The exact nesting depth in `SUMMARY.md`.
