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

# **Where the archive is, and what it is called, are both variables.** This
# read `target/release/libkhora_rt.a` outright, so it reported every symbol
# missing on Windows -- where the file is `khora_rt.lib` -- and on any machine
# with `CARGO_TARGET_DIR` set, where `target/` is not the target directory.
# A check that answers "all of them are gone" whenever it is run somewhere it
# was not written is worse than one that does not run: it is a failing gate
# nobody believes, and this one exists precisely to be believed.
target="${CARGO_TARGET_DIR:-target}"
case "$(uname -s)" in
    MINGW* | MSYS* | CYGWIN*) archive="$target/release/khora_rt.lib" ;;
    *) archive="$target/release/libkhora_rt.a" ;;
esac
if [ ! -f "$archive" ]; then
    echo "  building the release runtime archive first" >&2
    cargo build --release -p khora-rt >&2
fi
if [ ! -f "$archive" ]; then
    echo "  FAILED  no runtime archive at $archive after building it" >&2
    exit 1
fi

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

# What the backend declares, which is the set a program can call.
grep -oE 'declare\("khora_[a-z_0-9]+"' crates/khora-codegen-llvm/src/runtime.rs \
    | sed 's/declare("//; s/"//' | sort -u > "$work/wanted"

# What the archive exports. `T` is a defined text symbol; anything else cannot
# satisfy a call from outside.
# **`nm` has to be one that reads the archive in front of it.** GNU `nm`
# knows ELF; the Windows archive is a COFF one, and it answered with nothing
# at all rather than with an error -- so every symbol read as missing and the
# gate failed with a list of thirty names that were all present. `llvm-nm`
# reads both, and LLVM is already a build requirement.
reader=nm
if command -v llvm-nm > /dev/null 2>&1; then
    reader=llvm-nm
elif [ -n "${LLVM_SYS_221_PREFIX:-}" ] && [ -x "$LLVM_SYS_221_PREFIX/bin/llvm-nm.exe" ]; then
    reader="$LLVM_SYS_221_PREFIX/bin/llvm-nm.exe"
elif [ -n "${LLVM_SYS_221_PREFIX:-}" ] && [ -x "$LLVM_SYS_221_PREFIX/bin/llvm-nm" ]; then
    reader="$LLVM_SYS_221_PREFIX/bin/llvm-nm"
fi
# **A Mach-O symbol carries a leading underscore and an ELF one does not.**
# `_khora_alloc` never matches `khora_alloc`, so on macOS this compared a list
# of bare names against a list of underscored ones, found nothing in common,
# and reported every symbol as missing -- including ones that had been in the
# archive since the first commit. The same shape as the `nm` bug above: the
# reader was right and the comparison was reading a different dialect.
#
# Stripping one leading underscore is enough and is not ambiguous. Nothing the
# backend declares starts with one; the names are all `khora_*`, so an
# underscore at the front is the platform's and never the symbol's.
"$reader" --defined-only "$archive" 2>/dev/null \
    | awk '$2 == "T" { sub(/^_/, "", $3); print $3 }' | sort -u > "$work/have"
if [ ! -s "$work/have" ]; then
    printf '  FAILED  %s read no defined symbols from %s.'"\n" "$reader" "$archive" >&2
    printf '          An empty read is the wrong reader for the format, not an'"\n" >&2
    printf '          empty archive -- it would report every symbol missing.'"\n" >&2
    exit 1
fi

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
