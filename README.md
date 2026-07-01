# jj-change-viewer

A fast Rust terminal UI for reviewing [`jj`](https://jj-vcs.github.io/jj/latest/) changes more ergonomically than raw `jj show`.

This repo is intentionally early, but the first vertical slice is in place:

- reads `jj show --git` for a target revision
- parses changed files and hunks into structured Rust data
- shows a navigable hierarchical file tree + diff pane in a Ratatui TUI
- persists per-file viewed state keyed by a content fingerprint
- auto-restores viewed state only when the file's diff is unchanged
- preserves each file's diff cursor and scroll position while navigating
- supports repeated `--ignore <glob>` filters for noisy generated files
- records lightweight file-level and line-level comments from the TUI
- exports review artifacts as JSON or Markdown
- includes an initial tree-sitter Rust parse hook for syntax-aware diff context
- is packaged with a Nix flake and dev shell

## Development

```sh
nix develop
jj lint
cargo test
cargo clippy --all-targets -- -D warnings
cargo fmt --check
```

`jj lint` is the canonical full local verification suite. It runs Rust formatting,
Clippy, tests, dependency/security checks, TOML checks, Alejandra Nix formatting,
Nix static analysis, spellcheck, and flake validation through the Nix dev shell so
it works even on hosts without Rust tools installed globally.

If you do not want to enter the shell:

```sh
nix shell nixpkgs#cargo nixpkgs#rustc -c cargo test
```

## Usage

Launch the TUI for the current change:

```sh
cargo run -- tui
```

Review another jj revision:

```sh
cargo run -- --rev 'trunk()..@' tui
```

Hide generated/noisy files:

```sh
cargo run -- --ignore 'Cargo.lock' --ignore '**/*.lock' tui
```

You can also configure default ignores and artifact output in
`.jj-change-viewer/config.toml`:

```toml
[ignore]
globs = ["Cargo.lock", "**/*.lock", "**/generated/**"]

[artifact]
format = "markdown"
output_dir = ".jj-change-viewer"
basename = "review"

[keybindings]
move-down = ["j", "down"]
move-up = ["k", "up"]
toggle-focus = ["tab"]
comment = ["c"]
quit = ["q", "esc"]
```

CLI `--ignore` values are appended to configured ignore globs. Use
`--config <path>` to load a specific config file.

Print a non-interactive summary:

```sh
cargo run -- summary
```

Mark all visible files as viewed:

```sh
cargo run -- --ignore 'Cargo.lock' mark-viewed
```

Export artifacts:

```sh
cargo run -- export json --output review.json
cargo run -- export markdown --output review.md
cargo run -- export # uses configured artifact defaults
```

Persistent state defaults to:

```text
.jj-change-viewer/state.json
```

Use `--state <path>` to override it.

## TUI keys

The defaults below can be overridden in the `[keybindings]` config section.
Key names support single characters plus `esc`, `enter`, `tab`, `backspace`,
arrow keys, `pageup`, `pagedown`, and `space`.

| Key | Action |
| --- | --- |
| `j` / Down | next file |
| `k` / Up | previous file |
| `n` / `N` | next / previous unviewed file |
| `m` / `M` | next / previous comment |
| Enter | mark selected file viewed |
| `v` | toggle selected file viewed |
| `a` | mark all visible files viewed |
| `u` / PageUp | scroll diff up |
| `d` / PageDown | scroll diff down |
| `g` | top of diff |
| Tab | switch focus between file tree and diff |
| `c` | add a file comment in file focus or line comment in diff focus |
| `q` / Esc | quit and save state |

## Architecture

Current module layout:

- `jj`: shell boundary for `jj show --git --color=never --no-pager -r <rev>`
- `diff`: small git-unified-diff parser with file fingerprints and hunk line numbers
- `file_tree`: derived directory grouping, viewed counts, and flattened TUI rows
- `state`: persisted viewed-state and comments
- `app`: review session/domain state manipulated by UI and commands
- `tui`: Ratatui/Crossterm interface
- `syntax`: tree-sitter integration point, currently Rust summaries only
- `artifact`: JSON/Markdown review artifact serialization

The design goal is to keep jj interaction, parsing, review state, rendering, and artifact export separable so future work can be delegated safely.

## Near-term roadmap

See [`docs/roadmap.md`](docs/roadmap.md) for a longer backlog. Highest-value next steps:

1. real syntax-highlighted diff rendering with `tree-sitter-highlight`
2. directory folding for the hierarchical file tree
3. multiline comment editor
4. better jj revision/range semantics and support for reviewing stacks
5. snapshot tests for parser, artifact, and TUI rendering
