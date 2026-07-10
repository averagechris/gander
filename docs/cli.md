# Gander CLI reference

Gander is CLI-first: every durable review operation writes only Gander review
state (the `--state-file` file or the XDG state path shown by `gander paths`). These
commands inspect jj-visible code, but mutation commands do **not** edit the code
workspace, fetch from forges, or post reviews remotely.

Global options accepted by all commands include `--repo <path>`, `--rev <rev>`
(default `@`), `--base <revset>` (default `trunk()`), repeated `--ignore`,
generated-file filters, `--state-file <path>`, and `--config <path>`. They are
listed under a separate "Target & state (global)" heading in every
subcommand's `--help`.

Durable review objects (sessions, tasks, walkthroughs, and scoped comments) are keyed by the exact
`base..rev` target. When a read command's target matches no open session but
one exists for another target, Gander warns on stderr and names the session
(`warning: no open review session matches 'trunk()..@'; open session "…"
targets 'main..@' …`) instead of silently emitting a truncated artifact; read
commands never create sessions as a side effect. New comments belong to the
active matching session when one exists; legacy unscoped comments remain visible
for compatibility in every session until edited or otherwise migrated into a
session scope.

Most list commands and mutation echoes accept `--format <json|text>`. JSON is
always the default (agent-stable); `text` prints compact aligned rows or a
short human echo.

## Reviews

```sh
gander reviews create [--title <title>]
gander reviews list [--format json|text]
gander reviews show <id>
```

`create` opens a durable review session for the current target. Example output:

```json
{
  "id": "b777f59e-c91c-4610-a272-c4154c2e350e",
  "title": "Demo",
  "target": { "revset": "trunk()..@", "base": "trunk()", "revision": "@", "repo": "/repo", "file": null, "line": null, "end_line": null, "symbol": null },
  "status": "open",
  "walkthroughs": [],
  "tasks": [],
  "created_at": "2026-07-04T22:19:05.917185Z",
  "updated_at": "2026-07-04T22:19:05.917185Z"
}
```

`list` returns `{ "sessions": [...] }` with `task_count` and
`walkthrough_count`; `show` returns the full session object.

## Files and hunks

```sh
gander files list [--format json|text]
gander hunks list [<path>] [--path <path>] [--format json|text]
gander hunks show <path:index> [--format json|diff|text]
```

These are read-only queries over the current jj diff. `hunks list` returns
hunk ids suitable for `hunks show` and accepts the file as a positional or
`--path` (`--file` remains a hidden compatibility alias). `hunks show --format diff` (alias `text`) prints a unified diff.

## Comments

```sh
gander comments list [--format json|text]
gander comments add (--path <path> [--line <n>] [--end-line <n>] | --general) --body <text> \
  [--kind note|issue|question|praise] [--action none|fix|explain|test|follow-up] \
  [--state draft|todo] [--format json|text]
gander comments reply <id> --body <text> [--resolve] [--format json|text]
gander comments resolve <id> [--reply <text>] [--format json|text]
gander comments set-state <id> --state draft|todo|resolved [--format json|text]
gander comments ready (<id>... | --all-drafts) [--format json|text]
gander comments edit <id> [--path <path>] [--line <n> | --start-line <n> --end-line <n>] \
  [--body <text>] [--kind note|issue|question|praise] [--action none|fix|explain|test|follow-up] [--format json|text]
gander comments delete <id> [--format json|text]
```

`add` creates a persisted comment. Exactly one of `--path` or `--general` is
required. General comments are session-level notes with no file location, line,
anchor, or excerpt. `--state` overrides the configured initial state for this
comment. `--end-line` requires `--line`; `edit --line` conflicts with `--start-line`, and `edit --end-line` needs either a supplied start or an existing anchored start. End lines must be greater than or equal to their start. `--line`, `--start-line`, and `--end-line`
are 1-indexed diff line anchors, preferring the new side (post-image). For a
removed-only line with no new-side coordinate, Gander falls back to the old-side
line in the current jj diff. Omitting them creates a file-level anchor. `edit`
updates the body and/or re-anchors the comment with the same semantics,
recomputing the stored excerpt anchor from the current diff. If a supplied line
is not in either accepted side for the file, Gander stores the comment without
an excerpt anchor and prints a warning so intentional unchanged-context comments
remain possible.
`delete` removes the comment from local Gander review state.

Comment state semantics are deliberately workflow-oriented:

- `draft`: saved, private, and withheld from implementation handoff. Drafts are
  durable reviewer notes until explicitly readied, resolved, or deleted.
- `todo`: ready/actionable. Every todo comment asks an agent to address it,
  regardless of `kind` (`question`, `praise`, `note`, or `issue`) or `action`
  (`none`, `explain`, `fix`, `test`, or `follow-up`). A todo praise may ask the
  agent to preserve a good behavior; a todo question asks for an answer/change.
- `resolved`: retained history. Resolved comments are not selected for prompt
  handoff or delegation by default, but remain in full exports and threads.

New comments default to `[comments].initial-state`, which is `todo`; set it to
`draft` to save new feedback privately until it is readied:

```toml
[comments]
initial-state = "draft" # todo (default) | draft
```

Schema compatibility note: missing serialized comment states still deserialize as
`draft`; this preserves artifacts and state files written before `CommentState`
was introduced.

Example:

```json
{
  "id": "d9e9e6de-f819-4022-823d-b1ed573d6091",
  "session_id": "active-review-session-id",
  "path": "README.md",
  "line": 1,
  "body": "Clarify intro",
  "kind": "issue",
  "action": "fix",
  "state": "todo",
  "created_at": "2026-07-04T22:19:06.819614Z"
}
```

`comments list` returns `{ "comments": [...] }`. `comments ready <id>...` marks
the selected active-session drafts `todo`; `comments ready --all-drafts` marks
all active-session drafts `todo`. The operation is atomic: if any supplied id is
unknown, ambiguous, not in the active session, or already resolved, no comments
are changed. `reply` appends an immutable
UUID-addressed reply with a timestamp and updates the parent comment's
`updated_at`; `--resolve` also marks the parent resolved. `resolve` is a
convenience for `set-state --state resolved`, and `--reply` first appends the
given reply before resolving.

## Tasks

```sh
gander tasks add --title <title> [--body <text>] [--action none|fix|explain|test|follow-up] \
  [--comment <comment-id>] [--path <path>] [--line <n>] [--format json|text]
gander tasks list [--format json|text]
gander tasks complete <id> [--summary <resolution>] [--format json|text]
gander tasks reopen <id> [--format json|text]
gander tasks edit <id> [--title <title>] [--body <text>] [--action none|fix|explain|test|follow-up] \
  [--comment <comment-id>] [--path <path>] [--line <n>] [--format json|text]
gander tasks delete <id> [--format json|text]
```

Tasks are review-state todos for humans or agents. Ids on `complete`,
`reopen`, `edit`, and `delete` accept unambiguous prefixes; unknown or
ambiguous prefixes are one-line errors. Comment-backed todo entries in
`tasks list` always carry a string `title` (synthesized from the comment
body's first line when the comment has no explicit title).
Example `tasks list`:
`--line` is a 1-indexed diff line anchor in the current jj diff and requires `--path` when adding a task. Editing patches the existing target: path-only preserves an existing line, line-only preserves an existing file, and line-only is rejected if the task has no target file. New side is
preferred, old side only for removed-only lines. `--action` emits `follow-up`; legacy JSON or CLI input spelled
`followup` is still accepted. `--comment` accepts a full comment id or
unambiguous prefix and stores the canonical full id; unknown or ambiguous
comment ids are rejected.

```json
{
  "tasks": [
    {
      "id": "57e87a50-e83a-4778-9cd6-b154d1a2caa5",
      "title": "Update intro",
      "body": null,
      "target": { "file": "README.md", "line": 1, "end_line": null, "symbol": null, "revset": null, "base": null, "revision": null, "repo": null },
      "action": "fix",
      "status": "open",
      "source_comment_id": null,
      "resolution": null,
      "source": "session"
    }
  ]
}
```

## Walkthroughs

```sh
gander walkthrough add-step --title <title> [--path <path>] [--line <n>] \
  [--end-line <n>] [--symbol <name>] [--why <text>] [--body <text>] \
  [--importance spotlight|glance] [--change <change-id>] [--artifact '<json>']...
gander walkthrough add-chapter --change <change-id> --summary <text>
gander walkthrough set [--file <spec.json|->] [--dry-run]
gander walkthrough remove-step <id>
gander walkthrough move-step <id> --to <zero-based-index>
gander walkthrough show
gander walkthrough export
```

Walkthroughs are the durable source of truth for zen tours. Steps have a title,
importance (`spotlight` tours; `glance` lands on the glance board), optional
why/body/artifacts, an optional change id, and an optional stable target. `--line`
and `--end-line` are 1-indexed diff line anchors: new side (post-image)
preferred, with old-side fallback for removed-only lines in the current jj diff.
Chapters introduce stack changes and use `summary` as their narrative.
`walkthrough set` replaces the current walkthrough from `{ "title", "steps" }`
JSON using the same step fields as state.json. Pass `--dry-run` to validate the
spec, print diagnostics plus the would-be replacement summary, and echo the
preserved-id result without writing state. The target is a nested object:

```json
{
  "title": "Review tour",
  "steps": [
    {
      "kind": "chapter",
      "change_id": "abc123",
      "title": "abc123",
      "body": "Why this stack change exists"
    },
    {
      "kind": "step",
      "importance": "spotlight",
      "title": "Read the state model",
      "why": "The durable fields drive CLI, TUI, and MCP behavior.",
      "body": "Check that serialization remains backward compatible.",
      "change_id": "abc123",
      "target": { "file": "src/state.rs", "line": 129, "symbol": "WalkthroughStep" },
      "artifacts": [
        { "title": "Example", "kind": "note", "body": "Agents can attach supporting context." }
      ]
    },
    {
      "kind": "step",
      "importance": "glance",
      "title": "Skim docs",
      "target": { "file": "docs/cli.md", "line": 146 }
    }
  ]
}
```

Repeated `walkthrough set` runs preserve existing step ids, matching by explicit
id first and then by `(kind, title, target.file, target.line)`; only new steps
receive new UUIDs. The command warns about unknown JSON fields and targets that
fall outside the diff line space (including the valid ranges); chapter steps
must include a non-empty `change_id`.
`show` emits JSON; `export` emits Markdown.

File-anchor commands accept both `--path` and `--file` for compatibility. The
canonical form in docs and JSON remains `--path`.

## Drafts

## Tour slide deck

```sh
gander tui --tour
gander tour render [--width 100] [--height 30] [--slide N]
```

`tui --tour` launches the normal TUI directly into zen's full-screen slide deck.
`tour render` uses the same ratatui draw path with a test backend and prints the
slides as plain text separated by `──── slide K/N ────`, which is useful for
agents and documentation snapshots. If a walkthrough is present, spotlight steps
become slides, chapter steps become intro slides, and glance steps appear on the
final “At a glance” slide; otherwise the tour falls back to changed files.

```sh
gander drafts list|add [--file <spec>]|remove --id <id>
```

Drafts remain a supported overlay-backed CLI surface. Specs are JSON files or
stdin, and a live TUI on the same workspace picks up overlay writes within a
poll. Public `chunks` and `briefs` commands have been removed; use
`walkthrough` for durable tour curation. ACP/MCP agents may still provide live
overlay chunks and change briefs, which Gander adapts into walkthrough/zen views.

## Live state and the TUI

Review-state writes are safe while a TUI holds the session: the TUI watches
the state file and merges external changes (a CLI-added comment appears in
the running TUI within a poll and survives the TUI's save/quit). Deletions
made in the TUI are not resurrected by merges. Same-id comments use the newer
`updated_at` value for body/state metadata and union append-only replies by
reply id, so an external reply or resolution is not overwritten by a later TUI
save. Tasks, walkthroughs, walkthrough steps, and sessions likewise use newer
`updated_at` values for same-id conflicts.

## Export/import and state utilities

```sh
gander handoff [--mode prompt|delegate] [--format markdown|json] [--output <path>] [--copy]
gander export [json|markdown|html] [--profile human|agent] [--output <path>]
gander import <json-artifact>
gander mark-viewed
gander mark-generated-viewed
gander paths
gander summary
```

`handoff` is the one-shot actionable prompt for an implementer agent. Markdown
defaults to action items first, walkthrough next, then reference hunks limited
to files that carry action items or walkthrough stops. JSON uses the same
default action-item selection: open tasks plus `todo` comments only. Drafts are
saved/private/withheld, and resolved comments are history. Action
items are deterministically ordered the same way in both formats: action
priority (fix > test > follow-up > other), then path, then line. `handoff
--format json` is a stable action artifact shaped
as `{ "session", "action_items", "walkthrough", "reference" }`: action items
are task/comment objects with `id`, `source`, `kind`/`action`, `path`, `line`,
`end_line`, `excerpt`, `body`, `state`, and canonical linked task/comment ids.
`--output` writes without stdout body output; `--copy` copies it to the clipboard (pbcopy,
wl-copy, xclip, or OSC52 via `/dev/tty`). Use `export --profile agent` instead
when you need the full session artifact for archive/reference or broad
automation: full exports include all comments (`draft`, `todo`, and
`resolved`), all tasks, walkthroughs, replies, and excerpts when available (its
H1 is `# Review session export (agent profile)`; the two artifacts
cross-reference each other). Import currently restores only matching
viewed state and duplicate-safe comments; exported tasks and walkthroughs are
not restored by `gander import`.

Humans read the durable review session directly in the TUI (and future web UI).
Prompt handoff and delegate mode are outbound adapters for transferring work to
an external agent or harness. Delegate mode emits an independently versioned
`gander_delegation` packet for typed orchestration; it selects open work without
mutating review state or executing verification text:

```sh
gander handoff --mode delegate \
  --task <task-prefix> --include-comment <comment-prefix> \
  --to implementation-agent \
  --objective "Fix the parser finding and add coverage." \
  --constraint "Preserve the public API." \
  --accept "The regression test fails before the fix and passes after it." \
  --verify "nix run .#ci-test" \
  --format json
```

Selectors accept full ids or unambiguous prefixes. With no selectors, delegate
mode includes open durable tasks and actionable `todo` comments, folds linked
source comments into task evidence, and excludes resolved comments and closed
tasks. Draft comments are rejected when explicitly selected for delegation unless
they are first readied, and implicit delegation never selects them. Packets include source fingerprints, walkthrough context, relevant
hunks, reply history, and concrete `comments resolve --reply` / `tasks complete
--summary` return commands. `--verify` is inert requested text; Gander never
executes it.

`export html` writes a self-contained static review page and rejects an explicitly supplied `--profile`; JSON and Markdown are the complete session artifact formats. `--profile agent` adds all raw hunks and comment excerpts for tools, including resolved comments as reference.

## Bundled agent skills

```sh
gander skills list [--format text|json]
gander skills show <name> [--format markdown|json]
gander skills install [<name>...] [--dir <path>] [--force] [--format text|json]
```

`skills` commands are embedded, config-free, and repository-free: they run
without initializing jj or Gander review state. `show` prints the exact bundled
`SKILL.md` by default. `install` writes `<dir>/<name>/SKILL.md`, validates all
requested names and overwrite conflicts before writing, and refuses to replace
files unless `--force` is supplied. The harness-neutral default directory is
`~/.agents/skills`; use `--dir` for a project or harness-specific location.

The bundled `gander-review` skill teaches read-only review authoring. The
`gander-address-review` skill teaches an implementation agent to consume the
relevant review state, use the project's normal tools, and then record concise
reply, resolution, and task-completion evidence in Gander.

## MCP and ACP

```sh
gander mcp
gander acp
```

Normal automation should use the CLI directly. `mcp` is optional typed/live harness integration, including CLI-parity state tools. `acp` is a low-level/internal line-delimited JSON-RPC bridge for debugging live-session curation: it
bridges to a running TUI on the same workspace when one exists (announcing
`bridged to live TUI session` vs `serving snapshot` on stderr, and a `mode`
field in the `initialize` response). See `docs/acp.md`.

## Automation without MCP

For low-context agent loops, use the CLI and `jq` directly:

```sh
state="$TMPDIR/gander-review.json"
rm -f "$state"
review_id=$(gander --state-file "$state" reviews create --title "Agent pass" | jq -r .id)
comment_id=$(gander --state-file "$state" comments add --path README.md --line 1 \
  --state todo --kind issue --action fix --body "Clarify the introduction." | jq -r .id)
task_id=$(gander --state-file "$state" tasks add --title "Fix intro" \
  --action fix --comment "$comment_id" --path README.md --line 1 | jq -r .id)
gander --state-file "$state" tasks list | jq -r '.tasks[] | select(.status == "open") | .id' |
  while read -r id; do
    gander --state-file "$state" tasks complete "$id" --summary "Handled by agent"
  done
gander --state-file "$state" reviews show "$review_id" | jq '{id, title, tasks}'
```
## `gander present`

`gander present` is the CLI-first way for an external process to drive what a
human sees in a live TUI. It discovers the live instance for the current
workspace from the registry, connects once to that instance's ACP socket,
sends a `present/*` JSON-RPC request, and prints the raw JSON-RPC response.
If several live TUIs serve the same workspace, pass `--pid <pid>`; otherwise
the command errors and lists the candidate pids.

Examples:

```bash
gander present                         # present/status
gander present start                   # start the tour
gander present next                    # advance one slide
gander present goto --index 3          # zero-based slide index
gander present goto --step step-id     # durable walkthrough step id
gander present focus --path src/foo.rs --line 42 --end-line 60 --note "look here"
gander present reload                  # reload review state and rebuild tour
```

The command requires a live TUI (`gander tui --tour`) and respects modal
safety: if the human is typing a comment or using a popup, the TUI returns
`user is busy: <mode>` instead of moving the view.
