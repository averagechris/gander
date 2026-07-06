# Summary

Gander's ambient watch pane is genuinely fresh: every mutation I threw at it — a working-copy edit, a burst of rapid edits, `jj describe`, `jj new`, `jj undo`, and `jj abandon` — was reflected within one poll cycle (~2–5s), with explicit, change-id-aware notices like `@ moved to llyymqkxzopp · change llyymqkxzopp entered range · change yptkrzmnkkyt left range`. The footer permanently shows `following @`, and the file tree marks changed-since-look files with `±`/`~` badges.

The two things that break trust for all-day use are review-state fidelity and jj side effects:

1. **Inline comment annotations vanish from the diff after any refresh.** The comment survives in the comment list (`C`) and the status-bar count, but its gutter marker and body line at the anchored line never re-render — not even when jumping to it from the comment list. Your own review notes silently disappear from the code you are reading.
2. **Gander's polling pollutes the jj op log with `snapshot working copy` operations**, so a user's (or agent's) `jj undo` undoes *gander's* snapshot instead of their own last operation. In my run, `jj undo` reverted my `worker.rs` edit on disk because the topmost op was gander's poll snapshot.

Secondary gaps: viewed state is silently dropped (1/7 → 0/7) when a viewed file changes instead of showing a "needs re-review" count; the file tree re-sorts on viewed/changed transitions (files jump around); revert notices lack deltas and don't say "reverted"; the latest info notice covers the key-hint footer until `ctrl-g`.

# Step log

Fixture: `/tmp/gander-eval/round3b-s3` (task-queue Rust lib; stack of 3 commits on `main` + empty `@`). Launched in tmux session `gander-r3-s3` (200x50) via:

```
cd /tmp/gander-eval/round3b-s3 && /Users/chris/projects/gander/target/release/gander -b main -r @ tui
```

All jj mutations used `jj --config 'user.name="Eval"' --config 'user.email="eval@example.com"' --config signing.behavior=drop ...`.

## Seeding review state

- Initial render: 7 files, `+134/-12`, footer `main..@ · focus files · … · following @`. Correct.
- `?` help: comprehensive two-column overlay; documents badges `± changed since look · ~ viewed, changed since`.
- `v` on `src/config.rs`: marked viewed (`✓`, counter 1/7) — **and the file re-sorted to the bottom of the `src` group**.
- `Tab` to diff, cursor to line 10, `c`: comment popup opened, footer said `new comment · refresh paused · ctrl-s save · esc cancel` (refresh pausing during edit is a good touch). Popup title is just `comment` — no file:line anchor shown. Saved; inline annotation rendered:

  ```
  1  10 +     /// Configuration suitable for tests: fast polling, one retry.
        ↳ bf18082f [draft] for_tests should probably set max_retries to 0 ...
  ```

  Status bar: `1 comments` (grammar nit).

## Mutation 1: edit an already-changed (and viewed) file

Appended a const to `src/config.rs`. ~5s later:

- Stats updated `+134/-12 → +137/-12`; `config.rs` header `+11 -0 → +14 -0`; new hunk shown with header tag `@@ -15,3 +26,6 @@  changed`.
- Notice: `info: repository changed — change llspwtmrmryn updated · src/config.rs updated (+3 −0)`. Accurate per-file delta.
- `config.rs` badge went `✓ → ±`, moved back to the top of the tree, **viewed counter dropped 1/7 → 0/7** with no explanation.
- **The inline comment annotation at `src/config.rs:10` disappeared from the diff.** `C` still lists `[ draft ] src/config.rs:10 …`; `enter` (jump) from the list moved the cursor but did not re-render the marker. It never came back for the rest of the session.

## Mutation 2: rapid successive edits

Five appends to `src/config.rs` at ~0.4s intervals. Pane debounced them into two refreshes (activity feed shows `+4 −0` then `+6 −0` events one second apart), ended accurate at `+24 -0` / `+147/-12`, all five constants visible. No flicker, no stale state. Notice shows only the last batch's delta, not a cumulative "since you last looked" figure.

`ctrl-a` activity feed: timestamped history of every refresh with per-file deltas — genuinely useful, but each event is rendered ~3 times (combined line + per-file line + per-change line).

## Mutation 3: describe, new, edit a different file

- `jj describe -m "chore: eval tweaks to config"` → notice `change llspwtmrmryn updated` within 5s.
- `jj new -m "feat: eval new change"` → notice `@ moved to yptkrzmnkkyt · change yptkrzmnkkyt entered range`. File list correctly unchanged (range is `main..@`). Footer still `following @`.
- Append to `src/worker.rs` → `worker.rs` gains `±`, stats `+147/-12 → +150/-12`, notice `change yptkrzmnkkyt updated · src/worker.rs updated (+3 −0)`.

## Mutation 4: undo and abandon

- `jj op log` revealed gander's polls create ops: `snapshot working copy / args: jj log -r main..@ --no-graph --color=never …` interleaved with my own ops.
- `jj undo` therefore undid **gander's snapshot**, not my op — reverting my `worker.rs` edit on disk (`Added 0 files, modified 1 files`). The pane did refresh correctly (`+150/-12 → +147/-12`), but the notice said only `src/worker.rs updated` — no delta counts, no indication content was *reverted*.
- `jj abandon @` → notice `@ moved to llyymqkxzopp · change llyymqkxzopp entered range · change yptkrzmnkkyt left range`. Best-in-run change summary; the pane visibly tracked `@` through abandonment.

## Badge semantics probe

Re-marked `config.rs` viewed (`✓`, 1/7), edited it again: `✓ → ±`, counter back to 0/7. After moving focus into the diff, the badge transitioned `± → ~` ("viewed, changed since"). So `~` exists but the viewed *counter* treats such files as plain unviewed, and nothing shows what changed since the viewed mark (no interdiff). Also: `G` (jump to bottom) briefly rendered a completely blank diff pane (scroll past end) that self-corrected on the next poll.

Quit with `q` (dumped a review artifact to the shell) and killed the tmux session.

# Findings

1. **Inline comment annotations disappear from the diff after any watch refresh — major.**
   Repro: launch `gander -b main -r @ tui`; add a line comment on any diff line (`c`, type, `ctrl-s`) and confirm the gutter marker + `↳` body render; append any line to any changed file in the repo; wait ~5s. The annotation is gone from the diff even though the anchored line's content and number are unchanged; `C` still lists the comment at the same `file:line`; jumping to it from the list does not re-render it. Your review notes are invisible exactly where you need them.

2. **Gander's polling writes `snapshot working copy` ops into the jj op log, turning `jj undo` into a trap — major.**
   Repro: with the TUI open, edit a file, wait for a refresh, run `jj op log` (top op is gander's `jj log -r main..@ …` snapshot), then `jj undo`. The undo targets gander's snapshot and reverts your working-copy edit rather than undoing your last intentional operation. Beside an autonomous agent issuing jj ops, interleaved watcher snapshots make undo semantics unpredictable for both parties.

3. **Viewed state is silently dropped when a viewed file changes — minor.**
   Repro: `v` a file (counter 1/7), append a line to it, wait ~5s. Badge becomes `±`, counter shows 0/7 with no message like "1 file needs re-review". The `~` (viewed-but-changed) badge only appears after further interaction, and there is no way to see the delta since your viewed mark (no interdiff), so "re-review" means re-reading the whole file diff.

4. **File tree re-sorts on viewed/changed transitions — minor.**
   Repro: mark the first file in a group viewed → it jumps to the bottom of the group; mutate it → it jumps back to the top. In an ambient pane files relocate on their own, defeating spatial memory. Stable ordering (or a visible "sorted: unviewed first" indicator) would help.

5. **Change notices are inconsistent and ephemeral — minor.**
   Repro: (a) edit a file → notice includes `(+3 −0)`; undo/revert the same file → notice says only `src/worker.rs updated`, no counts, no "reverted" wording. (b) The single info line replaces the previous notice and covers the key-hint footer until `ctrl-g`; miss it and you must know about `ctrl-a` to recover history. (c) In the activity feed each refresh is listed ~3 times (combined + per-file + per-change lines).

6. **`G` (jump to bottom) can scroll past the end and blank the diff pane — papercut.**
   Repro: focus diff in a file whose diff is shorter than the last scroll target, press `G`; the pane rendered entirely blank for ~2s until the next poll re-clamped it.

7. **Comment editor shows no anchor context — papercut.**
   Repro: press `c` on a diff line; the popup is titled just `comment` with no `file:line` or code excerpt, so you cannot confirm what you are annotating before saving. (Counterpoint: `refresh paused` during editing is exactly right.)

8. **Status bar grammar: `1 comments` — papercut.**

# Scores

- **Watch freshness/follows-@: 4/5.** Every mutation type — edit, rapid edit burst, `describe`, `new`, `undo`, `abandon` — was reflected within one poll cycle, with notices tied to change IDs and a persistent `following @` indicator; `@` movement through `new` and `abandon` was tracked visibly and correctly. Withheld a point because gander's own snapshot ops corrupt `jj undo` semantics (finding 2) — the watcher is fresh partly by mutating the thing it watches — plus the transient blank-pane blip.

- **Watch change awareness: 4/5.** Per-file delta notices, enter/leave-range summaries on `@` movement, a timestamped activity feed, `±`/`~` badges, and `changed` tags on hunk headers comfortably exceed the "aggregate stats only" anchor. Missing for a 5: reverted/removed work is not marked as such (undo notice had no counts or direction), notices show only the latest batch delta rather than delta-since-last-look, there's no interdiff for "viewed but changed" files, and the feed's triple-listing adds noise.

- **Pane-worthiness: 3/5.** As a live diff/status aid it is trustworthy — I never caught it stale or following the wrong revision. But an all-day reviewer's pane must also preserve the reviewer's own state: comments vanishing from the diff after every refresh (finding 1), viewed progress silently zeroing (finding 3), files re-sorting under you (finding 4), and the undo trap (finding 2) mean I would keep it open but would not trust it to tell me what still needs review after a busy agent hour.

# Top proposals

1. **Re-anchor and re-render inline comments across refreshes.** Comments must survive watch refreshes in the diff view, with explicit drift handling: keep the marker when the anchored line is unchanged, and show a "comment anchor moved/stale" indicator when it isn't. This is the single biggest trust fix for the pane.
2. **Make polling snapshot-free (or clearly cooperative).** Use `--ignore-working-copy` for read-only polls, or an op-log watching strategy that never writes `snapshot working copy` ops attributed to the user, so `jj undo` always targets the user's/agent's own operations.
3. **Introduce a "needs re-review" state instead of dropping viewed.** Count `~` files separately in the status bar (e.g. `1/7 viewed · 1 changed since viewed`), offer a keybinding to jump to them, and show an interdiff (what changed since the viewed mark) rather than forcing a full re-read.
4. **Stabilize file-tree ordering in watch mode.** Don't re-sort on viewed/changed transitions while the pane is ambient; badges already carry the signal.
5. **Richer, sticky operation summaries.** Tag notices with the operation kind (edited/reverted/abandoned), include deltas consistently, accumulate "since you last looked" rather than per-poll batches, keep the key-hint footer visible, and de-duplicate activity-feed entries.
