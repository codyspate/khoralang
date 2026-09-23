#!/bin/sh
# The published documentation is for somebody using Khora, not for somebody
# maintaining it.
#
# **What this catches, and why it is worth catching.** A doc comment is written
# by the person who just changed the code, so it comes out in their voice: what
# was wrong before, which attempt fixed it, which errata records the argument,
# which file under `docs/design/` has the rest. All of that is true and none of
# it belongs on a page somebody reads to find out what a function does. It also
# ages badly -- "this used to trap" is a claim about a version nobody can run.
#
# The markers below are the unambiguous ones. Deliberately absent: "used to",
# which is ordinary English ("a schema, used to decode input") and is also how
# a legitimate migration note reads ("`Clock` used to live here"). Those need a
# person, so this does not try.
#
# A private item's `///` is not published and is not checked -- only what
# `khora doc` writes and what is hand-written under `website/content/docs`.
set -e

root=$(cd "$(dirname "$0")/.." && pwd)
cd "$root"

pages=${1:-website/content/docs}
fail=0

report() {
    if [ -n "$2" ]; then
        printf '  FAILED  %s\n' "$1"
        printf '%s\n' "$2" | sed 's/^/          /'
        fail=1
    fi
}

# A path into the repository, in backticks. A reader on the website cannot open
# it. Either the sentence needs the substance inlined, or the path needs to
# become a link to GitHub.
#
# **Unbackticked too.** The backtick was assumed and it is not required:
# `docs/roadmap.md` in plain prose 404s exactly as hard, and it reached a
# published page twice because this only looked for the quoted form.
paths=$(grep -rnE '`?(docs/design/|docs/errata|docs/roadmap|crates/)' "$pages" --include=*.md | grep -v '](http' || true)
report 'a repository path a website reader cannot open' "$paths"

# The errata and the roadmap are internal indexes. A number from either means
# nothing to a reader, and dates the page besides.
indexes=$(grep -rniE '\berrata [0-9]+|roadmap #[0-9]+' "$pages" --include=*.md || true)
report 'a reference to the errata or the roadmap by number' "$indexes"

# Notes about the writing rather than about the subject.
selfref=$(grep -rniE 'took a second attempt|the note that stood here|used to be written here|this said that it was|nobody noticed' "$pages" --include=*.md || true)
report 'a note about a previous version of the note' "$selfref"

# **The published pages state what the language does, not what changed.** A
# reader arrives to learn how Khora behaves; which release altered it is the
# changelog's business, and a page written in that voice goes stale the moment
# the next thing lands. This is the shape it takes in practice: a sentence that
# describes the repair rather than the behaviour.
#
# `is now`, `no longer` and their neighbours are the tell. They read as facts
# but they are dated claims -- each one silently means "as against a version the
# reader never used". The phrasings here were all written into published pages
# by somebody who had just made the change, which is exactly whose voice this
# script exists to catch.
#
# **`already` is deliberately not on the list.** "A failed handshake has already
# closed the socket" is a statement about the order two things happen in, which
# is exactly what an API page should say. The first version of this check flagged
# it, and a gate that cries wolf on correct prose gets switched off.
history=$(grep -rniE 'is now correct|now exits|now returns|used to (exit|return|be)|no longer (exits|returns|hangs)|this (is|was) now (fixed|closed)|partly closed' "$pages" --include=*.md || true)
report 'a page written as a change rather than as a behaviour' "$history"

# A link inside a link. Turning a bare path into a link twice produces
# `[the note](https://.../[the note](https://.../x.md))`, which renders as
# visible punctuation around a dead anchor -- and reads, from the page, as
# though nobody looked. The pass that fixed the paths above did exactly that
# to one of them, and only a reader of the live site would have caught it.
nested=$(grep -rn '](http[^)]*\[' "$pages" --include=*.md || true)
report 'a Markdown link nested inside another link' "$nested"

if [ "$fail" -eq 0 ]; then
    printf '  ok    the documentation is addressed to a reader\n'
fi
exit "$fail"
