# The comment's list of breaking changes, as Markdown, from counts.jq's
# numbers and the page's URL (`--arg url`); empty if there are none. Each
# item links to it on the page (`#/overview/item/<id>`, which opens its
# row on its module page).
#
# The comment-only action reads the list from an artifact of a run on the
# PR's code, so all of it is reduced to what such a list needs: the ids are
# only used encoded in the link, and the texts keep a few characters only,
# none that make a link, mention, reference or markup. Code (in backticks)
# stays code.
def count: if type == "number" and . >= 0 and . == floor then . else error("not a count") end;
def str: if type == "string" then .[:300] else "" end;
def code: gsub("[^A-Za-z0-9_:<>,&'*()!?=+ .\\[\\]-]"; "") | if . == "" then "" else "`\(.)`" end;
def plain: gsub("[^A-Za-z0-9 ,;:'()-]"; "");
# Text with code in backticks, e.g. "variant `B` added".
def text: str | [splits("`")] | to_entries
  | map(if .key % 2 == 1 then .value | code else .value | plain end) | join("");
["mod", "struct", "enum", "union", "trait", "fn", "method", "const", "static", "type", "macro", "variant"] as $kinds
| (. // {}) | (.breaking_total // 0 | count) as $n
| if $n == 0 then empty else
    ((.breaking // []) | if type == "array" then .[:5] else [] end) as $list
    | "**⚠ \($n) breaking \(if $n == 1 then "change" else "changes" end)** to the public API, by Cargo's SemVer rules:\n"
    + ($list | map(
        (if (.kind | IN($kinds[])) then "\(.kind) " else "" end)
        + (if (.item | type) == "string" and $url != ""
           # No brackets or parentheses in a link's text.
           then "[\(.path | str | gsub("[\\[\\]()]"; "") | code)](\($url)#/overview/item/\(.item[:300] | @uri))"
           else .path | str | code end)
        + ((.why | text) as $w | if $w == "" then "" else ": \($w)" end)
        | "- " + .) | join("\n"))
    + (if $n > ($list | length) then "\n- and \($n - ($list | length)) more on the page." else "" end)
  end
