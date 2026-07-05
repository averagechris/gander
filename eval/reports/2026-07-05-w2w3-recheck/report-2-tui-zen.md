# Summary

Re-evaluation verdict: the core TUI remains a solid everyday review surface, and zen has materially improved in both uncurated and curated modes. Uncurated zen is no longer just a file slideshow: the chapter intro now labels itself as derived from the diff, reports role/churn/symbol facts, chooses five spotlight stops plus a two-item glance list, and gives each stop a small rationale. That helped me reach the retry bug faster than plain browsing because `src/retry.rs` and `src/worker.rs` were both spotlighted and the off-by-one was visible in the stop.

The remaining gap is that uncurated rationale is still generic. It says “Showing the largest hunk” rather than forming a review question like “does this retry budget terminate?” Curated zen is much stronger and feels like an agent-guided briefing. The ACP validation improvement is significant: a deliberately invalid chunk part was rejected with a precise JSON-RPC error instead of producing a misleading card. The live bridge now also warns when requested ACP flags differ from the TUI target.

# Step log

## Setup and help

- Launched from fixture with:

```sh
nix shell nixpkgs#tmux --command tmux new-session -d -s gander-tui-eval-recheck -x 200 -y 50 -- '/Users/chris/projects/gander/target/release/gander -b main -r @ tui'
```

- Pressed `?`. Help was complete and discoverable for a first-time reviewer, but still keymap-oriented rather than task-oriented. It showed normal review actions, comments, stack stepping, and zen/agent keys in one dense overlay.

Captured excerpt:

```text
general                  comments
 tab switch focus ...       c comment at cursor
 ...
targets & jj
 t compare trunk()..@       >/< step through stack
agent
 S agent review chunks      T zen briefing (focus stops + glance)
```

## Normal review interactions

- Navigated with `n` through changed files to `src/retry.rs` and saw the intentional off-by-one at `attempts <= self.max_retries`.
- Pressed `enter` on `src/retry.rs`; the file became viewed and focus advanced to `src/worker.rs`.
- Added and saved a line-anchored comment via `c`, typed body, `ctrl-s`:

```text
Re-enqueueing creates a fresh job id, so attempts keyed by the old id can reset forever.
```

- The comment count updated to `1 comments`. The comment appeared anchored near the currently visible worker hunk; the flow was reliable, though exact cursor/line selection still depends on knowing where the diff cursor is.

## Uncurated zen stop log

Entered zen with `T` before ACP curation. Opening card now had real derived facts:

```text
chapter 1/1 · main..@
7 file(s) · +134 −12
n tours the 5 stop(s) in this chapter
uncurated tour — derived from the diff; press @ to summon ACP/agent curation for intent/risk
roles: 6 source · 1 tests
churn: +134 −12 · tests touched
top changed symbols: impl Config, fn for_tests, mod config, mod priority, mod queue
what this change does
No agent brief for this change — the facts above are derived from the diff.
```

Stop-by-stop:

1. `src/worker.rs`: showed the largest worker hunk with retry loop, `attempts` map, and `queue.enqueue_with_priority(...)`. This added value beyond ordinary browsing because it picked the riskiest hunk early and centered the cross-line retry identity issue. Rationale was still generic: “Showing the largest of 1/2 hunk(s).”
2. `src/queue.rs`: showed the priority queue hunk and called out that it was the largest hunk. Useful for understanding `enqueue_with_priority` and fresh id allocation, but did not connect that fact back to worker retry identity.
3. `src/priority.rs`: showed the new enum. Mild value as API/concept orientation; no risk question beyond generic hunk sizing.
4. `src/retry.rs`: showed the full retry policy including comments and `attempts <= self.max_retries`. This absolutely helped find the real off-by-one faster because the relevant file was a spotlight stop and the bad expression was visible without opening full diff. It still did not say “boundary condition appears suspicious.”
5. `src/config.rs`: showed `Config::for_tests`. Helpful as test-support context, but low risk.

Glance board:

```text
5 spotlight stop(s) toured. 2 remaining item(s) below — skim, then `a` marks them all viewed.
› • src/lib.rs src/lib.rs +3 -0 — exports only
  • tests/basic.rs tests/basic.rs +19 -1 — tests
```

Uncurated verdict: improved from baseline. It now provides information I would not get from normal browsing at a glance: chapter-level derived facts, a curated-ish spotlight/glance split, stop anchoring, and basic rationale. It helped locate both retry-relevant files faster. It still does not explain intent/risk/dependencies in a human review-question sense; most “why this matters” text is metadata, not insight.

## ACP commands and behavior

Read state through live ACP:

```text
review/summary -> live_session: true, active_target: main..@, summary: 7 files ...
review/stack_changes -> meaningful changes:
  lvlumrzyzyol feat: priority scheduling for the job queue
  komqxrzpmnzt refactor: extract retry policy from worker
  tvutuynuqprl test: cover priority ordering and retry drops
```

Deliberately tested target divergence by invoking ACP with flags different from the live TUI:

```sh
gander -b 'trunk()' -r '@-' acp
```

This now printed a clear stderr warning:

```text
warning: bridging to live TUI session reviewing main..@; requested trunk()..@- ignored
```

and `review/summary` returned `live_session: true` plus `active_target: main..@`. This is a strong improvement over silent flag ignoring.

Deliberately sent one invalid `review/set_chunks` request: a chunk anchored to retry change `komqxrzpmnzt` with one valid `src/worker.rs` part and one invalid `src/queue.rs` part, which is not in that change diff. Result:

```json
{"error":{"code":-32000,"message":"invalid chunk part(s): chunk 'Retry identity spans worker and queue' part 2 (src/queue.rs): file not present in change komqxrzpmnzt's diff: src/queue.rs"},"id":1,"jsonrpc":"2.0"}
```

This is exactly the right safety behavior: the request was rejected, named the bad chunk and part, named the bad file, and said why.

Then corrected the curation and proceeded:

```text
review/set_change_briefs: 3 briefs for priority, retry, and tests changes
review/set_chunks: 4 chunks
  spotlight: Retry limit is off by one, src/retry.rs:14-20, change komqxrzpmnzt
  spotlight: Re-enqueue resets retry identity, src/worker.rs:23-36, change komqxrzpmnzt
  glance: Priority queue mechanics, src/priority.rs and src/queue.rs, change lvlumrzyzyol
  spotlight: Tests miss retry boundary and identity, tests/basic.rs, change tvutuynuqprl
review/draft_comment: src/worker.rs:34 fresh job id / attempts reset issue
```

ACP authoring friction remains: I still had to collect stack change IDs, fetch change-specific diffs to know the right line space, and hand-author long JSON strings. But validation and target warnings made failures much safer.

## Curated zen comparison

After ACP curation, `T` opened a two-chapter briefing:

```text
chapter 1/2 · change komqxrzpmnzt
refactor: extract retry policy from worker
3 file(s) · +52 −6
n tours the 2 stop(s) in this chapter
what this change does
Extracts retry decisions into RetryPolicy and changes Worker from dropping failures to re-enqueueing them. This is the risky behavior change: retry limits and job identity determine termination.
```

Curated stops:

1. `Retry limit is off by one`: centered `src/retry.rs:14-20` and explained the boundary failure: with `max_retries=3`, attempt count 3 still returns true.
2. `Re-enqueue resets retry identity`: centered `src/worker.rs:23-36` and explicitly explained that always-failing payloads can come back with fresh ids, so the per-id counter may never hit the drop path.
3. Chapter 2 card for tests explained what the tests intend and what to question.
4. `Tests miss retry boundary and identity`: guided me to inspect `tests/basic.rs` as coverage evidence and ask whether retry attempts terminate for the same logical job.

Glance board after curation:

```text
3 spotlight stop(s) toured. 4 remaining item(s) below — skim, then `a` marks them all viewed.
› • Priority queue mechanics [lvlumrzyzyol] src/priority.rs:1-25 — Mechanical implementation of BTreeMap bands; skim to confirm highest priority and FIFO semantics.
  • Priority queue mechanics [lvlumrzyzyol] src/queue.rs:24-49 — Mechanical implementation of BTreeMap bands; skim to confirm highest priority and FIFO semantics.
  • src/lib.rs src/lib.rs — exports only
  • src/config.rs src/config.rs +11 -0 — source
```

Curated verdict: much better than uncurated and slightly better than baseline due to validation and bridge warnings. The narrative correctly grouped the retry change before the test change and made the real bugs obvious. Remaining rough edges: multi-part glance chunks still duplicate the title/rationale per part, long titles/paths still truncate (`tests/basic.r...`), and uncovered fallback glance items remain sparse.

Finally killed the tmux session:

```sh
nix shell nixpkgs#tmux --command tmux kill-session -t gander-tui-eval-recheck
```

# Findings

## Major: uncurated zen still stops short of real review questions

Concrete repro: start the fixture TUI, press `T`, then press `n` through uncurated stops. The chapter card now shows useful derived facts and the tour spotlights `src/worker.rs`, `src/queue.rs`, `src/priority.rs`, `src/retry.rs`, and `src/config.rs`, but each stop's “why this matters” text is mostly “Showing the largest hunk; file total...”. Expected: derived review questions/risk labels, e.g. “retry boundary: should attempts == max_retries retry?” or “re-enqueue allocates a fresh id; verify retry accounting follows logical jobs.”

## Minor: help remains dense and keymap-first

Concrete repro: press `?`. The overlay lists many commands accurately, but does not teach the recommended first review loop. Expected: a task-oriented top section such as “1. choose target, 2. navigate files, 3. tab into diff, 4. comment, 5. enter mark viewed, 6. T zen / @ agent.”

## Minor: ACP curation is safer but still manually brittle

Concrete repro: use `review/stack_changes`, `review/change_diff`, then hand-write `review/set_chunks` JSON with change-specific line numbers. Validation now catches bad parts, but authors still need to juggle change IDs, target-specific line spaces, and escaped multiline explanations. Expected: a higher-level CLI/file helper such as `gander curate apply curation.yaml --validate --preview`.

## Minor: glance board duplicates multi-part conceptual chunks

Concrete repro: set one glance chunk titled “Priority queue mechanics” with parts in `src/priority.rs` and `src/queue.rs`. The glance board shows two separate bullets with duplicated title/rationale. Expected: one conceptual item expandable into two locations, or visible “part 1/2” grouping.

## Papercut: long titles/paths truncate on zen cards

Concrete repro: curated test stop title rendered as `Tests miss retry boundary and identity · [tvutuynuqprl] tests/basic.r...` in the card header. Expected: wrap or secondary location line so the exact file remains visible.

# Scores

- TUI review ergonomics: **4/5**. Before/after vs baseline: **4 → 4**. Normal review remains fast and reliable: file navigation, viewed state, comments, and stack awareness all work. No regression. Deductions remain for dense help and cursor/line anchoring ambiguity while commenting.
- Zen uncurated: **3/5**. Before/after vs baseline: **2 → 3**. Improved materially: derived chapter facts, spotlight/glance split, stop anchoring, and basic rationale make it calmer and somewhat smarter than browsing. It helped find the retry bugs faster. Still not a 4 because it does not generate meaningful risk questions or connect worker retry identity to queue id allocation.
- Zen curated: **4/5**. Before/after vs baseline: **4 → 4**. The curated briefing is genuinely useful and directly points to the off-by-one, retry identity, and missing tests. Improved safety from chunk validation and target warnings, but presentation rough edges keep it below 5: duplicate glance entries, truncation, and sparse fallback glance items.
- Curation protocol ergonomics: **3/5**. Before/after vs baseline: **2 → 3**. Validation now rejects invalid change-anchored parts with a precise error, and live-bridge target divergence is explicitly warned. That removes a major footgun. Still manual JSON-RPC with exact change IDs and line spaces, so it is capable but not ergonomic.

Regressions/new friction: I did not observe functional regressions. The only new-ish friction is that uncurated zen's derived facts can imply more intelligence than the stops deliver; the UI says “why this matters,” but the text is often just hunk metadata.

# Top proposals

1. **Upgrade uncurated stop rationales from metadata to review prompts.** Keep the derived facts, but add heuristic questions for retry/error handling, queue identity, tests touched, public exports, and config helpers.
2. **Add a first-class curation authoring helper.** Accept YAML/TOML/Markdown, validate chunks before applying, preview the resolved line excerpts, and support incremental edits instead of replace-all JSON-RPC.
3. **Group conceptual glance chunks.** Show one “Priority queue mechanics” item with expandable locations instead of duplicated bullets.
4. **Make help task-oriented.** Add a short recommended workflow above the exhaustive keymap.
5. **Improve zen header wrapping.** Preserve exact file and line anchor even for long titles/change IDs.
