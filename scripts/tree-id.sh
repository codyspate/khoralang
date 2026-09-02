#!/bin/sh
# One line naming the content of the working tree.
#
# Two runs over an unchanged tree print the same line; edit, add, remove or
# rename one tracked file and they do not.
#
# **Content, not the commit.** The first version of this was `HEAD` plus a hash
# of the diff, and it had one fatal property: committing changes the answer
# without changing a byte of the program, so the receipt went stale in the act
# of committing and a pre-push hook would refuse every push. A hook that always
# refuses is a hook that gets uninstalled. What the baseline passed for is the
# content, and that is what this names.
#
# **Tracked files only**, which is a deliberate limit rather than an oversight.
# The baseline itself writes untracked files — built executables, object files,
# a link store — so including them would invalidate the receipt in the act of
# writing it. A new source file nobody has `git add`ed is therefore not seen;
# it is also not in the commit that would be pushed.
#
# **A deleted file used to make this print a constant.** It was
# `git ls-files -z | xargs -0 git hash-object`, and `ls-files` lists a tracked
# file that is no longer on disk. `git hash-object` fatals on that path and
# **abandons the rest of its batch**, so the stream lost the hashes of hundreds
# of files it never reached — and because the failure is inside a pipeline, the
# exit status belonged to the last stage and `set -e` saw nothing. Deleting
# fifteen pages and editing a sixteenth left the receipt byte-identical, which
# is the worst way for a check to be wrong: it kept answering.
#
# What replaces it reads no files at all. The index already holds a hash per
# tracked path, and one diff holds everything the working tree does differently
# — a deletion included, which is what the old shape could not see.
set -e

root=$(cd "$(dirname "$0")/.." && pwd)
cd "$root"

# `git hash-object` rather than sha1sum or shasum: git is already required
# here, and neither of the others is on every machine this runs on.

# Every tracked path with the hash of its staged content. Names as well as
# contents, so a rename or a removal counts as a change.
index=$(git ls-files -s | git hash-object --stdin)

# Everything the working tree does differently from that. `--binary` so a
# changed image or fixture contributes its bytes rather than the words
# "Binary files differ", and the three `--no-` flags so a machine with a
# textconv filter or an external differ configured gets the same answer as one
# without.
worktree=$(git diff --binary --no-color --no-ext-diff --no-textconv | git hash-object --stdin)

printf '%s %s\n' "$index" "$worktree"
