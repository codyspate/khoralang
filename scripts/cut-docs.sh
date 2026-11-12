#!/bin/sh
# Cut a released documentation tree from its tag.
#
#     sh scripts/cut-docs.sh v0.2.0
#
# Copies `website/content/docs` **as the tag left it** into
# `website/content/versions/<id>`, and adds the entry to `website/versions.mjs`
# so the switcher, the sidebar, the banners and `/docs/` all follow.
#
# # Why this is a script and not a paragraph
#
# It was a comment in `versions.mjs` describing two `git` commands to run by
# hand. The result, after `v0.1.0`: `khora --version` says one minor, `/docs/`
# redirects to the previous one, `/docs/v0.2/` is a 404, and the manifest
# reference exists only on a tree the version switcher does not offer. A
# newcomer reads that as "my compiler is ahead of its documentation" and starts
# discounting correct pages. Every release step that is a paragraph gets skipped
# eventually; this one had already been.
#
# # From the tag, never from the working tree
#
# `git archive <tag>` is the whole point. A tree copied from whatever happened
# to be checked out cannot honestly claim to document what that tag published,
# and `scripts/check-released-docs.sh` exists to notice the difference later.
#
# **A cut tree can have broken links, and the site build will say so.** Pages
# get corrected after a release -- `v0.1` has three such corrections, including
# two links to a `#catch` heading that the tag's own pages do not have -- so
# re-cutting an already-published version discards those fixes and the link
# checker fails. That is the right failure: it means the corrections have to be
# re-applied deliberately rather than lost quietly. For a first cut of a new
# version there is nothing to lose and this does not arise.
set -eu

root=$(cd "$(dirname "$0")/.." && pwd)
cd "$root"

tag=${1:?usage: cut-docs.sh <tag>        e.g. cut-docs.sh v0.2.0}

case "$tag" in
  *-*)
    echo "cut-docs.sh: $tag is a pre-release; candidates stay in next" >&2
    exit 1
    ;;
esac

git rev-parse -q --verify "refs/tags/$tag" > /dev/null || {
    echo "cut-docs.sh: no such tag: $tag" >&2
    echo "  tag the release first; this cuts documentation *from* a tag." >&2
    exit 1
}

# `v0.2.0` -> `v0.2`. The section granularity is the minor before 1.0 and the
# major after, which is the rule `versions.mjs` states and this follows rather
# than restates.
case "$tag" in
  v0.*) id=$(echo "$tag" | cut -d. -f1,2) ;;
  v*)   id=$(echo "$tag" | cut -d. -f1) ;;
  *)    echo "cut-docs.sh: a tag starts with v: $tag" >&2; exit 1 ;;
esac

dest="website/content/versions/$id"

if [ -e "$dest" ]; then
    echo "cut-docs.sh: $dest already exists, so $id has been cut" >&2
    echo "  remove it first if you mean to re-cut." >&2
    exit 1
fi

# A failure between here and the `versions.mjs` edit -- the rewrite failing,
# an interrupt -- must not leave a half-made `$dest` behind: that blocks a
# rerun with "already exists" on a tree that was never finished. Cleared on a
# clean exit, right before the closing message.
trap 'rm -rf "$dest"' EXIT INT TERM

echo "cutting $dest from $tag ..."
mkdir -p "$dest"
git archive "$tag" website/content/docs | tar -x --strip-components=3 -C "$dest"

pages=$(find "$dest" -name '*.md' | wc -l | tr -d ' ')
[ "$pages" -gt 0 ] || {
    echo "cut-docs.sh: $tag has no pages under website/content/docs" >&2
    exit 1
}
echo "  $pages page(s)"

# `next` documents `main` and its repository links say so correctly; a tree
# cut from a tag documents that tag and must say so too, or a reader of a
# past release gets sent to today's `main` instead. Done here so the bug
# this exists to prevent cannot recur in a tree cut after this script was
# fixed; `scripts/check-released-docs.sh` catches it if it does anyway.
#
# **By the absolute path, not `$(dirname "$0")`.** That was wrong the moment
# this script was called from anywhere but the root -- `cd "$root"` above
# already moved away from wherever `$0` was relative to -- and the failure
# left a half-made `$dest` with no `versions.mjs` entry, which the trap above
# now also cleans up.
sh "$root/scripts/rewrite-released-links.sh" "$dest" "$tag"

# The entry, inserted ahead of the previous newest so the list stays newest
# first. `next` is always first and is never stable, so the insertion point is
# the line after its closing brace.
node - "$id" "$tag" <<'NODE'
const fs = require('fs');
const [id, tag] = process.argv.slice(2);
const path = 'website/versions.mjs';
const text = fs.readFileSync(path, 'utf8');

if (text.includes(`id: '${id}'`)) {
  console.error(`cut-docs.sh: versions.mjs already lists ${id}`);
  process.exit(1);
}

// **The line ending actually in the file, not assumed.** `core.autocrlf=true`
// (the Windows default) checks this file out with CRLF; `.gitattributes`
// pins `*.sh` to LF but says nothing about `.mjs`, and shouldn't start now --
// renormalizing it is a separate change. A search hardcoding `\n` finds
// nothing in a CRLF file and throws "could not find the end of the `next`
// entry" on every Windows cut. Detected once, from the first line ending
// after the anchor, and used for both the search and the inserted entry, so
// a cut never writes CRLF and LF mixed in the same file.
const anchor = text.indexOf("id: 'next'");
if (anchor < 0) throw new Error('versions.mjs has no `next` entry to insert after');
// The first line ending after the anchor decides it: if a `\r` sits right
// before that `\n`, the file is CRLF throughout (checked out that way by
// git, consistently) and stays CRLF; otherwise it is LF.
const firstNewline = text.indexOf('\n', anchor);
const eol = firstNewline > 0 && text[firstNewline - 1] === '\r' ? '\r\n' : '\n';
const closer = `${eol}  },${eol}`;
const close = text.indexOf(closer, anchor);
if (close < 0) throw new Error('could not find the end of the `next` entry');
const at = close + closer.length;

const entry = [
  '  {',
  `    id: '${id}',`,
  `    label: '${id}',`,
  '    stable: true,',
  '    /// The tag this tree was cut from, with its repository links pinned to it.',
  `    cutFrom: '${tag}',`,
  `    from: 'content/versions/${id}',`,
  '  },',
  '',
].join(eol);

fs.writeFileSync(path, text.slice(0, at) + entry + text.slice(at));
console.log(`  versions.mjs now lists ${id}, cut from ${tag}`);
NODE

cat <<EOF

Cut. What follows from it, with no further edits:

  /docs/                  -> /docs/$id/      (newest stable)
  /docs/$id/              the tag $tag published, with repository links pinned to it
  /docs/next/             keeps being the working tree

Build the site to confirm, then commit both the tree and versions.mjs.
EOF

# Reached only on success: the cut is meant to be kept from here, so the
# cleanup trap must not remove it on a normal exit.
trap - EXIT
