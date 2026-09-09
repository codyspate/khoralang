#!/bin/sh
# Two implementations of one contract, made to answer the same questions.
#
# This repository is good at implementing an abstraction and bad at proving
# that two of them agree. Seven divergences were found in a single day, and
# every one was two components that were individually correct and mutually
# inconsistent:
#
#   - the value-layout decision against the reference-counting plan, which
#     miscompiled to a SIGILL on the success path
#   - `std::permissions::granted` against `khora_manifest::granted_path`, twice
#     -- once over a `..` segment and once over a `.` one
#   - a `std` signature against its callers inside Rust test strings
#   - a security fix against the default-permissions semantics, where `**` had
#     come to mean both "a wide grant" and "no restriction"
#   - documentation against implementation, for `decode_or_stop`
#   - the reference against the compiler against the linter, on whether a trait
#     must be imported
#   - `TraitDef` against `ImplDef`, neither of which recorded trait type
#     parameters
#
# **None was found by a test.** Every one was found by making the two sides
# meet by hand, which is not a thing that happens on a schedule. So the pairs
# below are made to meet on every run.
#
# The rule each of these tests enforces is not "this function is correct". It
# is "these two say the same thing", which is a weaker claim and the one that
# actually broke.
#
#     sh scripts/check-agreement.sh
#
# Run from `scripts/baseline.sh`, and standalone while working on either side
# of a pair.
set -eu

cd "$(dirname "$0")/.."

# Pair 1: the permission matchers.
#
# `std/permissions.kh` answers "may this program touch this" for a running
# program; `khora-manifest` answers it for the compiler. `std/permissions.kh`'s
# own doc comment says of the two that "the two have to agree", and they have
# not, twice.
#
# The cases live in `crates/khora-manifest/tests/agreement/permission_cases.rs`
# and both tests are driven from that one list -- the Rust test calls
# `khora_manifest` directly, and the Khora test compiles a program generated
# from the same list, runs it, and compares the transcript to what the Rust
# side said. **A case cannot be added to one side only**, which is the whole
# mechanism: the two matchers drifted apart in coverage before they drifted
# apart in behaviour.
echo '== the permission matchers'
cargo test -p khora-manifest --test permissions_agree
cargo test -p khora-codegen-llvm --features llvm --test suite -- agreement::

# Pair 2: the key an impl's method is filed under.
#
# `khora_hir::body::impl_key` names the body and `khora_types::traits::method_key`
# names the signature, in two crates, from one piece of syntax. The trait half
# is shared code; the type half is not -- lowering reads the head off the
# syntax and the checker takes the head of a built `Type`. A body filed under a
# key no signature matches is a method the checker cannot see; a signature with
# no body is a call that type-checks and links to nothing.
echo '== the impl-method keys'
cargo test -p khora-types --test keys_agree

# Adding a pair
# ------------
# Write one test that runs both implementations over one shared list of cases,
# and add it above with a comment saying which two things are being held
# together and what went wrong when they were not. The list is the part that
# must not be duplicated: if the two sides have separate case lists, the gate
# checks that each side agrees with itself. Where the two implementations live
# in different crates, `#[path]`-include the list from one of them --
# `crates/khora-codegen-llvm/tests/agreement.rs` does exactly that, across the
# workspace, so the build fails rather than the coverage silently halving.
echo '   ok  every pair agrees'
