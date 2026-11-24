#!/usr/bin/env bash
# The comment-only action: posts the comment for an analysis run that used
# `comment: workflow-run` (see action.yml).
#
# The analysis run ran the workflow and the action as the PR has them, so a
# PR from a fork controls all of its artifacts. Everything this script takes
# from them is checked: the PR number against the PR's head commit and
# repository from the `workflow_run` event, the commits and counts by their
# form, and the reason why the changes since the previous comment could not
# be shown is reduced to plain words. The rest it reads from the API: the
# previous comment, and the links and expiry of the pages, which must be
# artifacts of the analysis run.
set -euo pipefail

if [ "$EVENT" != "workflow_run" ]; then
  echo "::error::The comment-only action runs in a workflow triggered by workflow_run, not $EVENT."
  exit 1
fi
if [ "$RUN_EVENT" != "pull_request" ]; then
  echo "::notice::The analysis run was triggered by $RUN_EVENT, not by a pull request: no comment."
  exit 0
fi
if [ "$CONCLUSION" != "success" ]; then
  echo "::notice::The analysis run did not succeed ($CONCLUSION): no comment."
  exit 0
fi

dir="$RUNNER_TEMP/rust-semantic-review-comment"
rm -rf "$dir"
mkdir -p "$dir"

gh api --paginate "repos/$REPO/actions/runs/$RUN_ID/artifacts" \
  --jq '.artifacts[] | select(.expired | not) | {id, name, size_in_bytes, expires_at} | @json' > "$dir/artifacts.jsonl"
artifact() {
  jq -sc --arg name "$1" 'map(select(.name == $name)) | last // empty' "$dir/artifacts.jsonl"
}

data=$(artifact rust-semantic-review-comment)
if [ -z "$data" ]; then
  # With `comment: true`, the analysis run only uploads it if it could not
  # comment itself.
  echo "::notice::The analysis run has no rust-semantic-review-comment artifact: it posted the comment itself, or it doesn't comment."
  exit 0
fi
if [ "$(jq '.size_in_bytes' <<< "$data")" -gt 65536 ]; then
  echo "::error::The rust-semantic-review-comment artifact is too large."
  exit 1
fi
gh api "repos/$REPO/actions/artifacts/$(jq '.id' <<< "$data")/zip" > "$dir/data.zip"
# Only the one file, to stdout: no paths from the archive are used.
unzip -p "$dir/data.zip" rust-semantic-review-comment.json | head -c 65536 > "$dir/data.json"

# Refuses anything but the expected form. The counts are checked when the
# summary is made (see action/summary.jq).
if ! jq -e '
  def sha: type == "string" and test("^[0-9a-f]{40}$");
  .version == 1
  and (.pr | type == "number" and . > 0 and . == floor)
  and (.head | sha)
  and (.previous | . == "" or sha)
  and (.since | type == "string")
  and (.counts | type == "object")
  and (.since_counts | . == null or type == "object")
' "$dir/data.json" > /dev/null; then
  echo "::error::The rust-semantic-review-comment artifact is malformed."
  exit 1
fi
PR=$(jq -r '.pr' "$dir/data.json")
HEAD=$(jq -r '.head' "$dir/data.json")

# The PR must be the one the analysis ran for, and still at that commit: a
# newer push has its own run, whose comment this one must not follow.
pr=$(gh api "repos/$REPO/pulls/$PR" --jq '{sha: .head.sha, repo: (.head.repo.full_name // "")} | @json')
if [ "$(jq -r '.repo' <<< "$pr")" != "$HEAD_REPO" ] || [ "$(jq -r '.sha' <<< "$pr")" != "$HEAD_SHA" ]; then
  echo "::notice::PR #$PR is not at the analysis run's commit $HEAD_SHA of $HEAD_REPO (a newer push?): no comment."
  exit 0
fi
if [ "$HEAD" != "$HEAD_SHA" ]; then
  echo "::notice::The analysis is of $HEAD, not of the run's commit $HEAD_SHA: no comment."
  exit 0
fi
export PR HEAD

# The previous comment as it is now, read here and not taken from the
# artifact. The analysis' page of the changes since then only fits if it
# started from the same commit.
"$GITHUB_ACTION_PATH/../action/previous-comment.sh" "$dir"
PREVIOUS=$(cat "$dir/previous-head")
PREVIOUS_ID=$(cat "$dir/previous-id" 2>/dev/null || true)
PREVIOUS_NODE_ID=$(cat "$dir/previous-node-id" 2>/dev/null || true)
PREVIOUS_STATE=$(cat "$dir/previous.json" 2>/dev/null || echo '{}')
SINCE=
if [ -n "$PREVIOUS" ] && [ "$(jq -r '.previous' "$dir/data.json")" = "$PREVIOUS" ]; then
  SINCE=$(jq -r '.since' "$dir/data.json")
fi

summarize() { jq -r -f "$GITHUB_ACTION_PATH/../action/summary.jq"; }
SUMMARY=$(jq '.counts' "$dir/data.json" | summarize)
COUNTS=$(jq -c '.counts' "$dir/data.json")
page=$(artifact "rust-semantic-review-$PR-${HEAD:0:7}.html")
if [ -z "$page" ]; then
  echo "::error::The analysis run has no page artifact for PR #$PR."
  exit 1
fi
ARTIFACT_ID=$(jq -r '.id' <<< "$page")
URL="$SERVER/$REPO/actions/runs/$RUN_ID/artifacts/$ARTIFACT_ID"
SINCE_URL=
SINCE_ARTIFACT_ID=
SINCE_SUMMARY=
case "$SINCE" in
  changes)
    since_page=$(artifact "rust-semantic-review-$PR-${HEAD:0:7}-since.html")
    if [ -n "$since_page" ] && [ "$(jq '.since_counts' "$dir/data.json")" != null ]; then
      SINCE_ARTIFACT_ID=$(jq -r '.id' <<< "$since_page")
      SINCE_URL="$SERVER/$REPO/actions/runs/$RUN_ID/artifacts/$SINCE_ARTIFACT_ID"
      SINCE_SUMMARY=$(jq '.since_counts' "$dir/data.json" | summarize)
    else
      SINCE=
    fi
    ;;
  none | "") ;;
  unavailable*)
    # The tool's reason, as plain words: no links (not even `www.` ones,
    # which GitHub links without a scheme), mentions, references or markup.
    why=$(printf '%s' "${SINCE#unavailable }" | tr -cd "A-Za-z0-9 ,;'()-" | cut -c1-200)
    SINCE="unavailable $why"
    ;;
  *) SINCE= ;;
esac

COMMENT_ONLY=true POST=true RETENTION_DAYS='' \
  URL=$URL ARTIFACT_ID=$ARTIFACT_ID SUMMARY=$SUMMARY COUNTS=$COUNTS \
  SINCE=$SINCE SINCE_URL=$SINCE_URL SINCE_ARTIFACT_ID=$SINCE_ARTIFACT_ID SINCE_SUMMARY=$SINCE_SUMMARY \
  PREVIOUS=$PREVIOUS PREVIOUS_ID=$PREVIOUS_ID PREVIOUS_NODE_ID=$PREVIOUS_NODE_ID PREVIOUS_STATE=$PREVIOUS_STATE \
  "$GITHUB_ACTION_PATH/../action/post-comment.sh"
