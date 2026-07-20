# TUI keybindings

Gander resolves keys by **effective input context**, not from one global flat
map. Two actions may reuse a key when their contexts are disjoint (for example,
`enter` marks a file viewed in normal review, selects a popup row, and inserts a
newline in the comment editor). Two canonical keys assigned to different
actions in an overlapping context are a configuration error rather than a
first-match-wins shadow.

Configuration layering is unchanged: XDG user config, repository
`gander.toml`, deprecated repository `.gander/config.toml`, then explicit
`--config`, with each later field replacing the earlier field's complete key
list. `tour` remains an alias for `zen`, and `task-list` remains an alias for
`open-work`.

Preset layering happens inside `[keybindings]`: the last configured
`preset = "gander"` (the default) or `preset = "hunk"` selects the complete
base action map. Gander then applies every explicit per-action array from all
config layers in normal source order, including overrides from a layer before
the layer that selected the final preset; each explicit array replaces that
action's whole list. Immutable safety fallbacks are appended after the
effective map and still participate in collision validation.

The `hunk` preset keeps the Gander punctuation bindings and adds an Alt
navigation cluster: Alt-J / Alt-K move to the next / previous changed hunk,
while Alt-L / Alt-H move to the next / previous file.

## Key syntax and safety bindings

Keys are single characters or `esc`, `enter`, `tab`, `backspace`, `space`,
`up`, `down`, `left`, `right`, `pageup`, and `pagedown`. Prefix a key with
`ctrl-`, `alt-`, or `shift-`; `+` is also accepted as the separator. Names are
canonicalized, so `escape` = `esc`, `return` = `enter`, `bs` = `backspace`,
`page-up` = `pageup`, and `control-j` = `ctrl-j`. Unknown `[keybindings]`
fields and unsupported syntax fail config/keymap validation with the field or
key in the error. For ASCII letters, uppercase and Shift are one canonical
form: `G` = `shift-g`, and terminal `G` events match with or without an
explicit SHIFT flag. For punctuation, bind the emitted character (`?`, `>`,
`!`) directly; terminal SHIFT is ignored after punctuation translation and
forms such as `shift-1` are rejected rather than guessing a keyboard layout.

Arrows, Enter, and Esc retain their conventional movement/select/dismiss
behavior. Normal review also retains Vim `j`/`k` movement fallbacks. Non-text
list popups retain `j`/`k` and arrow movement. In text-filter popups (`target
chooser` and `file search`), literal `j` and `k` are query text; movement uses
Up/Down or Ctrl-K/Ctrl-J. No popup gains `q` as an implicit close key.
The existing `q` close behavior remains limited to help, View Options, and the
zen artifact viewer and is configurable as `popup-close-q`.

These are immutable safety bindings, not just defaults: normal `j`/`k` and
arrows, filter arrows/Ctrl-J/Ctrl-K, list `j`/`k` and arrows, popup Enter, and
popup Esc are part of collision validation even when their configurable action
list is replaced. Repeating a safety key for the same action is valid; assigning
it to another action in the same effective context reports an `immutable
fallback` collision.

Zen focus/reading is layered over normal review. Zen actions dispatch first and
other keys fall through to the active normal files/diff context. Validation uses
that same layering and permits only the declared built-in shadows (for example
zen `n/p/g/e/d`, Enter, Tab, arrows, and Space over their established normal
actions). A custom shadow such as `zen-next = ["c"]` is rejected because it
would hide normal `comment`. Esc is an immutable zen-close action; changing
`popup-close` does not change or mislabel the focus/reading close key.

## Context inventory

| Context | Inputs and dispatch |
| --- | --- |
| normal/files | Global actions, file-tree navigation/folding, viewed state, comments |
| normal/diff | Global actions, cursor/scroll, diff view, context, comments, walkthrough marks |
| help | popup movement and close |
| target chooser, file search | text input, filter movement, select, close |
| revset input | text input, Tab/Up/Down field switch, select, close |
| operation picker, jj helpers, flags, open work, activity, symbol outline | list movement, select, close |
| walkthrough list | list movement/select/close plus reorder and delete |
| draft list | list movement/close plus accept, edit, and discard |
| comment list | list movement/select/close plus general creation and comment metadata actions |
| view options | list movement, toggle, select, close |
| comment editor | local text editing plus configurable channel cycle, newline, save, cancel, backspace |
| attention glance | list movement/select/close plus peek, selected acknowledge, and bulk acknowledge |
| zen focus/reading | configurable stop, card, glance, artifact, detail, and refocus actions; other normal actions fall through |
| zen glance | list movement/select/close, acknowledge all, back |
| zen artifact | scroll/select/close and previous/next artifact |

## Default action map

These names are the exact `[keybindings]` fields. An empty list leaves the
direct configured action unbound; any immutable safety binding listed above
remains effective.

### Global, target, and review navigation

| Field | Default | Effective context |
| --- | --- | --- |
| `quit` | `q` | normal |
| `help` | `?` | normal |
| `summon-agent` | `@` | normal |
| `yank-handoff` | `ctrl-y` | normal |
| `move-down` / `move-up` | `j`, Down / `k`, Up | normal files and diff |
| `toggle-focus` | Tab | normal |
| `diff-top` / `diff-bottom` | `g` / `G` | normal diff |
| `compare-trunk` / `compare-parent` | `t` / `p` | normal |
| `target-chooser` / `revset-input` | `b` / `R` | normal |
| `stack-next` / `stack-previous` | `>` / `<` | normal |
| `operation-picker` / `jj-helpers` | `I` / `!` | normal |
| `next-unviewed` / `previous-unviewed` | `n` / `N` | normal |
| `next-comment` / `previous-comment` | `m` / `M` | normal |
| `file-search` | `/` | normal |
| `symbol-outline` | `o` | normal diff |
| `next-symbol` / `previous-symbol` | `}` / `{` | normal diff |
| `next-changed-hunk` / `previous-changed-hunk` | `]` / `[` (`hunk`: Alt-J, `]` / Alt-K, `[`) | normal diff |
| `next-file` / `previous-file` | `.` / `,` (`hunk`: Alt-L, `.` / Alt-H, `,`) | normal |
| `spotlight-next` / `spotlight-previous` | Alt-N / Alt-P | normal |

### Review, diff, and agent actions

| Field | Default | Effective context |
| --- | --- | --- |
| `scroll-down` / `scroll-up` | `d`, PageDown / `u`, PageUp | normal diff |
| `scroll-diff-left` / `scroll-diff-right` | Shift-Left / Shift-Right | normal diff |
| `mark-viewed` / `toggle-viewed` / `mark-all-viewed` | Enter / `v` / `a` | normal (`a` only acknowledges a selected skim fold in stream diff context; ordinary stream rows no-op) |
| `toggle-generated` / `cycle-viewed-filter` | `h` / `f` | normal |
| `toggle-fold` | Space | normal files; selected skim fold in normal diff |
| `collapse-fold` / `expand-fold` | Left / Right | normal files |
| `toggle-context-fold` | `z` | normal diff |
| `expand-context` / `expand-context-all` / `collapse-context` | `+` / `=` / `-` | normal diff |
| `view-options` | `V` | normal |
| `toggle-word-highlight` / `toggle-line-background` / `toggle-gutter-bar` / `toggle-diff-wrap` | unbound | normal diff |
| `toggle-file-pane` / `toggle-diff-view` | `w` / `\|` | normal / normal diff |
| `widen-file-pane` / `narrow-file-pane` | Alt-Right / Alt-Left | normal diff |
| `attention-promote` / `attention-demote` | Alt-Up / Alt-Down | normal diff |
| `attention-focus` / `attention-glance` | `Z` / Alt-G | normal; Focus is an ephemeral preset, glance is a popup |
| `toggle-large-diff` | `L` | normal diff |
| `toggle-agent-order` / `flag-list` | `A` / `F` | normal |
| `open-work` / `activity` | `X` / Ctrl-A | normal |
| `walkthrough-list` / `mark-walkthrough` | `W` / `Y` | normal / normal diff |
| `zen` / `draft-list` | `T` / `D` | normal (legacy zen remains until its M18 removal package) |

### Comments and editor

| Field | Default | Effective context |
| --- | --- | --- |
| `range-comment` / `cancel-range-comment` | `r` / Ctrl-G, Esc | normal diff / normal |
| `comment` | `c` | normal |
| `cycle-comment-state` / `edit-comment` / `delete-comment` | `s` / `e` / `x` | normal and comment list |
| `comment-list` | `C` | normal |
| `comment-list-new-general` | `n` | comment list; the complete gesture is `C`, then `n` |
| `comment-list-ready` | `R` | comment list |
| `comment-list-cycle-action` / `comment-list-cycle-kind` | `a` / `K` | comment list |
| `submit-comment` / `cancel-comment` | Ctrl-S / Esc | comment editor |
| `insert-newline` / `delete-char` | Enter / Backspace | comment editor |
| `cycle-comment-channel` | Tab | comment editor; onboarding → delegation → collaboration → note |
| `toggle-annotation-artifacts` | `E` | normal diff; expands/collapses artifacts only when the cursor's inline card has artifacts |

The editor also keeps ordinary text insertion, arrows, Home/End, Ctrl-A/E,
Ctrl-B/F, Ctrl-P/N, Ctrl-H/D, Ctrl-K/U/W, Alt-B/F, and Alt-Backspace as local
editing controls. Tab is consumed by channel cycling in this context, so it
never inserts text or changes global focus. Ctrl-D deletes the next complete
grapheme, so combining text and emoji clusters are never split.

### Popup controls

| Field | Default | Effective context |
| --- | --- | --- |
| `target-picker-down` / `target-picker-up` | Down, Ctrl-J / Up, Ctrl-K | text-filter popups |
| `popup-move-down` / `popup-move-up` | `j`, Down / `k`, Up | non-text lists, help, zen glance/artifact |
| `popup-select` | Enter | selectable popups and zen glance/artifact |
| `popup-toggle` | Space | View Options |
| `popup-close` | Esc | popups and zen glance/artifact; focus/reading uses immutable zen Esc |
| `popup-close-q` | `q` | help, View Options, and zen artifacts only (established compatibility) |
| `draft-accept` / `draft-edit` / `draft-discard` | Enter, `a` / `e` / `x` | draft list |
| `walkthrough-delete` | `d` | walkthrough list |
| `walkthrough-move-down` / `walkthrough-move-up` | `J` / `K` | walkthrough list |
| `glance-peek` | Space | attention glance; opens the selected current fold in the stream and closes |
| `glance-acknowledge` / `glance-acknowledge-all` | `a` / `A` | attention glance; stale entries remain inert |

### Zen controls

| Field | Default | Effective context |
| --- | --- | --- |
| `zen-next` | `n`, Enter, Right, Space | focus/reading |
| `zen-previous` | `p`, Left | focus/reading and glance |
| `zen-toggle-view` | Tab | focus/reading |
| `zen-glance` | `g` | focus |
| `zen-artifact` | `e` | focus and artifact close |
| `zen-toggle-details` | `d` | chapter focus |
| `zen-refocus` | `.` | focus/reading |
| `zen-acknowledge` | `a` | glance |
| `zen-artifact-next` / `zen-artifact-previous` | `l`, Right, Tab / `h`, Left | artifact viewer |

## Collision-free Colemak Mod-DH override

This is a complete override for every binding affected by making Colemak-DH
`n`/`e` the preferred vertical movement keys (immutable normal/list `j`/`k`
remain safety aliases). It deliberately leaves filter movement on
arrows/Ctrl-J/Ctrl-K so `j` and `k` remain literal search text.

```toml
[keybindings]
move-down = ["n", "down"]
move-up = ["e", "up"]
next-unviewed = ["alt-j"]
previous-unviewed = ["J"]
edit-comment = ["alt-e"]

popup-move-down = ["n", "down"]
popup-move-up = ["e", "up"]
comment-list-new-general = ["ctrl-n"]
draft-edit = ["alt-e"]
zen-artifact = ["i"]
zen-next = ["enter", "right", "space"]
```

The apparently repeated `alt-e` is valid: comment editing in normal/comment
center and agent-draft editing are disjoint dispatch contexts. `alt-j` avoids
the immutable normal `j` movement fallback. In contrast, leaving
`edit-comment = ["e"]`, `comment-list-new-general = ["n"]`,
`draft-edit = ["e"]`, `zen-artifact = ["e"]`, or the default `n` in
`zen-next` would collide with an effective popup or layered zen/normal action,
so the keymap validator rejects those incomplete overrides.
