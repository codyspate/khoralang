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

## The flow operator

`||>` creates a unary anonymous function whose argument becomes the starting value of the pipeline.

```khora
users |> List::map(
  ||> normalize
  |> validate!
  |> persist!
)
```

That is exactly the same program as naming the value yourself:

```khora
users |> List::map(fn value =>
  value
  |> normalize
  |> validate!
  |> persist!
)
```

The flow operator is the shorter way to write it when the name would be read once and mean nothing. It is sugar and nothing more: effects, failures and captures are inferred exactly as they are for the lambda it stands for, and `!` behaves the same in both.

**A named function needs no flow operator.** Pass it directly:

```khora
List::map(normalize)
```

Two rules are worth knowing.

The flow operator is **greedy** — every `|>` after the first stage belongs to it. To pipe the function it makes somewhere else, use parentheses:

```khora
(||> normalize) |> apply_twice
```

And it is always **unary**. A pipeline is one value moving through stages, so a function of two arguments still wants `fn`.
