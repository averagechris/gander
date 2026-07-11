# gander

Take a gander at your [`jj`](https://jj-vcs.github.io/jj/latest/) changes: a
fast, local-first review workspace for turning jj diffs into durable, guided,
actionable review sessions for humans and agents.

Gander's product direction is documented in [docs/vision.md](docs/vision.md):
it reads jj-visible code state, writes review state, and stays forge-agnostic.
Users or external harnesses prepare the workspace and can post/export results;
Gander focuses on review sessions, comments, optional durable action items,
walkthroughs, TUI/CLI
automation, and optional MCP access over the same core logic.

![gander demo: reviewing a jj change, marking files viewed, leaving a range comment, and exporting a review artifact](docs/demo.gif)

The tour is recorded from [`docs/demo.tape`](docs/demo.tape) and rendered with
`nix run .#render-demo`. CI re-renders and commits the GIF automatically when
the tape or its fixture change (`.builds/demo.yml`), so updating the tape in
the same change as a visual feature keeps the README showing gander's current
review flow.

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
- tracks comment states (draft saved/private/withheld, todo ready/actionable,
  resolved history), kinds (note/issue/question/praise), and action tags
  (fix/explain/test/follow-up) with a comment list pane (`C`, then `s`/`a`/`K`)
- persists durable review sessions with optional higher-level action items and maintainer-authored
  walkthrough steps over the shared CLI/MCP/TUI review core
- exposes scriptable CLI groups for `reviews`, `files`, `hunks`, `comments`,
  `action-items`, and `walkthrough`; see [docs/cli.md](docs/cli.md)
- exports review artifacts as JSON, Markdown, or self-contained HTML, including an agent profile
  with raw hunks and comment excerpts (`--profile agent`)
- reviews arbitrary revsets (`R`), steps through stacks change-by-change
  (`>`/`<`), and re-reviews incrementally against a prior jj operation (`I`)
- offers split/squash jj helpers that only run after explicit confirmation (`!`)
- streams diff parsing, renders lazily, and shows placeholders for binary
  files and diffs over a configurable size threshold (`L` to expand)
- hosts agent-collaborative review through CLI-first workflows, optional MCP
  (`gander mcp`), and the lower-level ACP bridge (`gander acp`): agents read
  the session, curate walkthroughs (`W`/`T`), suggest ordering (`A`), flag
  critical sections (`F`), and draft comments the human triages (`D`)
- offers optional MCP (`gander mcp`) with live-session tools plus CLI-parity
  tools for reviews, comments, action items, and walkthroughs; see
  [docs/harness-setup.md](docs/harness-setup.md)
- syntax-highlights common languages with a built-in tree-sitter registry
- makes changes obvious at a glance: word-level change highlights, added/
  removed line background tints, and an optional gutter change bar, all
  toggleable at runtime (`V`) and configurable under `[diff]`
- hides the file tree (`w`) so the diff gets the full width when you are
  focused on the code
- offers a side-by-side removed/added view (`|`) alongside the unified
  layout, falling back to unified on narrow terminals
- soft-wraps diff lines by default; turn wrapping off in View Options or with
  `[diff] soft-wrap = false` when horizontal scrolling is preferable; split
  pairs align to the taller wrapped side, while nowrap scrolling keeps both
  gutters, line numbers, and the divider fixed
- expands hidden hunk context per gap (`+` by `[diff] context-step`, `=`
  fully, `-` re-collapses), lazily fetching file contents via `jj file show`
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
4. project-local config at `.gander/config.toml` (**deprecated**: still
   loads for one release with a warning; move it to `gander.toml` or the
   XDG user config)
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
nudge-diff-lines = 1000 # changed-line count that triggers the large-change nudge (0 disables)
nudge-files = 25 # changed-file count that triggers the large-change nudge (0 disables)

[diff]
word-highlight = true
line-background = true
gutter-bar = false
view = "unified" # unified | side-by-side; side-by-side keeps removed/added rows aligned
soft-wrap = true # default; false enables horizontal scrolling for long diff lines
context-step = 10

[artifact]
format = "markdown"
profile = "human" # human | agent (agent adds raw hunks + comment excerpts to JSON)
# output_dir = "artifacts" # unset (the default): artifacts go to stdout
basename = "review"
on_tui_quit = "stdout" # never | write | stdout

[comments]
initial-state = "todo" # todo (default) | draft; per-comment CLI --state overrides this

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
open-work = ["X"]
walkthrough-list = ["W"]
zen = ["T", "Z"]
draft-list = ["D"]
target-picker-down = ["down", "ctrl-j"]
target-picker-up = ["up", "ctrl-k"]
popup-move-down = ["j", "down"]
popup-move-up = ["k", "up"]
popup-select = ["enter"]
popup-toggle = ["space"]
popup-close = ["esc"]
popup-close-q = ["q"] # only help, View Options, and zen artifacts
toggle-generated = ["h"]
cycle-viewed-filter = ["f"]
toggle-fold = ["space"]
collapse-fold = ["left"]
expand-fold = ["right"]
toggle-context-fold = ["z"]
expand-context = ["+"]
expand-context-all = ["="]
collapse-context = ["-"]
view-options = ["V"]
toggle-word-highlight = [] # direct cue toggles default unbound; use View Options
toggle-line-background = []
toggle-gutter-bar = []
toggle-diff-wrap = []
scroll-diff-left = ["shift-left"]
scroll-diff-right = ["shift-right"]
toggle-file-pane = ["w"]
toggle-diff-view = ["|"]
file-search = ["/"]
symbol-outline = ["o"]
next-symbol = ["]"]
previous-symbol = ["["]
comment = ["c"] # opens an empty comment editor
mark-walkthrough = ["Y"]
edit-comment = ["e"]
delete-comment = ["x"]
comment-list = ["C"]
comment-list-new-general = ["n"] # press C, then this key
comment-list-ready = ["R"]
comment-list-cycle-action = ["a"]
comment-list-cycle-kind = ["K"]
draft-accept = ["enter", "a"]
draft-edit = ["e"]
draft-discard = ["x"]
walkthrough-delete = ["d"]
walkthrough-move-down = ["J"]
walkthrough-move-up = ["K"]
zen-next = ["n", "enter", "right", "space"]
zen-previous = ["p", "left"]
zen-toggle-view = ["tab"]
zen-glance = ["g"]
zen-artifact = ["e"]
zen-toggle-details = ["d"]
zen-refocus = ["."]
zen-acknowledge = ["a"]
zen-artifact-next = ["l", "right", "tab"]
zen-artifact-previous = ["h", "left"]
insert-newline = ["enter"]
submit-comment = ["ctrl-s"]
quit = ["q"]

[agent]
# Optional: a shell command that summons a review agent (press @ in the TUI,
# or set autostart). Agent-agnostic: any CLI that accepts a prompt works.
command = "opencode run"
autostart = false
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

Personal keybindings belong in the XDG user config
(`~/.config/gander/config.toml`), which applies across all repositories.
This complete Colemak Mod-DH movement override resolves every affected normal
and popup binding while leaving text-filter `j`/`k` available for typing:

```toml
[keybindings]
move-down = ["n", "down"]
move-up = ["e", "up"]
next-unviewed = ["j"]
previous-unviewed = ["J"]
edit-comment = ["alt-e"]

popup-move-down = ["n", "down"]
popup-move-up = ["e", "up"]
comment-list-new-general = ["ctrl-n"]
draft-edit = ["alt-e"]
zen-artifact = ["i"]
```

See [docs/keybindings.md](docs/keybindings.md) for the complete action/context
inventory, validation rules, aliases, modal defaults, and this example's
collision analysis.

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

Persistent state lives outside the repository, in a per-workspace
directory under the XDG state dir (`~/.local/state/gander/<workspace-key>/`
by default; `XDG_STATE_HOME` is respected). Ephemeral endpoints (the live
ACP socket, agent logs) prefer `XDG_RUNTIME_DIR` when set. Run:

```sh
gander paths
```

to print every resolved location for the current workspace. Use
`--state-file <path>` to override the state file. Legacy state in a project-local
`.gander/` directory is migrated to the new location automatically (one
release of fallback).
State stores the last reviewed base/revision metadata alongside viewed files and
comments so future resume/import behavior can detect target mismatches safely.
The TUI autosaves state whenever viewed marks or comments change, so a crash or
killed terminal does not lose review progress.

## TUI keys

The defaults below can be overridden in the `[keybindings]` config section.
Key names support single characters plus `esc`, `enter`, `tab`, `backspace`,
arrow keys, `pageup`, `pagedown`, and `space`, with `ctrl-`, `alt-`, and
`shift-` modifiers. Canonical aliases such as `escape`/`esc`, `return`/`enter`,
and `control-j`/`ctrl-j` are equivalent. Unknown keybinding fields, invalid key
syntax, and duplicate canonical assignments in an overlapping input context are
errors; the same key may be reused in disjoint modes. Press `?` in the TUI for
the full grouped keymap; the footer only shows the everyday hints.

| Key | Action |
| --- | --- |
| `?` | help overlay with the full keymap |
| `@` | summon the configured review agent (`[agent] command`) |
| Ctrl-y | copy the agent handoff to the clipboard; unchanged from prior releases, discoverable in `?`, and reports selected todo/draft counts |
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
| `X` | open work popup for action items and todo comments (jump to linked evidence, cycle state) |
| `W` | walkthrough panel (jump, reorder with `J`/`K`, delete with `d`) |
| `T` / `Z` | zen mode: focused walkthrough of authored steps (or files), marking files viewed |
| `D` | agent draft comments triage popup (accept/edit/discard) |
| `V` | View Options popup for word highlights, line backgrounds, gutter bar, soft wrap, file pane, and side-by-side view |
| `w` / `\|` | hide/show the file pane / toggle unified vs side-by-side diff view |
| `h` | hide/show generated/noisy files in the TUI |
| `z` | fold/unfold long unchanged context runs in the diff |
| `+` / `=` / `-` | expand the nearest hidden-context gap by `context-step` / fully / re-collapse it |
| `o` | changed-symbol outline popup for the selected file |
| `]` / `[` | jump to next / previous changed symbol in the diff |
| `r` in diff focus | start/cancel a range selection for a multi-line comment |
| Ctrl-G / Esc | cancel active range selection and dismiss notices |
| Enter | mark selected file viewed and advance to the next unviewed file |
| `v` | toggle selected file viewed/unviewed |
| `a` | mark all visible files viewed |
| `u` / PageUp | scroll diff up |
| `d` / PageDown | scroll diff down |
| Shift-Left / Shift-Right | scroll diff content horizontally when soft wrap is off; gutters, line numbers, and the split divider stay fixed |
| `g` | top of diff |
| Tab | switch focus between file tree and diff |
| `c` | open an empty comment editor for a file comment in file focus, or line/range comment in diff focus |
| `C`, then `n` | add a general session comment with no file location or excerpt (`comment-list-new-general`) |
| `Y` | mark the current hunk/range as a walkthrough step |
| `e` / `x` | edit / delete the selected comment |
| `C` | comment list popup (jump, `s` cycle state, `a` cycle action, `K` cycle kind, `x` delete) |
| `R` in comment list | ready all active-session draft comments as todo atomically |
| Enter in comment editor | insert newline |
| Ctrl-S in comment editor | save comment |
| `q` | quit and save state |

Mouse support:

- click the file tree to focus/select files or directories
- click the diff pane to focus/select a diff line
- click-drag across diff rows to open a range comment editor

## Agent-collaborative review

The durable session is the product surface humans read in the TUI (and future
web UI). Prompt handoff and delegation packets are outbound adapters for
transferring selected work to external agents or harnesses. The normal agent
automation path is CLI-first (`gander reviews`, `comments`, `action-items`,
`walkthrough`, `handoff`) or typed MCP (`gander mcp`) when a harness benefits
from tool schemas and live focus. `gander acp` is the lower-level
line-delimited JSON-RPC bridge used by MCP and live presentation plumbing, not
the interface most agents need to hand-author:

```sh
cargo run -- acp
```

Agents can read the diff, comments, action items, and viewed state, and write review state or
suggestions: durable walkthrough steps/chapters, a review ordering, flagged
critical sections, live overlay curation, and comments. New durable comments are
todo by default; set `[comments].initial-state = "draft"` or pass `--state draft`
to save them privately. Draft means saved/private/withheld; todo means
ready/actionable; resolved means history. Every todo comment asks an
implementation agent to address it regardless of kind or action tag, including
questions, explanations, praise, and `action=none`. Agent overlay drafts remain a
separate pending, pre-acceptance concept. A running TUI polls the persisted
state/overlay and surfaces suggestions live; overlay draft dispositions are
written back so agents observe the outcome.

New CLI, TUI, and MCP comments also freeze a compact observation of the diff
already loaded at creation. CLI/MCP replies capture a current result snapshot,
link it to the original aggregate fingerprint when available, report same-path,
rename, or no-longer-in-diff status, and compare a portable ordered
line-kind/text patch fingerprint. This adds no jj query or workspace mutation.
The evidence travels in artifacts, handoffs, delegation packets, and static
HTML; legacy records say when snapshots are unavailable. It is provenance, not
an outcome attestation: labels and unchanged fingerprints do not prove a fix or
test result.

For a one-shot prompt handoff, run `gander handoff` (or `gander handoff --copy`).
It prints an implementation prompt with todo comments as the primary implicit
feedback, plus any open durable action items for higher-level coordination.
Ordinary draft/general notes are not action items unless readied as `todo` or
linked from an action item. Walkthrough context follows, with reference hunks
limited to files that carry action items or walkthrough stops. Drafts are
withheld; resolved comments are history. Use
`gander handoff --format json` for the legacy structured action schema
(`session`, `action_items`, `walkthrough`, trailing `reference.hunks`).

For typed delegation, use the read-only outbound adapter. This is orchestration
for harnesses, not a mode humans must use to read review state:

```sh
gander handoff --mode delegate --action-item <action-item-prefix> --include-comment <comment-prefix> \
  --to "build agent" --objective "Address the selected review feedback" \
  --constraint "Do not mutate unrelated files" \
  --accept "All selected action items are completed" \
  --verify "nix run .#ci-test" --format markdown
gander handoff --mode delegate --action-item <action-item-prefix> --format json --output delegate.json
```

Verification strings are recorded as instructions only; Gander does not execute
them. Delegate return commands include `--repo`, `--base`, and `--rev` from the
packet so they work from a different cwd; `--state-file` selects storage only,
not the code workspace. Delegate-only flags intentionally fail unless `--mode
delegate` is set.
Use `gander export markdown --profile agent` or `gander export json --profile
agent` for a fuller archive/reference artifact with all hunks and all comments,
including drafts and resolved history.

Bundled CLI-first agent skills can be inspected and installed without a jj repo:

```sh
gander skills list
gander skills show gander-review --format markdown
gander skills install --dir ~/.agents/skills --force --format json
```
Current import restores duplicate-safe comments and matching viewed state only;
action items and walkthroughs are exported for reference but are not restored by
`gander import`.

While the TUI is running it also serves the same protocol on a Unix socket
(in the workspace runtime dir; see `gander paths`) backed by the **live**
session — and `gander acp`
automatically bridges stdio to that socket when it exists, so agents that
spawn `gander acp` see current viewed state, comments, and target instead
of a startup snapshot. See [`docs/acp.md`](docs/acp.md) for the method
reference.

To pull an agent into the loop without leaving the review, configure
`[agent] command` (any prompt-taking CLI: `opencode run`, `claude -p`,
`opencode run --attach http://localhost:4096` to reuse a running server,
...) and press `@` in the TUI, or set `autostart = true` to summon it on
startup. gander hands the command a built-in review prompt (customizable
via `[agent] prompt`), logs its output to the workspace agent log, announces
its progress in the footer, and kills it when you quit.

## MCP server

For agent harnesses that speak MCP (opencode, Claude Code, Codex, Zed,
...), `gander mcp` serves the same review session as typed tools on stdio.
MCP is optional: the CLI is the baseline automation contract, and every MCP
capability should have an equivalent scriptable CLI path over the same core
logic:

```sh
gander mcp
```

Tools: `review_summary`, `review_files`, `file_diff`, `comments`,
`current_focus` (what the human is looking at right now), `stack_changes`
and `change_diff` (the jj stack and one change's own diff, for
stacked-PR-style reviews), `set_ordering`, `flag_section`, `set_chunks`
(internal live-curation inputs that can anchor to a stack change via `change_id`), `draft_comment`,
and `list_reviews` (every
running review instance). Spawned in a workspace, each tool call routes to
that workspace's live gander TUI through the instance registry — with
several instances, the most recently touched one wins — so the harness
sees current viewed state and comments, and suggestions surface live in
the reviewer's terminal. Without a running TUI it serves a snapshot loaded
at startup.

Register it with the workspace as the working directory, e.g. for
opencode:

```json
{
  "mcp": {
    "gander": {
      "type": "local",
      "command": ["gander", "mcp"]
    }
  }
}
```

When a review is large (thresholds under `[limits]`), the TUI nudges you
that an agent can organize it: summon one with `@` or ask your harness,
then press `T` (or `Z`) for **zen mode** — a focused walkthrough of authored
steps and chapters. The file pane hides, rows outside the current stop dim, and
a bottom panel shows progress and rationale; advancing (Enter/`n`) marks the
file viewed. Without walkthrough steps, zen walks the files in review order
instead. Public `chunks`/`briefs` commands have been removed; use
`gander walkthrough ...` for durable automation, while ACP/MCP overlay chunks and
briefs remain active internal curation inputs. Every
normal review key (comments, flags, context expansion, view toggles) keeps
working mid-walkthrough; Esc returns to free navigation.

See [`docs/harness-setup.md`](docs/harness-setup.md) for full recipes:
CLI-first automation, optional MCP registration for opencode/Claude Code/Codex,
the split-pane workflow, and attach-to-running-server summon commands.

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
- `agent`: shared agent-overlay schema (`agent.json` in the workspace state dir)
- `paths`: per-workspace XDG state/runtime path resolution and legacy migration
- `registry`: instance registry (one entry per running TUI, routed by cwd)
- `acp`: JSON-RPC stdio server for agent-collaborative review
- `mcp`: MCP stdio server (rmcp) adapting the ACP dispatch into typed tools

The design goal is to keep jj interaction, parsing, review state, rendering, and artifact export separable so future work can be delegated safely.
See [`docs/jj-integration.md`](docs/jj-integration.md) for the decision to use
the `jj` CLI boundary instead of embedding `jj-lib` for now.

## Near-term roadmap

See [`docs/vision.md`](docs/vision.md) and [`docs/roadmap.md`](docs/roadmap.md)
for the longer product plan. Highest-value next steps:

1. first-class durable review sessions with comments, action-tagged optional action items, and
   walkthroughs
2. complete CLI automation with stable JSON output and parity with MCP/TUI
3. TUI affordances for key hunks, walkthrough editing, and open action-item work
4. static web walkthrough export after the session model stabilizes

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT license ([LICENSE-MIT](LICENSE-MIT))

at your option.

Unless you explicitly state otherwise, any contribution intentionally
submitted for inclusion in the work by you, as defined in the Apache-2.0
license, shall be dual licensed as above, without any additional terms or
conditions.
