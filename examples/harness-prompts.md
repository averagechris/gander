# Harness prompts

## CLI-only path

> Use Gander's CLI only. Do not edit code yet. Create a temp `--state` file,
> run `gander reviews create`, inspect `gander files list` and `gander hunks
> list/show`, add comments with `gander comments add --kind issue --action fix`
> for concrete problems, create tasks with `gander tasks add`, then print
> `gander tasks list` and a short summary. Remember that Gander mutation
> commands write only review state.

## MCP path

> Use the `gander` MCP server for this workspace. Start with `review_summary`,
> `review_files`, and `current_focus` if the human asks about the open hunk.
> Use `reviews_create`, `comment_add`, `task_add`, and
> `walkthrough_add_step` for durable state, and mention each CLI equivalent in
> your response. Do not fetch PRs, post to forges, or edit code unless asked.

## Split-pane live review

> Organize this review for the human in the adjacent Gander TUI. Use
> `stack_changes` and `change_diff` for stacked changes, `set_ordering` for
> risky files first, `flag_section` for sensitive areas, `set_chunks` for a
> guided zen pass, and `draft_comment` only for comments the human should triage.
