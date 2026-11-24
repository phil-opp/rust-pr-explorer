# The numbers of the comment's summary, from the analysis' JSON: the changed
# Rust items (not modules), how many of them are public, and the other
# changed files: those the page lists as files, not as items (non-Rust
# files, and Rust files with changes outside every item). `semver` is null
# without cargo-semver-checks, "error" if it could not check the PR, else
# the number of its findings.
#
# `breaking` lists the first breaking changes (see action/breaking.jq), as
# the page's list of breaking changes has them: changed items of the public
# API whose change is breaking, in crates that don't allow breaking changes,
# then cargo-semver-checks' breaking findings that match no item.
# `breaking_total` counts all of them.
def kind_label:
  {method: "method", trait_method: "method", assoc_const: "const",
   type_alias: "type", assoc_type: "type"}[.] // .;
(.meta.versions // {}) as $versions
| [.items[] | select(.kind != "mod" and .status != "unchanged")] as $items
| ([.items[]
   | select(.status != "unchanged"
       and ((.public // false) or ((.changes.exports_removed // []) | length > 0))
       and .api.level == "breaking"
       and (($versions[.id | split("::")[0]].allow_breaking // false) | not))]
  | sort_by(.file, .line, .id)
  | map({item: .id,
         path: ((.public_paths // [])[0] // (.changes.exports_removed // [])[0] // .name),
         kind: (if .kind == "impl" then "" else .kind | kind_label end),
         why: (.api.reasons[0] // "")}))
  as $breaking_items
| [(.semver_checks.findings // [])[]
   | select(.level == "breaking" and .item == null)
   | {item: null, path: .message, kind: "", why: "cargo-semver-checks: \(.title)"}]
  as $unmatched
| ($breaking_items + $unmatched) as $breaking
| {
    items: ($items | length),
    public: ($items | map(select((.public_paths // []) | length > 0)) | length),
    files: ([.files[] | select((.analyzed // false | not) or (.outside_items // 0) > 0)] | length),
    semver: (if .semver_checks == null then null
             elif .semver_checks.error != null then "error"
             else .semver_checks.findings | length
             end),
    breaking: $breaking[:5],
    breaking_total: ($breaking | length)
  }
