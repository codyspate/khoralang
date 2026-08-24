#!/bin/sh
# The correctness baseline Phase 9 must preserve.
#
# Reuse analysis and FBIP change when memory is allocated and freed, and
# `docs/design/compatibility.md` decides that no program may depend on that. So
# what this checks is everything else: that programs still compile, still say
# the same things, and still answer ordinary clients the same way.
#
# It does not check throughput. That is `bench/README.md`, run by hand, because
# a number from a machine that is also running a test suite is not a number.
#
#     sh scripts/baseline.sh
#
# Exits non-zero on the first failure, and says which step.
set -e

root=$(cd "$(dirname "$0")/.." && pwd)
cd "$root"
khora="./target/debug/khora.exe"
[ -x "$khora" ] || khora="./target/debug/khora"

# `khora build` names its output with the host's executable extension, so a
# compiled program is `main.exe` on Windows and `main` everywhere else.
built() {
    [ -x "$1.exe" ] && printf '%s\n' "$1.exe" || printf '%s\n' "$1"
}

step() {
    printf '\n=== %s\n' "$1"
}

step 'the native suite'
cargo test --workspace --features llvm

step 'clippy, all targets'
cargo clippy --workspace --features llvm --all-targets -- -D warnings

step 'the corpus checks'
"$khora" check std
"$khora" check examples
"$khora" check bench

step 'the corpus is formatted'
"$khora" fmt std --check
"$khora" fmt examples --check
"$khora" fmt bench --check

step 'every reference application builds'
for app in examples/core_demo examples/risk_analyzer examples/link_shortener; do
    "$khora" build "$app"
done

step 'the reference applications that end on their own, run'
"$(built ./examples/core_demo/src/main)" > /dev/null

step 'an ordinary client gets ordinary answers'
sh "$root/scripts/http_conformance.sh"

# The runtime's reactor is `WSAPoll` here and `poll` everywhere else, and only
# one of those runs on this machine. WSL2 is a real kernel with real sockets, so
# it answers the question for Linux at no cost. Skipped rather than failed when
# there is no WSL: this is a Windows developer's extra check, not a requirement.
if command -v wsl >/dev/null 2>&1; then
    step 'the runtime on Linux, through WSL'
    sh "$root/scripts/check-linux.sh" > /dev/null
    printf '  ok    khora-rt passes against the POSIX `poll`\n'
else
    printf '\n=== skipping the Linux check: no wsl\n'
fi

printf '\n=== baseline clean\n'
