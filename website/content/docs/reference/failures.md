---
title: Failures
sidebar:
  order: 12
---

Recoverable failure is part of a Khora function type. `raises` declares the channel, `raise` creates a failure, postfix `!` propagates one, `catch` handles typed cases, and `attempt` converts the channel into `Result` data.

The four pieces, at a glance:

```text
raise E                    create a typed failure
foo()!                     allow E to propagate out of the current function
foo()! catch { ... }       handle E here
attempt(fn () => foo()!)   turn E into a Result<A, E> value
```

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

A bare name binds the whole failure, the way it does in a `match`:

```khora
let user = load_user(id)! catch { trouble => User::unavailable(Show::show(trouble)) };
```

That arm handles everything the operand can raise, so it needs no companion. It is the form to reach for when the answer does not depend on which variant arrived -- turning a nine-variant error into one `Refused` is one arm this way and nine identical ones by constructor.

The binding is typed by the operand's failure row, so it works when that row names **one** type. Where an operand raises two, there is no single type to give the name, and the compiler says so:

```text
this `catch` arm binds the failure, but the operand can raise more than one
type (DbError, ModelError), so there is no single type to give the binding.
Name a constructor, or use `_` to handle them all without looking
```

`_` remains the arm for "handle everything and do not look at it": it binds nothing, and the failure is released without being read.

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

A layer should usually not expose the failures of the layer beneath it, which
makes this the ordinary boundary shape:

```text
infrastructure/domain failure
          |  catch + raise
application/API failure
          |  catch
response or other boundary value
```

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

### One error type

`E` is a type, not a row, so `attempt` handles a body that raises exactly one thing. A body raising two has nowhere to go through it:

```text
error: this argument: `Denied` is not accounted for here. This takes one error
       type and the body raises `IoError` and `Denied`; there is no type that
       means "either of these", so handle them with `catch` instead
```

This is a real limit rather than an oversight. `Result<A, E>` needs one `E`, and Khora has no anonymous sum type to name "either of these two" — so there is nothing for a two-type row to collapse into. Naming the union would mean declaring a type for every pair of failures a program happens to combine.

Use [`catch`](#catch) for a wider row. It matches per type and never has to name the union:

```khora
let answer = fetch(url)! catch {
  IoError::NotFound(_path) => fallback(),
  Denied(_path) => refuse(),
};
```

## Two marks, doing different jobs

A fallible function can be mapped directly, because a function type carries its
own capability and failure rows and the combinator carries the caller's:

```khora
let users = ids |> List::map(fn id => load_user(id)!)!;
```

The inner `!` is `load_user` failing. The outer one is `List::map` passing that
failure on, which it can do only because its own signature ends `raises 'er`.
Leaving the outer one off is an error naming both halves:

```text
error: `List::map` can leave this function, so the call needs `!`: write `List::map(..)!`
error: `List::map` needs `Bad`, which this function does not raise
```

Every combinator that takes a function is written that way — `fold`, `filter`,
`find`, `any`, `all`, `partition`, `flat_map`, and their counterparts on
`Option` and `Result` — so a fallible step never means leaving the chain to
write the walk by hand. Mapping a *pure* function needs neither mark: an empty
row is what a row variable takes when nothing fills it.

## Collect per-item failures

Propagating from inside `List::map` stops at the first failure:

```khora
items |> List::map(fn item => process(item)!)!
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

## When you want every reason, not the first

`raises` stops at the first failure, which is right when the next step needs
the last one's value. Validation is the other shape: the fields of a form, the
keys of a configuration, the columns of a row do not depend on each other, and
reporting them one restart at a time makes somebody run the program five times
to be told five things it knew the first time.

`Validated<A, E>` from `std::core` is that shape. `map2` runs its combiner only
when both sides succeeded, and otherwise carries every error from both:

```khora
let settings = Validated::map2(
  integer("PORT"),
  string("HOST"),
  fn (port, host) => { port: port, host: host },
);
```

`and_then` is the fail-fast one, for a second step written in terms of the
first's value — there is no second answer to collect when there is no first
value to write it against.

`to_result` collapses the whole thing into a `Result<A, List<E>>` when it is
time to rejoin a `raises` chain. It keeps the list rather than the first error,
because throwing the rest away at the boundary would undo the collecting.

[Load application configuration](/docs/cookbook/configuration/) and
[Schemas](/docs/stdlib/schema/) are both built on it.

## `Result::map_err`

Once a failure is already data, use normal `Result` operations rather than `catch`:

```khora
let mapped = result
  |> Result::map_err(fn error =>
    ApiError::from_user_error(error)
  );
```

This maps `Result<A, E>` to `Result<A, F>`. By contrast, `catch { ... raise F ... }` maps the live failure channel from `E` to `F`.

## A failure that reaches `main`

`main` is not called by anything, so there is nowhere to hand a failure that
gets that far. A program whose entry point raises and does not handle it ends
with **exit status 1**, and prints the error type's name to standard error:

```
khora: `IoError` reached the entry point and nothing handled it
note: `main` has nowhere to hand an error, so the program ends here with status 1. Catch it in `main`, or return a `Result`
```

The *type* is named, not the value. Handle it where the program can say
something better:

```khora
pub fn main() -> Int {
  with { reads: FsRead::real() } {
    match attempt(fn () => read_text(path)!) {
      Result::Ok(text) => { print(text); 0 },
      Result::Err(IoError::NotFound(where)) => { print("no such file: ${where}"); 1 },
      Result::Err(other) => { print("could not read it"); 1 },
    }
  }
}
```

A cancellation that reaches the entry point is a different outcome and exits
**130** — 128 plus `SIGINT`, which is what a shell already means by
"interrupted". It prints nothing, because stopping a program with a keystroke
is not a failure to report.

## `main` installs capabilities rather than requiring them

For the same reason, `main` may not carry a `with { .. }` requirement: nothing
calls it, so nothing could supply one. Install them in the body instead —
`with { reads: FsRead::real() } { .. }`, as above. A `with` clause on `main` is
refused at compile time.

## Failure is part of the API

A `raises` row is documentation the compiler checks. It answers, in the
signature, a question many languages leave to prose: what normal failure
conditions must a caller be prepared for?

Keep the failure types meaningful at each boundary. A low-level package can
expose precise operational failures, an application service can translate those
into domain failures, and the outermost boundary can consume them into
responses, exit codes or other protocol values.

`with` and `raises` are the two halves of an effectful signature and are
independent: `with` says what authority a computation needs,
[Capabilities](./capabilities/) covers it, and `raises` says how its normal
result may fail.

## Traps are separate

Arithmetic overflow, bounds failures, and other violated invariants are traps, not ordinary `raises` values. See [Traps](./traps/).