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

cd "$(dirname "$0")/.."

tag=${1:?usage: cut-docs.sh <tag>        e.g. cut-docs.sh v0.2.0}

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

echo "cutting $dest from $tag ..."
mkdir -p "$dest"
git archive "$tag" website/content/docs | tar -x --strip-components=3 -C "$dest"

pages=$(find "$dest" -name '*.md' | wc -l | tr -d ' ')
[ "$pages" -gt 0 ] || {
    echo "cut-docs.sh: $tag has no pages under website/content/docs" >&2
    rm -rf "$dest"
    exit 1
}
echo "  $pages page(s)"

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

// After `next`'s entry: the first `},` that follows `id: 'next'`.
const anchor = text.indexOf("id: 'next'");
if (anchor < 0) throw new Error('versions.mjs has no `next` entry to insert after');
const close = text.indexOf('\n  },\n', anchor);
if (close < 0) throw new Error('could not find the end of the `next` entry');
const at = close + '\n  },\n'.length;

const entry = `  {
    id: '${id}',
    label: '${id}',
    stable: true,
    /// The tag this tree was cut from, byte for byte.
    cutFrom: '${tag}',
    from: 'content/versions/${id}',
  },
`;

fs.writeFileSync(path, text.slice(0, at) + entry + text.slice(at));
console.log(`  versions.mjs now lists ${id}, cut from ${tag}`);
NODE

cat <<EOF

Cut. What follows from it, with no further edits:

  /docs/                  -> /docs/$id/      (newest stable)
  /docs/$id/              the pages as $tag published them
  /docs/next/             keeps being the working tree

Build the site to confirm, then commit both the tree and versions.mjs.
EOF
