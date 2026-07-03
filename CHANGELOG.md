# Changelog

## Unreleased

### Added

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
