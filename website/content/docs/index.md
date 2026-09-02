---
title: Khora documentation
description: Learn the Khora language, standard library, toolchain, deployment model, and production patterns.
sidebar:
  order: 0
---

Welcome to the Khora documentation. This is the entry point for everything you need to install the toolchain, learn the language, build applications, understand exact language behavior, and use the standard library.

## Start here

- **[Getting started](/docs/getting-started/)** — install Khora, create a project, build it, run it, and test it.
- **[Language Reference](/docs/reference/)** — every construct in one place: values, functions, algebraic data types, pattern matching, pipelines, generics, traits, effects, capabilities, resources, and fibers. It opens with a reading order for a first pass.
- **[Language reference](/docs/reference/)** — precise syntax and semantic rules when you need an exact answer.
- **[Standard library](/docs/stdlib/)** — curated overview plus generated API reference kept in sync with the source by `khora doc`.

## Build real applications

- **[Cookbook](/docs/cookbook/)** — production-oriented patterns for HTTP services, database access, tracing, cancellation, bounded concurrency, and testing.
- **[Deployment](/docs/deployment/)** — supported targets and how Khora applications are built and deployed.
- **[Migration guides](/docs/migration/)** — mental-model bridges for developers coming from Effect TypeScript, Go, and Rust.
- **[Limitations](/docs/limitations/)** — functionality that is intentionally incomplete, unsupported, or still evolving.

## Tooling

The Khora toolchain includes compiler-backed commands for checking, testing, formatting, generating API documentation, editor integration, and coding-agent integration.

```text
khora check
khora test
khora fmt
khora doc
khora lsp
khora mcp
```

If you are new to Khora, start with **Getting started**, then read the **Language reference** in the order its first section gives. Use the **Standard library** for what ships with the toolchain, and the **Cookbook** for a whole task working end to end.
