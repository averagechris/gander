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

Durable review objects (sessions, action items, walkthroughs, and scoped comments) are keyed by the exact
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

## Themes

```sh
gander themes list [--format json|text]
```

`themes list` is the scriptable palette contract for TUI and future web
renderers. JSON returns `{ "themes": [{ "name": "gander", "aliases": [...] }, ...] }`.
Canonical names are `gander`, `catppuccin`, `gruvbox`, `solarized`, `nord`,
`tokyo-night`, and `dracula`; config also accepts documented aliases after
normalizing ASCII case and treating spaces/underscores as hyphens.

## Reviews

```sh
gander reviews create [--title <title>]
gander reviews list [--format json|text]
gander reviews show <id>
gander reviews disposition show [--format json|text]
gander reviews disposition set comment|approve|request-changes [--format json|text]
gander reviews disposition clear [--format json|text]
```

`create` opens a durable review session for the current target. Example output:

```json
{
  "id": "b777f59e-c91c-4610-a272-c4154c2e350e",
  "title": "Demo",
  "target": { "revset": "trunk()..@", "base": "trunk()", "revision": "@", "repo": "/repo", "file": null, "line": null, "end_line": null, "symbol": null },
  "status": "open",
  "walkthroughs": [],
  "action_items": [],
  "created_at": "2026-07-04T22:19:05.917185Z",
  "updated_at": "2026-07-04T22:19:05.917185Z"
}
```

`list` returns `{ "sessions": [...] }` with `action_item_count` and
`walkthrough_count`; `show` returns the full session object. The optional
session disposition is durable local review state used by team exports; it does
not post to any forge.

## Files and hunks

```sh
gander files list [--format json|text]
gander hunks list [<path>] [--path <path>] [--format json|text]
gander hunks show <path:index> [--format json|diff|text]
```

These are read-only queries over the current jj diff. `hunks list` returns
hunk ids suitable for `hunks show` and accepts the file as a positional or
`--path` (`--file` remains a hidden compatibility alias). `hunks show --format diff` (alias `text`) prints a unified diff.

## Attention map

```sh
gander attention list [--mode effective|assigned] [--format json|text]
gander attention coverage show [--format json|text]
gander attention skim-fold list [--format json|text]
gander attention acknowledge (--path <path> [--line <n> [--end-line <n>]] | \
  --fold-id <stable-id> | --all) [--format json|text]
gander attention set --path <path> [--line <n> [--end-line <n>]] \
  --salience spotlight|supporting|skim [--rationale <text>] [--format json|text]
gander attention clear --path <path> [--line <n> [--end-line <n>]] [--format json|text]
gander attention promote|demote --path <path> [--line <n> [--end-line <n>]] \
  [--rationale <text>] [--format json|text]
gander attention seed-heuristics [--format json|text]
gander attention recompute-heuristics [--format json|text]
```

Attention assignments are durable file or inclusive diff-line regions. Human
set/promote/demote assignments outrank agent curation, which outranks generated,
lockfile, custom-generated, and ignore-policy heuristics; ordinary content stays
implicit `supporting` and is not persisted. Agent `spotlight` walkthrough steps
map to `spotlight`, while `glance` maps to `skim`. `list --mode assigned` returns
raw durable records with a `stale` flag. The default effective list returns one
entry for each current file plus deterministic disjoint spans partitioned at
every current assignment boundary, and an envelope with
`"default_salience": "supporting"`.

Targets reuse the existing diff anchor and fingerprint representation. A
fingerprint mismatch retains the assignment for later re-anchoring, marks it
stale, and excludes it from effective attention so stale `skim` never hides
changed code. `recompute-heuristics` may remove current assignments no longer
matched by policy, but never deletes fingerprint-drifted records. All mutations
write only Gander review state.
`clear` resolves only normalized file/range identity, so it can remove a stale
human assignment for a missing file or out-of-range line. `set`, `promote`, and
`demote` require a target in the current unfiltered attention diff.

`coverage show` reports current spotlight visits and skim acknowledgements.
`skim-fold list` returns the same stable fold ids, paths/file count, churn,
rationale, current/stale state, acknowledgement state, and whole-file coverage
used by the TUI glance board. `acknowledge` has no implicit cursor: select an
exact file/range, a returned stable id, or `--all`. Only current folds record
fingerprint-guarded progress. Stale entries remain history and never acknowledge
or count. A whole-file fold also writes that file's current fingerprint to
viewed state; a partial fold never marks the file viewed. JSON acknowledgement
output includes `matched`, `acknowledged`, `already_acknowledged`, `stale`, and
`whole_files_viewed` counts/effects. Unknown ids/targets are errors. A path-only
selector is accepted when it identifies one fold; multiple partial folds on the
same path require an exact range or stable id. `--all` is the sole intentional
successful no-op: `matched: 0` means there are no current unacknowledged folds.

## Comments

```sh
gander comments list [--channel onboarding|delegation|collaboration|note] [--format json|text]
gander comments add (--path <path> [--line <n>] [--end-line <n>] | --general) --body <text> \
  [--kind note|issue|question|praise] [--action none|fix|explain|test|follow-up] \
  [--state draft|todo] [--channel onboarding|delegation|collaboration|note] [--format json|text]
gander comments reply <id> --body <text> [--resolve] [--format json|text]
gander comments resolve <id> [--reply <text>] [--format json|text]
gander comments set-state <id> --state draft|todo|resolved [--format json|text]
gander comments ready (<id>... | --all-drafts) [--format json|text]
gander comments edit <id> [--path <path>] [--line <n> | --start-line <n> --end-line <n>] \
  [--body <text>] [--kind note|issue|question|praise] [--action none|fix|explain|test|follow-up] \
  [--channel onboarding|delegation|collaboration|note] [--format json|text]
gander comments delete <id> [--format json|text]
```

`add` creates a persisted comment. Exactly one of `--path` or `--general` is
required. General comments are session-level notes with no file location, line,
anchor, or excerpt. `--state` overrides the configured initial state for this
comment. `--channel` overrides `[comments].default-channel`; without either,
CLI additions retain the legacy state-derived default (todo → delegation,
draft → note). A non-actionable onboarding/note selection is stored as a draft
rather than accidentally creating a publishable todo. `edit --channel` is the
scriptable equivalent of the TUI editor's channel cycle. `--end-line` requires
`--line`; `edit --line` conflicts with `--start-line`, and `edit --end-line`
needs either a supplied start or an existing anchored start. End lines must be greater than or equal to their start. `--line`, `--start-line`, and `--end-line`
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
- `todo`: ready/actionable. Delegation-channel todos ask an agent to address
  them regardless of `kind` or `action`; collaboration-channel todos are open
  team feedback eligible for `--profile team` export.
- `resolved`: retained history. Resolved comments are not selected for prompt
  handoff or delegation by default, but remain in full exports and threads.

New comments default to `[comments].initial-state`, which is `todo`; set it to
`draft` to save new feedback privately until it is readied:

```toml
[comments]
initial-state = "draft" # todo (default) | draft
# default-channel = "note" # optional fixed default; otherwise the TUI infers
```

The TUI infers new-comment channels in this order: existing thread, agent
onboarding target, actually attached agent on the reviewer's own reviewed
range, a consistent foreign jj author across `base..rev`, then private note.
Ownership matching compares `[identity].name` and the optional
`[identity].email` against the range's consistent jj author name/email,
trimmed and case-insensitively; either field matching counts the work as your
own, so benign display-name drift does not misclassify it as a teammate's.
Mixed/empty/ambiguous range authorship stays private, as does a range where
neither identity pair is comparable. Merely configuring an
agent command is not attachment. `[comments].default-channel` pins the initial
choice (thread replies still preserve their thread), and Tab cycles all four
channels while composing.

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
  "author": { "kind": "human", "name": "local" },
  "channel": "delegation",
  "created_at": "2026-07-04T22:19:06.819614Z"
}
```

`comments list` returns `{ "comments": [...] }`. Pass `--channel delegation`
(or `onboarding`, `collaboration`, `note`) to filter through the shared review
query; omitting it preserves the full active-session list. JSON and text output
retain each comment's `author` and `channel`. `comments ready <id>...` marks
the selected active-session drafts `todo`; `comments ready --all-drafts` marks
all active-session *human-authored* drafts `todo`. Agent-authored drafts are
onboarding suggestions awaiting your triage, so the bulk sweep leaves them
untouched and reports `skipped N agent draft(s) awaiting triage` instead of
escalating them past accept-time channel inference; selecting an agent draft
explicitly by id/prefix still readies it as a delegation todo. The operation is
atomic: if any supplied id is
unknown, ambiguous, not in the active session, or already resolved, no comments
are changed. `reply` appends an immutable
UUID-addressed reply with a timestamp and updates the parent comment's
`updated_at`; `--resolve` also marks the parent resolved. `resolve` is a
convenience for `set-state --state resolved`, and `--reply` first appends the
given reply before resolving. New CLI comments freeze an `observation` from the
diff already loaded for the command. New replies embed a current `result`
snapshot, reference the original aggregate when available, classify the path as
`same_path`, `renamed_from`, or `not_in_diff`, and compare portable patch
fingerprints. Plain resolve creates no synthetic reply/result. Legacy records
remain null/missing, and target labels or fingerprint equality are evidence—not
proof that an outcome was implemented or verified. Capture performs no extra jj
query and never snapshots or mutates the working copy.

## Action items

```sh
gander action-items list [--format json|text]
gander action-items show <id> [--format json|text]
gander action-items add --title <title> [--body <text>] [--action none|fix|explain|test|follow-up] \
  [--comment <comment-id>]... [--ticket <ref>]... [--path <path>] [--line <n>] [--format json|text]
gander action-items edit <id> [--title <title>] [--body <text>] [--action none|fix|explain|test|follow-up] \
  [--path <path>] [--line <n>] [--format json|text]
gander action-items link-comment <id> --comment <comment-id> [--format json|text]
gander action-items unlink-comment <id> --comment <comment-id> [--format json|text]
gander action-items add-ticket <id> --ticket <ref> [--format json|text]
gander action-items remove-ticket <id> --ticket <ref> [--format json|text]
gander action-items close <id> --disposition completed|dismissed|deferred [--outcome <resolution>] [--format json|text]
gander action-items reopen <id> [--format json|text]
gander action-items delete <id> [--format json|text]
```

Todo comments are Gander's primary implicit feedback: a `todo` comment is an
open action request by itself. Ordinary comments (`draft` notes and resolved
history) do not create action items. Durable action items are optional,
higher-level coordination records for grouping many comments, tracking work that
spans files, or carrying external ticket references; they are not required for
simple line-level feedback. Linked todo comments are folded into their parent
action item as evidence and are not duplicated as separate handoff bullets.
Unlinked todo comments continue to appear as independent action items in list and
handoff output.

Action item ids accept unambiguous prefixes; unknown or ambiguous prefixes are
one-line errors. `--comment` is repeatable on `add`; `link-comment` and
`unlink-comment` add/remove individual evidence comments later. `--ticket` is
repeatable on `add`, adds opaque external ticket references (for example a Linear
or SourceHut issue URL/id), and never fetches from or posts to that system.
`close --disposition deferred` requires at least one ticket reference to make the
deferral actionable outside Gander. `completed` means the local work was done;
`dismissed` means no work is needed; `deferred` means the item was moved to an
external tracker. Add that reference first with `action-items add-ticket`; close
does not create a ticket reference implicitly.

`--line` is a 1-indexed diff line anchor in the current jj diff and requires
`--path` when adding an action item. Editing patches the existing target:
path-only preserves an existing line, line-only preserves an existing file, and
line-only is rejected if the action item has no target file. New side is
preferred, old side only for removed-only lines. `--action` emits `follow-up`;
legacy JSON input spelled `followup` is still accepted.

Example `action-items list`:

```json
{
  "action_items": [
    {
      "id": "57e87a50-e83a-4778-9cd6-b154d1a2caa5",
      "title": "Update intro",
      "body": null,
      "target": { "file": "README.md", "line": 1, "end_line": null, "symbol": null, "revset": null, "base": null, "revision": null, "repo": null },
      "action": "fix",
      "status": "open",
      "linked_comment_ids": ["comment-id"],
      "external_tickets": [],
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

Walkthroughs are the durable source of truth for normal-stream curation. Steps have a title,
importance (`spotlight` joins ordered Focus navigation; `glance` contributes Skim attention), optional
why/body/artifacts, an optional change id, and an optional stable target. `--line`
and `--end-line` are 1-indexed diff line anchors: new side (post-image)
preferred, with old-side fallback for removed-only lines in the current jj diff.
Chapters introduce stack changes and use `summary` as their narrative.
CLI-created/replaced steps are stamped with the configured human identity.
MCP-created steps are stamped with the configured agent identity; legacy steps
without `author` remain neutral and are never inferred to be agent-authored.
Walkthrough JSON targets accept the optional existing `anchor` object; its
path/line/range must agree with the target coordinates. A supplied valid anchor
is preserved rather than silently refreshed.
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
receive new UUIDs. Duplicate omitted identities consume prior matches one-to-one
in prior order, while explicit ids reserve their prior slots before omitted
matching regardless of incoming order. Duplicate explicit ids or any duplicate
final ids after preservation/generation reject the whole replacement without a
state write. CLI and MCP replacement use the same normalization service,
including exact current-stack validation for chapter change ids. The command
warns about unknown JSON fields and targets that
cannot anchor in the current diff; those durable targets remain stale until a
matching fingerprint can re-anchor. Chapter steps must include a non-empty
`change_id`.
`show` emits JSON; `export` emits Markdown.

File-anchor commands accept both `--path` and `--file` for compatibility. The
canonical form in docs and JSON remains `--path`.

## Drafts

## Tour compatibility over the review stream

```sh
gander tui --tour
gander tour render [--width 100] [--height 30] [--slide N]
```

`tui --tour` starts Focus at the first current durable Spotlight in the normal
review stream. It errors non-fatally into ordinary review when no current
Spotlight exists; there is no changed-file fallback or separate modal surface.
`tour render` uses the same normal-stream ratatui draw path with a test backend and prints the
slides as plain text separated by `──── slide K/N ────`, which is useful for
agents and documentation snapshots. Current Spotlight regions become slides in
durable walkthrough order; chapter headers and inline narration remain ordinary
stream rows/cards. Focus keeps comments, search, folds, retargeting, and other
normal review actions available.

```sh
gander drafts list|add [--file <spec>]|remove --id <id-or-unique-prefix>
```

Drafts remain a supported CLI surface, now backed by durable agent-authored
comments (`state=draft`, `channel=onboarding`) rather than an overlay bucket.
Specs are JSON files or stdin, and a live TUI on the same workspace picks up
review-state writes within a poll. Legacy chunk and change-brief commands/tools
are removed. Use `walkthrough set|add-step|show` plus
`attention set|seed-heuristics|list` for durable curation.

## Live state and the TUI

Review-state writes are safe while a TUI holds the session: the TUI watches
the state file and merges external changes (a CLI-added comment appears in
the running TUI within a poll and survives the TUI's save/quit). Deletions
made in the TUI are not resurrected by merges. Same-id comments use the newer
`updated_at` value for body/state metadata and union append-only replies by
reply id, enriching a missing same-ID reply result with deterministic conflict
handling, so an external reply or resolution is not overwritten by a later TUI
save. Action items, walkthroughs, walkthrough steps, and sessions likewise use newer
`updated_at` values for same-id conflicts. The attention map is session-level
last-writer-wins during external-state merge; region identity and effective
resolution are deterministic within the winning map.

## Local web peer (Phase 1)

```sh
gander web [--port <port>] [--no-open]
```

`web` starts a standalone live instance on `127.0.0.1`. Port `0` (the
default) asks the OS for a free port; `--port` pins it. The first stdout line is
the usable URL including an ephemeral capability token. Gander currently does
not auto-open a browser: open that URL manually, or pass `--no-open` to make
the scripted intent explicit and suppress the explanatory stderr note.

Every HTTP request—including `/events` and embedded assets—must carry the
token query parameter, the exact printed `Host`, and either no `Origin` or the
exact printed origin. The token is never stored. Phase 1 serves an embedded
server-rendered lifecycle shell and SSE readiness notice; stream UI, browser
presentation, and review action endpoints are later M16 phases. The process
registers and hosts ACP exactly like the TUI, and SIGINT/SIGTERM gracefully
remove its registry entry and Unix socket.

### Debugging TUI responsiveness

Set `GANDER_FRAME_LOG=<path>` before launching `gander tui` to append one
line per handled input batch: `handle_us=<n> draw_us=<n>` (event dispatch
through autosave, then the terminal draw that rendered the result). The log
is opt-in and has zero overhead when the variable is unset. Use it to spot
interactive-latency regressions on large reviews, e.g.
`GANDER_FRAME_LOG=/tmp/frames.log gander tui`.

## Export/import and state utilities

```sh
gander handoff [--mode prompt|delegate] [--format markdown|json] [--output <path>] [--copy]
gander export [json|markdown|html] [--profile human|agent|team] [--output <path>]
gander import <json-artifact>
gander mark-viewed
gander mark-generated-viewed
gander paths
gander summary
```

`gander paths` also reports the web bind convention and confirms that the web
token is ephemeral; it never prints a current token.

`handoff` is the one-shot actionable prompt for an implementer agent. Markdown
defaults to action items first, walkthrough next, then reference hunks limited
to files that carry action items or walkthrough stops. JSON uses the same
default action-item selection: open durable action items plus unlinked `todo`
comments only. Todo comments linked to an open action item are folded into that
item as evidence and are not duplicated. Drafts are saved/private/withheld,
ordinary comments do not become action items, and resolved comments are history.
Action items are deterministically ordered the same way in both formats: action
priority (fix > test > follow-up > other), then path, then line. `handoff
--format json` is a stable action artifact shaped
as `{ "session", "action_items", "walkthrough", "reference" }`: action items
are action-item/comment objects with `id`, `source`, `kind`/`action`, `path`, `line`,
`end_line`, `excerpt`, `body`, `state`, canonical linked comment ids, and external ticket refs.
`--output` writes without stdout body output; `--copy` copies it to the clipboard (pbcopy,
wl-copy, xclip, or OSC52 via `/dev/tty`). Use `export --profile agent` instead
when you need the full session artifact for archive/reference or broad
automation: full exports include all comments (`draft`, `todo`, and
`resolved`), all action items, walkthroughs, replies, and excerpts when available (its
H1 is `# Review session export (agent profile)`; the two artifacts
cross-reference each other). `export --profile team` includes only collaboration
`todo`/`resolved` comments and session disposition for team review handoff.
Import requires the artifact's base/revision to exactly match the currently
loaded target; on match it remaps imported comments into the active local session
while preserving foreign authors, channels, replies, and identities. Exported
action items, walkthroughs, and private attention assignments are not restored
by `gander import`.

Humans read the durable review session directly in the TUI (and future web UI).
Prompt handoff and delegate mode are outbound adapters for transferring work to
an external agent or harness. Delegate mode emits an independently versioned
`gander_delegation` packet for typed orchestration; it selects open work without
mutating review state or executing verification text:

```sh
gander handoff --mode delegate \
  --action-item <action-item-prefix> --include-comment <comment-prefix> \
  --to implementation-agent \
  --objective "Fix the parser finding and add coverage." \
  --constraint "Preserve the public API." \
  --accept "The regression test fails before the fix and passes after it." \
  --verify "nix run .#ci-test" \
  --format json
```

Selectors accept full ids or unambiguous prefixes. With no selectors, delegate
mode includes open durable action items and actionable unlinked `todo` comments,
folds linked todo evidence into its parent action item, and excludes resolved
comments and closed action items. Draft comments are rejected when explicitly selected for delegation unless
they are first readied, and implicit delegation never selects them. Packets include source fingerprints, walkthrough context, relevant
hunks, reply history, and concrete `comments resolve --reply` / `action-items close
--disposition completed --outcome` return commands. `--verify` is inert requested text; Gander never
executes it.

`export html` writes a self-contained static review page. JSON is the canonical
machine-readable artifact format; `--profile agent` adds all raw hunks and
comment excerpts for tools, and `--profile team --format json` is the canonical
forge-mappable team contract. Team Markdown/HTML are filtered human summaries
over the same public collaboration projection, not machine-readable forge
mapping formats.

By default, `gander tui` saves durable review state on quit without dumping a
Markdown artifact to stdout. It prints a short stderr reminder to run
`gander export markdown` or `gander handoff` when you want a dump. The artifact
mechanism remains opt-in: set `[artifact] on-tui-quit = "stdout"` (or pass
`gander tui --artifact-on-quit stdout`) for the old stdout dump, or use
`"write"` with `output-dir`/`--artifact-output` to write a file.

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
reply, resolution, and action-item closure evidence in Gander.

## MCP and ACP

```sh
gander mcp
gander acp
```

Normal automation should use the CLI directly. `mcp` is optional typed/live harness integration, including CLI-parity state tools. `acp` is a low-level/internal line-delimited JSON-RPC bridge for debugging live-session curation: it
bridges to a running TUI or web peer on the same workspace when one exists
(announcing `bridged to live session` vs `serving snapshot` on stderr, and a `mode`
field in the `initialize` response). See `docs/acp.md`.

## Automation without MCP

For low-context agent loops, use the CLI and `jq` directly:

```sh
state="$TMPDIR/gander-review.json"
rm -f "$state"
review_id=$(gander --state-file "$state" reviews create --title "Agent pass" | jq -r .id)
comment_id=$(gander --state-file "$state" comments add --path README.md --line 1 \
  --state todo --kind issue --action fix --body "Clarify the introduction." | jq -r .id)
item_id=$(gander --state-file "$state" action-items add --title "Fix intro" \
  --action fix --comment "$comment_id" --path README.md --line 1 | jq -r .id)
gander --state-file "$state" action-items list | jq -r '.action_items[] | select(.status == "open") | .id' |
  while read -r id; do
    gander --state-file "$state" action-items close "$id" --disposition completed --outcome "Handled by agent"
  done
gander --state-file "$state" reviews show "$review_id" | jq '{id, title, action_items}'
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
gander present start                   # Focus the first current Spotlight
gander present next                    # advance one Spotlight
gander present goto --index 3          # zero-based Spotlight index
gander present goto --step step-id     # durable walkthrough step id
gander present focus --path src/foo.rs --line 42 --end-line 60 --note "look here"
gander present reload                  # reload state and re-anchor presentation
```

The command requires a live instance (`gander tui` or `gander web`). The TUI
respects modal safety: if the human is typing a comment or using a popup, it returns
`user is busy: <mode>` instead of moving the view. `present start` applies the
Focus preset and drives the same durable Spotlight ordering as Alt-N/Alt-P in the
normal stream. Explicit `present end` restores the prior view when presentation
owned Focus. Retarget and refresh preserve presentation/Focus safely and
re-resolve `(step_id, part)` identity, so insertion/reordering cannot change the
current target. Removed or stale identities report stale/empty rather than
falling through to the prior numeric index.
