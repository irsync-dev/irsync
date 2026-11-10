# Correctness and testing docs Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make irsync's correctness story visible. This means:
- a correctness page in the book,
- fileset as its own top-level part of the book,
- a correctness sentence and a fileset section on the landing page,
- a page on how filegen's Markov chain works,
- the testing pages tied together under one Testing hub.

**Architecture:** Docs and landing page only, with no code changes. The fileset part comes first, because the correctness page and the landing page link into it. The spec's order put the correctness page first, but then that page would link to pages that don't exist yet and the link check would fail. After that come the correctness page, the landing page, the filegen page, and finally the testing hub, the sidebar nesting and the cross-links. Each task ends with a clean site build and link check.

**Tech Stack:** mdBook 0.5 (`book/`), static HTML/CSS (`site/`), Python site checker (`site/tools/check.py`).

**Spec:** `docs/superpowers/specs/2026-11-10-correctness-docs-design.md`. Read it before any task; it holds the outlines, the agreed claim and the user's decisions.

## Global Constraints

- The claim, as the user agreed it: *a completed `-a --delete` sync of a source that is not changing leaves the destination exactly matching the source.* No page, nor the landing page, may claim more.
- Evidence per item of "exactly" (spec part 1). fileset's default tier covers type, content, symlink target, device numbers, mode, mtime and size. Hard links need `--require nlink` or `--paranoid`. Sparse holes are not recorded by fileset; only the engine tests and the `sparse` campaign row check them.
- fileset is presented as a tool in its own right: a reader who never uses irsync must be able to follow its part of the book.
- filegen is internal. It is not mentioned on the landing page or in the user-facing parts of the book (Introduction, Tutorial, How-to, Reference, Explanation, fileset part).
- Each fact lives on one page; other pages link to it. "Bugs the harness has found" stays in `book/src/contributing/stress.md`.
- Content of the netstress, stress and bug-hunting pages is not rewritten; they only get a placement line and new links.
- House style:
  - Short plain sentences.
  - Write home directories as `~`, never `/home/you` or `/Users/you`.
  - Example roles are client and server; example hosts are `myclient` and `myserver`.
  - Every command and flag quoted must exist. Check it against the real binary (`cargo run -q -p fileset -- <cmd> --help`) or against `book/src/reference/fileset.md`.
- Commit messages: a short summary line, a blank line, then a short explanation of why. No attribution trailers of any kind.
- Verification after every task:
  ```sh
  cargo build -q -p irsync && site/build.sh && python3 site/tools/check.py --irsync target/debug/irsync target/site
  ```
  Expected: exit 0 and no `broken link` lines.

## Review Focus

1. **A copy tool that does not carry all metadata.** For example, rclone does not keep modes on most backends, and `cp` without `-p` keeps neither modes nor times. A reader who runs a default `fileset check` then sees a `CHG` line for every file and concludes the copy is broken. The rsync/rclone how-to (Task 1) must show `--relaxed` or `--ignore mode` / `--ignore mtime` for such tools, with sample output.
2. **The manifest inside the tree.** `fileset init` writes `.fileset.sqlite` into the tree, so the copy tool copies it too. A filter (such as `--exclude`) may leave it behind. The fileset tutorial and how-tos (Task 1) must say where the manifest is, and show `--manifest` for the case where the copy lacks it.
3. **A copy that isn't on a local filesystem.** For example, rclone to S3. fileset needs a local path. The rclone how-to (Task 1) must say so, and point to checking through `rclone mount` or on the machine that holds the copy.
4. **A bitrot check after legitimate edits.** `check` reports intended changes as well as rot. The bitrot how-to (Task 1) must say to check before `update`, and what an intended change looks like next to rot. Today this lives in `how-to/verify-with-fileset.md`.
5. **Overclaiming in short copy.** The landing page's hero sentence and fileset section (Task 3) and the correctness page's opening (Task 2) must not drop the "completed" condition, or imply that fileset checks hard links or holes by default. Task 3, Step 3 greps for this.

---

### Task 1: fileset as a top-level part of the book

**Files:**
- Create: `book/src/fileset/introduction.md`. What fileset is, that it shares no code with irsync, the idea of recording a manifest and checking against it, and how to install it.
- Create: `book/src/fileset/tutorial.md`. Record a manifest, copy the tree with `rsync -a`, check the copy.
- Create: `book/src/fileset/verify-other-tools.md`. Verify an rsync or rclone transfer.
- Move: `book/src/how-to/verify-with-fileset.md` → `book/src/fileset/verify-irsync.md`. Move its section "Check a disk for bitrot later" and the read-only snapshot part out to the bitrot page.
- Create: `book/src/fileset/bitrot.md`. Check data for bitrot over time, including on a schedule (a cron line using exit codes 0/1/2), plus read-only media and snapshots.
- Create: `book/src/fileset/keep-current.md`. Keep a manifest current with `update`, and why to `check` before `update`.
- Move: `book/src/reference/fileset.md` → `book/src/fileset/reference.md`.
- Create: `book/src/fileset/how-it-works.md`. What a manifest records, the three tiers, and what fileset cannot catch (holes, ACLs, the known limitations).
- Modify: `book/src/SUMMARY.md`. Add a new part `# fileset` after the Explanation part and before Contributing. Remove the two old entries.
- Modify: links to the moved pages. Find them with `grep -rn -E 'verify-with-fileset|reference/fileset' book/src site/index.html README.md crates/fileset/README.md`. The hits today are:
  - `book/src/contributing/setup.md:14`
  - `README.md:83`
  - `crates/fileset/README.md:11` (absolute `https://irsync.org/book/...` URL)
  - `site/index.html:337`
  - the moved pages' links to each other
- Modify: `book/src/introduction.md`. Under "How the book is organised", add a bullet for the fileset part.
- Modify: `book/src/tutorial/first-sync.md`. Add one line at the end linking to `../fileset/verify-irsync.md`. Do not add a new step.

**Interfaces:**
- Produces these pages, linked by later tasks: `fileset/introduction.md`, `fileset/verify-irsync.md`, `fileset/how-it-works.md` (anchor for the tiers: `#comparison-tiers`, a heading named "Comparison tiers") and `fileset/reference.md`. Built URLs: `book/fileset/introduction.html` and so on.

- [ ] **Step 1: Move the two pages with `git mv`** and update `SUMMARY.md` to the new part. The new pages go in the order listed above (introduction, tutorial, how-tos, reference, explanation).
- [ ] **Step 2: Write the new pages.** All pages use `~/Photos` and an rsync backup at `/mnt/backup/Photos` as the running example. Every command shown must run. Before writing about it, check it against `cargo run -q -p fileset -- --help` and the reference. The introduction opens with the two uses in the user's words: verifying transfers and backups made with other tools, such as rsync or rclone, and checking important data sets in storage for bitrot. For the rsync/rclone page, cover Review Focus 1–3. For the bitrot page, cover Review Focus 4.
- [ ] **Step 3: Fix every link** found by the grep above. Absolute URLs become `https://irsync.org/book/fileset/reference.html` and so on.
- [ ] **Step 4: Check that no old path is left.** Run `grep -rn -E 'verify-with-fileset|reference/fileset' book/src site README.md crates`. Expected: no output.
- [ ] **Step 5: Run the verification command** from Global Constraints. Expected: exit 0.
- [ ] **Step 6: Commit.** `git commit -m "Give fileset its own part of the book" -m "fileset verifies copies made with any tool and finds bitrot, but its docs assumed irsync. Its own part lets rsync and rclone users follow it, and lets the rest of the book point to one place."`

### Task 2: The correctness page

**Files:**
- Create: `book/src/explanation/correctness.md`. The title is "How we know irsync is correct"; the builder may improve it.
- Modify: `book/src/SUMMARY.md`. Make it the first entry under Explanation.
- Modify: `book/src/introduction.md`. Name the page in the Explanation bullet of "How the book is organised", and add one sentence near the top linking to it.

**Interfaces:**
- Consumes: `fileset/introduction.md`, `fileset/how-it-works.md#comparison-tiers` (Task 1), `contributing/stress.md#bugs-the-harness-has-found`, `contributing/bug-hunting.md#what-the-loop-can-and-cannot-see`, `contributing/testing.md`, `reference/limitations.md`, `reference/command-line.md` (the `--fsync` row).
- Produces: `book/explanation/correctness.html`, linked by Tasks 3 and 5. Section anchors that later tasks link to: `#the-claim` and `#check-it-yourself-with-fileset`.

- [ ] **Step 1: Research what happens when a source file changes during a sync.** Read:
  - the sender's file reading and hashing in `crates/irsync/src/send.rs` and `crates/irsync/src/session/sender_role.rs`,
  - the listing walk and the planner in `crates/irsync/src/session/planner.rs`,
  - the vanished-file handling described in `book/src/explanation/receiving-a-changed-file.md` (around lines 41 and 213),
  - any tests that change a file mid-transfer: `grep -rln -i -E 'mid|during|grow|shrink|vanish' crates/irsync/tests`.

  Write down, with `file:line`, what happens in each of three cases: a file grows or shrinks while it is sent, a file is rewritten with the same size, and a file is deleted after it was listed. **If the behaviour is unclear, or it could leave a wrong file at the destination without an error or exit 23, stop and report it to the user before writing the page.**
- [ ] **Step 2: Write the page** to the spec's six-part outline (spec part 1):
  1. the claim with its conditions, and the evidence for each item,
  2. checking it yourself with fileset,
  3. the layers,
  4. the evidence,
  5. what lies outside the claim and what the tests cannot see, including the Step 1 findings and the `--fsync` case,
  6. one line for contributors.

  Headings must produce the anchors listed in Interfaces. Check each evidence statement against `book/src/fileset/reference.md` and `book/src/contributing/bug-hunting.md`.
- [ ] **Step 3: Check the page for overclaiming.** Run `grep -n -i -E 'every sync|always|guarantee' book/src/explanation/correctness.md`. Each hit must either be qualified by the conditions or quote the claim. Run `grep -n -i -E 'nlink|hard link' book/src/explanation/correctness.md`. Each mention of fileset with hard links must name `--require nlink` or `--paranoid`.
- [ ] **Step 4: Run the verification command.** Expected: exit 0.
- [ ] **Step 5: Commit.** `git commit -m "Add a page on how we know irsync is correct" -m "The evidence for correctness was spread over the Contributing pages. This page states the claim with its conditions, what backs each part, how to check it with fileset, and what the tests cannot see."`

### Task 3: Landing page, layout B

**Files:**
- Modify: `site/index.html`
- Modify: `site/style.css` (only if the new section needs styles the page lacks; reuse the existing classes `terminal`, `prompt`, `dim`, `button`, `card` first)

**Interfaces:**
- Consumes: `book/explanation/correctness.html` (Task 2), `book/fileset/introduction.html` and `book/fileset/verify-other-tools.html` (Task 1).

- [ ] **Step 1: Make the edits.**
  - **Hero:** after the `lede-last` paragraph, add one paragraph. Draft wording: "Correctness comes first: a completed sync leaves an exact copy, and you can check that yourself with fileset." It links to `book/explanation/correctness.html`, and "fileset" links to `#fileset`.
  - **Nav:** add `<li><a href="#fileset">fileset</a></li>` after "documentation".
  - **New section:** add `<section id="fileset" aria-labelledby="fileset-heading">` between the `claims` section and `#rsync`. It holds:
    - the heading "fileset: check any copy, made by any tool",
    - one or two sentences on verifying rsync, rclone, cp or irsync copies and catching bitrot,
    - a `pre.terminal` with `fileset init ~/Photos`, `rsync -a ~/Photos/ /mnt/backup/Photos/`, `fileset check /mnt/backup/Photos`, styled like the install terminal,
    - links to `book/fileset/introduction.html` and `#install`.

    The commands must match the fileset tutorial from Task 1.
  - **Install section:** drop "Optional:", reword the fileset paragraph as a peer tool, and link it to `book/fileset/introduction.html`.
  - **"The book" map** (`#the-book`): add an entry for the fileset part.
- [ ] **Step 2: Run the verification command.** Expected: exit 0.
- [ ] **Step 3: Check for overclaiming.** Run `grep -n -i -E 'every sync|always|guarantee' site/index.html`. There must be no new hits.
- [ ] **Step 4: Look at the page by hand.** Run `site/serve.sh`. Check light and dark themes, a 360 px wide window (no horizontal page scroll; the terminal scrolls inside itself), and keyboard focus on the new links.
- [ ] **Step 5: Commit.** `git commit -m "Put correctness and fileset on the landing page" -m "Correctness comes first for irsync. The hero now says so, and fileset gets its own section as a tool in its own right rather than an optional extra."`

### Task 4: How filegen works

**Files:**
- Create: `book/src/contributing/filegen-internals.md` ("How filegen works")
- Modify: `book/src/contributing/filegen.md` (add one pointer line near the top to the new page)
- Modify: `book/src/SUMMARY.md` (add the entry right after "Generating test trees with filegen"; Task 5 nests it)

**Interfaces:**
- Produces: `book/contributing/filegen-internals.html`, linked from the Testing hub in Task 5.

- [ ] **Step 1: Read the code the page describes:**
  - `crates/filegen/src/chain.rs`: the 22×22 matrix and the pressure in `Chain::next`,
  - `crates/filegen/src/kind.rs`: `OpKind`, `grows`/`shrinks`,
  - `crates/filegen/src/pick.rs`: locality,
  - `crates/filegen/src/sizes.rs`, `crates/filegen/src/names.rs` and `crates/filegen/src/content.rs`,
  - `crates/filegen/src/run.rs` and `crates/filegen/src/model.rs`: applying a step,
  - `crates/filegen/src/state.rs`: resume.

  Describe them as the code has them, not as the 2026-10-20 design spec says.
- [ ] **Step 2: Write the page** to spec part 4. It has four parts:
  - **Why a Markov chain.**
  - **How it works.** The table, switching a kind off, locality, the size pressure (above 80% of `max_total_size`), sizes, names and content, the seed, and resume.
  - **Tuning without code.** `--config`, `--filegen-set`, and how `crates/stress/tools/matrix.sh` uses its `col` function.
  - **Extending filegen.** New critical user journeys and filesystem-specific features. Say where each change goes, using the real file and type names from Step 1. Say that a new effect must be observable by `fileset check`, or fileset needs a matching change. Name no planned journeys or features.

  Model the opening on `contributing/netstress-internals.md`: one paragraph saying what the page covers and where usage lives. Add no user-facing links to this page.
- [ ] **Step 3: Run the verification command.** Expected: exit 0.
- [ ] **Step 4: Commit.** `git commit -m "Explain how filegen's Markov chain works" -m "The chain shapes every stress workload but was described in one sentence and a config comment. Contributors need to know how to read, tune and extend it."`

### Task 5: Testing hub, sidebar and cross-links

**Files:**
- Modify: `book/src/contributing/testing.md`. Add a new opening with the layers and the guide that maps a change to a test. The existing sections stay below it.
- Modify: `book/src/SUMMARY.md`. Nest the testing pages under Testing.
- Modify: the opening line of each of these, to say where the page fits and link to the hub:
  - `book/src/contributing/filegen.md`
  - `book/src/contributing/filegen-internals.md`
  - `book/src/contributing/stress.md`
  - `book/src/contributing/bug-hunting.md`
  - `book/src/contributing/netstress.md`
  - `book/src/contributing/netstress-internals.md`

**Interfaces:**
- Consumes: `explanation/correctness.md` (Task 2), `contributing/filegen-internals.md` (Task 4).

- [ ] **Step 1: Write the hub's opening.**
  - One paragraph linking to `../explanation/correctness.md` for the user-level case.
  - The four layers, fast to slow: engine tests with `test-hooks`, the loopback stress loop, netstress, and campaigns. Each gets what it catches and when to reach for it.
  - The guide from change to test, checked against the code and the existing pages. The spec's examples are a starting point:
    - receiver or planner → engine test in `crates/irsync/tests/`,
    - walking or writing the tree → a stress run,
    - sessions, resume or transport → a netstress run,
    - anything risky → a campaign row.

  The existing "Workload and stress tools" section may shrink to links, since the layers now say the same thing. Keep one copy of each fact.
- [ ] **Step 2: Nest the pages in `SUMMARY.md`.** Under `- [Testing](contributing/testing.md)`, indented two spaces, in this order:
  - filegen
  - How filegen works
  - Stress testing
  - Hunting bugs
  - Network stress with netstress
  - How netstress works

  Setup, Performance, Design records and This website stay at the top level of Contributing.
- [ ] **Step 3: Add the opening line to each page** listed under Files. Example: "Part of [Testing](testing.md): the loopback layer of the stress loop." Keep it to one line, followed by a blank line. Don't change anything else on these pages.
- [ ] **Step 4: Run the verification command.** Expected: exit 0.
- [ ] **Step 5: Check by hand** in `site/serve.sh` that the sidebar shows the nested Testing pages, and that each page opens with its line.
- [ ] **Step 6: Commit.** `git commit -m "Make Testing the hub of the testing pages" -m "The stress, netstress and filegen pages were hard to find and did not read as one story. Testing now lays out the layers and which test a change needs, and the pages nest under it."`
