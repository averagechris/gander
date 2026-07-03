# Design decisions

Short ADR-style log of directional decisions. Newest first. Each entry
records the decision, the reasoning, and what it supersedes, so future
sessions (human or agent) can pick up implementation without relitigating.

## D6 (2026-07): runtime state moves out of the repo (no more `.gander/` pollution)

**Decision.** Stop writing runtime state into the project directory. The
project-local `.gander/` dir (state.json, agent.json, acp.sock, agent.log,
default artifact output) forces users to gitignore tool droppings in every
repo — unpolished and unprofessional. Move all of it to per-user directories
keyed by workspace root:

- durable state (viewed marks, comments, overlay): XDG state dir, e.g.
  `~/.local/state/gander/<workspace-key>/` (`XDG_STATE_HOME` respected)
- ephemeral endpoints (sockets, instance registry entries, logs):
  `XDG_RUNTIME_DIR` when set, else the state dir
- `<workspace-key>` = short hash of the canonicalized workspace root plus a
  human-readable slug (e.g. `gander-3f9c2a`), so paths are debuggable
- explicit overrides stay supported (`--state`, config), and a
  `gander paths` style command should print resolved locations

**Config is the exception.** A committed `gander.toml` at the repo root is a
legitimate project file (like `.editorconfig`) and stays supported, as does
XDG user config. The `.gander/config.toml` layer is deprecated along with
the directory. Default artifact output moves to stdout/explicit paths
instead of `.gander/review.md`.

**Migration.** Read legacy `.gander/` state when present (one release of
fallback), prefer the new location for writes, and note the move in release
notes. jj workspaces get distinct keys naturally since each has its own
working-copy root.

## D5 (2026-07): MCP is the agent-facing tool surface; the custom `gander-acp` vocabulary is transitional

**Decision.** Expose the review session to harnesses as an MCP server
(`gander mcp`, stdio), implemented as a thin adapter over the same
`AcpHandler`/live-socket plumbing that exists today. Tools: `review_summary`,
`review_files`, `file_diff`, `comments`, `set_ordering`, `flag_section`,
`set_chunks`, `draft_comment`, plus `current_focus` (what the human is
looking at: instance, file, line, hunk) and `list_reviews` (instance
registry). Prefer the official Rust MCP SDK (`rmcp`) over hand-rolling;
MCP's surface (initialization, capabilities, tool schemas) is larger than
what we hand-rolled for the ACP slice.

**Why.** MCP is what harnesses (opencode, Claude Code, Codex, Zed) discover
natively: typed tools, no wire protocol explained in a prompt. It dissolves
the "custom vocabulary" debt of `gander-acp` v1 — the tool schemas *are* the
discovery layer. The line-delimited JSON-RPC socket stays as internal
plumbing (TUI liveness bridge) and for raw scripting, but agents should
arrive via MCP.

**Supersedes.** The plan to adopt full spec-ACP server compliance for the
review surface (roadmap milestone 7 debt note). Spec ACP may still matter
later, but as a *client* role (see D4).

## D4 (2026-07): no full chat panel; harness owns the conversation

**Decision.** Do not build a general chat UI inside the gander TUI. The
conversation lives in the user's harness (opencode/claude/... in a split
pane), which does chat UX (history, modes, permissions, streaming) far
better than a ratatui side panel would. Gander's inbound channel from agents
is structured suggestions (ordering, flags, chunks, drafts) plus footer
status.

**Kept open.** Two smaller affordances may earn their place later:

- an **ask popup**: one keystroke on a line → one-shot question → one
  streamed answer in a popup (esc dismisses). Patches the hot-path "explain
  *this* line" gap without making gander a chat app. Likely built on the
  harness's API or MCP sampling; requires gander to act as a spec-ACP
  *client* or harness-API client — deliberately deferred.
- **tour mode** (`T`): step through agent-suggested chunks in order with
  their rationale displayed; gander-native, reads the overlay, needs no live
  agent.

**Why.** The chat panel is where TUIs go to get complicated (focus
management, scrollback, streaming layout). Tour mode + drafts + `current_focus`
in the harness chat cover most of the value at a fraction of the complexity.

## D3 (2026-07): one gander instance per workstream; cwd routing plus an instance registry

**Decision.** The supported model is one gander TUI per workstream (per jj
workspace / working directory). Harness sessions route to the right instance
by cwd: an MCP server spawned in a workspace finds that workspace's
endpoints. To make multiplicity first-class:

- sockets become per-instance (`acp-<pid>.sock` naming) instead of one
  contended path
- each TUI registers itself (workspace root, target, summary, socket path,
  pid, `last_input_at`) in a small instance registry and cleans up on exit
- `list_reviews` exposes the registry; `current_focus` disambiguates "the
  review I'm looking at" via `last_input_at` when a session sees several

**Rejected.** A global daemon multiplexing all instances behind one
endpoint: recreates discovery/lifecycle complexity in the middle for no
gain once cwd routing exists.

## D2 (2026-07): agent integration is agent-agnostic

**Decision.** Gander never hardcodes a specific harness. `[agent] command`
is an arbitrary shell command handed a prompt (`opencode run`, `claude -p`,
`opencode run --attach <url>` to reuse a running server, ...). Harness
names appear only in docs as examples. Attach-to-running-server is the
harness CLI's job, not gander's (no HTTP client in gander).

## D1 (2026-07): TUI and agent server share state through files/sockets, not threads

**Decision.** The TUI and agent-facing servers stay separate processes (or
a listener thread feeding the event loop through a channel). Shared state
flows through the overlay file (durable, mergeable, crash-safe atomic
writes) and the live socket (fresh reads, immediate UI application). This
keeps stdio ownership clean: TUI renders to stderr, keyboard on stdin,
artifacts on stdout; `gander acp`/`gander mcp` own their own stdio.
