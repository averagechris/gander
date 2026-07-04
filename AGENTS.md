# Coding Agent Guidelines for gander

gander is a fast, local-first jj (Jujutsu) review workspace: take a gander at
jj-visible changes with durable review sessions, viewed-state, comments,
walkthroughs, action-oriented review tasks, and exportable artifacts. Rust
(edition 2024), ratatui + crossterm UI, tree-sitter syntax highlighting, and an
rmcp-based MCP server. Canonical repo:
[git.sr.ht/~averagechris/gander](https://git.sr.ht/~averagechris/gander).

Product direction lives in `docs/vision.md`. Preserve these boundaries unless
the user explicitly changes the vision:

- Gander reads code state and writes review state. Do not add direct
  GitHub/GitLab/forge fetching or posting flows for now.
- Do not mutate the user's code workspace as a side effect of review state
  operations. External harnesses/agents may edit/fetch/post; Gander should
  inspect jj-visible work and persist local review sessions.
- CLI parity is mandatory: every MCP/TUI/future-web capability should have a
  scriptable CLI equivalent over the same core business logic.
- MCP is optional, not privileged; many users prefer CLI automation to avoid
  MCP context pollution.

## Build, lint, and test

Everything goes through the Nix flake:

```bash
nix develop                # cargo devShell (rustc, clippy, fmt, cargo-* tools)
nix run .#ci-fmt           # cargo fmt --check
nix run .#ci-clippy        # cargo clippy --all-targets -- -D warnings
nix run .#ci-test          # cargo test
nix flake check --accept-flake-config
```

`jj lint` (from `.jj-lint.toml`) is the full pre-push gate: the three
`ci-*` apps above plus cargo audit / deny / machete / sort, taplo,
alejandra, statix, deadnix, typos, and `nix flake check`. Run it before
pushing. SourceHut CI (`.builds/ci.yml`) runs flake check + the same
`ci-*` apps on every push.

## Version control

- Use `jj`, not raw git. Trunk bookmark is `main` → remote `origin`.
- Conventional commit messages (`feat:`, `fix:`, `chore:`, …). These feed
  changelog generation: `prepare-release` maps commit types into the
  `Added` / `Changed` / `Fixed` sections of `CHANGELOG.md` when the
  `## Unreleased` section is empty — prefer writing `Unreleased` bullets
  by hand as you land changes.

## Releases

See [docs/release.md](docs/release.md) for the full flow. Short version:

```bash
nix run .#release -- --version X.Y.Z [--publish-pages] [--submit-linux-build]
```

The orchestrator runs `prepare-release`, `cargo check --locked`,
`jj lint`, tags and pushes `vX.Y.Z`, builds the local `release-artifact`
tarball, and builds (optionally publishes) the SourceHut Pages downloads
site. Each stage is also its own flake app (`prepare-release`,
`release-tag`, `build-pages`, `publish-pages`).

Note: the Linux release manifest intentionally lives in
`builds/release-linux-x86_64.yml` — *outside* `.builds/` — so
builds.sr.ht does not auto-submit it on push. Release artifacts and pages
are only built/published on an explicit submit (`--submit-linux-build` or
`hut builds submit`). Do not move it into `.builds/`.
