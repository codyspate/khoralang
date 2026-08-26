#!/bin/sh
# The runtime under ThreadSanitizer, from a Windows machine, in WSL2.
#
# Roadmap 13.6's second half. The audit read every `unsafe` site and found
# three defects; this is the half a reader cannot do, because a data race is a
# fact about two threads at a moment and not about a line of code.
#
# # What it can and cannot see
#
# **It cannot see through a stack switch.** TSan tracks shadow state per
# thread, and `corosensei` moves a whole stack from one worker to another
# without telling it. Annotating the switch (`__tsan_switch_to_fiber`) is the
# supported answer and is not something this crate can reach from safe Rust
# today.
#
# This is not a theoretical limit that might be fine in practice. Pointing it
# at `blocking::`, whose tests run their work on a `Scheduler`, produces:
#
#     ThreadSanitizer: SEGV on unknown address 0x7ffff6a00000
#     ThreadSanitizer: nested bug in the same thread, aborting.
#
# — the sanitizer reading a fiber's guard page and dying, before any test
# result. So the modules that run a fiber are excluded here, not because they
# are trusted but because the tool cannot answer for them.
#
# What is left is every module that races between **ordinary threads**, which
# is where TSan is at its best:
#
#   - `channel` — a bounded queue with senders and receivers on real threads,
#     and the newest concurrency primitive in the tree.
#   - `wait` — the park/wake protocol itself, whose whole content is the race
#     between a suspension and the wake that beats it.
#   - `contain` — thread-locals and the trap flag.
#   - `decimal`, `trap` — single-threaded, and cheap to include.
#
# `docs/design/soundness.md` records what that does and does not cover.
#
# # `-Zbuild-std` is not optional
#
# It looks like a thoroughness setting and is a correctness one. The host and
# the target are the same triple here, so the toolchain's precompiled `std` is
# a candidate for linking — and it was built without the sanitizer, which
# `rustc` refuses as "mixing `-Zsanitizer` will cause an ABI mismatch" on the
# first dependency it reaches. Building `std` from source is what makes every
# crate in the graph agree.
#
# The cost is the whole standard library compiled per run, which is why this is
# a script somebody invokes rather than a step in `scripts/baseline.sh`.
#
# Usage:  sh scripts/tsan.sh
set -eu

say() { printf '\n=== %s\n' "$*"; }

if ! command -v wsl >/dev/null 2>&1; then
    echo "tsan: no wsl on this machine" >&2
    exit 1
fi

TARGET=/tmp/khora-tsan-target

here=$(pwd -W 2>/dev/null || pwd)
inside=$(wsl -e wslpath -a "$here" | tr -d '\r')

say 'a nightly toolchain, which is where the sanitizers live'
wsl -e bash -lc '
    . "$HOME/.cargo/env"
    rustup toolchain install nightly --profile minimal >/dev/null 2>&1 || true
    rustup +nightly component add rust-src >/dev/null 2>&1 || true
    rustc +nightly --version
'

build_std="-Zbuild-std"


# The tests that race between real threads. Named rather than "everything
# minus", so that a new test is included on purpose rather than by accident.
FILTERS="channel:: wait:: contain:: decimal:: trap::"

# Where the run is kept, so the verdict can be read out of it below.
report=${TMPDIR:-/tmp}/khora-tsan-report.txt

say 'the runtime under ThreadSanitizer'
wsl -e bash -lc "
    set -eu
    . \"\$HOME/.cargo/env\"
    cd '$inside'
    export CARGO_TARGET_DIR='$TARGET'
    # Target-scoped rather than plain RUSTFLAGS. The host and the target are
    # the same triple here, so RUSTFLAGS would instrument build scripts and
    # proc macros too, and those link against an uninstrumented host std --
    # which rustc refuses as an ABI mismatch before anything runs. The
    # target-specific variable leaves the build's own tools alone.
    #
    # No backticks anywhere in this block: it reaches bash through a
    # double-quoted argument, so a backtick that survives the outer shell
    # starts a command substitution and swallows the rest -- which is how the
    # TSAN_OPTIONS line below once ran as a command rather than an assignment,
    # and made a clean run look like a failure.
    export CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_RUSTFLAGS='-Zsanitizer=thread'
    export TSAN_OPTIONS='halt_on_error=0 second_deadlock_stack=1'
    failed=0
    for filter in $FILTERS; do
        printf '\n--- %s\n' \"\$filter\"
        # The status is captured and reported per filter rather than folded
        # into one flag. A run where every module printed 'test result: ok'
        # and the script still exited non-zero cost an hour, because nothing
        # said which of the five had objected.
        code=0
        cargo +nightly test $build_std \\
            --target x86_64-unknown-linux-gnu \\
            -p khora-rt --lib -- --test-threads=1 \"\$filter\" \\
            || code=\$?
        if [ \"\$code\" -ne 0 ]; then
            printf -- '--- %s exited %s\n' \"\$filter\" \"\$code\"
            failed=1
        fi
    done
    if [ \"\$failed\" -eq 0 ]; then
        printf 'KHORA_TSAN_ALL_CLEAR\n'
    fi
" | tee "$report"

# **The verdict is a sentinel in the output, not an exit status.** Running this
# through `wsl -e bash -lc` loses the inner status: every module printed
# `test result: ok`, every filter returned zero when run one at a time, and the
# invocation as a whole still came back 1 with nothing to say why. Rather than
# keep guessing at a layer this script does not own, the inner shell says in
# words whether it got to the end without a failure, and that is what is
# checked.
if ! grep -q 'KHORA_TSAN_ALL_CLEAR' "$report"; then
    printf '\ntsan: the suite did not finish clean. A ThreadSanitizer WARNING\n' >&2
    printf 'names the two accesses and the threads that made them; a "--- x\n' >&2
    printf 'exited N" line names a module that failed for another reason.\n' >&2
    exit 1
fi

say 'ThreadSanitizer found nothing'
