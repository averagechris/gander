# Changelog

## Unreleased

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
