# Summary

**The ambient watch mode is completely dead in this build against jj 0.42.0, while the footer confidently claims "following @".** The root cause is the change-fingerprint poll: `src/jj.rs:229` uses the template `if(self, "@ " ++ ...)`, and jj 0.42.0 rejects it (`Expected expression of type Boolean, but actual type is Commit`). Every 2-second poll therefore fails, and because `maybe_refresh_review` deliberately swallows fingerprint errors as "transient", the failure is permanent and invisible. Over a ~12-minute session with seven repo mutations (plain edits, rapid edits, edits to a viewed file, `jj describe`, `jj new` + edit, `jj abandon`, `jj undo`), the pane never refreshed once, the stats stayed frozen at `+134/-12`, no "@ moved / entered range / updated" notice ever appeared, and the ctrl-a activity feed stayed empty the entire time.

Everything downstream of the fingerprint — freshness badges (`±`), viewed-stale markers (`~`), changed-hunk suffixes, activity events, @-follow notices — is unreachable, so the W4 watch-legibility features effectively do not exist for a real user on current jj. The designed architecture looks right (in-place reload preserving selection/viewed/comments, per-file and per-hunk freshness fingerprints, capped activity feed), and the manual escape hatches are genuinely good: retargeting via `R`/`t` reloads correctly with comments and viewed-staleness preserved, and the `I` "compare against prior operation" popup is an excellent catch-up primitive (though it computed against the stale in-memory diff and wrongly marked a changed file viewed).

Most important product gaps:

1. A watch pane whose core poll can fail permanently **must not** advertise "following @" without a health/staleness indicator; silent-skip on error turns one bad template into an all-day lie.
2. No integration coverage against a real jj binary for the fingerprint template — a template typo shipped despite unit tests passing (they stub jj).
3. The stale footer notice ("info: loaded main..@" persisting for minutes across five mutations) reinforces false freshness instead of communicating pane age.

# Step log

Environment: gander release binary `target/release/gander` (built Jul 5 17:22), jj 0.42.0, fixture `/tmp/gander-eval-r3/fixture-3` (task-queue Rust lib, `main` + 3 commits + empty `@`). Launched in tmux 200x50: `gander -b main -r @ tui`. All jj mutations used `--config user.name/user.email --config signing.behavior=drop`.

**Setup & seeding.** TUI loaded instantly: 7 files, `+134/-12`, footer `7 files (0/7 viewed …) · following @` — follow mode for the symbolic target is clearly indicated. `?` help is dense but complete; noted `ctrl-a` was *not* listed in help (found via source exploration), while `I` (diff against prior jj operation) and `]/[` changed-symbol nav were listed. Marked `config.rs` viewed (`✓`, moved to bottom of its directory, count `1/7`). Added a line comment on `src/lib.rs:4` (`c`, type, `ctrl-s`): rendered inline as `↳ f9392bac [draft] consider re-exporting retry policy here too`, footer count `1 comments`. The comment popup does not show which file/line it is anchored to — you have to trust your cursor position.

**Mutation 1 — plain working-copy edit to already-changed file.** Appended 3 lines to `src/worker.rs`. Captures at +5s, +11s: no change (`+134/-12`, no badge, no notice). Ran `jj status` to force a snapshot (`@` → `6e6f8023`, jj sees `src/worker.rs | 3 +++`); captures at +5s, +9s, +24s after the snapshot: still frozen. A keypress did not trigger refresh either.

**Diagnosis detour.** Verified the TUI process (pid 27460) was alive and in Normal mode. `ctrl-a` opened an **empty** activity feed — zero refresh events since launch. Source exploration: poll runs every 2s in Normal mode via `change_fingerprint` (`jj log -r main..@ --template 'if(self, "@ " …)'`), errors silently skip the tick (`src/tui/mod.rs:709-716`). Reproduced the exact command manually:

```
Error: Failed to parse template: Expected expression of type `Boolean`, but actual type is `Commit`
  | if(self, "@ " ++ change_id.short() ++ ...
exit: 1
```

Every poll fails; the pane can never refresh. Root cause for all subsequent observations.

**Mutation 2 — rapid successive edits.** Three appends to `src/queue.rs` 1s apart + `jj status`. +5s: frozen (`+134/-12`).

**Mutation 3 — edit a viewed file.** Appended to `src/config.rs` (previously `✓`) + snapshot. +5s: still `✓`, no `~` stale marker, no refresh. Then exercised the manual workaround: pressed `t` (trunk()..@). Reload happened with notice `info: loaded trunk()..@`; `config.rs` correctly showed **`~` viewed-stale** and dropped from the viewed count; the comment survived (`1 comments`). Side note: `trunk()` resolved to `root()` in this remote-less fixture, so the "trunk" preset silently showed all 8 files as `added` (+248/-0) — accurate but disorienting. Retargeted back with `R` (first attempt failed gracefully: typing appends to the pre-filled `trunk()` field and ctrl-u doesn't clear it, producing `trunk()main..@` and a clear footer error; backspacing worked). `main..@` reloaded at `+148/-12` picking up all edits — but here `config.rs` rendered as plain `•` unviewed, not `~` stale, inconsistent with the trunk()..@ rendering moments earlier.

**Mutation 4 — describe / new / edit so `@` moves.** `jj describe -m "wip: eval ambient edits"`, `jj new` (@ → `murrqtrn`), appended 2 lines to `tests/basic.rs`, snapshot. +6s: frozen at `+148/-12`, no "@ moved" notice; footer still displayed the minutes-old `info: loaded main..@` notice.

**Mutation 5 — abandon and undo.** `jj abandon` (@ → `lvrrqurv`): +6s, frozen. `jj undo` (restored `murrqtrn`): +6s, frozen. Activity feed still empty after ~10 minutes of churn.

**Catch-up affordances.** `I` opened a well-designed "prior operations" popup (op id, age, description, explanatory subtitle "unchanged files are marked viewed"). Selected the snapshot from ~9 minutes prior: notice `info: since op 151eee1f7ef9: 5 unchanged file(s) marked viewed, 2 need re-review`, leaving `config.rs`/`queue.rs` unviewed at the top. Two problems: (a) it computed against the stale in-memory diff (`+148/-12`, missing the `tests/basic.rs` edit) and wrongly marked `basic.rs` viewed even though it changed after that op; (b) it marks never-looked-at files (e.g. `retry.rs`) viewed, conflating "unchanged since op" with "reviewed". No dedicated next/previous *fresh-hunk* key exists; `]/[` in the diff is changed-*symbol* (tree-sitter) navigation, and the per-hunk "changed" suffix is only set by the (broken) in-place refresh.

Quit with `q`; killed tmux session `geval` (left the unrelated parallel session `gander-tui-eval` untouched).

# Findings

1. **[blocker] Watch/follow never refreshes on jj 0.42.0: invalid fingerprint template.**
   Repro: in any jj 0.42.0 repo, run `gander -b main -r @ tui`, then edit a file and run `jj status` (or any jj op). The pane never updates. Manual repro of gander's poll command: `jj log -r main..@ --no-graph --template 'if(self, "@ " ++ change_id.short() ++ " " ++ commit_id ++ "\n", "") ++ …'` → `Expected expression of type Boolean, but actual type is Commit` (source: `src/jj.rs:229`; likely intended `current_working_copy`). Every 2s poll fails, so refresh, "@ moved/entered/left" notices, `±`/`~` badges, changed-hunk markers, and activity events never fire.

2. **[major] Persistent watcher failure is silent; footer keeps claiming "following @".**
   Repro: with the broken template (or any persistently failing jj invocation), the pane shows `following @` and stale stats indefinitely; the error-skip in `src/tui/mod.rs:709-716` treats permanent failures as transient. There is no last-refreshed timestamp, no "watch degraded" state, no error after N consecutive failures. For an all-day pane beside an autonomous agent this is the worst failure mode: confidently wrong.

3. **[major] `I` compare-against-operation computes on a stale diff and mis-marks changed files as viewed.**
   Repro: with the pane stale (here: `tests/basic.rs` edited after last load), press `I` and pick an op older than the edit. `basic.rs` is marked viewed as "unchanged" because the comparison uses the in-memory diff instead of reloading current state first. Even with the poll fixed, a race window exists; the flow should refresh the target before comparing.

4. **[major] No real-jj integration test for the fingerprint/poll path.**
   The template bug ships while `cargo test` passes because polling tests stub jj. A smoke test executing `change_fingerprint` against a real `jj` binary (already a dev dependency of the environment) would have caught this immediately.

5. **[minor] Footer notices never age out, implying false freshness.**
   Repro: trigger any notice (e.g. `info: loaded main..@`), then mutate the repo repeatedly; the minutes-old notice persists through five mutations, reading as if it just happened. Notices need timestamps or expiry.

6. **[minor] "Unchanged since op → mark viewed" clobbers genuine unreviewed state.**
   Repro: leave `retry.rs` never-viewed, press `I`, pick any op it hasn't changed since → it becomes `✓`. Catching up on *my* review shouldn't assert I reviewed files I never opened; consider only re-marking files that were previously viewed.

7. **[minor] Viewed-stale rendering is inconsistent across retargets.**
   Repro: mark `config.rs` viewed, edit it, retarget to `trunk()..@` → shows `~` (stale); retarget back to `main..@` → shows plain `•` unviewed. The "you saw an older version of this" signal is lost depending on target string.

8. **[papercut] `ctrl-a` activity feed is not listed in `?` help.**
   The help panel documents zen/comments/targets/agent keys but omits the activity feed entirely; I found it only by reading source. The one key designed for "what happened while I looked away" is undiscoverable.

9. **[papercut] Revset input field doesn't support ctrl-u clear and appends to the pre-filled value.**
   Repro: press `R`, type `main` → field becomes `trunk()main` → `error: failed to load trunk()main..@`. Recovery required 12 backspaces. (Error surfacing itself was good.)

10. **[papercut] Comment editor doesn't display its anchor (file:line) in the popup or footer.**
    Repro: press `c` on a diff line; the popup shows only free text. In an ambient pane where the diff may reload underneath you, showing the anchor would build trust.

# Scores

- **Watch freshness/follows-@: 1/5.** The pane went stale immediately and stayed stale across every mutation class in the scenario (plain edit, rapid edits, viewed-file edit, describe/new/@-move, abandon, undo) for the entire session. Follow-mode labeling is present but false in practice. This is the rubric's "pane often goes stale... " anchor exactly, with the aggravator that it *advertises* following.
- **Watch change awareness: 1/5.** All change-awareness machinery (notices, `±` badges, changed-hunk suffixes, activity feed) is gated behind the broken refresh, so changes were 100% silent; the activity feed stayed empty all session. The `I` op-comparison is a genuinely promising catch-up affordance but it mis-marked a changed file as viewed on stale data, so it can't rescue the score.
- **Pane-worthiness: 1.5/5 (report as 1–2).** As shipped against jj 0.42.0, the pane cannot be trusted without manual retarget-to-refresh, and worse, it looks trustworthy while wrong. The half point is for what works: state durability (comments/viewed survived reloads), graceful revset error handling, and manual reload paths that preserve review state — the skeleton of a good pane is visibly there.

# Top proposals

1. **Fix the fingerprint template and pin it with a real-jj test.** Replace `if(self, …)` with `if(current_working_copy, …)` (or equivalent for the running jj version) in `src/jj.rs:229`, and add an integration test that runs `change_fingerprint`, `run_diff`, and `operations` against an actual `jj` binary in a temp repo so template/CLI drift fails CI, not the user.
2. **Make watch health first-class in the footer.** Track consecutive fingerprint failures; after ~3, replace `following @` with something like `watch stalled (jj error) — press … for details`, and always show a last-refresh age (`fresh 4s ago`). A watch pane's core invariant is that silence means "nothing changed", never "I broke".
3. **Refresh before comparing in the `I` op-catch-up flow** so "unchanged files are marked viewed" is computed against current reality, and restrict auto-mark-viewed to files that were previously viewed (leave never-viewed files alone). This turns an already-good affordance into the pane's killer feature for "I looked away for an hour".
4. **Add a freshness-first navigation key and surface ctrl-a in help.** A single key (e.g. `*` or reuse `]` with a modifier) cycling through hunks changed since last look, plus listing the activity feed in `?`, would make catch-up a two-keystroke habit; today freshness marks are passive and the feed is hidden.
5. **Give notices timestamps/expiry and unify stale-viewed rendering.** Age or auto-expire footer notices, and make the `~` viewed-stale marker consistent across retargets so "you saw an older version" survives target changes — both directly serve the "can I trust this at a glance" test an ambient pane lives or dies by.
