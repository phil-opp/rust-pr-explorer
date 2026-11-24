# The numbers of the comment's summary, from the analysis' JSON: the changed
# Rust items (not modules), how many of them are public, and the other
# changed files: those the page lists as files, not as items (non-Rust
# files, and Rust files with changes outside every item). `semver` is null
# without cargo-semver-checks, "error" if it could not check the PR, else
# the number of its findings.
[.items[] | select(.kind != "mod" and .status != "unchanged")] as $items
| {
    items: ($items | length),
    public: ($items | map(select((.public_paths // []) | length > 0)) | length),
    files: ([.files[] | select((.analyzed // false | not) or (.outside_items // 0) > 0)] | length),
    semver: (if .semver_checks == null then null
             elif .semver_checks.error != null then "error"
             else .semver_checks.findings | length
             end)
  }
