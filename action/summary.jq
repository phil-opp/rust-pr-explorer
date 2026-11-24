# The comment's one-line summary from counts.jq's numbers. The comment-only
# action reads the numbers from an artifact of a run on the PR's code, so
# anything but the expected types is refused. Test items are named apart
# from the other Rust items (`tests` is missing in the numbers of older
# versions of the action, which counted them as items).
def count: if type == "number" and . >= 0 and . == floor then . else error("not a count") end;
def plural($word): if . == 1 then "1 \($word)" else "\(.) \($word)s" end;
(.items | count) as $n
| (.public | count) as $p
| (.tests // 0 | count) as $t
| (.files | count) as $f
| .semver as $s
| ($f | plural("other changed file")) as $other
| ($n | plural("Rust item")) as $items
| ($t | plural("test item")) as $tests
| (if $p > 0 then " (\($p) public)" else "" end) as $public
| if $n == 0 and $t == 0 and $f == 0 then "no changes."
  else
    (if $n == 0 and $t == 0 then "no Rust items changed"
     elif $t == 0 then "\($items) changed\($public)"
     elif $n == 0 then "\($tests) changed, no other Rust items"
     else "\($items)\($public) and \($tests) changed"
     end)
    + (if $f > 0 then ", \($other)." else "." end)
  end
+ (if $s == null then ""
   elif $s == "error" then " cargo-semver-checks could not check this PR."
   else " cargo-semver-checks: \($s | count) findings."
   end)
