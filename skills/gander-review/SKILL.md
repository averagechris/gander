---
name: gander-review
description: Use Gander to review jj-visible changes with durable local review state, CLI automation, TUI navigation, comments, optional action items, walkthroughs, and outbound handoffs without mutating code or posting to forges.
---

# Gander Review

Use when reviewing local jj-visible work with Gander. Durable review state is the source of truth; the CLI automates it and the TUI helps navigate it. Gander reads code state and writes review state only.

## Workflow

1. Verify target and session before judging changes:

   ```bash
   gander summary
   gander reviews list --format json
   ```

   Confirm the repo, revision/range, base, and existing session match the user's request. If they do not, stop and ask or switch explicitly.

2. Inspect bounded context from the CLI first:

   ```bash
   gander files list --format json
   gander hunks list --path <path> --format json
   gander comments list --format json
   gander action-items list --format json
   gander walkthrough show
   ```

   Prefer targeted file/diff reads over broad scans. Use the TUI for navigation and the CLI for repeatable automation.

3. Record durable review state as you go:

   ```bash
   gander comments add --path <path> --line <n> --state todo --body "Check this invariant."
   gander comments add --general --state draft --body "Private note; not ready for handoff."
   gander comments ready <comment-prefix>
   gander action-items add --title "Follow up" --path <path> --line <n> --body "Add a regression test."
   gander walkthrough add-step --path <path> --line <n> --title "Why this matters" --body "Start here before reading callers."
   ```

   Anchor comments/action items to the smallest useful location. Use `--general` only
   for session-level comments with no location/excerpt. Commands accept compact
   unique prefixes for comment/action-item IDs (`<comment-prefix>`, `<action-item-prefix>`;
   minimum 8 chars unless a collision requires more). Comment states are
   workflow gates: `draft` is durable, private, and withheld; `todo` is ready and
   actionable; `resolved` is history. Every todo asks an agent to address it,
   even if the kind is question/praise/note or the action is none/explain. Mark
   optional higher-level work as durable action items when useful; action items are the handoff selection (open durable action
   items + unlinked todo comments). Use walkthroughs for reading order or
   handoff context. Linked todo evidence is folded into its parent action item.

4. Export a handoff when asked or when another agent will act:

   ```bash
   gander handoff --format markdown
   gander handoff --format json
   gander handoff --mode delegate --action-item <action-item-prefix> --include-comment <todo-comment-prefix> --format json
   ```

## Guardrails

- Do not edit the user's code, run formatters that mutate files, fetch refs, post to GitHub/GitLab, or resolve comments outside Gander.
- Do not claim forge status; Gander is local review state, not a forge integration.
- Keep feedback direct and evidence-based. Verify findings against the target/session before recording them.
- Do not delegate drafts: implicit delegate selects todo comments only, and an
  explicit draft selector should be readied first or rejected.
- Report what you reviewed and what you actually checked; do not overstate verification.
