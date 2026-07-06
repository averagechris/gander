# Changelog

## Unreleased

### Added

- Incremental chunk curation: new ACP methods `review/update_chunks`
  (upsert by id, position-preserving) and `review/remove_chunks`
  (strict remove by id), mirrored as MCP tools, alongside the existing
  full-replace `set_chunks`.
- `gander chunks` command group for scriptable curation without
  JSON-RPC: `list`, `set --file <spec|->`, `update --file <spec|->`,
  `remove --id …`, and `clear` all operate on the same overlay file a
  live TUI watches; specs are plain JSON `{ "chunks": [...] }`
  documents with optional ids, validated against the session diff with
  per-part reasons on reject.
- The TUI chunk list popup (`S`) now badges chunk parts that were
  excluded as invalid, with the exclusion reason; overlay re-applies
  after refresh run the same validation as the initial load.
- Multi-part review chunks read as one concept: spotlight stops
  cross-reference their sibling parts, and the zen glance board groups
  a multi-part chunk into a single entry listing its parts.
- Derived zen content now includes factual risk labels ("public API
  change", "touches error handling") on fallback stops and aggregated
  chapter lines.
- Help overlay (`?`) gained a task-oriented "first review loop" section
  and a cluster documenting the in-zen keys.
- Watch legibility: file tree rows badge files changed since you last
  looked (`±`) and viewed-but-changed-since-viewed files (`~`), with
  changed hunk headers marked in the diff pane; `}`/`{` jump to the
  next/previous changed hunk across files.
- Activity feed (`ctrl-a`): a bounded, timestamped feed of refresh
  events — `@ moved to <id>`, `change <id> entered range/updated/left
  range`, per-file update collapsing — with the footer notice
  summarizing each refresh batch instead of a generic message.
- The footer indicates follow mode (`following @`) whenever the review
  target contains a symbolic revset.
- Zen card titles and locations truncate width-aware instead of
  clipping against the card border; long paths keep their tails.
- Zen tours without agent curation now derive useful content from the
  diff itself: chapter cards show files-by-role, churn, tests-touched,
  and top changed symbols; fallback stops anchor on each file's largest
  hunk with touched-symbol summaries; manifests, tests, docs, and
  exports-only files route to the glance board with a rationale label.
- Agent review chunks are validated at ingestion: ACP/MCP `set_chunks`
  rejects parts that reference files outside the anchored change's diff,
  out-of-range lines, or unknown change ids; invalid parts in an overlay
  file written directly to disk are excluded with a TUI notice.
- `gander acp`/`gander mcp` warn on stderr when bridging to a live TUI
  whose review target differs from the requested flags, and
  `review/summary` now reports `active_target` and `live_session`.
- `gander handoff`: one-shot, prompt-style agent handoff of the current
  review — action items (open tasks + unresolved issue/fix comments with
  excerpts) first, walkthrough order, then full hunks as reference.
  Supports `--format markdown|json`, `--only-open`, `--output <path>`,
  and `--copy` (pbcopy/wl-copy/xclip with OSC52 fallback).
- TUI `ctrl-y` (`yank-handoff` keybinding) copies the agent handoff to
  the clipboard without leaving the review.
- Review artifacts (schema v5) now include session metadata, review
  tasks, and walkthrough steps in JSON and Markdown; v4 artifacts still
  import.
- Dogfood evaluation loop: `eval/` harness (fixture generator, scripted
  evaluator scenarios, scoring rubric, archived baseline reports) and a
  living findings/action-item tracker in `docs/dogfood.md`.

### Fixed

- **Data loss:** retargeting the TUI to a narrow or empty diff (e.g.
  stack-stepping onto an empty working-copy change) no longer rewrites
  the workspace review state with only what that target can see —
  viewed-state entries and comments outside the current diff now
  survive every save.
- Viewed marks are persisted per content fingerprint
  (`viewed_fingerprints` per path; legacy state files still load), so
  visiting a target where the same path diffs differently no longer
  clobbers the mark, and the viewed-but-changed (`~`) state survives
  refreshes durably.
- Live watch actually refreshes again: the repo-poll fingerprint
  template used `if(self, …)`, which real jj rejects, so the 2s poll
  failed silently on every tick. Persistent poll failures now surface
  a footer error (and a recovery notice) instead of freezing behind
  the `following @` indicator.
- Read-only popups (activity feed, help) no longer pause live refresh;
  popups that do pause show a "refresh paused" hint.
- `}`/`{` changed-hunk navigation cycles within and across files
  instead of sticking on the first changed hunk, and freshness badges
  no longer decay when the file-tree cursor merely passes a file —
  only when its diff is focused or it is marked viewed.
- Activity events name only the files that changed in that refresh,
  with honest churn deltas ("appeared" for new files) instead of
  re-listing every un-reviewed file with whole-file totals; timestamps
  render in local time; help documents `ctrl-a`, `}`/`{`, and the
  freshness badge legend.
- Zen chapter cards no longer repeat session-global stats on every
  chapter: derived facts are scoped to the chapter's own files
  (labelled `stops:`), or omitted when they cannot be honestly
  attributed.
- `t` returns to the target the TUI was launched with (e.g. `-b main`)
  instead of hard-coded `trunk()..@`, so stack stepping always has a
  way home.
- The target chooser ranks exact bookmark matches (then prefix
  matches) above fuzzy matches, snaps the selection cursor to the top
  match as you type, and drops scattered fuzzy matches when a better
  tier exists — typing `main` and pressing Enter selects `main`.
- The prior-operation catch-up flow (`I`) refreshes the diff before
  comparing, and its notice states exactly how many unchanged files
  were marked viewed vs changed files needing re-review.
- CLI commands exit quietly on broken pipes (e.g. `gander hunks list |
  head`) instead of printing an error and backtrace.
- Comments added via `gander comments add` now carry stable line/range
  anchors, so agent-profile exports include the documented
  `comments[].anchor` and `excerpt` context. Anchor derivation is shared
  core logic between the CLI and the TUI.
- File-anchor flags are consistent across command groups: `--path` and
  `--file` are both accepted everywhere a file anchor is taken; `--path`
  is canonical.

## v0.5.0 - 2026-07-04

### Added

- Durable review sessions now carry review tasks, walkthrough steps
  (`title`/`why`/`body` plus file/line/symbol targets), comment kinds
  (`note`/`issue`/`question`/`praise`), and action intents
  (`fix`/`explain`/`test`/`follow-up`) in the persisted review state.
- Session, comment, task, and walkthrough CLI commands: `reviews
  create/list/show`, `comments add/resolve/set-state`, `tasks
  add/complete/reopen/list`, and `walkthrough
  add-step/remove-step/move-step/show/export`, plus machine-readable
  `files list` and `hunks list/show` queries.
- Self-contained static HTML review export via `gander export html`, alongside
  the existing JSON and Markdown artifact formats.
- TUI action tags and kind badges in the comment list (`C`, then `a`/`K`), a
  review-tasks popup (`X`), and maintainer walkthrough authoring (`Y` to mark a
  step, `W` to jump/reorder/delete steps).
- MCP parity tools for durable review state: `reviews_list/show/create`,
  `comment_add/resolve/set_state`, `task_add/complete/reopen/tasks_list`, and
  `walkthrough_add_step/remove_step/move_step/show`, each matching a CLI
  equivalent.
- `nix run .#render-demo` renders docs/demo.gif from docs/demo.tape with the
  flake-built binary, and a new CI manifest (`.builds/demo.yml`) re-renders
  and commits the GIF automatically when the tape or demo fixture change.

## v0.4.1 - 2026-07-04

### Fixed

- Support mouse wheel in zen tour.

## v0.4.0 - 2026-07-04

### Added

- Zen mode (`T`/`Z`): a focused, agent-curated briefing with three
  surfaces (docs/focused-diff-ux.md §6). The **focus card** is a
  full-screen stop per spotlight chunk: only the critical lines,
  extracted and vertically centered, with the agent's multi-sentence
  `explanation` rendered beside them ("why this matters"). `tab` drops
  into the **reading view** — the normal diff with out-of-range rows
  dimmed and the full review vocabulary (comments, flags, context
  expansion, view toggles) available. After the last stop (or via `g`)
  the **glance board** lists every glance chunk *and* every file no
  chunk covers on one skimmable screen with stats and one-liners;
  `enter` jumps into the diff, `a` bulk-marks the lot viewed and
  finishes. Agents label chunks `importance=spotlight` (capped at 3–7 by
  the summon prompt, each requiring an explanation that teaches the
  change) or `importance=glance` for the mechanical rest. Without an
  agent, zen falls back to one stop per file. Retargeting the review
  ends the briefing safely.
- Change-aware walkthroughs for stacked reviews: chunks can carry a
  `change_id` anchoring them to one jj change of the stack, and zen
  retargets the review to that change's own diff (`change-..change`) as
  the tour flows through the stack — stacked-PR review, change by
  change. Ending zen returns to the target it started from. The chunk
  list (`S`) retargets the same way, and chunk/zen locations show the
  anchored change id. Agents get `stack_changes` (the `trunk()..@` stack
  with the reviewed change marked) and `change_diff` (one change against
  its parent) over both MCP and ACP (`review/stack_changes`,
  `review/change_diff`), and the summon prompt and MCP instructions
  teach the stacked-PR workflow.
- Zen chapters: the walkthrough is organized change by change instead of
  dropping the reviewer onto bare change ids. Every run of stops
  anchored to the same jj change opens with a full-screen *chapter card*
  showing the change's description, bookmarks, and live diff stats plus
  the agent's high-level *change brief* — a few sentences on what the
  change accomplishes, why it exists, and how it builds on the previous
  changes (`review/set_change_briefs` over ACP, `set_change_briefs` over
  MCP; briefs live in the shared overlay like every other suggestion).
  Every walkthrough starts with an opening chapter for its target, the
  progress strip groups stop dots by chapter (`▎` bars), and chapter
  cards mark nothing viewed. Cards show the change's full multiline
  description (headline bold, body beneath); `d` collapses the body to
  the headline. Stack queries (`stack_changes`, the target picker's
  backing data) now carry full descriptions — agents get the change's
  own words, one-line surfaces show the first line.
- Zen artifacts: agents can attach exhibits to spotlight chunks and
  change briefs (`artifacts: [{title, kind: example|output|diagram|note,
  body}]`) — a usage example of the changed API, output captured by
  exercising the code, an ASCII diagram of the new flow. Cards with
  exhibits show an `e` hint; `e` opens a modal scrollable viewer over
  the focus card (`j`/`k` scroll, `h`/`l` cycle between exhibits,
  `esc` closes). The zen footer also advertises `e` with the exhibit
  count whenever the current stop or chapter carries artifacts. The
  summon prompt and MCP instructions teach agents to
  show, not just tell.
- Live review refresh: the TUI polls jj on idle (throttled, ~2s) and,
  when the reviewed range changes — new changes landing, rewrites,
  working-copy edits — reloads the diff in place with a footer notice.
  View state (pane visibility, filters, folds, selection, viewports) is
  preserved, viewed marks and comments carry over by fingerprint, agent
  suggestions are reapplied, and an active zen walkthrough rebuilds its
  stops instead of going stale.

### Changed

- The `tour` keybinding is renamed `zen` (old configs with
  `[keybindings] tour` keep working); the default binding gains `Z`
  alongside `T`.

## v0.3.0 - 2026-07-03

### Added

- Diff visual cues that make changes obvious at a glance
  (docs/focused-diff-ux.md): word-level change highlights within modified
  line pairs (unicode word diff, similarity-thresholded so rewrites don't
  over-highlight) and subtle added/removed line background tints, both on
  by default, plus an opt-in colored gutter change bar. Configured under
  the new `[diff]` / `[diff.theme]` sections; defaults are
  GitHub-dark-inspired truecolor tints that quantize to the nearest
  indexed color on terminals without truecolor support.
- View options popup (`V`): session-only runtime toggles for the visual
  cues, the file pane, and the side-by-side view. Every toggle also has a
  bindable action under `[keybindings]`.
- Collapsible file pane (`w`): hide the file tree to give the diff the
  full terminal width. Hiding moves focus to the diff, focusing the files
  pane re-shows it (never traps), and the diff pane title carries the
  selected file path and viewed mark while the tree is hidden.
- Side-by-side diff view (`|`, or `[diff] view = "side-by-side"`):
  removed/context cells on the left, added/context on the right, with
  side-specific line numbers and word-level emphasis aligned across the
  divider. Implemented as a render-time projection over the unified rows,
  so the cursor, comments, anchors, flags, and range selection behave
  identically in both layouts. Falls back to unified on terminals
  narrower than 100 columns.
- Per-gap hunk context expansion (`+` expand by `[diff] context-step`
  (default 10), `=` expand fully, `-` re-collapse): pull in file lines
  beyond what the jj diff emitted, above, between, and below hunks.
  Content is fetched lazily via `jj file show` with real line numbers on
  the expanded rows; adjacent hunks render contiguously when a gap
  closes. Expanded context rows are not commentable in this release.
- Style specs (diff cue theme and syntax themes) now support `on <color>`
  backgrounds, indexed colors (`22`), and hex (`#rrggbb`).

### Documentation

- docs/focused-diff-ux.md: design for the focused diff UX work above and
  the direction for a future agent-guided "zen mode" walkthrough
  (roadmap milestone 10).

## v0.2.1 - 2026-07-03

### Changed

- Collapse assert shortened by the opencode example fix.

### Documentation

- Drop nonexistent --quiet flag from opencode run examples.

## v0.2.0 - 2026-07-02

### Added

- Tour mode (`T`): step through agent-suggested review chunks in order
  with the agent's rationale shown in a bottom panel; advancing marks the
  current stop's file viewed, Esc returns to free navigation. Movement and
  scroll keys keep working within a stop.
- Large-change nudge: when a review exceeds the new `[limits]`
  `nudge-diff-lines` (default 1000) or `nudge-files` (default 25)
  thresholds and no agent has organized it yet, the footer suggests
  summoning an agent (`@`) or asking the harness; re-raised when loading a
  new target. Set a threshold to 0 to disable that criterion.
- `docs/harness-setup.md`: harness setup recipes — MCP registration for
  opencode/Claude Code/Codex, the split-pane review workflow, and
  attach-to-running-server `[agent] command` examples.
- `gander mcp`: an MCP stdio server (official `rmcp` SDK) exposing the
  review session as typed tools — `review_summary`, `review_files`,
  `file_diff`, `comments`, `current_focus`, `set_ordering`,
  `flag_section`, `set_chunks`, `draft_comment`, `list_reviews`. Tool
  calls route to the workspace's live TUI instance by cwd, with a
  snapshot fallback when no TUI is running.
- Per-instance ACP sockets (`acp-<pid>.sock`) plus a shared instance
  registry (workspace root, target, summary, socket, pid,
  `last_input_at`; heartbeats on input, cleaned up on exit). A second TUI
  on the same workspace now gets its own endpoint, `gander acp` routes to
  the most recently touched live instance for the workspace, and the new
  `review/current_focus` method reports what the human is looking at.
- `gander paths`: prints every resolved state/runtime/config location for
  the current workspace.

### Changed

- **Runtime state moved out of project directories.** Review state, the
  agent overlay, the live ACP socket, and agent logs now live in
  per-workspace directories under the XDG state dir
  (`~/.local/state/gander/<workspace-key>/`; `XDG_STATE_HOME` respected),
  with sockets/logs preferring `XDG_RUNTIME_DIR` when set. Legacy
  `.gander/` state is migrated automatically (one release of read
  fallback), and the new `gander paths` command prints every resolved
  location. The `.gander/config.toml` layer is deprecated (still loads,
  with a warning) — use a committed `gander.toml` or the XDG user config.
- Artifacts default to stdout: `gander export` and
  `--artifact-on-quit write` only write files for explicit output paths or
  a configured `[artifact] output-dir` (previously `.gander/review.*`).
- CI (flake check, fmt, clippy, tests) now runs on builds.sr.ht for every
  push via `.builds/ci.yml`; the Linux release manifest moved to
  `builds/release-linux-x86_64.yml` so artifacts and the downloads page are
  only built and published for explicit releases.

## v0.1.0 - 2026-07-01

### Added

- First vertical slice of the jj review TUI: parses `jj show --git` output
  into structured files and hunks with a navigable file tree and diff pane.
- Durable per-file viewed state keyed by a content fingerprint, restored only
  when the file's diff is unchanged.
- File-level and line-level review comments recorded from the TUI.
- Review artifact export as JSON or Markdown.
- Repeated `--ignore <glob>` filters with generated/noisy file labeling.
- Syntax highlighting via a built-in tree-sitter language registry.
- Nix flake packaging, dev shell, and `jj lint` local verification suite.
- Release tooling: `prepare-release`, `release-tag`, `release-artifact`,
  `build-pages`, `publish-pages`, and a `release` orchestrator, plus a
  SourceHut Pages downloads site.
