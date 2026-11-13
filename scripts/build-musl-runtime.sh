#!/bin/sh
# Builds the Khora runtime for a musl target, so `khora build --target` can
# link a static executable for it.
#
#     sh scripts/build-musl-runtime.sh x86_64-unknown-linux-musl
#     sh scripts/build-musl-runtime.sh aarch64-unknown-linux-musl --release
#
# Writes `<cargo target dir>/<triple>/<profile>/libkhora_rt.a`, which is where
# the compiler looks for it.
#
# **Why this is a script and not `cargo build --target`.** That command fails:
# `khora-rt` compiles C -- its setjmp shim, `mimalloc` and `ring` -- and for a
# musl target the `cc` crate asks for `x86_64-linux-musl-gcc`, which no
# distribution's default install has. Rust's musl target brings musl's
# `libc.a` and start files but no C *headers*. So this supplies the two missing
# pieces and nothing else:
#
#   - musl's headers, from the musl release that matches the `libc.a` Rust
#     ships (1.2.5, checked by SHA-256), installed with `make install-headers`,
#     which copies files and compiles nothing;
#   - clang, from the pinned LLVM or PATH, as the C compiler, told the triple
#     and pointed at those headers.
#
# The headers are needed only to compile the runtime. They are not shipped and
# a program's link never reads them.
#
# Needs network the first time (the musl tarball and `rustup target add`),
# `make`, and `curl`.
#
# **Two sources for the tarball, and the same hash for both.** musl.libc.org
# was down for hours once and every musl build failed with it: the CI job,
# `package.sh` and the Alpine release builds. Alpine's distfiles mirror
# carries the same release byte for byte, so it is tried when the first
# fails, and a file from either is refused unless its SHA-256 is the one
# pinned here -- a mirror can be wrong without a build using what it sent.
# `--connect-timeout` makes a dead host fail in seconds instead of after the
# kernel's own connect timeout, which is over two minutes per attempt, and
# `--speed-limit 1024 --speed-time 30` gives up on a host that accepted and
# then sends less than 1 KB/s for 30 s -- a half-dead mirror held the build
# on its first source with the second never tried. No `--retry`: the next
# source is the retry, and curl's own would wait out a stalled host three
# times over before trying it.
# `MUSL_URLS` replaces the list (space-separated), for testing the fallback.
set -eu

MUSL_VERSION=1.2.5
MUSL_SHA256=a9a118bbe84d8764da0ea0d28b3ab3fae8477fc7e4085d90102b8596fc7c75e4
MUSL_URLS="${MUSL_URLS:-https://musl.libc.org/releases/musl-$MUSL_VERSION.tar.gz https://distfiles.alpinelinux.org/distfiles/v3.21/musl-$MUSL_VERSION.tar.gz}"

# The SHA-256 of a file, from whichever tool the host has.
sha256_of() {
    if command -v sha256sum > /dev/null 2>&1; then
        sha256sum "$1" | cut -d' ' -f1
    else
        shasum -a 256 "$1" | cut -d' ' -f1
    fi
}

# Fetches the tarball to `$1` from the first source that answers with the
# pinned hash. A source that fails, or sends anything else, is said so and
# the next is tried; nothing unverified is left at `$1`.
fetch_musl() {
    for url in $MUSL_URLS; do
        rm -f "$1.part"
        if ! curl -fsSL --connect-timeout 10 --speed-limit 1024 --speed-time 30 -o "$1.part" "$url"; then
            rm -f "$1.part"
            echo "build-musl-runtime: could not fetch $url" >&2
            continue
        fi
        have=$(sha256_of "$1.part")
        if [ "$have" != "$MUSL_SHA256" ]; then
            echo "build-musl-runtime: $url has SHA-256 $have, not $MUSL_SHA256; refused" >&2
            rm -f "$1.part"
            continue
        fi
        mv "$1.part" "$1"
        echo "build-musl-runtime: musl-$MUSL_VERSION.tar.gz from $url" >&2
        return 0
    done
    echo "build-musl-runtime: no source gave musl-$MUSL_VERSION.tar.gz with SHA-256 $MUSL_SHA256" >&2
    return 1
}

# For the test of the fallback: fetch and verify, and stop there.
if [ "${1:-}" = "--fetch-only" ]; then
    fetch_musl "${2:?usage: --fetch-only <file>}"
    exit $?
fi

triple="${1:-}"
profile_flag=""
print_headers=0
for arg in "${2:-}" "${3:-}"; do
    case "$arg" in
        --release) profile_flag="--release" ;;
        # For `package.sh`: print the header directory (which also holds
        # musl's COPYRIGHT) instead of the archive.
        --print-headers) print_headers=1 ;;
    esac
done
case "$triple" in
    x86_64-unknown-linux-musl) arch=x86_64 ;;
    aarch64-unknown-linux-musl) arch=aarch64 ;;
    *)
        echo "usage: sh scripts/build-musl-runtime.sh <x86_64|aarch64>-unknown-linux-musl [--release]" >&2
        exit 2
        ;;
esac

root=$(cd "$(dirname "$0")/.." && pwd)
cd "$root"
target_dir="${CARGO_TARGET_DIR:-$root/target}"
case "$target_dir" in
    /*) ;;
    *) target_dir="$root/$target_dir" ;;
esac

# The C compiler. The pinned LLVM first, for the reason `toolchain::linker`
# gives: a build from this tree uses the LLVM it was built against.
clang=""
if [ -n "${LLVM_SYS_221_PREFIX:-}" ] && [ -x "$LLVM_SYS_221_PREFIX/bin/clang" ]; then
    clang="$LLVM_SYS_221_PREFIX/bin/clang"
    ar="$LLVM_SYS_221_PREFIX/bin/llvm-ar"
elif command -v clang > /dev/null 2>&1; then
    clang=$(command -v clang)
    ar=$(command -v llvm-ar || command -v ar)
else
    echo "build-musl-runtime: no clang; set LLVM_SYS_221_PREFIX or put clang on PATH" >&2
    exit 1
fi

# musl's headers, once per architecture.
headers="$target_dir/musl-headers/$MUSL_VERSION/$arch"
if [ ! -f "$headers/include/setjmp.h" ]; then
    work="$target_dir/musl-headers/src"
    mkdir -p "$work"
    tarball="$work/musl-$MUSL_VERSION.tar.gz"
    # A tarball already here was verified when it was fetched, but a cache
    # can hand back anything: checked again.
    if [ -f "$tarball" ] && [ "$(sha256_of "$tarball")" != "$MUSL_SHA256" ]; then
        echo "build-musl-runtime: $tarball does not have SHA-256 $MUSL_SHA256; fetching it again" >&2
        rm -f "$tarball"
    fi
    if [ ! -f "$tarball" ]; then
        fetch_musl "$tarball"
    fi
    rm -rf "$work/musl-$MUSL_VERSION"
    tar xzf "$tarball" -C "$work"
    make -s -C "$work/musl-$MUSL_VERSION" ARCH="$arch" prefix=/ DESTDIR="$headers" install-headers
    # Beside the headers, so the license survives a cache that kept them and
    # dropped the unpacked source.
    cp "$work/musl-$MUSL_VERSION/COPYRIGHT" "$headers/COPYRIGHT"
fi
if [ ! -f "$headers/COPYRIGHT" ] && [ -f "$target_dir/musl-headers/src/musl-$MUSL_VERSION/COPYRIGHT" ]; then
    cp "$target_dir/musl-headers/src/musl-$MUSL_VERSION/COPYRIGHT" "$headers/COPYRIGHT"
fi
if [ ! -f "$headers/COPYRIGHT" ]; then
    echo "build-musl-runtime: $headers has musl's headers but not its COPYRIGHT; delete it and re-run" >&2
    exit 1
fi

# Rust's musl target: the standard library the runtime is compiled against,
# and the `libc.a` and start files a program is linked with.
if command -v rustup > /dev/null 2>&1; then
    rustup target add "$triple" > /dev/null
fi

# `cc`'s per-target variables use underscores where the triple has hyphens.
#
# **`-isystem` as well as `--sysroot`**, because which directories a sysroot
# implies is the driver's decision and drivers differ: LLVM's own clang
# searches `<sysroot>/include` for a musl triple, and Alpine's patched clang
# searched nothing under that sysroot at all (`clang -v` listed only its own
# resource directory), so on Alpine `ring` failed on `'assert.h' file not
# found` with the headers sitting in the sysroot. Naming the directory is the
# same answer from both, in the same order.
var=$(printf '%s' "$triple" | tr '-' '_')
env "CC_$var=$clang" "AR_$var=$ar" \
    "CFLAGS_$var=--target=$triple --sysroot=$headers -isystem $headers/include" \
    cargo build -p khora-rt --target "$triple" $profile_flag 1>&2

profile=debug
[ -n "$profile_flag" ] && profile=release
if [ "$print_headers" = 1 ]; then
    printf '%s\n' "$headers"
else
    printf '%s\n' "$target_dir/$triple/$profile/libkhora_rt.a"
fi
