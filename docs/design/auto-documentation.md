---
title: Automatic API documentation
---

Khora's generated API documentation should come from compiler-resolved program information, not from a website source-code scraper.

> **Partly built.** `khora doc` exists and generates the standard library
> reference; `docs/design/documentation.md` records what was decided and why
> while building it, and this page stays as the plan for the rest. The two
> disagree in one place, noted under *Command surface* below.

## What exists today

`khora doc [paths] --out <dir>` writes one markdown page per module, and
`--check` writes nothing and fails when the checked-in pages no longer match
the source. It runs in `scripts/baseline.sh` over `std`, so the published
reference cannot drift from the code it describes.

Documentation comments are `///` for a declaration and `//!` for a module —
the second was added for this, because seventeen of eighteen `std` files
opened with a `//` block that nothing could tell from a note to a maintainer.

Read from the syntax tree rather than the HIR, because the HIR does not
contain the API: `collect_decl` returns early on `Decl::Impl`, so every method
in `std` is absent from `item_map`.

## Command surface

The planned toolchain commands are:

```text
khora doc
khora doc --package
khora doc --stdlib
khora doc --format json
khora doc --check
```

`khora doc --format json` is the integration boundary for khoralang.com and other documentation consumers. Not built: today the markdown pages are the boundary, which is enough while the only consumer is this site and will stop being enough as soon as there is a second.

`--package` and `--stdlib` are also not built, and may not need to be: `khora doc <path>` already documents whatever it is pointed at, so they would be shorthands rather than capabilities.

**One correction.** `--check` is built, and it means *the checked-in pages match the source* rather than *the examples compile*. Both are worth having and they are different questions asked at different times — one is a gate on a generated tree, the other is a test. Compiling examples belongs to `khora test`, which is already the command that compiles and runs `test` blocks; making a documentation flag also run code would put two unrelated failures behind one exit status.

## Documentation comments

Public documentation comments use Markdown-oriented `///` comments attached to the following exported declaration.

A generated symbol record should include the fully qualified name, symbol kind, resolved signature, generic/trait constraints, capability/effect row, typed failure row, source location, documentation comment, and cross-references to other resolved symbols.

## Checked examples

Fenced Khora examples in API documentation should be compiled when they claim to be valid executable examples, and illustrative pseudocode must be marked explicitly. **Not built**, and it is the most important thing on this page: an example nothing compiles is the one part of a generated reference that can still be wrong. See the correction under *Command surface* for where it should live.

## Standard library

The website build should generate stdlib API data from the exact source revision associated with the compiler release and render it alongside curated conceptual pages.

## Third-party packages

The same documentation model should apply to packages. A future package index can therefore publish API docs from the exact immutable package revision without defining a second documentation format.

The design rule is: **the compiler generates facts; humans write explanations.**
