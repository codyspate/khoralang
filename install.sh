#!/bin/sh
# Installs the Khora toolchain.
#
#     curl -fsSL https://raw.githubusercontent.com/codyspate/khoralang/main/install.sh | sh
#
# Downloads the release for this platform, checks it against the published
# checksum, and unpacks it into ~/.khora. Nothing is compiled, and the only
# thing written outside that directory is a PATH line appended to whichever of
# ~/.profile, ~/.bashrc and ~/.zshrc already exist.
#
#     --pre                 the newest release, candidates included
#     --version 0.2.0-rc.1  a particular release, latest or not
#     --to DIR              somewhere other than ~/.khora
#     --no-modify-path      never touch a shell profile
#
# **Two channels, and GitHub already had them.** A candidate is published as a
# *pre-release*, which is installable by name and is excluded from the API's
# idea of "latest" — so a plain `curl | sh` never reaches one, and `--pre` is
# how somebody volunteers to test. Candidates are their own versions,
# `0.2.0-rc.1` then `-rc.2`; the stable release is `0.2.0`, built from the same
# commit as the last candidate. Nothing is promoted: a build is what it was
# published as, and a version number never changes meaning.
#
# **A script piped into a shell is a thing to read first**, and this one is
# short on purpose. It fetches two files, verifies the second against the
# first, and unpacks. No root, no package manager, and `rm -rf ~/.khora` undoes
# it.
set -eu

REPO="codyspate/khoralang"
HOME_DIR="${KHORA_HOME:-$HOME/.khora}"
VERSION=""
PRERELEASE=0
MODIFY_PATH=1

while [ $# -gt 0 ]; do
    case "$1" in
        --version) VERSION="$2"; shift 2 ;;
        --pre|--prerelease) PRERELEASE=1; shift ;;
        --to) HOME_DIR="$2"; shift 2 ;;
        --no-modify-path) MODIFY_PATH=0; shift ;;
        -h|--help) sed -n '2,27p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
        *) echo "unknown argument: $1" >&2; exit 2 ;;
    esac
done

say() { printf '%s\n' "$*"; }
die() { printf 'install: %s\n' "$*" >&2; exit 1; }
need() { command -v "$1" > /dev/null 2>&1 || die "this needs \`$1\` and cannot find it"; }

# --- which build ------------------------------------------------------------

triple() {
    os=$(uname -s)
    arch=$(uname -m)
    case "$os" in
        Linux)  suffix="unknown-linux-gnu" ;;
        Darwin) suffix="apple-darwin" ;;
        MINGW*|MSYS*|CYGWIN*)
            die "on Windows use PowerShell:
  irm https://raw.githubusercontent.com/$REPO/main/install.ps1 | iex" ;;
        *) die "unsupported system: $os" ;;
    esac
    case "$arch" in
        x86_64|amd64) cpu="x86_64" ;;
        arm64|aarch64) cpu="aarch64" ;;
        *) die "unsupported processor: $arch" ;;
    esac
    printf '%s-%s' "$cpu" "$suffix"
}

# --- what the compiler cannot bring with it ---------------------------------

# Checked before downloading eighty megabytes, because a toolchain that unpacks
# and then cannot link is a worse first five minutes than a warning. A warning
# rather than a refusal: somebody may be installing on one machine to build on
# another, and this script does not get to decide that.
check_linker() {
    if command -v clang > /dev/null 2>&1 || command -v cc > /dev/null 2>&1 \
        || command -v gcc > /dev/null 2>&1; then
        return 0
    fi
    say ""
    say "  No C driver found on PATH."
    say ""
    say "  Khora compiles to a native object and needs one to link it against"
    say "  this platform's runtime, which is the requirement rustc has too."
    case "$(uname -s)" in
        Darwin) say "    xcode-select --install" ;;
        *)      say "    apt install clang     (or your package manager's clang/gcc)" ;;
    esac
    say ""
    say "  Installing anyway; \`khora build\` will say the same until one exists."
    say ""
}

# --- fetch ------------------------------------------------------------------

need uname
need tar
if command -v curl > /dev/null 2>&1; then
    fetch() { curl -fsSL "$1" -o "$2"; }
    fetch_stdout() { curl -fsSL "$1"; }
elif command -v wget > /dev/null 2>&1; then
    fetch() { wget -qO "$2" "$1"; }
    fetch_stdout() { wget -qO- "$1"; }
else
    die "this needs \`curl\` or \`wget\` and cannot find either"
fi

# The newest stable release. `/releases/latest` already excludes drafts and
# pre-releases, which is the whole reason candidates are published as
# pre-releases: there is nothing extra to filter here.
#
# Read without a JSON parser — one field, one line, and `jq` is not on a fresh
# machine.
latest() {
    fetch_stdout "https://api.github.com/repos/$REPO/releases/latest" \
        | tr ',' '\n' \
        | sed -n 's/.*"tag_name"[ ]*:[ ]*"\([^"]*\)".*/\1/p' \
        | head -1
}

# The newest release of any kind. `/releases` is newest first, so this is the
# first `tag_name` in it.
#
# **Newest, not "newest candidate".** `--pre` means "include candidates", the
# way it does everywhere else, rather than "only candidates". The difference
# shows the day after a stable release: under the narrower reading `--pre`
# would install the candidate that *preceded* it, which is older than what a
# plain install gets and is nobody's idea of the bleeding edge.
newest_any() {
    fetch_stdout "https://api.github.com/repos/$REPO/releases" \
        | tr ',' '\n' \
        | sed -n 's/.*"tag_name"[ ]*:[ ]*"\([^"]*\)".*/\1/p' \
        | head -1
}

# `<file>.sha256` holds `<digest>  <name>`, as `sha256sum` writes it.
verify() {
    expected=$(cut -d' ' -f1 < "$2")
    if command -v sha256sum > /dev/null 2>&1; then
        actual=$(sha256sum "$1" | cut -d' ' -f1)
    elif command -v shasum > /dev/null 2>&1; then
        actual=$(shasum -a 256 "$1" | cut -d' ' -f1)
    else
        say "  no sha256 tool; skipping verification"
        return 0
    fi
    [ "$expected" = "$actual" ] || die "checksum mismatch:
  expected $expected
  got      $actual
The download is not what the release says it is. Do not use it."
}

TRIPLE=$(triple)
check_linker

if [ -n "$VERSION" ]; then
    TAG="v${VERSION#v}"
elif [ "$PRERELEASE" -eq 1 ]; then
    TAG=$(newest_any)
    [ -n "$TAG" ] || die "nothing has been released yet.
See https://github.com/$REPO/releases"
else
    TAG=$(latest)
    [ -n "$TAG" ] || die "could not find a stable release. Is there one yet?
There may be a candidate: try --pre, or see https://github.com/$REPO/releases"
fi
NUMBER=${TAG#v}

NAME="khora-$NUMBER-$TRIPLE"
BUNDLE="$NAME.tar.gz"
BASE="https://github.com/$REPO/releases/download/$TAG"

case "$NUMBER" in
    *-*) say "Khora $NUMBER for $TRIPLE  (a release candidate)" ;;
    *)   say "Khora $NUMBER for $TRIPLE" ;;
esac

WORK=$(mktemp -d)
# Removed however this exits, including the failure paths below.
trap 'rm -rf "$WORK"' EXIT INT TERM

say "  downloading"
fetch "$BASE/$BUNDLE" "$WORK/$BUNDLE" \
    || die "no build for $TRIPLE in $TAG.
See https://github.com/$REPO/releases/tag/$TAG for what was published."
fetch "$BASE/$BUNDLE.sha256" "$WORK/$BUNDLE.sha256" \
    || die "the release has no checksum for $BUNDLE, so it cannot be verified"

say "  verifying"
verify "$WORK/$BUNDLE" "$WORK/$BUNDLE.sha256"

say "  unpacking into $HOME_DIR"
tar xzf "$WORK/$BUNDLE" -C "$WORK"
# Replaced rather than merged: a file left over from an older release is a file
# the new compiler was never tested against.
rm -rf "$HOME_DIR"
mkdir -p "$(dirname "$HOME_DIR")"
mv "$WORK/$NAME" "$HOME_DIR"

# --- PATH -------------------------------------------------------------------

BIN="$HOME_DIR/bin"
case ":$PATH:" in
    *":$BIN:"*) ON_PATH=1 ;;
    *) ON_PATH=0 ;;
esac

if [ "$ON_PATH" -eq 0 ] && [ "$MODIFY_PATH" -eq 1 ]; then
    for profile in "$HOME/.profile" "$HOME/.bashrc" "$HOME/.zshrc"; do
        [ -f "$profile" ] || continue
        grep -q "$BIN" "$profile" 2>/dev/null && continue
        printf '\n# Added by the Khora installer\nexport PATH="%s:$PATH"\n' "$BIN" >> "$profile"
        say "  added $BIN to $profile"
    done
fi

say ""
say "Installed. $("$BIN/khora" --version 2>/dev/null || echo "khora $NUMBER")"
if [ "$ON_PATH" -eq 0 ]; then
    say ""
    say "  Open a new shell, or for this one:"
    say "    export PATH=\"$BIN:\$PATH\""
fi
say ""
say "  khora --help        what it can do"
say "  khora build .       compile the package in this directory"
case "$NUMBER" in
    *-*)
        say ""
        say "  This is a candidate. Please report what breaks:"
        say "    https://github.com/$REPO/issues"
        ;;
esac
say ""
say "  Uninstall with: rm -rf $HOME_DIR"
