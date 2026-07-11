---
name: gander-review
description: Use Gander to review jj-visible changes with durable local review state, CLI automation, TUI navigation, comments, optional action items, walkthroughs, and outbound handoffs without mutating code or posting to forges.
---

# Gander Review

Use when reviewing local jj-visible work with Gander. The durable review session is the source of truth: the CLI is the normal automation surface, the TUI helps humans navigate, and MCP is an optional adapter over the same operations. Gander reads code state and writes review state only.

## Workflow

1. Verify the workspace and target before judging changes:

   ```bash
   gander paths
   gander summary
   gander reviews list --format json
   ```

   Confirm the workspace, revision/range, base, and existing session match the user's request. If they do not, stop and ask or switch explicitly. If the target is correct but has no matching session, create or return it explicitly:

   ```bash
   gander reviews create
   ```

2. Inspect bounded context from the CLI first:

   ```bash
   gander files list --format json
   gander hunks list --path <path> --format json
   gander hunks show '<path>:<index>' --format diff
   gander comments list --format json
   gander action-items list --format json
   gander walkthrough show
   ```

   Prefer targeted file/diff reads over broad scans. Use the TUI for navigation and the CLI for repeatable automation.

3. Record durable review state as you go:

   ```bash
   gander comments add --path <path> --line <n> --state todo --action fix --body "Preserve this invariant."
   gander comments add --path <test-path> --line <n> --state todo --action test --body "Add regression coverage."
   gander comments add --general --state draft --body "Private note; not ready for handoff."
   gander comments ready <comment-prefix>
   gander action-items add --title "Coordinate parser hardening" --comment <first-todo-prefix> --comment <second-todo-prefix>
   gander walkthrough add-step --path <path> --line <n> --title "Why this matters" --body "Start here before reading callers."
   ```

   Anchor todo comments to the smallest useful location; use `--general` only for session-level feedback with no location or excerpt. Commands accept a full ID or any unambiguous prefix; Gander-generated selectors are at least eight characters and grow to avoid collisions.

   Comment states are workflow gates: `draft` is durable, private, and withheld; `todo` is ready and actionable; `resolved` is history. Every todo asks an agent to address it, even if its kind is question/praise/note or its action is none/explain. Todo comments are the normal feedback primitive. Create an optional durable action item only to group comments, coordinate broader work, or carry external tickets. An open action item folds its linked todos beneath it; other todos remain standalone open work. Closing the item does not resolve those comments. Use walkthroughs for reading order and explanatory context.

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
- Observation/reply provenance is evidence about the already-loaded review diff,
  not an outcome attestation. A target label or unchanged portable patch does
  not prove correctness or testing, and legacy records may have no snapshot.
