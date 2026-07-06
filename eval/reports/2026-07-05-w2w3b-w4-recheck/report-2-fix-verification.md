# Fix verification re-check — fixture-2

- Binary: `/Users/chris/projects/gander/target/release/gander`
- Fixture: `/tmp/gander-eval-r4/fixture-2` (stack: `main` = chore initial lib → feat priority → refactor retry → test coverage → empty `@`)
- State file: `/Users/chris/.local/state/gander/fixture-2-9005e442/state.json` (fresh at start of run)
- Method: detached tmux 200x50, single keystrokes, state.json inspected after every step. jj mutations were not needed — the fixture already had an empty `@` at the top of the stack.

# Summary

| # | Claim | Verdict |
|---|-------|---------|
| 1 | [was blocker] Comment/viewed erasure on empty-diff targets | **PARTIAL FAIL** — the original wipe (`files: {}` / `comments: []`) on empty changes is fixed and comments now survive everything; but **viewed entries still get erased** when stack-stepping onto a change where the same path has a different diff (path-keyed clobber), and are not restored on return |
| 2 | [was major] Zen chapter cards show session-global stats | **PASS** — each chapter card's roles/churn/symbols lines are scoped to that chapter's own change/files and differ across chapters |
| 3 | [was major] No way back to launch target | **PASS** — `t` ("return to launch target" in help) restores `main..@` after stack-stepping and after chooser retargets; the loaded target is reported literally as `main..@`, not `trunk()..@` |
| 4 | [was major] Base chooser fuzzy ranking | **FAIL** — exact `main` now *ranks* first, but the selection cursor does not follow the filter, so pressing Enter lands on the wrong change (`lqtnkwlqyoll..@` instead of `main..@`) |

# Verification log

## Claim 1 — comment/viewed survival across stack steps incl. empty change

Setup: state dir was absent (`gander paths` reported `state.json (absent)`).

1. `gander comments add -b main -r @ --path src/queue.rs --line 5 --body "Eval recheck comment: survives stack stepping?" --kind note` → comment persisted to state.json with a full line anchor; session target recorded as `main..@`.
2. Launched TUI `gander -b main -r @` in tmux. Footer target line: `main..@ · focus files · …`, status `1 comments · following @`.
3. Pressed `Enter` on `src/config.rs` → marked viewed. state.json: `src/config.rs {fingerprint fc979565…, viewed: true}`, 1 comment.
4. Stepped down the stack with `<` four times, checking state.json after each step:
   - `<` → stack 4/5 (test change): viewed `['src/config.rs']`, 7 entries, 1 comment ✓
   - `<` → stack 3/5 (refactor): viewed `['src/config.rs']`, 7 entries, 1 comment ✓
   - `<` → stack 2/5 (feat): viewed `['src/config.rs']`, 7 entries, 1 comment ✓
   - `<` → stack 1/5 (chore initial lib): **viewed `[]`**, 8 entries, 1 comment ✗ — the `src/config.rs` entry was overwritten with the stack-1 fingerprint `8eb1857b…` and `viewed: false`.
5. Stepped back up with `>` four times through stack 5/5 (the **empty** change, "0 files (0/0 viewed) … 1 comments"): comments stayed at 1 and files entries stayed at 8 on every step — **no wipe on the empty change**. state.json was never reset to `files: {}` / `comments: []`.
6. Pressed `t` → `info: loaded main..@`. state.json: `src/config.rs` entry now has the original fingerprint `fc979565…` **but `viewed: false`** — the viewed mark did not survive the round trip. TUI showed `0/7 viewed`.
7. The comment was live after retargeting: cursor on `src/queue.rs` showed `↳ 1439f8b7 [draft] [note] Eval recheck comment: survives stack stepping?` inline in the diff.
8. Clean minimal repro of the viewed loss (same session): re-marked `src/config.rs` viewed at `main..@` (`viewed: true, fp fc979565`) → `<` ×4 to stack 1/5 → state.json shows `viewed: false, fp 8eb1857b` → `t` back to `main..@` → `viewed: false, fp fc979565`. Deterministic.
9. Quit with `q`; post-quit state.json: 8 file entries, 1 comment with intact body.

Verdict: the specific claimed fix (empty-diff target erasing comments + all viewed state) **is** fixed. But the claim's acceptance criterion "comments and viewed entries must survive" is only half-met: viewed entries are silently destroyed by visiting any stack entry where the same path has a different diff, because the `files` map is keyed by path and holds a single `{fingerprint, viewed}` slot that gets clobbered.

## Claim 2 — zen chapter card stats scoping

1. Curated 3 spotlight chunks via `gander chunks set -b main -r @ --file -`, each anchored to a different `change_id` (`lqtnkwlq` feat, `ununsrtv` refactor, `rxytrrlp` test) with parts referencing that change's files. `Set 3 chunks`.
2. Launched TUI `-b main -r @`, pressed `T` → `zen: 3 chapter(s), 6 focus stop(s), 1 at a glance`.
3. Chapter cards (toured with `n`):
   - **Chapter 1 · lqtnkwlq**: `3 file(s) · +52 −5` (matches jj diff of that change exactly); `roles: 2 source`; `churn: +50 −5 · no tests touched` (= the chapter's two stop files, priority.rs +25, queue.rs +25 −5); `top changed symbols: enum Priority, impl Priority, fn default, fn as_str, struct Job`.
   - **Chapter 2 · ununsrtv**: `3 file(s) · +52 −6`; `roles: 2 source`; `churn: +51 −6 · no tests touched` (retry.rs +22, worker.rs +29 −6); `top changed symbols: struct RetryPolicy, impl RetryPolicy, fn from_config, fn should_retry, struct Worker`.
   - **Chapter 3 · rxytrrlp**: `2 file(s) · +30 −1`; `roles: 1 source · 1 tests`; `churn: +30 −1 · tests touched`; `top changed symbols: impl Config, fn for_tests, fn processes_jobs_in_order, fn high_priority_jobs_first, fn empty_payload_jobs_are_dropped`.
4. All three cards show distinct, chapter-appropriate roles/churn/symbols; no session-global (+134/−12, 7 files) numbers repeated anywhere; the derived stats do not contradict the per-change file/churn summary on the same card (churn lines are scoped to the chapter's stop files, the `N file(s)` line to the whole change).

Verdict: **fixed**.

## Claim 3 — return to launch target

1. Launched `gander -b main -r @`. Footer target line at launch: `main..@ · focus files · …`.
2. Stack-stepped `<` ×4 and `>` ×4 (through the empty tip), then pressed `t` → `info: loaded main..@`, 7 files `+134/-12`, `following @` — identical to the launch view. The literal `main..@` string is preserved (would read `trunk()..@` if `-b main` were lost; it does not).
3. Help (`?`) documents it: `t  return to launch target`.
4. Also exercised after a chooser retarget (see claim 4): from `lqtnkwlqyoll..@`, `t` again restored `main..@`.

Verdict: **fixed**.

## Claim 4 — base chooser fuzzy ranking

1. In the TUI at `main..@`, pressed `b` → "Choose base for main..@ (tab toggles base/tip)" chooser with 6 rows; initial selection `›B` on `vpzqvnmxnlto main` (current base).
2. Typed `main`. Filtered list (top to bottom):
   ```
    B   vpzqvnmxnlto main   chore: initial task queue library   ← ranked first ✓
        ununsrtvsvlt        refactor: extract retry policy from worker
   ›    lqtnkwlqyoll        feat: priority scheduling for the job queue   ← selection cursor ✗
   ```
   The exact `main` bookmark ranks first — the ranking part of the fix works. But the selection cursor `›` stayed at a stale position (it does not reset to the top match, nor track the previously selected item, which was `main` itself).
3. Pressed Enter → `info: loaded lqtnkwlqyoll..@`, 5 files +82/−7 — the **wrong** range. The user typed `main`, saw `main` at the top, and got the feat change as base.
4. Characterization: reopened the chooser (selection starts on the current-base row, index 4 of 6). After typing `m` (4 matches) the cursor sits on the last row; after `ma` (3 matches) still the last row. The cursor appears to keep its old list index (clamped), detached from both the top-ranked match and the previously selected item.

Verdict: **not fixed in practice**. Ranking is correct but Enter does not land on the expected range, which is the outcome that matters.

# New findings

1. **[major] Viewed state is clobbered across stack/target switches (path-keyed single-slot store).** The persisted `files` map keys by path with one `{fingerprint, viewed}` slot. Visiting any target where the same path has a different diff overwrites the slot with `viewed: false`; returning to the original target does not restore the mark. Repro: `gander -b main -r @` → mark `src/config.rs` viewed → `<` ×4 to the bottom stack entry (where config.rs is a fresh add with a different fingerprint) → `t` back → config.rs is unviewed (state.json confirms `viewed: false` under the original fingerprint). Silent loss of review progress in exactly the stacked-review workflow that stack-stepping encourages. (This is the residual half of claim 1.)
2. **[major] Base/tip chooser selection cursor ignores the fuzzy filter.** Typing in the chooser re-ranks the list but leaves `›` at a stale index (not the top match, not the previously selected item). Enter then targets whatever happens to sit at that index — in this run, typing `main` and pressing Enter loaded `lqtnkwlqyoll..@`. Repro in claim 4 log. (This is the residual half of claim 4.)
3. **[nit] Two unlabeled churn figures on zen chapter cards.** Chapter 1 shows `3 file(s) · +52 −5` (whole change) and `churn: +50 −5` (chapter stop files) on the same card with no scope labels; a reader may read them as contradictory. Cosmetic — the numbers are each correct in their own scope.
4. **[nit] Chooser rows that match the fuzzy filter only weakly stay listed.** With filter `main`, `refactor: extract retry policy from worker` and `feat: priority scheduling…` remain as candidates (presumably subsequence matches across the row text). Combined with finding 2 this amplifies the wrong-selection risk; on its own it is just noise.
