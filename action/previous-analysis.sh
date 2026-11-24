#!/usr/bin/env bash
# Downloads the analysis of the previous comment's commit, which the run
# that made that comment saved as the `rust-semantic-review-analysis`
# artifact, so that the tool can reuse it for the page of the changes since
# then instead of analyzing that commit again.
#
# Usage: previous-analysis.sh DIR, with GH_TOKEN, REPO, PREVIOUS (the
# previous comment's commit) and TRUST (this run's trust-project-code) set,
# and DIR/previous.json from previous-comment.sh. Writes
# DIR/since-analysis.json.gz if there is one this run may use, and else
# says why not. Reading the artifact needs `actions: read`.
#
# The analysis is untrusted input. The tool only uses it if it is of the
# same tree, by the same build of the tool, with the same settings and
# toolchain (see crates/rust-semantic-review/src/reuse.rs), but a made-up
# file can claim all of that. So it is only taken from a run that ran this
# repository's own code, or when this run runs the PR's code anyway:
# - A `pull_request` run of a PR from a fork ran the workflow as the fork
#   has it, so the fork controls its artifacts: it could hide changes from
#   the next run's page of the changes since then. Its analysis is only
#   used if this run trusts the project code, which lets the PR's build
#   scripts change this run's pages as much.
# - Every other run (a PR from a branch of this repository, whose authors
#   can push to it, or e.g. `pull_request_target` or `workflow_dispatch`,
#   which run the workflow from this repository) counts as trusted.
set -euo pipefail
dir=$1
out="$dir/since-analysis.json.gz"
rm -f "$out"
# The largest artifact that is downloaded (it is gzipped already). A large
# workspace's (dora's) is a few tens of MB.
max=$((512 << 20))

skip() { echo "::notice::Analyzing $PREVIOUS again for the changes since then: $1"; exit 0; }

run=$(jq -r '.run // "" | tostring' "$dir/previous.json" 2>/dev/null || true)
if ! [[ "$run" =~ ^[0-9]{1,20}$ ]]; then
  skip "the previous comment names no run (it is older than this feature)"
fi
info=$(gh api "repos/$REPO/actions/runs/$run" \
  --jq '{event, head_sha, head_repo: (.head_repository.full_name // "")} | @json' 2>&1) \
  || skip "could not read run $run (does the token have actions: read?): $info"
if [ "$(jq -r '.head_sha' <<< "$info")" != "$PREVIOUS" ]; then
  skip "run $run analyzed another commit"
fi
event=$(jq -r '.event' <<< "$info")
head_repo=$(jq -r '.head_repo' <<< "$info")
case "$event" in
  pull_request_target) fork=false ;;
  pull_request*) if [ "$head_repo" = "$REPO" ]; then fork=false; else fork=true; fi ;;
  *) fork=false ;;
esac
if [ "$fork" = true ] && [ "$TRUST" != true ]; then
  skip "run $run ran the workflow of the fork $head_repo, which controls its artifacts, and this run doesn't run the PR's code"
fi

artifact=$(gh api --paginate "repos/$REPO/actions/runs/$run/artifacts" \
  --jq '.artifacts[] | select(.name == "rust-semantic-review-analysis" and (.expired | not)) | {id, size_in_bytes} | @json' 2>&1 | tail -n1) \
  || skip "could not list the artifacts of run $run: $artifact"
if [ -z "$artifact" ] || ! jq -e 'type == "object"' <<< "$artifact" > /dev/null 2>&1; then
  skip "run $run has no rust-semantic-review-analysis artifact (expired, or saved by an older version)"
fi
if [ "$(jq '.size_in_bytes' <<< "$artifact")" -gt "$max" ]; then
  skip "its artifact is larger than $((max >> 20)) MB"
fi
start=$(date +%s)
gh api "repos/$REPO/actions/artifacts/$(jq '.id' <<< "$artifact")/zip" > "$dir/analysis.zip" 2> "$dir/analysis.zip.err" \
  || skip "could not download the artifact of run $run: $(head -c 300 "$dir/analysis.zip.err")"
# Only the one file, to stdout: no paths from the archive are used. Cut off
# at the limit, a larger file fails to unpack in the tool.
unzip -p "$dir/analysis.zip" head-analysis.json.gz 2> /dev/null | head -c "$max" > "$out" || true
rm -f "$dir/analysis.zip" "$dir/analysis.zip.err"
if ! [ -s "$out" ]; then
  rm -f "$out"
  skip "the artifact of run $run holds no analysis"
fi
echo "Downloaded the analysis of $PREVIOUS from run $run ($(($(stat -c %s "$out") >> 20)) MB) in $(($(date +%s) - start)) s."
