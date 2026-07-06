# Summary

Gander's CLI review loop is genuinely usable end-to-end without touching the TUI: session
creation, file/hunk inspection, line-anchored comments with kind/action, comment-linked
tasks, walkthrough steps, and both export and handoff artifacts all worked on the first or
second try. Error handling is a highlight — clean one-line errors, prefix matching with
ambiguity detection, and an out-of-diff comment is stored with a warning plus a recovery
hint instead of being rejected or silently mangled. The handoff JSON is close to the ideal
single artifact: ordered action items with anchors, code excerpts, and bidirectional
task↔comment links.

The most important product gaps found this round:

1. **The default base (`trunk()`) silently diverges from the session base.** In a repo
   where `trunk()` doesn't resolve to `main` (no remote bookmark), `gander handoff` with no
   `-b` produced a plausible-looking but wrong artifact: 5 comments, **0 tasks, no
   walkthrough, no session title**, target `trunk() → @` — with no warning that an open
   session exists for `main..@`. An agent consuming that handoff gets a silently truncated
   review.
2. **Inconsistent state scoping:** comments are visible regardless of base while tasks and
   walkthroughs are session-scoped, which is exactly what makes gap #1 look plausible
   instead of obviously empty.
3. **Format-surface inconsistencies:** `--format text` exists on `list`/`comments add` but
   not on `hunks show` (which uses `diff` instead of `text`), and not at all on `tasks
   add/complete/reopen/edit/delete` or `comments delete`, which echo 20–50 lines of JSON at
   a human.
4. **Range anchors collapse to a single line in handoff** (both formats), while export JSON
   preserves `end_line`; export JSON comments also lack the `linked_task_ids` back-pointer
   that handoff JSON has.

# Step log

Fixture: `/tmp/gander-eval/round6-s1`, a 3-change stack over `main`
(`feat: priority scheduling` → `refactor: extract retry policy` → `test: cover priority
ordering`), empty working copy on top. `G=/Users/chris/projects/gander/target/release/gander`,
all commands run from the fixture root.

1. **Session create** — `G -b main -r @ reviews create --title "Round 6: priority queue
   scheduling + retry policy extraction"` → JSON echo with id, target revset `main..@`,
   status open. Echo is ~25 lines of JSON; fine for scripts, chatty for a human.
2. **Files** — `G -b main -r @ files list` (JSON default) and `--format text` → clean
   aligned table: status, path, +/−, hunk count. 7 files.
3. **Hunks list** — `--format text` gives one line per hunk with id (`path:index`), +/−,
   header, and first-line preview. Piping JSON through `head -20` worked cleanly: exit 0,
   no broken-pipe panic. Narrowing to one file (`hunks list src/worker.rs`) works.
4. **Hunks show** — `--format text` is **rejected** (`possible values: json, diff`);
   `--format diff` prints a plain unified diff, no color/line numbers. Showed
   `src/queue.rs:1`, `src/retry.rs:0`, `src/worker.rs:0`, `tests/basic.rs:1`,
   `src/config.rs:0` — content complete and correct across the whole stack.
5. **Comments** — added 5 with `--kind`/`--action` (issue/fix ×2, question/explain,
   praise, note/test). `--format text` echo is excellent: id, state/kind/action, anchor,
   plus the **anchored source line text** as confirmation. Range anchor (`--line 28
   --end-line 38`) accepted.
6. **Tasks** — added 2, each linked via `--comment <8-char prefix>` (prefix expanded to the
   full comment id) and anchored with `--path/--line`. Echo is JSON-only; no `--format`
   flag exists on any task mutation.
7. **Walkthrough** — `walkthrough add` fails but clap suggests `add-step` (good tip);
   added 2 steps with `--symbol`, `--why`, range target. Echo JSON-only.
8. **Human listing** — `comments list --format text` and `tasks list --format text` are
   compact and readable (id prefix, state, kind/action, anchor, truncated body; tasks show
   the linked comment prefix).
9. **Agent role** — `export markdown --profile agent` (389 lines: header, ordered action
   items with diff excerpts, walkthrough, full reference hunks), `export json --profile
   agent` (58 KB, version 5, anchors with hunk metadata + line fingerprints, structured
   excerpts), `walkthrough export` (clean Markdown), `handoff` markdown + JSON. Handoff
   JSON shape `{session, action_items, walkthrough, reference}` with items ordered
   fix → test → explain → praise, exactly as documented.
10. **Lifecycle probes** — `comments resolve af995d23` dropped the handoff item count from
    7 to 6 (`--only-open` default works); `set-state --state todo` reflected in list;
    `comments edit` re-anchored an out-of-diff comment onto a valid line; delete → repeat
    delete errors cleanly; `tasks complete`/`reopen`/`edit`/`delete` all work by prefix.
11. **Error probes** — see Findings; every failure was a clean one-liner with a correct
    non-zero exit code.
12. **Repro for the base trap** (surprising behavior):

    ```
    cd /tmp/gander-eval/round6-s1
    G -b main -r @ handoff | sed -n 7p   # "Action items: 7 item(s) (2 open task(s), ..."  + session title
    G handoff | sed -n 7p                # "Action items: 5 item(s) (0 open task(s), ..." — no title, no walkthrough, no warning
    G tasks list --format text           # empty
    G comments list --format text        # all 5 comments — inconsistent with tasks
    G files list --format text | head -3 # whole repo shown as "added" (trunk() == root())
    ```

# Findings

- **major — `handoff`/queries silently use a different session when `-b` is omitted.**
  Default base is `trunk()`; in a fixture with no remote bookmark it resolves past `main`,
  so `gander handoff` emits a wrong-but-plausible artifact (0 tasks, no walkthrough, no
  session title) with zero warning, even though `reviews list` shows an open session for
  `main..@` in the same repo. Repro above. An implementer agent run with the wrong/default
  flags gets a silently truncated review. Expected: warn or auto-attach when exactly one
  open session exists for the repo, or at minimum print "no session matched target".
- **major — comments and tasks disagree on scoping.** With no `-b`, `comments list`
  returns all 5 comments while `tasks list` returns nothing. Whatever the intended model
  (comments repo-scoped, tasks session-scoped), the asymmetry makes the base-mismatch trap
  above look like a valid review state. Repro above.
- **minor — range anchors collapse in handoff.** Comment `a3c7b887` is anchored
  `src/worker.rs:28-38`; handoff markdown prints `src/worker.rs`:28 and handoff JSON items
  have `line` but **no `end_line` key**, while export JSON preserves `end_line: 38`.
  Formats disagree on the same anchor. Repro: `G -b main -r @ handoff --format json |
  python3 -c "import json,sys; print([i.keys() for i in json.load(sys.stdin)['action_items']])"`.
- **minor — export JSON lacks comment→task back-links.** Handoff JSON items carry both
  `linked_task_ids` and `linked_comment_ids`; export JSON tasks have `linked_comment_ids`
  but comments have no `linked_task_ids` field, so an agent consuming the export must
  invert the join manually.
- **minor — inconsistent `--format` surface.** `files/hunks/comments/tasks list` accept
  `text`; `hunks show` accepts only `json|diff` (a `--format text` attempt errors);
  `comments add/edit` accept `text` but `comments delete`, `resolve`, `set-state`, and
  every `tasks`/`walkthrough` mutation are JSON-only. `comments delete` echoes ~30 lines of
  JSON including hunk fingerprints at a human. Repro: `G -b main -r @ hunks show
  src/retry.rs:0 --format text`.
- **minor — optional-field serialization is inconsistent in export JSON.** `end_line` is
  omitted from comment objects when unset but present as a key when set (per-item key sets
  differ); elsewhere (`reviews create` target, task target) unset fields are explicit
  `null`. Schema-driven consumers have to treat both conventions.
- **papercut — mixed old/new line numbers interleave confusingly in markdown excerpts.**
  In the handoff/export action-item excerpt for `src/queue.rs:46`, `-  28  self.jobs.pop_front()`
  renders between new-side lines 44 and 45, and `-  32` after new line 51. Numbers are
  technically correct per-side but read as out-of-order garbage.
- **papercut — praise counts as an action item.** A `[comment][praise/none]` entry appears
  in the handoff "Action items" list of an implementation prompt (and inflates the count).
- **papercut — linked task+comment pairs render as two separate action items.** The
  off-by-one issue occupies two adjacent entries (task 38c645e5 + comment 703b82ce) that
  say the same thing; a one-shot prompt would be tighter with the pair merged.
- **papercut — `tasks reopen` on an already-open task exits 0 and bumps `updated_at`**
  with no "already open" notice.
- **papercut — walkthrough verbs are `add-step`/`remove-step`/`move-step`** while every
  other noun uses `add`/`edit`/`delete`; `walkthrough add` fails (clap's suggestion
  rescues it). There is also no `walkthrough edit-step`.
- **papercut — `summary` column misalignment**: `added` rows get extra padding
  (`• added       src/priority.rs` vs `• mod       src/config.rs`).

Positives worth keeping: out-of-diff line comments are stored with a warning and a
recovery hint (`use 'comments edit' to fix`) and the anchor is marked `(no anchor ⚠)` in
the echo; unknown/ambiguous id prefixes produce exact one-line errors (`error: ambiguous
comment prefix 'a'`); mistyped flags get clap suggestions; pipes don't panic; the
`comments add --format text` echo showing the anchored source line is a great
wrong-line-number tripwire; help for `export`/`handoff` cross-references the other command
with concrete examples.

# Scores

- **Discoverability: 4/5.** The subcommand tree maps cleanly onto the review loop, `--help`
  everywhere is complete, `export`/`handoff` help includes worked examples and points at
  each other, and clap tips rescue near-miss verbs. Deductions: format-flag vocabulary must
  be re-learned per subcommand (`text` vs `diff` vs nothing), `walkthrough add-step` breaks
  the verb convention, and nothing tells you that omitting `-b` targets a different session
  than the one you just created.
- **Output quality for humans: 3.5/5.** The `--format text` tables (files, hunks, comments,
  tasks), `summary`, and the comment-add echo with anchored line text are exactly right.
  But JSON is the default everywhere, `hunks show` has no compact text mode, and half the
  mutations (all of `tasks`, `comments delete/resolve/set-state`, walkthrough) shout raw
  JSON — a routine human review keeps paying a "add --format text, oh wait, unsupported
  here" tax.
- **Output quality for agents: 4/5.** Handoff JSON is a genuinely strong single artifact:
  stable shape, action items ordered by priority, bidirectional links, structured excerpts
  with per-line new/old numbers, and reference hunks last. Export JSON adds anchors with
  hunk headers and line fingerprints. Deductions: `end_line` lost in handoff, comment→task
  links missing in export JSON, and omitted-vs-null field conventions differ between the
  two artifacts.
- **Handoff readiness: 4/5.** With the right flags, `handoff --format json` is directly
  actionable without re-reading the diff: fixes first, anchors + excerpts inline, trimmed
  reference hunks scoped to action/walkthrough files. Deductions: the silent
  default-base trap can hand an agent a truncated review with no error; duplicate
  task/comment pairs and praise-as-action-item add prompt noise; range extents are lost.

# Top proposals

1. **Make session targeting fail-safe.** When a command's (base, rev) doesn't match any
   open session but the repo has exactly one, either attach to it or emit a loud warning
   naming the mismatched target (`note: open session 1503913f targets main..@; you are
   querying trunk()..@`). This single change removes the worst failure mode found.
2. **Unify state scoping for comments vs tasks/walkthroughs** (or surface the distinction):
   `comments list` and `tasks list` should agree on what "the current review session" means
   for the same flags.
3. **Finish the `--format text` surface**: accept `text` on `hunks show` (compact, numbered)
   and add proportionate one-line text echoes to `tasks add/complete/reopen/edit/delete`,
   `comments delete/resolve/set-state`, and walkthrough mutations, matching the excellent
   `comments add --format text` echo.
4. **Preserve range anchors and links uniformly**: add `end_line` to handoff items (both
   formats) and `linked_task_ids` to export JSON comments; pick one convention
   (explicit `null`) for absent optional fields across all artifacts.
5. **Tighten the handoff prompt**: merge linked task+comment pairs into one action item
   (task title + comment body + excerpt), and demote `praise`/action-less comments out of
   "Action items" into a separate "FYI" section so the item count means "things to do".
6. **Fix markdown excerpt interleaving** by grouping removed lines before the added lines
   they replace (or labeling sides), so mixed old/new numbering doesn't read as disorder.
