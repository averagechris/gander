# Product vision

Gander's north star is to be a **local-first review workspace for jj-visible
changes**: the place where humans and agents jointly understand, annotate,
walk through, and act on code changes without Gander owning remote-provider or
working-copy orchestration.

In one sentence:

> Gander turns jj diffs into durable, guided, actionable review sessions for
> humans and agents.

## What Gander owns

Gander should read code state and write review state.

It owns:

- reading jj changes, revsets, files, hunks, symbols, and diff context;
- durable review sessions, viewed state, comments, optional action items, walkthroughs, and
  exported artifacts;
- local review navigation in the TUI and future web/static views;
- machine-readable automation through a complete CLI surface and optional MCP
  adapter;
- safe handoff points where external human or agent harnesses can decide what
  to do next.

It should not own:

- fetching pull requests or remote branches;
- posting reviews to GitHub/GitLab/SourceHut or other forges;
- rebasing, checking out, editing, committing, or otherwise mutating code as
  part of review state management;
- a general-purpose agent chat UI;
- MCP as the only automation path.

The expected integration model is that a user or harness prepares a workspace
where the work is already visible to jj, then invokes Gander there:

```sh
gander reviews create
gander tui
```

External harnesses can fetch code, run agents, post review comments, create
issues, or edit files. Gander provides the structured review artifact and the
local commands/protocols those harnesses can consume.

## Design rules

1. **Local-first and forge-agnostic.** Review sessions are rooted in the local
   jj repository/workspace, not in GitHub/GitLab/etc. Hosting-provider
   integrations belong in harnesses or wrappers until a later explicit product
   decision changes that boundary.
2. **Do not mutate the user's code workspace.** Inspect jj state freely and
   persist Gander state, but do not fetch, checkout, rebase, commit, edit files,
   or move bookmarks as a side effect of reviewing. Any helper that would run a
   jj mutation must be explicit, confirmed, and outside the review-state core.
3. **Core first; interfaces are adapters.** TUI, CLI, MCP, and future web UI
   must call the same review-domain services. No interface gets separate
   business logic.
4. **CLI parity is mandatory.** Every capability exposed through MCP or the TUI
   must have a scriptable CLI equivalent with stable JSON output where useful
   (most list/mutation commands default to JSON and use `--format text` for a
   compact human view). Many agent workflows should work without MCP to avoid
   context pollution.
5. **MCP is optional, not privileged.** MCP is a convenience adapter for
   harnesses that want typed tools. It should be thin over the same core API and
   no more capable than the CLI.
6. **Durable review sessions are the core product object.** A diff is input;
   the durable session is the thing Gander creates, resumes, exports, and shares
   with agents.
7. **Private thinking precedes publishing.** Comments, action items, and walkthroughs
   are local/private until an external tool exports or posts them. Gander should
   make review intent explicit but not surprise users by publishing anything.
   Annotation channels make this boundary structural: only collaboration-channel
   annotations are eligible for team-facing export (see
   [docs/annotations.md](annotations.md)).

## Review session model

The next-generation review session should include:

- subject: jj revset/change stack/base+tip already visible in the workspace;
- files, hunks, stable anchors, fingerprints, viewed state, and symbols;
- comments with author identity, channel, kind, status, action intent,
  timestamps, and target;
- annotation channels separating the three review conversations — onboarding
  (agent → reviewer), delegation (reviewer → agent), collaboration
  (human ↔ human), and private notes — with context-inferred defaults and a
  structural publication boundary; see [docs/annotations.md](annotations.md);
- delegation todo comments as the primary agent feedback: every delegation
  todo is an actionable request, while ordinary draft/resolved comments are
  not action items;
- optional durable action items for higher-level coordination, grouping many
  linked comments, and recording opaque external ticket references;
- an attention map assigning salience (spotlight/supporting/skim) to regions
  so reviewers spend attention where the mental-model delta is and dismiss
  the rest with confidence; see [docs/attention.md](attention.md);
- walkthroughs: an ordered path over spotlight regions with explanations and
  rationale, presented in the normal diff view rather than a separate mode;
- exports in JSON, Markdown, and static HTML.

This model is the shared substrate for self-review, reviewing agent-generated
changes, onboarding new contributors, and collaborative teammate review.

## Milestone plan

### M11: Core review sessions

- Promote sessions to first-class durable objects.
- Model comments, optional action items, walkthrough steps, and stable targets in
  the domain layer.
- Keep artifacts serializable and migratable.
- Preserve existing viewed-state/comment behavior through the new model.

### M12: Complete CLI surface

- Add scriptable commands for sessions, files, hunks, comments, action items,
  walkthroughs, and exports.
- Ensure mutating review-state commands write only Gander state.
- Provide stable JSON output for harnesses and agents.
- Treat the CLI as the automation contract, not just a human convenience.

Example shape:

```sh
gander reviews list
gander reviews show <id>
gander hunks list --file src/lib.rs
gander comments add --path src/lib.rs --line 42 --state todo --kind issue --action fix --body "..."
gander action-items list
gander walkthrough add-step --file src/lib.rs --line 42 --why "Entry point" --title "Start here"
gander walkthrough export
```

### M13: TUI over the session core

- Make the TUI read and write the same session objects as the CLI.
- Add first-class affordances for key hunks, walkthrough editing, action-tagged
  todo comments, and open action-item work.
- Present focused/zen review as views over walkthrough/session state; the
  end state is the attention map (M18).

### M14: MCP parity adapter

- Rework MCP tools as thin wrappers over the same services used by the CLI.
- Keep the current live-instance routing and `current_focus` value where useful.
- Document the CLI equivalent for every MCP tool.

### M15: Static web walkthroughs

- Export a self-contained HTML review/walkthrough artifact for sharing or
  onboarding.
- Include key hunks, comments, action-item state, and walkthrough navigation.
- Keep it local/static first; no hosted sync or forge integration.

### M16: Optional local web UI

- Add an interactive local browser UI only after the session model stabilizes.
- Use the same core services and respect the no-code-mutation boundary.

Milestones 17–19 predate M16 in build order; the numbers record when they
were planned, not sequence.

### M17: Annotation channels

Design: [docs/annotations.md](annotations.md).

- Add author identity and channel (onboarding/delegation/collaboration/note)
  to comments and replies.
- Infer the channel from context with a quiet, always-visible indicator and
  one-key override in the editor.
- Fold agent drafts into the comment model; make the publication boundary
  structural (`--profile team` exports collaboration threads only).
- Ship the forge-readiness primitives (anchor round-tripping, identity
  config, disposition, import) with no forge integration.

### M18: Attention map and the review stream

Design: [docs/attention.md](attention.md).

- Make salience (spotlight/supporting/skim) a durable session property fed by
  agent curation, heuristics, and human overrides.
- Render one diff view by salience: skim folds with one-key acknowledge,
  inline narration cards on spotlights, chapters as stream headers.
- Recast walkthroughs as an ordering over spotlight regions; focus becomes a
  view preset, coverage replaces files-viewed as progress.
- Delete the zen phase machinery and the ephemeral overlay-chunk model.

### M19: Presentation polish

- Derived theme system: all chrome routed through one contrast-guarded theme
  computed from a small base palette; auto light/dark detection; transparent
  terminals.
- Keybinding presets (`gander` classic, `hunk`-style) on top of the existing
  remapping system.
- Responsive layout (breakpoint-driven pane behavior) and menu-driven
  discoverability rendered from the live keymap.

## Canonical workflows

### Self-review

```sh
gander reviews create
gander tui
gander action-items list | jq '.action_items[] | select(.status == "open")'
gander walkthrough export
```

### Reviewing an agent's changes

An external agent edits code in a jj workspace. Gander inspects the resulting
change, the human leaves todo comments and optional durable action items, and the agent harness consumes
that open work through CLI or MCP. The agent may edit files; Gander only records
and resolves review state.

### Reviewing teammate changes

A user or harness fetches/checks out/prepares the teammate change. Gander opens
against the local jj-visible revset. Any posting to a remote review system is a
separate harness concern using Gander's exported comments/artifacts.
