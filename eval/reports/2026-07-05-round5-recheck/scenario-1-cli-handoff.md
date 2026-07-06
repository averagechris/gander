# Summary

Round 5 recheck of the CLI review → agent handoff loop (`gander 0.5.0`, fixture
`/tmp/gander-eval/round5-s1`, target `main..@`: priority queue + retry
extraction + tests).

The core loop is now genuinely usable end-to-end from the CLI with no panics,
no source-diving, and no stderr noise. All of the round-4 fixes verified:
`--state-file` works (and `--state` at the top level gives a helpful "similar
argument exists" tip), `comments set-state --state todo` works instead of
panicking, bad ids on every mutation verb I tried print clean one-line
`error: unknown …` messages with exit 1, `follow-up` round-trips through
comments/tasks/handoff in both formats, `comments edit`/`comments delete`
exist and echo the affected object, and handoff markdown is now a distinct
artifact from `export markdown --profile agent` (different preamble, trimmed
vs full hunks, each cross-referencing the other).

The most important remaining gaps are data-integrity ones, not ergonomics
ones: task→comment links are stored as unvalidated raw strings (a dangling
`--comment deadbeef` link persists silently, and real links are stored as the
prefix the user typed rather than the resolved id, so JSON consumers cannot
equality-join `linked_comment_ids` to comment ids); a comment anchored to a
line that is not in the diff (`--line 999`) silently persists with no anchor
and no warning; and the default markdown handoff filters to open/unresolved
items while the default JSON handoff still includes `done` tasks, so the two
formats disagree unless the caller knows to add `--only-open` to the JSON
call.

# Step log

All commands run from `/tmp/gander-eval/round5-s1` with
`G=/Users/chris/projects/gander/target/release/gander`.

1. **Session create** — `$G -b main -r @ reviews create --title "Priority
   scheduling + retry extraction review"` → JSON session object with id,
   target revset `main..@`, status `open`. Clean. `reviews create --help` has
   real descriptions for every flag.
2. **Files** — `$G -b main -r @ files list` → 7 files with additions/
   deletions, per-file `hunk_count`, `fingerprint`, `status`, `viewed`,
   `generated`. Complete and easy to consume.
3. **Hunks + pipe composition** — `$G -b main -r @ hunks list | head -3`
   exits `0 0` with **zero bytes on stderr**: no broken-pipe panic, no noise.
   Friction: `hunks list src/queue.rs` fails (`error: unexpected argument`)
   even though the subcommand help says "Optionally narrow to one file"; the
   actual flag is `--file`, whose own help line is **blank**.
4. **Hunk display** — `hunks show "src/queue.rs:1" --format diff` (and
   `src/retry.rs:0`, `src/worker.rs:0`, `src/worker.rs:1`, `src/config.rs:0`,
   `tests/basic.rs:1`) all render clean unified diffs. Bad ids
   (`src/queue.rs:9`, `nope.rs:0`) → one-line `error: unknown hunk id`,
   exit 1.
5. **Comments** — added three anchored comments with meaningful kind/action:
   - `issue`/`fix` at `src/retry.rs:20` (off-by-one in `should_retry`),
   - `issue`/`fix` range `src/worker.rs:31-34` (re-enqueue allocates a new
     job id so the attempts map never accumulates → infinite retry),
   - `question`/`follow-up` at `src/queue.rs:46` (empty bands never removed).
   The returned anchor JSON includes `line_text` and fingerprints, which let
   me verify my line math instantly — genuinely good.
   `comments add --path src/nonexistent.rs` → clean error. **But**
   `comments add --path src/retry.rs --line 999 --body test` silently
   succeeded with no `anchor` object at all (see finding 3).
6. **Lifecycle** — `comments set-state <prefix> --state todo` worked on two
   comments (no panic; previously broken). `comments edit ca3b7e8e --body …`
   worked and preserved state. `comments delete` on a throwaway comment
   echoed the deleted object; deleting again → `error: unknown comment`,
   exit 1.
7. **Tasks** — added three tasks (`--action fix/test`), two linked to
   comments via `--comment <prefix>` and anchored with `--path/--line`.
   `tasks complete deadbeef` → clean one-line error. **But**
   `tasks add --title x --comment deadbeef` succeeded and persisted the
   dangling link (see finding 1). There is no `tasks delete` or `tasks edit`;
   the junk task could only be `tasks complete`d, and it then still appeared
   in default JSON handoff output (see findings 2 and 5).
8. **Walkthrough** — two ordered steps with `--path/--line/--end-line/--why`.
   `walkthrough show` and `walkthrough export` (Markdown with locations and
   Why lines) both correct.
9. **Agent role-switch** —
   - `export markdown --profile agent` → 355-line artifact: action items with
     inline diff excerpts, walkthrough, full reference hunks.
   - `export json --profile agent` → full structured artifact (files with
     hunks, comments with anchors+excerpts, tasks, walkthroughs, summary).
   - `tasks list` → session tasks plus comment-backed todos; comment-backed
     entries have `title: null` (my first consumer script crashed on it).
   - `comments list`, `walkthrough export` → correct.
   - `handoff --format json` → `{session, action_items, walkthrough,
     reference}`; action items carry id, source, kind/action, path/line,
     excerpt (with `new_line` numbers), body, state, linked ids. Excellent
     shape, two caveats below.
   - `handoff` (markdown) → prompt-ready; **6** action items ("3 open
     task(s), 3 unresolved comment(s)") while default JSON has **7**
     (includes the `done` task). `handoff --format json --only-open` → 6,
     matching markdown.
10. **Flag rename check** — `--state-file /tmp/...` accepted (fresh state ⇒
    empty comments list); top-level `--state` → clap tip pointing at
    `--state-file`.

# Findings

1. **major — task→comment links are unvalidated raw strings.**
   `source_comment_id` stores exactly what the user typed (`"7b2376b6"`, a
   prefix) instead of the resolved full comment id, and a nonexistent
   reference is accepted silently.
   Repro: `$G -b main -r @ tasks add --title x --comment deadbeef` → exit 0,
   task persisted with `"source_comment_id": "deadbeef"`. Real links surface
   in `handoff --format json` as `"linked_comment_ids": ["7b2376b6"]`, which
   does not equality-match any comment `id`
   (`7b2376b6-07ff-4eb3-b2c7-bc5f715a1c8b`), so an agent must implement
   prefix-matching to join tasks to comments and gets no signal when a link
   is dangling.

2. **major — default markdown handoff and default JSON handoff disagree.**
   Markdown filters to open tasks/unresolved comments ("Action items: 6
   item(s)"); JSON includes `done` tasks (7 `action_items`). They only match
   when the JSON caller adds `--only-open`.
   Repro: complete any task (`tasks complete <id>`), then compare
   `$G -b main -r @ handoff | grep 'Action items:'` (6) against
   `$G -b main -r @ handoff --format json | python3 -c "import json,sys; print(len(json.load(sys.stdin)['action_items']))"` (7).
   An agent consuming default JSON re-does finished work or must know an
   undocumented default asymmetry.

3. **major — comments silently persist without an anchor when the line is
   not in the diff.** No warning, no error; the stored comment simply lacks
   the `anchor` object and will have no excerpt in handoff. A typo'd line
   number produces a permanently degraded comment.
   Repro: `$G -b main -r @ comments add --path src/retry.rs --line 999 --body
   test` → exit 0, JSON output has no `anchor` key (contrast with any valid
   `--line`). `comments list` confirms the anchorless comment persisted.

4. **minor — `hunks list` file narrowing is undiscoverable.** Help summary
   says "Optionally narrow to one file", but the natural positional form is
   rejected, and the actual `--file` flag has an empty help description.
   Repro: `$G -b main -r @ hunks list src/queue.rs` → `error: unexpected
   argument`; `$G hunks list --help` shows `--file <FILE>` with no text.

5. **minor — tasks are append-only: no `delete` or `edit`.** A mistyped task
   can only be completed, and completed tasks still show in `tasks list` and
   default JSON handoff. Comments got `edit`/`delete` this round; tasks
   didn't.
   Repro: `$G tasks delete <id>` → `error: unrecognized subcommand 'delete'`.

6. **minor — comment-backed items in `tasks list` have `title: null`.**
   Consumers must know to fall back to `body`; my first parsing script
   crashed. Synthesizing a title from the comment body (or documenting the
   contract) would remove the trap.
   Repro: set a comment to todo, then
   `$G -b main -r @ tasks list | python3 -c "import json,sys; [t['title'][:10] for t in json.load(sys.stdin)['tasks']]"` → `TypeError: 'NoneType' object is not subscriptable`.

7. **papercut — help output interleaves domain and global flags.** In
   `comments add --help` the order is path, repo, line, rev, end-line, body,
   ignore, generated-preset, kind, action, generated-glob, … Domain flags
   (`--kind`, `--action`) are buried in the middle of repeated global
   plumbing. Every subcommand repeats the same 8 global flags, ~30 lines of
   noise around 4-6 relevant options.

8. **papercut — `export markdown --profile agent` and `handoff` share the
   same H1** (`# Human review handoff for a coding agent`), and the export
   preamble claims "includes all comments/tasks" while the `done` task
   appears nowhere in the markdown body (it is present in the JSON export).
   Repro: `head -5` both artifacts; `rg '\[task\]' export.md` shows only the
   3 open tasks.

9. **papercut — a few args still have blank help strings:** `hunks show <ID>`
   argument, `export -o/--output`, `handoff -o/--output`, `hunks list
   --file`.

# Scores

- **Discoverability: 4/5.** Every subcommand and flag I checked now has real
  help text (a round-4 fix, verified), top-level `--help` maps the whole
  workflow, and clap tips rescue mistyped flags/subcommands (`--state` →
  `--state-file`, `tasks delete` → `complete`). I completed the entire
  scenario from help output alone with zero source-diving. Held back from 5
  by the `hunks list` positional trap with a blank `--file` description, the
  interleaved global/domain flag ordering, and a handful of empty help
  strings.
- **Output quality for humans: 3/5.** `summary` and `hunks show --format
  diff` are good, and mutation echoes (anchor `line_text`) let a human verify
  targeting immediately. But everything else is JSON-only — a human running
  `files list`, `comments list`, or `tasks list` gets pretty-printed JSON
  with no compact/table format, and range-comment echoes dump ~70 lines of
  anchor detail per mutation. Accurate and usable, but routine review means
  manual context assembly — squarely the rubric's 3.
- **Output quality for agents: 4/5.** Anchors carry `line_text`, per-line and
  per-diff fingerprints, hunk headers, and both old/new line numbers; handoff
  JSON action items include compact excerpts with real line numbers; exports
  include full hunks. `follow-up` and all kind/action/state values round-trip
  across md and JSON. Held back from 5 by the prefix/dangling link problem
  (finding 1), `title: null` (finding 6), and the done-task default mismatch
  (finding 2) — each forces defensive consumer code.
- **Handoff readiness: 4/5.** `handoff` markdown is a legitimately one-shot
  implementer prompt: action items with diff excerpts, the reviewer's ordered
  walkthrough with rationale, and trimmed reference hunks scoped to files
  with action items — an implementer would not need to re-read the diff for
  this stack. Held back from 5 by findings 1-3: link integrity is not
  guaranteed (dangling/prefix ids), the two handoff formats disagree on
  defaults, and anchorless typo comments degrade silently, so a consumer
  cannot fully trust the artifact without validating it.

# Top proposals

1. **Resolve and validate `--comment` links at write time.** Store the full
   comment id, error on unknown ids/prefixes (the resolver already exists for
   `set-state`/`resolve`), and emit full ids in `linked_comment_ids`. This
   is the single biggest trust fix for the JSON handoff artifact.
2. **Unify handoff defaults across formats.** Either default
   `handoff --format json` to open items (matching markdown) or include a
   filtered/total count and the `--only-open` hint in both. Formats silently
   disagreeing is worse than either default.
3. **Warn or fail when a comment/task line doesn't anchor.** `comments add
   --line 999` should at minimum print a warning to stderr and mark the
   result (`"anchor": null` is easy to miss); a `--strict` failure mode would
   let agents avoid persisting degraded comments.
4. **Give tasks parity with comments: `tasks edit` and `tasks delete`.**
   Append-only tasks plus done-tasks-in-default-JSON means mistakes are
   permanently visible to consumers.
5. **Split help into domain vs global flag sections** (clap `help_heading`),
   fill the remaining blank help strings, and accept a positional file for
   `hunks list`. Low-cost polish that removes most remaining discoverability
   friction.
