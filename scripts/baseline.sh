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
./examples/core_demo/src/main.exe > /dev/null

step 'an ordinary client gets ordinary answers'
sh "$root/scripts/http_conformance.sh"

printf '\n=== baseline clean\n'
