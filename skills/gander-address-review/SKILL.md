---
name: gander-address-review
description: Address Gander review comments or tasks, then update durable Gander review state with the outcome.
---

# Address a Gander Review

Use when a Gander handoff, delegation packet, or local session asks you to fix review feedback. Gander stores review state; use the reviewed project's normal tools to edit and check code.

1. **Target the reviewed workspace.** If you are operating from another cwd, pass `--repo <reviewed-workspace>` to Gander commands. If a packet supplies repository/base/revision, preserve those global options (`--repo`, `--base`, `--rev`) in return commands. `--state-file` only selects review-state storage; it does not select the code workspace.
2. **Read the relevant open items.** Commands accept compact unique ID prefixes (minimum 8 chars, longer only on collisions). Prompt handoff includes only open tasks and `todo` comments. Treat every todo comment as a request to address it, regardless of whether it is phrased as a question, explanation, praise, or `action=none`. Draft comments are private/withheld and should not appear in delegation unless the reviewer first readied them; resolved comments are history/reference. Use the packet/handoff selectors or list state:
   ```bash
   gander --repo <reviewed-workspace> comments list
   gander --repo <reviewed-workspace> tasks list
   ```
3. **Make the code/docs changes** with the project's usual edit, build, and test workflow.
4. **Update Gander state concisely.** Reply/resolve comments and complete tasks with what changed and what was actually checked:
   ```bash
   gander --repo <reviewed-workspace> comments resolve <comment-prefix> --reply '<what changed; what was checked>'
   gander --repo <reviewed-workspace> tasks complete <task-prefix> --summary '<what changed; what was checked>'
   ```
5. **If follow-up remains, record it** instead of claiming it is done. New
   comments default to the reviewer's configured initial state; pass `--state
   todo` when the follow-up should be addressed by another agent, or `--state
   draft` for a private note:
   ```bash
   gander --repo <reviewed-workspace> comments add --path <path> --line <line> --state todo --kind issue --action follow-up --body '<follow-up>'
   ```

Honesty rule: report what changed and what you actually checked; do not imply broader verification than you performed.
