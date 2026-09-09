---
title: From Go
sidebar:
  order: 2
---

Go and Khora agree about the shape of the artifact: one native executable, no
VM beside it, fast start, and an operational story you can hold in your head.
They disagree about how much of a service's behaviour belongs in the type. Four
places where a Go habit needs adjusting, with the Khora each of them becomes.

## Errors: the second return value moves into the type

Go returns failure as a value, by convention, and the compiler is happy if you
ignore it:

```go
user, err := loadUser(id)
if err != nil {
    return Response{}, err
}
```

Khora puts the failure channel in the signature. The failure type is an
ordinary ADT:

```khora
pub type UserError =
  | NotFound(id: Id)
  | Unavailable(reason: String);
```

and the function says it uses the channel:

```khora
fn load_user(id: Id) -> User
  raises UserError
{
  // ...
}
```

The `if err != nil` becomes a postfix `!` at the call, which propagates:

```khora
Id -> User raises UserError
```

Three consequences a Go reader should expect:

- **`!` is not `?` and it is not `panic`.** It says *this call may fail and I am
  letting the failure out of here*, which is only legal if the enclosing
  function's own `raises` row admits it. A missing `!` is a compile error, not a
  dropped error.
- **`raises` composes with `+`** — `raises UserError + HttpError` — rather than
  collapsing to one `error` interface, so the arms of a `catch` are checked for
  exhaustiveness the way a `match` is.
- **There is no `errors.Is` / `errors.As` unwrapping ceremony**, because nothing
  was wrapped into an interface to begin with. [Failures](/docs/reference/failures/)
  has `catch` and `attempt`, which are the two ways back out.

The habit to drop: returning a zero value beside an error. A Khora function that
fails does not also produce an answer, so there is no zero `Response{}` to
invent and no caller reading one by mistake.

## Dependencies: from a struct field to a `with` row

Go passes a `*sql.DB`, a logger and a clock down through constructors, or
reaches for a package-level variable. Khora makes the requirement part of the
type, and it is discharged once, at the boundary:

```khora
with {
  config: env_config(),
  scope: Scope::root(),
  db: postgres_db()!,
  store: sql_store(),
} {
  run_server()!
}
```

Everything inside that block can call functions declaring `with { db: Db }`
without being handed a `db` argument, and a function that does *not* declare it
cannot reach one. That is the part with no Go analogue: the absence of ambient
authority is checked, so a helper five layers down cannot quietly open a socket.

The habit to drop: the `context.Context` first parameter. It is carrying three
different things — cancellation, deadlines and request-scoped values — and each
of them has its own home here. Values are capabilities in the `with` row.
Cancellation belongs to the nursery, below. Deadlines are the piece that does
not exist yet: there is no `timeout`, no `race` and no `select`, and
[Concurrency](/docs/reference/concurrency/) says so plainly rather than leaving
you to find out.

## Concurrency: `go f()` always has an owner

The Go loop that starts work and hopes:

```go
for _, job := range jobs {
    go handle(job)
}
```

has no answer to *who waits for these*, *what happens when one panics*, or
*what stops them*. A `sync.WaitGroup` and a `context` are the conventional
answers and neither is enforced. In Khora a fiber is adopted by a nursery, and
the nursery is a scope that does not return until its children are done:

```khora
module main;

import std::core::{ChildFailed, Fiber, Nursery, bounded_nursery, print};

fn handle(job: Int) -> () {
  print("processing job ${job}");
}

fn launch_jobs() -> ()
  with { nursery: Nursery }
{
  let mut next = 0;

  while next < 1000 {
    let job = next;

    nursery.adopt(
      Fiber::spawn(fn () => handle(job))
    );

    next = next + 1;
  }
}

pub fn main() raises ChildFailed {
  bounded_nursery(64, launch_jobs)!
}
```

`bounded_nursery(64, ...)` is also the `WaitGroup` plus the semaphore Go
programs write by hand: adoption blocks once the limit is live, so the producer
slows at the boundary where it makes work instead of filling a queue. See
[Bound concurrent work](/docs/cookbook/bounded-concurrency/), including the
off-by-one to subtract when the limit is a real resource.

Cancellation arrives without a `ctx.Done()` channel to select on, because a
cancellation travels out on the same tagged return a failure does. It is
observed at a `!` and at a **loop back-edge**, which is what makes an ordinary
polling worker stoppable with nothing in it that looks like a cancellation
check:

```khora
fn reaper() -> () with { clock: Clock } raises Stop {
  loop {
    clock.sleep(1000);
    sweep();
  }
}
```

The habit to drop: a bare `go`. There isn't one — `Fiber::spawn` produces a
handle, and you either `join` it for its answer or `adopt` it into a nursery
that will.

## Memory: reference counting instead of a tracing collector

Go's collector gives you no annotations and a tail. Khora's answer is Perceus
reference counting with compiler ownership and reuse analysis: also no
annotations, no borrow checker, and no collector thread — the counts are
inserted at compile time and a uniquely-owned value is reused in place rather
than freed and reallocated.

What that changes operationally is the shape of the latency curve and the
resident set rather than the code you write. The
[Performance](/docs/performance/) page has the measured numbers, the ladder
they were taken on, and what is explicitly not measured; it is the page to read
before quoting anything.

The one place a Go programmer gets *less*: **a reference cycle leaks.** A
tracing collector is what Khora exists not to have, so there is no cycle
collector, and weak references — the usual way to break one — do not exist yet
either. A cycle needs a `mut` field deliberately pointing back at something that
already reaches it, so it is not accidental; it is also not caught. The leak is
bounded and quiet rather than unsound: nothing is freed early and nothing is
read after free, the memory is simply never returned.

## What stays familiar

Direct-looking code with no monadic plumbing, one binary to ship, a standard
library that covers HTTP and TLS, structured logging as JSON on standard error,
and a small number of runtime concepts visible in application code. `khora fmt`
is `gofmt`'s idea — canonical formatting, not a style argument — and `khora
test .` compiles and runs the package's tests one fiber each.

The thing that is genuinely missing rather than different: there is no package
registry, so a dependency is a `git` URL, and `khora install` fetches and locks
what `khora.toml` declares. [Modules and packages](/docs/reference/modules-and-packages/)
is the page for that.
