#!/bin/sh
# The release notes for one version, out of `CHANGELOG.md`.
#
#     sh scripts/release-notes.sh 0.1.0
#
# **The changelog is the notes.** This repository keeps one carefully, grouped
# by what a reader needs to know first, and a second hand-written summary
# beside it is a second thing to keep in step -- which in practice means one of
# them goes stale and nobody knows which. So the notes are cut from the file
# that is already reviewed with every change.
#
# A version that is not in the changelog is an error rather than an empty file.
# A release published with empty notes looks like a release nobody wrote notes
# for, and the fix is to write the entry, not to ship the blank.
set -eu

version=${1:-}
if [ -z "$version" ]; then
    echo "usage: $0 <version>   # as in 0.1.0, without a leading v" >&2
    exit 2
fi

root=$(cd "$(dirname "$0")/.." && pwd)
changelog="$root/CHANGELOG.md"

# `## 0.1.0 — 2026-08-27` or `## Unreleased`. The heading is matched exactly to
# its version so that `0.1.0` does not also match `0.1.0-rc.3`.
# A candidate is the same release's notes unless it has its own section. A
# separate RC section is optional: repeating the entire release entry for each
# candidate makes the public notes drift. Keep an exact match first, so a
# candidate-specific correction still takes precedence.
base=${version%-rc.*}
notes_for() {
    awk -v want="$1" '
        /^## / {
            if (inside) exit
            line = substr($0, 4)
            split(line, parts, " ")
            inside = (parts[1] == want)
            if (inside) next
        }
        inside { print }
    ' "$changelog"
}
notes=$(notes_for "$version")
if [ -z "$notes" ] && [ "$base" != "$version" ]; then
    notes=$(notes_for "$base")
fi

# Trim the blank lines the section boundary leaves at each end.
notes=$(printf '%s\n' "$notes" | sed -e '/./,$!d' | sed -e :a -e '/^\n*$/{$d;N;ba' -e '}')

if [ -z "$notes" ]; then
    echo "no section for $version in CHANGELOG.md." >&2
    echo "Add one before releasing: the notes are cut from it." >&2
    exit 1
fi

printf '%s\n' "$notes"
