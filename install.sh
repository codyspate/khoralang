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
# **Run this once.** After it, `khora` manages itself: `khora update` gets the
# next release, `khora toolchain install` gets a particular one, and
# `khora toolchain default` chooses between them. There is no second program to
# learn -- what this script exists for is the moment before there is a `khora`
# at all.
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

# **Not `sed "$0"`, which is how this was written and why `--help` was broken.**
# Piped into a shell there is no script file to read back -- `$0` is `sh` --
# so the documented form printed `sed: can't read sh` and exited 0. It was also
# pinned to a line range, so editing the header above silently changed what the
# help said.
usage() {
    cat <<'END'
Installs the Khora toolchain into ~/.khora.

    curl -fsSL https://raw.githubusercontent.com/codyspate/khoralang/main/install.sh | sh

    --pre                 the newest release, candidates included
    --version 0.1.0-rc.2  a particular release, latest or not
    --to DIR              somewhere other than ~/.khora
    --no-modify-path      never touch a shell profile

Arguments need `sh -s --` when this is piped:

    curl -fsSL .../install.sh | sh -s -- --pre

A candidate is published as a pre-release, and a plain run takes only the
newest release that is not one -- so it never reaches a candidate, and --pre
is how somebody volunteers to test. It means "candidates as well", not
"candidates only".

Nothing is compiled, nothing needs root, and `rm -rf ~/.khora` undoes it.
END
}

REPO="codyspate/khoralang"

# **The oldest glibc the published Linux build runs on.** Not a preference: a
# glibc program carries the symbol versions of the machine that compiled it,
# the Linux release is compiled on GitHub's `ubuntu-latest` runner
# (`.github/workflows/release.yml`), and that runner is Ubuntu 24.04, whose
# glibc is 2.39. The 0.1.0 binary asks for `GLIBC_2.39` and will not start
# against anything older.
#
# So this number is a fact about the release rather than a policy, and it moves
# only when the image that builds the release moves. Anybody changing the
# runner there has to change it here, and `scripts/check-install.sh` is what
# says whether the pair is still telling the truth.
MIN_GLIBC="2.39"
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
        -h|--help) usage; exit 0 ;;
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

# **The C library is not a warning, because the binary cannot start without
# it.** The linker above is missing at *build* time and only for programs;
# a glibc older than the one the release was compiled against stops `khora`
# itself, on every command, including `--version`. Checked here for the same
# reason the linker is -- before eighty megabytes are downloaded -- and refused
# rather than warned about, because there is nothing an install can leave
# behind here that would ever run.
#
# `ldd --version` prints the version on its first line, last field:
#
#     ldd (Debian GLIBC 2.36-9+deb12u14) 2.36
#     ldd (Ubuntu GLIBC 2.39-0ubuntu8.8) 2.39
#
# A C library that is not glibc answers differently or not at all -- musl's
# `ldd` prints usage to stderr and exits 1 -- and an unreadable answer is
# treated as no answer. Guessing "too old" from a line this does not recognise
# would refuse installs that work; the check after unpacking is the backstop
# for everything this cannot see.
host_glibc() {
    command -v ldd > /dev/null 2>&1 || return 0
    ldd --version 2>/dev/null | head -n 1 | awk '{ print $NF }' \
        | grep -E '^[0-9]+\.[0-9]+$' || true
}

# True when $1 is an older glibc than $2. Two numeric fields, compared as
# numbers: `2.9` is older than `2.36`, which a string comparison gets backwards.
glibc_older() {
    awk -v have="$1" -v want="$2" 'BEGIN {
        split(have, h, ".")
        split(want, w, ".")
        if (h[1] + 0 != w[1] + 0) { exit (h[1] + 0 < w[1] + 0) ? 0 : 1 }
        exit (h[2] + 0 < w[2] + 0) ? 0 : 1
    }'
}

check_glibc() {
    [ "$(uname -s)" = "Linux" ] || return 0
    HAVE_GLIBC=$(host_glibc)
    [ -n "$HAVE_GLIBC" ] || return 0
    glibc_older "$HAVE_GLIBC" "$MIN_GLIBC" || return 0

    say ""
    say "  This machine's C library is older than the published build needs."
    say ""
    say "    this machine   glibc $HAVE_GLIBC"
    say "    the release    glibc $MIN_GLIBC or newer"
    say ""
    say "  Nothing is wrong with your machine, and nothing you can install"
    say "  fixes it: the Linux release is compiled on a newer distribution, and"
    say "  a glibc program cannot run against a library older than the one it"
    say "  was linked against. Downloading it would leave you with a \`khora\`"
    say "  that fails on every command, so this stops before the download."
    say ""
    say "  Distributions with glibc $MIN_GLIBC or newer include Ubuntu 24.04 and"
    say "  Debian 13. Debian 12 (2.36), Ubuntu 22.04 (2.35) and RHEL 9 (2.34)"
    say "  are older than the release and are not supported by it."
    say ""
    say "  Two ways forward:"
    say ""
    say "    - install on a newer distribution, or in a container built on one;"
    say "    - build the toolchain from source here, which links it against the"
    say "      glibc you have:"
    say "      https://khoralang.com/docs/getting-started/installation/"
    say ""
    die "glibc $HAVE_GLIBC is older than the $MIN_GLIBC this release needs"
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

# Every toolchain release, newest first, as `<tag> pre` or `<tag> stable`.
#
# **`/releases/latest` is not asked any more, and the reason is a bug this
# had.** That endpoint means "the newest release in this repository that is
# not a draft or a pre-release" — the whole repository. The VS Code extension
# is released from here too, on `vscode-v*` tags, and it is not a pre-release,
# because there is nothing provisional about it. So `/releases/latest` began
# returning the editor extension, `${TAG#v}` turned `vscode-v0.3.0` into
# `scode-v0.3.0`, and a plain `curl | sh` went looking for
# `khora-scode-v0.3.0-<triple>.tar.gz`. `--pre` broke the same way, because
# `/releases` is newest first and the extension was newest.
#
# So the filtering happens here: keep the tags that name a toolchain, and
# choose between stable and candidate from the release's own flag.
#
# Read without a JSON parser — `jq` is not on a fresh machine. `tr` puts one
# field on each line, and `awk` pairs each `tag_name` with the `prerelease`
# that follows it inside the same release. The fields GitHub sends before
# `tag_name` — `url`, `id`, `author`, `node_id` — contain no `prerelease`, so
# the pairing cannot cross from one release into the next.
#
# A tag names a toolchain when it is `v` and then a digit. `vscode-v0.3.0`
# also starts with `v`; the digit is what tells them apart.
releases() {
    fetch_stdout "https://api.github.com/repos/$REPO/releases" \
        | tr ',' '\n' \
        | awk '
            /"tag_name"[ ]*:/ {
                tag = $0
                sub(/.*"tag_name"[ ]*:[ ]*"/, "", tag)
                sub(/".*/, "", tag)
                next
            }
            /"prerelease"[ ]*:/ {
                if (tag ~ /^v[0-9]/) {
                    print tag, ($0 ~ /true/ ? "pre" : "stable")
                }
                tag = ""
            }
        '
}

# The newest stable toolchain. Empty when every release so far is a candidate,
# which is a state this repository has been in for its whole life and which the
# caller reports rather than papering over.
latest() {
    releases | awk '$2 == "stable" { print $1; exit }'
}

# The newest toolchain of any kind.
#
# **Newest, not "newest candidate".** `--pre` means "include candidates", the
# way it does everywhere else, rather than "only candidates". The difference
# shows the day after a stable release: under the narrower reading `--pre`
# would install the candidate that *preceded* it, which is older than what a
# plain install gets and is nobody's idea of the bleeding edge.
newest_any() {
    releases | awk '{ print $1; exit }'
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
check_glibc
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
    || die "no build for $TRIPLE in $TAG yet.

If that release was just created, its artifacts are still building -- try again
in a few minutes. Otherwise this platform was not published for it.

See https://github.com/$REPO/releases/tag/$TAG for what is there."
fetch "$BASE/$BUNDLE.sha256" "$WORK/$BUNDLE.sha256" \
    || die "the release has no checksum for $BUNDLE, so it cannot be verified"

say "  verifying"
verify "$WORK/$BUNDLE" "$WORK/$BUNDLE.sha256"

say "  unpacking into $HOME_DIR"
# **`--no-same-owner`, or this fails when run as root.**
#
# The archives are built by CI and carry that machine's uid -- `runner/runner`,
# 1001. GNU tar restores ownership from the archive when it believes it is
# root, and then fails on every entry because 1001 is not a user here:
#
#   tar: khora-0.1.0-.../bin/khora: Cannot change ownership to uid 1001 ..
#   tar: Exiting with failure status due to previous errors
#
# An ordinary user never saw it, because tar ignores stored ownership for
# anybody who cannot honour it -- which is why a laptop install worked and a
# `RUN` line in a Dockerfile, a CI step, or any rootless container build did
# not. That is most of the ways somebody scripts an install.
#
# Being explicit is also the right answer rather than merely a working one: a
# toolchain unpacked into `$HOME` should belong to whoever is installing it,
# and the build machine's uids mean nothing on this side.
tar --no-same-owner -xzf "$WORK/$BUNDLE" -C "$WORK"
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

# **The one command that proves the install is the one whose failure used to
# be discarded.** This line was
#
#     say "Installed. $("$BIN/khora" --version 2>/dev/null || echo "khora $NUMBER")"
#
# which ran the binary, threw its stderr away, and on failure printed a version
# string it had assembled from the tag -- so a toolchain that could not start
# reported `Installed. khora 0.1.0` and exited 0, and the difference from a
# real install was that the line was *shorter*. On Debian 12 and Ubuntu 22.04
# that is exactly what happened: every later command died with
# `libc.so.6: version 'GLIBC_2.39' not found`, and the installer had said the
# install was fine.
#
# So the run is the check now. Its output is kept, failure is not swallowed,
# and there is no synthesised fallback -- if `khora --version` cannot say what
# it is, this script has nothing true to print.
if VERSION_LINE=$("$BIN/khora" --version 2>&1); then
    say ""
    say "Installed. $VERSION_LINE"
else
    say ""
    say "  Unpacked into $HOME_DIR, and it cannot run here."
    say ""
    say "  Running \`$BIN/khora --version\` said:"
    # Indented by hand rather than through `sed`, which this script has not
    # asked for anywhere else and does not need to start depending on here.
    printf '%s\n' "$VERSION_LINE" | while IFS= read -r line; do
        say "    $line"
    done
    say ""
    if [ "$(uname -s)" = "Linux" ]; then
        HAVE_GLIBC=$(host_glibc)
        if [ -n "$HAVE_GLIBC" ]; then
            say "    this machine   glibc $HAVE_GLIBC"
            say "    the release    glibc $MIN_GLIBC or newer, as this script"
            say "                   has it -- and reaching here means either"
            say "                   that number is wrong or the cause is"
            say "                   something else entirely"
            say ""
        fi
    fi
    say "  This is not something you did wrong, and it is not a broken"
    say "  download -- the archive matched its published checksum. The build"
    say "  simply cannot start on this system."
    say ""
    say "  Two ways forward:"
    say ""
    say "    - install on a newer distribution, or in a container built on one;"
    say "    - build the toolchain from source here, which links it against"
    say "      this machine's own libraries:"
    say "      https://khoralang.com/docs/getting-started/installation/"
    say ""
    say "  Please report this, with the lines above:"
    say "    https://github.com/$REPO/issues"
    say ""
    say "  What was unpacked is still at $HOME_DIR."
    say "  Remove it with: rm -rf $HOME_DIR"
    die "the installed toolchain cannot run on this machine"
fi
if [ "$ON_PATH" -eq 0 ]; then
    say ""
    say "  Open a new shell, or for this one:"
    say "    export PATH=\"$BIN:\$PATH\""
fi
say ""
say "  khora --help        what it can do"
say "  khora build .       compile the package in this directory"
say "  khora update        get the next release, when there is one"

case "$NUMBER" in
    *-*)
        say ""
        say "  This is a candidate. Please report what breaks:"
        say "    https://github.com/$REPO/issues"
        ;;
esac
say ""
say "  Uninstall with: rm -rf $HOME_DIR"
