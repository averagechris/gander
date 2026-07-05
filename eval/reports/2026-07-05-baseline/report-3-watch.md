# Summary

Gander is already useful as a passive diff pane: it did poll the jj repo, refresh after working-copy snapshots, keep the selected file stable, and preserve existing review state while an agent edited and reshaped the stack. It is not yet a first-class ambient/watch experience because refreshes are mostly silent context replacement: the UI says the repository changed, but does not identify the files/hunks/revisions that changed since I last looked, and there is no durable unread/new marker or event feed.

In this run, `gander -b main -r @ tui` correctly continued comparing `main..@` after `jj new` moved `@`, so it was not stranded on the old commit id. However, that correctness was subtle: the footer stayed `main..@`, the file list did not reorder or badge the new work, and a user glancing over could easily miss that the reviewed range now included a new change.

# Step-by-step observation log

## 1. Initial screen and seeded review state

Started detached tmux session `gander-watch-eval` at about 200x50 with:

```sh
/Users/chris/projects/gander/target/release/gander -b main -r @ tui
```

Initial excerpt:

```text
┌files──────────────────────────┐┌diff──────────────────────────┐
│• ▾ src 0/6                    ││src/config.rs  +11 -0         │
│  • mod     config.rs          ││tree-sitter: rust ...         │
│  • mod     lib.rs             ││@@ -6,6 +6,17 @@              │
│  • added   priority.rs        ││  9 + impl Config {           │
│  • mod     queue.rs           ││ 10 + /// Configuration ...   │
│  • added   retry.rs           ││ ...                          │
│  • mod     worker.rs          ││                              │
│• ▾ tests 0/1                  ││                              │
│  • mod     basic.rs           ││                              │
└───────────────────────────────┘└──────────────────────────────┘
7 files (0/7 viewed, 0 generated/noisy), +134/-12, 0 comments
main..@ · focus files · ... · c comment · ? help · q quit
```

I used `?` to discover keys, then marked one file viewed with `enter`, moved to another file, and added/saved one comment with `c`, text, `ctrl-s`.

Seeded-state excerpt:

```text
│◐ ▾ src 1/6
│  • mod     config.rs
│  • added   priority.rs
│  • mod     queue.rs
│  • added   retry.rs
│  • mod     worker.rs
│  ✓ mod     lib.rs
...
7 files (1/7 viewed, 0 generated/noisy), +134/-12, 1 comments
```

## 2a. Edit an existing tracked file in the working copy

Mutation: appended `ambient_over_limit` to `src/queue.rs`.

After ~5 seconds, gander refreshed automatically. The footer stats changed from `+134/-12` to `+140/-12` and a notice appeared:

```text
7 files (1/7 viewed, 0 generated/noisy), +140/-12, 1 comments
info: repository changed — refreshed main..@
```

The selected file remained `priority.rs`, so the pane did not immediately show what changed. After moving to `queue.rs`, its header reflected the new total for that file:

```text
│  • mod     queue.rs                      ││src/queue.rs  +31 -5
...
7 files (1/7 viewed, 0 generated/noisy), +140/-12, 1 comments
info: repository changed — refreshed main..@
```

Assessment: freshness worked; change awareness was weak. The user gets “something changed” plus aggregate stats, but no “queue.rs changed since last refresh” marker and no jump target for the new hunk.

## 2b. Edit the same file repeatedly in quick succession

Mutation: appended three `agent_marker_N` functions to `src/queue.rs` with short sleeps between writes.

After settling, gander showed stable state rather than flickering through every intermediate edit:

```text
│  • mod     queue.rs                      ││src/queue.rs  +40 -5
...
7 files (1/7 viewed, 0 generated/noisy), +149/-12, 1 comments
info: repository changed — refreshed main..@
```

Assessment: the pane was not noisy in the sense of spamming visible events. That is pleasant, but it also collapses meaningful iteration history into a generic refresh notice. If an agent rewrites a hunk several times, I cannot tell what changed since my last look without re-reading the file.

## 2c. `jj describe`, `jj new`, edit a different file

First attempt hit local jj signing config; reran with `--config signing.behavior=drop`.

Commands:

```sh
jj --config 'user.name="Eval"' --config 'user.email="eval@example.com"' --config signing.behavior=drop describe -m 'feat: quota enforcement'
jj --config 'user.name="Eval"' --config 'user.email="eval@example.com"' --config signing.behavior=drop new
# append Config::quota_enabled to src/config.rs
```

`jj log` then showed `@` had moved to a new empty/newly edited change above the described quota-enforcement change:

```text
@  nzzpsppk ... (no description set)
○  ylnszrtp ... feat: quota enforcement
○  tlxyuxwx ... test: cover priority ordering and retry drops
```

Gander after polling:

```text
7 files (1/7 viewed, 0 generated/noisy), +156/-12, 1 comments
info: repository changed — refreshed main..@
```

The range still displayed `main..@`, and the aggregate stat included the new `src/config.rs` edit. Therefore gander did follow the moving revset expression `@` rather than pinning to the old change id. But the UI did not explain that `@` moved, that a new change entered the stack, or that `config.rs` had new unread work. Since `config.rs` was already modified in the original range, the file list looked almost identical.

## 2d. Review-state survival

The viewed marker and comment count survived all working-copy edits, quick iterations, `jj describe`, `jj new`, and the new `config.rs` edit:

```text
│  ✓ mod     lib.rs
...
7 files (1/7 viewed, 0 generated/noisy), +156/-12, 1 comments
```

This was a strong result. Review state appeared bound to the session/range/files rather than being wiped by repo refresh.

## 2e. `jj undo` and abandon newest change

Ran `jj undo`, which removed the latest config-file working-copy snapshot and returned the new `@` to empty. Gander refreshed back to previous aggregate stats:

```text
7 files (1/7 viewed, 0 generated/noisy), +149/-12, 1 comments
info: repository changed — refreshed main..@
```

Then abandoned the empty newest change. jj created a fresh empty working copy on the same parent:

```text
@  tqslvrzm ... (empty) (no description set)
○  ylnszrtp ... feat: quota enforcement
```

Gander again refreshed cleanly and still showed:

```text
7 files (1/7 viewed, 0 generated/noisy), +149/-12, 1 comments
info: repository changed — refreshed main..@
```

No errors or stale-diff symptoms observed. Again, the behavior was accurate but ambiently under-explained: no “change removed” or “@ moved from nzzpsppk to tqslvrzm” notice.

# What refreshed correctly vs incorrectly vs silently

## Refreshed correctly

- Working-copy file edits were picked up within the polling window.
- Aggregate stats updated accurately: `+134/-12` → `+140/-12` → `+149/-12` → `+156/-12` → `+149/-12`.
- The selected file and tree expansion were stable across refreshes.
- The symbolic revset `@` was re-evaluated after `jj new`; `main..@` included the new top change.
- `jj undo` and abandoning the newest change did not break the TUI; it refreshed back to the right aggregate range.

## Refreshed incorrectly

- I did not observe an outright incorrect diff or stale aggregate after waiting for polling.
- No review-state loss observed.

## Refreshed silently / under-signaled

- The notice only said `repository changed — refreshed main..@`; it did not say which file changed.
- A changed file that was not currently selected did not receive a visible “new since last look” badge.
- Existing modified files (`config.rs`, `queue.rs`) looked the same in the tree before and after additional edits; only the file-level stat changed when selected.
- Movement of `@` after `jj new` was not called out as an event.
- Undo/removal of work was not called out beyond another generic refresh notice and changed aggregate stats.

# Review-state preservation results

- Viewed-state: preserved. `lib.rs` remained checked (`✓ mod lib.rs`) throughout.
- Comment: preserved. Footer remained `1 comments` throughout.
- Selection/layout: mostly preserved. The selected file remained stable across refreshes, which reduced disorientation.
- Caveat: preservation is good, but in a watch mode a previously viewed file that receives new edits probably needs to become “viewed but changed since viewed” rather than staying conceptually done forever.

# Findings

## Major: no per-file or per-hunk “changed since last look” markers

Repro:
1. Start `gander -b main -r @ tui`.
2. Mark any file viewed and leave selection on a different file.
3. Append code to `src/queue.rs`.
4. Wait for polling.

Observed: footer says `info: repository changed — refreshed main..@` and aggregate stats update, but `queue.rs` has no special badge indicating it changed since the last refresh or since it was last viewed.

Impact: in an ambient pane, this is the key missing trust affordance. Users need to know where to look without manually comparing stats or rereading every modified file.

## Major: `@` movement is accurate but not legible

Repro:
1. With TUI running as `main..@`, run `jj describe -m 'feat: quota enforcement'` then `jj new`.
2. Edit `src/config.rs` in the new `@`.
3. Wait for polling.

Observed: gander follows the symbolic `@` and includes the new work, but only shows the same generic refresh notice and `main..@` footer.

Impact: the user is not stranded technically, but they may not realize a new change entered the reviewed range. For a stacked jj workflow, “@ moved / new change started” is semantically important.

## Minor: quick agent iterations are stable but lose activity context

Repro:
1. Append to the same file several times within a few seconds.
2. Wait for polling.

Observed: the pane settles cleanly on final state with one generic refresh notice. No flicker/noise, which is good; no timeline/count of collapsed refreshes, which is limiting.

Impact: acceptable for calmness, but insufficient for understanding an agent’s active loop. A compact activity feed or “3 updates to queue.rs” summary would be much better.

## Minor: undo/removal events are not distinguished from additions

Repro:
1. Add/edit a new top change.
2. Run `jj undo` or abandon the top empty change.
3. Wait for polling.

Observed: aggregate stats go down and the generic refresh notice appears.

Impact: accurate but easy to miss. Deletions/reverts deserve explicit negative/change-removed messaging in watch mode.

## Papercut: comment save key is discoverable only after entering the editor

Repro:
1. Press `?`; see `c comment at cursor`.
2. Press `c` and type.

Observed: the modal footer then says `ctrl-s save`; this is fine, but for a dogfood/eval flow I initially had to infer the save action after opening the editor.

Impact: small. Help could mention “c comment, ctrl-s save” or the comment modal could have stronger visual affordance.

# Scores

- Freshness/accuracy: **4/5**. Polling worked, stats and diffs refreshed, `@` was re-evaluated, undo/abandon recovered correctly. I am withholding one point because there is no visible revision identity/operation summary to independently verify freshness at a glance.
- Change awareness: **2/5**. The UI tells me “repository changed” and updates aggregate stats, but not what changed since I last looked. This is the biggest gap for ambient use.
- Follow-the-work / tracking `@`: **4/5**. Functionally, yes: `main..@` followed the moving working copy after `jj new`. Legibility is the missing point; the UI should announce `@` movement and new stack entries.
- Pane-worthiness overall: **3/5**. Useful as a live diff/status pane for an attentive user, not yet trustworthy as an all-day ambient review pane next to an autonomous coding agent. I would keep it open, but I would not rely on it alone to know what needs fresh review.

# Top 3 concrete proposals for a first-class watch/follow mode

1. **Add “changed since last viewed/refresh” badges and navigation.** Track a refresh baseline per file/hunk. Show badges like `NEW`, `CHANGED`, `REVIEWED+CHANGED`, or a colored dot distinct from normal modified status. Add keys for next/previous newly changed file/hunk, separate from next unviewed file.

2. **Add an ambient activity feed / refresh summary.** A small collapsible pane or footer cycle could show events such as `14:31 queue.rs +9`, `14:33 @ moved ylnszrtp → nzzpsppk`, `14:35 config.rs +7`, `14:38 undo removed config.rs +7`. Collapse rapid edits into one calm event (`queue.rs updated 3 times, +9 lines`).

3. **Make symbolic follow mode explicit.** When launched with `-r @`, show something like `following @: tqslvrzm (parent ylnszrtp) · main..@`. On `jj new`, display a durable notice: `@ moved: quota enforcement → new empty change; range now includes 4 changes`. Consider a toggle between “follow symbolic revset” and “pin current commit id” so users understand and control whether they are watching the agent’s current work or a fixed review target.
