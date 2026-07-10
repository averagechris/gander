---
name: gander-address-review
description: Address a Gander review handoff by editing and testing code externally, then updating durable Gander replies, resolutions, tasks, and refreshed handoff without overstating verification.
---

# Gander Address Review

Use when a review handoff, delegation, or local Gander session asks you to address findings. Code edits happen outside Gander; Gander stores review state.

## Workflow

1. Read the handoff/delegation and verify the target:

   ```bash
   gander summary
   gander reviews list --format json
   gander comments list --format json
   gander tasks list --format json
   ```

   Match repo, revision/range, files, and task IDs before changing code. If the handoff is ambiguous or stale, ask for clarification.

2. Edit code with normal project tools, not through Gander. Keep changes scoped to the requested findings. Run the relevant tests/checks externally:

   ```bash
   cargo test <name>
   cargo clippy --all-targets -- -D warnings
   ```

   Use the project's actual commands when they differ.

3. Update durable review state after each addressed item:

   ```bash
   gander comments reply <id> --body -
   gander comments resolve <id>
   gander tasks complete <id> --body -
   gander tasks add --title "Follow up" --body -
   ```

   Explain what changed and cite tests that actually ran. If verification was not run or failed, say so and leave the item unresolved or add a follow-up task.

4. Rerun the handoff for the next reviewer/agent:

   ```bash
   gander handoff --format markdown
   gander handoff --format json
   gander handoff --mode delegate --task <id> --format json
   ```

## Guardrails

- Do not claim verification prematurely. Distinguish edited, compiled, tested, and manually inspected.
- Do not resolve comments or complete tasks until the code change and evidence support it.
- Do not post to forges from this skill; use Gander replies/resolutions/tasks for local durable state.
- Avoid deprecated chunk/brief/stale-flag workflows; rely on comments, tasks, walkthroughs, and handoffs.
