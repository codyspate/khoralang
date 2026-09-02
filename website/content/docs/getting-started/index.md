---
title: Getting Started
sidebar:
  order: 0
---

You can go from an empty directory to a checked, tested, native Khora executable in a few minutes.

## 1. Install Khora

On macOS or Linux:

```bash
curl -fsSL https://raw.githubusercontent.com/codyspate/khoralang/main/install.sh | sh
```

On Windows, in PowerShell:

```powershell
irm https://raw.githubusercontent.com/codyspate/khoralang/main/install.ps1 | iex
```

Verify the toolchain is available:

```bash
khora --version
```

If the installer asks for a system linker, follow the platform-specific instructions in [Installation](/docs/getting-started/installation/).

## 2. Create a package

```bash
khora new hello_khora
cd hello_khora
```

That writes the whole package:

```text
hello_khora/
├── khora.toml
└── src/
    └── main.kh
```

`khora.toml` names the package:

```toml
[package]
name = "hello_khora"
version = "0.1.0"
```

and `src/main.kh` is a program that compiles:

```khora
module hello_khora::main;

pub fn main() -> Int {
  0
}
```

A package name is a module path segment, so it is letters, digits and
underscores — `hello_khora`, not `hello-khora`. `khora new` says so rather than
creating something that will not compile.

Add `khora new --lib` for a library, which writes `src/lib.kh` and no `main`.

Make it print something:

```khora
module hello_khora::main;

import std::core::{print};

pub fn main() -> Int {
  print("Hello, Khora!");
  0
}
```

## 3. Run it

```bash
khora run .
```

```text
Hello, Khora!
```

`khora run` compiles and starts the program, and gives you its exit status as
its own — so it behaves in a script the way running the executable would. The
build is cached, so the second run starts immediately.

To get an executable you can hand to somebody:

```bash
khora check .
khora build .
./build/hello_khora
```

A build writes into the package's own `build/` directory, named after the
package -- `build/hello_khora` here, and `build\hello_khora.exe` on Windows,
where the same two commands run it as `.\build\hello_khora.exe`. Nothing
lands among your sources, and `build/` is the one directory a package does not
track.

`--out` overrides the path, and adds the platform's extension if you did not
write one.

`khora check` is the fast feedback command: it parses and resolves your package, checks types and exhaustiveness, and reports diagnostics without producing an executable. `khora build` continues through native code generation and linking.

## 4. Format and test as you work

Khora ships the formatter and test runner in the same toolchain:

```bash
khora fmt .
khora test .
```

`khora fmt . --check` writes nothing and fails if anything is unformatted,
which is the form for CI.

## 5. Depend on another package

A package that needs another names it under `[dependencies]`, by path:

```toml
[package]
name = "hello_khora"
version = "0.1.0"

[dependencies]
postgres = { path = "../packages/postgres" }
```

The name on the left is what your `import` lines say — `import
postgres::pool::{Pool}` — and it need not match the directory. A dependency's
own dependencies come with it.

Continue with [Your first Khora project](/docs/getting-started/first-project/) to add a function and a test and learn the everyday check-format-test-build loop.

## 6. Set up your editor

Khora's language server is part of the compiler toolchain. Your editor starts `khora lsp` for you and gets compiler-backed diagnostics, navigation, formatting, and symbol information.

See [Editor setup](/docs/getting-started/editor/) for configuration and the compiler-backed MCP integration for coding agents.

## Where to go next

- [Installation](/docs/getting-started/installation/) — toolchains, version pinning, source builds, and optional release candidates.
- [Your first Khora project](/docs/getting-started/first-project/) — work through the normal package workflow.
- [Language Reference](/docs/reference/) — every construct, one page per topic. Its first section is a reading order, from values and functions through effects and structured concurrency.
- [Standard Library](/docs/stdlib/) — find the APIs available once you start building real programs.
