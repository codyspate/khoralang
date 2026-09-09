#!/bin/sh
# Every runtime function the code generator can emit a call to exists in the
# archive that ships.
#
# **The archive is not built by the command that builds the toolchain.**
# `cargo build -p khora-cli` compiles `khora-rt` as an *rlib* dependency;
# `libkhora_rt.a` is a separate artifact produced only by
# `cargo build -p khora-rt`. So `target/release/libkhora_rt.a` can be hours
# older than the compiler beside it, and it is the file every generated program
# links against.
#
# That is exactly what happened, three times in one session. A fourteen-hour-old
# archive was missing `khora_enable_counters`, so every program declaring a
# counter failed to link with `undefined reference` from `std.clock`; the same
# stale file was then used to benchmark an allocator change it did not contain,
# producing a comparison between two different runtimes that read as a result.
#
# The first diagnosis was that `lto = true` had deleted the symbol, since
# nothing inside the crate calls it -- plausible, and wrong. Removing the
# keep-alive that was added for it and rebuilding from scratch still exports
# the symbol. There was never anything to fix in the source; there was a build
# step nobody runs.
#
# So this checks the shipped archive against what the code generator can call,
# and it exists because "the tests pass" says nothing about a file the tests do
# not rebuild. Roadmap 16.
set -eu

cd "$(dirname "$0")/.."

archive=target/release/libkhora_rt.a
if [ ! -f "$archive" ]; then
    echo "  building the release runtime archive first" >&2
    cargo build --release -p khora-rt >&2
fi

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

# What the backend declares, which is the set a program can call.
grep -oE 'declare\("khora_[a-z_0-9]+"' crates/khora-codegen-llvm/src/runtime.rs \
    | sed 's/declare("//; s/"//' | sort -u > "$work/wanted"

# What the archive exports. `T` is a defined text symbol; anything else cannot
# satisfy a call from outside.
nm --defined-only "$archive" 2>/dev/null \
    | awk '$2 == "T" { print $3 }' | sort -u > "$work/have"

missing=$(comm -23 "$work/wanted" "$work/have")
if [ -n "$missing" ]; then
    printf '  FAILED  the code generator can call these and the shipped archive does not define them:\n' >&2
    printf '            %s\n' $missing >&2
    printf '\n  `%s` is the archive every generated program links.\n' "$archive" >&2
    printf '  It is built by `cargo build --release -p khora-rt`, which is not what\n' >&2
    printf '  builds the compiler -- so it can be stale, and `lto = true` can drop a\n' >&2
    printf '  symbol whose only caller is generated code.\n' >&2
    exit 1
fi

printf '  ok    all %s runtime symbols the backend declares are in the archive\n' "$(wc -l < "$work/wanted" | tr -d ' ')"
