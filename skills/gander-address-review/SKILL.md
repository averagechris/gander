---
name: gander-address-review
description: Address Gander todo comments or action items, then update durable Gander review state with the outcome.
---

# Address a Gander Review

Use when a Gander handoff, delegation packet, or local session asks you to fix review feedback. Gander stores review state; use the reviewed project's normal tools to edit and check code.

1. **Target the reviewed workspace.** If you are operating from another cwd, pass `--repo <reviewed-workspace>` to Gander commands. If a packet supplies repository/base/revision, preserve those global options (`--repo`, `--base`, `--rev`) in return commands. `--state-file` only selects review-state storage; it does not select the code workspace.
2. **Read the relevant open items.** Commands accept a full ID or any unambiguous prefix; Gander-generated selectors are at least eight characters and grow to avoid collisions. Prompt handoff includes open durable action items plus todo comments not currently folded beneath an open item. Treat every todo as a request to address it, regardless of whether it is phrased as a question, explanation, praise, or `action=none`. Drafts are private/withheld; resolved comments are history. An open action item groups its linked todos as evidence, not duplicate work.

   Use the packet/handoff selectors or list state:
   ```bash
   gander --repo <reviewed-workspace> comments list
   gander --repo <reviewed-workspace> action-items list
   ```
3. **Make the code/docs changes** with the project's usual edit, build, and test workflow.
4. **Update Gander state concisely.** Resolve each satisfied todo with what changed and what was actually checked. Closing a parent action item does not resolve its linked comments, so resolve the satisfied linked todos before closing the item:
   ```bash
    gander --repo <reviewed-workspace> comments resolve <comment-prefix> --reply '<what changed; what was checked>'
    gander --repo <reviewed-workspace> action-items close <item-prefix> --disposition completed --outcome '<what changed; what was checked>'
    ```
5. **If the requested work remains, leave it open.** Reply with the current result without resolving the todo or closing its action item:

   ```bash
   gander --repo <reviewed-workspace> comments reply <comment-prefix> --body '<what changed; what remains; what was checked>'
   ```

   Add a new comment only for distinct follow-up work. New comments default to the reviewer's configured initial state; pass `--state todo` when another agent should address it, or `--state draft` for a private note:
   ```bash
   gander --repo <reviewed-workspace> comments add --path <path> --line <line> --state todo --kind issue --action follow-up --body '<follow-up>'
   ```

Honesty rule: report what changed and what you actually checked; do not imply broader verification than you performed. Snapshot provenance identifies the observed patch state, not correctness or test success, and legacy records may have no snapshot.
