# Gander CLI reference

Gander is CLI-first: every durable review operation writes only Gander review
state (the `--state-file` file or the XDG state path shown by `gander paths`). These
commands inspect jj-visible code, but mutation commands do **not** edit the code
workspace, fetch from forges, or post reviews remotely.

Global options accepted by all commands include `--repo <path>`, `--rev <revset>`
(default `@`), `--base <revset>` (default `trunk()`), repeated `--ignore`,
generated-file filters, `--state-file <path>`, and `--config <path>`.

## Reviews

```sh
gander reviews create [--title <title>]
gander reviews list
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
gander files list
gander hunks list [--file <path>]
gander hunks show <path:index>
```

These are read-only JSON queries over the current jj diff. `hunks list` returns
hunk ids suitable for `hunks show`.

## Comments

```sh
gander comments list
gander comments add --path <path> [--line <n>] [--end-line <n>] --body <text> \
  [--kind note|issue|question|praise] [--action fix|explain|test|follow-up]
gander comments resolve <id>
gander comments set-state <id> --state draft|todo|resolved
gander comments edit <id> [--path <path>] [--line <n> | --start-line <n> --end-line <n>] \
  [--body <text>]
gander comments delete <id>
```

`add` creates a persisted comment. `--line`, `--start-line`, and `--end-line`
are 1-indexed new-side (post-image) line numbers in the current jj diff;
omitting them creates a file-level anchor. `edit` updates the body and/or
re-anchors the comment with the same post-image line semantics, recomputing the
stored excerpt anchor from the current diff. `delete` removes the comment from
local Gander review state. Example:

```json
{
  "id": "d9e9e6de-f819-4022-823d-b1ed573d6091",
  "path": "README.md",
  "line": 1,
  "body": "Clarify intro",
  "kind": "issue",
  "action": "fix",
  "state": "draft",
  "created_at": "2026-07-04T22:19:06.819614Z"
}
```

`comments list` returns `{ "comments": [...] }`. `resolve` is a convenience for
`set-state --state resolved`.

## Tasks

```sh
gander tasks add --title <title> [--body <text>] [--action fix|explain|test|follow-up] \
  [--comment <comment-id>] [--path <path>] [--line <n>]
gander tasks list
gander tasks complete <id> [--summary <resolution>]
gander tasks reopen <id>
```

Tasks are review-state todos for humans or agents. Example `tasks list`:
`--line` is a 1-indexed new-side (post-image) line number in the current jj
diff. `--action` emits `follow-up`; legacy JSON or CLI input spelled
`followup` is still accepted.

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
  [--end-line <n>] [--symbol <name>] [--why <text>] [--body <text>]
gander walkthrough remove-step <id>
gander walkthrough move-step <id> --to <zero-based-index>
gander walkthrough show
gander walkthrough export
```

Walkthrough steps have a title, optional why/body, and an optional stable target.
`--line` and `--end-line` are 1-indexed new-side (post-image) line numbers in
the current jj diff.
`show` emits JSON; `export` emits Markdown.

File-anchor commands accept both `--path` and `--file` for compatibility. The
canonical form in docs and JSON remains `--path`.

## Export/import and state utilities

```sh
gander handoff [--format markdown|json] [--only-open] [--output <path>] [--copy]
gander export [json|markdown|html] [--profile human|agent] [--output <path>]
gander import <json-artifact>
gander mark-viewed
gander mark-generated-viewed
gander paths
gander summary
```

`handoff` is the one-shot actionable prompt for an implementer agent. Markdown
defaults to action items first, walkthrough next, then reference hunks limited
to files that carry action items or walkthrough stops. `handoff --format json` is a stable action artifact shaped
as `{ "session", "action_items", "walkthrough", "reference" }`: action items
are task/comment objects with `id`, `source`, `kind`/`action`, `path`, `line`,
`excerpt`, `body`, `state`, and linked task/comment ids. `--only-open` filters
to unresolved comments and open tasks; `--output` writes without stdout body
output; `--copy` copies it to the clipboard (pbcopy, wl-copy, xclip, or OSC52
via `/dev/tty`). Use `export --profile agent` instead when you need the full
session artifact for import/archive or broad automation.

`export html` writes a self-contained static review page. JSON and Markdown are
the complete session artifact formats; `--profile agent` adds all raw hunks and
comment excerpts for tools, including resolved comments as reference.

## MCP and ACP

```sh
gander mcp
gander acp
```

`mcp` exposes typed tools for harnesses, including CLI-parity state tools.
`acp` is the line-delimited JSON-RPC bridge used by older live-session flows.

## Automation without MCP

For low-context agent loops, use the CLI and `jq` directly:

```sh
state="$TMPDIR/gander-review.json"
rm -f "$state"
review_id=$(gander --state-file "$state" reviews create --title "Agent pass" | jq -r .id)
comment_id=$(gander --state-file "$state" comments add --path README.md --line 1 \
  --kind issue --action fix --body "Clarify the introduction." | jq -r .id)
task_id=$(gander --state-file "$state" tasks add --title "Fix intro" \
  --action fix --comment "$comment_id" --path README.md --line 1 | jq -r .id)
gander --state-file "$state" tasks list | jq -r '.tasks[] | select(.status == "open") | .id' |
  while read -r id; do
    gander --state-file "$state" tasks complete "$id" --summary "Handled by agent"
  done
gander --state-file "$state" reviews show "$review_id" | jq '{id, title, tasks}'
```
