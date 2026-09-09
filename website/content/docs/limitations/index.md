---
title: Known limitations
sidebar:
  order: 0
---

Khora is pre-1.0. This page exists so users can tell the difference between a language rule, a supported feature, and unfinished work.

## Toolchain distribution

Khora has versioned toolchain artifacts and installers for the platforms released by the project. The normal application-developer path is the installer documented in [Installation](/docs/getting-started/installation/), not compiling the compiler from source.

The remaining limitation is **target coverage**, not the absence of distribution. A target is only labeled supported when the compiler, runtime, linker/sysroot, packaging, CI, and deployment/conformance path work end to end. See [Supported targets](/docs/deployment/supported-targets/) for that distinction.

## Recursion depth and very large lists

Khora does not guarantee tail-call optimisation, so a function that recurses once per element uses one stack frame per element. Running out of stack ends the program; it reports

```
khora: the stack ran out
```

on standard error and exits with the platform's stack-overflow status.

Every traversal in `std::core`'s `List` is written as a loop rather than as recursion — `length`, `fold`, `reverse`, `filter`, `take`, `drop`, `any`, `all`, `find`, `contains`, `zip`, `flat_map`, `sum`, and the `merge` inside `sort` — so walking a list of any size is safe. `List::sort` recurses only to divide, which is about `log2(n)` deep.

The same is true of text. `String::split` was a frame per field and `String::join` a frame per piece, so splitting a large file and joining it back up were both cliffs; both are loops now, and `join` combines adjacent pairs rather than one piece at a time, so building a string out of many is no longer quadratic in its own length. `String::repeat` doubles for the same reason.

Releasing a value costs no stack either: reference counting frees a value's children through a queue rather than by recursing, so letting go of a long list is a loop like walking one. A million-element `List` sorts.

What is left is ordinary recursion that somebody writes. A function that calls itself once per element of its input will use a frame per element, and no analysis in the compiler turns that into a loop.

`Array<A>` and `Vector<A>` remain the better shape for a large indexed collection — a list is for building front-to-back and walking once — but the choice is now about cost rather than about a cliff.

## Package ecosystem

Dependencies can be pinned reproducibly to git revisions, but there is not yet a public package registry or broad third-party ecosystem.

## Editor tooling

`khora lsp` already provides compiler-backed diagnostics, hover, formatting, completion, signature help, go-to-definition, references, document/workspace symbols, semantic tokens, code actions, code lenses, and inlay hints.

Rename now covers a declaration and every file that names it, including the import that brings the name into each file, and it renames the original rather than a file's own alias. It is still narrower than the rest of the navigation surface in two places, and refuses each with a sentence saying why rather than applying a partial rename: a **trait member**, whose name belongs to the trait and to every impl of it, and a **constructor**, which has no recorded range to edit. Further refactoring operations remain editor-tooling work.

See [Editor setup](/docs/getting-started/editor/) for the language-server command and client setup.

## Standard-library API docs

`khora doc` generates the checked-in standard-library API reference from compiler-resolved declarations plus `///` and `//!` documentation comments. `khora doc --check` is used to detect drift between the source declarations and generated pages.

Two important documentation-tooling gaps remain:

- Khora code blocks in API documentation are not yet compiled as documentation tests.
- Generated signatures name referenced types but do not yet cross-link those type names to their API pages.

See the [Standard library](/docs/stdlib/) entry point for the generated reference.

## HTTP surface

The reference HTTP implementation is intentionally not presented as every protocol feature a mature web framework might provide. The shipping documentation should be treated as the supported surface; unlisted body encodings, upgrades, protocol versions, or framework conveniences should not be assumed merely because the core server/client path exists.

**The verbs are `GET`, `POST`, `PUT`, `PATCH` and `DELETE`, routed to
handlers, plus `HEAD` and `OPTIONS`, which the router answers from what is
mounted unless you mount a handler for them.** Anything else — `TRACE`,
`CONNECT`, an extension method — is answered `400` and the connection closed,
because `Method::of` does not name it and an unparsed request line is a
malformed one as far as the reader is concerned. This page is where that
belongs because the failure is silent from the client's side: a `400` to a
verb the server does not know reads as the client's mistake.

**A request is capped at 8 KB by default, headers and body together, and the
cap is configurable.** Past it the server answers `413` before parsing
anything, so the handler never runs. The number answers "how much may an
unauthenticated client make a server hold" rather than "how large can a
request be" — nothing in the parser recurses per byte or per line, and a
39,808-byte request carrying 2,001 headers parses when the limit admits it.
`Router::holding` sets another; the buffer is allocated once at that size per
connection, so it multiplies by the connection bound below when deciding what
a full server costs. There is no chunked transfer and no multipart decoding,
and a body must be UTF-8 text.

**The server serves at most 256 connections at once, and the number is not
configurable.** `Router::listen` and the TLS form wrap their accept loops in
`bounded_nursery(256, ..)`, and an accepted connection is a fiber that is
inside your handler for as long as the handler runs. There is no second,
smaller pool that handlers queue for, so that one number is both the most
connections served at once and the most handlers running at once; the next
connection waits for one of them to finish. It is a property of the reference
server rather than of the `Router` type, so it does not appear in the generated
[`std::net::http` pages](/docs/stdlib/api/net/http/); [Serve HTTP
requests](/docs/cookbook/http-service/) is where it is discussed. Because of
the off-by-one below, the peak actually observed is 257.

## The fiber scheduler

A fiber is an operating-system thread. The M:N scheduler — stackful coroutines on a worker pool — is built and is opt-in with `KHORA_FIBERS=scheduler`.

It is not the default for 0.1.0 for three reasons, and one of them is a gap rather than a preference:

- Threads are faster at the connection counts a service runs at.
- The scheduler exists for fiber **density**, and that claim is measured on Windows only. Linux caps `vm.max_map_count` at 65530 and guard pages split mappings, so the "100,000 waiting fibers" figure has not been reproduced on the platform most deployments use.
- It is the less-exercised path, and therefore the likelier home of the next runtime bug.

The two are *meant* to be indistinguishable, and the [concurrency
reference](/docs/reference/concurrency/) still says a program cannot tell which
it has. It can, today: see [The two fiber backends are
distinguishable](#what-a-nursery-actually-does) below. Until that is closed the
default cannot change without notice, whatever the compatibility policy says
about it.

**Throughput figures published before September 2026 were two to twelve times too high.** The load generator reported one connection's rate multiplied by the number of connections, which is why no ceiling was ever found. It has been replaced, the numbers have been retaken, and [Performance](/docs/performance/) carries them with the conditions they satisfy. Any requests-per-second number for Khora from an older document or post is wrong.

## Characters and strings

A `String` is UTF-8 and is indexed in **bytes**. `String::slice` stops the program if a cut lands inside a character, so ask first:

```khora
let safe = String::slice(text, 0, String::next_boundary(text, 20));
```

`is_char_boundary`, `next_boundary`, `previous_boundary`, `char_at`, `chars` and `char_length` are the character-level API; `byte_length` is the constant-time one and `char_length` walks.

The character predicates — `Char::is_digit`, `is_alpha`, `is_whitespace`, `to_upper`, `to_lower` — are **ASCII only** and say so in their own documentation. Unicode case mapping and the full `Nd` category are not in `std`, deliberately: they need tables that would double its size, and a library is the right place for them.

## Union types

There is no way to write "an `Int` or a `String`" as the type of a value. `+` joins the failure types of a `raises` row and means nothing outside one; `T: Eq + Show` is the other meaning of the symbol, a trait bound, and works as it does in Rust.

The practical consequence is that `attempt` handles a body raising exactly one type. Use [`catch`](/docs/reference/failures/#handle-failures-with-catch) for a wider row — it matches per type and never has to name a combined type.

[The unions design note](https://github.com/codyspate/khoralang/blob/main/docs/design/unions.md) records what a union would mean, what it would cost, and why existentials are not part of the same question.

## Concurrency combinators

A fiber carries its answer and its failure row — `Fiber<A, 'er>`, with `join` re-raising what the child raised — and `Clock` can `sleep`. The combinators built on top of those do not exist yet: there is no `timeout`, no `race`, and no bounded parallel map.

**The reason previously given for that is out of date.** This page and the
[concurrency reference](/docs/reference/concurrency/) both said that a parent
blocked on `Channel::receive` serialises the fibers it spawned, and gave the
number: two 2000 ms fibers taking 4.8 seconds by channel against 2.8 seconds
when the parent waits on their handles. **That no longer reproduces.**
Re-measured on exactly that shape — two fibers of 2000 ms, a parent taking two
values off a `Channel::bounded(4)`, against two `Fiber::join`s, against
`join_all` — with `khora 0.2.0 (b16417c)` on the project's x86_64 Linux
development machine, three runs per backend: 2072 ms by channel, 2105 ms by
handles, 2122 ms by `join_all` under the default thread backend, and the same
three indistinguishable from each other under `KHORA_FIBERS=scheduler`. At
500 ms per fiber over 25 runs they are 518, 514 and 516 ms. Channel fan-in is
concurrent; neither the 4.8-second figure nor the 2.8-second one survives.
Either the serialisation was fixed or the published measurement was wrong, and
the honest answer is that this project does not know which.

**What still blocks a hand-written `race` or `timeout` is something else.**
`Fiber::wait` and `Fiber::join` are not cancellation points and carry no
failure row, so a fiber parked in one cannot be stopped until the child it is
waiting on ends by itself: cancelling a parent that is inside `Fiber::wait` on
a 2000 ms child landed after 1939-1984 ms, 20 runs out of 20 on both backends,
against 0-5 ms for the same parent with no child to wait on. Worse, the parent
then ran the rest of its body — the statement after the wait executed in 20 of
20 runs — so a fiber that was cancelled can still publish a result. A `race`
built out of `spawn` plus a wait is therefore bounded by its *slowest* branch
rather than its fastest, and a deadline built that way reports a cancellation
the run then ignores. A cancellable wait — `Fiber::wait` with a bound, or a
`raises` row on `wait`/`join` of the kind `Channel::receive` already has — is
what these combinators are actually waiting on.

Two smaller things a supervisor meets on the way. `Fiber::wait` tells you
nothing about how the fiber ended — there is no status and no
`Option<Result<..>>` — so the only way to find out is a `Shared` cell the child
writes before it fails, which is the sort of thing the failure row was supposed
to make unnecessary. And a fiber that raised and was `wait`ed on rather than
`join`ed prints `khora: a fiber ended with an error nobody was waiting for` to
standard error at process exit, once per such fiber, with no way to suppress it
and no effect on the exit status. Since `wait` is the documented thing to use
after `cancel`, a supervisor that cancels children prints that line routinely.

`Channel` also has no `select` (waiting on the first of several) and no zero-capacity rendezvous. `Channel::bounded(0)` gets a capacity of one rather than a rendezvous, deliberately.

## What a nursery actually does

Four things about nurseries are not yet what the [concurrency
reference](/docs/reference/concurrency/) describes, and one of them has a rider
of its own. All of them were found by measuring, not by reading, and a
reader who builds on the prose will meet them as flakiness. The figures below
are `khora 0.2.0 (b16417c)` on the project's x86_64 Linux development machine,
20 to 25 runs per backend, under both the default thread backend and
`KHORA_FIBERS=scheduler`.

**A bounded nursery admits `limit + 1` children.** `bounded_nursery(4)` runs
five at once; the peak was exactly `limit + 1` on every run of both backends at
limits of 1, 2, 4 and 8. `Fiber::spawn` *starts* the child, and `nursery.adopt`
is what blocks when the nursery is full — so by the time the bound is applied,
the work is already running. Treat the number as a bound on children the
nursery is holding, not on work in flight, and **subtract one** where the limit
is a real resource such as a connection pool. [Bounded
concurrency](/docs/cookbook/bounded-concurrency/) is the recipe built on this
number, and `bounded_nursery(64, ..)` there is 65 live children.

**A limit of zero or less means no limit.** `bounded_nursery(0, ..)` is how the
unbounded `nursery` is built, so zero is not a mistake in the implementation —
200 children ran at once under a limit of 0, and a negative limit behaves the
same way. It is a mistake waiting for a caller, because nothing on the
`bounded_nursery` page says so: `Channel::bounded` documents that it clamps a
capacity below one *up* to one, and the nursery does the opposite without
mentioning it. A limit computed from configuration that comes out zero removes
the bound rather than failing or clamping, so check it before you pass it.

**A child's failure usually cancels no siblings at all.** The reference says
the first failure cancels the siblings. A nursery reaps handles oldest-first,
so a failure is invisible until every child adopted before it has finished —
and by then there may be nothing left to cancel. Measured with twelve children
of 400 ms each, one of which raises after 10 ms, 25 runs a side, default thread
backend:

| the doomed child was adopted | siblings cancelled, of 11 | runs where **nothing** was cancelled |
| --- | --- | --- |
| first | 11, after 2-8 ms | 0 of 25 |
| in the middle (6th of 12) | 0 (median; 0-5) | 13 of 25 |
| last | 0 | 25 of 25 |

`KHORA_FIBERS=scheduler` is the same shape: 11 cancelled at the first position,
a median of 1 and nothing cancelled in 11 of 25 runs at the middle, and nothing
cancelled in 25 of 25 at the last.

At the last position the group does not collapse in any useful sense: every
sibling runs to completion and the nursery returns `ChildFailed` at the end,
about 420 ms after a failure that happened at 11 ms. In the middle position the
runs where something *was* cancelled recorded it at 401-413 ms, and that is not
a cancellation landing either — it is the 400 ms of work ending by itself, with
the cancellation arriving after there was nothing left to stop.

What a nursery does still guarantee is the other half: every child is waited
for, and the failure is reported and not lost. What it does not do yet is
stop the remaining work. So do not rely on a sibling's failure to stop work
that is expensive, holds a resource, or has an effect outside the process —
have the work check a `Shared` flag itself where it must stop early.

**A bounded nursery starts brand-new children after a sibling has already
failed.** With `bounded_nursery(3)` over twelve 200 ms children and a failure
at 11 ms in the second, a median of 6.5 of the 10 remaining children were
*started* after the failure had been recorded, and the group did about 400 ms
of further work past it — 20 runs a side, both backends. The nursery admits,
spawns and runs to completion work it was told 11 ms in would never be wanted.

**The two fiber backends are distinguishable.** The reference says twice that a
program cannot tell which it has. It can. Under `KHORA_FIBERS=scheduler` a
fiber inside `clock.sleep` is woken by a cancellation and its sleep returns
early; under the default thread backend the sleep runs to completion. A
`clock.sleep(400)` cancelled at 50 ms stopped in 0-1 ms under the scheduler and
after 346-352 ms under threads, 25 runs a side — about a 350× difference in
wall clock on the same source, and 1898 ms against 1 ms at `sleep(2000)`. The
difference is not confined to timing: a program built on nurseries did a
different *amount of work* under the two backends, executing more of its jobs
in 4 of 20 scheduler runs than the thread backend ever did. The default cannot
be changed without notice while that is true.

Related, and true under both: **cancellation only lands where the fiber checks
for it** — at a `!` in a function that can raise, or a loop back-edge. A long
`sleep` is not a cancellation point, so work that must stop promptly has to be
chunked into a loop. The reference's "a blocked or suspended operation is made
runnable" describes the intent rather than the current thread backend.

## Cross-compilation and WebAssembly

LLVM object/module emission is further along than the complete runtime/link/sysroot/deployment path for every target. Only targets tested end to end are labeled supported.

WebAssembly also requires a host-appropriate standard-library/platform surface rather than reusing native filesystem and socket assumptions. Cloudflare Workers remains an experimental/planned deployment path rather than a supported production target.

## Stability

Khora has not reached 1.0. Source compatibility across arbitrary development revisions is not promised. Pin the toolchain version for applications where reproducible builds matter, and review migration notes when deliberately moving between incompatible releases.

[Compatibility and stability](/docs/reference/compatibility/) is the policy: what a `0.x` release promises, what counts as a breaking change, and the four things 1.0 is waiting for.

## Reporting a limitation

If the documentation says something should work and the compiler disagrees, treat that as a bug in either the implementation or the docs.

Every hand-written example on this site is compiled as a step of the project's build gate, and the generated API pages are checked against their declarations by `khora doc --check`. Two gaps remain. A hand-written fragment is *parsed* rather than type-checked unless it declares its own `module`, so an example can be syntactically valid and still mean the wrong thing — `List<String` with the bracket missing is a valid comparison. And the examples inside `///` doc comments, which become the generated pages, are not run at all. Where an example and the compiler disagree, reconcile them against the implementation rather than assuming either side is right.
