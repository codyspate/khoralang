#!/bin/sh
# Reports corrections that landed in `next` and never reached a released tree.
#
# **Why this is a gate and not a habit.** `/docs/` serves the newest stable
# tree, so that is what a reader gets and what an agent evaluating the language
# reads. A correction written in `website/content/docs/` -- which is `next`,
# the tree matching the compiler in this checkout -- is invisible to every one
# of them until it is copied across.
#
# That has happened twice. A trial reported that the documentation promised a
# SQLite driver that does not exist; the correction went into `next`; the next
# trial read the published tree and reported the same thing again, having lost
# twenty minutes to it a second time.
#
# **Not a diff of the trees.** They are supposed to differ: `next` describes a
# compiler nobody has yet, and a released tree must keep describing the one it
# was cut from. `versions.mjs` says so in as many words, and a gate demanding
# they match would be demanding the versioning be undone.
#
# What this looks for is narrower: a marked phrase, listed below by hand,
# that names a limitation or a correction true of the released compiler too.
# Adding a line here is the deliberate act of saying "this one is not about a
# new feature; readers of the released documentation are being misled without
# it."
#
# **Only the tree `/docs/` serves.** An older tree is reached by somebody who
# asked for it by name, having chosen to read the documentation for a compiler
# they are still on; correcting it is welcome and not urgent, because nobody
# arrives there by default. The tree the short paths redirect into is the one
# every reader and every agent gets without choosing, and that is the one a
# stale correction actually costs.
set -eu

# One phrase per line: a distinctive sentence from a correction that applies to
# every tree, not only `next`. Keep the reason with it.
corrections=$(cat <<'PHRASES'
No database driver is published|std::db names packages that do not exist
A build cannot link against a native library|extern fn cannot reach a system library in any release
PHRASES
)

# The newest stable tree, which is what `/docs/` redirects into. Read from
# `versions.mjs` so this cannot disagree with the site about which one that is.
current=$(node -e '
  import("./website/versions.mjs").then(m => {
    const stable = m.versions.filter(v => v.stable);
    if (!stable.length) { process.exit(0); }
    process.stdout.write(stable[0].from.replace(/^content\//, "website/content/"));
  });
' 2>/dev/null || true)

[ -n "$current" ] || { printf '  no stable tree yet, so nothing is served but `next`\n'; exit 0; }
[ -d "$current" ] || { printf '  FAILED  %s is named in versions.mjs and is not there\n' "$current" >&2; exit 1; }

missing=0

printf '%s\n' "$corrections" | while IFS='|' read -r phrase why; do
    [ -n "$phrase" ] || continue
    if ! grep -rqF "$phrase" website/content/docs/ 2>/dev/null; then
        printf '  FAILED  `next` no longer says "%s"\n' "$phrase" >&2
        printf '          Either it was reworded -- update this list -- or it was lost.\n' >&2
        exit 1
    fi
    if ! grep -rqF "$phrase" "$current" 2>/dev/null; then
        printf '  FAILED  %s is missing a correction: %s\n' "$(basename "$current")" "$why" >&2
        printf '          Add it there too. `/docs/` redirects into that tree, so a\n' >&2
        printf '          reader following the published documentation never sees what\n' >&2
        printf '          only `next` says.\n' >&2
        missing=$((missing + 1))
    fi
    if [ "$missing" -gt 0 ]; then
        exit 1
    fi
done || exit 1

printf '  ok    every listed correction reaches %s\n' "$(basename "$current")"
