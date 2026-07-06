# Summary

Gander's watch/follow behavior is in genuinely strong shape. Used as an ambient side pane
(`gander -b main -r @ tui`, 200x50 tmux) beside simulated agent activity, it refreshed within
one poll cycle (~2–5s) on every mutation I threw at it: working-copy edits, rapid successive
edits, edit-then-revert, `jj describe`, `jj new`, `jj undo`, and `jj abandon`. It never lost
selection, viewed state, or the saved comment; it detected content *reverts* ("reverted to
previously seen content") instead of leaving stale changed-flags; it auto-invalidated viewed
state when a viewed file changed ("was viewed, needs re-review"); it visibly followed `@`
("@ moved to rkpyvyylkpnl") with a persistent `following @ · @ <changeid> <desc>` footer; and
the `I` operation catch-up flow previewed its exact effect ("will mark 5 caught up · 1 already
viewed · 1 need re-review") live per-selection before applying, leaving badges and counts
mutually consistent. The op log stayed clean: zero idle snapshots over 20+ seconds of polling.

The remaining gaps are polish, not correctness: the activity feed is redundant (3 near-identical
rows per refresh), long op descriptions (esp. `undo` with embedded 128-char op ids) truncate in
both the footer *and* the feed even though the footer promises "ctrl-a for detail", feed
navigation ignores the `n`/`e` movement keys used everywhere else (arrow keys only), Enter on
non-file events silently closes the feed, and revision identity lives only in the small bottom
status line rather than the pane chrome.

# Step log

Fixture: `/tmp/gander-eval/round6-s3`, 3-change stack over `main`, `@` an empty change on top.
Launched `gander -b main -r @ tui` in tmux (200x50). All captures ~5s after each mutation.

**Seeding review state.**
- `?` help renders a full two-column key reference in the 50-row pane, including
  `ctrl-a activity feed (live refresh while open)`, `I diff against prior jj operation`, and a
  badge legend (`✓ viewed · ◌ caught up · ~ done, changed since · ± changed since look`).
- `enter` marked `src/config.rs` viewed (✓, footer `1/7 viewed`) and advanced to `lib.rs`.
- `c` on lib.rs line 8 opened a comment box titled `comment · src/lib.rs:8`; hint bar said
  `refresh paused` while typing (nice: no refresh race while composing). `ctrl-s` saved; comment
  rendered inline (`↳ f8747e8f [draft] ...`) and footer showed `1 comment`.
- **Chrome before mutations:** footer line 1 read
  `7 files (1/7 viewed, 0 generated/noisy), +134/-12, 1 comment · following @ · @ okrklwup (no description)`
  and the hint line began `main..@`. A returning glance *can* confirm mode (`following @`),
  current change id, and range — but only from one small status line at the very bottom; the
  panel titles say just `files`/`diff`.

**Mutation 1 — edit already-changed file** (`>> src/queue.rs`):
refreshed in ≤5s; queue.rs got `±`; stats `+134→+136`; notice:
`repository changed — change okrklwupknox updated · op: snapshot working copy · src/queue.rs updated (+2 −0)`.
Selection (lib.rs diff), viewed state, and comment all preserved.

**Mutation 2 — rapid successive edits** (4 appends 0.4s apart):
coalesced into a single event `src/queue.rs updated (+4 −0)`; stats `+140`; no flicker or lost state.

**Mutation 3 — edit UNVIEWED file, then revert** (`src/worker.rs`):
edit → `±` badge + `updated (+1 −0)`. Revert to original bytes → badge returned to `•` and the
notice explicitly said `src/worker.rs reverted to previously seen content`. **No stale changed
flag lingered.** Stats returned to the exact prior values.

**Activity feed (ctrl-a):** timestamped events, each carrying the causal jj op
(`op: snapshot working copy`, `op: describe commit …`, `op: new empty commit`,
`op: abandon commit …`). Enter on a file-named event jumped the diff pane to that file
(worked: landed on worker.rs). Friction: `n` (the app-wide "move down" key) did NOT move
feed selection — only arrow keys did; each refresh emits ~3 near-duplicate rows (aggregate +
per-file + per-change); Enter on change-/op-level events just closes the feed silently.

**Mutation 4 — `jj describe`, `jj new`, edit different file:**
- describe: footer updated live to `@ okrklwup chore: watch-test tweaks to queue`; op attributed.
- new: notice `@ moved to rkpyvyylkpnl · op: new empty commit · change rkpyvyylkpnl entered range`;
  footer `@` indicator updated; file list/range (`main..@`) stayed correct; viewed state + comment intact.
- edit *viewed* `src/config.rs` in the new `@`: badge flipped `✓ → ±`, viewed count `1/7 → 0/7`,
  notice `src/config.rs updated (+2 −0) — was viewed, needs re-review · 1 viewed file changed — needs re-review`.
  Coherent: the file was never simultaneously "viewed" and "needs attention".

**Mutation 5 — `jj undo` then `jj abandon okrklwup`:**
- undo (of the snapshot op): config.rs returned to `✓`, viewed count restored to 1/7, stats
  restored; op attributed as `undo: restore to operation 2cbe4214…` but the raw 128-char op id
  overflowed the footer (`… · ctrl-a for detail`) and was *still* truncated inside the feed.
- abandon: notice `change okrklwupknox left range · op: abandon commit 008dd7c7…`; stats
  reverted to +134/-12; queue.rs `±` cleared (feed showed `src/queue.rs reverted to previously
  seen content`). Stack membership changes are first-class events.

**Operation catch-up (`I`):** modal lists the jj op log with ages and descriptions, fits the
50-row pane, and shows a live effect preview at the bottom that updates per selection:
default `will mark 6 caught up · 1 already viewed · 0 need re-review`; after moving to the
pre-undo `new empty commit` op: `will mark 5 caught up · 1 already viewed · 1 need re-review`.
Applied → 5 files got `◌` (caught up), config.rs stayed `✓`, queue.rs stayed `•` needing
re-review; footer `6/7 viewed`; notice
`5 file(s) caught up (unchanged since 2cbe4214f2cc), 1 already viewed; 1 changed/new file(s) need re-review`.
Counts and badges fully consistent.

**Op log audit** (after 20s idle with the pane polling): 4 `snapshot working copy` ops at the
mutation cluster = exactly my 4 content-mutation groups (tweak 1, coalesced tweaks 2–5, worker
edit, worker revert); one more for the config.rs edit; **zero snapshots during idle**. Every op
is attributable to a real content change or an explicit jj command.

**Exit:** `q` wrote the artifact; `gander comments list` afterwards confirmed the comment
persisted durably with hunk/line fingerprints anchored to `src/lib.rs:8`.

# Findings

1. **minor — Activity feed ignores the app's own movement keys.** Help says `n/e move` for
   files and diff, and the `I` picker even advertises `↑/↓ or n/e move`, but in the ctrl-a
   feed pressing `n` repeatedly moves nothing; only arrow keys work. Repro: open ctrl-a,
   press `n` ×7, selection marker `›` stays on the first row.
2. **minor — "ctrl-a for detail" doesn't deliver detail.** Footer truncates long op
   descriptions (`op: undo: restore to operation 2cbe4214…· ctrl-a for detail`), but the feed
   truncates the same rows with `…` and offers no expand/wrap for the selected event. The full
   undo target op id is unreadable anywhere in the TUI. Repro: run `jj undo`, read the notice,
   open ctrl-a, select the undo row.
3. **minor — Raw op ids leak into human-facing notices.** `undo: restore to operation
   <128-hex-chars>` is jj's raw description; the watch pane should elide to a short op id +
   age like the `I` picker already does.
4. **papercut — Feed emits ~3 near-duplicate rows per refresh** (aggregate "repository
   changed — …" line + per-file line + per-change line, all same timestamp). Scanning a busy
   agent session means reading everything twice. Repro: any single file edit, then ctrl-a.
5. **papercut — Enter on non-file feed events silently closes the feed.** Change-level
   (`change … left range`) and op-level rows give no navigation and no "nothing to jump to"
   feedback. Repro: ctrl-a, select `change okrklwupknox left range`, Enter.
6. **papercut — Revision identity is footer-only and small.** `following @ · @ rkpyvyyl
   (no description)` is accurate and live, but it's the last line of the pane; panel titles
   carry nothing. For an ambient pane glanced from across the screen, the mode/identity cue
   deserves more visual weight (e.g., in the `diff`/`files` titles or a header strip), and
   `(no description)` plus no short commit hash is weak identity for empty changes.
7. **papercut — Badge legend advertises `~ done, changed since` but the observed behavior for
   a viewed-then-changed file was `±` plus automatic un-viewing.** I never saw `~` in the whole
   session; either the legend is stale or the state is rare enough that the legend confuses.
   Repro: mark file viewed, edit it, observe `±` and viewed-count decrement rather than `~`.
8. **papercut — Aggregate notice duplicates op attribution within one line**, e.g.
   `…op: abandon commit 008dd7… · change okrklwupknox left range · op: abandon commit 008dd7…`.
   Compose once per op, then list effects.

# Scores

- **Watch freshness/follows-@: 5/5.** Every mutation refreshed within one poll cycle;
  `jj describe`, `new`, `undo`, and `abandon` were all followed correctly with a persistent
  `following @ · @ <change>` indicator that updated in place; content reverts were recognized
  rather than left stale; selection, viewed state, and comments survived everything, and undo
  even *restored* previously invalidated viewed state. I could not make it go stale or follow
  the wrong revision.
- **Watch change awareness: 4.5/5.** Per-file deltas with +N/−M, explicit revert detection,
  viewed-invalidation callouts, stack membership events (`entered range`/`left range`), causal
  jj op attribution on every event, and a catch-up flow with a live, accurate effect preview.
  Docked half a point for the redundant triple-row feed, truncated-with-no-detail long ops,
  no per-hunk "what's new inside this file" granularity, and non-navigable non-file events.
- **Pane-worthiness: 4.5/5.** I would trust this open all day next to an agent: it is
  accurate, calm (notices, not redraws-from-scratch), never needed a restart, doesn't pollute
  the op log when idle, and the viewed/caught-up bookkeeping always matched the badges. The
  half-point gap: revision identity and "what needs fresh review" signals are concentrated in
  one small footer line, and the feed's noise means catching up after an hour away is more
  reading than it should be.

# Top proposals

1. **Per-op digest rows in the activity feed.** Collapse the aggregate/per-file/per-change
   triplet into one expandable row per jj op ("snapshot · src/queue.rs +4 −0", expandable to
   per-file children). Makes catching up after long agent runs scale with ops, not rows×3.
2. **Make the selected feed event expandable (fulfill "ctrl-a for detail").** Wrap or pop the
   full event text — especially undo/restore targets — and shorten op ids to 12 chars + age.
3. **Promote revision identity into the pane chrome.** Put `main..@ · @ rkpyvyyl (desc|hash)`
   in the diff pane title (or a one-line header), so a cross-room glance confirms target
   without reading the footer; bold/flash it briefly when `@` moves.
4. **Unify movement keys** — `n`/`e` should move selection in the activity feed exactly as
   they do in files, diff, and the `I` picker.
5. **Hunk-level freshness within a re-review file.** After catch-up or a viewed-file change,
   mark *which hunks* are new since my last look (the `±` file badge already proves the diff
   engine knows), so re-review of a large file doesn't mean rereading all of it.
6. **Navigation affordance for change-level events** — Enter on `change … entered/left range`
   could jump to that change's file subset (or show a toast "no longer in range") instead of
   silently closing.
7. **Reconcile the badge legend** — either surface `~ done, changed since` in the default flow
   or remove it from the legend; today the observed state machine (`✓ → ± + un-viewed`) doesn't
   match the documented one.
