---
title: Installation
sidebar:
  order: 1
---

Khora is pre-release. Until signed compiler binaries are published, the supported development installation is a source build from the Khora repository.

## Requirements

- Git
- a current Rust toolchain
- LLVM 22.1.8 for native code generation

The parser, type checker, formatter, and most compiler tests do not need LLVM. Building Khora programs into native executables does.

## Build the compiler

```bash
git clone https://github.com/codyspate/khoralang.git
cd khoralang
sh scripts/setup-llvm.sh
cargo build -p khora-cli --features llvm
cargo build -p khora-rt
```

The LLVM setup script installs or locates the pinned LLVM version and writes the machine-specific Cargo configuration used by the backend.

## Verify the toolchain

```bash
cargo test --workspace --features llvm
cargo run -p khora-cli --features llvm -- check std examples
```

For repository development, `sh scripts/baseline.sh` is the full gate.

## Running `khora`

During source development you can invoke the CLI through Cargo:

```bash
cargo run -p khora-cli --features llvm -- --help
```

You can also register a locally built compiler with Khora's toolchain shim once the executable is on disk.

## Release installations

The public release will provide versioned compiler artifacts so ordinary users do not need Rust or a compiler checkout. Released projects will be able to pin the compiler version they expect in `khora.toml`.

Until those artifacts exist, this page intentionally describes the source-build path rather than pretending an installer exists.
