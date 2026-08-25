---
title: Pipelines
sidebar:
  order: 4
---

Khora's pipeline operator `|>` keeps left-to-right data transformations readable without requiring every API to be designed around unary functions.

```khora
value |> transform(a, b)
```

passes `value` as the first argument, equivalent to:

```khora
transform(value, a, b)
```

When the value belongs somewhere else in the call, use a single `_` placeholder:

```khora
value |> insert_before(existing, _, suffix)
```

A stage may contain at most one placeholder. A bare function works as expected:

```khora
value |> normalize |> validate
```

Pipelines have deliberately low precedence so a multi-step transformation reads as one expression rather than a nest of calls.

Fallible stages can use the ordinary `!` propagation syntax at the stage where failure occurs. The pipeline does not introduce a second error model; it is only call syntax arranged around the value flowing through the expression.

Use pipelines when they make the data flow clearer. Ordinary calls remain preferable when the operation itself, rather than the transformed value, is the main idea of the line.
