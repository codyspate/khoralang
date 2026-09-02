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
#
# **It also leaves a receipt**, `.khora-gate-full`, naming the tree it passed
# for. `scripts/gate.sh` reads it, and the pre-push hook
# `scripts/install-hooks.sh` writes calls that. An exit status can be dropped
# by the shell chain around this script — that has put three commits on a red
# baseline, most recently a `| grep` taking grep's status — and a file on disk
# cannot be. Roadmap 13.20.
#
# `scripts/check.sh native` leaves `.khora-gate-fast` the same way, and passing
# this one satisfies that one. Roadmap 14.32.
set -e

root=$(cd "$(dirname "$0")/.." && pwd)
cd "$root"

# Removed first, so that a run which dies halfway leaves no receipt at all
# rather than a stale one. Every early exit below is a failure, and every one
# of them now says so twice: a non-zero status, and nothing on disk.
receipt="$root/.khora-gate-full"
# The fast receipt goes too: this run is about to answer for both, and a stale
# `fast` sitting beside a failed `full` would be a receipt for a tree whose
# baseline is not known to pass.
rm -f "$receipt" "$root/.khora-gate-fast"
khora="./target/debug/khora.exe"
[ -x "$khora" ] || khora="./target/debug/khora"

# `khora build` puts a package's program in the package's own `build/`, named
# after the package and given the host's executable extension -- so `core_demo`
# is `build/core_demo.exe` on Windows and `build/core_demo` everywhere else.
built() {
    [ -x "$1.exe" ] && printf '%s\n' "$1.exe" || printf '%s\n' "$1"
}

step() {
    printf '\n=== %s\n' "$1"
}

step 'the native suite'
# `cargo nextest` when it is installed, and `cargo test` when it is not.
#
# **Measured, not assumed: 116s against 271s** for the same 1527 tests on the
# same machine. `cargo test` runs one test *binary* at a time and parallelises
# only within it, which is why 271 seconds of wall clock sat on 262 seconds of
# in-binary time; nextest gives each test its own process and runs many at
# once. `.config/nextest.toml` is where the tests that cannot take that are
# named.
#
# A fallback rather than a requirement, because a contributor who has just
# cloned this should be able to run the gate without installing anything.
if command -v cargo-nextest > /dev/null 2>&1; then
    cargo nextest run --workspace --features llvm --no-fail-fast
    # **And the doctests, which nextest does not run and will not.** It drives
    # libtest binaries; rustdoc compiles each example into a program of its own
    # and has no such binary to drive. There are four, in `khora-db`,
    # `khora-manifest` and `khora-syntax`, and none of them need LLVM — eleven
    # seconds to not quietly lose the examples in the documentation.
    cargo test --workspace --doc
else
    printf '  cargo-nextest is not installed; falling back to cargo test.\n'
    printf '  It is about 2.3x slower: cargo install cargo-nextest\n'
    cargo test --workspace --features llvm
fi

step 'clippy, all targets'
cargo clippy --workspace --features llvm --all-targets -- -D warnings

step 'and the build with no backend'
# **The configuration nothing was checking.** Every step above passes
# `--features llvm`, so the `#[cfg(not(feature = "llvm"))]` stubs are compiled
# by nothing here -- and one of them drifted out of step with the function it
# stands in for, which meant `cargo build -p khora-cli` did not compile for as
# long as it took somebody to try it. That is the build `CONTRIBUTING.md`
# recommends for work on the front end, because it is much faster.
#
# `check` rather than `clippy`: the lints have already run over the same code
# with the feature on, and what is being asked here is whether it compiles at
# all.
cargo check --workspace --all-targets

step 'no text a quoting slip mangled'
# Cheap, and it catches a class nothing else does: a doc comment that is a
# valid program and not English. `scripts/no-mangled-text.sh` names the three
# that reached `main` before it existed.
sh "$root/scripts/no-mangled-text.sh"

step 'every unsafe block has an argument'
sh "$root/scripts/no-bare-unsafe.sh"

step 'no doc comment on the wrong item'
# The sibling of the check above, and the same kind of defect: an edit produces
# something valid that says the wrong thing. Twenty-seven doc comments in this
# tree had been separated from the item they were written for, eleven of them
# describing something else entirely, and every test passed the whole time --
# `std_surface` counts doc comments and the count never moved.
sh "$root/scripts/no-stranded-docs.sh"

step 'the corpus checks'
# `std` on its own, because it is not a workspace member: it is found beside
# the compiler rather than resolved through a manifest, and it has no
# `khora.toml` at all.
"$khora" check std
# And everything else in one command. This used to be a `for` loop over
# `examples/*/` with a comment explaining that each example is its own package
# and a walk over the directory would resolve one manifest for four programs.
# That comment was a workaround for a missing feature; `[workspace]` in the
# root `khora.toml` is the feature. Roadmap 14.13.
"$khora" check .

step 'the corpus is formatted'
"$khora" fmt std --check
"$khora" fmt . --check

step 'the standard library reference matches the standard library'
# `--check` writes nothing and fails if the checked-in pages are stale, which
# is the only way generated documentation stays true: a page regenerated by
# hand, sometimes, is a page that is wrong at the moment somebody reads it.
"$khora" doc std --out website/content/docs/stdlib/api --check

step 'the hand-written examples compile'
# **The generated pages were the only ones anything checked.** `khora doc std
# --check` keeps the 993 examples under `stdlib/api` honest, because they come
# from `///` comments this gate already compiles. The 580 in the Guide, the
# Reference and the Cookbook — the pages somebody reads first — had never been
# compiled at all, and 55 of them did not.
#
# About three minutes, nearly all of it `khora parse` starting up. It is slower
# than every other step here and is worth it: a documentation example that no
# longer compiles is the failure a reader meets before any other.
bash scripts/check-docs.sh

step 'the documentation site assembles and its links resolve'
# **The gate did not build the site, and the site was broken for a week.**
# Three links to `CONTRIBUTING.md` and friends on GitHub were rejected by the
# link checker -- correctly shaped, wrongly classified -- and `npm run build`
# failed. Nothing here noticed, because this script covered every other tree in
# the repository and not that one, and the CI workflow that would have caught it
# only runs on a push.
#
# `sync-docs.mjs` rather than `npm run build`: it is plain Node with no
# dependencies, so it needs no `npm install` and adds about a second. It is also
# the part that broke -- it copies the content tree and refuses a link that does
# not resolve. The Astro build proper stays in CI, where the dependencies live.
if command -v node > /dev/null 2>&1; then
    ( cd website && node scripts/sync-docs.mjs )
else
    # Loudly, not silently. A gate step that quietly skips is a gate with a
    # hole, and this one is cheap enough that the only reason to skip it is a
    # machine with no Node on it at all.
    echo "!! node is not installed, so the documentation site was not checked" >&2
    exit 1
fi

step 'the packages pass their own tests'
# A package whose tests nobody runs is a package with no tests. `khora test`
# compiles the `test` blocks into their own executable and runs it — the same
# path a user of the language would take.
"$khora" test packages/postgres

step 'every reference application builds'
# `ledger_service` is here for the same reason the packages are: it depends on
# `packages/postgres`, so building it is what catches a package change that
# breaks its only real consumer. It is not *run* here -- that needs a database,
# which `crates/khora-codegen-llvm/tests/postgres.rs` gates on KHORA_POSTGRES.
#
# `--no-cache`, because this step's claim is that the compiler builds them and
# not that it built them once. 14.17's key includes the compiler binary, so a
# hit is only possible when the compiler and the sources are both unchanged and
# a cached build would not actually be hiding anything -- but a gate that can
# be satisfied by a lookup is a gate with a moving part, and this one is the
# receipt everything else is measured against.
for app in examples/core_demo examples/risk_analyzer examples/link_shortener \
           examples/ledger_service examples/khq; do
    "$khora" build "$app" --no-cache
done

# **And `khq`'s own tests**, which are the only reference application that has
# any. It is a query language, so what it means is a table of query, document
# and answer -- and half of those are refusals, because a query language
# producing nothing and looking like it worked is its whole failure mode.
"$khora" test examples/khq

step 'the build cache answers, and answers with the right bytes'
# The claim 14.17 rests on, checked against the real corpus rather than a
# fixture: a release build reused from the cache is byte-identical to one made
# with `--no-cache`. That only holds because 13.10 made release reproducible,
# so this is also a standing check that it still is.
cached=$(mktemp -d)
"$khora" build examples/core_demo --release --no-cache -o "$cached/fresh" > /dev/null
"$khora" build examples/core_demo --release -o "$cached/reused" > /dev/null
if cmp -s "$cached/fresh" "$cached/reused"; then
    printf '  ok    a release hit is the artifact the build would have produced\n'
else
    printf '  FAILED  a cached release build differs from a fresh one\n' >&2
    rm -rf "$cached"
    exit 1
fi
rm -rf "$cached"

step 'the reference applications that end on their own, run'
"$(built ./examples/core_demo/build/core_demo)" > /dev/null

step 'an ordinary client gets ordinary answers'
sh "$root/scripts/http_conformance.sh"

# The runtime's reactor is `WSAPoll` here and `poll` everywhere else, and only
# one of those runs on this machine. WSL2 is a real kernel with real sockets, so
# it answers the question for Linux at no cost. Skipped rather than failed when
# there is no WSL: this is a Windows developer's extra check, not a requirement.
#
# **`wsl -l -q`, not `command -v wsl`.** A GitHub `windows-latest` runner has
# `wsl.exe` on PATH and no distribution behind it, so the command exists and
# every use of it fails -- which turned "an extra check a laptop can do" into a
# CI failure on the one platform that cannot fix it. Asking for the list asks
# the question that matters, which is whether there is a Linux here.
if wsl -l -q >/dev/null 2>&1; then
    step 'the runtime on Linux, through WSL'
    # **Kept, not discarded.** This was `> /dev/null`, and when the Linux check
    # exited 101 the baseline log ended mid-sentence with no error anywhere in
    # it: every `say` header and every summary line that script writes goes to
    # stdout, so discarding stdout discards the part that names the step. It is
    # the same mistake the repeat loop inside `check-linux.sh` had, one level up.
    linux_log="${TMPDIR:-/tmp}/khora-baseline-linux.log"
    if ! sh "$root/scripts/check-linux.sh" > "$linux_log" 2>&1; then
        printf '  FAILED  the Linux check, kept at %s\n' "$linux_log" >&2
        tail -40 "$linux_log" >&2
        exit 1
    fi
    printf '  ok    khora-rt passes against the POSIX `poll`\n'
else
    printf '\n=== skipping the Linux check: no wsl\n'
fi

step 'the receipt can tell two trees apart'
# **It could not, for one whole commit.** `tree-id.sh` hashed every tracked
# file by handing `git ls-files` to `git hash-object`, and a tracked file that
# is no longer on disk makes that fatal and abandon the rest of its batch — so
# a tree with any deletion in it hashed to the same line no matter what else
# changed. The receipt kept answering and had stopped meaning anything. Errata
# 72. This runs the four cases that catch it, the last of which is an edit made
# while another file is deleted.
sh "$root/scripts/check-tree-id.sh"

# The receipt, last, after every step has passed. What it records is which
# *tree* passed — `sh scripts/tree-id.sh` — so editing a file afterwards
# invalidates it without anybody having to remember that it should.
sh "$root/scripts/tree-id.sh" > "$receipt"

printf '\n=== baseline clean\n'
