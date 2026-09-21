#!/bin/sh
# Test fixtures that hand-copy a `std` signature must still match it.
#
# **The failure this prevents.** `crates/khora-codegen-llvm/tests/*.rs` embed
# Khora programs as Rust string literals, and those programs declare their own
# copy of whatever `std` items they use -- `Fiber`, `Channel`, `Region`. They
# are not `import`ed; the fixture is a self-contained module. So when a `std`
# signature changes, the fixtures keep compiling against the old one until the
# type checker refuses them, and that refusal arrives as a wall of unrelated
# errors from the full LLVM suite, twenty-eight minutes at a time.
#
# That happened when `Fiber::wait` gained its `raises` row: twenty-five call
# sites across eight files, found over four rounds, none of them visible from
# the change that caused them. `grep` for callers in `std/`, `packages/` and
# `examples/` -- which is what the developer did, correctly -- finds none of
# these, because they are not `.kh` files.
#
# **What this compares.** Every `fn NAME(...)` declared inside an `impl` block
# in a fixture, against the same method on the same type in `std/core.kh`,
# modulo two differences that are not drift:
#
#   - `pub`, which a fixture omits.
#   - The name of a row variable. `'r` and `'er` are both bound by the
#     signature that introduces them, so a fixture that says `'r` where `std`
#     says `'er` is spelling the same type.
#
# Anything else -- a missing row, an extra parameter, a changed return type --
# is drift, and drift is what this refuses.
#
# **What it does not do.** It does not compile anything. A fixture may
# legitimately declare a *narrower* thing than `std` offers, and a few do; the
# allow-list below names them with the reason. This catches the case where the
# fixture and `std` disagree about a signature that both claim to have.
set -eu

root=$(cd "$(dirname "$0")/.." && pwd)
cd "$root"

std=std/core.kh
fixtures=crates/khora-codegen-llvm/tests

[ -f "$std" ] || { echo "no $std" >&2; exit 1; }

python3 - "$std" "$fixtures" <<'PY'
import re, sys, pathlib

std_path, fixture_dir = sys.argv[1], sys.argv[2]

# A fixture may narrow a signature on purpose. Name it here with the reason;
# an entry with no reason is how this check stops meaning anything.
ALLOWED = {
    # `Fibers::wait` is the nursery's, not `Fiber`'s -- a different function
    # that happens to share a name, and the fixtures declare it as `-> Int`.
    ("Fibers", "wait"),
    # `channels.rs` has two preludes. The second (`PRELUDE`, line ~275) is for
    # tests that never cancel anything and declares `Channel` without its
    # `raises` row on purpose -- a narrower channel, so those programs need no
    # `!` and the tests read as what they are about. The first prelude carries
    # the real signature, so the row is covered.
    ("Channel", "send"),
    ("Channel", "receive"),
}

def normalise(sig: str) -> str:
    """A signature with `pub` gone, bodies gone, and rows spelled `'_`.

    A `std` method may carry its body on the same line -- `fn show(self) ->
    String { Int::to_string(self) }` -- while the fixture declares only the
    signature. The body is not part of what a fixture copies, so it goes.
    """
    sig = sig.strip()
    sig = re.sub(r"\{.*$", "", sig)          # an inline body, if any
    sig = sig.rstrip(";").strip()
    sig = re.sub(r"^pub\s+", "", sig)
    sig = re.sub(r"'[A-Za-z_][A-Za-z0-9_]*", "'_", sig)
    sig = re.sub(r"<\s*'_\s*>", "", sig)     # a row-only parameter list
    # **Parameter names are not part of the type.** A fixture calling the
    # argument `child` where `std` calls it `fiber` is not drift, and flagging
    # it would train a reader to ignore this check.
    sig = re.sub(r"\b([a-z_][\w]*)\s*:", ":", sig)
    sig = re.sub(r"\s+", " ", sig)
    return sig.strip()

def methods(text: str) -> dict:
    """`{(Type, method): normalised signature}` for every `impl` block."""
    found = {}
    impl = None
    depth = 0
    for line in text.splitlines():
        stripped = line.strip()
        m = re.match(r"impl\s*(?:<[^>]*>)?\s*(?:[A-Za-z_][\w]*\s+for\s+)?([A-Z][\w]*)", stripped)
        if m and stripped.endswith("{"):
            impl, depth = m.group(1), 1
            continue
        if impl is None:
            continue
        depth += line.count("{") - line.count("}")
        if depth <= 0:
            impl = None
            continue
        fn = re.match(r"(pub\s+)?fn\s+([a-z_][\w]*)\s*(?:<[^>]*>)?\s*\(", stripped)
        if fn:
            found[(impl, fn.group(2))] = normalise(stripped)
    return found

def defines_own(text: str, ty: str) -> bool:
    """Does this fixture declare `ty` with a body of its own?

    **A fixture that writes `pub type Map<V> = { .. }` is not talking about
    `std`'s `Map`**; it is a self-contained test type that happens to share a
    name, and comparing the two produces noise. Only a *bodyless* declaration
    -- `pub type Fiber<A, 'r>;` -- is a fixture standing in for the real item.
    """
    return bool(re.search(rf"^\s*(?:pub\s+)?type\s+{re.escape(ty)}\b[^;\n]*=", text, re.M))

std_methods = methods(pathlib.Path(std_path).read_text())

drift = []
checked = 0
for path in sorted(pathlib.Path(fixture_dir).glob("*.rs")):
    text = path.read_text()
    for (ty, name), sig in methods(text).items():
        if (ty, name) in ALLOWED:
            continue
        if defines_own(text, ty):
            continue          # the fixture's own type, not `std`'s
        want = std_methods.get((ty, name))
        if want is None:
            continue          # not a std item; the fixture owns it
        checked += 1
        if sig != want:
            drift.append((path.name, ty, name, sig, want))

if drift:
    print("  FAILED  a fixture's copy of a `std` signature has drifted", file=sys.stderr)
    for fname, ty, name, got, want in drift:
        print(f"\n          {fname}: {ty}::{name}", file=sys.stderr)
        print(f"            fixture: {got}", file=sys.stderr)
        print(f"            std:     {want}", file=sys.stderr)
    print(
        "\n  The fixture declares its own copy of this signature, so it keeps\n"
        "  compiling against the old one until the type checker refuses it --\n"
        "  as a wall of unrelated errors from the full LLVM suite. Update the\n"
        "  fixture, or add it to ALLOWED in this script with the reason.",
        file=sys.stderr,
    )
    sys.exit(1)

print(f"  ok    {checked} fixture signature(s) match `std`")
PY
