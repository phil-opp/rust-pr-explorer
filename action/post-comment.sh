#!/usr/bin/env bash
# Writes the job summary and posts or updates the PR comment that links the
# pages. Used by the action (`comment: true`) and by the comment-only action
# in `comment/` (`comment: workflow-run`).
#
# Writes `posted=false` to GITHUB_OUTPUT if commenting failed.
#
# Environment: GH_TOKEN, REPO, PR, POST (`true` to comment, else only the job
# summary), URL, ARTIFACT_ID and SUMMARY of the page, COUNTS (counts.jq's
# numbers for the page, for the list of breaking changes), SINCE_URL,
# SINCE_ARTIFACT_ID and SINCE_SUMMARY of the page of the changes since the
# previous comment (empty without one), HEAD, SINCE (the `since:` line of
# the tool, empty without `--since`), PREVIOUS (the previous comment's
# commit), PREVIOUS_ID, PREVIOUS_NODE_ID and PREVIOUS_STATE (see
# previous-comment.sh), RETENTION_DAYS, RUN_URL and RUN_ID (the run that
# analyzed HEAD, whose saved analysis the next run can reuse, see
# previous-analysis.sh), and UPDATE_COMMENT (`true` to keep one comment and
# update it on every push, see the `update-comment` input).
set -euo pipefail

# A link to a page that tells the page its artifact's id (`#~ID`), from
# which the page builds its permalinks (see the viewer's `nav.rs`).
page_link() {
  if [[ "$2" =~ ^[0-9]+$ ]]; then printf '%s#~%s' "$1" "$2"; else printf '%s' "$1"; fi
}
PAGE=$(page_link "$URL" "$ARTIFACT_ID")
SINCE_PAGE=
if [ -n "$SINCE_URL" ]; then SINCE_PAGE=$(page_link "$SINCE_URL" "$SINCE_ARTIFACT_ID"); fi

# The breaking changes, with links into the page (see breaking.jq).
breaking=$(jq -r --arg url "$URL" --arg id "$ARTIFACT_ID" -f "$(dirname "$0")/breaking.jq" <<< "${COUNTS:-null}")

{
  echo "### Rust Semantic Review"
  echo
  echo "[Open the page]($PAGE): $SUMMARY"
  if [ -n "$SINCE_URL" ]; then
    echo
    echo "[Changes since ${PREVIOUS:0:7}]($SINCE_PAGE): $SINCE_SUMMARY"
  fi
  if [ -n "$breaking" ]; then
    echo
    echo "$breaking"
  fi
} >> "$GITHUB_STEP_SUMMARY"
if [ "$POST" != "true" ]; then exit 0; fi

# When an artifact expires: from the API (needs `actions: read`), else from
# the `retention-days` input; empty if neither works.
expiry() {
  local at=
  if [ -n "$1" ]; then
    # Right after the upload the API sometimes answers 404 for a few
    # seconds, so try a few times.
    local try
    for try in 1 2 3 4 5; do
      if at=$(gh api "repos/$REPO/actions/artifacts/$1" --jq '.expires_at' 2>&1); then
        break
      fi
      if [ "$try" = 5 ]; then
        echo "::notice::Could not read when the artifact expires (does the token have actions: read?): $at" >&2
        at=
      else
        sleep 3
      fi
    done
  fi
  if [ -z "$at" ] && [ -n "$RETENTION_DAYS" ]; then
    at=$(date -u -d "+$RETENTION_DAYS days" +%Y-%m-%dT%H:%M:%SZ)
  fi
  printf '%s' "$at"
}
expires=$(expiry "$ARTIFACT_ID")
# How many earlier pushes an updated comment lists (with UPDATE_COMMENT).
MAX_HISTORY=10

# A new comment for every push that changed the PR, with the changes since
# the previous comment's commit. A push that only merged the base branch or
# rebased onto it (and a re-run) updates the previous comment instead, which
# keeps its "since" link. With UPDATE_COMMENT, a push that changed the PR
# updates the previous comment too, and what that comment showed moves into
# its list of earlier pushes.
#
# `patch`: the previous comment is updated, not a new one posted. `fresh`:
# this push changed the PR, so the comment shows the changes since the
# previous one's commit.
patch=
fresh=1
history='[]'
if [ -n "$PREVIOUS_ID" ] && { [ "$SINCE" = "none" ] || [ "$PREVIOUS" = "$HEAD" ]; }; then
  patch=1
  fresh=
  since=$(jq -c '.since // empty' <<< "$PREVIOUS_STATE")
  history=$(jq -c '.history | if type == "array" then . else [] end' <<< "$PREVIOUS_STATE")
else
  since=
  if [ "$SINCE" = "changes" ]; then
    since=$(jq -nc --arg from "$PREVIOUS" --arg to "$HEAD" --arg url "$SINCE_PAGE" \
      --arg summary "$SINCE_SUMMARY" --arg expires "$(expiry "$SINCE_ARTIFACT_ID")" \
      '{$from, $to, $url, $summary} + (if $expires != "" then {$expires} else {} end)')
  fi
  if [ "${UPDATE_COMMENT:-}" = true ] && [ -n "$PREVIOUS_ID" ]; then
    patch=1
    # What the previous comment showed, then the pushes before it: the
    # latest $MAX_HISTORY whose pages are still there. A comment from before
    # the page was saved in the state has no link to list.
    history=$(jq -c --arg now "$(date -u +%Y-%m-%dT%H:%M:%SZ)" --argjson max "$MAX_HISTORY" '
      ([{head, page, since} | with_entries(select(.value != null))
        | select((.head | type) == "string" and (.page.url? | type) == "string")]
       + (.history | if type == "array" then . else [] end))
      | map(select((.page.expires // "9999") > $now)) | .[:$max]' <<< "$PREVIOUS_STATE")
  fi
fi
page=$(jq -nc --arg url "$PAGE" --arg summary "$SUMMARY" --arg expires "$expires" \
  '{$url, $summary} + (if $expires != "" then {$expires} else {} end)')
state=$(jq -nc --arg head "$HEAD" --arg run "${RUN_ID:-}" --argjson page "$page" \
  --argjson since "${since:-null}" --argjson history "$history" \
  '{$head} + (if $run != "" then {run: ($run | tonumber)} else {} end) + {$page}
   + (if $since then {$since} else {} end)
   + (if ($history | length) > 0 then {$history} else {} end)')
# Commit hashes are written out in full and not as code, so that GitHub
# links them (shown shortened).
body="<!-- rust-semantic-review $state -->
### Rust Semantic Review

- **[All changes]($PAGE)** up to $HEAD: $SUMMARY"
footer="Built by [this run]($RUN_URL)."
if [ -n "$since" ]; then
  from=$(jq -r '.from' <<< "$since")
  to=$(jq -r '.to' <<< "$since")
  upto=
  # An updated comment showed that commit itself, before this push.
  previous_is="previous comment"
  if [ "${UPDATE_COMMENT:-}" = true ]; then previous_is="previous push"; fi
  if [ "$to" != "$HEAD" ]; then upto=" up to $to"; fi
  body="$body
- **[Changes since]($(jq -r '.url' <<< "$since"))** $from ($previous_is)$upto: $(jq -r '.summary' <<< "$since")"
  footer="$footer \"Changes since\" leaves out what the base branch brought in."
  # The links expire with the earlier of the two artifacts.
  since_expires=$(jq -r '.expires // ""' <<< "$since")
  if [ -n "$since_expires" ] && { [ -z "$expires" ] || [[ "$since_expires" < "$expires" ]]; }; then
    expires=$since_expires
  fi
elif [ -n "$fresh" ] && [[ "$SINCE" == unavailable* ]]; then
  body="$body
- The changes since $PREVIOUS, the previous comment's commit, could not be shown ${SINCE#unavailable }."
fi
# For the whole PR, not only the changes since the previous comment: the
# question is whether merging it breaks the API.
if [ -n "$breaking" ]; then
  body="$body

$breaking"
fi
if [ -z "$fresh" ] && [ "$PREVIOUS" != "$HEAD" ]; then
  body="$body

Updated for $HEAD, which only merged the base branch or was rebased onto it."
fi
# The pages of the pushes before this one (with UPDATE_COMMENT), folded.
# Commits are shortened in a link's text, where GitHub doesn't do it.
earlier=$(jq -r '.[]
  | "- Up to \(.head): [all changes](\(.page.url))"
    + (if .page.summary then ": \(.page.summary)" else "." end)
    + (if .since.url then " [Changes since \(.since.from // "" | .[:7])](\(.since.url))." else "" end)' <<< "$history")
if [ -n "$earlier" ]; then
  body="$body

<details><summary>Earlier pushes</summary>

$earlier

</details>"
fi
# Anyone can read the comment of a public repository, but GitHub serves
# artifacts only to people who are signed in.
if [ -n "$expires" ]; then
  footer="$footer The pages need a GitHub login and expire on $(date -u -d "$expires" '+%-d %b %Y')."
else
  footer="$footer The pages need a GitHub login and expire with the run's artifacts."
fi
body="$body

<sub>$footer</sub>"
failed=
if [ -n "$patch" ]; then
  result=$(gh api -X PATCH "repos/$REPO/issues/comments/$PREVIOUS_ID" -f body="$body" 2>&1) || failed=1
else
  result=$(gh api -X POST "repos/$REPO/issues/$PR/comments" -f body="$body" 2>&1) || failed=1
fi
if [ -n "$failed" ]; then
  # E.g. a PR from a fork, whose token is read-only. The action then uploads
  # the data for the comment-only action.
  echo "posted=false" >> "$GITHUB_OUTPUT"
  if [ "${COMMENT_ONLY:-}" = true ]; then
    echo "::error::Could not comment on the PR (does the token have pull-requests: write?): $result"
    exit 1
  fi
  echo "::warning::Could not comment on the PR, e.g. because the token is read-only on PRs from forks. The data for the comment is uploaded, so that a workflow_run workflow with the comment-only action can post it (see the README): $result"
elif [ -z "$patch" ] && [ -n "$PREVIOUS_NODE_ID" ]; then
  # Collapse the previous comment: the new one has the current numbers and
  # links. Each new comment hides the one before it, so only the latest
  # stays expanded.
  gh api graphql -f id="$PREVIOUS_NODE_ID" -f query='
    mutation($id: ID!) {
      minimizeComment(input: { subjectId: $id, classifier: OUTDATED }) {
        minimizedComment { isMinimized }
      }
    }' > /dev/null \
    || echo "::warning::Could not hide the previous Rust Semantic Review comment"
fi
