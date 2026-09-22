#!/bin/sh
# Every `KHORA_*` the shipped compiler and runtime read is either documented or
# deliberately not.
#
# **The failure this prevents.** An environment variable is a public interface
# that nothing declares. A `std::env::var` added in a runtime file changes what
# a compiled program does on a machine that happens to have the name exported,
# and there is no signature, no manifest key and no `--help` entry anywhere for
# a reader to find it from. `KHORA_HOME` -- which decides where every cached
# artifact on the machine lands -- was read by two crates and named on no
# documentation page at all.
#
# So this compares the names shipped code reads, measured from the source,
# against three lists that must between them account for all of them:
#
#   - `website/content/docs/reference/environment.md`, the supported knobs;
#   - INTERNAL below, ours rather than a user's;
#   - UNDECIDED below, behaviour a user can reach that nobody has yet decided
#     should be reachable that way.
#
# A name in none of the three fails the check.
#
# **What it does not do.** It does not read the documentation for accuracy. A
# page that names `KHORA_CACHE_BUDGET` and describes the wrong default passes
# this and is still wrong; only a person catches that. What it catches is the
# silent case -- a variable added with no entry anywhere -- which is the one
# nobody notices, because nothing about adding it looks like documenting it.
#
# It also cannot see a name assembled at run time. `format!("KHORA_{what}")`
# reads as no name at all here and would pass unnoticed. Nothing in the tree
# does that today; if something starts to, this check is not the thing that
# will tell you.
#
#     sh scripts/check-environment-surface.sh
set -eu

root=$(cd "$(dirname "$0")/.." && pwd)
cd "$root"

page=website/content/docs/reference/environment.md

[ -f "$page" ] || {
    echo "  FAILED  no $page" >&2
    echo "          Every KHORA_* a user may set is documented there." >&2
    exit 1
}

python3 - "$page" <<'PY'
import re, sys, pathlib

page = pathlib.Path(sys.argv[1])

# **Internal: read by our own scaffolding, and not a knob anybody may set.**
# A bare name here is how this list stops meaning anything, so each carries the
# reason it is not on the page. Removing an entry is the way to propose that a
# variable become supported -- the check then demands a documented entry.
INTERNAL = {
    "KHORA_TOOLCHAIN":
        "set by `khora` on the child it re-execs, so a shim cannot exec forever. "
        "A protocol between two processes rather than an input; a value set by "
        "hand makes toolchain handover stop without saying so.",
    "KHORA_RELEASE":
        "read at compile time by `crates/khora-toolchain/build.rs` so a packaged "
        "binary reports the tag it was published as. `scripts/package.sh` sets "
        "it; it does nothing in the environment of a `khora` that already exists.",
    "KHORA_VERSION_LINE":
        "written by `crates/khora-toolchain/build.rs` for `khora --version` to "
        "print. A build script's own output, never read from a shell.",
    "KHORA_SOAK_MINUTES": "bounds the runtime soak in `crates/khora-rt/src/soak.rs`, which is `#![cfg(test)]`.",
    "KHORA_SOAK_PATIENCE": "how long the soak waits for a settled pool before calling it a hang; test-only.",
    "KHORA_SOAK_ROUNDS": "how much of the soak workload to run; test-only.",
    "KHORA_SOAK_SEED": "fixes the soak's workload so a failure can be re-run; test-only.",
    "KHORA_SOAK_WORKERS": "how many scheduler workers the soak starts; test-only.",
}

# **Reachable from a user's shell, and not decided.** These change what a
# compiled program does and are here because saying so is better than either
# documenting them as knobs -- which promises they will not move -- or leaving
# them off every list, which is what this check exists to refuse. Each entry
# says what it ought to be instead.
UNDECIDED = {
    "KHORA_UNBOXED":
        "turns off flat layout for small values whole-program. A representation "
        "decision belongs to a flag or to nothing at all; it is an environment "
        "variable because a bisect needed to separate a miscompile from the "
        "change that exposed it.",
    "KHORA_NO_SIGNALS":
        "stops the signal watcher installing, so a shutdown test can run its "
        "control half. It ships in every program, so any process with the name "
        "exported loses graceful shutdown and there is no way to notice.",
}

# A name documented as a knob is a heading that is nothing but the name:
# `### `KHORA_NAME``. Matching whole headings rather than any mention is
# deliberate twice over. A page that merely says the words `KHORA_FIBERS` in a
# sentence about something else has not documented it and would otherwise
# satisfy this check forever; and a heading that qualifies the name -- "not a
# knob", "removed" -- is saying the opposite of what a documented entry says,
# so it must not count as one.
documented = set(
    re.findall(r"^#{2,4}\s+`(KHORA_[A-Z0-9_]+)`\s*$", page.read_text(encoding="utf-8"), re.M)
)

def shipped(text: str) -> str:
    """`text` with the parts that do not ship removed.

    Two shapes, which are the two this tree uses: a whole file behind
    `#![cfg(test)]`, and a trailing `#[cfg(test)] mod tests`. Anything a test
    module reads is scaffolding, and sorting it as a public surface would put
    `KHORA_SOAK_SEED` in front of a user as a knob.
    """
    if re.search(r"^#!\[cfg\(test\)\]", text, re.M):
        return ""
    cut = re.search(r"^#\[cfg\(test\)\]\s*\nmod tests\b", text, re.M)
    return text[: cut.start()] if cut else text

# A name in a string literal or an `env!`/`option_env!`. String literals rather
# than `env::var` call sites alone, because the name is not always at the call:
# `khora-toolchain` binds `KHORA_TOOLCHAIN` to a `const` and reads *that*, and
# a check that only saw `env::var("...")` would have missed it. The cost is
# that a name quoted inside an error message counts as read -- which is the
# safe direction, since a message that tells somebody to set a variable has
# published it just as surely.
NAME = re.compile(r'"(KHORA_[A-Z0-9_]+)"')

read = {}
for path in sorted(pathlib.Path("crates").glob("*/")):
    for src in sorted(list((path).glob("src/**/*.rs")) + list(path.glob("build.rs"))):
        text = shipped(src.read_text(encoding="utf-8"))
        for name in NAME.findall(text):
            read.setdefault(name, []).append(str(src))

# The soak is `#![cfg(test)]` and `shipped` removes it, so its names are read
# by nothing here by construction. They stay on INTERNAL anyway, because a
# reader who greps the tree finds them and the list is where the answer is.
TEST_ONLY = {n for n in INTERNAL if n.startswith("KHORA_SOAK_")}

declared = set(documented) | set(INTERNAL) | set(UNDECIDED)
undeclared = sorted(n for n in read if n not in declared)
stale = sorted(n for n in declared - TEST_ONLY if n not in read)

overlap = sorted((set(documented) & set(INTERNAL)) | (set(documented) & set(UNDECIDED)))
if overlap:
    print("  FAILED  a KHORA_* is both documented and not", file=sys.stderr)
    for name in overlap:
        print(f"          {name}", file=sys.stderr)
    print(
        "\n  A variable is supported or it is not, and a reader who finds it on the\n"
        "  page has been told it is. Drop it from INTERNAL or UNDECIDED in this\n"
        f"  script, or from {page}.\n",
        file=sys.stderr,
    )
    sys.exit(1)

if undeclared:
    print("  FAILED  a KHORA_* variable shipped code reads is in none of the three lists", file=sys.stderr)
    for name in undeclared:
        where = ", ".join(sorted(set(read[name]))[:3])
        print(f"\n          {name}", file=sys.stderr)
        print(f"            read by: {where}", file=sys.stderr)
    print(
        "\n  A variable that changes behaviour and is written down nowhere is a\n"
        "  surface nobody can rely on and nobody can avoid depending on. One of:\n"
        "\n"
        f"    - document it as a knob in {page}, under a\n"
        "      `### `NAME`` heading, with what it does, its default and its legal\n"
        "      values; or\n"
        "    - add it to INTERNAL in this script with the reason it is ours and\n"
        "      not a user's; or\n"
        "    - add it to UNDECIDED with what it ought to be instead, if it is\n"
        "      behaviour that should not have been an environment variable.\n",
        file=sys.stderr,
    )
    sys.exit(1)

if stale:
    print("  FAILED  a KHORA_* is declared and nothing reads it", file=sys.stderr)
    for name in stale:
        print(f"          {name}", file=sys.stderr)
    print(
        "\n  Naming a variable the compiler stopped reading tells somebody to set\n"
        f"  something that does nothing. Remove it from {page}, or from\n"
        "  INTERNAL or UNDECIDED in this script.\n",
        file=sys.stderr,
    )
    sys.exit(1)

print(
    f"  ok    {len(read)} KHORA_* read by shipped code: {len(documented)} documented, "
    f"{len(INTERNAL) - len(TEST_ONLY)} internal, {len(UNDECIDED)} undecided"
)
PY
