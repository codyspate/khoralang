---
title: Grammar and precedence
sidebar:
  order: 1
---

The authoritative implemented grammar currently lives in the compiler repository at `docs/grammar.ebnf`. This public page records the user-visible rules that most often affect how an expression is parsed.

## Paths and fields

`::` is used for compile-time paths such as modules, types, constructors, and associated items. `.` is used for runtime field projection.

## Blocks and records

A `{ ... }` form is parsed as a record literal when its opening tokens identify record fields; otherwise it is a block. In a `match` scrutinee context, the following braces belong to the arm list.

## Operator precedence

From loosest to tightest:

1. `=` — right associative
2. `|>`
3. `||`
4. `&&`
5. comparisons
6. `+ -`
7. `* / %`
8. prefix `- !`
9. call and field access

Assignment's low precedence means `x = a |> b` assigns the result of the whole pipeline.

## Pipelines

`x |> f(a)` parses as a pipeline stage that calls `f(x, a)`. A stage may contain one `_` placeholder to select another position, such as `x |> f(a, _, b)`.

## Decimal literals

Exact decimal literals use a `d` suffix, such as `0.01d`. Bare `0.01` remains an IEEE `Float` literal.

Before the first public release, this reference will be checked against or generated from the implemented grammar so the public syntax reference cannot drift from the parser.
