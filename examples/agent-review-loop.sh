#!/usr/bin/env bash
set -euo pipefail

state="${TMPDIR:-/tmp}/gander-agent-review-loop-$$.json"
rm -f "$state"

review_id=$(cargo run --quiet -- --state "$state" reviews create --title "Agent review loop" | jq -r .id)
comment_id=$(cargo run --quiet -- --state "$state" comments add \
  --path README.md \
  --line 1 \
  --kind issue \
  --action fix \
  --body "Clarify the first sentence for new users." | jq -r .id)
task_id=$(cargo run --quiet -- --state "$state" tasks add \
  --title "Clarify README opening" \
  --action fix \
  --comment "$comment_id" \
  --path README.md \
  --line 1 | jq -r .id)

cargo run --quiet -- --state "$state" tasks list \
  | jq -r '.tasks[] | select(.status == "open") | .id' \
  | while read -r id; do
      cargo run --quiet -- --state "$state" tasks complete "$id" --summary "Completed in example loop"
    done

cargo run --quiet -- --state "$state" reviews show "$review_id" \
  | jq --arg task_id "$task_id" '{review: .id, title, completed_task: $task_id, task_count: (.tasks | length)}'
