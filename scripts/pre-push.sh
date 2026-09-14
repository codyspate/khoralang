#!/bin/sh
# Every gate CI runs, in one command, before pushing.
#
# **Copied from `.github/workflows/ci.yml`, not composed from memory.** The
# feature flags are part of the command: `cargo nextest run --workspace` builds
# `khora` *without* the LLVM backend, and a `khora` without a backend refuses
# every `khora build`. Running the suite with `--features llvm` out of habit
# tests a different binary than CI does, which is how three consecutive pushes
# went red on a test that passed locally every time.
#
# Keep this file in step with the workflow. If a step is added there and not
# here, this script's green stops meaning anything.
set -eu

cd "$(dirname "$0")/.."

echo "== nextest (no --features: this is what CI runs) =="
cargo nextest run --workspace

echo
echo "== clippy (no --features, -D warnings) =="
cargo clippy --workspace --all-targets -- -D warnings

echo
echo "== docs gates =="
sh scripts/check-docs.sh
sh scripts/check-corrections.sh
sh scripts/check-api-programs.sh

echo
echo "ok  every gate CI runs passed here first"
