---
title: Known limitations
sidebar:
  order: 0
---

Khora is pre-1.0. This page exists so users can tell the difference between a
language rule, a supported feature, and unfinished work.

**The ones most likely to affect you:**

- [What a signal does](#what-a-signal-does-and-the-three-shapes-it-does-not-reach)
  — `SIGTERM` and `SIGINT` unwind the program and run its finalizers, but an
  infallible `main`, a blocking `connect_to` and Windows are not covered.
- [A bounded nursery runs `limit + 1` children](#what-a-nursery-actually-does),
  and a limit of zero means no limit at all — so **a bound of exactly one
  cannot be written**, which is the value a "one at a time" flag wants most.
- [A child's failure usually does not cancel its siblings](#a-childs-failure-usually-cancels-no-siblings).
- [A cancelled fiber waiting on a child finishes waiting, then runs the rest of
  its body](#a-cancelled-fiber-parked-in-fiberwait-keeps-going).
- [The two fiber backends behave differently](#the-two-fiber-backends-are-distinguishable)
  under cancellation.
- [There is no `timeout` or `race`](#concurrency-combinators).
- [Inbound connections are not permissioned](#inbound-connections-are-not-permissioned) —
  the manifest governs outbound only.

## Target coverage

Khora has versioned toolchain artifacts and installers for the platforms the
project releases. The normal path is the installer documented in
[Installation](/docs/getting-started/installation/), not compiling the compiler
from source.

A target is only called supported when the compiler, runtime, linker/sysroot,
packaging, CI and deployment path work end to end. [Supported
targets](/docs/deployment/supported-targets/) lists the ones that do.

## Recursion depth and very large lists

Khora does not guarantee tail-call optimisation, so a function that recurses once per element uses one stack frame per element. Running out of stack ends the program; it reports

```
khora: the stack ran out
```

on standard error and exits with the platform's stack-overflow status.

Every traversal in `std::core`'s `List` is written as a loop rather than as recursion — `length`, `fold`, `reverse`, `filter`, `take`, `drop`, `any`, `all`, `find`, `contains`, `zip`, `flat_map`, `sum`, and the `merge` inside `sort` — so walking a list of any size is safe. `List::sort` recurses only to divide, which is about `log2(n)` deep.

String operations are loops too. `split`, `join` and `repeat` handle inputs of
any size, and `join` is linear rather than quadratic in the length of its
result.

Releasing a value costs no stack either: reference counting frees a value's children through a queue rather than by recursing, so letting go of a long list is a loop like walking one. A million-element `List` sorts.

What is left is ordinary recursion that somebody writes. A function that calls itself once per element of its input will use a frame per element, and no analysis in the compiler turns that into a loop.

`Array<A>` and `Vector<A>` are the better shape for a large indexed collection; a list is for building front-to-back and walking once.

## Package ecosystem

Dependencies can be pinned reproducibly to git revisions, but there is not yet a public package registry or broad third-party ecosystem.

**Three packages are maintained in the Khora repository**, and they are what "the ecosystem" means today: [`postgres`](/docs/packages/postgres/), `ai` — the effect a caller names when it wants model inference — and `otlp`, an exporter for `std::trace` over OTLP/HTTP JSON. [Packages](/docs/packages/) lists all three and gives the manifest line each is depended on with. Beyond those there is nothing to install: no registry to search and no third-party publishing.

**One database driver is published: `postgres`.** `std::db` defines `Db`, transaction semantics and cancellation behaviour, and [`packages/postgres`](/docs/packages/postgres/) satisfies that interface — it speaks the wire protocol directly, authenticates with `scram-sha-256`, and supplies the `Db` handler. Depend on it with a git revision and a `subdir`; there is no registry yet. **SQLite and D1 have no driver**, and a program that needs one writes its own handler — `Db` is a record of closures, so that is a day's work and a test double is a few lines — over a native client it links with [`build.link`](/docs/reference/manifest/#build--what-to-produce).

**A dependency cannot link a native library on your behalf.** `build.link` is read from the root package's manifest and nowhere else, so a package that ships an archive and declares `extern fn` against it cannot put a flag on your link line — a transitive package adding a native library to your build would be a supply-chain change with no signal at the place that would have to consent. The package does the work and documents one line for you to add, which means every native library a program links can be read off its own manifest. [Foreign function interface](/docs/reference/ffi/#link-against-a-native-library) has the shape.

## Editor tooling

`khora lsp` already provides compiler-backed diagnostics, hover, formatting, completion, signature help, go-to-definition, references, document/workspace symbols, semantic tokens, code actions, code lenses, and inlay hints.

Rename covers a declaration and every file that names it, including the import that brings the name into each file, and it renames the original rather than a file's own alias. It refuses two cases rather than applying a partial rename, each with a sentence saying why: a **trait member**, whose name belongs to the trait and to every impl of it, and a **constructor**, which has no recorded range to edit. Further refactoring operations are editor-tooling work.

See [Editor setup](/docs/getting-started/editor/) for the language-server command and client setup.

## Standard-library API docs

`khora doc` generates the checked-in standard-library API reference from compiler-resolved declarations plus `///` and `//!` documentation comments. `khora doc --check` is used to detect drift between the source declarations and generated pages.

Two important documentation-tooling gaps remain:

- An API code block is only type-checked if it declares its own `module`. One of the 1,023 blocks on the generated pages does, and `scripts/check-api-programs.sh` checks it on every CI run; the rest are parsed as fragments and none are executed.
- Generated signatures name referenced types but do not yet cross-link those type names to their API pages.

See the [Standard library](/docs/stdlib/) entry point for the generated reference.

## HTTP surface

Assume nothing beyond what this section lists.

**The verbs are `GET`, `POST`, `PUT`, `PATCH` and `DELETE`, routed to
handlers, plus `HEAD` and `OPTIONS`, which the router answers from what is
mounted unless you mount a handler for them.** Anything else — `TRACE`,
`CONNECT`, an extension method — is answered `400` and the connection closed,
because `Method::of` does not name it and an unparsed request line is a
malformed one as far as the reader is concerned. The failure is silent from the
client's side: a `400` to a verb the server does not know reads as the client's
mistake.

**A request is capped at 8 KB by default, headers and body together, and the
cap is configurable.** Past it the server answers `413` before parsing
anything, so the handler never runs. The number answers "how much may an
unauthenticated client make a server hold" rather than "how large can a
request be" — nothing in the parser recurses per byte or per line, and a
39,808-byte request carrying 2,001 headers parses when the limit admits it.
`Router::holding` sets another; the buffer is allocated once at that size per
connection, so it multiplies by the connection bound below when deciding what
a full server costs. There is no multipart decoding, and a body must be UTF-8
text. **Chunked transfer is read but not written:** `HttpClient` accepts a
response framed by `Transfer-Encoding: chunked` and hands the handler the
de-chunked body, so a service that answers that way can be called; the server
never writes a chunked response. That is the split real traffic has — a
response of unknown length is ordinary and a request of unknown length is not.

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

## Inbound connections are not permissioned

**`[permissions] network` governs outbound connections only.** The grant list is
consulted in exactly one place in the standard library — inside
`HttpClient::send`, after the URL is parsed and before anything is dialled — and
what it decides is whether that host may be reached. Nothing on the server side
consults it: `Router::listen`, `Router::listen_quietly`, `Router::listen_tls`
and the `listen_on` under them bind and serve without asking. A program with
`default = "deny"` and `network = []` opens whatever port it is given and
answers requests on it.

So do not read a `network` grant as an authorisation to listen, and do not read
its absence as a refusal to. `network = ["127.0.0.1:8787"]` written to allow a
server to bind 8787 does nothing at all — it neither permits the bind, which
needed no permission, nor restricts it. The mistake is easy to make and leaves
no trace: the program works, and the line that was supposed to be holding it
back is not.

The reason it is this way rather than fixed is that the two directions do not
share a vocabulary. `network` is a list of hosts a program may *reach*. A port
it may *bind* is not a host it reaches, so extending the same list to cover
listening would produce a control whose meaning could not be written down —
`api.example.com:443` would have to mean one thing as a destination and another
as a local port, and a permission that cannot be stated is a permission nobody
can audit. Naming inbound authority properly needs a grant of its own, and what
that grant ranges over — ports, interfaces, both — is an open design question
rather than a fix waiting to be typed. Until there is one, a program's ability
to listen is bounded by the operating system and by whatever runs it, not by its
manifest.

## What a signal does, and the three shapes it does not reach

`SIGTERM` and `SIGINT` become a **cancellation at the root of the program**.
There is no new API and nothing to spell: a program observes a signal as the
cancellation it already knows how to observe, so nursery cancellation runs,
scoped finalizers run, a `std::db` transaction sends its `ROLLBACK`, and the
process exits **130**. `Ctrl-C` is `SIGINT` and behaves the same way.

**The second signal is forceful.** The runtime restores the default
disposition and re-raises, so the process dies the way the platform says —
a real `WIFSIGNALED`, not an `exit(143)` that prints the same number. The
operator holds the deadline: there is no grace period in the language, because
a runtime timer that disagreed with `TimeoutStopSec` or
`terminationGracePeriodSeconds` would be the one number nobody configured.

Measured, on a program with a scoped finalizer in a fallible loop:

```
kill -TERM -> exit 130; the finalizer ran
kill -TERM twice -> killed by signal 15 mid-finalizer
```

### What this does not cover

**An infallible `main` dies rather than unwinding.** A cancellation travels the
error channel, so a `main` with no `raises` row has nowhere for one to go. The
runtime notices this and falls back to the old behaviour — default disposition,
re-raised at once — so such a program still answers `kill`, at wait-status 143
with no finalizers. It loses nothing it previously had; it gains nothing
either. Give `main` a `raises` row to get the graceful path.

A compiler warning for this shape — a `main` with no `raises` row and a `loop`
or a `listen` in it — is decidable from the signature and is **not built**. The
runtime fallback was chosen over it because it helps the program already
running rather than the author who reads warnings; the warning is still worth
having and is not yet written.

**`Fiber::wait` cannot be called from a function with no failure channel.**
This is the same rule met at compile time rather than at `kill` time, and it is
sharper: `wait` is a cancellation point, a cancellation point needs a channel
to travel, and a function with no `raises` row has none. So

```khora
fn main() -> Int {
  let hand = Fiber::spawn(fn () => body());
  Fiber::wait(hand)   // refused: this call can leave the function
}
```

is rejected even when `body` raises nothing at all — the empty row is still a
row, and the diagnostics say so from three directions at once (`!` reports the
missing clause, a `catch` reports that nothing raises, an annotation reports
`expected Oops, found {}`). Give the waited-on function a `raises` row and
catch it at the top.

`Fiber::join` has always behaved this way, so this is `wait` joining a rule
that already existed rather than a new one. It is still a real cost: `wait` on
a provably-infallible child needs no channel in principle, and requiring one
is the implementation showing through. Making the empty row callable without
`!` is the fix, and it is a type-system change rather than a runtime one.

**A nursery shutdown exits 0, so a supervisor is told it succeeded.** When the
root's body is a nursery, the children are cancelled and their finalizers run —
that part works — but the nursery absorbs its children's cancellations, returns
normally, and `main` runs on to its own `0`. Measured on both backends:

```
two-child nursery + SIGTERM -> both finalizers ran, exit 0
scoped finalizer  + SIGTERM -> finalizer ran,     exit 130
```

The unwinding is right and the status is wrong, which is the more dangerous
half: a supervisor reading the exit status of a shutdown it signalled cannot
tell it from a clean finish, and a `restart: on-failure` policy will not fire.
The fix is for a nursery to distinguish a cancellation it absorbed from a child
that merely finished; until then, do not read the exit status of a nursery-
rooted program as a shutdown outcome.

**A long `clock.sleep` delays or survives the shutdown, and the two backends
differ.** Measured against an eight-second sleep in a child fiber, one
`SIGTERM`:

```
thread backend     stopped after 6034 ms; the sleep completed normally
scheduler backend  stopped after 3 ms; the sleep still returned normally
```

Under the default thread backend the sleep is simply not interrupted, so
shutdown waits it out. Under the scheduler the fiber *is* woken — but the wake
returns from `sleep` **normally rather than raising**, so the statement after
the sleep runs before the fiber stops at its next cancellation point. A
cancelled fiber runs one more step of the work it was told to abandon.

This is the most reachable of the three gaps on this page: `clock.sleep` is in
every poll loop and every retry backoff. Chunk a long sleep into a loop of
short ones if shutdown latency matters, which makes the loop back-edge the
cancellation point.

**A blocking `connect_to` is not a cancellation point.** `khora_net_connect` is
a blocking `connect(2)` on the worker, so a fiber inside one reaches no
cancellation point until the kernel gives up — minutes, on an unroutable host.
`accept` and `recv` are fine: they are reactor-driven, and an idle
`Router::listen` server stops in about ten milliseconds. The fix is a
non-blocking connect driven by the reactor, which the roadmap schedules for
Phase 13.

**Windows has none of this.** Windows has no `SIGTERM`, and no way for an
arbitrary process to ask another to stop: `TerminateProcess` is `SIGKILL` with
no notice and nothing to observe. The console events (`CTRL_C_EVENT` and its
two siblings) are not wired up in this release.

So a program that must not lose work should still be crash-only — durable state
advancing by one atomic append or rename — because `SIGKILL`, a power cut and
the three cases above all remain. What has changed is that the ordinary deploy
is no longer one of them. [Running on Linux](/docs/deployment/linux/) and
[Containers](/docs/deployment/containers/) say this in the setting where it
bites.

## The fiber scheduler

A fiber is an operating-system thread. The M:N scheduler — stackful coroutines on a worker pool — is built and is opt-in with `KHORA_FIBERS=scheduler`.

It is not the default in 0.2.0, and is still not the default in the compiler this page describes, for three reasons, and one of them is a gap rather than a preference:

- Threads are faster at the connection counts a service runs at.
- The scheduler exists for fiber **density**, and that claim is measured on Windows only. Linux caps `vm.max_map_count` at 65530 and guard pages split mappings, so the "100,000 waiting fibers" figure has not been reproduced on the platform most deployments use.
- It is the less-exercised path, and therefore the likelier home of the next runtime bug.

The two backends are distinguishable under cancellation — see [The two fiber
backends are distinguishable](#the-two-fiber-backends-are-distinguishable)
below — so the default cannot change without a breaking-change note.

## Characters and strings

A `String` is UTF-8 and is indexed in **bytes**. `String::slice` stops the program if a cut lands inside a character, so ask first:

```khora
let safe = String::slice(text, 0, String::next_boundary(text, 20));
```

`is_char_boundary`, `next_boundary`, `previous_boundary`, `char_at`, `chars` and `char_length` are the character-level API; `byte_length` is the constant-time one and `char_length` walks.

The character predicates — `Char::is_digit`, `is_alpha`, `is_whitespace`, `to_upper`, `to_lower` — are **ASCII only** and say so in their own documentation. Unicode case mapping and the full `Nd` category are not in `std`, deliberately: they need tables that would double its size, and a library is the right place for them.

## Anonymous union types

There is no way to write "an `Int` or a `String`" **inline**, as the type of a value, without declaring anything.

A named [variant type](/docs/reference/types/#variant-types) — a discriminated union, in other languages' words — is how "one of several" is expressed, and it is exhaustively checked:

```khora
pub type Answer =
  | Number(Int)
  | Text(String);
```

What is missing is the anonymous form. Declaring `Answer` is the cost, and the compiler's exhaustiveness checking is what it buys.

`+` joins the failure types of a `raises` row and means nothing outside one; `T: Eq + Show` is the other meaning of the symbol, a trait bound, and works as it does in Rust. Writing `Int + String` in a value's type is refused by name rather than by a parse error.

The practical consequence is that `attempt` handles a body raising exactly one type. Use [`catch`](/docs/reference/failures/#handle-failures-with-catch) for a wider row — it matches per type and never has to name a combined type.

[The unions design note](https://github.com/codyspate/khoralang/blob/main/docs/design/unions.md) records what an anonymous union would mean, what it would cost, and why existentials are not part of the same question.

## A cancelled fiber parked in `Fiber::wait` keeps going

`Fiber::wait` and `Fiber::join` are not cancellation points and carry no
failure row, so a fiber parked in one cannot be stopped until the child it is
waiting on ends by itself.

Cancelling a parent inside `Fiber::wait` on a 2000 ms child landed after
1939-1984 ms, 20 runs out of 20 on both backends, against 0-5 ms for the same
parent with no child to wait on.

**Worse, the parent then ran the rest of its body** — the statement after the
wait executed in 20 of 20 runs — so a fiber that was cancelled can still
publish a result. A supervisor that cancels a worker and assumes it stopped is
wrong on both counts: it did not stop promptly, and it did not stop.

Until `wait` and `join` carry a `raises` row of the kind `Channel::receive`
already has, a parent that must stop promptly cannot be waiting on a child when
the cancellation arrives.

## Concurrency combinators

A fiber carries its answer and its failure row — `Fiber<A, 'er>`, with `join`
re-raising what the child raised — and `Clock` can `sleep`. The combinators
built on top of those do not exist: there is no `timeout`, no `race`, and no
bounded parallel map.

**Channel fan-in is concurrent.** Two 2000 ms fibers take about 2.1 seconds
whether the parent reads their results off a `Channel::bounded(4)`, joins both
handles, or uses `join_all` — measured three runs per backend on x86_64 Linux,
and indistinguishable under `KHORA_FIBERS=scheduler`. At 500 ms per fiber over
25 runs the three are 518, 514 and 516 ms.

**What blocks a hand-written `race` or `timeout` is the wait above.** A `race`
built out of `spawn` plus a wait is bounded by its *slowest* branch rather than
its fastest, and a deadline built that way reports a cancellation the run then
ignores.

Two smaller things a supervisor meets on the way. `Fiber::wait` tells you
nothing about how the fiber ended — there is no status and no
`Option<Result<..>>` — so the only way to find out is a `Shared` cell the child
writes before it fails, which is the sort of thing the failure row was supposed
to make unnecessary. And a fiber that raised without anybody taking its answer
prints `khora: a fiber ended with an error nobody was waiting for` to standard
error, once per such fiber, with no way to suppress it and no effect on the
exit status.

**The line is written when that fiber ends, not at process exit**, and
neither `Fiber::wait` nor `Fiber::join` changes whether it appears: the fiber
writes it itself the moment it stores its outcome, which is before any joiner
can have taken that outcome. So **a failure the program joins and handles is
reported too**, and the message's own wording is wrong about that case —
somebody was waiting. What it reliably means is narrower than it says: *a
fiber ended in a failure*.

The timing is the part worth using. The line sits at the point in the output
where the failure happened, so a line among a program's first few lines means
something failed during startup — a listener that could not bind is the usual
one.

**A cancellation is excluded from it.** The
runtime emits that line only for a fiber that ended in a *failure*; a
cancelled fiber is silent, so a supervisor that cancels children does not
print it — it prints it only when a child failed on its own. That is what
keeps the line worth reading: it is not the ordinary noise of a shutdown, and
seeing one means a fiber somewhere raised.

`Channel` also has no `select` (waiting on the first of several) and no zero-capacity rendezvous. `Channel::bounded(0)` gets a capacity of one rather than a rendezvous, deliberately.

## What a nursery actually does

Four things about nurseries differ from what the [concurrency
reference](/docs/reference/concurrency/) describes. All were found by
measuring, and a program built on the prose will meet them as flakiness.

| what you write | what happens | what to do |
| --- | --- | --- |
| `bounded_nursery(4)` | 5 children run at once | subtract one when the limit is a real resource |
| `bounded_nursery(0)` | **no limit at all** | check a computed limit before passing it |
| a bound of exactly 1 | **not expressible** — `bounded_nursery(0)` is unbounded, `bounded_nursery(1)` admits 2 | accept 2, or guard the work with a `Shared` flag of your own |
| a child fails | siblings usually keep running | have long work check a `Shared` flag itself |
| a child fails in a bounded nursery | new children still start | as above |
| `Fiber::cancel` on a fiber whose body is a nursery | **returns** — it is the following `Fiber::wait` that never does, if a child has no cancellation point | cancel through a `Shared` flag the children read |

A nursery does still guarantee the other half: every child is waited for, and a
failure is reported rather than lost.

The measurements below are `khora 0.2.0 (b16417c)` on x86_64 Linux, 20–25 runs
per backend, under both the default thread backend and `KHORA_FIBERS=scheduler`.

### The bound is on children held, not work in flight

`bounded_nursery(4)` runs five at once — the peak was exactly `limit + 1` on
every run of both backends at limits of 1, 2, 4 and 8. `Fiber::spawn` *starts*
the child and `nursery.adopt` is what blocks when the nursery is full, so by
the time the bound applies the work is already running.

[Bounded concurrency](/docs/cookbook/bounded-concurrency/) is built on this
number: `bounded_nursery(64, ..)` there is 65 live children.

### A limit of zero or less means no limit

`bounded_nursery(0, ..)` is how the unbounded `nursery` is built, so zero is
deliberate: 200 children ran at once under a limit of 0, and a negative limit
behaves the same way.

It is still a trap, because `Channel::bounded` does the opposite — it clamps a
capacity below one *up* to one. A limit computed from configuration that comes
out zero removes the bound rather than failing.

### A child's failure usually cancels no siblings

**Do not rely on a sibling's failure to stop work that is expensive, holds a
resource, or has an effect outside the process.**

A nursery reaps handles oldest-first, so a failure is invisible until every
child adopted before it has finished — and by then there may be nothing left to
cancel. Twelve children of 400 ms, one raising after 10 ms, 25 runs a side:

| the doomed child was adopted | siblings cancelled, of 11 | runs where **nothing** was cancelled |
| --- | --- | --- |
| first | 11, after 2-8 ms | 0 of 25 |
| in the middle (6th of 12) | 0 (median; 0-5) | 13 of 25 |
| last | 0 | 25 of 25 |

`KHORA_FIBERS=scheduler` is the same shape. At the last position the group does
not collapse in any useful sense: every sibling runs to completion and the
nursery returns `ChildFailed` about 420 ms after a failure that happened at
11 ms.

### New children start after a sibling has failed

With `bounded_nursery(3)` over twelve 200 ms children and a failure at 11 ms in
the second, a median of 6.5 of the 10 remaining children were *started* after
the failure was recorded, and the group did about 400 ms of further work past
it.

## The two fiber backends are distinguishable

A program can tell which backend it has. Under `KHORA_FIBERS=scheduler` a fiber
inside `clock.sleep` is woken by a cancellation and its sleep returns early;
under the default thread backend the sleep runs to completion.

A `clock.sleep(400)` cancelled at 50 ms stopped in 0-1 ms under the scheduler
and after 346-352 ms under threads — about a 350× difference on the same
source, and 1898 ms against 1 ms at `sleep(2000)`. It is not only timing: a
program built on nurseries executed more of its jobs in 4 of 20 scheduler runs
than the thread backend ever did.

**The default cannot change without a breaking-change note while that is true.**

**Cancellation only lands where the fiber checks for it** — at a `!` in a
function that can raise, or a loop back-edge. **Under the default thread
backend a long `sleep` is not one of those points**, so work that must stop
promptly has to be chunked into a loop; under the scheduler backend the sleep
is woken and returns early, as above. Write for the thread backend: it is the
default and the pessimistic case.

Either way, the statements between the wake and the *next* cancellation point
still run. Cancellation unwinds at a point, not between arbitrary instructions
— [Concurrency](/docs/reference/concurrency/) has the model.

## Cross-compilation and WebAssembly

The compiler can emit an object for a triple it cannot link, and
[Supported targets](/docs/deployment/supported-targets/) lists which triples
have been carried all the way to a running binary. Only those are called
supported.

WebAssembly needs more than a triple: a Worker or a browser has no filesystem
and no sockets, so `std` would need a platform surface shaped for the host.
Cloudflare Workers in particular is **not** an experimental target — none of
the pieces exist, and [that page](/docs/deployment/cloudflare/) says what they
would be.

## Stability

Khora has not reached 1.0. Source compatibility across arbitrary development revisions is not promised. Pin the toolchain version for applications where reproducible builds matter, and review migration notes when deliberately moving between incompatible releases.

[Compatibility and stability](/docs/reference/compatibility/) is the policy: what a `0.x` release promises, what counts as a breaking change, and the four things 1.0 is waiting for.

## Reporting a limitation

If the documentation says something should work and the compiler disagrees, treat that as a bug in either the implementation or the docs.

Every hand-written example on this site is compiled as a step of the project's build gate, and the generated API pages are checked against their declarations by `khora doc --check`. Two gaps remain. A hand-written fragment is *parsed* rather than type-checked unless it declares its own `module`, so an example can be syntactically valid and still mean the wrong thing — `List<String` with the bracket missing is a valid comparison. And the examples inside `///` doc comments, which become the generated API pages, follow the same rule: one of them declares a `module` and is type-checked by `scripts/check-api-programs.sh` in CI, and the rest are parsed. None are executed, so an example that compiles can still print something other than what it says it prints. Where an example and the compiler disagree, reconcile them against the implementation rather than assuming either side is right.
