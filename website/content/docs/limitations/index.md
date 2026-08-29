---
title: Known limitations
sidebar:
  order: 0
---

Khora is pre-1.0. This page exists so users can tell the difference between a language rule, a supported feature, and unfinished work.

## Toolchain distribution

Khora has versioned toolchain artifacts and installers for the platforms released by the project. The normal application-developer path is the installer documented in [Installation](/docs/getting-started/installation/), not compiling the compiler from source.

The remaining limitation is **target coverage**, not the absence of distribution. A target is only labeled supported when the compiler, runtime, linker/sysroot, packaging, CI, and deployment/conformance path work end to end. See [Supported targets](/docs/deployment/supported-targets/) for that distinction.

## Recursion depth and very large lists

Khora does not guarantee tail-call optimisation, so a function that recurses once per element uses one stack frame per element. Running out of stack ends the program; it reports

```
khora: the stack ran out
```

on standard error and exits with the platform's stack-overflow status.

Every traversal in `std::core`'s `List` is written as a loop rather than as recursion — `length`, `fold`, `reverse`, `filter`, `take`, `drop`, `any`, `all`, `find`, `contains`, `zip`, `flat_map`, `sum`, and the `merge` inside `sort` — so walking a list of any size is safe. `List::sort` recurses only to divide, which is about `log2(n)` deep.

Releasing a value costs no stack either: reference counting frees a value's children through a queue rather than by recursing, so letting go of a long list is a loop like walking one. A million-element `List` sorts.

What is left is ordinary recursion that somebody writes. A function that calls itself once per element of its input will use a frame per element, and no analysis in the compiler turns that into a loop.

`Array<A>` and `Vector<A>` remain the better shape for a large indexed collection — a list is for building front-to-back and walking once — but the choice is now about cost rather than about a cliff.

## Package ecosystem

Dependencies can be pinned reproducibly to git revisions, but there is not yet a public package registry or broad third-party ecosystem.

## Editor tooling

`khora lsp` already provides compiler-backed diagnostics, hover, formatting, completion, signature help, go-to-definition, references, document/workspace symbols, semantic tokens, code actions, code lenses, and inlay hints.

Rename is intentionally narrower than the rest of the navigation surface: it currently renames locals only and refuses edits it cannot prove complete rather than applying a partial rename. Broader symbol rename and additional refactoring operations remain editor-tooling work.

See [Editor setup](/docs/getting-started/editor/) for the language-server command and client setup.

## Standard-library API docs

`khora doc` generates the checked-in standard-library API reference from compiler-resolved declarations plus `///` and `//!` documentation comments. `khora doc --check` is used to detect drift between the source declarations and generated pages.

Two important documentation-tooling gaps remain:

- Khora code blocks in API documentation are not yet compiled as documentation tests.
- Generated signatures name referenced types but do not yet cross-link those type names to their API pages.

See the [Standard library](/docs/stdlib/) entry point for the generated reference.

## HTTP surface

The reference HTTP implementation is intentionally not presented as every protocol feature a mature web framework might provide. The shipping documentation should be treated as the supported surface; unlisted body encodings, upgrades, protocol versions, or framework conveniences should not be assumed merely because the core server/client path exists.

## Concurrency combinators

A fiber carries its answer and its failure row — `Fiber<A, 'er>`, with `join` re-raising what the child raised — and `Clock` can `sleep`. The combinators built on top of those do not exist yet: there is no `timeout`, no `race`, and no bounded parallel map. Write them by hand out of `Fiber`, `Channel` and a nursery in the meantime.

`Channel` also has no `select` (waiting on the first of several) and no zero-capacity rendezvous. `Channel::bounded(0)` gets a capacity of one rather than a rendezvous, deliberately.

## Cross-compilation and WebAssembly

LLVM object/module emission is further along than the complete runtime/link/sysroot/deployment path for every target. Only targets tested end to end are labeled supported.

WebAssembly also requires a host-appropriate standard-library/platform surface rather than reusing native filesystem and socket assumptions. Cloudflare Workers remains an experimental/planned deployment path rather than a supported production target.

## Stability

Khora has not reached 1.0. Source compatibility across arbitrary development revisions is not promised. Pin the toolchain version for applications where reproducible builds matter, and review migration notes when deliberately moving between incompatible releases.

## Reporting a limitation

If the documentation says something should work and the compiler disagrees, treat that as a bug in either the implementation or the docs.

Generated API declarations are checked for drift, but not every prose example in the Guide, Reference, Cookbook, or generated doc comments is a compiler-run test yet. When an example and the compiler disagree, the compiler behavior and current language implementation need to be reconciled rather than assuming either side is automatically correct.
