#!/usr/bin/env bash
# Finds the latest Rust Semantic Review comment on a PR, which says which
# commit it showed: the page of the changes since then starts there.
#
# Usage: previous-comment.sh DIR, with GH_TOKEN, REPO and PR set. Writes to
# DIR: `previous-head` (the commit the comment showed; empty if there is no
# comment or it names none) and, if there is a comment, `previous-id`,
# `previous-node-id` (empty unless the workflow's bot wrote it) and
# `previous.json` (the state saved in it, `{}` for an older comment).
#
# Only comments by the workflow's bot or by people with write access count,
# so that nobody else can pick that commit (or the links that are carried
# over from it).
set -euo pipefail
dir=$1
: > "$dir/previous-head"
rm -f "$dir/previous-id" "$dir/previous-node-id" "$dir/previous.json"
gh api --paginate "repos/$REPO/issues/$PR/comments" --jq '
  .[]
  | select(.body | startswith("<!-- rust-semantic-review") or startswith("<!-- rust-pr-explorer"))
  | select(.user.login == "github-actions[bot]"
      or (.author_association | IN("OWNER", "MEMBER", "COLLABORATOR")))
  | {id, node_id, bot: (.user.login == "github-actions[bot]"), body} | @json' > "$dir/comments.jsonl" \
  || { echo "::warning::Could not read the PR's comments: every run posts a new one, without the changes since the last"; exit 0; }
if ! [ -s "$dir/comments.jsonl" ]; then exit 0; fi
tail -n1 "$dir/comments.jsonl" | jq -r '.body' > "$dir/previous.md"
tail -n1 "$dir/comments.jsonl" | jq -r '.id' > "$dir/previous-id"
# Only the bot's own comments are hidden when a new one is posted.
tail -n1 "$dir/comments.jsonl" | jq -r 'if .bot then .node_id else "" end' > "$dir/previous-node-id"
# `<!-- rust-semantic-review {…} -->` (`<!-- rust-pr-explorer {…} -->`
# before the rename), else a comment from before that, which names its
# commit as "built for <sha>".
state=$(sed -n '1s/^<!-- \(rust-semantic-review\|rust-pr-explorer\) \({.*}\) -->$/\2/p' "$dir/previous.md")
if [ -n "$state" ] && jq -e 'type == "object"' <<< "$state" > /dev/null 2>&1; then
  printf '%s' "$state" > "$dir/previous.json"
  prev=$(jq -r '.head // "" | strings' <<< "$state")
else
  echo '{}' > "$dir/previous.json"
  prev=$(grep -oE 'built for [0-9a-f]{40}' "$dir/previous.md" | head -n1 | cut -c11- || true)
fi
if [[ "$prev" =~ ^[0-9a-f]{40}$ ]]; then printf '%s' "$prev" > "$dir/previous-head"; fi
