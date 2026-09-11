---
title: Known limitations
sidebar:
  order: 0
---

Khora is pre-1.0. This page exists so users can tell the difference between a
language rule, a supported feature, and unfinished work.

**The ones most likely to affect you:**

- [A bounded nursery runs `limit + 1` children](#what-a-nursery-actually-does),
  and a limit of zero means no limit at all.
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

## Editor tooling

`khora lsp` already provides compiler-backed diagnostics, hover, formatting, completion, signature help, go-to-definition, references, document/workspace symbols, semantic tokens, code actions, code lenses, and inlay hints.

Rename covers a declaration and every file that names it, including the import that brings the name into each file, and it renames the original rather than a file's own alias. It refuses two cases rather than applying a partial rename, each with a sentence saying why: a **trait member**, whose name belongs to the trait and to every impl of it, and a **constructor**, which has no recorded range to edit. Further refactoring operations are editor-tooling work.

See [Editor setup](/docs/getting-started/editor/) for the language-server command and client setup.

## Standard-library API docs

`khora doc` generates the checked-in standard-library API reference from compiler-resolved declarations plus `///` and `//!` documentation comments. `khora doc --check` is used to detect drift between the source declarations and generated pages.

Two important documentation-tooling gaps remain:

- Khora code blocks in API documentation are not yet compiled as documentation tests.
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

## The fiber scheduler

A fiber is an operating-system thread. The M:N scheduler — stackful coroutines on a worker pool — is built and is opt-in with `KHORA_FIBERS=scheduler`.

It is not the default for 0.1.0 for three reasons, and one of them is a gap rather than a preference:

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

## Union types

There is no way to write "an `Int` or a `String`" as the type of a value. `+` joins the failure types of a `raises` row and means nothing outside one; `T: Eq + Show` is the other meaning of the symbol, a trait bound, and works as it does in Rust.

The practical consequence is that `attempt` handles a body raising exactly one type. Use [`catch`](/docs/reference/failures/#handle-failures-with-catch) for a wider row — it matches per type and never has to name a combined type.

[The unions design note](https://github.com/codyspate/khoralang/blob/main/docs/design/unions.md) records what a union would mean, what it would cost, and why existentials are not part of the same question.

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
to make unnecessary. And a fiber that raised and was `wait`ed on rather than
`join`ed prints `khora: a fiber ended with an error nobody was waiting for` to
standard error at process exit, once per such fiber, with no way to suppress it
and no effect on the exit status. Since `wait` is the documented thing to use
after `cancel`, a supervisor that cancels children prints that line routinely.

`Channel` also has no `select` (waiting on the first of several) and no zero-capacity rendezvous. `Channel::bounded(0)` gets a capacity of one rather than a rendezvous, deliberately.

## What a nursery actually does

Four things about nurseries differ from what the [concurrency
reference](/docs/reference/concurrency/) describes. All were found by
measuring, and a program built on the prose will meet them as flakiness.

| what you write | what happens | what to do |
| --- | --- | --- |
| `bounded_nursery(4)` | 5 children run at once | subtract one when the limit is a real resource |
| `bounded_nursery(0)` | **no limit at all** | check a computed limit before passing it |
| a child fails | siblings usually keep running | have long work check a `Shared` flag itself |
| a child fails in a bounded nursery | new children still start | as above |

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
function that can raise, or a loop back-edge. A long `sleep` is not a
cancellation point, so work that must stop promptly has to be chunked into a
loop.

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

Every hand-written example on this site is compiled as a step of the project's build gate, and the generated API pages are checked against their declarations by `khora doc --check`. Two gaps remain. A hand-written fragment is *parsed* rather than type-checked unless it declares its own `module`, so an example can be syntactically valid and still mean the wrong thing — `List<String` with the bracket missing is a valid comparison. And the examples inside `///` doc comments, which become the generated pages, are not run at all. Where an example and the compiler disagree, reconcile them against the implementation rather than assuming either side is right.
