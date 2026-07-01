# jj-change-viewer

A fast Rust terminal UI for reviewing [`jj`](https://jj-vcs.github.io/jj/latest/) changes more ergonomically than raw `jj show`.

This repo is intentionally early, but the first vertical slice is in place:

- reads `jj show --git` for a target revision
- parses changed files and hunks into structured Rust data
- shows a navigable hierarchical file tree + diff pane in a Ratatui TUI
- persists per-file viewed state keyed by a content fingerprint
- auto-restores viewed state only when the file's diff is unchanged
- sorts viewed files below unviewed files and advances after marking viewed
- preserves each file's diff cursor and scroll position while navigating
- supports repeated `--ignore <glob>` filters for noisy generated files
- labels generated/noisy files in the TUI, summaries, and artifacts
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

Launch the TUI for the current stack compared to `trunk()`:

```sh
cargo run -- tui
```

Review another jj revision or compare against a different base:

```sh
cargo run -- --rev @ tui                  # default base is trunk()
cargo run -- --base @- --rev @ tui        # equivalent to the old jj-show style parent diff
cargo run -- --base 'trunk()' --rev @ tui # explicit stack-vs-trunk diff
```

Hide generated/noisy files:

```sh
cargo run -- --ignore 'Cargo.lock' --ignore '**/*.lock' tui
```

You can also configure default ignores, artifact output, and keys. Config is
loaded in this order, with later files overriding earlier files field-by-field:

1. built-in defaults
2. XDG user config at `$XDG_CONFIG_HOME/jj-change-viewer/config.toml`, or
   `~/.config/jj-change-viewer/config.toml` when `XDG_CONFIG_HOME` is unset
3. shareable project config at `jj-change-viewer.toml`
4. ignored project-local config at `.jj-change-viewer/config.toml`
5. an explicit `--config <path>`, when provided

Example config:

```toml
[ignore]
globs = ["Cargo.lock", "**/*.lock", "**/generated/**"]

[generated]
presets = ["lockfiles", "api-clients"]
globs = ["schemas/*.json"]

[artifact]
format = "markdown"
output_dir = ".jj-change-viewer"
basename = "review"
on_tui_quit = "stdout" # never | write | stdout

[keybindings]
move-down = ["j", "down"]
move-up = ["k", "up"]
toggle-focus = ["tab"]
compare-trunk = ["t"]
compare-parent = ["p"]
toggle-fold = ["space"]
collapse-fold = ["left"]
expand-fold = ["right"]
comment = ["c"]
insert-newline = ["enter"]
submit-comment = ["ctrl-s"]
quit = ["q", "esc"]
```

CLI `--ignore` values are appended to configured ignore globs. Use
`--config <path>` to load a specific config file.

Generated/noisy presets are opt-in and can be configured with `[generated]` or
CLI flags:

```sh
cargo run -- --generated-preset lockfiles mark-generated-viewed
cargo run -- --generated-glob 'schemas/*.json' mark-generated-viewed
```

Available generated presets are `lockfiles`, `api-clients`, and
`vendored-assets`. `mark-generated-viewed` intentionally runs before ignore
filtering so hidden generated files can still have their viewed state updated.

This repo's `.gitignore` excludes `.jj-change-viewer/`, so you can keep
personal project-local keybindings there without committing them. For example,
Colemak Mod-DH-friendly vertical movement can use:

```toml
[keybindings]
move-down = ["n", "down"]
move-up = ["e", "up"]
next-unviewed = ["]"]
previous-unviewed = ["["]
```

Print a non-interactive summary:

```sh
cargo run -- summary
```

Mark all visible files as viewed:

```sh
cargo run -- --ignore 'Cargo.lock' mark-viewed
```

Mark generated/noisy files as viewed:

```sh
cargo run -- --generated-preset lockfiles mark-generated-viewed
```

Export artifacts:

```sh
cargo run -- export json --output review.json
cargo run -- export markdown --output review.md
cargo run -- export # uses configured artifact defaults
cargo run -- tui > review.md # TUI on stderr, Markdown artifact on stdout after quit
```

By default, TUI artifact emission prints Markdown to stdout after the alternate
screen is restored. The TUI itself renders to stderr, so stdout redirection
captures only the artifact:

```sh
cargo run -- tui > review.md
```

Set `[artifact].on_tui_quit = "never"` to disable this default, or `"write"` to
write the artifact to the configured artifact path instead. `write` is mainly
useful when you want a config-driven save without shell redirection.

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
| Space | fold / unfold selected directory or selected file's parent directory |
| Left / Right | collapse / expand selected directory |
| `t` / `p` | compare `trunk()..@` / `@-..@` |
| `r` in diff focus | start/cancel a range selection for a multi-line comment |
| Ctrl-G | cancel active range selection |
| Enter | mark selected file viewed and advance to the next unviewed file |
| `v` | toggle selected file viewed/unviewed |
| `a` | mark all visible files viewed |
| `u` / PageUp | scroll diff up |
| `d` / PageDown | scroll diff down |
| `g` | top of diff |
| Tab | switch focus between file tree and diff |
| `c` | add a file comment in file focus, or line/range comment in diff focus |
| Enter in comment editor | insert newline |
| Ctrl-S in comment editor | save comment |
| `q` / Esc | quit and save state |

Mouse support:

- click the file tree to focus/select files or directories
- click the diff pane to focus/select a diff line
- click-drag across diff rows to open a range comment editor

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
2. better jj revision/range semantics and support for reviewing stacks
3. snapshot tests for parser, artifact, and TUI rendering
