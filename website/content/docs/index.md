---
title: Khora documentation
description: Learn the Khora language, standard library, toolchain, deployment model, and production patterns.
sidebar:
  order: 0
---

## Where Khora actually is

**Khora is `0.x` and pre-1.0, with one maintainer.** The released line is
`0.1`; these pages document the `0.2` development series. What that means in
practice, stated here so nobody has to discover it:

- The language and `std` can break between releases. Every breaking change is
  in the changelog under a **Breaking** heading first, and pinning
  `[toolchain]` makes a build reproducible — but there is no source-compatibility
  promise across releases yet. [Compatibility and
  stability](/docs/reference/compatibility/) is the policy, including the four
  things 1.0 is waiting for.
- Recent bug-hunting sessions still turn up **silent-wrongness bugs**, most
  recently in trait dispatch and structured concurrency. That is the specific
  reason 1.0 has not been called.
- There is **no public package registry** yet; dependencies are paths and
  pinned git revisions.
- Structured concurrency is the least finished part of the language. There is
  no `timeout`, no `race` and no `select`, a bounded nursery admits one more
  child than its limit, and a child's failure does not reliably cancel its
  siblings. [Known limitations](/docs/limitations/) has the measurements.
- Nobody who did not write it has shipped with it. If you are evaluating Khora
  for something load-bearing, read [Known limitations](/docs/limitations/) and
  [Performance](/docs/performance/) before anything else on this site; both are
  written to be falsifiable, not persuasive.

## Why this rather than Rust, OCaml or Gleam

Khora's bet is that three things belong in the type system together — what a
function can *fail* with, what external *authority* it needs, and how its
concurrent children are *scoped* — and that a native language can have all
three without a borrow checker and without a tracing garbage collector. That
combination is the argument; every piece of it exists somewhere else already.

**Against Rust.** Khora has no lifetimes, no borrow checker and no source-level
ownership syntax. Memory is reference-counted, with the compiler eliding
retain/release pairs and reusing uniquely-owned storage where it cannot change
what the program means; [Memory and
resources](/docs/reference/memory-and-resources/) says exactly what is
promised. Failures are a `raises` row the compiler propagates rather than a
`Result` you thread by hand, and capabilities are a `with` row, not a
convention. What Rust has that Khora does not: a stable language, a decade of
libraries, a registry, and predictable performance without a reference-count
cost. Khora's HTTP server answers about 174,000 requests a second in 8.4 MB —
mid-table on rate and first on memory in [the
comparison](/docs/performance/) — which is a real number but not a Rust-beating
one.

**Against OCaml.** OCaml 5 also has effect handlers, a fast native compiler and
a far more mature implementation. The differences are that Khora's rows carry
failure *and* capability *and* are inferred through call chains, that memory is
reference-counted rather than traced (which is where the 8.4 MB comes from),
and that concurrency is structured in the language itself.
OCaml wins on maturity, on tooling that has been used in anger for thirty
years, and on a real ecosystem.

**Against Gleam.** Gleam is a friendlier and much more finished language, and
it inherits the BEAM: OTP, supervision trees, and a preemptive scheduler, none
of which Khora's nurseries match today (see the nursery measurements in [Known
limitations](/docs/limitations/)). Khora compiles to native code through LLVM
with no VM underneath, which is the case for it where a runtime is not
available or a few megabytes of RSS is the constraint, and it has effects and
capabilities, which Gleam does not.

If none of those trades is one you want to make, the honest answer is to use
the mature thing. Khora is worth your time if failure rows, capabilities and
structured concurrency in one native language is a combination you cannot get
elsewhere, and you can tolerate a pre-1.0 implementation while it is proved.

## Start here

- **[Getting started](/docs/getting-started/)** — install Khora, create a project, build it, run it, and test it.
- **[Language reference](/docs/reference/)** — every construct in one place: values, functions, algebraic data types, pattern matching, pipelines, generics, traits, effects, capabilities, resources, and fibers. It opens with a reading order for a first pass, and is the lookup-oriented page for an exact answer.
- **[Standard library](/docs/stdlib/)** — curated overview plus generated API reference kept in sync with the source by `khora doc`.

## Build real applications

- **[Cookbook](/docs/cookbook/)** — worked patterns for HTTP services, database access, tracing, cancellation, bounded concurrency, and testing.
- **[Deployment](/docs/deployment/)** — supported targets and how Khora applications are built and deployed.
- **[Migration guides](/docs/migration/)** — mental-model bridges for developers coming from Effect TypeScript, Go, and Rust.
- **[Performance](/docs/performance/)** — what the HTTP server answers, measured against a load generator that is not the bottleneck, with the conditions each figure had to satisfy and an account of why every number published before September 2026 was too high.
- **[Limitations](/docs/limitations/)** — functionality that is intentionally incomplete, unsupported, or still evolving, with the measurements behind each entry.

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
