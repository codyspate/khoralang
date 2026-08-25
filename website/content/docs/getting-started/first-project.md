---
title: Your first Khora project
sidebar:
  order: 2
---

A Khora package has a `khora.toml` manifest and Khora source under `src/`.

The examples in the repository are the best current templates while the project-creation command is still being finalized.

## Build an existing example

From the compiler repository:

```bash
cargo run -p khora-cli --features llvm -- build examples/core_demo
```

That command takes the package through parsing, name resolution, type inference, exhaustiveness checking, monomorphization, reference-count planning, LLVM code generation, and linking.

## Check before building

Use `khora check` for the fast feedback loop:

```bash
cargo run -p khora-cli -- check examples/core_demo
```

Checking does not require native code generation and is the command editors and CI should use for ordinary diagnostics.

## Format

Khora has a canonical formatter:

```bash
cargo run -p khora-cli -- fmt examples/core_demo
```

Use `--check` in CI when you want formatting differences to fail the build.

## Tests

Khora packages can be tested through the CLI:

```bash
cargo run -p khora-cli -- test examples/core_demo
```

## What to learn next

Start with the Language Guide rather than the compiler design documents. The Guide explains the programmer-facing model: values and functions, algebraic data types, pattern matching, pipelines, typed failure, effects and capabilities, structured concurrency, and resource cleanup.
