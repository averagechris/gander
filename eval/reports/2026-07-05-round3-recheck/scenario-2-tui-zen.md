# Summary

Scenario 2 (TUI + zen) against the taskq fixture stack (`main..@`, 3 described changes, 7 files, +134/−12). Verdict: the core review loop is solid and fast — tree navigation, per-change stack stepping, viewed tracking, line-anchored comments, and the ACP/CLI curation round-trip all worked on the first try, and curated zen genuinely feels like a guided briefing. The biggest product gaps are:

1. **Uncurated zen wastes the jj stack.** It collapses 3 well-described changes into one chapter titled "(no description)" and fills "why this matters" with hunk-count boilerplate. Per-change chapters with commit messages would give most of the curated experience for free.
2. **CLI error output is hostile.** A simple chunk-validation mistake produces a 30-line color-eyre backtrace of `__mh_execute_header` frames around one (excellent) error message. Agents parsing stderr and humans alike get noise.
3. **Small discoverability/rendering debts add up:** unlabeled comment editor with no save-key hint, `tree-sitter: … errors=true` debug text atop every diff, briefs silently dropped for changes without spotlight stops, truncation glitches on glance rows and chapter cards.

Curation itself (briefs + chunks + draft comment) took about 10 minutes end to end including one deliberate validation failure, and the curated tour surfaced both planted bugs plus a real third one (retry re-keying) with precise anchors.

# Step log

Session: `gander-r3-s2` (200x50), fixture `/tmp/gander-eval/round3b-s2`, binary `target/release/gander` launched as `gander -b main -r @ tui`.

## TUI review pass

- `tmux new-session -d -s gander-r3-s2 -x 200 -y 50 -c <fixture> -- 'gander -b main -r @ tui'` → files tree + diff pane, footer `7 files (0/7 viewed …) +134/-12 … following @`. Immediately visible oddity: `tree-sitter: rust root=source_file errors=true` rendered as the second line of the diff pane.
- `?` → help overlay. Genuinely task-oriented: opens with a "first review loop" section (`/` open file, `]` next unviewed, `enter` mark viewed and advance, `c` comment, `T` zen, `ctrl-y` handoff). Dense but scannable; two-column layout with zen/comments/targets/agent groups. Caveats: movement is `n/e` (Colemak-style), `e`/`T`/`p` are overloaded per-context, and `j` in the files pane silently does nothing.
- Navigation: `n` steps the tree and the diff follows correctly.
- Stack stepping: `>` → `info: already at the top of the stack`; `<` three times walked `stack 4/5: test: …` → `3/5: refactor: …` → `2/5: feat: …` → `1/5: chore: initial task queue library`, each retargeting file list, diff, and footer churn to that single change. Note stack position 1/5 is the *base* commit (`main`) itself. `t` returned to `main..@` (`info: loaded main..@`).
- Viewed: `v` marked `config.rs` viewed (✓, re-sorted to bottom of its directory; counter `1/7`).
- Comment: `/` fuzzy search "retry" → Enter → Tab into diff → `G`, `e e` to land on `attempts <= self.max_retries` (line 20) → `c` opened an editor box titled just "comment" — no file:line anchor shown, no save-key hint anywhere (footer still showed stale `info: loaded main..@`). Guessed `C-s`; it saved and rendered inline: `↳ 8209511c [draft] Off-by-one: attempts <= max_retries allows max_retries+1 retries…`. Footer: `1 comments`.

## Zen uncurated — stop-by-stop log

`T` → `zen · chapter 1/1 · then 2 at a glance`, chapter card `main..@` / "(no description)" with derived facts (roles 6 source · 1 tests; churn; top changed symbols; "6 file(s) change pub signatures") and an honest "uncurated tour — derived from the diff; press @ to summon…" note.

| Stop | Target | Added information beyond browsing? |
| --- | --- | --- |
| 1/5 | `src/worker.rs:1-41` (largest hunk) | Marginal. Good excerpt choice, but "why this matters" reads "Showing the largest of 2 hunk(s); file total +29 −6…" — pure mechanics, zero why. |
| 2/5 | `src/queue.rs:24-58` | Same boilerplate ("largest of 2 hunk(s); +25 −5"). No mention this is the core data-structure change. |
| 3/5 | `src/priority.rs:1-25` | Same boilerplate. |
| 4/5 | `src/retry.rs:1-22` | Same boilerplate. The intentional off-by-one is on screen but nothing draws the eye to it. |
| 5/5 | `src/config.rs:6-22` | Same boilerplate. |
| glance | `src/lib.rs — exports only`, `tests/basic.rs — tests` | Useful triage; rows render path twice ("src/lib.rs src/lib.rs"). |

Intent: **no** (single chapter "(no description)" despite 3 described jj changes; `review/stack_changes` proves the data is available). Risk: **no**. Dependencies: **no** (stop order is churn-descending, not reading order). Review questions: **no**. What it does provide: calm pacing, sensible largest-hunk excerpts, changed-symbol summaries, auto viewed-marking (`n next (marks viewed)`), and a good glance/`a` finisher (`zen complete — 2 glance item(s) marked viewed`, 7/7 viewed).

## Curation (ACP + CLI), TUI live

- `gander -b main -r @ acp` bridged to the live TUI. `initialize` → protocol `gander-acp` v1, 15 capabilities. `review/stack_changes` → change ids + full multiline descriptions (includes the base `main` change, `current:false` — agents must know to skip it).
- `review/change_diff` for `kytvmurzxzvu` / `zlsqsslypyxn` / `sxrsqwtyuzsy` → raw git-style diffs; I hand-derived new-file line numbers from `@@` headers for chunk parts. This is the most error-prone step; nothing previews what a part will render.
- `review/set_change_briefs` with 3 briefs (feat/refactor/test) → `{"briefs":3}`.
- Deliberate bad spec via `gander chunks set --file …bad.json` (out-of-range lines; file not in that change): rejected all-or-nothing with *excellent* per-part reasons — "chunk 'Bad anchor test' part 1 (src/retry.rs): line range outside diff line space…; chunk 'Wrong file test' part 1 (src/queue.rs): file not present in change zlsqsslypyxn's diff" — but wrapped in a full color-eyre backtrace (12 `__mh_execute_header` frames, "Location: src/main.rs:940").
- Real set via CLI: 4 chunks (3 spotlight anchored to feat/refactor with explanations + artifacts, 1 glance on tests) → `Set 4 chunks`.
- Incremental edit via ACP `review/update_chunks` (retitled/reworded the glance chunk by id) → `{"added":0,"chunks":4,"updated":1}`. `chunks remove --id nonexistent-id` → clean "unknown chunk id(s)" message (again + backtrace).
- `review/draft_comment` on `src/worker.rs:34` → id returned; TUI footer flipped to `info: agent suggestions updated` within a second.
- CLI vs raw JSON-RPC: the CLI path is discoverable (`chunks` is listed in `gander --help`, `--help` embeds a spec example) and materially easier — file-based specs, no JSON-RPC framing, and `update`/`remove`/`clear` cover the incremental cases. Briefs and draft comments have **no** CLI equivalent; those still require raw JSON-RPC (violates the CLI-parity principle).

## Zen curated vs uncurated

`T` again → `zen · chapter 1/2 · then 4 at a glance`, **2 chapters keyed to jj changes** with commit message + my brief under "what this change does".

- Chapter 1 (`kytvmurzxzvu`): stop 1/3 `src/queue.rs:30-49` — retargeted to the change's own diff, correct anchor, my explanation under "why this matters", `e` opened the "Dequeue order example" artifact in a nested card.
- Chapter 2 (`zlsqsslypyxn`): brief rendered but clipped mid-sentence at the card edge ("check for runaway" — no scroll/expand cue). Stop 2/3 `src/retry.rs:14-21` (off-by-one, fix + boundary-test ask); stop 3/3 `src/worker.rs:22-41` (re-keying/infinite-retry risk).
- Glance board: my test-coverage chunk with full rationale, but the row truncates change-ids into the path text (`[sxrsqwty…sts/basic.rs:12-29`); header said "3 remaining item(s)", footer "4 item(s)". The brief I wrote for the test change (`sxrsqwtyuzsy`) never appeared anywhere — no spotlight chunk ⇒ no chapter ⇒ brief silently dropped.
- `D` drafts panel: clear `[pending]` list with explicit keys (`enter/a accept · e edit then accept · x discard`); accepted → `info: accepted agent draft as comment`, footer 2 comments; `review/overlay` showed `state: "accepted"` + `accepted_comment_id` for agents.

Comparison: uncurated zen = progress meter with excerpts; curated zen = an actual review briefing — intent per chapter, risk callouts at exact anchors, a teaching artifact, and triage-able draft comments. The delta is large and entirely dependent on curation quality.

`q` (writes artifact) then `tmux kill-session -t gander-r3-s2`; verified no session remains.

# Findings

1. **major — CLI errors buried in color-eyre backtraces.** Repro: `gander -b main -r @ chunks set --file bad.json` where a part has `start_line: 900` for `src/retry.rs` anchored to `zlsqsslypyxn`. The one-line validation message (which is excellent) is followed by Location `src/main.rs:940` and a 12-frame `__mh_execute_header` backtrace. User input errors should print the message and exit; agents parsing stderr get ~30 lines of noise.
2. **major — uncurated zen ignores the jj stack.** Repro: fixture with 3 described changes, no overlay; press `T`. One chapter titled `main..@` / "(no description)" (the empty `@`'s description), 5 churn-ordered stops. `review/stack_changes` already exposes per-change descriptions; per-change chapters with commit messages would deliver intent for free. As shipped, the tour explains no intent, risk, dependency, or review question.
3. **major — uncurated "why this matters" contains no why.** Repro: any uncurated zen stop. The section labeled "why this matters" says "Showing the largest of 2 hunk(s); file total +29 −6. tab opens the full diff." — presentation mechanics under an intent-promising header. Either derive something (symbol roles, pub-API delta, callers) or rename the section.
4. **minor — comment editor is anchorless and save is undiscoverable.** Repro: cursor on a diff line, press `c`. Box titled "comment", no file:line shown, no key hints in box or footer (footer keeps the stale previous info line). Saved only by guessing `C-s`.
5. **minor — tree-sitter debug line leaks into every diff header.** Repro: open any file; line 2 of the diff pane is `tree-sitter: rust root=source_file errors=true`. Debug/status output in the primary reading surface; `errors=true` on plain Rust (config.rs) also suggests the grammar/query fails to parse valid code.
6. **minor — movement keys are inconsistent across panes.** Repro: in the files pane press `j` (nothing) vs `n` (moves); zen and the drafts panel advertise `j/k`. First-timers try arrows/hjkl before reading help; either support both everywhere or be consistent.
7. **minor — change briefs without spotlight chunks are silently dropped.** Repro: `review/set_change_briefs` for `sxrsqwtyuzsy` (test change), chunks for that change only `glance`. Curated zen shows 2 chapters; the third brief renders nowhere and no warning is emitted at authoring time.
8. **minor — no CLI parity for briefs and draft comments.** Repro: `gander --help` — `chunks` exists but there is no `gander briefs`/`gander drafts` equivalent; those require hand-rolled JSON-RPC against `gander acp`, contradicting the project's CLI-parity principle.
9. **minor — chunk line-space authoring is manual and unaided.** Repro: to anchor `src/queue.rs:30-49` to `kytvmurzxzvu` I had to read `review/change_diff.raw` and count post-image lines from `@@` headers. Validation catches mistakes after the fact, but nothing helps produce correct ranges (no per-change `hunks`-style listing, no preview of what a part will render).
10. **papercut — glance row truncation mangles ids into paths.** Repro: curated glance board at 200 cols with a 2-part chunk: `[sxrsqwty…sts/basic.rs:12-29 · [sxrsqwty…src/config.rs:9-18`.
11. **papercut — glance count mismatch.** Repro: curated glance board header "3 remaining item(s)" vs footer "zen glance · 4 item(s)".
12. **papercut — chapter card clips brief text.** Repro: chapter 2/2 card; brief ends mid-sentence at the card border with no truncation indicator or expand cue.
13. **papercut — stack stepping walks into the base commit.** Repro: from `main..@` press `<` four times: position 1/5 is `chore: initial task queue library` (= `main`), outside the review range you asked for.
14. **papercut — pluralization/duplication nits.** "1 comments" in the footer; uncurated glance rows print the path twice ("src/lib.rs src/lib.rs").

# Scores

- **TUI review ergonomics: 3.5/5.** Everything a review needs worked first-try: tree+diff follow, fuzzy search, per-change stack stepping with correct retargeting (standout feature), viewed tracking with directory rollups, inline comment rendering, live agent-suggestion pickup. Held back by discoverability debt: anchorless comment editor with guess-the-save-key (finding 4), inconsistent movement keys (6), debug noise in the diff header (5), and disorientation papercuts (13, 14).
- **Zen uncurated: 3/5.** Matches the rubric's 3 anchor almost exactly: calmer than browsing, sensible largest-hunk excerpts, derived symbol/churn/pub-API facts, honest about being uncurated, and auto-viewed bookkeeping — but no intent, risk, dependencies, or review questions (findings 2, 3), despite the stack descriptions being available to it.
- **Zen curated: 4/5.** Chapters keyed to jj changes with briefs + commit messages, precise change-diff anchors, teaching explanations under "why this matters", openable artifacts, rationale-bearing glance items, and a clean draft-triage loop with disposition write-back. Both planted bugs plus a real third one were surfaced exactly where a reviewer looks. Short of 5 due to rendering/consistency gaps (10–12) and silently dropped briefs (7).
- **Curation protocol ergonomics: 3.5/5.** Docs are accurate; validation is all-or-nothing with precise per-part reasons; `update_chunks`/`remove` make edits genuinely incremental; the `chunks` CLI is discoverable and materially easier than raw JSON-RPC; live TUI pickup is ~instant; the overlay round-trips dispositions. Held back by backtrace-wrapped errors (1), manual line-space derivation with no preview (9), and missing CLI parity for briefs/drafts (8).

# Top proposals

1. **Derive per-change chapters in uncurated zen.** Use `review/stack_changes` to chapter the tour by jj change with commit messages as the narrative, and order stops in dependency/reading order within each chapter. This closes most of the uncurated↔curated gap for zero curation cost and is the single highest-leverage change.
2. **Print clean errors for user-input failures.** Validation and unknown-id errors from `chunks`/ACP CLI paths should emit the (already great) message without a color-eyre backtrace; reserve backtraces for panics. One-line fix in perceived quality for both humans and agents.
3. **Make the comment editor self-describing.** Title it with the anchor (`comment · src/retry.rs:20`) and show `ctrl-s save · esc cancel` in the box or footer.
4. **Add authoring aids for chunk anchors.** A `gander hunks --change <id>` (or `chunks lint --file spec.json --explain`) that lists valid per-change line ranges, and a dry-run preview of what each part will render, would remove the count-lines-from-raw-diff step (and make wrong-line-space mistakes structurally unlikely).
5. **Close the CLI-parity gap for curation.** `gander briefs set` and `gander drafts add/list` mirroring the ACP methods; warn (or render a chapterless brief card) when a brief targets a change with no spotlight stop, instead of dropping it silently.
6. **Sweep the rendering nits:** hide the tree-sitter status line behind the `V` view options, fix glance truncation/count mismatch and chapter-card clipping, dedupe glance paths, fix pluralization, and accept `j/k` wherever `n/e` works.
