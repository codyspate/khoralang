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

root=$(cd "$(dirname "$0")/.." && pwd)
cd "$root"

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

# The two paths in `released-links-main-allowlist.txt`, as a regex
# alternation, built once rather than per tree.
allow=$(grep -v '^#' "$root/scripts/released-links-main-allowlist.txt" | grep -v '^$' \
    | sed 's/[.[\*^$]/\\&/g' | paste -sd '|' -)

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

    case "$tag" in
        *-*)
            echo "  $id: cutFrom names \`$tag\`, a pre-release candidate, not a release" >&2
            : > "$failed"
            continue
            ;;
    esac

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

    # The archived copy still reads `blob/main`/`tree/main`, as the tag itself
    # does; the tree being compared against it has those pinned to the tag.
    # Without this, every page carrying a repository link reports as
    # "corrected since" the tag, drowning the real corrections -- fifteen of
    # them, among eighty-one rows, the time this was tried without it. The
    # comparison has to be tag-with-links-pinned against tree, not tag against
    # tree, or it is comparing two different things and calling the
    # difference drift.
    sh "$root/scripts/rewrite-released-links.sh" "$work/$id/website/content/docs" "$tag"

    drift=$(diff -r --strip-trailing-cr "$work/$id/website/content/docs" "$tree" 2>&1 \
        | grep -E '^(Only in|diff -r|Files )' | wc -l | tr -d ' ')

    if [ "$drift" = "0" ]; then
        echo "  $id is $tag, unchanged"
    else
        echo "  $id is $tag, with $drift page(s) corrected since:"
        diff -rq --strip-trailing-cr "$work/$id/website/content/docs" "$tree" 2>&1 \
            | sed 's/^/    /'
    fi

    # A link to `blob/main`/`tree/main` (or one of GitHub's other ref-shaped
    # paths, or `raw.githubusercontent.com`) in a cut tree points a reader of
    # a past release at today's `main`, not at what that release published --
    # the bug this tree exists to keep from recurring. Unlike drift above,
    # this is never legitimate, with the exceptions in
    # `released-links-main-allowlist.txt`: a link to a living document --
    # the changelog, the contributing guide, the installer -- is correctly
    # left on `main`, because what it says is about the current one, not the
    # one the tag had.
    #
    # **No `-P`, no lookahead.** The previous version matched with `grep -P`
    # and a negative lookahead for the allow-list, and macOS's `grep` (BSD)
    # doesn't have `-P` at all: it refused the option, exited non-zero, and
    # the surrounding `2>/dev/null || true` turned that refusal into "stale
    # links: none" rather than a loud failure -- on macOS the gate never
    # looked for a single `main`-pointing link and still passed. Done here
    # with two ordinary `-E` passes instead, which need nothing a basic
    # POSIX-ish `grep` doesn't have: first every matching line, then a
    # second `grep -v` to drop the ones whose path names an allow-listed
    # file. Neither pass can silently find "nothing" through an option it
    # doesn't support, because neither uses an option it needs to.
    main_hits=$(grep --include='*.md' --include='*.mdx' -rnoE "github\.com/codyspate/khoralang/(blob|tree|raw|edit|commits|blame)/(main|HEAD|refs/heads/main)/[^) \`\"#]*" \
        "$tree") && main_rc=0 || main_rc=$?
    if [ "$main_rc" -gt 1 ]; then
        echo "  $id: grep failed (exit $main_rc) looking for main-pointing links" >&2
        : > "$failed"
        continue
    fi
    raw_hits=$(grep --include='*.md' --include='*.mdx' -rnoE "raw\.githubusercontent\.com/codyspate/khoralang/main/[^) \`\"#]*" \
        "$tree") && raw_rc=0 || raw_rc=$?
    if [ "$raw_rc" -gt 1 ]; then
        echo "  $id: grep failed (exit $raw_rc) looking for raw.githubusercontent.com links" >&2
        : > "$failed"
        continue
    fi
    # `grep -v` below can itself find "every line matched" (exit 1, meaning
    # zero lines left) when every hit was allow-listed -- that is a correct,
    # ordinary outcome here, not an error, so it alone is let through to
    # `stale_links=""`.
    stale_links=$(printf '%s\n%s\n' "$main_hits" "$raw_hits" | grep -v '^$' \
        | grep -vE "/(main|HEAD|refs/heads/main)/($allow)([^A-Za-z0-9_.-]|\$)" \
        | cut -d: -f1 | sort -u)
    if [ -n "$stale_links" ]; then
        echo "  $id: a main-pointing repository link, pointing past releases at today's main:" >&2
        echo "$stale_links" | sed 's/^/    /' >&2
        : > "$failed"
    fi

    # And a link pinned to the tag must name something the tag has, or the
    # fix traded a wrong link for a 404. Each one is looked up in the tag.
    #
    # **Grep's own exit status is caught, not discarded through `2>/dev/null`
    # into a pipe.** `grep` exiting 1 ("no matches") is the ordinary case --
    # a tree's links can all resolve -- but exiting 2 (a real error) must
    # still fail the gate, and piping straight into `sed`/`while` hid that:
    # the pipeline's own exit status comes from `while`, not `grep`, so a
    # `grep` error upstream produced an empty line list and a silent "all
    # paths resolve" rather than a failure.
    tag_hits=$(grep --include='*.md' --include='*.mdx' -rohE "github.com/codyspate/khoralang/(blob|tree|raw|edit|commits|blame)/$tag/[^) \`\"#]*" "$tree") && tag_rc=0 || tag_rc=$?
    if [ "$tag_rc" -gt 1 ]; then
        echo "  $id: grep failed (exit $tag_rc) looking for links pinned to $tag" >&2
        : > "$failed"
        continue
    fi
    printf '%s\n' "$tag_hits" | grep -v '^$' \
        | sed -E "s#github.com/codyspate/khoralang/(blob|tree|raw|edit|commits|blame)/$tag/##; s#/\$##" | sort -u \
        | while IFS= read -r path; do
            [ -n "$path" ] || continue
            if ! git cat-file -e "$tag:$path" 2>/dev/null; then
                echo "  $id: links $path at $tag, which has no such path" >&2
                : > "$failed"
            fi
        done
    raw_tag_hits=$(grep --include='*.md' --include='*.mdx' -rohE "raw\.githubusercontent\.com/codyspate/khoralang/$tag/[^) \`\"#]*" "$tree") && raw_tag_rc=0 || raw_tag_rc=$?
    if [ "$raw_tag_rc" -gt 1 ]; then
        echo "  $id: grep failed (exit $raw_tag_rc) looking for raw-links pinned to $tag" >&2
        : > "$failed"
        continue
    fi
    printf '%s\n' "$raw_tag_hits" | grep -v '^$' \
        | sed -E "s#raw\\.githubusercontent\\.com/codyspate/khoralang/$tag/##; s#/\$##" | sort -u \
        | while IFS= read -r path; do
            [ -n "$path" ] || continue
            if ! git cat-file -e "$tag:$path" 2>/dev/null; then
                echo "  $id: raw-links $path at $tag, which has no such path" >&2
                : > "$failed"
            fi
        done
done

if [ -f "$failed" ]; then
    exit 1
fi
