#!/bin/sh
# The Linux half of `check-linux.sh`, run inside WSL as one process.
#
# **One invocation, and that is the point.** This was four separate
# `wsl -e bash -lc '...'` calls from the Windows side, and `wsl.exe` can return
# before its child has finished writing — so the "sequential" steps overlapped.
# The evidence was a log with one step's output cut off mid-word, a later
# step's header, and then the rest of the first step: two processes appending
# to one file with independent offsets. Every one of them shared
# `CARGO_TARGET_DIR`, so the next cargo could start against a tree the previous
# one was still linking into, and about one run in four failed with cargo's
# exit 101 and nothing in the log to say why. Reproduced on the fourth attempt
# of eight after being unexplained for two sessions.
#
# It is also a plain file rather than a quoted string, which removes a whole
# class of bug the string form kept producing: a backtick in a comment inside
# `bash -lc "..."` is command substitution, and one of them ran
# `docs/design/scheduler.md` as a shell script.
#
# Usage:  sh scripts/linux-inner.sh <target-dir> <repeats>
set -eu

TARGET=${1:?a CARGO_TARGET_DIR}
REPEATS=${2:?how many times to run the suite}

say() { printf '\n=== %s\n' "$*"; }

. "$HOME/.cargo/env"
export CARGO_TARGET_DIR="$TARGET"

# **`set -e` matters here and is not decoration.** Without it a failing
# `cargo test` was followed by a passing `cargo clippy`, the shell returned
# clippy's status, and the whole check reported success over a segfault. It
# did, for one commit.
say 'khora-rt on Linux'
cargo test -p khora-rt
cargo clippy -p khora-rt --all-targets -- -D warnings

# **Again, several times, because the scheduler's failures are races.** One
# green run of a flaky suite is not evidence, and a `poll` that behaves
# differently under load is exactly what this exists to catch. The binary is
# copied out first: cargo rebuilds under a different hash often enough that a
# loop over `cargo test` measures the wrong thing.
say 'the runtime, repeatedly, because a race needs more than one look'

# **Not `2>/dev/null`, which is how this failed silently twice.** The whole
# Linux check exited 101 with its log ending at the line above and nothing
# after it: no test output, no error, no clue. That is not the repeat loop
# failing -- a failing run prints which one and keeps its log -- it is *this*
# build step failing with its stderr thrown away, after which `set -e` kills
# the script without a word. A step that can end the run has to be able to say
# why it did.
if ! cargo test -q -p khora-rt --lib --no-run; then
    echo "the runtime's tests would not build; the error is above" >&2
    exit 1
fi

# And the same again for finding the binary. A `ls` that matches nothing is a
# `set -e` exit with no message, which looks identical to the case above.
bin=$(ls -t "$TARGET"/debug/deps/khora_rt-* 2>/dev/null | grep -v '[.]d$' | head -1)
if [ -z "$bin" ]; then
    echo "no khora_rt test binary under $TARGET/debug/deps after building one" >&2
    exit 1
fi
cp "$bin" /tmp/khora-rt-under-test
chmod +x /tmp/khora-rt-under-test

# **Keep what failed.** This loop once said "1 of 15 runs crashed or failed"
# and sent the output to /dev/null, and the run was not reproducible
# afterwards. A flaky-failure reporter that discards its evidence turns a race
# into a rumour, which is the one thing the scheduler's bug list says not to
# let happen. Core dumps are on for the same reason: instrumenting these has
# hidden them before, and a dump is the observation that does not perturb.
ulimit -c unlimited 2>/dev/null || true
failures=0
kept=/tmp/khora-rt-failure.log
rm -f "$kept"
run=1
while [ "$run" -le "$REPEATS" ]; do
    # **`|| status=$?`, and this is the whole of why #108 was never diagnosed.**
    # `set -e` is on. A bare `cmd > log` that exits non-zero ends the shell
    # *there*, so the `status=$?` below never ran and neither did any of the
    # reporting under it -- the run that crashed took the script down with it
    # and the log ended at the `say` line above with nothing after it. Every
    # careful thing this loop does with its evidence was unreachable from the
    # only path that produces any. Consuming the failure with `||` is what
    # exempts it, and the status inside is the one that matters.
    #
    # It is the third time this file has lost a message to `set -e`, and the
    # other two have comments above them saying so. This one was hiding the
    # race the loop exists to catch.
    status=0
    /tmp/khora-rt-under-test > /tmp/khora-rt-run.log 2>&1 || status=$?
    if [ "$status" -ne 0 ]; then
        failures=$((failures + 1))
        if [ ! -f "$kept" ]; then
            { echo "run $run of $REPEATS, exit $status"
              cat /tmp/khora-rt-run.log; } > "$kept"
        fi
    fi
    run=$((run + 1))
done

if [ "$failures" -ne 0 ]; then
    echo "  FAILED  $failures of $REPEATS runs crashed or failed" >&2
    echo "  --- the first one, kept at $kept ---" >&2
    tail -40 "$kept" >&2
    exit 1
fi
printf '  ok    %s runs, all clean\n' "$REPEATS"
