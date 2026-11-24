# The comment's one-line summary from counts.jq's numbers. The comment-only
# action reads the numbers from an artifact of a run on the PR's code, so
# anything but the expected types is refused.
def count: if type == "number" and . >= 0 and . == floor then . else error("not a count") end;
(.items | count) as $n
| (.public | count) as $p
| (.files | count) as $f
| .semver as $s
| (if $f == 1 then "1 other changed file" else "\($f) other changed files" end) as $other
| if $n == 0 and $f == 0 then "no changes."
  elif $n == 0 then "no Rust items changed, \($other)."
  else "\($n) Rust \(if $n == 1 then "item" else "items" end) changed"
    + (if $p > 0 then " (\($p) public)" else "" end)
    + (if $f > 0 then ", \($other)." else "." end)
  end
+ (if $s == null then ""
   elif $s == "error" then " cargo-semver-checks could not check this PR."
   else " cargo-semver-checks: \($s | count) findings."
   end)
