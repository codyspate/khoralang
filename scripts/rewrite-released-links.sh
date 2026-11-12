#!/bin/sh
# Points every `blob/main`/`tree/main` repository link in a cut documentation
# tree at the tag it was cut from, instead of at whatever is on `main` today.
#
#     sh scripts/rewrite-released-links.sh website/content/versions/v0.4 v0.4.0
#
# # The bug this undoes
#
# A page under `website/content/versions/<id>/` is a copy of `main` taken at
# a tag, but every repository link on it still reads `blob/main`: a reader of
# the 0.4 docs who follows one lands on today's `main`, not on what 0.4.0
# published. `scripts/cut-docs.sh` now calls this on every future cut, so the
# bug cannot recur there; this script is also how the four trees already cut
# (`v0.1`-`v0.4`) are fixed once, by hand, each against its own tag.
#
# # What "fix" means for a link with nothing at that tag
#
# A rewritten link is only right if the file exists at the tag -- otherwise
# the fix trades one wrong link for a 404. This script does not check that;
# `scripts/check-released-docs.sh` does, for every stable tree, as part of the
# gate.
#
# # One `sed` spelling, three platforms
#
# Branch CI runs this on GNU sed (Linux), BSD sed (macOS) and Git-for-Windows'
# own GNU sed (Windows), and the first version of this only worked on the
# first of those:
#
#   - BSD sed's basic regular expressions have no `\|` alternation at all --
#     `\(a\|b\)` is an error ("\1 not defined in the RE"), not a silent
#     no-op. So every pattern here is `-E` (extended), with plain `(a|b)`
#     groups and `\1`, and `$allow` is built with a plain `|` rather than the
#     BRE-flavoured `\|` an earlier version escaped it to.
#   - `sed -i` with no suffix argument is GNU-only; BSD sed reads the next
#     argument as the suffix and tries to edit a file named after whatever
#     follows. Avoided entirely: every edit here writes to `$file.tmp` and
#     `mv`s it over the original, which is one behaviour on every `sed`.
set -eu

root=$(cd "$(dirname "$0")/.." && pwd)
cd "$root"

tree=${1:?usage: rewrite-released-links.sh <tree-dir> <tag>}
tag=${2:?usage: rewrite-released-links.sh <tree-dir> <tag>}

[ -d "$tree" ] || {
    echo "rewrite-released-links.sh: no such directory: $tree" >&2
    exit 1
}
git rev-parse -q --verify "refs/tags/$tag" > /dev/null || {
    echo "rewrite-released-links.sh: no such tag: $tag" >&2
    exit 1
}

# The paths in `released-links-main-allowlist.txt`, as an ERE alternation
# (`CHANGELOG\.md|CONTRIBUTING\.md|...`) -- plain `|`, which is what `-E`
# below needs; `\|` is BRE syntax and BSD sed's BRE doesn't have it at all.
allow=$(grep -v '^#' "$root/scripts/released-links-main-allowlist.txt" | grep -v '^$' \
    | sed 's/[.[\*^$]/\\&/g' | paste -sd '|' -)

# `blob`, `tree`, `raw`, `edit`, `commits` and `blame` are all GitHub paths
# that take a ref next, and a page has used `raw` to link a file rendered
# plain. `HEAD` and `refs/heads/main` are the other two spellings of
# "whatever `main` is right now" GitHub accepts in the same slot.
kinds='blob|tree|raw|edit|commits|blame'
refs='main|HEAD|refs/heads/main'

changed=0
for file in $(find "$tree" \( -name '*.md' -o -name '*.mdx' \)); do
    if grep -E "github\.com/codyspate/khoralang/($kinds)/($refs)(/|\`|\\\$)" "$file" > /dev/null \
        || grep -F 'raw.githubusercontent.com/codyspate/khoralang/main/' "$file" > /dev/null; then
        sed -E \
            -e "s#github\\.com/codyspate/khoralang/($kinds)/($refs)#github.com/codyspate/khoralang/\\1/$tag#g" \
            -e "s#raw\\.githubusercontent\\.com/codyspate/khoralang/main/#raw.githubusercontent.com/codyspate/khoralang/$tag/#g" \
            "$file" > "$file.tmp"
        mv "$file.tmp" "$file"
        # The rewrite above is wholesale and also caught the allow-listed
        # paths, which matched the same pattern. Put just those back: a link
        # to a living document -- the changelog, the contributing guide, the
        # installer -- means today's, not the tag's -- see
        # `released-links-main-allowlist.txt`.
        sed -E \
            -e "s#github\\.com/codyspate/khoralang/blob/$tag/($allow)#github.com/codyspate/khoralang/blob/main/\\1#g" \
            -e "s#github\\.com/codyspate/khoralang/tree/$tag/($allow)#github.com/codyspate/khoralang/tree/main/\\1#g" \
            -e "s#raw\\.githubusercontent\\.com/codyspate/khoralang/$tag/($allow)#raw.githubusercontent.com/codyspate/khoralang/main/\\1#g" \
            "$file" > "$file.tmp"
        mv "$file.tmp" "$file"
        changed=$((changed + 1))
    fi
done

echo "  rewrote links in $changed page(s) under $tree to $tag"
