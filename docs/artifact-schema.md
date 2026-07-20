# Review artifact schema

Current schema version: `13`.

Artifacts are intentionally simple and serializable. JSON is the canonical tool
format; Markdown is rendered for humans. The schema is evolving toward the
durable review-session model described in docs/vision.md: comments, optional action items, and
walkthroughs are local review state that external harnesses may consume or
publish elsewhere.

## Profiles

Artifacts render in three profiles:

- `human` (default): the compact shape below, without raw diff content.
- `agent`: adds raw excerpts so tools can reason about the change without
  re-running jj:
  - a top-level `"profile": "agent"` marker
  - `files[].hunks`: the full raw hunks (header, ranges, and per-line
    kind/old_line/new_line/text)
  - `comments[].excerpt`: raw diff lines around each comment anchor
    (3 context lines on each side; file-level comments excerpt the top of
    the first hunk)
- `team`: JSON is the canonical forge-mappable team contract. Team JSON carries
  session disposition, collaboration `todo`/`resolved` comments only, comment
  and reply authorship, structured anchors/fingerprints, excerpts, and full
  hunks. Team Markdown and HTML are filtered human summaries: they preserve the
  publication boundary, authorship, channel, disposition, and concise location /
  side context, but are not machine-readable forge mapping contracts.

Select the profile with `gander export --profile agent|team`,
`gander tui --artifact-profile agent|team`, or `[artifact] profile = "agent"`
or `"team"` in config.

## JSON shape

```json
{
  "version": 13,
  "generated_at": "2026-06-30T00:00:00Z",
  "repo": "/path/to/repo",
  "base": "trunk()",
  "revision": "@",
  "profile": "agent",
  "summary": "3 files (1/3 viewed), +10/-2, 1 comments",
  "session": {
    "id": "review-session-id",
    "title": "Review handoff",
    "disposition": "request-changes"
  },
  "files": [
    {
      "path": "src/main.rs",
      "old_path": null,
      "status": "mod",
      "viewed": true,
      "generated": false,
      "additions": 5,
      "deletions": 1,
      "fingerprint": "sha256...",
      "hunks": [
        {
          "header": "@@ -39,6 +39,7 @@ fn render() {",
          "old_start": 39,
          "old_len": 6,
          "new_start": 39,
          "new_len": 7,
          "lines": [
            { "kind": "context", "old_line": 39, "new_line": 39, "text": "fn render() {" },
            { "kind": "added", "new_line": 42, "text": "    new_call();" }
          ]
        }
      ]
    }
  ],
  "comments": [
    {
      "id": "stable-ish-id",
      "session_id": "review-session-id",
      "path": "src/main.rs",
      "line": 42,
      "anchor": {
        "type": "line",
        "path": "src/main.rs",
        "old_path": null,
        "side": "new",
        "line": 42,
        "old_line": 41,
        "new_line": 42,
        "hunk_header": "@@ -39,6 +39,7 @@ fn render() {",
        "hunk_old_start": 39,
        "hunk_old_len": 6,
        "hunk_new_start": 39,
        "hunk_new_len": 7,
        "hunk_index": 0,
        "line_index": 4,
        "line_kind": "added",
        "line_text": "    new_call();",
        "line_fingerprint": "sha256...",
        "diff_fingerprint": "sha256..."
      },
      "body": "Comment body",
      "state": "todo",
      "author": { "kind": "human", "name": "Reviewer" },
      "channel": "delegation",
      "observation": {
        "snapshot": {
          "captured_at": "2026-06-30T00:00:00Z",
          "identity": {
            "session_id": "review-session-id",
            "target": {
              "revset": null,
              "base": "trunk()",
              "revision": "@",
              "repo": "/path/to/repo",
              "file": null,
              "line": null,
              "end_line": null,
              "symbol": null
            }
          },
          "scope": { "version": 1, "aggregate": "sha256..." },
          "files": [{
            "path": "src/main.rs",
            "status": "Modified",
            "diff_fingerprint": "sha256-exact...",
            "portable_patch_fingerprint": "sha256-portable..."
          }]
        },
        "anchor": {
          "type": "line",
          "path": "src/main.rs",
          "old_path": null,
          "side": "new",
          "line": 42,
          "old_line": 41,
          "new_line": 42,
          "hunk_header": "@@ -39,6 +39,7 @@ fn render() {",
          "hunk_old_start": 39,
          "hunk_old_len": 6,
          "hunk_new_start": 39,
          "hunk_new_len": 7,
          "hunk_index": 0,
          "line_index": 4,
          "line_kind": "added",
          "line_text": "    new_call();",
          "line_fingerprint": "sha256...",
          "diff_fingerprint": "sha256-exact..."
        }
      },
      "linked_action_item_ids": ["action-item-id"],
      "created_at": "2026-06-30T00:00:00Z",
      "updated_at": "2026-06-30T00:05:00Z",
      "replies": [
        {
          "id": "reply-uuid",
          "body": "Acknowledged; resolving after the fix.",
          "author": { "kind": "agent", "name": "agent" },
          "created_at": "2026-06-30T00:05:00Z",
          "result": {
            "parent_comment_id": "stable-ish-id",
            "observation_aggregate_fingerprint": "sha256...",
            "snapshot": {
              "captured_at": "2026-06-30T00:05:00Z",
              "identity": {
                "session_id": "review-session-id",
                "target": {
                  "revset": null,
                  "base": "trunk()",
                  "revision": "@",
                  "repo": "/path/to/repo",
                  "file": null,
                  "line": null,
                  "end_line": null,
                  "symbol": null
                }
              },
              "scope": { "version": 1, "aggregate": "sha256-current..." },
              "files": [{
                "path": "src/main.rs",
                "status": "Modified",
                "diff_fingerprint": "sha256-current-exact...",
                "portable_patch_fingerprint": "sha256-current-portable..."
              }]
            },
            "related": { "kind": "same_path", "path": "src/main.rs" },
            "portable_patch_changed": false
          }
        }
      ],
      "excerpt": [
        { "kind": "context", "old_line": 40, "new_line": 40, "text": "    before();" },
        { "kind": "added", "new_line": 42, "text": "    new_call();" },
        { "kind": "context", "old_line": 42, "new_line": 43, "text": "    after();" }
      ]
    }
  ],
  "action_items": [
    {
      "id": "action-item-id",
      "title": "Fix the unchecked parse path",
      "status": "open",
      "action": "fix",
      "comment_ids": ["stable-ish-id"],
      "external_tickets": [{
        "tracker": "linear",
        "reference": "LIN-123",
        "url": "https://linear.app/example/issue/LIN-123",
        "created_at": "2026-06-30T00:00:00Z",
        "updated_at": "2026-06-30T00:00:00Z"
      }],
      "target": { "file": "src/main.rs", "line": 42 }
    }
  ],
  "walkthroughs": [
    {
      "id": "walkthrough-id",
      "title": "Suggested review order",
      "steps": [
        {
          "id": "step-id",
          "author": { "kind": "agent", "name": "review-agent" },
          "kind": "step",
          "importance": "spotlight",
          "change_id": "change-id",
          "title": "Start with the renderer",
          "why": "This establishes the new data shape.",
          "body": "Confirm the serialized fields before reading callers.",
          "artifacts": [{
            "title": "Expected output",
            "kind": "output",
            "body": "rendered output"
          }],
          "target": { "file": "src/main.rs", "line": 42, "symbol": "render" }
        }
      ]
    }
  ],
  "attention_regions": [
    {
      "target": {
        "file": "src/main.rs",
        "line": 42,
        "anchor": {
          "type": "line",
          "path": "src/main.rs",
          "side": "new",
          "line": 42,
          "diff_fingerprint": "sha256..."
        }
      },
      "salience": "spotlight",
      "rationale": "The new state transition controls all callers.",
      "source": "human",
      "stale": false
    }
  ],
  "attention_progress": [{
    "target": { "members": [{ "file": "src/main.rs", "line": 42, "end_line": 42 }] },
    "kind": "spotlight-visited",
    "fingerprint": "sha256...",
    "recorded_at": "2026-07-20T00:00:00Z",
    "stale": false
  }]
}
```

Notes:

- `profile`, `files[].hunks`, and `comments[].excerpt` are omitted entirely
  in the `human` profile. The `team` profile sets `"profile": "team"`, includes
  hunks/excerpts for forge mapping, omits action items and walkthroughs, and
  filters comments to collaboration `todo`/`resolved` only.
- `attention_regions` contains durable assigned regions (not implicit
  `supporting` content) in human and agent profiles. It includes the existing
  anchor/fingerprint evidence and a derived `stale` flag. Team exports omit the
  field because attention is private review state, not collaboration data.
  Staleness is evaluated against the unfiltered attention diff, so current
  ignore-policy heuristic regions do not become stale merely because the normal
  file view filters them out.
- `attention_progress` is private append-only skim acknowledgement and
  spotlight-visit history. Its stable target identity is separate from the
  aggregate current diff fingerprint; `stale: true` records remain history but
  never count toward coverage. Team exports omit progress.
- `session` is present when the exported change matches an open durable review
  session; it includes the session `id` and optional `title`.
- `comments[].session_id` identifies the durable session that owns a newly
  created comment. Version 5 and older comments omit it; those legacy unscoped
  comments remain visible in matching-session exports for compatibility.
- `action_items` and `walkthroughs` are exported from the active durable session when
  one is present. They are empty arrays otherwise. Action-item targets and walkthrough
  step targets include local file/line/symbol coordinates when recorded.
- Todo comments are the primary implicit action feedback. A todo comment linked
  to an open durable action item is folded into that item as evidence in handoff
  output and is not duplicated as a separate action item. Ordinary comments
  (`draft` notes and resolved history) are not action items.
- Durable action items are optional higher-level coordination records. They can
  link many comments via `comment_ids` and carry structured `external_tickets`
  (`tracker`, `reference`, optional `url`, and timestamps). Open items omit
  `disposition`, `outcome`, and `closed_at`; closed items emit those fields when
  recorded. Gander records ticket references only; it does not fetch from or
  post to forges or ticket systems.
- Export is broader than import: `gander import` requires an exact base/revision
  target match, restores duplicate-safe comments into the active local session
  while preserving foreign authors/channels/replies, restores viewed files whose
  diff fingerprints still match, and applies an incoming session disposition only
  when one is present. It does not restore `action_items`, `walkthroughs`, or
  private `attention_regions` from the artifact yet.
- Line anchors are 1-indexed diff lines. New-side/post-image anchors are
  preferred; old-side coordinates are used only as a fallback for removed-only
  lines that have no new-side line.
- `comments[].path` is optional as of version 6. It and `line`, `anchor`, and
  `excerpt` are absent/null for general session comments. General comments
  intentionally have no location or excerpt and do not select unrelated hunks
  for handoff reference output. Existing version 5 anchored comments retain
  their string `path` and deserialize unchanged.
- `comments[].state` is one of `draft`, `todo`, `resolved`. `draft` means saved,
  private, and withheld from prompt/delegate handoff; `todo` means ready and
  actionable (delegation todos ask an agent to address them; collaboration todos
  are open team feedback); `resolved` is retained history. Missing values deserialize as `draft` for
  artifacts written before version 4 and for persisted state written before
  `CommentState` existed.
- `comments[].author` is an identity with lowercase `kind` (`human` or
  `agent`) and a non-empty `name`; `comments[].channel` is `onboarding`,
  `delegation`, `collaboration`, or `note`. Replies carry their own `author`
  and remain in the parent comment's channel. Todo comments may use
  `delegation` for agent-directed work or `collaboration` for open team
  feedback; collaboration drafts remain private and are excluded from team
  exports.
- During the one-release state/artifact compatibility window, a missing author
  becomes the deterministic local human identity `{ "kind": "human",
  "name": "local" }`. A missing channel derives from state (`todo` becomes
  `delegation`; draft/resolved become `note`), and legacy replies receive the
  same local identity. New agent-authored drafts use the temporary deterministic
  configured agent name, falling back to `agent` when absent.
- Full exports include all comment states. Prompt handoff and delegation select
  only open durable action items plus unlinked `todo` comments by default; draft
  comments are rejected by explicit delegate selectors unless first readied.
- `comments[].updated_at` and `comments[].replies` are emitted when present.
  Replies are append-only objects with stable UUID `id`, `body`, and
  `created_at`. Import merges same-id comments by timestamp and unions replies,
  so newer reply/state data is not dropped as a duplicate.
- `comments[].observation` is immutable evidence captured from the diff already
  loaded when the comment was created. It records capture time, durable
  session/target labels, a versioned aggregate fingerprint, per-file
  path/old-path/status, the exact existing raw-diff fingerprint, a portable
  patch fingerprint (null for binary/non-comparable patches), and the original
  anchor when one existed. General comments have no anchor but still carry a
  review-scope snapshot; that missing anchor is
  meaningful, so a later mutable location cannot turn their reply relation into
  a located comparison.
- Snapshot file `status` uses the serialized `FileStatus` variant names
  (`Added`, `Modified`, `Deleted`, `Renamed`, `Copied`, `Binary`, `Unknown`);
  binary renames/copies keep `Renamed`/`Copied` structural status while their
  portable fingerprint is null;
  `old_path` is omitted when absent, while a non-comparable
  `portable_patch_fingerprint` is explicitly null.
- `replies[].result` records the current loaded snapshot, parent comment ID,
  the original observation aggregate (null for legacy comments), path relation
  (`same_path`, `renamed_from`, or `not_in_diff`), and whether the related
  portable patch changed. Portable fingerprints hash ordered parsed line kind
  and text, excluding headers and line coordinates. They are evidence of patch
  equality, not proof that a requested outcome was implemented or verified.
- Older state/artifacts remain readable. Missing observations/results are
  rendered explicitly as unavailable; current target labels must not be treated
  as proof of what a legacy reviewer saw. Import/merge enrich missing evidence
  on same-ID replies and use a deterministic canonical-JSON tie break if two
  non-empty immutable values conflict.

## Version history

- `13`: private fingerprint-guarded skim acknowledgement and spotlight visit
  progress, including derived stale status.
- `12`: walkthrough steps may include an attributed `author` identity. Legacy
  steps omit it and remain neutral; readers must not infer agent authorship.
- `11`: human/agent artifacts include durable attention assignments and stale
  status; team artifacts exclude the private attention map.
- `10`: team JSON exports collaboration `todo`/`resolved` threads only,
  including disposition, author attribution, full hunk/excerpt data, and
  forge-mappable anchor/fingerprint metadata. Team Markdown/HTML are filtered
  human summaries over the same public projection.
- `9`: durable annotation authors/channels and reply authors.
- `8`: optional immutable comment observations and reply results add portable
  A→B patch provenance while preserving legacy comments/replies.
- `7`: `tasks` is renamed to `action_items`, comment backrefs are
  `linked_action_item_ids`, action items can link many comments and external
  ticket refs, and closed action items record `completed`, `dismissed`, or
  `deferred` disposition. Deferred items require at least one external ticket.
- `6`: comments may carry a durable `session_id`, and `path` is optional so a
  comment can be general to its session. Version 5 anchored comments remain
  readable; missing `session_id` is treated as legacy-visible compatibility
  state.
- `5`: active durable session metadata plus session `tasks` and `walkthroughs`;
  comment `updated_at` and append-only `replies` are backward-compatible fields.
- `4`: comment `state`, optional `profile` marker, agent-profile `hunks` and
  `excerpt` blocks.
- `3`: stable anchors (side, hunk header, line/diff fingerprints).

Review-state schema `8` adds append-only attention progress keyed by stable
region identity plus current aggregate diff fingerprint. Review-state schema
`7` adds optional walkthrough-step author identity; a
missing author remains neutral/unknown and is never guessed. Review-state
schema `6` adds durable attention regions and optional existing
anchor/fingerprint evidence on `ReviewTarget`. Review-state schema `5` adds session disposition and permits collaboration todo
comments for team feedback. Review-state schema `4` adds durable comment
author/channel and reply author fields. Delegation schema `5` carries author/channel on comment evidence and
reply authors. Delegation's existing top-level `fingerprints.diff` retains its v3
path-plus-exact-file-fingerprint algorithm; provenance uses the separate
versioned `observation.snapshot.scope.aggregate`. Both remain backward-readable
through serde defaults.

## Planned schema additions

- per-file ignored/collapsed metadata
