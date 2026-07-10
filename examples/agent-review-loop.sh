#!/usr/bin/env bash
set -euo pipefail

state="${TMPDIR:-/tmp}/gander-agent-review-loop-$$.json"
rm -f "$state"

review_id=$(cargo run --quiet -- --state-file "$state" reviews create --title "Agent review loop" | jq -r .id)
comment_id=$(cargo run --quiet -- --state-file "$state" comments add \
  --path README.md \
  --line 1 \
  --state todo \
  --kind issue \
  --action fix \
  --body "Clarify the first sentence for new users." | jq -r .id)
item_id=$(cargo run --quiet -- --state-file "$state" action-items add \
  --title "Clarify README opening" \
  --action fix \
  --comment "$comment_id" \
  --path README.md \
  --line 1 | jq -r .id)

cargo run --quiet -- --state-file "$state" action-items list \
  | jq -r '.action_items[] | select(.status == "open") | .id' \
  | while read -r id; do
      cargo run --quiet -- --state-file "$state" action-items close "$id" --disposition completed --summary "Completed in example loop"
    done

cargo run --quiet -- --state-file "$state" reviews show "$review_id" \
  | jq --arg item_id "$item_id" '{review: .id, title, completed_action_item: $item_id, action_item_count: (.action_items | length)}'
