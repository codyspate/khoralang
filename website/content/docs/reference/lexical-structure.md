---
title: Lexical structure
sidebar:
  order: 4
---

Khora source is UTF-8 text. Whitespace and comments separate tokens where needed; the parser retains trivia in its lossless syntax tree even though trivia does not change ordinary program semantics.

Identifiers name bindings, types, modules, traits, variants, and associated items according to their syntactic position. Compile-time path segments are joined with `::`.

Numeric literals include integers, IEEE floating-point literals, and exact decimal literals with the `d` suffix. A bare fractional literal such as `0.01` is a `Float`; `0.01d` is a `Decimal`.

String literals represent Unicode text. String interpolation is supported for human-readable composition.

Comments are retained by the syntax tree so tools such as the formatter and future documentation generator can preserve or interpret them. Public API documentation comments use the planned `///` Markdown form.

The exact token grammar is maintained by the parser and `docs/grammar.ebnf`; this public reference should be updated whenever user-visible lexical syntax changes.
