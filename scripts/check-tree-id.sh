#!/bin/sh
# tree-id against known changes. What matters is not that it prints a line but
# that a different tree prints a different one.
cd "$(dirname "$0")/.."

id() { sh scripts/tree-id.sh 2>/dev/null; }

base=$(id)
echo "base: $base"
fail=0

check() {
    if [ "$(id)" = "$base" ]; then
        echo "  NOT SEEN  $1"
        fail=1
    else
        echo "  seen      $1"
    fi
}

same() {
    if [ "$(id)" = "$base" ]; then
        echo "  unchanged $1"
    else
        echo "  DRIFTED   $1"
        fail=1
    fi
}

lints=website/content/docs/reference/lints.md
traps=website/content/docs/reference/traps.md
cp "$lints" "${TMPDIR:-/tmp}/lints.keep"
cp "$traps" "${TMPDIR:-/tmp}/traps.keep"

# 1. an edit to a tracked file
printf '\nprobe\n' >> "$lints"
check "an edit to a tracked file"
cp "${TMPDIR:-/tmp}/lints.keep" "$lints"
same "and restoring it"

# 2. deleting a tracked file
rm "$lints"
check "deleting a tracked file"
cp "${TMPDIR:-/tmp}/lints.keep" "$lints"
same "and putting it back"

# 3. an edit while another tracked file is already deleted. This is the case
#    that broke the old shape, and the state this whole commit is in.
rm "$lints"
after_delete=$(id)
printf '\nprobe\n' >> "$traps"
if [ "$(id)" = "$after_delete" ]; then
    echo "  NOT SEEN  an edit while another file is deleted"
    fail=1
else
    echo "  seen      an edit while another file is deleted"
fi
cp "${TMPDIR:-/tmp}/traps.keep" "$traps"
cp "${TMPDIR:-/tmp}/lints.keep" "$lints"
same "and restoring both"

# 4. a run that changes nothing
same "two runs over one tree"

echo
if [ "$fail" -eq 0 ]; then
    echo "tree-id: every change seen"
else
    echo "tree-id: FAILURES ABOVE"
fi
exit "$fail"
