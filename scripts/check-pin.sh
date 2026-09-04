#!/bin/sh
# The repository's own `[toolchain]` pin, against the version it builds.
#
# **A pin is required now, and this repository is the one project a pin can
# strand.** Everywhere else a pin means "fetch that toolchain and run it". Here
# the toolchain being pinned is the one in `target/debug`, and it exists only
# because `cargo build` just made it -- so the pin is satisfied by a version
# number matching, and by nothing else.
#
# `khora_toolchain::decide` proceeds when the pin equals the running version.
# The running version of a locally built compiler is `Cargo.toml`'s. So while
# those two agree, every `khora` in this tree runs itself; the moment they
# diverge -- a version bump that misses `khora.toml`, or the reverse -- every
# command in the repository tries to hand over to an installed release instead.
#
# What that failure looks like is worth stating, because it does not look like
# this: `khora check` starts reporting errors against `~/.khora/toolchains/
# 0.1.0/std/*.kh`, paths from a toolchain nobody in the command mentioned, or
# stops with "Khora 0.3.0 is not installed" in a repository whose whole job is
# building Khora 0.3.0. Both read as a broken checkout.
#
# It is also load-bearing for the test suites. `CARGO_TARGET_TMPDIR` is inside
# `target/`, so a scratch project a test scaffolds sits underneath this
# manifest and inherits this pin -- which is what lets those tests have projects
# at all now that an unpinned one is refused.
set -eu

root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)

# The workspace version, which every crate inherits. One line, and the first
# `version =` under `[workspace.package]` is it.
cargo=$(sed -n '/^\[workspace.package\]/,/^\[/p' "$root/Cargo.toml" |
	sed -n 's/^version = "\(.*\)"$/\1/p' | head -1)

# The pin, from the root manifest's `[toolchain]`.
pin=$(sed -n '/^\[toolchain\]/,/^\[/p' "$root/khora.toml" |
	sed -n 's/^version = "\(.*\)"$/\1/p' | head -1)

if [ -z "$cargo" ]; then
	echo "check-pin: no version under [workspace.package] in Cargo.toml" >&2
	exit 1
fi

if [ -z "$pin" ]; then
	cat >&2 <<EOF
check-pin: khora.toml has no [toolchain] version.

A pin is required of every project, and this repository is a project. Add:

    [toolchain]
    version = "$cargo"
EOF
	exit 1
fi

if [ "$pin" != "$cargo" ]; then
	cat >&2 <<EOF
check-pin: khora.toml pins Khora $pin, and this tree builds $cargo.

Every \`khora\` run inside this repository will try to hand over to an
installed $pin instead of running the compiler it just built. Set them
equal -- the pin follows the version, so:

    khora.toml   [toolchain] version = "$cargo"
EOF
	exit 1
fi

echo "check-pin: khora.toml pins $pin, which is what this tree builds"
