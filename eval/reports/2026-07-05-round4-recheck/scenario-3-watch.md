# Summary

Scenario 3 (ambient watch pane) against `gander -b main -r @ tui` in fixture
`/tmp/gander-eval/round4-s3`, tmux session `gander-r4-s3`, 200x50.

Verdict: the watch loop is now genuinely trustworthy. Every mutation —
working-copy edit, rapid successive edits, `jj describe`, `jj new`, `jj undo`,
`jj abandon` — was reflected within one ~2s poll, with selection, the inline
comment annotation, and file-tree ordering preserved across all refreshes. The
round-4 claims all verified:

- **No op-log pollution from polling.** After ~6 minutes of continuous polling
  the fixture op log contained exactly 3 `jj util snapshot` ops, one per real
  content change (idle periods produced zero ops).
- **Viewed-file change callout works.** Editing a viewed file produced
  `src/config.rs updated (+3 −0) — was viewed, needs re-review · 1 viewed file
  changed — needs re-review` in both the footer notice and the ctrl-a feed.
- **Inline comment annotations survive refreshes** — the `[draft]` annotation
  on `src/lib.rs:4` was visible after every mutation, `@` move, undo, and
  abandon.
- **File-tree rows never re-sorted** as badges flipped `•`→`✓`→`±`→`◌`.
- **`I` catch-up distinguishes `◌` caught up from `✓` viewed** in the tree and
  the result notice.

The most important remaining gaps: the `I` prior-operations picker silently
drops the app-standard `n`/`e` movement keys (arrow keys work), which caused a
wrong-baseline catch-up that mass-marked all 7 files caught up with no preview
or undo; and change notices describe *effects* generically ("change updated",
"file updated") without naming the jj operation or distinguishing reverts, so
`describe` and `undo` events are under-explained.

# Step log

Setup: fixture is a 4-change stack over `main` (queue library: priority
scheduling, retry policy, tests) with an empty `@`. Baseline op-log head
`93112bc9a1a2`. Launched via
`nix shell nixpkgs#tmux --command tmux new-session -d -s gander-r4-s3 -x 200 -y 50 ...`.

1. **Launch + seed.** Footer: `7 files (0/7 viewed …) · following @`. Help
   (`?`) documents the badge legend
   `✓ viewed · ◌ caught up · ~ done, changed since · ± changed since look`.
   Marked `src/config.rs` viewed (Enter → `✓`, advanced to `lib.rs`). Added a
   line comment on `src/lib.rs:4` (`c`, type, `ctrl-s`); while editing the
   footer said `refresh paused` — good. Comment rendered inline:
   `↳ a547ccf1 [draft] Consider re-exporting RetryPolicy here too…`.
   Op-log check after ~2 min of polling: head still `93112bc9a1a2` — polling
   writes no ops.

2. **Edit a viewed file.** Appended 3 lines to `src/config.rs`. ~5s later:
   badge `✓`→`±`, viewed count 1/7→0/7, stats +134→+137, footer notice:
   `info: repository changed — change tvoputrtllvv updated · src/config.rs
   updated (+3 −0) — was viewed, needs re-review · 1 viewed file changed —
   needs re-review`. Selection stayed on `lib.rs`; comment intact. Op log:
   exactly one new `jj util snapshot` op.

3. **Rapid successive edits** (3 appends to `src/queue.rs` ~1s apart, ctrl-a
   feed open). Two poll windows caught them: feed entries
   `19:44:52 src/queue.rs updated (+3 −0)` and `19:44:54 src/queue.rs updated
   (+1 −0)` — accurate, no lost edits, no flicker. Feed refreshed live while
   open. Op log: two snapshot ops (one per detected change), not one per poll.

4. **`jj describe`** on `@`: notice was only
   `repository changed — change tvoputrtllvv updated` — accurate but doesn't
   say *what* about the change was updated (description vs content).

5. **`jj new`**: notice
   `@ moved to rsrrqylvlqxn · change rsrrqylvlqxn entered range` — explicit
   `@`-follow with change identity. Then edited `src/worker.rs`: `±` badge,
   `src/worker.rs updated (+2 −0)`, stats +143.

6. **`I` catch-up.** Picker lists prior ops with ages/descriptions. Pressing
   `n` ×6 did **not** move the `›` selector (verified separately: `n` and `e`
   are dead in this picker; arrow keys work). My Enter therefore compared
   against the *newest* op and marked all 7 files caught up (7/7 viewed) with
   no preview or confirmation. Redid it with arrows against pre-mutation op
   `93112bc9a1a2`: `4 file(s) caught up (unchanged since 93112bc9a1a2), 0
   already viewed; 3 changed/new file(s) need re-review` — `◌` on unchanged
   files, `±` retained on config/queue/worker. Correct and legible once the
   right op is selected.

7. **`jj undo`** (undid the worker.rs snapshot; jj reverted the file on
   disk): pane updated within one poll, stats +143→+141, notice
   `change rsrrqylvlqxn updated · src/worker.rs updated` — no diffstat and no
   indication this was a revert.

8. **`jj abandon tvoputrt`** (removed the eval-marker change; descendant
   rebased): notice `change rsrrqylvlqxn updated · change tvoputrtllvv left
   range · src/config.rs updated · src/queue.rs updated`; stats back to
   +134/-12. Standout behavior: `config.rs` content reverted to exactly what I
   had marked viewed, and gander silently restored it to the viewed tally
   (5/7; unviewed filter `f` showed only queue.rs and worker.rs) — though the
   tree badge stayed `±` and nothing announced the restoration.

9. Quit with `q` (artifact written on exit), killed/confirmed tmux session
   `gander-r4-s3` gone.

# Findings

1. **`I` prior-operations picker ignores `n`/`e` movement keys — silent
   wrong-baseline catch-up.** — **major.** The rest of the app moves with
   `n`/`e`; in this picker those keys are silently dropped (only arrows work),
   there's no key hint in the modal, and Enter immediately applies a
   mass-state change (all 7 files marked caught up against the default newest
   op) with no preview, confirmation, or single-key undo. In an ambient
   workflow this quietly destroys the "what needs fresh review" signal.
   *Repro:* press `I`, press `n` several times — `›` stays on the first row;
   press Enter — every unchanged-since-latest-op file flips to `◌`.

2. **Notices/feed describe effects generically, not jj operations or
   reverts.** — **minor.** `jj describe` produced only
   `change tvoputrtllvv updated` (no hint the *description* changed);
   `jj undo` of a snapshot produced `src/worker.rs updated` with no diffstat
   and no revert indication, indistinguishable from a forward edit. The feed
   never names operations (`describe`, `undo`, `abandon`), which the op log
   knows. *Repro:* run `jj describe -m …` then `jj undo` beside the pane and
   compare notices with `jj op log`.

3. **`±` badge vs viewed-count ambiguity; silent viewed-state restoration.**
   — **minor.** After the abandon reverted `config.rs` to
   previously-viewed content, the file counted as viewed again (5/7, absent
   from the unviewed filter) yet still displayed `±`, and nothing announced
   the restore. You cannot tell from the tree which `±` files count toward
   the viewed tally. *Repro:* view a file, edit it, abandon the change that
   contained the edit, compare tree badges with the `f` unviewed filter.

4. **Activity-feed lines truncate without wrap.** — **papercut.** The
   19:44:24 entry lost its tail (`… 1 viewed file c│`) even on a 200-col
   terminal because the overlay is narrower and doesn't wrap or allow
   horizontal scroll. *Repro:* trigger a viewed-file change, open ctrl-a.

5. **Stale copy in the catch-up picker header.** — **papercut.** Header says
   "unchanged files are marked viewed" but they're actually marked `◌` caught
   up, which the help legend explicitly distinguishes from `✓` viewed.
   *Repro:* press `I` and compare header text with resulting badges.

6. **Feed entries are not actionable.** — **papercut.** Feed items name files
   ("src/queue.rs updated (+1 −0)") but Enter doesn't jump to that file/hunk;
   you must close the feed and navigate manually. *Repro:* open ctrl-a,
   select a file entry, press Enter.

# Scores

- **Watch freshness/follows-@: 5/5.** Every mutation reflected within one
  ~2s poll; `@` movement announced with change identity (`@ moved to
  rsrrqylvlqxn`), range entry/exit tracked through `jj new`, `undo`, and
  `abandon`; selection, comments, and tree order stable throughout; footer
  advertises `following @`. No stale or wrong-revision state observed at any
  point, and polling leaves the op log untouched.

- **Watch change awareness: 4/5.** Footer notices and the ctrl-a feed give
  timestamped per-file deltas with diffstats, viewed-file re-review callouts,
  and explicit `entered range`/`left range` events — well above the "aggregate
  stats only" anchor. Held back from 5 because operations aren't named
  (describe/undo look like generic "updated"), reverts aren't distinguished,
  the undo event lacked a diffstat, feed lines truncate, and feed entries
  offer no navigation to the fresh work.

- **Pane-worthiness: 4/5.** I'd keep this open all day beside an agent: it's
  accurate, calm (no flicker, no op-log pollution, refresh pauses during
  comment editing), and the viewed/±/◌ ledger plus "needs re-review" callouts
  answer "what do I still need to look at" honestly — including the genuinely
  impressive content-based viewed-state restore after abandon. Not a 5
  because the `I` picker can silently wreck the needs-review signal (finding
  1) and the `±`/viewed-count ambiguity means the tree alone can't always be
  trusted for "what's pending" without cross-checking the filter.

# Top proposals

1. **Harden the catch-up picker.** Support `n`/`e` (and show key hints in the
   modal), preview what the selected baseline will mark (`4 caught up, 3 need
   re-review`) *before* Enter applies it, and offer one-key undo of a
   catch-up. This is the one interaction that can silently destroy review
   state.
2. **Name jj operations in notices and the feed.** Gander already snapshots
   deliberately; tag each refresh with the triggering op kind (`edit`,
   `describe`, `undo`, `abandon`) and mark reverts distinctly (e.g.
   `src/worker.rs reverted (−2)`), with diffstats on every file event.
3. **Make the activity feed a navigation surface.** Enter on a file entry
   jumps to that file's first changed hunk; wrap long lines. The feed already
   has the right content to be the "what happened while I was away" home
   base.
4. **Disambiguate `±`.** Either split into "viewed but changed since look" vs
   "unviewed and changed since look", or clear `±` when viewed state is
   restored/re-confirmed, and announce content-based viewed restoration
   ("src/config.rs back to viewed content") so the tally changes aren't
   silent.
