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

Create this directory structure:

```text
hello-khora/
├── khora.toml
└── src/
    └── main.kh
```

Add `khora.toml`:

```toml
[package]
name = "hello-khora"
version = "0.1.0"
edition = "2026"
```

Then add `src/main.kh`:

```khora
module main;

import std::core::{print};

fn main() -> Int {
  print("Hello, Khora!");
  0
}
```

## 3. Check and run it

From the package root:

```bash
khora check .
khora build . --out hello
./hello
```

On Windows:

```powershell
khora check .
khora build . --out hello
.\hello.exe
```

You should see:

```text
Hello, Khora!
```

`khora check` is the fast feedback command: it parses and resolves your package, checks types and exhaustiveness, and reports diagnostics without producing an executable. `khora build` continues through native code generation and linking.

## 4. Format and test as you work

Khora ships the formatter and test runner in the same toolchain:

```bash
khora fmt .
khora test .
```

Continue with [Your first Khora project](/docs/getting-started/first-project/) to add a function and a test and learn the everyday check-format-test-build loop.

## 5. Set up your editor

Khora's language server is part of the compiler toolchain. Your editor starts `khora lsp` for you and gets compiler-backed diagnostics, navigation, formatting, and symbol information.

See [Editor setup](/docs/getting-started/editor/) for configuration and the compiler-backed MCP integration for coding agents.

## Where to go next

- [Installation](/docs/getting-started/installation/) — toolchains, version pinning, source builds, and optional release candidates.
- [Your first Khora project](/docs/getting-started/first-project/) — work through the normal package workflow.
- [Language Guide](/docs/guide/) — learn Khora in a deliberate order, from values and functions through effects and structured concurrency.
- [Standard Library](/docs/stdlib/) — find the APIs available once you start building real programs.
