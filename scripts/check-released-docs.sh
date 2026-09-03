#!/bin/sh
# Every released documentation tree came from the tag it says it did.
#
# `website/versions.mjs` gives a stable version a `cutFrom` naming the tag its
# pages were taken from, and `/docs/v0.1/` claims to document what `v0.1.0`
# published. A tree copied from whatever happened to be checked out that
# afternoon cannot make that claim, and nothing about the directory itself
# would show the difference.
#
# **Drift is allowed and is reported, not failed.** Documentation gets
# corrected after a release -- that is most of why versioned trees exist -- so
# a page differing from the tag is legitimate. What is not acceptable is for it
# to be invisible. A missing tag or a missing directory is a real failure,
# because then the claim has nothing behind it at all.
set -eu

cd "$(dirname "$0")/.."

if ! command -v node > /dev/null 2>&1; then
    echo "!! node is not installed, so the released documentation was not checked" >&2
    exit 1
fi

# `id<tab>cutFrom<tab>from` for each stable version, from the one list.
stable=$(cd website && node -e '
  import("./versions.mjs").then((m) => {
    for (const each of m.versions.filter((v) => v.stable)) {
      process.stdout.write([each.id, each.cutFrom ?? "", each.from].join("\t") + "\n");
    }
  });
')

if [ -z "$stable" ]; then
    echo "  no released documentation trees yet"
    exit 0
fi

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT INT TERM

# **The loop runs in a subshell, because it is fed by a pipe.** A counter
# incremented in there is lost at the closing `done`, which would make every
# failure below report itself and then exit 0 -- a gate that prints its own
# bad news and passes. A file survives the subshell; a variable does not.
failed="$work/failed"

echo "$stable" | while IFS="$(printf '\t')" read -r id tag from; do
    [ -n "$id" ] || continue

    if [ -z "$tag" ]; then
        echo "  $id: stable, and no cutFrom saying which tag it came from" >&2
        : > "$failed"
        continue
    fi

    if ! git rev-parse -q --verify "refs/tags/$tag" > /dev/null; then
        echo "  $id: cutFrom names \`$tag\`, which is not a tag in this repository" >&2
        : > "$failed"
        continue
    fi

    tree="website/$from"
    if [ ! -d "$tree" ]; then
        echo "  $id: no directory at $tree" >&2
        : > "$failed"
        continue
    fi

    # `git archive` writes the committed bytes, which end their lines the way
    # the repository stores them; a working tree on Windows does not. So the
    # comparison ignores the carriage return rather than reporting all 77 pages
    # as changed, which is what a naive `diff -r` does here.
    rm -rf "$work/$id"
    mkdir -p "$work/$id"
    git archive "$tag" website/content/docs | tar -x -C "$work/$id"

    drift=$(diff -r --strip-trailing-cr "$work/$id/website/content/docs" "$tree" 2>&1 \
        | grep -E '^(Only in|diff -r|Files )' | wc -l | tr -d ' ')

    if [ "$drift" = "0" ]; then
        echo "  $id is $tag, unchanged"
    else
        echo "  $id is $tag, with $drift page(s) corrected since:"
        diff -rq --strip-trailing-cr "$work/$id/website/content/docs" "$tree" 2>&1 \
            | sed 's/^/    /'
    fi
done

if [ -f "$failed" ]; then
    exit 1
fi
