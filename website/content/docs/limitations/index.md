---
title: Known limitations
sidebar:
  order: 0
---

Khora is pre-1.0. This page exists so users can tell the difference between a language rule, a supported feature, and unfinished work.

## Toolchain distribution

Khora has versioned toolchain artifacts and installers for the platforms released by the project. The normal application-developer path is the installer documented in [Installation](/docs/getting-started/installation/), not compiling the compiler from source.

The remaining limitation is **target coverage**, not the absence of distribution. A target is only labeled supported when the compiler, runtime, linker/sysroot, packaging, CI, and deployment/conformance path work end to end. See [Supported targets](/docs/deployment/supported-targets/) for that distinction.

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

## Cross-compilation and WebAssembly

LLVM object/module emission is further along than the complete runtime/link/sysroot/deployment path for every target. Only targets tested end to end are labeled supported.

WebAssembly also requires a host-appropriate standard-library/platform surface rather than reusing native filesystem and socket assumptions. Cloudflare Workers remains an experimental/planned deployment path rather than a supported production target.

## Stability

Khora has not reached 1.0. Source compatibility across arbitrary development revisions is not promised. Pin the toolchain version for applications where reproducible builds matter, and review migration notes when deliberately moving between incompatible releases.

## Reporting a limitation

If the documentation says something should work and the compiler disagrees, treat that as a bug in either the implementation or the docs.

Generated API declarations are checked for drift, but not every prose example in the Guide, Reference, Cookbook, or generated doc comments is a compiler-run test yet. When an example and the compiler disagree, the compiler behavior and current language implementation need to be reconciled rather than assuming either side is automatically correct.
