---
name: irsync
description: Resilient file sync over any network. A field manual for the landing page and the book.
colors:
  ground: "#f2f5f6"
  ground-dark: "#0e1920"
  raised: "#ffffff"
  raised-dark: "#15242d"
  rule: "#ccd6db"
  rule-dark: "#263943"
  ink: "#13222c"
  ink-dark: "#e2ebee"
  muted: "#4f616c"
  muted-dark: "#96a9b3"
  aqua: "#00a6b4"
  aqua-dark: "#2cc4d1"
  aqua-ink: "#00707b"
  aqua-ink-dark: "#5ed6e0"
  on-aqua: "#ffffff"
  on-aqua-dark: "#0e1920"
  signal: "#f6c700"
  on-signal: "#13222c"
  term: "#13222c"
  term-dark: "#081116"
  term-ink: "#dde7eb"
  term-muted: "#8da3ae"
  term-muted-dark: "#7f949f"
  error: "#c8322b"
  on-error: "#ffffff"
  warn-ink: "#7a5a00"
  warn-ink-dark: "#f6c700"
  danger-ink: "#b02a22"
  danger-ink-dark: "#f5877f"
  aqua-soft: "#d5eff2"
  aqua-soft-dark: "#103a42"
  warn-soft: "#fbefc0"
  warn-soft-dark: "#3a3106"
  danger-soft: "#f9e0de"
  danger-soft-dark: "#41201f"
typography:
  display:
    fontFamily: "Barlow, Bahnschrift, \"DIN Alternate\", \"Franklin Gothic Medium\", sans-serif"
    fontSize: "clamp(2rem, 4vw, 3rem)"
    fontWeight: 600
    lineHeight: 1.05
    letterSpacing: "-0.01em"
  headline:
    fontFamily: "Barlow, Bahnschrift, \"DIN Alternate\", \"Franklin Gothic Medium\", sans-serif"
    fontSize: "1.75rem"
    fontWeight: 600
    lineHeight: 1.15
  title:
    fontFamily: "\"Barlow Semi Condensed\", Bahnschrift, \"DIN Alternate\", \"Arial Narrow\", sans-serif"
    fontSize: "1rem"
    fontWeight: 600
    lineHeight: 1.75
  lede:
    fontFamily: "Inter, system-ui, -apple-system, \"Segoe UI\", Roboto, sans-serif"
    fontSize: "1.3125rem"
    fontWeight: 400
    lineHeight: 1.5
  body:
    fontFamily: "Inter, system-ui, -apple-system, \"Segoe UI\", Roboto, sans-serif"
    fontSize: "1.0625rem"
    fontWeight: 400
    lineHeight: 1.6
  small:
    fontFamily: "Inter, system-ui, -apple-system, \"Segoe UI\", Roboto, sans-serif"
    fontSize: "0.9375rem"
    fontWeight: 400
    lineHeight: 1.55
  label:
    fontFamily: "Barlow, Bahnschrift, \"DIN Alternate\", \"Franklin Gothic Medium\", sans-serif"
    fontSize: "1rem"
    fontWeight: 600
    lineHeight: 1.25
  code:
    fontFamily: "\"JetBrains Mono\", ui-monospace, \"SF Mono\", Menlo, Consolas, \"DejaVu Sans Mono\", monospace"
    fontSize: "0.9em"
    fontWeight: 400
rounded:
  sm: "2px"
  md: "4px"
spacing:
  page-gutter-sm: "20px"
  page-gutter: "40px"
  page-max: "1120px"
  station-gap: "20px"
  station-indent: "40px"
components:
  button-primary:
    backgroundColor: "{colors.aqua-ink}"
    textColor: "{colors.on-aqua}"
    typography: "{typography.label}"
    rounded: "{rounded.sm}"
    padding: "12px 20px"
  button-outline:
    backgroundColor: "{colors.ground}"
    textColor: "{colors.ink}"
    typography: "{typography.label}"
    rounded: "{rounded.sm}"
    padding: "12px 20px"
  button-outline-hover:
    backgroundColor: "{colors.raised}"
  badge-beta:
    backgroundColor: "{colors.signal}"
    textColor: "{colors.on-signal}"
    rounded: "{rounded.sm}"
    padding: "2px 8px"
  terminal-strip:
    backgroundColor: "{colors.term}"
    textColor: "{colors.term-ink}"
    typography: "{typography.code}"
    rounded: "{rounded.md}"
    padding: "12px 12px 12px 16px"
  copy-button:
    textColor: "{colors.term-ink}"
    rounded: "{rounded.sm}"
    padding: "4px 8px"
  copy-button-hover:
    backgroundColor: "{colors.term-ink}"
    textColor: "{colors.term}"
  nav-link:
    textColor: "{colors.ink}"
    typography: "{typography.label}"
  version-menu:
    backgroundColor: "{colors.ground}"
    textColor: "{colors.ink}"
    rounded: "{rounded.md}"
    padding: "4px 0"
  version-menu-item-hover:
    backgroundColor: "{colors.raised}"
---

# Design System: irsync

## Overview

**Creative North Star: "The Field Manual"**

irsync's pages read like a field manual you keep in the server room: plain,
dependable reference that tells you what to type and what will happen. The
materials are industrial. Signage type modelled on road signs, nearly square
corners, colour taken from fibre-optic cable jackets and used only where it
means something. They are arranged calmly, on a quiet navy-and-paper ground
with room around every block, so nothing shouts.

Everything is flat and exact. State shows as a change of colour, an
underline or a thicker rule, never as a lift or a glow. The one shadow in
the system belongs to the version menu, which really does float above the
page. The landing page and the book share one set of tokens
(`site/tokens.json`), so the manual looks the same from its cover to its
last reference page. Light and dark follow the reader's system setting on
the landing page and mdbook's theme in the book.

The commands are the content. A real command on a dark terminal strip,
copyable in one click, says more than any illustration. There are no
pictures, gradients, or decorative icons beyond the logo mark.

**Key Characteristics:**
- Flat, borders and tone instead of shadows.
- One accent (aqua) for every action and the route; yellow only for beta.
- Road-sign type (Barlow) for anything you scan, Inter for anything you read.
- Commands on dark terminal strips, dark in both themes.
- Nearly square corners: 2px and 4px, no pills.

## Colors

Deep navy ink on cool paper, with cable-jacket aqua as the single accent and
single-mode yellow held back for warnings.

### Primary
- **Multimode Aqua** (`aqua`): the fibre-jacket aqua as a fill. The route
  line that joins the Get started steps, and its stations. Never text.
- **Aqua Ink** (`aqua-ink`): the same aqua deepened to read as text. Every
  link, the primary button, the focus outline. The one colour of action.
- **On Aqua** (`on-aqua`): text on an aqua or aqua-ink fill.

### Secondary
- **Single-mode Yellow** (`signal`): the beta badge and the yellow bar
  beside the beta note. A fill only, with **On Signal** (`on-signal`) for
  any text on it. It disappears from the landing page once there is a
  stable release.

### Neutral
- **Cool Paper / Night Navy** (`ground`): the page background.
- **Raised White / Raised Navy** (`raised`): surfaces on the page: the
  book's sidebar, code blocks, tables, quotes, hover grounds.
- **Hairline** (`rule`): dividers and borders.
- **Deep Navy Ink** (`ink`): primary text.
- **Slate** (`muted`): secondary text: notes, metadata, release versions,
  the footer.
- **Terminal Navy** (`term`, `term-ink`, `term-muted`): the command
  strip, its text and its prompt. Dark in both themes.

### Book only
- `warn-ink` / `warn-soft` and `danger-ink` / `danger-soft`: warning
  and caution notes and code highlighting in the book. `aqua-soft`: search
  matches and menu hover in the book. The landing page uses none of them.
- `error` / `on-error`: daisyUI's error colour; nothing on the landing
  page uses it today.

### Named Rules
**The Cable Jacket Rule.** A colour means what its cable means: aqua is the
route and every action, yellow is the beta warning. Neither is ever
decoration.

**The Fill-or-Ink Rule.** Fills (`aqua`, `signal`) never carry text on
the ground; text uses the `-ink` variant. Text keeps 4.5:1 against the
ground it sits on (`site/tools/test_gen_tokens.py` checks it).

## Typography

**Display Font:** Barlow (with Bahnschrift, DIN Alternate)
**Condensed Font:** Barlow Semi Condensed (with Bahnschrift, Arial Narrow)
**Body Font:** Inter (with system-ui)
**Mono Font:** JetBrains Mono (with ui-monospace, SF Mono, Menlo)

**Character:** Barlow is modelled on road signs. Like the labels on a patch
panel, it is for what you scan. Inter is for what you read. JetBrains Mono
is for what you type. All three ship as web fonts, Latin only.

### Hierarchy
- **Display** (Barlow 600, clamp(2rem, 4vw, 3rem), 1.05, -0.01em): the
  landing page headline only.
- **Headline** (Barlow 600, 1.75rem, 1.15): section headings; the book's
  page titles.
- **Title** (Barlow Semi Condensed 600, 1rem): the step headings on the
  route ("Install", "Sync").
- **Lede** (Inter 400, 1.3125rem, 1.5): the introduction under the
  headline.
- **Body** (Inter 400, 1.0625rem, 1.6): running text. Keep lines under
  about 36em.
- **Small** (Inter 400, 0.9375rem, 1.55): notes under commands, the footer.
- **Label** (Barlow 600, 1rem, 1.25): navigation, buttons, the header.
- **Code** (JetBrains Mono, 0.9em of the text around it): inline code
  never wraps.

The scale is a perfect fourth (4:3) from a 17px body, in rem so the
reader's font size setting applies.

### Named Rules
**The Sign, Read, Type Rule.** Barlow for what you scan, Inter for what you
read, JetBrains Mono for what you type. Never swap them.

**The Inline Label Rule.** A label in running text ("No open ports.",
"Beta.") is Barlow Semi Condensed 600 at the size of the text around it,
not bigger.

## Layout

One column of up to 1120px, with 20px gutters on small screens and 40px from
the `sm` breakpoint. On wide screens the landing page's first section splits
into two columns (0.9fr : 1.1fr): the promise on the left, Get started on the
right. Below `md` they stack.

The Get started steps are stations on one aqua route: each step is indented
40px, with a 28px numbered circle on a 4px aqua line. Steps are 20px apart.
Spacing follows Tailwind's 4px scale. Text blocks cap at about 36em.

Touch targets are at least 44px on a coarse pointer (`pointer-coarse`),
reached by padding, not by bigger type.

**The Route Rule.** The aqua station line is only for content that really
is a sequence.

## Elevation & Depth

Flat by default. Depth comes from tone (`raised` on `ground`), hairline
rules and the dark terminal strip, never from shadows. daisyUI's `depth`
and `noise` are switched off.

### Shadow Vocabulary
- **Floating menu** (`box-shadow: 0 4px 6px -1px rgb(0 0 0 / 0.1), 0 2px 4px -2px rgb(0 0 0 / 0.1)`):
  the version menu on the landing page, the one element that overlaps
  content.

**The Flat Manual Rule.** Nothing lifts on hover. A shadow is allowed only
where an element truly floats over the page.

## Shapes

Nearly square. Badges, buttons and inputs take 2px; boxes (the terminal
strip, code blocks, tables, the menu) take 4px. No pills, no circles except
the numbered station markers and the logo's two rings. Borders are 1px
hairlines, or 2px for the outline button and the station ring (3px).

## Components

### Buttons
Flat and exact, set in Barlow 600.
- **Shape:** 2px corners.
- **Primary:** aqua-ink fill with on-aqua text, 12px × 20px padding. "Your
  first sync".
- **Outline:** a 2px border in the text colour, no fill; the raised tone on
  hover. "Read the introduction".
- **Focus:** a 2px aqua-ink outline, 3px off the element, on every focusable
  thing.

### Terminal strip
The signature component. A command on a dark strip, dark in both themes:
term ground, term-ink text in JetBrains Mono, a term-muted `$` prompt that
is not selected with the text. 4px corners, padding 12px 12px 12px 16px. A
long command wraps inside the strip. Script adds a **Copy** button at the
top right: a term-muted hairline, and inverted (term-ink fill, term text) on
hover. It says "Copied" for 1.5s.

### Beta badge and note
The badge: signal fill, on-signal text, 2px corners, small Barlow 600 text,
next to the logo. The note: a 4px signal bar on the left of a small
paragraph. Both show only while there is no stable release.

### Release versions
One line per channel on the Install heading's line, in Barlow Semi
Condensed, muted: the channel (linked to its book), the version, the date
(linked to the release). With script, each version becomes a small outline
button that copies the install command of its channel.

### Navigation
The header: the logo mark in aqua and "irsync" in Barlow 600 at the
headline size, then Barlow labels in ink with no underline, underlined on
hover. The version switcher sits in parentheses after "Documentation", in
muted text with a ▾. It opens a 4px-cornered list on the ground colour with
a hairline border and the floating-menu shadow. The current book is ticked
and semibold; versions are muted.

### Links in text
Aqua ink, underlined 1px with a 3px offset, 2px on hover.

## Do's and Don'ts

### Do:
- **Do** take every colour, font stack, text size and radius from
  `site/tokens.json`; write `text-muted` or `bg-base-200`, never a raw
  colour.
- **Do** put commands on a terminal strip with a Copy button.
- **Do** keep text at 4.5:1 or better on its ground, and touch targets at
  44px on a coarse pointer.
- **Do** keep the page working without script; script only adds buttons.

### Don't:
- **Don't** use signal yellow for anything but the beta badge and note, or
  as text.
- **Don't** add shadows, gradients, glows or lift-on-hover to flat surfaces.
- **Don't** round corners past 4px, or use pill shapes.
- **Don't** set running text in Barlow or headings in Inter.
- **Don't** add illustrations, stock imagery or decorative icons.
