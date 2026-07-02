# gander

Take a gander at your [`jj`](https://jj-vcs.github.io/jj/latest/) changes: a fast terminal UI for reviewing changes more ergonomically than raw `jj show`.

![gander demo: reviewing a jj change, marking files viewed, leaving a range comment, and exporting a review artifact](docs/demo.gif)

This repo is intentionally early, but the first vertical slice is in place:

- reads `jj show --git` for a target revision
- parses changed files and hunks into structured Rust data
- shows a navigable hierarchical file tree + diff pane in a Ratatui TUI
- persists per-file viewed state keyed by a content fingerprint
- auto-restores viewed state only when the file's diff is unchanged
- sorts viewed files below unviewed files and advances after marking viewed
- preserves each file's diff cursor and scroll position while navigating
- supports repeated `--ignore <glob>` filters for noisy generated files
- labels generated/noisy files in the TUI, summaries, and artifacts, and
  auto-detects generated files by header markers like `@generated`/`DO NOT EDIT`
- fuzzy file search (`/`) and viewed/unviewed file filters (`f`)
- changed-symbol outline (`o`) with `]`/`[` jumps between changed functions
- symbol-aware folding of long unchanged context runs (`z`)
- records lightweight file-level and line-level comments from the TUI
- tracks comment states (draft/todo/resolved) with a comment list pane (`C`)
- exports review artifacts as JSON or Markdown, including an agent profile
  with raw hunks and comment excerpts (`--profile agent`)
- reviews arbitrary revsets (`R`), steps through stacks change-by-change
  (`>`/`<`), and re-reviews incrementally against a prior jj operation (`I`)
- offers split/squash jj helpers that only run after explicit confirmation (`!`)
- streams diff parsing, renders lazily, and shows placeholders for binary
  files and diffs over a configurable size threshold (`L` to expand)
- hosts agent-collaborative review over ACP (`gander acp`): agents read the
  session and suggest ordering (`A`), flag critical sections (`F`), define
  review chunks (`S`), and draft comments the human triages (`D`)
- syntax-highlights common languages with a built-in tree-sitter registry
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

## Releases

Releases are cut with Nix flake apps and published to SourceHut (canonical
`vX.Y.Z` tags, a Pages downloads site, and an optional builds.sr.ht Linux
build):

```sh
nix run .#release -- --version X.Y.Z
```

See [docs/release.md](docs/release.md) for the full process and the individual
`prepare-release`, `release-tag`, `release-artifact`, `build-pages`, and
`publish-pages` stages.

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

Inside the TUI, press `b` to adjust the base or tip for a review target. The
picker lists jj change ids, associated bookmarks, and descriptions; select a row
and press Enter to reload the diff. It opens in base-selection mode for the
common `base..@` flow, and `tab` toggles between choosing the base and choosing
the tip for stacked-change review.

Press `R` to type arbitrary base/tip revsets (full jj revset syntax), and
`>` / `<` to step through the current stack (`trunk()..@`) change-by-change,
reviewing each change against its parent. Press `I` to pick a prior jj
operation for incremental re-review: files unchanged since that operation are
marked viewed, changed or new files are marked unviewed. Press `!` for
split/squash helpers; the exact `jj` command is shown and nothing runs until
you confirm it.

Hide generated/noisy files:

```sh
cargo run -- --ignore 'Cargo.lock' --ignore '**/*.lock' tui
```

You can also configure default ignores, artifact output, and keys. Config is
loaded in this order, with later files overriding earlier files field-by-field:

1. built-in defaults
2. XDG user config at `$XDG_CONFIG_HOME/gander/config.toml`, or
   `~/.config/gander/config.toml` when `XDG_CONFIG_HOME` is unset
3. shareable project config at `gander.toml`
4. ignored project-local config at `.gander/config.toml`
5. an explicit `--config <path>`, when provided

Example config:

```toml
[ignore]
globs = ["Cargo.lock", "**/*.lock", "**/generated/**"]

[jj]
binary = "jj" # default: first jj on PATH; absolute paths work well in Nix configs

[generated]
presets = ["lockfiles", "api-clients"]
globs = ["schemas/*.json"]

[limits]
max-diff-lines = 5000 # larger diffs render a placeholder until expanded with L

[artifact]
format = "markdown"
profile = "human" # human | agent (agent adds raw hunks + comment excerpts to JSON)
output_dir = ".gander"
basename = "review"
on_tui_quit = "stdout" # never | write | stdout

[syntax]
enabled = true
languages = [
  "bash",
  "css",
  "go",
  "html",
  "javascript",
  "json",
  "jsx",
  "markdown",
  "nix",
  "python",
  "rust",
  "toml",
  "tsx",
  "typescript",
  "yaml",
]

[[syntax.mappings]]
name = "python"
extensions = ["custompy"]
filenames = ["SConstruct"]

[syntax.theme]
keyword = "magenta bold"
function = "blue"
string = "green"
comment = "dark-gray"
type = "yellow"

[keybindings]
move-down = ["j", "down"]
move-up = ["k", "up"]
toggle-focus = ["tab"]
compare-trunk = ["t"]
compare-parent = ["p"]
target-chooser = ["b"]
revset-input = ["R"]
stack-next = [">"]
stack-previous = ["<"]
operation-picker = ["I"]
jj-helpers = ["!"]
toggle-large-diff = ["L"]
toggle-agent-order = ["A"]
flag-list = ["F"]
chunk-list = ["S"]
draft-list = ["D"]
target-picker-down = ["down", "ctrl-j"]
target-picker-up = ["up", "ctrl-k"]
toggle-generated = ["h"]
cycle-viewed-filter = ["f"]
toggle-fold = ["space"]
collapse-fold = ["left"]
expand-fold = ["right"]
toggle-context-fold = ["z"]
file-search = ["/"]
symbol-outline = ["o"]
next-symbol = ["]"]
previous-symbol = ["["]
comment = ["c"]
edit-comment = ["e"]
delete-comment = ["x"]
comment-list = ["C"]
insert-newline = ["enter"]
submit-comment = ["ctrl-s"]
quit = ["q"]
```

CLI `--ignore` values are appended to configured ignore globs. Use
`--config <path>` to load a specific config file.

`[jj].binary` controls which `jj` executable is used. The default is `"jj"`,
which resolves through `$PATH`. Set it to an absolute path when you want a
specific binary, for example from a Nix store path or a project-local wrapper.
If the configured binary is missing, the app falls back to `jj` on `$PATH`; if no
usable `jj` can be found, startup fails with an actionable install/config hint.
The startup probe closes stdin and times out, which guards against accidentally
selecting non-Jujutsu packages such as `nixpkgs#jj`; in Nix configs, prefer
`nixpkgs#jujutsu`.

Generated/noisy presets are opt-in and can be configured with `[generated]` or
CLI flags. Matched files remain reviewable by default, but the TUI groups them
under a `generated/noisy` section and marks them with a `gen` badge. Press `h`
to hide or show that section while reviewing:

```sh
cargo run -- --generated-preset lockfiles mark-generated-viewed
cargo run -- --generated-glob 'schemas/*.json' mark-generated-viewed
```

Available generated presets are `lockfiles`, `api-clients`, and
`vendored-assets`. `mark-generated-viewed` intentionally runs before ignore
filtering so hidden generated files can still have their viewed state updated.

Syntax highlighting is enabled by default for built-in grammars: Bash/Shell,
CSS, Go, HTML, JavaScript/JSX, JSON, Markdown, Nix, Python, Rust, TOML,
TypeScript/TSX, and YAML. Use `[syntax].languages` as an allow-list, set
`[syntax].enabled = false` to disable highlighting, or add `[[syntax.mappings]]`
entries to map extra extensions/filenames to an existing built-in grammar.
Highlight theme values are simple space-separated style specs: a foreground
color such as `red`, `green`, `yellow`, `blue`, `magenta`, `cyan`, `gray`, or
`dark-gray`, plus optional modifiers like `bold`, `italic`, `dim`, and
`underline`.

Built-in language detection:

| Language | Extensions / filenames |
| --- | --- |
| Bash/Shell | `.bash`, `.bats`, `.sh`, `.zsh`, `.bashrc`, `.envrc`, `.profile`, `.zshrc` |
| CSS | `.css` |
| Go | `.go` |
| HTML | `.htm`, `.html` |
| JavaScript | `.cjs`, `.js`, `.mjs` |
| JSX | `.jsx` |
| JSON | `.json`, `.jsonc` |
| Markdown | `.markdown`, `.md`, `.mdown`, `.mkd` |
| Nix | `.nix` |
| Python | `.py`, `.pyi`, `.pyw` |
| Rust | `.rs` |
| TOML | `.toml` |
| TypeScript | `.cts`, `.mts`, `.ts` |
| TSX | `.tsx` |
| YAML | `.yaml`, `.yml` |

Syntax highlights are cached by file path, diff fingerprint, and syntax matching
config. Unsupported files remain plain text, and supported-language highlight
failures fall back to unhighlighted text without interrupting review.

This repo's `.gitignore` excludes `.gander/`, so you can keep
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
cargo run -- export json --profile agent # adds raw hunks + comment excerpts
cargo run -- export # uses configured artifact defaults
cargo run -- tui > review.md # TUI on stderr, Markdown artifact on stdout after quit
```

Import comments/viewed state from a JSON artifact:

```sh
cargo run -- import review.json
```

Import is conservative: comments with duplicate IDs are skipped, and viewed
state is restored only when a file path and diff fingerprint still match the
current review target.

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
.gander/state.json
```

Use `--state <path>` to override it.
State stores the last reviewed base/revision metadata alongside viewed files and
comments so future resume/import behavior can detect target mismatches safely.
The TUI autosaves state whenever viewed marks or comments change, so a crash or
killed terminal does not lose review progress.

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
| `/` | fuzzy file search popup |
| `f` | cycle viewed filter: all → unviewed only → viewed only |
| Space | fold / unfold selected directory or selected file's parent directory |
| Left / Right | collapse / expand selected directory |
| `t` / `p` | compare `trunk()..@` / `@-..@` |
| `b` | base/tip target chooser popup |
| `R` | free-form base/tip revset input popup |
| `>` / `<` | step to the next / previous change in the stack (`trunk()..@`) |
| `I` | prior-operation picker for incremental re-review |
| `!` | jj split/squash helpers (runs only after confirmation) |
| `L` | render/hide a diff that exceeds the large-diff threshold |
| `A` | toggle agent-suggested review ordering |
| `F` | agent-flagged sections popup |
| `S` | agent review chunks popup |
| `D` | agent draft comments triage popup (accept/edit/discard) |
| `h` | hide/show generated/noisy files in the TUI |
| `z` | fold/unfold long unchanged context runs in the diff |
| `o` | changed-symbol outline popup for the selected file |
| `]` / `[` | jump to next / previous changed symbol in the diff |
| `r` in diff focus | start/cancel a range selection for a multi-line comment |
| Ctrl-G / Esc | cancel active range selection and dismiss notices |
| Enter | mark selected file viewed and advance to the next unviewed file |
| `v` | toggle selected file viewed/unviewed |
| `a` | mark all visible files viewed |
| `u` / PageUp | scroll diff up |
| `d` / PageDown | scroll diff down |
| `g` | top of diff |
| Tab | switch focus between file tree and diff |
| `c` | add a file comment in file focus, or line/range comment in diff focus |
| `e` / `x` | edit / delete the selected comment |
| `C` | comment list popup (jump, cycle draft/todo/resolved, delete) |
| Enter in comment editor | insert newline |
| Ctrl-S in comment editor | save comment |
| `q` | quit and save state |

Mouse support:

- click the file tree to focus/select files or directories
- click the diff pane to focus/select a diff line
- click-drag across diff rows to open a range comment editor

## Agent-collaborative review (ACP)

Serve the review session to agents over line-delimited JSON-RPC 2.0 on stdio:

```sh
cargo run -- acp
```

Agents can read the diff, comments, and viewed state, and write suggestions
into `.gander/agent.json`: a review ordering, flagged critical sections,
review chunks, and draft comments. A running TUI polls the overlay and
surfaces suggestions live; draft dispositions (accept/edit/discard) are
written back so agents observe the outcome. See [`docs/acp.md`](docs/acp.md)
for the method reference.

## Architecture

Current module layout:

- `jj`: shell boundary for `jj diff --from <base> --to <rev> --git --color=never --no-pager`
- `diff`: small git-unified-diff parser with file fingerprints and hunk line numbers
- `file_tree`: derived directory grouping, viewed counts, and flattened TUI rows
- `state`: persisted viewed-state and comments
- `app`: review session/domain state manipulated by UI and commands
- `tui`: Ratatui/Crossterm interface
- `syntax`: built-in language registry and tree-sitter highlighting
- `artifact`: JSON/Markdown review artifact serialization
- `agent`: shared agent-overlay schema (`.gander/agent.json`)
- `acp`: JSON-RPC stdio server for agent-collaborative review

The design goal is to keep jj interaction, parsing, review state, rendering, and artifact export separable so future work can be delegated safely.
See [`docs/jj-integration.md`](docs/jj-integration.md) for the decision to use
the `jj` CLI boundary instead of embedding `jj-lib` for now.

## Near-term roadmap

See [`docs/roadmap.md`](docs/roadmap.md) for a longer backlog. Highest-value next steps:

1. Helix-inspired external grammar/query loading for custom languages
2. full Agent Client Protocol schema compliance for `gander acp`
3. live session updates for long-running agent connections

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT license ([LICENSE-MIT](LICENSE-MIT))

at your option.

Unless you explicitly state otherwise, any contribution intentionally
submitted for inclusion in the work by you, as defined in the Apache-2.0
license, shall be dual licensed as above, without any additional terms or
conditions.
