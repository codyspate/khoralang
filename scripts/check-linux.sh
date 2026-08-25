#!/bin/sh
# The runtime's tests on Linux, from a Windows machine, for nothing.
#
# **Why this exists.** `khora-rt`'s reactor is the one part of the tree whose
# behaviour differs per platform in a way no amount of care can check locally:
# it is `WSAPoll` on Windows and `poll` on Linux and macOS, and only the first
# runs here. `.github/workflows/runtime.yml` is the answer that costs Actions
# minutes; this is the answer that costs none, and it is faster.
#
# WSL2 is a real Linux kernel with real sockets and real `poll`, so the socket
# tests are testing the thing they claim to test rather than an emulation.
#
# What it does not cover is macOS, which has no equivalent trick. `kqueue` is
# reached through the same `poll` call and the same struct, so the risk is
# smaller than it was — but it is not zero, and the workflow is still the only
# thing that checks it.
#
# Usage:  sh scripts/check-linux.sh
set -eu

say() { printf '\n=== %s\n' "$*"; }

if ! command -v wsl >/dev/null 2>&1; then
    echo "check-linux: no wsl on this machine; use .github/workflows/runtime.yml" >&2
    exit 1
fi

# A target directory inside the WSL filesystem rather than on `/mnt/c`. Cargo
# on a 9p mount is slow enough to be worth the one line, and mixing Windows and
# Linux artefacts in one `target/` would have each rebuild the other's.
TARGET=/tmp/khora-linux-target

# How many times to run the suite. The scheduler's bugs are races, and one of
# them showed up in about a third of runs — invisible to a single pass.
REPEATS=${KHORA_LINUX_REPEATS:-15}

# This repository, as WSL names it.
#
# `$PWD` under Git Bash is `/c/Users/...`, which `wslpath` reads as a relative
# path and turns into `/mnt/c/c/Users/...`. `pwd -W` gives the Windows spelling
# that `wslpath` actually wants.
here=$(pwd -W 2>/dev/null || pwd)
inside=$(wsl -e wslpath -a "$here" | tr -d '\r')

say 'the toolchain, if it is not there yet'
wsl -e bash -lc '
    if [ ! -x "$HOME/.cargo/bin/cargo" ]; then
        curl --proto "=https" --tlsv1.2 -sSf https://sh.rustup.rs \
            | sh -s -- -y --profile minimal --default-toolchain stable >/dev/null
    fi
    . "$HOME/.cargo/env"
    # `minimal` leaves clippy out, and the workflow this stands in for runs it.
    rustup component add clippy >/dev/null 2>&1 || true
    cargo --version
'

# `cc` is needed by `ring`, which `rustls` needs, which the runtime needs.
if ! wsl -e bash -lc 'command -v cc >/dev/null 2>&1'; then
    say 'a C compiler, which ring needs'
    wsl -u root -e bash -lc '
        apt-get update -qq >/dev/null 2>&1
        DEBIAN_FRONTEND=noninteractive apt-get install -y -qq gcc libc6-dev >/dev/null 2>&1
        cc --version | head -1
    '
fi

# **`set -e` inside the inner shell, and it is not decoration.** Without it a
# failing `cargo test` was followed by a passing `cargo clippy`, the inner shell
# returned clippy's status, and this script reported success over a segfault.
# It did, for one commit.
say 'khora-rt on Linux'
wsl -e bash -lc "set -eu
    . \"\$HOME/.cargo/env\"
    cd '$inside'
    CARGO_TARGET_DIR=$TARGET cargo test -p khora-rt
    CARGO_TARGET_DIR=$TARGET cargo clippy -p khora-rt --all-targets -- -D warnings
"

# **Again, several times, because the scheduler's failures are races.** One
# green run of a flaky suite is not evidence, and a `poll` that behaves
# differently under load is exactly the kind of thing this script exists to
# catch. The binary is copied out first: cargo rebuilds under a different hash
# often enough that a loop over `cargo test` measures the wrong thing.
say 'the runtime, repeatedly, because a race needs more than one look'
wsl -e bash -lc "set -eu
    . \"\$HOME/.cargo/env\"
    cd '$inside'
    CARGO_TARGET_DIR=$TARGET cargo test -q -p khora-rt --lib --no-run 2>/dev/null
    bin=\$(ls -t $TARGET/debug/deps/khora_rt-* | grep -v '[.]d\$' | head -1)
    cp \"\$bin\" /tmp/khora-rt-under-test
    chmod +x /tmp/khora-rt-under-test
    # **Keep what failed.** This loop said '1 of 15 runs crashed or failed' and
    # threw the output away, and the run was not reproducible afterwards — 160
    # clean passes of the same copied binary. A flaky-failure reporter that
    # discards the evidence turns a race into a rumour, which is the one thing
    # the scheduler's bug list in docs/design/scheduler.md says not to let
    # happen. Core dumps are on for the same reason: instrumenting these has
    # hidden them before, and a dump is the observation that does not perturb.
    #
    # No backticks in this comment. It is inside a double-quoted string handed
    # to bash -lc, so a backtick is command substitution and the shell ran the
    # design document.
    ulimit -c unlimited 2>/dev/null || true
    failures=0
    kept=/tmp/khora-rt-failure.log
    rm -f \$kept
    for run in \$(seq 1 $REPEATS); do
        /tmp/khora-rt-under-test > /tmp/khora-rt-run.log 2>&1
        # Read immediately. Taken any later it is the status of the test that
        # asks whether a failure has already been kept, which is always 0 or 1
        # and never the thing that crashed.
        status=\$?
        if [ \$status -ne 0 ]; then
            failures=\$((failures + 1))
            if [ ! -f \$kept ]; then
                { echo \"run \$run of $REPEATS, exit \$status\"
                  cat /tmp/khora-rt-run.log; } > \$kept
            fi
        fi
    done
    if [ \"\$failures\" -ne 0 ]; then
        echo \"  FAILED  \$failures of $REPEATS runs crashed or failed\" >&2
        echo \"  --- the first one, kept at \$kept ---\" >&2
        # The tail rather than the whole thing: a passing suite's output is
        # thousands of lines and the failure is at the end of it.
        tail -40 \$kept >&2
        exit 1
    fi
    echo \"  ok    $REPEATS runs, all clean\"
"

say 'linux clean'
