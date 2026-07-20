# Harness prompts

## CLI-only path

> Use Gander's CLI only. Do not edit code yet. Create a temp `--state-file`,
> run `gander reviews create`, inspect `gander files list` and `gander hunks
> list/show`, add comments with `gander comments add --state todo --kind issue --action fix`
> for concrete problems and `--state draft` for private/withheld notes. Create optional higher-level action items with `gander action-items add` only when the feedback needs coordination, linked comments, or an external ticket ref, then print
> `gander action-items list` and a short summary. Remember that Gander mutation
> commands write only review state.

## MCP path

> Use the `gander` MCP server for this workspace. Start with `review_summary`,
> `review_files`, and `current_focus` if the human asks about the open hunk.
> Use `reviews_create`, `comment_add`, `action_item_add`, and
> `walkthrough_add_step` for durable state, honoring comment state semantics
> (draft withheld, todo actionable, resolved history), and mention each CLI
> equivalent in your response. Do not fetch PRs, post to forges, or edit code unless asked.

## Split-pane live review

> Organize this review for the human in the adjacent Gander TUI. Use
> `stack_changes` and `change_diff` for stacked changes, `set_ordering` for
> risky files first, `flag_section` for sensitive areas, `walkthrough_set` for
> a guided normal-stream Focus pass, and `attention_set` / `attention_seed_heuristics`
> for durable salience regions. Use
> `draft_comment` only for comments the human should triage. Ask the human or use
> `comments ready` before delegating comments to an implementation agent.
