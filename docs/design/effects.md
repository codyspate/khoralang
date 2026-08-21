# D7 — Effects and handlers

**Status:** decided. Supersedes the monadic `Effect<A, R, E>` API shown in
`docs/project.md` §3 and §4.2, and refines roadmap decision A8.

Khora uses **direct-style algebraic effects**. Capabilities and typed errors are
rows on the function signature, discharged by handlers. Effectful code is
written as ordinary straight-line code — but calls that can abort the function
are marked.

## Why marked, when Koka is not

Koka, Unison and Frank all leave effectful calls unmarked: the signature is the
contract, and the body stays clean. We deliberately diverge, for the reason in
`docs/vision.md` — the audience includes developers who are not functional
programmers, and who will otherwise lose the sense of what a function is doing.

The rule that falls out is one line: **`!` means this call can abort the current
function.** Two signals, both cheap, neither repeated in the types:

- `ledger.` says *this is a capability, not a local function.* Capabilities are
  always reached through the label that binds them, so an effectful call can
  never masquerade as a pure one.
- `!` says *control can leave here.*

That is the legibility a monadic pipeline gave for free, without the plumbing.
Swift reaches the same place with `try` and `await`, from the same motivation.

## Declaring an effect

An effect is a named set of operations. Its shape is a record of function types,
which is exactly what a capability already was under the monadic design — so the
dependency-injection model carries over unchanged.

```
export effect Ledger {
  get_history:  String -> List<Txn> raises DbError,
  flag_account: (String, RiskLevel) -> () raises DbError,
}
```

## Signatures

Two optional clauses, in this order, between the return type and the body:

```
export fn analyze(account_id: String) -> Report
  with { ledger: Ledger, ai: Classifier }
  raises DbError + ModelError
{
  let history = ledger.get_history(account_id)!;
  let risk = ai.classify(history)!;

  match risk {
    RiskLevel::Low => (),
    _ => ledger.flag_account(account_id, risk)!,
  };

  Report::new(account_id, risk)
}
```

- `with { … }` is the capability row. Labels bound here are in scope in the
  body — there is no `ask`.
- `raises …` is the error row, an open union.

Both are **required on `export` functions and inferred on private ones**. Requiring
them on the public surface stops a `print` buried three calls deep from silently
changing an API; inferring them privately keeps the ceremony off everyday code.
§6.5's capability inlay hints show the inferred row without anyone typing it.

Rows may be open: `with { ledger: Ledger | 'r }` accepts any additional
capabilities the caller has.

## Raising

`raise` performs an operation of the error row. Its type is `Never`, so it can
appear wherever an expression can.

```
export fn validate(txn: Txn) -> () raises ValidationError {
  if txn.amount < 0 {
    raise ValidationError::NegativeAmount(txn.amount);
  }
}
```

A handler implementation raises the same way — this is where most real failures
originate:

```
let live_ledger = handler for Ledger {
  get_history: fn id =>
    match Db::query(pool, id) {
      Db::Rows(rows) => rows |> List::map(Txn::of_row),
      Db::Refused(e) => raise DbError::QueryFailed(e),
    },
  flag_account: fn (id, risk) => Db::exec(pool, Sql::flag(id, risk))!,
};
```

## Installing handlers

Handlers must lexically enclose the computation they serve: in direct style a
call evaluates immediately, so a `|> provide(h)` pipeline cannot work — the
effects would be performed before the handler existed.

Two forms, for two situations.

**Postfix**, for a single expression. The call site mirrors the signature:
`with { … }` on the declaration means *I need these*; `with { … }` on the call
means *here they are*.

```
let report = analyze("acc_9921") with {
  ledger: live_ledger,
  ai: live_classifier,
};
```

**Block**, for a region. Necessary whenever the body is a multi-line pipeline,
where postfix would force parentheses around everything and push the injection
far from what it feeds.

```
export fn main() {
  with { ledger: live_ledger, ai: live_classifier, scope: Scope::root } {
    Router::new()
    |> Router::post("/analyze/:id", handle)
    |> Router::listen(8080)
  }
}
```

The relationship between the two is the one Rust has between an `unsafe`
expression and an `unsafe` block.

## Handling errors

`catch` handles part of the error row and subtracts exactly what it handled:

```
export fn analyze_or_defer(id: String) -> Report
  with { ledger: Ledger, ai: Classifier }
  raises DbError
{
  analyze(id)! catch {
    ModelError::RateLimited(ms) => Report::deferred(id, ms),
    ModelError::ContextLengthExceeded(_) => Report::unavailable(id),
  }
}
```

`ModelError` is gone from the signature; `DbError` passes through untouched.

## Effect polymorphism

Function types carry effect rows, so higher-order functions are polymorphic in
their argument's effects:

```
export fn map<A, B, 'e, 'r>(xs: List<A>, f: A -> B with 'e raises 'r) -> List<B>
  with 'e
  raises 'r;
```

This is the single largest ergonomic difference from the monadic design.
`List::map(analyze)` simply works on an effectful function — there is no
`traverse`, no `Effect.all`, no sequencing step, and no Traversable instance to
learn. It is also why higher-kinded types (decision A4) are justified by
containers rather than by the effect system.

## Composing services and contexts

A handler may itself require capabilities. That is the direct-style equivalent
of Effect's `Layer<RIn, ROut>`: a service built on top of other services.
`Handler<E>` is the type; `handler for E { … }` constructs one.

```
export effect Db {
  query: (String, List<Value>) -> List<Row> raises DbError,
  exec:  (String, List<Value>) -> ()        raises DbError,
}

// Needs Config to find the connection string and Scope to own the pool.
export fn postgres_db() -> Handler<Db>
  with { config: Config, scope: Scope }
  raises ConfigError
{
  let url = match config.get("DATABASE_URL") {
    Option::Some(u) => u,
    Option::None => raise ConfigError::Missing("DATABASE_URL"),
  };
  let pool = scope.acquire(Pg::connect(url)!, Pg::close);

  handler for Db {
    query: fn (sql, args) => Pg::query(pool, sql, args)!,
    exec:  fn (sql, args) => Pg::exec(pool, sql, args)!,
  }
}

// Ledger is built on Db, and says so.
export fn sql_ledger() -> Handler<Ledger> with { db: Db } {
  handler for Ledger {
    get_history: fn id =>
      db.query("select * from txn where account = $1", [id])!
      |> List::map(Txn::of_row),
    flag_account: fn (id, risk) =>
      db.exec("update account set risk = $2 where id = $1", [id, risk])!,
  }
}
```

### Bindings in a `with` block are sequential

Each binding may use the ones above it, exactly like a `let` chain. This is what
keeps composition flat instead of nesting one `with` per layer:

```
export fn main() {
  with {
    config: env_config(),
    scope:  Scope::root,
    db:     postgres_db()!,      // uses config and scope
    ledger: sql_ledger(),        // uses db
    ai:     openai_classifier()!, // uses config
  } {
    Router::new()
    |> Router::post("/analyze/:id", handle)
    |> Router::listen(8080)
  }
}
```

Note what is *absent*: there is no layer memoization to reason about. Effect
memoizes layers because they are values combined by a graph algebra, so a shared
dependency could otherwise be built twice. Here `db` is a name bound once, and
everything below it refers to that binding. Sharing is structural, and the
build order is the order you read.

### A named context

A context is just a row, so it can be named and reused:

```
export context Production {
  config: env_config(),
  scope:  Scope::root,
  db:     postgres_db()!,
  ledger: sql_ledger(),
  ai:     openai_classifier()!,
}

export fn main() {
  with Production {
    Router::new() |> Router::post("/analyze/:id", handle) |> Router::listen(8080)
  }
}
```

### Overriding one service

Because contexts are rows, substituting a service is row update — the same
operation the type system already performs on capability rows. Nothing new is
needed for the case that matters most in tests:

```
test "a rate-limited model defers the report" {
  let report = analyze("acc_1") with Production {
    ai: stub_classifier(ModelError::RateLimited(2000)),
  };

  assert(report == Report::deferred("acc_1", 2000));
}
```

`Production` supplies everything; `ai` is replaced. The row machinery that
tracks requirements is the same machinery that composes the things satisfying
them.

## The entry point

`main` must have an empty capability row once handlers are installed — this is
the obligation §2.2 states for `run_native`, moved to where it belongs. An
uncaught error from `main` becomes a diagnostic and a non-zero exit status.

## Deliberate divergences

**From Koka:** fallible calls are marked with `!`. Koka leaves them unmarked.

**From Effect (TypeScript):** no `Effect<A, E, R>` in return types; capabilities
are in scope rather than fetched with `ask`; no `Effect.gen`, because direct
style is native rather than simulated. Every concept maps one to one —
`Layer` is a handler, `provide` is handler installation, `Scope` is unchanged —
but nothing looks the same on sight.

## Open sub-questions

- **Error widening.** When a function raising `DbError` is called from one
  raising `DbError + ModelError`, the row subsumes it. Whether user-defined
  conversions may also fire at `!` (as Rust's `?` does via `From`) is undecided.
- **`raises` versus `with` — settled.** The same resolution mechanism, both
  rows settled at compile time, and different control: a capability is called
  and returns, a failure leaves and does not. They compile to different things,
  which is why keeping them separate in the syntax was right.
  `docs/design/effect-runtime.md` §8.
- **Non-local control flow through handlers.** `break`, `continue` and `return`
  crossing a handler boundary must unwind correctly and run finalizers. See
  `docs/design/imperative.md`.
- **Continuation capture — settled.** Neither, for now: no syntax names a
  continuation, so every handler is tail-resumptive and `raise` is the only
  non-local exit. Adding a form that names one later is a widening rather than
  a break, and would be one-shot. `docs/design/effect-runtime.md` §3 and §4.
