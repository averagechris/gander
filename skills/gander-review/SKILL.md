---
name: gander-review
description: Use Gander to review jj-visible changes with durable local review state, CLI-first inspection, comments, tasks, walkthroughs, and exportable handoffs without mutating code or posting to forges.
---

# Gander Review

Use when reviewing local jj-visible work with Gander. Gander reads code state and writes review state only.

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
   gander tasks list --format json
   gander walkthrough show
   ```

   Prefer targeted file/diff reads over broad scans. Use the TUI for navigation, but keep automatable CLI steps as the source of truth.

3. Record durable review state as you go:

   ```bash
   gander comments add --path <path> --line <n> --body "Check this invariant."
   gander tasks add --title "Follow up" --path <path> --line <n> --body "Add a regression test."
   gander walkthrough add-step --path <path> --line <n> --title "Why this matters" --body "Start here before reading callers."
   ```

   Anchor comments/tasks to the smallest useful location. Mark actionable follow-ups as tasks; use walkthroughs for reading order or handoff context.

4. Export a handoff when asked or when another agent will act:

   ```bash
   gander handoff --format markdown
   gander handoff --format json
   gander handoff --mode delegate --task <id> --include-comment <id> --format json
   ```

## Guardrails

- Do not edit the user's code, run formatters that mutate files, fetch refs, post to GitHub/GitLab, or resolve comments outside Gander.
- Do not claim forge status; Gander is local review state, not a forge integration.
- Keep feedback direct and evidence-based. Verify findings against the target/session before recording them.
- Avoid deprecated chunk/brief/stale-flag workflows; use current comments, tasks, walkthroughs, and handoff artifacts.
