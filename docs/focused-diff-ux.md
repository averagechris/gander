# Focused diff UX design

Design for the next round of diff-pane UX work: stronger visual change cues,
a collapsible file pane, per-hunk context expansion, a side-by-side view, and
the longer-term "zen mode" focused walkthrough these all feed into.

Status: implemented through §6 (zen mode shipped 2026-07). Suggested
sequencing was bottom-up by risk: cues → file pane → side-by-side →
context expansion → zen.

## Motivation

The unified diff view works but is subtle: `+`/`-` prefixes and fg-only
colors make it hard to see at a glance what changed, the file tree is visual
noise once attention moves to the code, hunks cannot grow beyond the context
jj emitted, and some changes read much better side-by-side. The long-term
goal is a UI that focuses the reviewer on what matters, eventually with an
agent conducting a "zen mode" walkthrough of the most important changes.

## Guiding constraints

- Everything here is **configurable and toggleable at runtime**, with good
  defaults (word-level highlights and line backgrounds on; the rest opt-in).
- Comment anchors (`anchor.rs`) index into `(hunk_index, line_index)` and are
  persisted; nothing below may silently invalidate existing anchors.
- The rows cache (`app/mod.rs::DiffRowsCache`) is keyed by inputs that affect
  row construction; every new knob that changes rows must join the key or
  invalidate it.
- Rendering stays lazy: only the visible window builds spans
  (`render.rs::draw_diff`).

## 1. Diff visual cues

Three independent layers, all runtime-toggleable:

| Cue | Default | What it does |
| --- | --- | --- |
| word-level highlights | **on** | emphasize changed tokens within modified line pairs |
| line backgrounds | **on** | subtle green/red bg tint across added/removed lines |
| gutter bar | off | colored `▎` marker column on changed lines |

### Config

New `[diff]` section in `config.rs`, following the existing patch pattern:

```toml
[diff]
word-highlight = true
line-background = true
gutter-bar = false

[diff.theme]
added-line-bg = "#12261e"   # style specs reuse syntax_style_spec grammar
removed-line-bg = "#301b1f"
added-word = "bold on #1a4a29"
removed-word = "bold on #6b2b2b"
gutter-added = "#3fb950"
gutter-removed = "#f85149"
```

Defaults are GitHub-dark-inspired truecolor tints (the add/remove accents
alpha-blended at ~15% for line backgrounds and ~40% for word emphasis).
Terminals that do not advertise truecolor (`COLORTERM`) get the hex values
quantized to the nearest xterm-256 indexed color at TUI startup, so the
defaults stay usable in e.g. macOS Terminal.app. The existing style-spec
parser (`render.rs::syntax_style_spec`) grows an `on <color>` background
clause; syntax theme specs get it for free.

### Word-level diff algorithm

In `build_diff_rows` (or a sibling module `app/word_diff.rs`):

1. Within each hunk, find maximal runs of Removed lines immediately followed
   by runs of Added lines (the standard change-block shape the parser already
   produces in order).
2. Pair line *i* of the removed run with line *i* of the added run.
3. Word-diff each pair with the `similar` crate
   (`default-features = false, features = ["text", "inline", "unicode"]`;
   sole transitive dep is `unicode-segmentation`). Decision: dependency, not
   vendored and not hand-rolled — a hand-rolled token LCS would perform
   equivalently at this scale (dozens of tokens per line, memoized), but
   `similar` is better tested on unicode segmentation and change-ratio edge
   cases, and vendoring would combine the maintenance cost of hand-rolling
   with none of the upstream benefits.
4. If the pair's similarity ratio is below a threshold (~0.4), skip emphasis
   for that pair — highlighting everything is worse than nothing.
5. Store results on `DiffRow` as `emphasis: Vec<Range<usize>>` (byte ranges
   into `text`).

Cost is bounded per changed-line pair and computed during row construction,
so it is memoized by the rows cache. Unpaired lines (pure additions or
deletions) get no emphasis.

### Rendering and style precedence

`diff_text_spans` (render.rs:385) overlays emphasis ranges onto the existing
syntax spans: split spans at emphasis boundaries and patch backgrounds.
Background precedence, strongest last:

```
line bg  <  word emphasis bg  <  range-selection bg (Blue)  <  cursor bg (DarkGray)
```

Gutter bar renders in the existing gutter column logic (render.rs:264–290),
coexisting with comment-count / `!` flag marks (marks win; the bar fills
otherwise-empty gutter cells on changed lines).

### Cache key

Cues change row content (emphasis) and styling only; emphasis lives in rows,
so `word_highlight` joins the rows-cache key. `line_background` and
`gutter_bar` are render-time only and need no key change.

## 2. Collapsible file pane

A single visibility toggle for the entire files pane.

- `file_pane_visible: bool` on `ReviewSession` (ephemeral; not persisted in
  v1 — revisit if it proves annoying).
- New `Action::ToggleFilePane`, default key `w` (unbound today), also exposed
  in the view-options popup (below).
- `ui_layout` (render.rs:74) drops the 44-col files chunk when hidden; the
  diff pane takes the full width.
- **Never trap the user:** if the pane is hidden and focus is `Files` (or
  `ToggleFocus` targets it), the pane re-shows automatically. Symmetrically,
  file-search (`/`) and next/prev-unviewed keep working with the pane hidden.
- The diff pane title gains the current file path + viewed mark when the tree
  is hidden, so context is never lost.

Optional follow-up (config `ui.file-pane = "visible" | "hidden" | "auto"`):
`auto` hides the pane whenever focus is on the diff and shows it when focus
returns. Opt-in because focus-driven layout shifts can be jarring; the
explicit toggle ships first and `auto` only if wanted.

## 3. View options popup

Cues, pane visibility, and view mode add up to too many toggles for
single-key bindings. Add a small `Mode::ViewOptions` popup on `V` (pattern:
`tui/flags.rs`) listing checkbox toggles:

```
View options
 [x] word-level change highlights
 [x] line backgrounds
 [ ] gutter change bar
 [x] file pane
 ( ) side-by-side   (•) unified
```

Each row maps to an `Action` (`ToggleWordHighlight`, `ToggleLineBackground`,
`ToggleGutterBar`, `ToggleFilePane`, `ToggleDiffViewMode`) so users can also
bind direct keys via `[keybindings]` — all new bindings are remappable like
every existing one. Runtime toggles do not write config; they are
deliberately session-only (decided): config sets defaults, toggles are
transient view state.

## 4. Side-by-side view

### Model

```rust
enum DiffViewMode { Unified, SideBySide }
```

on `ReviewSession`, toggled via the popup or a direct binding (`|`
suggested). Config default under `[diff] view = "unified"`.

**Key decision: the unified row list stays the single source of truth.**
`DiffRow` already carries both `old_lineno` and `new_lineno`; the cursor,
comments, flags, range selection, and anchors all index into the unified
rows and are untouched by view mode. Side-by-side is a *projection* built at
render time (and cached alongside rows):

```rust
struct SplitRow {
    left: Option<usize>,   // index into unified rows (Removed/Context)
    right: Option<usize>,  // index into unified rows (Added/Context)
}
```

Pairing walks each hunk: context/meta/header rows occupy both cells; within
a change block, removed line *i* pairs with added line *i* (same alignment
as word-diff, so intra-line emphasis lines up across the gutter); leftovers
get a blank opposite cell.

### Rendering and interaction

- `draw_diff` splits the area into two halves with a `│` divider; each side
  renders lineno + text with the same span pipeline (word emphasis applies
  per side; line backgrounds fill only the occupied cell).
- The cursor remains a unified-row index; the render maps it to its split
  row and highlights the occupied cell(s). Moving the cursor through a
  change block therefore visits left cells then right cells — matching
  comment-anchor semantics exactly (a comment on a removed line is an
  old-side anchor regardless of view).
- Width fallback: below ~100 columns the view renders unified with a one-time
  notice, rather than producing two unreadable 40-col panes.
- Long lines truncate (as today); no wrapping in v1.

### Snapshots

`tui/snapshots` (insta buffer snapshots) gets side-by-side cases: paired
block, unpaired additions, fold rows, comment gutter marks.

## 5. Hunk context expansion

The inverse of the existing `z` context folding: pull in file lines *beyond*
what the jj diff emitted.

### Source of truth

Full file content per side fetched lazily via jj (`jj file show -r <rev>
<path>`, plumbed through `jj.rs` like existing invocations) and cached per
`(path, revision)` on the session. Context lines are identical on both
sides, so the new-side content suffices; old line numbers derive from the
hunk offsets. Files that fail to load (deleted on that side, weird
encodings) simply don't offer expansion.

### Interaction model (GitHub-style, per gap)

Row construction inserts explicit gap rows wherever hidden lines exist:
above the first hunk, between hunks, and below the last hunk:

```rust
DiffRowKind::ExpandGap { above_hunk: usize, hidden: usize }
```

rendered like `⋯ 34 lines hidden  (+ expand 10, = expand all)`. With the
cursor on a gap row (or on a hunk header):

- `+` expands the gap by `diff.context-step` lines (default 10)
- `=` expands the gap fully
- `-` re-collapses the gap to its original state

Expansion state lives on the session as
`BTreeMap<(String /*path*/, usize /*gap id*/), Expansion>` and joins the
rows-cache key. When a gap shrinks to zero the two hunks render as one
contiguous block (line numbers stay real; the interior hunk header is
dropped from the projection — the underlying `Hunk`s and all anchors are
untouched).

### Expanded rows and anchors

Expanded context rows are synthetic: they have real line numbers but no
backing `(hunk_index, line_index)` in the parsed diff, so in v1 they carry
`anchor: None` (not commentable, decided and accepted) — same treatment as
fold rows today. If commenting on expanded context turns out to matter, a
follow-up can extend `CommentAnchor` with a file-line variant; that is
deliberately out of scope now because it touches persisted state.

Interaction with `z` folding: folding applies to diff-emitted context only;
expanded rows collapse back via `-` rather than participating in symbol
folds. Keeps the two features orthogonal.

## 6. Zen mode (designed and shipped, 2026-07; refocused as a briefing)

Everything above composed into a focused briefing. The organizing idea:
a reviewer's attention is the scarce resource, so the agent budgets it —
a handful of full-screen *focus stops* that teach the critical lines,
then everything mechanical acknowledged in bulk. Design decisions:

- **Zen subsumes tour mode.** The old modal `Mode::Tour` is gone; `T`
  (or `Z`) now enters zen. One walkthrough mode, not two.
- **Three surfaces, one state machine** (`ZenPhase`):
  - *Focus card* (default): a full-screen takeover per spotlight stop —
    progress dots, the stop's critical lines excerpted ± 2 context rows
    and vertically centered, and the agent's `explanation` in a "why
    this matters" panel. One object of attention; no panes.
  - *Reading view* (`tab`/`o` toggles): the normal review UI with
    out-of-range rows dimmed (`Modifier::DIM`, render-time only) and a
    bottom orientation panel. The full normal-mode vocabulary —
    comments, flags, context expansion, split view, search — works here;
    only stop-navigation keys are intercepted.
  - *Glance board* (`g`, or automatically after the last stop): every
    glance chunk plus every file no chunk part covers, one line each
    (title, location, ±stats, rationale, viewed check). `enter` jumps
    into the diff and ends zen; `a` bulk-marks all glance files viewed
    and finishes. The board is modal — other keys are swallowed so
    normal actions cannot fire invisibly.
- **Attention budget is agent-enforced.** The summon prompt instructs
  agents: at most 3–7 `importance=spotlight` chunks, each with precise
  line ranges and a 2–5 sentence `explanation` that teaches the change;
  ALL remaining hunks grouped into `importance=glance` chunks. Uncovered
  files still land on the glance board, so the briefing always covers
  the whole change even with a sloppy agent.
- **Chunkless fallback.** With no agent chunks, stops are one-per-file in
  display order (agent `set_ordering` respected). Zen is useful
  standalone; agents upgrade it from "flip through files" to "be taught
  the change".
- **Framing.** The file pane hides on entry (visibility restored on
  exit). In the reading view, rows outside the stop's range dim; the
  cursor row never dims. Whole-file stops dim nothing.
- **Stacked walkthroughs.** Chunks anchored to a jj change (`change_id`)
  make zen retarget the review to that change's own diff
  (`change-..change`) for the stop — the tour flows through the stack
  like stacked PRs, and ending zen returns to the home target. Zen-driven
  retargets update the staleness key, so they never end the walkthrough.
- **Chapters** (2026-07 follow-up). A bare retarget taught nothing: the
  human landed on a change id they knew nothing about. The stop list is
  now organized into chapters — every run of stops anchored to the same
  change opens with a full-screen *chapter card* carrying that change's
  jj metadata (description, bookmarks, live diff stats) plus the agent's
  high-level *change brief* (`review/set_change_briefs`: one `{change_id,
  summary}` per change — what it accomplishes, why it exists, how it
  builds on the previous changes). Every walkthrough gets an opening
  chapter for its home target (single-change targets resolve their
  description; multi-change ranges stay generic rather than showing the
  tip's description), so even the chunkless fallback starts with the big
  picture. Chapter cards mark nothing viewed; the progress strip renders
  chapters as `▎` bars grouping the stop dots; human-facing stop numbers
  count spotlight stops only.
- **Artifacts** (2026-07 follow-up). Agents can *show* instead of only
  telling: spotlight chunks and change briefs may carry `artifacts`
  (`{title, kind: example|output|diagram|note, body}`) — a usage example
  of the changed API, output the agent captured by running the code, an
  ASCII diagram of the new flow. Cards with exhibits show an `e` hint;
  `e` opens a modal scrollable viewer over the focus card (`j`/`k`
  scroll, `h`/`l` cycle, `esc` closes). Bodies render verbatim.
- **Safety.** *User* retargeting (t/p/b/R, stack step, operation picker)
  invalidates the stops; the walkthrough ends with a notice rather than
  touring a stale map. A live refresh of the same target (new changes
  landing) instead rebuilds the stops in place. Runtime state is
  session-only, consistent with §3.

Config: the `tour` keybinding is renamed `zen` (serde alias keeps old
configs working); defaults are `T` and `Z`.

## Sequencing

1. **Visual cues** (§1) + view-options popup (§3) — self-contained, biggest
   at-a-glance win, establishes the `[diff]` config section.
2. **File pane toggle** (§2) — small, independent.
3. **Side-by-side** (§4) — render-side projection over unchanged model.
4. **Context expansion** (§5) — the only piece needing new jj plumbing and
   the most cache/anchor care.
5. **Zen mode** — separate design after 1–4 are in use (done; see §6).

Each step lands with unit tests (word-diff LCS via proptest, pairing,
gap/expansion math) and insta snapshot coverage, and passes `jj lint`.

## Open questions

- Side-by-side + very long lines: is truncation acceptable long-term, or is
  horizontal scroll/wrap needed?

## Resolved decisions (2026-07)

- Word diff uses the `similar` crate as a slim dependency (not vendored,
  not hand-rolled) — see §1.
- Expanded context rows are not commentable in v1.
- Runtime view toggles are session-only; config sets defaults.
- Keybind defaults (`V`, `w`, `|`, `+`/`=`/`-`) accepted; all remappable
  through `[keybindings]`.
- Cue color defaults are truecolor hex with automatic nearest-indexed
  quantization on non-truecolor terminals (resolves the former open
  question about indexed defaults).
