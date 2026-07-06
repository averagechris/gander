# Summary

Round-5 recheck of the TUI + zen loop on the `round5-s2` task-queue fixture (3-change stack: feat → refactor → test, empty `@` on top). The headline regressions from prior rounds are fixed and verifiable: chapters and stack stepping scope strictly to `base..rev` (the `main` bookmark change never appears; denominators are stable), uncurated derived facts come from each change's own diff (per-chapter churn matched `jj diff --stat` per change; "tests touched" flips correctly on the test chapter), the test-titled change spotlights `tests/basic.rs`, `chunks lines` output was accepted verbatim by the validator, brief-without-spotlight warnings fire, and the op picker and comment editor both show the new hints.

The most important remaining gaps:

1. **Glance "mark all viewed" doesn't stick.** After both zen runs reported "2 glance item(s) marked viewed", `src/lib.rs` was still unviewed (6/7) in the file tree. Zen says the review is complete while the session's own viewed tracking disagrees.
2. **Curated briefs get silently truncated on the chapter card.** A ~5-sentence brief was cut mid-sentence ("empty-payload jobs may") with no ellipsis, scroll, or expand affordance — `d` only expands the *commit description*. Curated content is lost exactly where it matters most.
3. **The purely-added-API fix is hunk-scoped, not symbol-scoped.** Brand-new `pub fn enqueue_with_priority` and `pub fn dropped()` (verified absent in the parent revisions) are labeled "signature changed" with the bogus review question "do callers handle the new signature?"; only symbols inside pure-addition hunks (`priority.rs`, `Config::for_tests`) get the correct "new public API" label.

Overall: the normal review loop is genuinely fast, curation authoring via the CLI is now the clearly better path (discoverable, validated, incremental), and curated zen is a real upgrade over uncurated — held back mainly by chapter-card rendering (truncation, derived-fact noise, narrow fixed-width card on a 200-col terminal).

# Step log

All TUI interaction through `nix shell nixpkgs#tmux --command tmux ...`, session `gander-r5-s2`, 200x50, running `/Users/chris/projects/gander/target/release/gander -b main -r @ tui` in `/tmp/gander-eval/round5-s2`. Session killed at the end.

## Normal review

- `?` help: organized into "first review loop" / general / files / diff / zen / comments / targets & jj / agent / badges. The "first review loop" block (`/` jump, `]` next unviewed, `enter` mark viewed and advance, `c` comment, `T` zen, `ctrl-y` handoff) is genuinely task-oriented for a first-timer. Dense but scannable.
- Stack stepping with `>`/`<`: launch target reports "already at the top of the stack"; `<` walks `3/4: test:` → `2/4: refactor:` → `1/4: feat:` → "already at the bottom of the stack". The base `main` change is correctly *not* a position and the denominator stayed 4 throughout. But position **4/4 is the empty working-copy change** — "(no description)", "No changed files" — a dead stop (see F4). `t` returned to the launch target ("info: loaded main..@").
- Per-change file/stat counts while stepping matched `jj diff --stat` for each change (e.g. refactor: 3 files +52/−6).
- `enter` marked `src/config.rs` viewed (✓) and advanced to `lib.rs`; counter 1/7.
- Comment: navigated to `src/retry.rs` in the diff pane, cursor to line 20, `c` opened "comment · src/retry.rs:20" with the new `ctrl-s save · esc cancel` hints; typed the off-by-one comment, `ctrl-s` saved ("1 comment" in status). `gander comments list` later showed a rich anchor (hunk header, line text `attempts <= self.max_retries`, fingerprints) at exactly retry.rs:20.

## Zen uncurated — stop-by-stop log

Entered with `T`: "3 chapter(s), 6 focus stop(s), 2 at a glance". Chapters = exactly the 3 real changes; base and empty `@` excluded.

| Stop | Anchor | Added info beyond browsing? |
|---|---|---|
| ch.1 card | feat sxqzuzqvywym | Commit body + own-diff facts: churn +52 −5 · **no tests touched** (correct for this change), top symbols, "3 file(s) change pub signatures", explicit "uncurated tour — press @ to curate" honesty. Yes, modest. |
| 1/6 | queue.rs:24-58 [feat] | Change-scoped diff (shows old `self.jobs` removals). "public API change: pub fn enqueue_with_priority( **signature changed** · do callers handle the new signature?" — wrong; the fn is brand new (F3). Symbols list useful. |
| 2/6 | priority.rs:1-25 [feat] | "**new public API: pub enum Priority** · is this the right surface to expose?" — correct label for pure-addition file; useful question. But symbols list has "impl Priority" twice (one is `impl Default for Priority`). |
| ch.2 card | refactor kynzuzwpppol | churn +52 −6 correct; "builds on ch.1 …: mod config, mod priority, mod queue, mod worker" dependency line is a nice touch. But "top changed symbols: mod config, mod priority, mod queue…" — the refactor never touched those; they're context lines around lib.rs's added `pub mod retry` (F5). |
| 3/6 | worker.rs:1-41 [refactor] | "public API change: pub fn dropped(&self) -> u64 **signature changed**" — `dropped()` is new (verified via `jj file show -r sxqzuzqv`); wrong label + misleading question (F3). |
| 4/6 | retry.rs:1-22 [refactor] | "**new public API: pub struct RetryPolicy**" — correct. The stop happens to frame the intentional off-by-one hunk. Good. |
| ch.3 card | test wylpsoususoo | "roles: 1 source · 1 tests", "churn +30 −1 · **tests touched**" — own-diff facts correct. |
| 5/6 | config.rs:6-22 [test] | "**new public API: pub fn for_tests() -> Self**" — correct for a pure-addition hunk in a modified file. |
| 6/6 | tests/basic.rs:9-29 [test] | Test-titled change spotlights its test file, as claimed. Symbols = the two new test fns. |
| glance | `src/lib.rs — source` ×2 | Two *identical* rows with no change attribution (one per owning change) (F6). `a` → "zen complete — 2 glance item(s) marked viewed", **but lib.rs stayed unviewed (6/7)** (F1). |

Verdict uncurated: calmer than browsing, correct per-change facts and pacing, real (if shallow) review questions; no intent/risk beyond commit messages — which the card honestly admits.

## Curation (ACP + CLI)

ACP via `printf ... | gander -b main -r @ acp` from the fixture (bridged to the live TUI — writes appeared in the UI within a tick: "info: agent suggestions updated"; nothing in the output *says* it bridged, F10):

- `initialize` → 15 capabilities. `review/stack_changes` → base `main` excluded; the empty `@` change `knzlymnkwwpk` **is** included with empty description (agents must know to skip it, F4).
- `review/set_change_briefs` with 3 briefs (one with a `note` artifact) → `{"briefs":3, warnings:[…]}` — the new "no spotlight chunk yet" advisory fired for all three, correctly non-blocking.
- `review/draft_comment` on worker.rs:36 (retry id-churn bug) → id returned; TUI `D` panel showed it pending with `enter/a accept · e edit · x discard` hints; accepted → "accepted agent draft as comment", comment count 2.

CLI chunks path:

- `gander chunks --help` includes an inline spec example — discoverable without docs.
- `gander chunks lines --change kynzuzwpppol` (and per-change/per-path variants) gave exact accepted line spaces. One wart: for feat's queue.rs the change diff has two hunks (`@@ -1,14 +1,20 @@`, `@@ -18,21 +24,35 @@`) but `lines` returned **one merged entry** `start_line:1, end_line:58` with only the first header (F7).
- Deliberate bad spec (`start_line:90` + fake change id) → single clear all-or-nothing error naming both parts and reasons; exit 1; previous overlay intact. Error does not echo the valid range (F8).
- `chunks set --file round5-s2-chunks.json` with 4 chunks (3 spotlight w/ explanations + artifacts, 1 glance, one chunk spanning retry.rs+worker.rs) → "Set 4 chunks"; every range taken from `chunks lines` validated first try — the ranges/validator agreement fix holds.
- Incremental: `chunks update` (stdin) retitled/extended `test-coverage` in place → "Updated 1 chunks, added 0; total 4". `chunks remove --id does-not-exist` → strict rejection.

The CLI path is materially easier than raw JSON-RPC: no envelope boilerplate, file-based specs, readable errors, and `chunks lines` closes the line-space guessing loop.

## Zen curated vs uncurated

`T` again: "3 chapter(s), 5 focus stop(s), 2 at a glance" (multi-part chunks step per part, titled "(part 1/2)" etc.).

- Chapter cards now render the brief under "what this change does"; ch.2's brief artifact is reachable (`e opens 1 artifact(s)`). **But ch.2's brief was cut mid-sentence with no affordance to read the rest** (F2), and the derived-fact block (including the wrong "top changed symbols") still sits *above* the brief (F11).
- Stops show my `explanation` as "why this matters" — a clear upgrade from derived heuristics; stop 1's `example` artifact rendered as a clean inline overlay (`e`, j/k, esc).
- Glance board: my curated glance item shows title + rationale ("lib.rs re-exports … Mechanical: new modules and pub use lines only."), and `src/priority.rs` was auto-appended as an uncovered leftover — good safety behavior, though unlabeled as such.
- `a` again reported "2 glance item(s) marked viewed" and **lib.rs again stayed unviewed** (F1).

Net: curated zen reads like a briefing (intent, risk, questions, exhibits) instead of a stat sheet; truncation and card noise are what keep it from feeling trustworthy.

## Misc verified

- Op picker (`I`) shows the new preview hint: "will mark 1 caught up · 6 already viewed · 0 need re-review" plus key hints — exactly the missing confidence signal from earlier rounds.
- Session `gander-r5-s2` killed; other sessions untouched.

# Findings

**F1 (major) — Glance acknowledgment claims files are marked viewed but session viewed-state disagrees.**
Repro: fixture stack, `T`, `n` through all spotlight stops, `a` on the glance board (containing `src/lib.rs`). Footer says "zen complete — 2 glance item(s) marked viewed" yet the file tree still shows `• lib.rs` and 6/7 viewed. Reproduced on both the uncurated run (change-scoped glance items) and the curated run (session-scoped glance chunk without `change_id`). Either the glance viewed-marking writes to a different scope than the session viewed tracker, or it silently fails; either way zen reports completion the file pane contradicts.

**F2 (major) — Curated change briefs are silently truncated on the chapter card.**
Repro: set a ~5-sentence brief for `kynzuzwpppol` via `review/set_change_briefs`, `T`, `n` to chapter 2/3. Brief ends "…empty-payload jobs may" — no ellipsis, no scroll, no expand. `d` ("toggle chapter details") only collapses/expands the *commit description* ("… d expands the description (2 more line(s))"), not the brief. The card is a fixed ~72-col box centered in a 200x50 terminal with huge unused margins, so the truncation is purely a layout choice. Curated narrative — the whole point of briefs — is lost without warning to either the author (no warning on write about length) or the reader.

**F3 (minor) — Purely-added public symbols in mixed hunks are labeled "signature changed" with a misleading review question.**
Repro: `T`, stop on `src/queue.rs` [feat] → "public API change: pub fn enqueue_with_priority( signature changed · review question: do callers handle the new signature?"; stop on `src/worker.rs` [refactor] → same for `pub fn dropped(&self) -> u64`. Both symbols verifiably do not exist in the parent revision (`jj file show -r sxqzuzqv src/worker.rs`, `jj file show -r main src/queue.rs`). Meanwhile `pub enum Priority` (new file) and `pub fn for_tests()` (pure-addition hunk) correctly read "new public API". The round-5 fix appears to detect "purely added" per hunk/file, not per symbol; a new fn inside a mixed hunk gets the wrong label and a nonsense question (a brand-new fn has no existing callers).

**F4 (minor) — The empty `@` working-copy change is a stack position and a stack_changes entry.**
Repro: `>` at launch → "already at the top of the stack"; step to `4/4: (no description)` → "No changed files … Current target: knzlymnkwwpk-..knzlymnkwwpk". ACP `review/stack_changes` likewise returns the empty change (`"description":""`, `current:true`). With the standard jj `jj new`-on-top workflow, every review ends in a dead position, and every curation agent must special-case an undocumented empty entry. (Base exclusion itself works — 1/4 is the feat change.)

**F5 (minor) — "top changed symbols" on chapter cards includes symbols the change never touched.**
Repro: `T`, chapter 2/3 (refactor extracting retry) → "top changed symbols: mod config, mod priority, mod queue, mod retry, mod worker". The change only adds `pub mod retry`/`pub use` to lib.rs and touches retry.rs/worker.rs; `mod config/priority/queue` are unchanged context lines inside lib.rs's hunk. The dependency line "builds on ch.1 …: mod config, mod priority, mod queue, mod worker" inherits the same noise, overstating coupling.

**F6 (minor) — Uncurated glance board shows duplicate, indistinguishable items.**
Repro: uncurated `T`, tour to the glance board → two identical rows "• src/lib.rs — source" (one per owning change) with no change id/description to tell them apart, under a confusing header "2 glance group(s) below (2 item(s))".

**F7 (papercut) — `chunks lines` coalesces multiple hunks into one entry with a stale header.**
Repro: `gander -b main -r @ chunks lines --change sxqzuzqvywym --path src/queue.rs` → one entry `header:"@@ -1,14 +1,20 @@", start_line:1, end_line:58`, while the actual change diff has two hunks (new ranges 1–20 and 24–58). The header contradicts the range, and docs/acp.md's example implies per-hunk entries. Ranges I derived from it were all accepted, so it "agrees with the validator", but the merged span also implies lines 21–23 (between hunks) are anchorable.

**F8 (papercut) — Chunk validation errors don't say what would be valid.**
Repro: pipe a spec with `start_line:90` for queue.rs → `error: … line range outside diff line space for src/queue.rs`. Accurate and all-or-nothing, but it neither echoes the valid ranges nor points at `gander chunks lines`, which costs an authoring round-trip.

**F9 (papercut) — Chapter-card rendering: repeated "stops:" prefixes and narrow fixed width.**
Repro: any chapter card — four consecutive lines each begin "stops:" ("stops: roles…", "stops: churn…", …), and the card wraps aggressively (~72 cols) on a 200-col terminal, even breaking the commit body as "The queue keeps a / band per priority".

**F10 (papercut) — `gander acp` gives no signal whether it bridged to the live TUI or served a snapshot.**
Repro: run `gander acp` with the TUI up; responses are identical in shape to snapshot mode. I only knew the bridge worked because the TUI footer flashed "agent suggestions updated". An agent authoring against a stale snapshot would not notice.

**F11 (papercut) — Derived-fact noise still dominates curated chapter cards.**
Repro: curated chapter 2/3 — the (partly wrong, see F5) "stops: top changed symbols…" block renders above the agent brief, while the brief itself is truncated (F2). Once a brief exists, derived facts should be demoted or collapsed.

# Scores

**TUI review ergonomics: 4/5.** The core loop is fast and confident: task-oriented "first review loop" help, `enter` mark-viewed-and-advance, precise line-anchored comments with editor hints and rich persisted anchors, stack stepping that now excludes the base with stable denominators, an op picker with an effect preview, and instant live pickup of agent suggestions with a clean accept/edit/discard triage. Docked a point for the glance viewed-state inconsistency (F1, which undermines trust in the viewed counter — the loop's core metric) and the dead 4/4 empty-`@` stack stop (F4).

**Zen uncurated: 3/5.** Solidly at the "calmer than browsing, helps with progress" anchor: correct per-change scoping and facts (churn, tests-touched, roles), commit bodies on chapter cards, dependency hints, and generic review questions; the test change spotlights its tests and pure additions read "new public API". But the insight layer is unreliable at the edges — wrong "signature changed" labels in mixed hunks (F3), context-symbol noise (F5), duplicate glance rows (F6) — so I wouldn't yet trust the derived facts without checking the diff.

**Zen curated: 3.5/5.** Curation transforms the tour: briefs give intent and risk on chapter intros, chunk explanations replace heuristics as "why this matters", multi-part chunks step logically with "(part n/m)" titles, artifacts render as clean inline exhibits, and uncovered files fall back to glance. Held below 4 by silent brief truncation (F2 — losing curated content is the cardinal sin here), derived-fact clutter above the brief (F11), and the same glance viewed-state bug (F1).

**Curation protocol ergonomics: 4/5.** The authoring loop is validated, incremental, and genuinely ergonomic: `chunks lines` closes the line-space loop and its ranges were accepted first try; errors are all-or-nothing with per-part reasons; `update`/`remove` have correct upsert/strict semantics; brief-without-spotlight warnings are advisory as documented; the CLI (`chunks`/`briefs`/`drafts` with inline help examples) is discoverable and clearly easier than raw JSON-RPC. Docked for the merged-hunk/stale-header wart (F7), errors that don't state the valid space (F8), no bridged-vs-snapshot signal (F10), and the undocumented empty-`@` entry in `stack_changes` (F4).

# Top proposals

1. **Make glance acknowledgment actually mark session viewed state (F1).** `a` on the glance board must produce the same ✓ the file tree and the n/m counter use, regardless of whether the item is change-scoped or session-scoped; if a file intentionally stays unviewed, say why instead of claiming it was marked.
2. **Never truncate curated briefs (F2, F11, F9).** Let the chapter card grow into the abundant free space (or make it scrollable with a visible "… j/k for more"), extend `d` to expand the brief, and demote/collapse the derived-fact block once a brief exists. Optionally warn at `briefs set` time when a brief exceeds the renderable size.
3. **Detect purely-added symbols per symbol, not per hunk (F3).** `enqueue_with_priority` and `dropped()` should read "new public API: …" with the "is this the right surface?" question; reserve "signature changed / do callers handle it" for symbols that existed in the parent. While in there, filter context-only symbols out of "top changed symbols" (F5).
4. **Handle the empty working-copy change explicitly (F4).** Skip empty changes in stack stepping (or annotate "4/4 · empty working copy"), and either omit them from `stack_changes` or tag them (`"empty": true`) so curation agents don't brief a ghost.
5. **Tighten the authoring feedback loop (F7, F8, F10).** Emit per-hunk entries (or fix the merged header) in `chunks lines`, include the valid ranges (or a pointer to `chunks lines`) in part-validation errors, and print/return whether `gander acp` bridged to a live instance or a snapshot.
