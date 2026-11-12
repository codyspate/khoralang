#!/bin/sh
# Cutting a version from a tag must produce no `/main/` repository link --
# that is the whole bug this batch of scripts exists to fix. This runs the
# real `cut-docs.sh` -- not a re-implementation of its two steps -- against a
# scratch clone, from a directory other than the clone's root, so it also
# covers running the script from elsewhere (see `rewrite-released-links.sh`'s
# call site in `cut-docs.sh`, which broke exactly that way once).
#
#     sh scripts/test-cut-docs-no-main-links.sh v0.2.0
#
# Doesn't touch the real `website/content/versions` or `website/versions.mjs`:
# a full `cut-docs.sh` run edits the tree it is pointed at, and a local clone
# is a cheap throwaway for that (`--local --shared` costs disk for its own
# working copy, nothing for the objects, which are shared with this
# worktree). Deleted the moment the test is done with it.
set -eu

root=$(cd "$(dirname "$0")/.." && pwd)
cd "$root"

tag=${1:-v0.2.0}
work=$(mktemp -d "${TMPDIR:-/tmp}/khora-cut-docs-test.XXXXXX")
trap 'rm -rf "$work"' EXIT INT TERM

git rev-parse -q --verify "refs/tags/$tag" > /dev/null || {
    echo "test-cut-docs-no-main-links.sh: no such tag: $tag" >&2
    exit 1
}

# A local, shared clone: its objects are the same files as this worktree's,
# so this costs the size of a working copy of `website/content/docs` and
# `scripts/`, not a second copy of the whole repository's history.
git clone -q --local --shared --no-checkout "$root" "$work" > /dev/null
git -C "$work" -c init.defaultBranch=main checkout -q --detach HEAD

# `cut-docs.sh` as it stands in *this* worktree, not whatever the clone's
# checked-out commit happens to carry -- the fix under test is in the
# working tree, uncommitted.
mkdir -p "$work/scripts"
cp -r "$root/scripts/." "$work/scripts/"

# The clone's checkout already has this tag's tree and `versions.mjs` entry,
# committed by an earlier cut -- `cut-docs.sh` refuses a tree that already
# exists, so the scratch clone has to look like the tag has never been cut,
# the way it would before a release.
id=$(echo "$tag" | sed -E 's/^(v[0-9]+\.[0-9]+)\..*/\1/')
rm -rf "$work/website/content/versions/$id"

# **`node` gets a relative path, from inside `$work/website`.** `$work` is
# an absolute path built from `$TMPDIR`; under Git-for-Windows that is an
# MSYS path (`/tmp/...`) that `bash` resolves to a drive letter for every
# shell builtin and external command it starts itself, but `node` is a
# native Windows program handed the *string*, not a path `bash` resolved for
# it, and a bare `/tmp/...` string means something else to it (the current
# drive's root). `check-released-docs.sh` sidesteps this the same way: `cd`
# into the directory holding the file and pass `node` a name relative to
# where it's running, which needs no translation on any of the three.
#
# **The regex accepts `\r?\n`, not just `\n`.** `core.autocrlf=true` (the
# Windows default) checks `versions.mjs` out with CRLF line endings --
# `.gitattributes` pins `*.sh` to LF but says nothing about `.mjs`, on
# purpose (renormalizing it is a separate change, not this one's to make).
# A pattern hardcoding `\n` then matches nothing in the file Windows
# actually has on disk, so the entry is never removed and the next
# `cut-docs.sh` call refuses with "already lists v0.4" -- the clone looked
# like the tag was never cut, but the regex that was supposed to make it
# look that way silently did nothing.
( cd "$work/website" && node - "$id" <<'NODE'
const fs = require('fs');
const id = process.argv[2];
const path = 'versions.mjs';
let text = fs.readFileSync(path, 'utf8');
text = text.replace(new RegExp(`  \\{\\r?\\n    id: '${id}',[\\s\\S]*?\\r?\\n  \\},\\r?\\n`), '');
fs.writeFileSync(path, text);
NODE
)

# From a directory other than the clone's root, and by a relative path, which
# is exactly the shape that broke: `cd "$(dirname "$0")/.."` inside
# `cut-docs.sh` moves away from wherever `$0` was relative to, and a later
# call built from the pre-`cd` `$0` landed nowhere.
mkdir -p "$work/website"
( cd "$work/website" && sh ../scripts/cut-docs.sh "$tag" )

dest="$work/website/content/versions/$id"
[ -d "$dest" ] || { echo "  FAILED  cut-docs.sh did not create $dest" >&2; exit 1; }

# A link naming `CHANGELOG.md`/`CONTRIBUTING.md` is correctly left on `main`
# (`released-links-main-allowlist.txt`), so a plain count of `main` links
# would fail on a correct cut. `scripts/plant-main-link-cases.sh` is the test
# for the other spellings `main` can show up as; this one is only about
# `cut-docs.sh` itself doing the rewrite it is supposed to.
allow=$(grep -v '^#' "$root/scripts/released-links-main-allowlist.txt" | grep -v '^$' | paste -sd '|' -)
main_hits=$(grep --include='*.md' -rohE 'github\.com/codyspate/khoralang/(blob|tree)/main/[^) ]*' "$dest") && main_rc=0 || main_rc=$?
if [ "$main_rc" -gt 1 ]; then
    echo "  FAILED  grep failed (exit $main_rc) looking for blob/main or tree/main links under $dest" >&2
    exit "$main_rc"
fi
stray=$(printf '%s\n' "$main_hits" | grep -v '^$' | grep -vE "/($allow)(\$|[^A-Za-z0-9_.-])") && stray_rc=0 || stray_rc=$?
if [ "$stray_rc" -gt 1 ]; then
    echo "  FAILED  grep failed (exit $stray_rc) filtering the allow-list" >&2
    exit "$stray_rc"
fi
if [ -n "$stray" ]; then
    echo "  FAILED  $dest still links blob/main or tree/main outside the allow-list:" >&2
    printf '%s\n' "$stray" | sed 's/^/    /' >&2
    exit 1
fi

if ! grep -q "cutFrom: '$tag'" "$work/website/versions.mjs"; then
    echo "  FAILED  versions.mjs has no cutFrom: '$tag' after the cut" >&2
    exit 1
fi

echo "  ok    cut-docs.sh $tag, run from website/ in a scratch clone: 0 main links, versions.mjs updated"
