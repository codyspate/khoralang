---
title: Failures
sidebar:
  order: 11
---

Recoverable failure is part of a Khora function type. `raises` declares the channel, `raise` creates a failure, postfix `!` propagates one, `catch` handles typed cases, and `attempt` converts the channel into `Result` data.

## Declare a failure type

Failure types are ordinary ADTs:

```khora
pub type UserError =
  | NotFound(id: Id)
  | Unavailable(reason: String);
```

## `raises` on a function

```khora
fn load_user(id: Id) -> User
  raises UserError
{
  // ...
}
```

Several failure types use `+`:

```khora
fn handle_request() -> Response
  raises UserError + HttpError
{
  // ...
}
```

## `raises` on a function type

```khora
Id -> User raises UserError
```

Open generic row:

```khora
A -> B raises 'er
```

## Explicit `raise`

```khora
raise UserError::NotFound(id)
```

`raise` is an expression of type `Never`: the current path does not produce the surrounding normal result.

Example:

```khora
fn require_user(found: Option<User>, id: Id) -> User
  raises UserError
{
  match found {
    Option::Some(user) => user,
    Option::None => raise UserError::NotFound(id),
  }
}
```

## Propagate with postfix `!`

```khora
let user = load_user(id)!;
```

The call's normal value remains `User`; its failure is allowed to leave the current computation. The enclosing function must either permit that failure in its own `raises` row or handle it.

```khora
fn load_profile(id: Id) -> Profile
  raises UserError
{
  let user = load_user(id)!;
  Profile::from_user(user)
}
```

## `!` is a call-site marker

Prefix and postfix `!` have different meanings:

```khora
!enabled      // boolean negation
load_user(id)! // failure propagation
```

Position disambiguates them.

## Handle failures with `catch`

```khora
let user = load_user(id)! catch {
  UserError::NotFound(_) => User::guest(),
  UserError::Unavailable(reason) => User::offline(reason),
};
```

A `catch` arm uses ordinary pattern syntax over the failure value. The success path keeps the value produced by the inner expression; a matching arm produces the replacement value.

## Exhaustiveness by failure type

Handling a named failure type commits to handling all of that type's variants:

```khora
operation()! catch {
  UserError::NotFound(id) => recover_missing(id),
  UserError::Unavailable(reason) => recover_unavailable(reason),
}
```

When those arms exhaust `UserError`, that type is subtracted from the failures that can leave the `catch` expression. A wildcard does not mean “all possible failures in an open row.”

## Translate failure types

Catch one type and raise another:

```khora
fn load_for_api(id: Id) -> User
  raises ApiError
{
  load_user(id)! catch {
    UserError::NotFound(_) =>
      raise ApiError::NotFound,

    UserError::Unavailable(reason) =>
      raise ApiError::ServiceUnavailable(reason),
  }
}
```

`UserError` no longer escapes `load_for_api`; callers see `ApiError`.

## Handle only part of a multi-type row

```khora
fn analyze(id: Id) -> Report
  raises DbError
{
  analyze_model(id)! catch {
    ModelError::RateLimited(_) => Report::deferred(id),
    ModelError::ContextLengthExceeded(_) => Report::unavailable(id),
    ModelError::InferenceEngineFailure(_) => Report::unavailable(id),
    ModelError::SchemaExtractionError(_) => Report::unavailable(id),
  }
}
```

If the inner expression raises `DbError + ModelError`, exhaustively catching `ModelError` leaves `DbError` untouched.

## Convert failures to a boundary value

```khora
fn handle(id: Id) -> Response {
  let user = load_for_api(id)! catch {
    ApiError::NotFound =>
      return Response::text(404, "not found"),

    ApiError::ServiceUnavailable(reason) =>
      return Response::text(503, reason),
  };

  Response::json(200, user)
}
```

The function has no `raises ApiError` because the boundary consumes every `ApiError` into an ordinary `Response`.

## `attempt`

`attempt` converts a computation's failure channel into `Result<A, E>`:

```khora
let result = attempt(fn () => load_user(id)!);
```

Conceptual type:

```khora
fn attempt<A, E, 'ef>(body: () -> A with 'ef raises E) -> Result<A, E>
  with 'ef;
```

The returned `Result` is ordinary data:

```khora
match result {
  Result::Ok(user) => use_user(user),
  Result::Err(error) => log_error(error),
}
```

## Collect per-item failures

Propagating from inside `List::map` stops on the first failure:

```khora
items |> List::map(fn item => process(item)!)
```

Convert each invocation with `attempt` to run all items and collect both successes and failures:

```khora
let results = items
  |> List::map(fn item =>
    attempt(fn () => process(item)!)
  );
```

The result has the shape:

```khora
List<Result<Output, ProcessError>>
```

## `Result::map_err`

Once a failure is already data, use normal `Result` operations rather than `catch`:

```khora
let mapped = result
  |> Result::map_err(fn error =>
    ApiError::from_user_error(error)
  );
```

This maps `Result<A, E>` to `Result<A, F>`. By contrast, `catch { ... raise F ... }` maps the live failure channel from `E` to `F`.

## Traps are separate

Arithmetic overflow, bounds failures, and other violated invariants are traps, not ordinary `raises` values. See [Traps](./traps/).