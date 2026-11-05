#!/bin/sh
# Builds and packages the Khora toolchain for Alpine, inside Alpine.
#
#     docker run --rm -v "$PWD:/src" -w /src alpine:3.24 \
#         sh scripts/build-alpine.sh 0.5.0
#
# Writes `dist/khora-<version>-<arch>-unknown-linux-musl.tar.gz` and its
# `.sha256`, through `scripts/package.sh` like every other platform, and the
# archive's bill of materials, `.cdx.json`, with its own `.sha256`.
#
# **Why inside Alpine, and not a cross build from Ubuntu.** `khora` links
# LLVM, and a static musl `khora` needs LLVM's libraries built for musl. LLVM
# publishes none; Alpine ships them (`llvm22-static`, for x86-64 and arm64), so
# building here costs a package install where a cross build would cost building
# LLVM itself, per architecture, on every cache miss. What comes out is one
# static-pie executable with no `NEEDED` entries: it starts on any Linux kernel
# of its architecture, Alpine or not.
#
# **What it costs:** Alpine 3.24's LLVM is 22.1.3 where the other toolchains
# link the 22.1.8 `scripts/setup-llvm.sh` pins. `llvm-sys` accepts any 22.1.x,
# and the C API does not change within a minor release; Alpine's edge branch
# has 22.1.8, and moving to it is a one-line change here when a stable branch
# carries it. The release build takes about five minutes on four cores.
#
# Cargo's caches go where `CARGO_HOME`, `RUSTUP_HOME` and `CARGO_TARGET_DIR`
# say, so CI can keep them between runs by mounting a directory for them.
set -eu

version=${1:?usage: sh scripts/build-alpine.sh <version>}

# `build-base` for `make` and the C toolchain `cc` crates use; `llvm22-static`
# and the three `-static` libraries it names in `llvm-config --system-libs`
# (`-lz -lzstd -lxml2`) for the link; `clang22` and `lld22` for the musl
# runtime `scripts/build-musl-runtime.sh` compiles; `curl` for that script's
# musl headers; `git` for the commit `khora --version` reports; `python3` for
# the bill of materials.
apk add --no-cache \
    build-base bash curl git file linux-headers python3 \
    llvm22-dev llvm22-static clang22 lld22 \
    zlib-static zstd-static libxml2-static libffi-dev

export CARGO_HOME="${CARGO_HOME:-/root/.cargo}"
export RUSTUP_HOME="${RUSTUP_HOME:-/root/.rustup}"
if [ ! -x "$CARGO_HOME/bin/cargo" ]; then
    curl -fsSL --retry 3 https://sh.rustup.rs -o /tmp/rustup-init.sh
    sh /tmp/rustup-init.sh -y --profile minimal --default-toolchain stable --no-modify-path
fi
export PATH="$CARGO_HOME/bin:$PATH"
rustup target add x86_64-unknown-linux-musl aarch64-unknown-linux-musl

export LLVM_SYS_221_PREFIX=/usr/lib/llvm22
# The checkout belongs to whoever ran `docker`, not to root in here, and git
# refuses to read a repository owned by somebody else. Not fatal: without git
# the version line says `unknown` for the commit, which is all it costs.
git config --global --add safe.directory "$(pwd)" 2> /dev/null || true

sh scripts/package.sh --version "$version"

# **A bill of materials for this archive**, as the release has for the
# others -- and one that says more, because more is inside. The other
# toolchains link their C libraries dynamically from the machine; this one is
# a static executable, so musl, libstdc++, zlib, zstd and LLVM are in it, at
# the versions this image's `apk` installed, and an advisory against any of
# them is an advisory against this archive. Only the packages whose code is
# linked in: `llvm-config` also names `libxml2`, and `libffi-dev` is installed,
# but no symbol from either is in the executable, and a bill of materials
# that named them would send an auditor after code that is not there. The
# unwinder is LLVM's `libunwind`, which Rust's musl target carries and the
# Rust entry in the document covers.
arch=$(uname -m)
name="khora-${version}-${arch}-unknown-linux-musl"
apk_version() { apk list --installed "$1" 2> /dev/null | awk -v p="$1-" '
    index($1, p) == 1 { print substr($1, length(p) + 1); exit }'; }
set --
for pkg in musl-dev libstdc++-dev zlib-static zstd-static llvm22-static; do
    v=$(apk_version "$pkg")
    [ -n "$v" ] || { echo "build-alpine: $pkg is not installed, so the bill of materials cannot name it" >&2; exit 1; }
    set -- "$@" --apk "$pkg=$v"
done
llvm=$(/usr/lib/llvm22/bin/llvm-config --version)
python3 scripts/toolchain-sbom.py --llvm "$llvm" "$@" > "dist/$name.cdx.json"
( cd dist && sha256sum "$name.cdx.json" > "$name.cdx.json.sha256" )
