#!/bin/sh
# The fast loop. `baseline.sh` is the slow one, and both have a job.
#
#     sh scripts/check.sh            front end only, no LLVM        ~20s
#     sh scripts/check.sh native     the above plus code generation ~90s
#
# **What this is for.** `baseline.sh` builds every reference application, runs
# clippy over all targets, formats the corpus and talks to a real `curl`. It is
# the right thing to run before a commit and the wrong thing to run after
# changing one line, because most of what it checks cannot be affected by most
# changes.
#
# What it deliberately does *not* do is decide which tests your change affected.
# That is a judgement, and a script that guesses it wrong is worse than one that
# is honestly partial: run this while working, run `baseline.sh` before you
# commit, and never let this one's silence stand in for that one's.
set -e

root=$(cd "$(dirname "$0")/.." && pwd)
cd "$root"

step() {
    printf '\n=== %s\n' "$1"
}

# `cargo nextest` when it is installed, `cargo test` when it is not. The whole
# workspace takes 116s under nextest against 271s under `cargo test`, because
# `cargo test` runs one test *binary* at a time; the tests that cannot take a
# process of their own are named in `.config/nextest.toml`. A fallback rather
# than a requirement, so that a fresh clone can run this with nothing installed.
#
# The second command is the doctests, which nextest drives libtest binaries to
# run and rustdoc does not produce: without it the examples in the
# documentation stop being compiled at all.
suite() {
    if command -v cargo-nextest > /dev/null 2>&1; then
        cargo nextest run "$@"
        cargo test --workspace --doc
    else
        cargo test "$@"
    fi
}

# The front end needs no LLVM, and this is where most mistakes are: the lexer,
# the parser, name resolution, inference, rows, traits, exhaustiveness,
# monomorphization, the formatter and the reference-counting plan. Twenty
# seconds, and it catches anything that is not about emitting machine code.
step 'front end'
suite --workspace

# `khora-types`'s own portability test lives in that run and checks `std` for
# every target from this host, so a mistake in a platform file is caught here
# rather than by whoever next builds on that platform.

if [ "${1:-}" != native ]; then
    printf '\n%s\n' "front end clean. \`sh scripts/check.sh native\` also builds and runs \
compiled programs; \`sh scripts/baseline.sh\` is the full one."
    exit 0
fi

# Everything above plus the backend. Slower because each test binary links the
# whole compiler and LLVM, and because the tests that matter most here start a
# process and talk to it over a socket.
step 'code generation, and programs that run'
suite --workspace --features llvm

printf '\n%s\n' "native clean. \`sh scripts/baseline.sh\` adds clippy, the corpus check, \
the reference applications and the HTTP conformance suite."
