# Design decisions

Short ADR-style log of directional decisions. Newest first. Each entry
records the decision, the reasoning, and what it supersedes, so future
sessions (human or agent) can pick up implementation without relitigating.

## D9 (2026-07): gander never spawns agents; harnesses own the agent lifecycle

**Decision.** Gander does not spawn, monitor, or kill agent processes.
The summon feature is removed entirely: the `@`/`summon-agent` action, the
`[agent] command`/`autostart`/`prompt` config fields, the `AgentProcess`
lifecycle (spawn, poll, kill-on-drop), the review-prompt templating
(`{repo}`/`{base}`/`{rev}`/`{prompt}` substitution), and the workspace
agent log. Harnesses (opencode, Claude Code, Codex, shell scripts, ...)
own the conversation *and* the process: they run the agent themselves and
drive gander from the outside through the CLI, MCP, or ACP surfaces.
Old configs fail loudly: `[agent] command`/`autostart`/`prompt` produce a
migration error pointing at docs/harness-setup.md, and a `summon-agent`
keybinding is rejected as an unknown field.

**Kept.** `[agent] name` survives as identity configuration: it stamps
agent identity on agent-authored annotations and implies nothing about
process ownership or attachment.

**Why.** Summoning was the one place gander crossed from "reads code
state, writes review state" into process orchestration, with real costs:
a confirmation-free shell-out reachable from a keybinding, prompt/quoting
surface, log plumbing, and lifecycle edge cases — all duplicating what
every harness already does better (docs/decisions.md D4). Removing it also
makes the docs/theme.md containment claim structural rather than
default-keybinding-conditional: leaked OSC 11 payload characters can
mutate durable local review state but can never mutate the code workspace
under any keybinding configuration, because the only remaining shell-out
(the jj helper popup) requires a literal Enter confirmation and an OSC
payload can never contain Enter.

**Supersedes/refines.** D2's `[agent] command` mechanism (the
agent-agnostic principle survives: gander still hardcodes no harness).
Refines D4/D7: the harness owns not just the chat but the agent process;
gander's agent-facing surface is CLI/MCP/ACP only.

## D7 (2026-07): local review core, CLI parity, and no direct forge integration

**Decision.** Gander's core object is a durable local review session over
jj-visible changes. Users or external harnesses are responsible for fetching,
checking out, or otherwise preparing teammate/agent work in a workspace where
jj can see it. Gander reads that code state and writes review state: viewed
marks, comments, optional action items, walkthroughs, and artifacts.

Gander should not directly integrate with GitHub/GitLab/SourceHut for now: no
PR fetching, no review posting, no forge-specific comment sync. Harnesses can
consume Gander artifacts/CLI output and post elsewhere if desired.

**Interface rule.** The CLI is the baseline automation contract. Every
capability exposed through MCP, the TUI, or a future web UI must have a
scriptable CLI equivalent, and all interfaces must call the same core business
logic. MCP remains valuable, but it is an optional adapter rather than the
privileged path; some users avoid MCP because tool definitions and state can
pollute agent context.

**Workspace rule.** Review-state operations must not mutate the user's code
workspace. Inspecting jj state is fine; fetching, rebasing, checking out,
editing files, committing, moving bookmarks, or posting remote reviews belongs
to explicit user/harness workflows outside the review-state core. Existing
confirmed jj helpers remain exceptional, user-confirmed affordances rather than
implicit session behavior.

**Supersedes/refines.** D5's phrasing that harnesses should "arrive via MCP".
MCP is still supported, but CLI parity is mandatory and direct forge
integration is out of scope for the current product direction. See
docs/vision.md and roadmap milestones 11-16.

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
- explicit overrides stay supported (`--state-file`, config), and a
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

**Decision (refined by D7).** Expose the review session to harnesses as an MCP server
(`gander mcp`, stdio), implemented as a thin adapter over the same
`AcpHandler`/live-socket plumbing that exists today. Tools: `review_summary`,
`review_files`, `file_diff`, `comments`, `set_ordering`, `flag_section`,
durable walkthrough/attention tools,
`draft_comment`, plus `current_focus` (what the human is
looking at: instance, file, line, hunk) and `list_reviews` (instance
registry). Prefer the official Rust MCP SDK (`rmcp`) over hand-rolling;
MCP's surface (initialization, capabilities, tool schemas) is larger than
what we hand-rolled for the ACP slice.

**Why.** MCP is what many harnesses (opencode, Claude Code, Codex, Zed) discover
natively: typed tools, no wire protocol explained in a prompt. It dissolves
the "custom vocabulary" debt of `gander-acp` v1 — the tool schemas *are* the
discovery layer. The line-delimited JSON-RPC socket stays as internal
plumbing (TUI liveness bridge) and for raw scripting. D7 later clarifies that
MCP is optional and must stay at parity with the CLI rather than becoming the
only or most capable agent path.

**Supersedes.** The plan to adopt full spec-ACP server compliance for the
review surface (roadmap milestone 7 debt note). Spec ACP may still matter
later, but as a *client* role (see D4).

## D4 (2026-07): no full chat panel; harness owns the conversation

**Decision.** Do not build a general chat UI inside the gander TUI. The
conversation lives in the user's harness (opencode/claude/... in a split
pane), which does chat UX (history, modes, permissions, streaming) far
better than a ratatui side panel would. Gander's inbound channel from agents
is structured suggestions (walkthroughs, attention, ordering, flags, drafts) plus footer
status.

**Kept open.** Two smaller affordances may earn their place later:

- an **ask popup**: one keystroke on a line → one-shot question → one
  streamed answer in a popup (esc dismisses). Patches the hot-path "explain
  *this* line" gap without making gander a chat app. Likely built on the
  harness's API or MCP sampling; requires gander to act as a spec-ACP
  *client* or harness-API client — deliberately deferred.
- **focused presentation**: delivered at M18 as `Z` Focus plus Alt-N/Alt-P
  durable Spotlight navigation in the normal stream; no separate modal layer.

**Why.** The chat panel is where TUIs go to get complicated (focus
management, scrollback, streaming layout). Focus + drafts + `current_focus`
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

**Decision (mechanism superseded by D9).** Gander never hardcodes a specific
harness. `[agent] command`
is an arbitrary shell command handed a prompt (`opencode run`, `claude -p`,
`opencode run --attach <url>` to reuse a running server, ...). Harness
names appear only in docs as examples. Attach-to-running-server is the
harness CLI's job, not gander's (no HTTP client in gander). D9 later removes
`[agent] command` and all agent spawning; the agent-agnostic principle
itself stands.

## D1 (2026-07): TUI and agent server share state through files/sockets, not threads

**Decision.** The TUI and agent-facing servers stay separate processes (or
a listener thread feeding the event loop through a channel). Shared state
flows through the overlay file (durable, mergeable, crash-safe atomic
writes) and the live socket (fresh reads, immediate UI application). This
keeps stdio ownership clean: TUI renders to stderr, keyboard on stdin,
artifacts on stdout; `gander acp`/`gander mcp` own their own stdio.
## D8 (2026-07): remote present control is ephemeral, socket-transported UI control

External agents may drive what a human sees in a live TUI with ACP
`present/*` methods, the CLI-first `gander present` commands, and MCP tools
that route to the same per-instance Unix socket. These commands are ephemeral
UI commands: they move the active stream Spotlight/Focus view and may reload local review state,
but they do not fetch from forges or mutate the user's code workspace.

The TUI event loop, not the immutable ACP snapshot handler, applies these
commands so modal safety is enforced with the same gate as live refresh. If
the human is in a comment editor or popup, presentation commands fail with
`user is busy: <mode>` rather than yanking the view away mid-edit.
