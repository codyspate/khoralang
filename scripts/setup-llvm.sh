#!/bin/sh
# Puts LLVM 22.1.8 where `--features llvm` can find it, and says how to say so.
#
# The version is pinned by `llvm-sys` 221.0.1, which is pinned by `inkwell`
# 0.10's `llvm22-1` feature. A different LLVM does not work; it does not
# half-work.
#
# **The three platforms need three different things**, and only one of them is
# hard. `docs/llvm-setup.md` has the why for each.
#
#   macOS, Linux   `brew install llvm@22` — pinned at exactly 22.1.8, bottled.
#   Debian/Ubuntu  apt.llvm.org's installer, which has a 22 channel.
#   Windows        The official tarball, plus two workarounds.
#
# Run it, then export what it prints. Nothing here writes to your shell profile
# and nothing needs administrator rights.
#
# Usage:  sh scripts/setup-llvm.sh [--quiet]

set -eu

VERSION=22.1.8
MAJOR=22
PREFIX_VAR=LLVM_SYS_221_PREFIX

say() { [ "${QUIET:-0}" = 1 ] || printf '%s\n' "$*" >&2; }
die() { printf 'setup-llvm: %s\n' "$*" >&2; exit 1; }

QUIET=0
[ "${1:-}" = "--quiet" ] && QUIET=1

# Already have one? Believe it, but check the version — a wrong LLVM is the
# failure this script exists to prevent, and it presents as unrelated crashes
# deep inside code generation rather than as a version complaint.
# `sh` gives && and || the same precedence, so the two spellings of the
# executable get a helper rather than being joined into one condition that does
# not mean what it reads as.
config_in() {
    if [ -x "$1/bin/llvm-config" ]; then printf '%s\n' "$1/bin/llvm-config"
    elif [ -x "$1/bin/llvm-config.exe" ]; then printf '%s\n' "$1/bin/llvm-config.exe"
    fi
}

existing=$(printenv "$PREFIX_VAR" 2>/dev/null || true)
if [ -n "$existing" ]; then
    found=$(config_in "$existing")
    if [ -n "$found" ]; then
        have=$("$found" --version 2>/dev/null || echo unknown)
        case "$have" in
            "$MAJOR".*)
                say "$PREFIX_VAR already points at LLVM $have."
                printf '%s\n' "$existing"
                exit 0
                ;;
            *) die "$PREFIX_VAR points at LLVM $have; this needs $MAJOR.x. Unset it to install one." ;;
        esac
    fi
    say "$PREFIX_VAR is set to $existing, which holds no llvm-config. Ignoring it."
fi

case "$(uname -s)" in
    Darwin | Linux)
        if ! command -v brew >/dev/null 2>&1; then
            if [ "$(uname -s)" = Linux ] && command -v apt-get >/dev/null 2>&1; then
                say "No Homebrew. Using apt.llvm.org, which needs sudo."
                tmp=$(mktemp -d)
                curl -fsSL https://apt.llvm.org/llvm.sh -o "$tmp/llvm.sh"
                chmod +x "$tmp/llvm.sh"
                sudo "$tmp/llvm.sh" "$MAJOR"
                rm -rf "$tmp"
                prefix="/usr/lib/llvm-$MAJOR"
            else
                die "Install Homebrew (https://brew.sh) and re-run, or set $PREFIX_VAR yourself."
            fi
        else
            # `llvm@22` rather than `llvm`, which moves to 23 the day it is cut.
            say "Installing llvm@$MAJOR with Homebrew (bottled; no compiling)."
            brew list --formula "llvm@$MAJOR" >/dev/null 2>&1 || brew install "llvm@$MAJOR"
            prefix=$(brew --prefix "llvm@$MAJOR")
        fi
        ;;
    MINGW* | MSYS* | CYGWIN*)
        # **Not the installer and not winget.** Both ship the tools without
        # `llvm-config`, the `llvm-c` headers or the static libraries, and
        # `llvm-sys` needs all three. §1 of docs/llvm-setup.md.
        prefix="$HOME/.llvm/llvm-$VERSION"
        if [ ! -x "$prefix/bin/llvm-config.exe" ]; then
            arch=$(uname -m)
            case "$arch" in
                x86_64) asset="clang+llvm-$VERSION-x86_64-pc-windows-msvc.tar.xz" ;;
                aarch64 | arm64) asset="clang+llvm-$VERSION-aarch64-pc-windows-msvc.tar.xz" ;;
                *) die "no official Windows tarball for $arch" ;;
            esac
            say "Downloading $asset (~862 MB, ~5 GB extracted)."
            mkdir -p "$HOME/.llvm"
            curl -fsSL --retry 3 \
                -o "$HOME/.llvm/llvm.tar.xz" \
                "https://github.com/llvm/llvm-project/releases/download/llvmorg-$VERSION/$(printf '%s' "$asset" | sed 's/+/%2B/')"
            say "Extracting."
            ( cd "$HOME/.llvm" && tar -xJf llvm.tar.xz )
            # The rename matters: a `+` in a path breaks enough build tooling
            # to be worth avoiding.
            mv "$HOME/.llvm/${asset%.tar.xz}" "$prefix"
            rm -f "$HOME/.llvm/llvm.tar.xz"
        fi

        # `llvm-config --system-libs` advertises `xml2s.lib`, which the
        # distribution does not ship. Only `LLVMWindowsManifest` needs it and
        # nothing here pulls that in, so an inert archive satisfies the
        # linker's file-existence check. §2 of docs/llvm-setup.md.
        if [ ! -f "$prefix/lib/xml2s.lib" ]; then
            say "Supplying the stub xml2s.lib."
            ( cd "$prefix" \
                && echo "int khora_xml2_stub_placeholder = 0;" > stub.c \
                && ./bin/clang.exe -c stub.c -o stub.obj \
                && ./bin/llvm-lib.exe "/OUT:lib/xml2s.lib" stub.obj \
                && rm -f stub.c stub.obj )
        fi
        ;;
    *) die "unrecognised platform $(uname -s); set $PREFIX_VAR yourself." ;;
esac

config=$(config_in "$prefix")
[ -n "$config" ] || die "no llvm-config under $prefix — the install did not produce a usable LLVM."

have=$("$config" --version)
case "$have" in
    "$MAJOR".*) ;;
    *) die "installed LLVM is $have, not $MAJOR.x" ;;
esac

say ""
say "LLVM $have is at $prefix. Export this, then build:"
say ""
say "    export $PREFIX_VAR=\"$prefix\""
say "    cargo test --workspace --features llvm"
say ""
printf '%s\n' "$prefix"
