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

say 'khora-rt on Linux'
wsl -e bash -lc ". \"\$HOME/.cargo/env\"
    cd \"\$(wslpath '$PWD')\"
    CARGO_TARGET_DIR=$TARGET cargo test -p khora-rt
    CARGO_TARGET_DIR=$TARGET cargo clippy -p khora-rt --all-targets -- -D warnings
"

say 'linux clean'
