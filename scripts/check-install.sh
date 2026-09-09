#!/bin/sh
# Does `install.sh` install a toolchain that actually runs?
#
#     sh scripts/check-install.sh
#
# **The gap this closes.** Roadmap 13.24 -- the clean-machine release test --
# says "still to do on a machine with no toolchain at all", and
# `docs/release-readiness.md` has the fresh-machine stranger test unticked.
# Every other check in this repository runs where the compiler was built, and
# so proves nothing about the one machine that matters: somebody else's, with
# no toolchain, no `std/`, and a C library that is not this one.
#
# A container is that machine. It costs no CI minutes, needs nothing installed
# but Docker, and it found a release blocker the first time it was run: the
# published 0.1.0 Linux build needs glibc 2.39, `install.sh` ran the binary with
# `2>/dev/null` and printed `Installed.` when the run failed, and every command
# after it died with `libc.so.6: version 'GLIBC_2.39' not found` on Debian 12
# and Ubuntu 22.04 -- the current Debian stable and a supported Ubuntu LTS.
#
# **What is asserted, and it is not "the installer exits 0".** That was true on
# the day the bug shipped. The invariant is that the installer and the binary
# agree:
#
#   - on a system new enough, the install succeeds *and* `khora --version`
#     runs, and says a real version rather than one assembled from the tag;
#   - on a system too old, the installer says so and exits non-zero;
#   - never, on any image, does the installer report success over a binary that
#     cannot start.
#
# Which images are which is not written down twice. Each container reports its
# own glibc, this compares it against the `MIN_GLIBC` in `install.sh`, and the
# expectation follows -- so raising the floor by rebuilding the release on an
# older base image needs one number changed in one file, and this check then
# holds the release to it.
#
# **It tests the published release**, not the working tree: `install.sh`
# downloads whatever GitHub is serving. That is deliberate. What a stranger
# gets is the artifact plus this script, and only running both together can
# catch an artifact that no longer matches what the installer believes.
#
# Skips, rather than fails, where there is no Docker -- so it can sit in
# `scripts/baseline.sh` on machines that cannot run it.
#
#     KHORA_INSTALL_IMAGES="debian:trixie-slim ubuntu:24.04"   another matrix
#     KHORA_INSTALL_ARGS="--pre"                               to install.sh
set -eu

root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
script="$root/install.sh"

[ -f "$script" ] || { echo "check-install: no install.sh at $script" >&2; exit 1; }

if ! command -v docker > /dev/null 2>&1; then
    echo "check-install: no docker on this machine; skipping."
    echo "  It installs the published release inside a container and checks"
    echo "  that the binary runs. Nothing else here can answer that."
    exit 0
fi
if ! docker info > /dev/null 2>&1; then
    echo "check-install: docker is installed but not answering; skipping."
    exit 0
fi

# Two that are older than the released build and one that is newer, which is
# the smallest matrix that can tell a refusal from a failure to install at all.
# Debian 12 is the current Debian stable and Ubuntu 22.04 is an LTS supported
# into 2027, so both are machines a stranger plausibly has.
IMAGES=${KHORA_INSTALL_IMAGES:-"debian:bookworm-slim ubuntu:22.04 ubuntu:24.04"}
ARGS=${KHORA_INSTALL_ARGS:-""}

# The floor the installer believes in, read out of the installer rather than
# repeated here.
MIN_GLIBC=$(sed -n 's/^MIN_GLIBC="\([^"]*\)".*/\1/p' "$script" | head -n 1)
[ -n "$MIN_GLIBC" ] || { echo "check-install: no MIN_GLIBC in install.sh" >&2; exit 1; }

# True when $1 is an older glibc than $2, by number and not by string.
older() {
    awk -v have="$1" -v want="$2" 'BEGIN {
        split(have, h, "."); split(want, w, ".")
        if (h[1] + 0 != w[1] + 0) { exit (h[1] + 0 < w[1] + 0) ? 0 : 1 }
        exit (h[2] + 0 < w[2] + 0) ? 0 : 1
    }'
}

field() { sed -n "s/^KHORA-CHECK $1 //p" "$2" | head -n 1; }

# Each run gets its own timeout: a hung pull or a stalled download should fail
# this check rather than a whole baseline run.
TIMEOUT=${KHORA_INSTALL_TIMEOUT:-600}
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT INT TERM

# `curl` and a certificate store are what a bare image is missing and what the
# installer's own documentation tells you to have. Installing them is not part
# of what is being tested, so its noise is dropped and only its failure is not.
inner='
    set +e
    export DEBIAN_FRONTEND=noninteractive
    if command -v apt-get > /dev/null 2>&1; then
        apt-get update -qq > /dev/null 2>&1
        apt-get install -y -qq curl ca-certificates > /dev/null 2>&1
    elif command -v apk > /dev/null 2>&1; then
        apk add --no-cache curl ca-certificates > /dev/null 2>&1
    fi
    command -v curl > /dev/null 2>&1 || { echo "KHORA-CHECK no-curl 1"; exit 0; }
    echo "KHORA-CHECK glibc $(ldd --version 2>&1 | head -n 1 | awk "{ print \$NF }")"
    sh /install.sh $KHORA_ARGS
    echo "KHORA-CHECK installer-exit $?"
    said=$("$HOME/.khora/bin/khora" --version 2>&1)
    echo "KHORA-CHECK khora-exit $?"
    echo "KHORA-CHECK khora-said $said"
'

failed=0
for image in $IMAGES; do
    printf '=== %s\n' "$image"
    log="$work/$(echo "$image" | tr '/:' '__').log"

    if ! timeout "$TIMEOUT" docker run --rm \
        -e KHORA_ARGS="$ARGS" \
        -v "$script":/install.sh:ro \
        "$image" sh -c "$inner" > "$log" 2>&1; then
        echo "  the container itself failed (pull, timeout, or no shell):"
        sed 's/^/    /' "$log"
        failed=1
        continue
    fi

    glibc=$(field glibc "$log")
    installer=$(field installer-exit "$log")
    khora=$(field khora-exit "$log")
    said=$(field khora-said "$log")

    if [ -n "$(field no-curl "$log")" ]; then
        echo "  no curl and no way to install one; not a verdict. Skipped."
        continue
    fi
    if [ -z "$installer" ] || [ -z "$khora" ]; then
        echo "  the run produced no verdict:"
        sed 's/^/    /' "$log"
        failed=1
        continue
    fi

    # The invariant that holds whatever the image is, and the one the shipped
    # bug broke: a successful install has to leave a binary that runs.
    if [ "$installer" -eq 0 ] && [ "$khora" -ne 0 ]; then
        echo "  FAIL  the installer reported success and the binary cannot run."
        echo "        This is the 0.1.0 bug. glibc here is ${glibc:-unknown}."
        sed 's/^/    /' "$log"
        failed=1
        continue
    fi

    case "$glibc" in
        [0-9]*.[0-9]*) ;;
        *)
            # Not a glibc, or a version this cannot read -- musl says
            # "Version 1.2.6". No expectation to check beyond the invariant
            # above, which already passed.
            echo "  ok    no readable glibc (${glibc:-none}); installer exit $installer, binary exit $khora"
            continue
            ;;
    esac

    if older "$glibc" "$MIN_GLIBC"; then
        if [ "$installer" -eq 0 ]; then
            echo "  FAIL  glibc $glibc is below the $MIN_GLIBC floor and the installer accepted it."
            sed 's/^/    /' "$log"
            failed=1
        else
            echo "  ok    glibc $glibc is below $MIN_GLIBC, and the installer refused (exit $installer)"
        fi
    else
        if [ "$installer" -ne 0 ] || [ "$khora" -ne 0 ]; then
            echo "  FAIL  glibc $glibc is at or above the $MIN_GLIBC floor, and this did not work."
            echo "        installer exit $installer, binary exit $khora"
            sed 's/^/    /' "$log"
            failed=1
        else
            # A version assembled from the tag is three words; a real one names
            # the commit and the triple. The short form was the tell.
            case "$said" in
                *"$(uname -m)"*|*unknown-linux-gnu*)
                    echo "  ok    glibc $glibc: installed, and it says \"$said\"" ;;
                *)
                    echo "  FAIL  glibc $glibc: it ran, and said \"$said\","
                    echo "        which is not a full version line."
                    failed=1 ;;
            esac
        fi
    fi
done

echo
if [ "$failed" -ne 0 ]; then
    echo "check-install: the published release and install.sh do not agree." >&2
    exit 1
fi
echo "check-install: every image agrees with the $MIN_GLIBC floor in install.sh."
