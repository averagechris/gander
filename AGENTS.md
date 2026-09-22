# Coding Agent Guidelines for gander

gander is a fast, local-first jj (Jujutsu) review workspace: take a gander at
jj-visible changes with durable review sessions, viewed-state, comments,
walkthroughs, optional durable action items, and exportable artifacts. Rust
(edition 2024), ratatui + crossterm UI, tree-sitter syntax highlighting, and an
rmcp-based MCP server. Canonical repo:
[github.com/averagechris/gander](https://github.com/averagechris/gander).

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
pushing. The legacy SourceHut CI definition remains for the archived release
line; GitHub release automation runs only for `v*` tag pushes or a manual run
explicitly attached to that same existing tag.

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
nix run --accept-flake-config .#release -- --version X.Y.Z --check
nix run --accept-flake-config .#release -- --version X.Y.Z
```

The release apps come from the shared fleet preset
(`lib.fleet.presets.gander` from the SHA-pinned
[averagechris/fleet](https://github.com/averagechris/fleet), via the `fleet`
input). Start from a fresh empty `@` aligned with local and remote
`main`. `--check` is a nonmutating ref/version preflight only; it does not run
the `ci-*` validation apps or build the release artifact. The real release
command validates the prepared tree with the `ci-*` apps and the evaluated
release contract, verifies the reproducible artifact, then atomically publishes
the tag and `main` under a remote-ref lease. The
tag-triggered GitHub workflow builds both configured archive/checksum pairs,
publishes the GitHub release only after the complete set verifies, and then
dispatches `pages.yml` in `averagechris/averagechris.github.io` with the
configured GitHub App. Missing App credentials produce an explicit warning;
they never masquerade as a successful site dispatch.

The SourceHut release manifest is archival documentation for old SourceHut
tags only. Do not use it for new releases.

There are no release-stage bypass flags. Resume after a post-publication
failure only when the requested version, tag and peeled commit, both `main`
refs, and checkout match exactly.
