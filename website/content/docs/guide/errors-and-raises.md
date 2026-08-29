---
title: Typed failure with raises
sidebar:
  order: 6
---

Khora uses typed failure for recoverable conditions. A function declares the failures that can leave it with `raises`, creates a failure with `raise`, propagates a fallible call with `!`, and handles failures with `catch`.

The four pieces fit together like this:

```text
raise E                    create a typed failure
foo()!                     allow E to propagate from foo
foo()! catch { ... }       handle E here
attempt(fn () => foo()!)   turn E into Result<A, E>
```

## Declare and raise a failure

Failure types are ordinary algebraic data types. Give variants enough information for a caller to make a useful decision.

```khora
pub type RandomFailure =
  | BelowThreshold(value: Int);
```

A function includes the type in its `raises` row and uses `raise` where normal control flow should stop:

```khora
fn determine_random() -> Bool
  with { random: Random }
  raises RandomFailure
{
  let value = random.in_range(0, 100);

  if value >= 50 {
    true
  } else {
    raise RandomFailure::BelowThreshold(value)
  }
}
```

`raise` is an expression of type `Never`: that path does not produce the function's normal return value. It leaves through the typed failure channel instead.

## Propagate with `!`

A caller that is not ready to handle the failure can propagate it. The `!` marks the call site where control may leave the current function:

```khora
fn decide() -> Bool
  with { random: Random }
  raises RandomFailure
{
  determine_random()!
}
```

Because `decide` propagates `RandomFailure`, its own signature must include that failure.

## Handle a failure with `catch`

`catch` handles a failure before it reaches the caller. Its arms use the same pattern syntax as `match`:

```khora
fn decide_or_false() -> Bool
  with { random: Random }
{
  determine_random()! catch {
    RandomFailure::BelowThreshold(value) => false,
  }
}
```

The normal path still produces the `Bool` returned by `determine_random`. If `RandomFailure::BelowThreshold` is raised, the matching arm produces the replacement `Bool` instead.

After the `catch`, `RandomFailure` is no longer part of this function's failure row. The failure has been consumed.

### Catch arms are typed patterns

A `catch` arm may destructure the failure just like a `match` arm:

```khora
let allowed = determine_random()! catch {
  RandomFailure::BelowThreshold(value) => {
    print("random value was ${value}");
    false
  },
};
```

Handling a failure type commits to handling that type. If `RandomFailure` had several variants, the `catch` arms for `RandomFailure` would need to cover them exhaustively. There is no wildcard arm that silently discards an unknown open failure row.

## Translate one failure into another

A layer often should not expose failures from the layer below it. Catch the lower-level failure and `raise` a failure in the vocabulary of the current API:

```khora
pub type ApiError =
  | ServiceUnavailable(message: String)
  | Internal(message: String);

fn determine_for_api() -> Bool
  with { random: Random }
  raises ApiError
{
  determine_random()! catch {
    RandomFailure::BelowThreshold(value) =>
      raise ApiError::ServiceUnavailable(
        "random value ${value} was below the threshold"
      ),
  }
}
```

`RandomFailure` does not escape `determine_for_api`; callers only need to know about `ApiError`.

This is the usual boundary pattern:

```text
infrastructure/domain failure
          ↓ catch + raise
application/API failure
          ↓ catch
response or other boundary value
```

## Turn a failure into an API response

At the outer HTTP boundary, the failure usually stops being a failure and becomes a normal `Response`. A `catch` arm can `return` a response while the success path continues normally:

```khora
fn handle_request() -> Response
  with { random: Random }
{
  let allowed = determine_for_api()! catch {
    ApiError::ServiceUnavailable(message) =>
      return Response::text(503, message),

    ApiError::Internal(message) =>
      return Response::text(500, message),
  };

  if allowed {
    Response::text(200, "allowed")
  } else {
    Response::text(403, "denied")
  }
}
```

That function has no `raises ApiError` clause because it handles every `ApiError` itself. The HTTP layer exposes HTTP responses; it does not leak application error types to the network boundary.

## Collect failures as values with `attempt`

Sometimes the caller does not want to handle a failure immediately. `attempt` converts the failure channel into an ordinary `Result` value:

```khora
let result = attempt(fn () => determine_random()!);

match result {
  Result::Ok(value) => print("success"),
  Result::Err(RandomFailure::BelowThreshold(value)) =>
    print("failed at ${value}"),
}
```

This is especially useful when mapping a fallible operation over a collection and you want every element to run:

```khora
let results = items
  |> List::map(fn item =>
    attempt(fn () => process(item)!)
  );
```

The result is a `List<Result<Output, ProcessError>>` rather than a `List<Output>` that aborts at the first `ProcessError`.

## When you want every reason, not the first

`raises` stops at the first failure, which is right when the next step needs the last one's value. Validation is the other shape: the fields of a form, the keys of a config, the columns of a row do not depend on each other, and reporting them one restart at a time makes somebody run the program five times to be told five things it knew the first time.

`Validated<A, E>` from `std::core` is that shape. `map2` runs its combiner only when both sides succeeded, and otherwise carries every error from both:

```khora
let settings = Validated::map2(
  integer("PORT"),
  string("HOST"),
  fn (port, host) => { port: port, host: host },
);
```

`and_then` is the fail-fast one, for a second step written in terms of the first's value — there is no second answer to collect when there is no first value to write it against.

`to_result` collapses the whole thing back into a `Result<A, List<E>>` when it is time to join a `raises` chain. It keeps the list rather than the first error, because throwing the rest away at the boundary would undo the collecting.

[Configuration](/docs/cookbook/configuration/) is the recipe built on this.

## Failure is part of the API

A function's `raises` row is documentation the compiler checks. It answers a question that many languages leave to prose: what normal failure conditions must a caller be prepared to handle?

Keep failure types meaningful at abstraction boundaries. Low-level packages can expose precise operational failures internally, application services can translate those into domain failures, and the outermost boundary can consume them into responses, exit codes, or other protocol values.

For the other half of an effectful function signature, see [Effects and capabilities](./effects-and-capabilities/). `with` says what authority a computation needs; `raises` says how its normal result may fail.

## Traps are different

Bounds violations, arithmetic overflow, and similar traps represent bugs or violated invariants rather than routine recoverable failure. They are intentionally distinct from `raises`; callers should not be forced to model programming errors as ordinary outcomes.
