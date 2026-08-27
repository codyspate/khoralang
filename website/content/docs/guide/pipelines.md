---
title: Pipelines
sidebar:
  order: 5
---

Khora's pipeline operator `|>` keeps left-to-right data transformations readable without requiring every API to be designed around unary functions.

## Pipe into the first argument

```khora
value |> transform(a, b)
```

passes `value` as the first argument, equivalent to:

```khora
transform(value, a, b)
```

A bare function is the unary case:

```khora
value |> normalize |> validate
```

## Choose another argument with `_`

When the piped value belongs somewhere else in the call, use one `_` placeholder:

```khora
value |> insert_before(existing, _, suffix)
```

which is equivalent to:

```khora
insert_before(existing, value, suffix)
```

A pipeline stage may contain at most one placeholder.

## Fallible pipeline stages

A fallible stage uses the same `!` as an ordinary call:

```khora
raw
|> parse!
|> validate(config)!
|> persist!
```

The pipeline does not introduce another error model. `!` still marks the exact call where typed failure may leave the current function, and `catch` can still handle the result of a stage or the completed pipeline.

```khora
let user = (
  raw
  |> parse!
  |> validate!
) catch {
  ParseError::Invalid(message) => User::invalid(message),
};
```

See [Typed failure with raises](./errors-and-raises.md) for failure handling.

## Pipeline precedence

`|>` binds more loosely than arithmetic, comparisons, and ordinary calls, so stage expressions stay readable:

```khora
value + 1 |> double
```

means the result of `value + 1` is piped to `double`. Use parentheses whenever grouping would otherwise be unclear to a reader.

## The flow operator `||>`

`||>` creates a unary anonymous function whose argument becomes the starting value of the pipeline:

```khora
users |> List::map(
  ||> normalize
  |> validate!
  |> persist!
)
```

That is the same shape as naming the value yourself:

```khora
users |> List::map(fn value =>
  value
  |> normalize
  |> validate!
  |> persist!
)
```

The flow operator is useful when the lambda parameter would only be named so it can immediately enter a pipeline. Effects, failures, and captures are inferred exactly as they are for the equivalent `fn` lambda.

A named function needs no flow operator:

```khora
users |> List::map(normalize)
```

## Flow pipelines are greedy

Every following `|>` stage belongs to the flow lambda. If you want to pipe the function value created by `||>` somewhere else, parenthesize it:

```khora
(||> normalize) |> apply_twice
```

`||>` is always unary. Use `fn` when the anonymous function itself needs several parameters:

```khora
fn (left: Int, right: Int) => left + right
```

Use pipelines when they make the value moving through the expression easier to follow. Ordinary calls remain preferable when the operation itself is the main idea of the line.