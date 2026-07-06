# Summary

Fixture: a 4-change jj stack (task queue library: initial lib → priority scheduling → retry-policy extraction with an intentional off-by-one → tests), reviewed with `gander -b main -r @ tui` inside tmux (200x50), release binary.

Overall verdict: the core review loop (tree navigation, diff reading, viewed marks, line comments) is fast and pleasant, the new "first review loop" help section is genuinely task-oriented, and curated zen — briefs on per-change chapter cards, spotlight explanations, artifacts — is a real, working guided briefing. The chunks CLI makes curation dramatically easier than raw JSON-RPC.

But I hit a **blocker: saved review comments are silently and permanently erased** when the review target lands on an empty diff (e.g., stepping the stack to the empty `@` with `>`, which "first review loop" adjacent keys invite you to do). Both my hand-written comment and an accepted agent draft were destroyed mid-session, twice. Until that's fixed I would not trust gander with real review notes.

Most important product gaps, in order:

1. Comment/viewed state is destroyed by ordinary target navigation (empty-diff target persists `comments: []`).
2. Curated zen chapter cards print session-global derived stats (roles/churn/symbols) that contradict the per-change numbers on the same card.
3. There is no "return to launch target" affordance; stack stepping dead-ends on the empty working copy, `t` goes to `trunk()..@` (not the `-b main` I launched with), and the base chooser's fuzzy filter doesn't rank the exact `main` bookmark match first.
4. Uncurated zen's "why this matters" section contains no *why* — only a viewport note ("Showing the largest of 2 hunk(s)") — and it ignores the stack structure it demonstrably has access to.
5. CLI parity gaps: briefs, draft comments, flags, and ordering are ACP/JSON-RPC-only; `chunks list` output can't be piped back into `chunks set`.

# Step log

All TUI interaction through `nix shell nixpkgs#tmux --command tmux ...`, session `gander-tui-eval`, 200x50, killed at the end.

## Launch and help

```sh
tmux new-session -d -s gander-tui-eval -x 200 -y 50 -- \
  'cd /tmp/gander-eval-r3/fixture-2 && …/gander -b main -r @ tui'
tmux send-keys -t gander-tui-eval '?'
```

Startup showed the files tree (7 files, `main..@`, +134/-12) with the first file's diff, tree-sitter status line, and a footer keymap. `?` opens help whose **first section is "first review loop"**:

```
first review loop
      /  open a file or fuzzy jump
      ]  next unviewed file
  enter  mark viewed and advance
      c  comment on what needs work
      T  zen briefing for a focused pass
 ctrl-y  copy handoff when done
```

Verdict on help: this is a real improvement — it's an ordered task recipe, not a key dump, and it matches what I actually did. Two gaps: it doesn't mention `>/<` stack stepping (the natural next question for a stacked fixture; it's buried in "targets & jj"), and `n/e` movement (Colemak-style) is stated but not explained, so a first-time QWERTY user briefly wonders where j/k went (j/k do work in popups, inconsistently with panes).

## Normal review

- `n` in the tree moved to `lib.rs`, diff followed instantly. `Enter` marked it viewed (`✓`), advanced to `priority.rs`, and re-sorted the viewed file to the bottom of its directory group — slightly disorienting the first time, but defensible.
- `Tab` into diff, `n`×4 to line 5, `c` opened the comment editor. **The editor popup gives no indication of which file/line it's anchored to** — title is just "comment".
- Typed a comment, `C-s` saved: rendered inline as `↳ f76d5a66 [draft] Consider deriving Ord…` under the line, footer count went to 1. Good inline rendering.
- Stack stepping: `>` at the launch target reported "already at the top of the stack"; `<` stepped to `stack 4/5: test: cover priority ordering and retry drops` with a change-scoped 2-file diff — this is excellent for stacked review. `<` again → 3/5 (retry refactor). `>`×2 → **`stack 5/5: (no description)` = the empty working copy: 0 files, +0/-0, and (discovered later) this step erased my comment from disk.**
- Recovering the launch target was a scavenger hunt: `t` loaded `trunk()..@` (8 files, +234 — *not* my launch target `main..@`), the `b` chooser with filter `main` matched three rows and did **not** rank the `main` bookmark first (Enter loaded `umuksskzrkoo..@` → "No changed files"), and I finally recovered by opening `R`, backspacing out the prefilled base, and typing `main`.
- Back at `main..@`: **0/7 viewed, 0 comments**. `C` popup: "no comments recorded yet". On-disk `state.json` confirmed `"comments": []`. The saved comment was gone. Re-added it (priority.rs:5) and re-marked viewed.

Deterministic repro established later:

1. With comments present, press `>` until `stack 5/5: (no description)` (the empty `@`). `state.json` is immediately rewritten with `comments: []` (verified by reading the file at each step).
2. Press `<` — footer shows 0 comments; the comments are permanently gone.

The same empty-target save also erased the ACP draft comment I had accepted, while `agent.json` still records `state: accepted, accepted_comment_id: 6dae6d2d…` pointing at a comment that no longer exists.

## Zen uncurated — stop-by-stop log

`T` from the full target. Intro card: `chapter 1/1 · main..@`, title "(no description)" (the empty tip's message — poor default), stats `7 file(s) · +134 −12`, derived facts (roles 6 source · 1 tests; churn; top changed symbols; "public API: 6 file(s) change pub signatures"), and an honest "uncurated tour — derived from the diff; press @ to summon ACP/agent curation for intent/risk".

| Stop | Content | Did it add information beyond browsing? |
|---|---|---|
| 1/5 | `src/worker.rs:1-41`, largest hunk, "why this matters: Showing the largest of 2 hunk(s); file total +29 −6" | Ordering by churn was mildly useful (biggest change first). The "why" contains no why. |
| 2/5 | `src/queue.rs:24-58`, same template | No. Same info as opening the file, minus the second hunk. |
| 3/5 | `src/priority.rs:1-25` | No — whole file, same as browsing. |
| 4/5 | `src/retry.rs:1-22` | No — and this stop contains the planted off-by-one; uncurated zen gave zero risk signal about it. |
| 5/5 | `src/config.rs:6-22` | No. |
| glance | `src/lib.rs +3 -0 — exports only`, `tests/basic.rs +19 -1 — tests` | Yes, mildly: the derived "exports only"/"tests" labels are factual and correctly triaged these as skimmable. |

Uncurated zen never explained intent, risk, dependencies, or review questions; the only "risk label"-style content was the intro card's role/churn/pub-API facts (accurate) and the glance labels (accurate, useful). It also toured file-by-file over the flattened range even though `review/stack_changes` shows it knows the 4-change stack — no per-change chapters, no commit messages used. Touring auto-marked stops viewed (5/7 after the tour), which I didn't expect and only noticed in the footer.

Card header legibility: header line and card title both repeat `path · role · path:range` — redundant duplication that will fight for space with long paths (see curated section for actual garbling).

## Curation (TUI live)

Briefs via ACP (`gander acp` bridged to the live socket; stderr correctly warned "bridging to live TUI session reviewing main..@; requested trunk()..@ ignored"):

- `review/stack_changes` → change ids + full descriptions (good; also correctly notes humans review these like stacked PRs).
- `review/change_diff` for the three meaningful changes → raw per-change diffs used to hand-compute part line numbers.
- `review/set_change_briefs` with 3 briefs → `{"briefs": 3}`.

Chunks, path 1 — **CLI full set** (`gander chunks set --file /tmp/gander-eval-r3/chunks-set.json`): 4 chunks — `queue-banding` (spotlight, 2 parts: priority.rs:1-25 + queue.rs:24-58, change-anchored), `retry-policy` (spotlight, 2 parts: retry.rs:14-22 + worker.rs:20-44, with an `output` artifact tracing the retry-id bug), `test-coverage` (glance), `exports` (glance). Response: `Set 4 chunks`. The live TUI footer flashed "agent suggestions updated" — the CLI→overlay→TUI pipeline worked without any polling fiddling.

Chunks, path 2 — **incremental CLI update** via stdin heredoc (`gander chunks update`): upserted `test-coverage` to add a `src/config.rs:9-18` part → `Updated 1 chunks, added 0; total 4`. Exactly the incremental semantics the docs promise.

Invalid submissions (deliberate):

- CLI (`chunks update` with a bad path + out-of-range lines): exit 1 with a **precise, complete message** — `chunk 'Invalid on purpose' part 1 (src/does_not_exist.rs): file not present in change yqkuxswyuvko's diff…; part 2 (src/retry.rs): line range outside diff line space` — but wrapped in a **color-eyre crash report** with `Location: src/main.rs:954` and a 12-frame backtrace of `__mh_execute_header <unknown>`, plus hints about `RUST_BACKTRACE=full`. A validation failure looks like a panic.
- ACP (`review/update_chunks`, lines 400-410): clean JSON-RPC error `-32000` with the same per-part reason. Neither path tells you what the *valid* line ranges are, so fixing requires re-deriving them from `review/change_diff` raw output by hand.
- Overlay untouched after both (all-or-nothing confirmed via `chunks list`). The TUI `S` popup consequently never shows invalid chunks — rejection happens at write time, and the human gets **no signal at all** that an agent attempted and failed a curation write.

`S` popup: lists every chunk **part** as its own row (`(1/2)`, `(2/2)`), each repeating the full rationale verbatim — 4 chunks became 7 wordy rows. Live, jumpable, correctly labeled `[spotlight]`/`[glance]` with change ids.

Draft via ACP: `review/draft_comment` on retry.rs:20 → id returned; `D` popup showed `[pending ]` with accept/edit/discard keys; `a` accepted → "accepted agent draft as comment", footer 2 comments. Smooth two-way flow (until the empty-target bug later destroyed the accepted comment).

## Zen curated

`T` again: `zen · chapter 1/2 · then 3 at a glance`, 2 chapters, 4 focus stops.

- Chapter card 1: `change zpmnvzmnvzmx`, commit message, **my brief rendered under "what this change does"** — the narrative works. But the card also printed `3 file(s) · +52 −5` and then `roles: 6 source · 1 tests / churn: +134 −12 · tests touched / top changed symbols: impl Config, fn for_tests, mod config…` — those derived facts are **session-global**, contradict the per-change numbers two lines above, and `impl Config`/`fn for_tests` belong to a *different* change. Chapter card 2 repeated the identical global block.
- Stops 1-2: `queue-banding` parts 1/2 and 2/2, change-scoped diffs, explanation under "why this matters" on **both** parts (verbatim duplicate; the dequeue explanation is odd on the priority.rs part). No cross-reference from part 1 to part 2's location beyond the "(part 1/2)" counter.
- Stop 3: `retry-policy` part 1 — the off-by-one hunk with my explanation and the header suffix `· e 1`; `e` opened the artifact ("Retry accounting trace (max_retries=1)", scrollable, `esc` closes). This stop is exactly what a guided review should feel like.
- Header legibility: in-card title truncates to `RetryPolicy extraction and … (part 1/2) · [yqkuxswyuvko] src/retry.rs:14-22 · e 1` and `[zpmnvzmnvz…]` — the ellipsized change id costs width the title needs; the untruncated line above the card partially compensates.
- Glance board: multi-part glance chunk rendered as one row with **garbled truncation**: `New integration tests + test config [umuksskz…sts/basic.rs:12-29 · [umuksskz…src/config.rs:9-18 — …` — the ellipsized change id collides with the path with no separator. Also "then **3** at a glance" vs board header "**2** remaining item(s)" (parts vs items) is inconsistent.
- My `umuksskzrkoo` brief — which carried the FIFO-coverage review question — **never rendered anywhere**: a change with only glance chunks gets no chapter card, so its brief is silently dropped.

## Curated vs uncurated

Curated zen is a different product tier: per-change chapters with commit message + narrative brief, stops scoped to the change's own diff at exact ranges, teaching explanations, an evidence artifact, and glance items pre-triaged. Uncurated zen is essentially "files by descending churn, largest hunk each, then a labeled skim list" — calmer than browsing and honest about its limits, but it surfaced none of the fixture's actual review questions (it walked right past the planted bug with "Showing the largest of 1 hunk(s)"). The delta is almost entirely the curation content; the zen *frame* (pacing, progress dots, glance finish) carries both.

## Misc observations

- My own tooling accidentally created `err.log` in the fixture; the live pane picked up the **addition** quickly (7→8 files), but after `rm` + `jj status` (jj confirmed 0 file changes in `@`), the pane still showed `err.log +1 -0` with content 20+ seconds and several keypresses later. Stale-deletion handling looks broken (out of scope for scoring, logged as a finding).
- Viewed counts churned across targets: 5/7 viewed before curated zen, 2/8 after — viewed marks recorded against per-change fingerprints during the tour don't map back to the session diff, so "what have I reviewed" is unstable across the very target switches zen itself performs.
- Cleanup: removed the stray `err.log`; overlay (briefs/chunks/drafts) left in place as review state.

# Findings

1. **[blocker] Visiting an empty-diff target permanently erases all comments (including accepted agent drafts).**
   Repro: `gander -b main -r @ tui` on the fixture; add a line comment (`c`, `C-s`); press `>` until `stack 5/5: (no description)` (the empty `@`); observe `state.json` now has `"comments": []`; press `<` — comments are gone from footer, `C` popup, and disk. Also reproduced via the base chooser landing on a 0-file target. The overlay still says the destroyed draft is `accepted` with a dangling `accepted_comment_id`.

2. **[major] Curated zen chapter cards show session-global derived stats that contradict the chapter.**
   Repro: set briefs + change-anchored chunks as in the step log; `T`; chapter 1 card says `3 file(s) · +52 −5` then `roles: 6 source · 1 tests · churn: +134 −12 · top changed symbols: impl Config, fn for_tests…` — whole-session numbers and symbols from a *different* change, repeated identically on every chapter card. Actively misleading on an otherwise-accurate card.

3. **[major] No way back to the launch target; stack navigation dead-ends and the chooser fights you.**
   Repro: launch with `-b main`; press `<`/`>` to explore the stack; `>` ends on the empty `@` (0 files) — the "top of the stack" is a contentless trap that also triggers finding 1; `t` loads `trunk()..@` (different range than `-b main`); `b` + typing `main` fuzzy-matches 3 rows and does not put the exact `main` bookmark match first (Enter loaded `umuksskzrkoo..@` → "No changed files"); recovery required `R` and manually retyping the base. One keystroke ("return to launch target") is missing.

4. **[major] Uncurated zen's "why this matters" contains no why, and the tour ignores stack structure.**
   Repro: `T` with no overlay; every stop's "why this matters" reads "Showing the largest of N hunk(s); file total +X −Y. tab opens the full diff." — a viewport note under a promise-heavy heading. The tour is one chapter titled "(no description)" over the flattened range, despite `review/stack_changes` exposing 4 described changes it could chapter by (as curated zen proves).

5. **[major] Viewed state is unstable across target switches, including the ones zen performs itself.**
   Repro: mark files viewed at `main..@` (5/7); run curated zen (stops retarget to per-change diffs, auto-mark viewed); exit — footer shows 2/8 viewed at the session target. Also: viewed marks made at `main..@` show 0/7 after stepping the stack and returning. Fingerprint-scoped viewed state may be intentional, but as experienced it silently discards progress tracking.

6. **[major] CLI chunk-validation failure renders as a crash report.**
   Repro: `gander chunks update` with a bad path or out-of-range lines → correct, precise per-part message wrapped in color-eyre `Error:` formatting with `Location: src/main.rs:954`, a 12-frame `__mh_execute_header <unknown>` backtrace, and `RUST_BACKTRACE` hints. Looks like gander panicked; will scare users and pollute agent transcripts.

7. **[minor] CLI parity gaps for curation: briefs, draft comments, flags, and ordering are JSON-RPC-only.**
   Repro: `gander help | grep -iE 'brief|draft|flag|order'` → nothing. The vision doc says CLI parity is mandatory; `chunks` proves the pattern works, but half the curation surface still requires hand-rolled JSON-RPC over `gander acp` (I had to script python for briefs and drafts).

8. **[minor] A brief for a change with only glance chunks is silently never shown.**
   Repro: set a brief for `umuksskzrkoo` and only glance chunks for that change; run zen — no chapter card is created, the brief renders nowhere, no warning to the curator.

9. **[minor] Glance-board and card-header truncation garbles multi-part rows.**
   Repro: multi-part glance chunk at 200 cols → `[umuksskz…sts/basic.rs:12-29 · [umuksskz…src/config.rs:9-18` (ellipsized change id fused with path). Also headers spend width on `[zpmnvzmnvz…]` while truncating the human title, and "then 3 at a glance" vs "2 remaining item(s)" counts parts vs items inconsistently.

10. **[minor] Stale deletion in the live pane.**
    Repro: create a file in the workspace (pane picks it up, 7→8 files); `rm` it and run `jj status` (jj reports 0 changes); pane still lists the file with its content 20+ seconds and several keypresses later.

11. **[papercut] Comment editor shows no anchor.** The popup is titled "comment" with no `file:line`, so you can't confirm what you're commenting on after the popup covers the diff.

12. **[papercut] `chunks list` output can't round-trip into `chunks set`.** `list` emits a bare JSON array; `set`/`update` require `{"chunks": [...]}` — the obvious list→edit→set workflow fails on shape.

13. **[papercut] `S` popup repeats the full rationale on every part row**, turning 4 chunks into 7 paragraph-length rows; parts of one chunk should group under one header.

14. **[papercut] Failed agent curation writes are invisible in the TUI.** All-or-nothing rejection is right, but the human gets no notice an agent tried and failed, and errors don't state the valid line ranges for the offending file (curator must re-derive them from `review/change_diff` raw diffs).

# Scores

- **TUI review ergonomics: 2/5.** The happy path is genuinely fast — follow-the-cursor diffs, one-key viewed-and-advance, inline comments, per-change stack stepping — and on that alone it'd be a 4. But a first-class navigation key silently destroys saved comments (finding 1), the launch target is unrecoverable without retyping revsets (finding 3), and viewed progress churns across target switches (finding 5). Rubric anchor 1 is "basic review actions are unreliable"; comments — the core artifact — are unreliable, so this cannot score 3.
- **Zen uncurated: 3/5.** Matches the "3" anchor almost exactly: calmer than browsing, sensible churn-first ordering, honest about being derived, accurate glance labels ("exports only", "tests") and intro-card facts (pub-API surface, tests touched). But zero per-stop insight into risk/intent (finding 4), no use of stack structure, and it walked past the planted bug without a whisper.
- **Zen curated: 3.5/5.** The structure is the promised guided briefing: per-change chapters with briefs, change-scoped anchored stops, teaching explanations, working artifacts, pre-triaged glance items. Held back from 4+ by misleading global stats on chapter cards (finding 2), a silently dropped brief (finding 8), duplicated explanations across parts, and garbled truncation (finding 9) — "rough rendering… limits confidence" is the rubric-3 phrase, and the accuracy bug on chapter cards is worse than rough.
- **Curation protocol ergonomics: 3.5/5.** Above the "3" anchor: the chunks CLI (set/update/remove/clear, stdin or file) removes the raw-JSON-RPC pain for the biggest authoring surface, updates are genuinely incremental, validation is all-or-nothing with precise per-part reasons, and the live bridge (`gander acp` → TUI socket, with a clear target-override warning) worked first try. Kept from 4+ by: briefs/drafts/flags/ordering still requiring manual JSON-RPC (finding 7), line-space authoring still meaning "hand-count lines in a raw diff" with errors that don't tell you the valid ranges (finding 14), the crash-report error rendering (finding 6), and the list/set shape mismatch (finding 12).

# Top proposals

1. **Never persist comment loss from target switches.** Treat comments as target-independent review state: keep every comment keyed by (path, anchor, fingerprint) regardless of the currently loaded range, render only the ones visible in the current diff, and never rewrite `state.json` comment content from a narrower/empty target. This single fix moves TUI ergonomics from 2 to 4.
2. **Add a "return to launch target" key and fix `>` at the stack top.** Remember the CLI-given base/rev as the home target (`t` long-press or `0`/`~`); make `>` from the last non-empty change either stop or announce "top (working copy is empty)"; rank exact bookmark-name matches first in the `b` chooser.
3. **Scope chapter-card derived facts to the chapter.** Compute roles/churn/symbols/pub-API from that change's diff (the data already exists via `review/change_diff`); and render a brief-only "chapter" (or an interstitial card) for changes that have briefs but no spotlight stops so curation is never silently dropped.
4. **Make uncurated zen stack-aware and honest.** Chapter by `review/stack_changes` with commit messages as narrative (already proven by the curated path); retitle "why this matters" to "viewport" unless there is actual why-content; add cheap derived risk notes per stop (pub-signature changes, new files, test-coverage touch) that the engine already computes for the intro card.
5. **Finish CLI parity for curation and polish errors.** `gander briefs set/list`, `gander drafts add/list`, `gander flags`, `gander order` mirroring `chunks`; print validation failures as plain one-line-per-part errors (no color-eyre report) including the valid diff line ranges for the file; make `chunks list` emit `{"chunks": [...]}` so list→edit→set round-trips; surface a footer notice in the TUI when an agent write is rejected.
6. **Group multi-part chunks in lists and fix truncation.** In `S` and the glance board, render one header per chunk with indented part rows (rationale once); truncate change ids to a fixed short width with a guaranteed separator before paths; reconcile "N at a glance" counts between the tour header and the board.
