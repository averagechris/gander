# Gander examples

Small copy-pasteable examples for headless and maintainer-driven review flows.

- `agent-review-loop.sh` creates a temp review state file, adds an actionable
  todo comment and optional durable action item, lists open action items with
  `jq`, and closes them.
- `onboarding-walkthrough.md` shows how a maintainer authors a walkthrough in
  the TUI with `Y`/`W` and exports it for a new contributor.
- `harness-prompts.md` contains prompts for agent harnesses using either the
  CLI or MCP tools.
