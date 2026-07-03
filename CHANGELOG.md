# Changelog

## Unreleased

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
