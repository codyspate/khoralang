#!/bin/sh
# Plants one `main`-pointing link at a time in a scratch copy of a cut tree,
# runs the gate, and expects it to fail -- covering the spellings `main` can
# show up as besides `blob/main`: `tree`, `raw`, `edit`, `commits`, `blame`,
# `HEAD`, `refs/heads/main`, `raw.githubusercontent.com`, and the same inside
# an `.mdx` file. A control (an already-pinned link, and an allow-listed one)
# must pass. After each plant the file is restored, so this never leaves the
# tree it runs against changed.
#
#     sh scripts/plant-main-link-cases.sh
set -eu

root=$(cd "$(dirname "$0")/.." && pwd)
cd "$root"

target=website/content/versions/v0.1/zz-plant.md
mdx_target=website/content/versions/v0.1/zz-plant.mdx
[ ! -e "$target" ] || { echo "plant-main-link-cases.sh: $target already exists" >&2; exit 1; }
[ ! -e "$mdx_target" ] || { echo "plant-main-link-cases.sh: $mdx_target already exists" >&2; exit 1; }
cleanup() { rm -f "$target" "$mdx_target"; }
trap cleanup EXIT INT TERM

gate() {
    TMPDIR="${TMPDIR:-/tmp}" sh scripts/check-released-docs.sh > "${TMPDIR:-/tmp}/plant-gate.log" 2>&1
}

fail=0
plant() {
    desc=$1
    line=$2
    want=$3
    printf '%s\n' "$line" > "$target"
    rc=0
    gate || rc=$?
    rm -f "$target"
    case "$want:$rc" in
        caught:0 | clean:[!0])
            echo "  FAILED  $desc: expected $want, gate exited $rc" >&2
            cat "${TMPDIR:-/tmp}/plant-gate.log" | sed 's/^/    /' >&2
            fail=1
            ;;
        *) echo "  ok      $desc" ;;
    esac
}

plant 'blob/main (the original case)' \
    'https://github.com/codyspate/khoralang/blob/main/docs/errata.md' caught
plant 'tree/main' \
    'https://github.com/codyspate/khoralang/tree/main/docs/design' caught
plant 'raw/main' \
    'https://github.com/codyspate/khoralang/raw/main/docs/errata.md' caught
plant 'edit/main' \
    'https://github.com/codyspate/khoralang/edit/main/docs/errata.md' caught
plant 'commits/main' \
    'https://github.com/codyspate/khoralang/commits/main/docs/errata.md' caught
plant 'blame/main' \
    'https://github.com/codyspate/khoralang/blame/main/docs/errata.md' caught
plant 'blob/HEAD' \
    'https://github.com/codyspate/khoralang/blob/HEAD/docs/errata.md' caught
plant 'blob/refs/heads/main' \
    'https://github.com/codyspate/khoralang/blob/refs/heads/main/docs/errata.md' caught
plant 'raw.githubusercontent.com/.../main/, a non-installer path' \
    'https://raw.githubusercontent.com/codyspate/khoralang/main/std/core.kh' caught
plant 'a correctly pinned link (control)' \
    'https://github.com/codyspate/khoralang/blob/v0.1.0/docs/errata.md' clean
plant 'CHANGELOG.md on main (allow-listed, control)' \
    'https://github.com/codyspate/khoralang/blob/main/CHANGELOG.md' clean
plant 'the installer on raw.githubusercontent.com (allow-listed, control)' \
    'https://raw.githubusercontent.com/codyspate/khoralang/main/install.sh' clean

# One case in an `.mdx` file, since `sync-docs.mjs` publishes those as pages
# too and an earlier version of the gate only looked at `*.md`.
printf '%s\n' 'https://github.com/codyspate/khoralang/blob/main/docs/errata.md' > "$mdx_target"
rc=0
gate || rc=$?
rm -f "$mdx_target"
if [ "$rc" -eq 0 ]; then
    echo "  FAILED  a blob/main link in an .mdx file: expected caught, gate exited 0" >&2
    fail=1
else
    echo "  ok      a blob/main link in an .mdx file"
fi

gate && echo "  ok      clean after every plant is removed" || {
    echo "  FAILED  the gate does not pass once every plant is removed" >&2
    fail=1
}

exit "$fail"
