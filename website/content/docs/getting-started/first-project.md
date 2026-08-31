---
title: Your first Khora project
sidebar:
  order: 2
---

A Khora package has a `khora.toml` manifest and source files under `src/`. This walkthrough adds a small function and a test, then formats, checks, tests, builds, and runs the package with the installed `khora` toolchain.

If `khora --version` does not work yet, start with [Installation](/docs/getting-started/installation/).

## Package structure

Create a directory like this:

```text
hello_khora/
├── khora.toml
└── src/
    └── main.kh
```

The manifest names the package and selects the language edition:

```toml
[package]
name = "hello_khora"
version = "0.1.0"
edition = "2026"
```

## Write some Khora

Put this in `src/main.kh`:

```khora
module main;

import std::core::{assert, print};

fn double(value: Int) -> Int {
  value * 2
}

test "double returns twice its input" {
  assert(double(21) == 42);
}

fn main() -> Int {
  print("Hello, Khora!");
  0
}
```

There are three ordinary language ideas here:

- `fn` declares a function. The parameter and return types are part of its public shape.
- `test` declares a test that the package test runner can execute.
- `import std::core::{...}` brings standard-library names into the module.

The [Language Guide](/docs/guide/) goes deeper on each of these after you have the workflow running.

## Check before building

From the package root:

```bash
khora check .
```

Use `check` constantly while you work. It validates the package without paying the cost of native code generation and linking.

## Format

Khora has one canonical formatter:

```bash
khora fmt .
```

In CI, check formatting without changing files:

```bash
khora fmt . --check
```

See [Values and functions](/docs/guide/values-and-functions/) for the core expression and function model.

## Run the tests

```bash
khora test .
```

The test runner discovers `test` blocks in the package and reports failures through the CLI. For testing patterns around capabilities, typed failure, and cancellation, see [Testing](/docs/guide/testing/).

## Build a native executable

```bash
khora build .
```

The program goes in `build/`, named after the package, so nothing a build makes
lands among your sources. `khora new` writes a `.gitignore` with `build/` in it
for exactly that reason.

Run it on macOS or Linux:

```bash
./build/hello_khora
```

Or on Windows:

```powershell
.\build\hello_khora.exe
```

`--out` puts it somewhere else, under whatever name you give it:

```bash
khora build . --out dist/hello
```

You should see:

```text
Hello, Khora!
```

For an optimized build, add `--release`:

```bash
khora build . --release
```

## The everyday loop

For most projects, these are the commands you will use repeatedly:

```bash
khora fmt .
khora check .
khora test .
khora build .
```

The language server uses the same compiler queries as `khora check`, so editor diagnostics and command-line diagnostics stay aligned. Continue with [Editor setup](/docs/getting-started/editor/) when you want that feedback in your editor.

## What to learn next

A useful path from here is:

1. [Values and functions](/docs/guide/values-and-functions/)
2. [Data types](/docs/guide/data-types/)
3. [Pattern matching](/docs/guide/pattern-matching/)
4. [Pipelines](/docs/guide/pipelines/)
5. [Errors and raises](/docs/guide/errors-and-raises/)
6. [Effects and capabilities](/docs/guide/effects-and-capabilities/)

For exact declarations while you work, browse the [Standard Library](/docs/stdlib/) reference.
