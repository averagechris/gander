# Scenario 2 — TUI review + zen (uncurated vs curated), round 6 recheck

Evaluator: fresh-eyes dogfood run against `target/release/gander` on the
`/tmp/gander-eval/round6-s2` fixture (3-change stack: feat → refactor → tests,
empty `@` on top). TUI driven via tmux (200x50); curation via `gander acp`
JSON-RPC and the `gander chunks`/CLI path. No gander source read.

# Summary

The core review loop is in good shape: the file tree, enter-to-mark-viewed,
`>`/`<` stack stepping, inline comments with `s` state cycling, and the
comment list all worked first try and were discoverable from the
task-oriented "first review loop" section of `?`. Curation tooling is the
standout: `gander chunks lines` plus the spec-file CLI made authoring easy,
and the invalid-chunk error message is a model of what validation should look
like (names the bad part, prints the valid ranges, tells you which command
lists them).

The most important gap found: **the live TUI silently dropped curated chunks
after a working-copy snapshot reload**. The overlay file kept them
(`gander chunks list` showed 4), the TUI said "agent suggestions updated",
but `S` reported "no review chunks suggested" and zen toured derived stops —
while still rendering my briefs under "what this change does" beneath a
banner claiming "uncurated tour". Re-running the identical `chunks set`
fixed it. Until live-session and overlay state can't diverge, an agent's
curation work can vanish without anyone being told.

Secondary gaps: uncurated zen's derived facts have real inaccuracies
(`impl Default for Priority` rendered as a duplicate "impl Priority";
dependency claims are file-overlap-only and miss the actual semantic
dependencies), the same canned review question repeats on most stops, and
human comments are invisible on zen stops that cover their exact lines.

# Step log

## Setup and help

```sh
nix shell nixpkgs#tmux --command tmux new-session -d -s gander-tui-eval -x 200 -y 50 \
  -c /tmp/gander-eval/round6-s2 -- '.../gander -b main -r @ tui'
```

- Launch: footer read `7 files (0/7 viewed…), +134/-12, 0 comments ·
  following @ · @ vzpqxzrx (no description)` — target, follow mode, and
  progress all visible. Bottom hint line lists the key core keys.
- `?`: help opens with a **"first review loop"** column (`/` open, `]` next
  unviewed, `enter` mark viewed and advance, `c` comment, `T` zen, `ctrl-y`
  handoff) before the exhaustive listing. Task-oriented enough for a
  first-timer; the sheer density (~60 bindings) is a lot, but the loop
  section carries you.

## Normal review pass

- `>` stepped to `stack 1/3: feat: priority scheduling…`; file list narrowed
  to that change's 3 files; footer updated to `@ ssxrpvoz feat: …`. `>` at
  3/3 says `already at the top of the stack` (does not fall onto the empty
  `@`) — good.
- Marked `src/priority.rs` viewed with `enter` (✓ in tree, `1/3 viewed`).
- In change 2, tabbed into the diff of `src/retry.rs`, moved the cursor with
  `n`/`e`. Overshot to line 22 first (`c` dialog title shows the anchor —
  `comment · src/retry.rs:22` — which saved me), adjusted, commented on
  line 20 (the off-by-one). Saved with `ctrl-s`; rendered inline as
  `↳ 1147cb6a [draft] …`.
- Comment state: pressed `s` at the cursor → `[todo]` → `[resolved]` →
  `[draft]` (cycle confirmed, info line announces each). Also discoverable
  from `C` comment list, which offers `s state · a action · K kind · x
  delete`. Flow is discoverable from help ("s cycle comment state") and from
  the comment list footer.
- Chrome: footer always showed file counts, +/- churn, comment count, follow
  mode, and current change id + description; the info line narrates stack
  position ("stack 2/3: refactor: …"). One capture caught the footer showing
  the previous change's summary for ~a second after `>` while the info line
  already showed the new one.

## Zen, uncurated (stop-by-stop log)

Entered with `T` from launch target. Header: `zen · chapter 1/3 · then 1 at
a glance`, progress dots `▎○ ○ ▎○ ○ ▎○ ○` (3 chapters × 2 auto stops).

- **Chapter 1 card** (feat): commit description body rendered, then
  `uncurated tour — derived from the diff; press @ to summon ACP/agent
  curation for intent/risk`, then derived facts: `roles: 3 source`, `churn:
  +52 −5 · no tests touched` (accurate), `top changed symbols: mod priority,
  enum Priority, impl Priority, fn default, fn as_str`, then a bare `…` line.
  `d` toggled the *description*, never the hidden facts; nothing told me what
  `…` was hiding or how to reveal it.
- **Stop 1/6** `src/queue.rs:24-58`: real content added — "largest hunk +18
  −4 · symbols: fn enqueue, fn enqueue_with_priority, fn dequeue, fn len, fn
  is_empty · new public API: pub fn enqueue_with_priority( · review question:
  is this the right surface to expose?". The new-public-API callout is
  genuinely useful beyond browsing.
- **Stop 2/6** `src/priority.rs:1-25`: symbols listed as `enum Priority,
  impl Priority, fn default, impl Priority, fn as_str` — **"impl Priority"
  appears twice**; the diff actually has `impl Default for Priority` and
  `impl Priority`. Same canned review question repeated.
- **Chapter 2 card** (refactor): churn `+52 −6` ✓, `no tests touched` ✓,
  `public API: 3 file(s) change pub signatures` ✓ (lib.rs, retry.rs,
  worker.rs all do). Dependency claim: `builds on ch.1 "feat: …":
  src/lib.rs` — technically true but the *real* dependency is worker.rs
  calling `enqueue_with_priority`/`job.priority` from ch.1; only the shared
  boilerplate file is named.
- **Stop 3/6** `src/worker.rs:1-41`: `new public API: pub fn dropped(&self)
  -> u64` ✓. Same canned question.
- **Stop 4/6** `src/retry.rs:1-22`: covers exactly the lines where my
  `[todo]` comment sits — **the comment does not render on the stop**. Facts
  accurate otherwise.
- **Chapter 3 card** (tests): `roles: 1 source · 1 tests` ✓, `tests touched`
  ✓, symbols ✓. **No builds-on claim at all**, although the tests exercise
  `Priority` (ch.1) and `w.dropped()` (ch.2) — the file-overlap heuristic
  finds nothing because the files differ.
- **Stops 5–6/6** config.rs / tests/basic.rs: accurate hunk facts; stop 6
  correctly drops the review-question boilerplate (no new pub API).
- **Glance board**: exactly 1 item, `src/lib.rs — source · ch.1, ch.2` —
  deduplicated across the two changes that touched it, attributed to both
  chapters. `a` marked it viewed, exited zen, and the file tree showed 7/7 ✓
  (state stuck; survived returning to launch target).
- Intent: only via commit messages. Risk: not surfaced (the flagged BUG
  comment in the diff is never highlighted). Review questions: one generic
  template repeated. Dependencies: file-overlap only.

## Curation (live TUI running)

- `gander acp` bridged and *announced it*: stderr
  `gander acp: bridged to live TUI session (target main..@)` before serving.
  `initialize` reports `"mode":"live-bridge"`. Clear.
- `review/stack_changes` gave full change ids + multiline descriptions —
  the one place change ids are easy to harvest.
- **Briefs via ACP JSON-RPC** (one multi-sentence brief for the refactor):
  accepted with three advisory warnings — `brief for change … has no
  spotlight chunk yet and will not render on a curated zen chapter right
  now`. Accurate, non-blocking, exactly right.
- **Chunks via CLI**: `gander chunks lines --change <id>` printed the exact
  accepted line spaces per file (with hunk excerpts for orientation);
  authored a 4-chunk spec (3 spotlight + 1 glance, one chunk with 2 parts
  across retry.rs/worker.rs, one artifact) → `chunks set --file` → `Set 4
  chunks`.
- **Invalid chunk** (`start_line: 200` + nonexistent file) via
  `chunks update` on stdin:

  ```
  error: invalid chunk part(s): chunk 'Bad range' part 1 (src/retry.rs):
  line range outside diff line space for src/retry.rs; valid ranges for
  src/retry.rs: 1-22; chunk 'Bad range' part 2 (src/nonexistent.rs): file
  not present in change vlsltmuklmtw's diff: src/nonexistent.rs; run
  'gander chunks lines' to list accepted ranges
  ```

  This tells you what would be valid. All-or-nothing rejection confirmed.
- **Incremental edit**: `chunks update` with the same id replaced in place
  (`Updated 1 chunks, added 0; total 4`).
- **Draft via ACP** `review/draft_comment` on worker.rs:30 → id returned,
  TUI info line said `agent suggestions updated`, `D` panel showed it
  `[pending]` with accept/edit/discard; `a` accepted it into a real comment
  (count 1 → 2).
- **The divergence bug**: my spec files were written inside the fixture repo,
  so `@` snapshotted them (footer briefly `10 files`). After moving them out,
  the TUI reloaded (`repository changed — change vzpqxzrxnlxt updated · op:
  snapshot working copy`). After that reload: briefs and the pending draft
  survived, but **chunks were gone from the live session** — `S` said
  `no review chunks suggested`, and `T` toured the 6 *derived* stops while
  rendering my briefs under "what this change does" beneath the "uncurated
  tour" banner. Meanwhile the bridged `gander chunks list` still returned all
  4 chunks. Re-running the identical `chunks set` restored them instantly.

## Zen, curated (after re-apply)

Info line: `zen: 3 chapter(s), 4 focus stop(s), 3 at a glance`; the
"uncurated tour" banner disappeared.

- Chapter cards: brief rendered in full (my 6-sentence refactor brief shown
  without truncation) under **what this change does**; derived facts demoted
  to a single compact `derived: …` line with `d for detail`. Reads as a
  briefing.
- Stop 1 = my `queue-banding` chunk: title in header, explanation as "why
  this matters", `e` opened the dequeue-order artifact in an overlay.
- Stops 2–3 = the two parts of `retry-off-by-one` — but the explanation is
  repeated **verbatim** on both parts, and chapter cards + briefs + stop
  explanations overlap when the author (me) restates the same bug three
  times; the tool does nothing to dedupe or vary part rendering.
- Glance board: my glance chunk with its rationale + change attribution,
  plus `src/config.rs +11 -0 — source · uncovered` and `src/priority.rs —
  source · uncovered` — uncovered files are surfaced rather than silently
  dropped, with viewed markers. `a` finished and the tree stayed 7/7 ✓.
- Footer during zen shows counts scoped to the currently-retargeted change
  (`2 files (2/2 viewed)` on the glance board that spans the whole stack) —
  mildly disorienting.

Session killed with `tmux kill-session` after `q` (artifact written on quit).

# Findings

1. **major — live session silently drops curated chunks on repo reload.**
   Repro: TUI running on `main..@`; `gander chunks set --file spec.json`
   (works, `S` shows them); cause a working-copy snapshot (add/remove any
   file in the repo so the TUI logs `repository changed … op: snapshot
   working copy`); `S` now says `no review chunks suggested` and zen tours
   derived stops, while `gander chunks list` (bridged to the same live
   session) still returns all chunks. Briefs and pending drafts survive the
   same reload. No warning that curation was discarded; re-running the same
   `chunks set` restores everything, proving the chunks were still valid.

2. **major — contradictory curation state rendering.** With briefs present
   but chunks dropped (state from finding 1), the chapter card shows the
   agent brief under "what this change does" directly beneath `uncurated
   tour — derived from the diff; press @ to summon ACP/agent curation for
   intent/risk`. The card is simultaneously claiming there is and isn't
   agent curation. Repro: set briefs only (no chunks), open `T`.

3. **minor — derived symbol facts are wrong for trait impls.** Uncurated
   stop for `src/priority.rs:1-25` lists symbols `enum Priority, impl
   Priority, fn default, impl Priority, fn as_str`; the diff contains `impl
   Default for Priority` and `impl Priority`. The trait impl is mislabeled
   and appears as a duplicate. Repro: fixture change `ssxrpvozolow`, zen
   stop 2.

4. **minor — dependency claims are file-overlap only.** Chapter 2 claims
   `builds on ch.1 … : src/lib.rs` (true but names only shared boilerplate,
   not `worker.rs`'s call into ch.1's `enqueue_with_priority`); chapter 3
   (tests exercising both prior changes) gets **no** builds-on line because
   it shares no files with them. The heuristic misses exactly the
   dependencies a reviewer cares about. Repro: compare chapter cards against
   `jj diff -r <change> --git`.

5. **minor — one canned review question repeated on nearly every stop.**
   `review question: is this the right surface to expose?` appeared verbatim
   on 4 of 6 uncurated stops (any stop with a new pub symbol). By stop 3 it
   is noise the eye skips. Repro: uncurated `T`, step stops.

6. **minor — human comments invisible in zen.** My `[todo]` comment on
   `src/retry.rs:20` did not render on the uncurated stop covering
   `src/retry.rs:1-22` (nor on curated stop 2 covering :9-22). Zen presents
   lines you have annotated with no trace of the annotation. Repro: comment
   on a line, enter `T`, visit the stop containing it.

7. **papercut — unlabeled `…` truncation on chapter cards.** Chapter 1's
   derived facts end in a bare `…` with no expansion hint; `d` toggles the
   *description*, not the facts, so whatever was elided (public-API/builds-on
   lines, judging by chapter 2) is unreachable. Repro: uncurated `T`,
   chapter 1 card, press `d` twice.

8. **papercut — no go-to-line for comment targeting.** Reaching diff line 20
   means counting `n` presses; I overshot to :22 and only the comment
   dialog's title revealed it. A `:`/goto or comment-anchor preview before
   opening the editor would remove the retry loop. Repro: tab into a diff,
   try to comment on a specific numbered line.

9. **papercut — multi-part chunk explanation repeated verbatim.** A chunk
   with 2 parts becomes stops `(part 1/2)`/`(part 2/2)` each carrying the
   full identical explanation, doubling reading load. Repro: curated spec
   with a 2-part chunk, tour both stops.

10. **papercut — zen chrome scoped to the retargeted change.** During zen the
    footer shows the current chapter's change (`2 files (2/2 viewed)`) even
    on the stack-wide glance board; momentarily suggests the review is
    smaller than it is. Repro: reach the glance board, read the footer.

11. **papercut — `draft_comment` line space undocumented.** docs/acp.md
    specifies the chunk-part line space precisely (per-change diff via
    `chunks lines`) but says nothing about which line space/side
    `review/draft_comment.line` uses against the session diff; I guessed
    new-side unified and it happened to land. Repro: docs/acp.md §Write
    methods.

# Scores

- **TUI review ergonomics: 4/5.** The first-review-loop help section, enter
  mark-and-advance, `>`/`<` stack stepping with narrated position, inline
  comment + `s` state cycling (discoverable in two places), and the live
  repo-change follow are all fast and confidence-inspiring. Held back by
  line-targeting friction (finding 8), the dense help wall after the loop
  section, and small chrome lags — not by anything unreliable.

- **Zen uncurated: 3/5.** Genuinely calmer than browsing: chapter framing,
  accurate churn/tests-touched/pub-API facts, new-public-API callouts, viewed
  progress as you tour, and a deduplicated attributed glance board whose `a`
  sticks. But insight is shallow — one recycled review question, mislabeled
  trait-impl symbols, file-overlap-only dependency claims, no risk surfacing,
  and your own comments are invisible (findings 3–6).

- **Zen curated: 3.5/5.** When the chunks are actually in the session, this
  is a real briefing: brief prominent with derived facts demoted to one line,
  authored titles/explanations on stops, artifacts on `e`, uncovered files
  surfaced on the glance board. But the silent chunk drop (finding 1) cost
  trust and a debugging round trip, the mixed state rendered contradictory
  chrome (finding 2), and multi-part stops repeat content verbatim.

- **Curation protocol ergonomics: 4/5.** The CLI path is discoverable
  (`chunks --help` embeds the spec shape), materially easier than raw
  JSON-RPC (spec files/stdin, exit codes, pretty errors), `chunks lines`
  solves the line-space problem outright, validation errors state what would
  be valid, updates are truly incremental, brief warnings are advisory and
  accurate, and `gander acp` explicitly announces live-bridge vs snapshot.
  Deductions for the live/overlay divergence (writes report success the TUI
  then ignores) and the undocumented draft line space.

# Top proposals

1. **Never silently discard curation.** On repository reload, revalidate
   overlay chunks against the refreshed diff and either keep the valid ones
   or emit a visible notice ("N curated chunks dropped: <reasons>") — and
   make the live session re-ingest from the overlay file so `chunks list`
   and the TUI can never disagree.
2. **Make partial-curation rendering honest.** If briefs exist but no
   spotlight chunks, say so ("agent briefs present; stops are derived —
   chunks pending") instead of stamping "uncurated tour" above an agent
   brief.
3. **Show existing comments on zen stops.** Any stop whose range contains a
   comment (human or accepted draft) should render it inline and/or badge the
   progress dot; comments are the review's most important state.
4. **Upgrade derived insight.** Fix trait-impl symbol labeling
   (`impl Default for Priority`), extend builds-on beyond file overlap
   (symbol references across changes would have caught worker→queue and
   tests→both), and vary or suppress the "right surface to expose?" question
   after its first appearance per chapter.
5. **Line targeting: goto-line in the diff pane** (and show the would-be
   anchor in the status line while moving), so commenting on a known line
   number doesn't need keypress counting.
6. **Small zen polish:** label the chapter-card `…` with its expansion key
   (or fold it into `d`), render multi-part chunk explanations once (parts
   share a header, subsequent parts get "continued"), and scope the zen
   footer to the whole tour rather than the retargeted change.
