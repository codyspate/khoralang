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

The Khora toolchain is one binary. These are the commands a project uses day to day; `khora --help` lists all of them.

```text
khora new      start a package
khora build    compile it
khora run      compile and run it
khora check    parse, type check and lint, without building
khora test     run its tests
khora bench    run its benchmarks
khora fmt      format its source
khora doc      generate API pages from its `///` comments
khora std      search the standard library from the terminal
khora lsp      the language server, for editors
khora mcp      the same knowledge, for coding agents
```

`khora std search <query>` is the fastest way to find out whether something exists. It reads the compiler's own view of the `std` beside it — signatures sliced from the declarations, descriptions taken from their `///` comments — so it is never out of step with the toolchain you have, which is more than these pages can promise.

There is no `khora lint`: the lints run inside `khora check`, because a separate command is a second thing to run and a second answer to disagree with the first. `khora sbom`, `khora toolchain`, `khora update`, `khora cache`, `khora why`, `khora graph` and `khora release` cover distribution and diagnosis.

If you are new to Khora, start with **Getting started**, then read the **Language reference** in the order its first section gives. Use the **Standard library** for what ships with the toolchain, and the **Cookbook** for a whole task working end to end.
