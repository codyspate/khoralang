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
attempt(fn () => foo()!)   turn E into a Result<A, Raised<E>> value
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

**The `!` is still required.** `catch` handles a failure that has already been
let out of the call, so `load_user(id) catch { .. }` — without the mark — is
refused before the arms are looked at:

```
error: `load_user` can leave this function, so the call needs `!`:
       write `load_user(..)!`
```

A `catch` arm uses ordinary pattern syntax over the failure value. The success path keeps the value produced by the inner expression; a matching arm produces the replacement value.

A bare name binds the whole failure, the way it does in a `match`:

```khora
let user = load_user(id)! catch { trouble => User::unavailable(Show::show(trouble)) };
```

That arm handles everything the operand can raise, so it needs no companion. It is the form to reach for when the answer does not depend on which variant arrived -- turning a nine-variant error into one `Refused` is one arm this way and nine identical ones by constructor.

The binding is typed by what can reach it. When the operand raises **one** type and nothing else, the binding is that type, and its variants can be matched directly. When more than one type can reach it -- two named types, a row variable such as `'er`, a failure raised by a closure whose type is decided later -- the binding is a `Raised` over all of them: a failure held as a value, which knows which type it is. See [Failures as values](#failures-as-values).

```khora
let message = sync(order)! catch {
  err => "sync failed: ${err.show()}",   // err: Raised<DbError + ModelError>
};
```

`_` remains the arm for "handle everything and do not look at it": it binds nothing, and the failure is released without being read.

A failure type with a type parameter is handled at the instantiation that was raised. Over `fetch() raises Gx<Int>`, the arm `Gx::X(msg, v)` binds `v` as an `Int`, and an arm body that uses it as anything else is a type error. In a generic function, an arm over `Gx<A>` handles whichever `A` that call was made at.

A `raises` row holds each failure type at one instantiation. Where a `Gx<Int>` and a `Gx<String>` would meet in one row -- the body of one closure, `raises E + F` with `E` and `F` both `Gx`, or one `catch` whose operand raises both -- the program is refused, because one arm reads one layout and nothing says which of the two arrived:

```text
a `catch` arm handles one instantiation of a type, and this operand raises
two; catch them with `_`, or in two `catch`es
```

```text
an error type appears in a `raises` row once, at one instantiation, and this
raises `Gx` at two; catch one of them where it is raised
```

Put a `catch` around each call, or, directly around the two calls, use a `_` arm alone: `_` reads nothing, so it needs no layout. A `_` beside a `Gx::..` arm, or an arm that binds the failure, is refused like a named arm.

A row is labeled by the failure type's name, so two *different* types of one name -- an `E` declared in `m::a` and another in `m::b` -- meet in a row the same way, and are refused the same way:

```text
an error type appears in a `raises` row once, by name, and this raises `E` at
two: two different types named `E`; catch one of them where it is raised
```

## Exhaustiveness by failure type

Handling a named failure type commits to handling all of that type's variants:

```khora
operation()! catch {
  UserError::NotFound(id) => recover_missing(id),
  UserError::Unavailable(reason) => recover_unavailable(reason),
}
```

When those arms exhaust `UserError`, that type is subtracted from the failures that can leave the `catch` expression. A wildcard does not mean “all possible failures in an open row.”

Beside a `_` arm, or an arm that binds the failure, the named arms may take only some of a type's variants. The variants they leave go to that arm, as does a variant whose arm is turned away by a guard or a field pattern:

```khora
let message = checkout(order)! catch {
  PaymentError::Declined(why) => "declined: ${why}",
  _ => "try again later",
};
```

When named arms take some of the types and a binding takes the rest, the binding is a `Raised` over what the named arms leave, even when that is one type: a failure that carried context keeps it through the narrowing.

Arms are tried in the order they are written, and a guard that is false moves on to the next arm. Arms naming a constructor come before the arms that take every failure. A guarded `_` or binding arm hands what its guard turns away to the arm after it, and needs an unguarded `_` or binding arm somewhere after it to take what every guard refuses:

```khora
let message = checkout(order)! catch {
  PaymentError::Declined(why) if retries > 3 => "declined: ${why}",
  e if verbose => describe(e),
  _ => "try again later",
};
```

An arm after an unguarded `_` or binding arm could never run, and is refused.

A `raise` inside an arm leaves the `catch`; it is not handled by the `catch` it is written in. The enclosing function declares what an arm raises, or an enclosing `catch` handles it.

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

`attempt` converts a computation's failure channel into a `Result` whose error is a [`Raised`](#failures-as-values), whatever the body raises -- one type, several, or a row variable:

```khora
let result = attempt(fn () => load_user(id)!);
```

Its type:

```khora
fn attempt<A, 'er, 'ef>(body: () -> A with 'ef raises 'er) -> Result<A, Raised<'er>>
  with 'ef;
```

The returned `Result` is ordinary data:

```khora
match result {
  Result::Ok(user) => use_user(user),
  Result::Err(failure) => log_error(failure.show()),
}
```

For a body that raises one type, `Raised::only` gives back the error itself, to match on:

```khora
match attempt(fn () => load_user(id)!) {
  Result::Ok(user) => use_user(user),
  Result::Err(failure) => match Raised::only(failure) {
    UserError::NotFound(_) => guest(),
    UserError::Unavailable(reason) => offline(reason),
  },
}
```

For a body that raises several, raise the failure into a `catch`, which matches it per type:

```khora
match attempt(fn () => fetch(url)!) {
  Result::Ok(answer) => answer,
  Result::Err(failure) => (raise failure) catch {
    IoError::NotFound(_path) => fallback(),
    Denied::Denied(_path) => refuse(),
    other => give_up(other.show()),
  },
}
```

## Failures as values

`Raised<'er>` is a failure held as a value: one error, of one of the types in the row `'er`, which remembers which. A `catch` binding over more than one type gives one, and so does `attempt`. It is how a failure is stored, logged, passed to a function, or returned in a `Result`, without declaring a type for every combination of failures a program happens to meet.

```khora
fn describe<'er>(failure: Raised<'er>) -> String {
  "${Raised::type_name(failure)}: ${Raised::describe(failure)}"
}
```

`Raised<A + B>` is written with the row's types, and `Raised<'er>` with a row variable.

What a `Raised` has:

| | |
| --- | --- |
| `Raised::type_name(f)` | the declared name of the error's type: `"PaymentError"` |
| `Raised::describe(f)` | the error by its own type's `Show`, or its type name for a type without one |
| `f.show()` | the context, then the error: `"charging: PaymentError::Declined(card)"` |
| `Raised::context(f, note)` | the same failure with `note` added outside its context |
| `Raised::note(f)` | the context attached so far, outermost first, or `""` |
| `Raised::forget(f)` | the same failure as a `Raised<Opaque>`, its row forgotten |
| `Raised::only(f)` | the error itself, for a `Raised` over exactly one type |
| `raise f` | sends the error on as the type it is, with its context |

**`raise f` is the way back.** It does not raise a value of type `Raised`: it raises the error inside, as its own type, so a `catch` after it matches `PaymentError::Declined` exactly as if the original `raise` had reached it. A function that raises `f: Raised<A + B>` declares `raises A + B`.

### Context

`context` adds a note to a failure without changing what it is:

```khora
fn sync(id: Int) -> Invoice raises DbError + HttpError {
  fetch(id)! catch { err => raise Raised::context(err, "syncing invoice ${id}") }
}
```

A `catch` after it still matches `DbError::Disconnected` by its type and variant. The note goes with the failure through `raise`, through `retry` and `retry_while`, through cancellation, and out of a fiber to whoever joins it, and `show` prints it first:

```text
syncing invoice 7: DbError::RowCorrupt(7)
```

Each `context` costs one allocation for the new failure value, since the one it was given may be held elsewhere and is left as it was, plus whatever building the note's text costs. A literal note costs nothing more, because a literal is not allocated. An interpolated one costs the string it builds: `"charging ${n}"` is three more. A second `context` on a failure that already has a note joins the two as `"outer: inner"`, which is one more string. So per failure, a literal note costs 1 allocation, `"charging ${n}"` costs 4, and two literal notes cost 3. A program that attaches no note pays nothing for the possibility.

### Forgetting the row

A record or a list that keeps outcomes from several places cannot list every error each of them might raise. `Raised::forget` turns any `Raised` into a `Raised<Opaque>`, which is one type for all of them:

```khora
let outcomes: List<Result<Int, Raised<Opaque>>> = jobs |> List::map(fn job =>
  attempt(fn () => run(job)!) |> Result::map_err(fn f => Raised::forget(f))
);
```

The error inside keeps its type, and a `catch` still matches it by that type. Because any type can arrive, a `catch` over `Opaque` needs a `_` or a binding arm:

```khora
(raise stored) catch {
  DbError::Disconnected(_why) => retry_later(),
  other => log(other.show()),
}
```

`Opaque` exists only inside a `Raised`. A value of type `Opaque` on its own, `Raised::only` on a `Raised<Opaque>`, and a `raises Opaque` row are refused, because there is no type to read the error as. A generic type named in a `catch` arm over `Opaque` -- `Gx::X(..)` -- needs its arguments decided by something else in the arm, since nothing in the failure says which `Gx` it is; and all the arms naming one type share one instantiation.

### What a `Raised` cannot do

A `Raised` cannot be compared with `==`, hashed, or sent to another fiber. It stays on the fiber that caught it, because the error and note inside it are counted by that fiber alone. That holds through a `Handoff`, a `Channel`, a capture of `Fiber::spawn` and a spawned body's error, and through any record or list holding one. To send a failure to another fiber, send its text, `failure.show()`, or its error, `Raised::only(failure)`, when that error type is `Share`.

`match` sees a `Raised` as a `Raised`, not as the error inside, so a `Pay::Declined` pattern in a `match` over one is a type error. Raise it into a `catch` to match on the error.

### Cost

A failure becomes a value in a box of three words -- which type, the error, the note -- allocated when it is bound or attempted. A `catch` that names its cases, or binds a failure of one type, allocates nothing, and a program that never attaches context runs the same failure paths it would without `Raised`.

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

Every combinator on `List` that takes a function is written that way — `fold`,
`filter`, `find`, `any`, `all`, `partition`, `flat_map`, and their counterparts
on `Option` and `Result` — so a fallible step never means leaving the chain to
write the walk by hand. Mapping a *pure* function needs neither mark: an empty
row is what a row variable takes when nothing fills it.

**`std::fs::fold_lines` and `fold_chunks` are the exception.** Their `step`
parameter is `(A, String) -> A` with no `with` row and no `raises` row, so a
step that fails or that needs a capability cannot be passed:

```text
error: this argument: `Bad` is not accounted for here
```

The message does not say why. Until those signatures grow rows, a streaming
walk whose step can fail has to be written as `read_text` plus
`String::split`, which holds the whole file.

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
type Settings = { port: Int, host: String };

let settings: Validated<Settings, String> = Validated::map2(
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
      Result::Err(failure) => match Raised::only(failure) {
        IoError::NotFound(missing) => { eprint("no such file: ${missing}"); 1 },
        _ => { eprint("could not read it"); 1 },
      },
    }
  }
}
```

`eprint` and not `print`: the diagnostic goes to standard error, so that a
program whose output somebody redirected still says why it stopped. It comes
from `std::log`.

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