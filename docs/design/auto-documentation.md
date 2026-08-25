---
title: Automatic API documentation
---

Khora's generated API documentation should come from compiler-resolved program information, not from a website source-code scraper.

## Command surface

The planned toolchain commands are:

```text
khora doc
khora doc --package
khora doc --stdlib
khora doc --format json
khora doc --check
```

`khora doc --format json` is the integration boundary for khoralang.com and other documentation consumers.

## Documentation comments

Public documentation comments use Markdown-oriented `///` comments attached to the following exported declaration.

A generated symbol record should include the fully qualified name, symbol kind, resolved signature, generic/trait constraints, capability/effect row, typed failure row, source location, documentation comment, and cross-references to other resolved symbols.

## Checked examples

Fenced Khora examples in API documentation should be compiled by `khora doc --check` when they claim to be valid executable examples. Illustrative pseudocode must be marked explicitly.

## Standard library

The website build should generate stdlib API data from the exact source revision associated with the compiler release and render it alongside curated conceptual pages.

## Third-party packages

The same documentation model should apply to packages. A future package index can therefore publish API docs from the exact immutable package revision without defining a second documentation format.

The design rule is: **the compiler generates facts; humans write explanations.**
