#!/bin/sh
# One line naming the content of the working tree.
#
# A hash over every tracked file's *working-tree* content, plus a hash of the
# list of names. Two runs over an unchanged tree print the same line; edit,
# add, remove or rename one tracked file and they do not.
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
# 466 files, under a fifth of a second. Cheap enough to ask twice.
set -e

root=$(cd "$(dirname "$0")/.." && pwd)
cd "$root"

# `git hash-object` rather than sha1sum or shasum: git is already required
# here, and neither of the others is on every machine this runs on.
#
# The names as well as the contents, so that removing a file or renaming one
# counts as a change — the contents hash alone would not notice a deletion.
names=$(git ls-files -z | git hash-object --stdin)
contents=$(git ls-files -z | xargs -0 git hash-object | git hash-object --stdin)

printf '%s %s\n' "$names" "$contents"
