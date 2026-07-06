# Summary

**Verdict: the live-refresh bug is genuinely fixed.** This re-run exercised every
mutation class from the scenario (working-copy edit, rapid edits, edit of a
viewed file, `jj describe`, `jj new`, `jj abandon`, `jj undo`) against
`gander -b main -r @ tui` and the pane refreshed correctly within the ~2s poll
window every single time. Symbolic `@` was followed across `describe`/`new`/
`abandon`/`undo` with precise, correct footer notices ("@ moved to
nnxxrolvzzrl · change nnxxrolvzzrl entered range · change kmmvtpxnplor left
range"). Selection, the saved line comment (anchored at `src/priority.rs:4`),
and range stats survived every mutation. Rapid successive edits coalesced
cleanly with no flicker or stale state.

The remaining gaps are in *change awareness*, not freshness. The most important
product gaps, in order:

1. **`}`/`{` changed-hunk navigation is broken after the first jump.** It jumps
   to the nearest changed hunk once, then repeated presses never advance to the
   other files' changed hunks. You cannot "cycle through what's new", which is
   the whole point of the affordance.
2. **Change/viewed markers decay too eagerly and inconsistently.** The `±`
   (changed since last look) badge is cleared by merely selecting the file in
   the tree — before you've read any hunk — and the `~` (viewed-but-changed)
   badge is silently dropped on the *next* refresh of any unrelated file,
   leaving a previously-reviewed file indistinguishable from a never-viewed
   one. After a busy hour beside an agent, the badges no longer reconstruct
   what needs re-review.
3. **Popups silently freeze polling with no indicator.** Opening the activity
   feed — the surface you use to catch up — pauses refresh with nothing on
   screen saying so. An edit made while the feed was open did not appear until
   Esc, and there is no "paused"/last-refreshed indicator anywhere.
4. **The watch affordances are undiscoverable.** The `?` help documents neither
   `ctrl-a` (activity feed) nor `}`/`{`, and there is no legend anywhere for
   the `•`/`±`/`~`/`✓`/`◐` badges; I had to reverse-engineer their semantics
   experimentally.

# Step log

Environment: fixture at `/tmp/gander-eval-r4/fixture-3` (5-commit jj repo,
task-queue library, `@` initially an empty change on top of the stack).
Launched in tmux (200x50): `gander -b main -r @ tui`. All jj commands run with
eval identity and `signing.behavior=drop`.

## Launch and seeding

- Footer on launch: `main..@ · focus files … following @` — follow mode is
  clearly indicated. Baseline: `7 files (0/7 viewed), +134/-12, 0 comments`.
- Marked `src/lib.rs` viewed (Enter). Observation: the file re-sorted to the
  bottom of its directory and selection auto-advanced to the next unviewed
  file. Count `1/7 viewed`.
- Added a line comment on `src/priority.rs:4` (`c`, type, ctrl-s). Rendered
  inline: `↳ 46a71c0a [draft] Consider deriving Hash for Priority too`.
  Footer `1 comments`. The comment popup gives no indication of which
  file/line it is anchored to.

## Mutation 1 — working-copy edit to an already-changed file

`printf ... >> src/worker.rs` (append 2 lines), wait 5s.

- Refreshed: `worker.rs` badge `•` → `±`, stats `+134/-12` → `+136/-12`.
- Footer notice: `info: repository changed — change muruxutovytz updated`.
- Selection (priority.rs) and comment untouched. **Pass.**

## Mutation 2 — rapid successive edits

Three appends to `src/worker.rs` at 0.5s intervals, wait 5s.

- Coalesced into one consistent state: `+139/-12`, still `±`, same notice, no
  flicker or intermediate corruption observed. **Pass.**

## Mutation 3 — edit a file already marked viewed

`printf ... >> src/lib.rs`, wait 5s.

- `lib.rs` badge `✓` → `±`; viewed count dropped `1/7` → `0/7`.
- On navigating to lib.rs the badge became `~` — a distinct viewed-but-changed
  treatment exists. The diff correctly showed the new trailing line.
- **However**: after the next repo mutation (`jj new`), lib.rs decayed to plain
  `•` — the memory that it was ever viewed, and that it changed since viewing,
  was lost. Re-confirmed later with config.rs (see Findings F2). **Partial
  pass: distinct state exists but does not persist.**

## Mutation 4 — describe, new, edit a different file (@ moves)

- `jj describe -m 'wip: worker tuning notes'`: refreshed, no spurious range
  change (correct — same change identity).
- `jj new` + append to `src/queue.rs`, wait 5s:
  - Footer: `info: repository changed — @ moved to nnxxrolvzzrl · change
    nnxxrolvzzrl entered range`.
  - `queue.rs` gained `±`, stats `+141/-12` → `+143/-12`, selection preserved.
  - **Pass — @ was followed with an explicit, correct notice.**

## Mutation 5 — abandon and undo

- `jj abandon @`: notice `@ moved to kmmvtpxnplor · change kmmvtpxnplor
  entered range · change nnxxrolvzzrl left range`; stats reverted to
  `+141/-12`; queue.rs correctly kept a `±` (its content reverted — a change
  since last look). **Pass.**
- `jj undo`: notice `@ moved to nnxxrolvzzrl · change nnxxrolvzzrl entered
  range · change kmmvtpxnplor left range`; stats back to `+143/-12`. **Pass.**
- Comment still intact and correctly anchored after both (verified via `C`
  comment list: `[draft] src/priority.rs:4 …`).

## Legibility affordances

- **Activity feed (ctrl-a):** full timestamped history of every refresh,
  including the @-move and range enter/leave events. Three problems:
  timestamps are UTC (`01:11:45` shown when local time was `18:11` PDT); every
  refresh is logged twice (a combined `repository changed — X · Y · Z` line
  *plus* one line per component); entries carry no file names, so "change
  muruxutovytz updated" ×5 tells you nothing about *what* to re-read.
- **Popup pause:** with the feed open I appended to `src/config.rs` and waited
  6s — no refresh, no "paused" indicator. After Esc, the refresh landed within
  the poll window (`±` on config.rs, `+144/-12`). The pane you open to catch
  up is exactly the thing that freezes updates, invisibly.
- **`}`/`{` changed-hunk nav:** first press correctly jumped cross-file to
  queue.rs's hunk labeled `@@ -18,21 +24,37 @@  changed`. Every subsequent
  `}` or `{` press stayed pinned there, never advancing to the changed hunks
  in worker.rs or config.rs (both `±` at the time). From worker.rs, one `}`
  wrapped to config.rs's changed hunk, then got stuck again. Cycling is
  broken.
- **Help (`?`):** documents neither `ctrl-a` nor `}`/`{`, and has no badge
  legend. The `V` view-options popup has no legend either.
- Session ended with `q` (artifact written; "No walkthrough steps recorded"
  printed to the shell); tmux session killed.

# Findings

## F1 — `}`/`{` changed-hunk cycling gets stuck after the first jump (major)

Repro: with fresh edits in 3 files (config.rs, queue.rs, worker.rs all `±`),
focus the diff pane and press `}` repeatedly. First press jumps to the nearest
`changed`-labeled hunk (queue.rs); all further `}`/`{` presses re-target the
same hunk and never advance to worker.rs's or config.rs's changed hunks.
Impact: the primary "show me what's new since I looked away" navigation only
ever shows you one thing.

## F2 — change/viewed acknowledgment is too eager and not durable (major)

Repro A: file with `±` (worker.rs) — select it in the file tree without
scrolling to its new hunks → badge immediately clears to `•`. Selection is
treated as "seen" even though the fresh hunks were never on screen.
Repro B: mark config.rs viewed (`✓`), append a line → `±` (viewed count drops
to 0/7, correct), navigate to it → `~` (viewed-but-changed, good), then append
a line to *tests/basic.rs* and wait one poll → config.rs decays to plain `•`.
The fact that it was viewed, and changed since viewing, is gone.
Impact: over a day of agent activity the badges cannot be trusted to
reconstruct the re-review backlog; only the very latest delta is visible.

## F3 — polling pauses invisibly while any popup is open (major)

Repro: open the activity feed (ctrl-a), append to a file, wait >5s → no
refresh, no indicator that polling is paused; press Esc → refresh arrives.
The behavior itself is defensible (don't mutate state under a modal), but it
is unmarked: no "paused" tag on the popup, no last-refresh timestamp in the
footer to reveal staleness. Leaving help or the feed open in an ambient pane
silently turns it into a stale pane.

## F4 — watch affordances and badge vocabulary are undocumented (minor)

Repro: press `?`. Neither `ctrl-a` (activity feed) nor `}`/`{` (changed-hunk
nav) appears anywhere in the help. No legend exists for `•`, `±`, `~`, `✓`,
`◐` in help or the `V` view-options popup. I derived `±` = changed since last
look, `~` = viewed-but-changed by experiment.

## F5 — activity feed timestamps are UTC, not local (minor)

Repro: mutate the repo at 18:11 PDT, open ctrl-a → entry reads `01:11:45`.
For a "what happened while I was away" timeline, wrong-timezone timestamps
actively mislead recency judgments.

## F6 — activity feed is duplicative and not actionable (minor)

Every refresh logs a combined summary line plus one line per component event
(same timestamp), doubling the noise. Entries name change IDs but never files
(`change muruxutovytz updated` ×5 in a row), and there is no jump-to-target
from an entry. Catching up still requires manually hunting badges in the tree.

## F7 — no liveness/last-refresh indicator (papercut)

The footer shows `following @` but never *when* the pane last refreshed. In a
quiet repo you cannot distinguish "healthy and idle" from "wedged" (and per F3,
wedged-by-popup is a real state). One timestamp would buy a lot of trust.

## F8 — comment popup shows no anchor (papercut)

Repro: press `c` on a diff line. The popup is titled only "comment" and shows
no file:line, so you cannot confirm what you are annotating until after save.

## F9 — viewed files re-sort to the bottom of their directory (papercut)

Marking a file viewed reorders the tree under you (lib.rs jumped below
worker.rs). Selection follows correctly, but the tree layout churns with every
viewed toggle, which fights spatial memory in a long-lived pane.

# Scores

## Watch freshness/follows-@ : 4/5

Every mutation class refreshed correctly and promptly: plain edits, rapid
bursts, `describe`, `new`, `abandon`, `undo`. `@` was followed with explicit,
accurate revision-identity notices ("@ moved to …", "change … entered/left
range"), and selection/comments/stats were preserved throughout — this meets
most of the 5-anchor ("timely, correct, visibly tied to revision identity,
robust across edits, jj new, undo, abandon"). Withholding the 5 because
freshness is not always *verifiable*: polling pauses invisibly under popups
(F3) and there is no last-refresh indicator (F7), so a user cannot always tell
live from stale.

## Watch change awareness : 3/5

Clearly above the 3-anchor's "aggregate stats only": per-file `±` badges, a
distinct `~` viewed-but-changed state, hunk-level `changed` labels, specific
footer notices, and a persistent activity feed all exist. But the delta system
does not hold up under sustained use: `}`/`{` cycling is broken (F1), badge
state is consumed by mere selection and dropped on the next refresh (F2), the
feed lacks file names and duplicates itself (F6), and the whole vocabulary is
undocumented (F4). The 5-anchor's "navigation to fresh work" is precisely the
broken part.

## Pane-worthiness : 3.5/5

The trust problem from the previous run is gone: I would now leave this pane
open beside an agent and believe its diff and stats. Comments and viewed state
survive repo churn, and @-following notices are exactly what an ambient
reviewer wants. It falls short of 4-5 because it cannot yet answer "what do I
need to re-review since lunch?" — acknowledgment decay (F2), stuck hunk
navigation (F1), and the invisible popup-pause (F3) mean catching up after
looking away still degrades to re-scanning the tree manually.

# Top proposals

1. **Fix `}`/`{` to cycle through all changed hunks across files** (F1), with
   wraparound and a counter ("changed hunk 2/5") in the footer. This is the
   single highest-leverage catch-up affordance and it currently dead-ends.
2. **Make change acknowledgment explicit and durable** (F2): keep `±` until
   the changed hunks have actually been on screen (or until an explicit
   "acknowledge" action), and persist `~` viewed-but-changed across refreshes
   until the file is re-marked viewed. Surface a "N files need re-review"
   count in the footer distinct from unviewed.
3. **Add a liveness/paused indicator** (F3, F7): footer shows "refreshed 4s
   ago"; while a popup suppresses polling, show "paused (popup)" and flush a
   refresh immediately on close. Consider letting the activity feed itself
   live-update since it is the catch-up surface.
4. **Make the activity feed actionable and legible** (F5, F6): local-time
   timestamps, one entry per refresh with file names ("worker.rs +3 · queue.rs
   +1"), Enter to jump to the file/hunk, and collapse the duplicate
   component lines.
5. **Document the watch vocabulary** (F4): add `ctrl-a` and `}`/`{` to help,
   plus a one-line badge legend (`± changed since last look · ~ viewed but
   changed · ✓ viewed`), ideally rendered in the files-pane title or help
   header.
