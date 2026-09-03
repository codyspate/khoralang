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

Rename is intentionally narrower than the rest of the navigation surface: it currently renames locals only and refuses edits it cannot prove complete rather than applying a partial rename. Broader symbol rename and additional refactoring operations remain editor-tooling work.

See [Editor setup](/docs/getting-started/editor/) for the language-server command and client setup.

## Standard-library API docs

`khora doc` generates the checked-in standard-library API reference from compiler-resolved declarations plus `///` and `//!` documentation comments. `khora doc --check` is used to detect drift between the source declarations and generated pages.

Two important documentation-tooling gaps remain:

- Khora code blocks in API documentation are not yet compiled as documentation tests.
- Generated signatures name referenced types but do not yet cross-link those type names to their API pages.

See the [Standard library](/docs/stdlib/) entry point for the generated reference.

## HTTP surface

The reference HTTP implementation is intentionally not presented as every protocol feature a mature web framework might provide. The shipping documentation should be treated as the supported surface; unlisted body encodings, upgrades, protocol versions, or framework conveniences should not be assumed merely because the core server/client path exists.

## The fiber scheduler

A fiber is an operating-system thread. The M:N scheduler — stackful coroutines on a worker pool — is built and is opt-in with `KHORA_FIBERS=scheduler`.

It is not the default for 0.1.0 for three reasons, and one of them is a gap rather than a preference:

- Threads are faster at the connection counts a service runs at.
- The scheduler exists for fiber **density**, and that claim is measured on Windows only. Linux caps `vm.max_map_count` at 65530 and guard pages split mappings, so the "100,000 waiting fibers" figure has not been reproduced on the platform most deployments use.
- It is the less-exercised path, and therefore the likelier home of the next runtime bug.

A program cannot observe which implementation it has, so the default may change in a later release without any source change.

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

The practical consequence is that `attempt` handles a body raising exactly one type. Use [`catch`](/docs/reference/failures/#catch) for a wider row — it matches per type and never has to name a combined type.

[The unions design note](https://github.com/codyspate/khoralang/blob/main/docs/design/unions.md) records what a union would mean, what it would cost, and why existentials are not part of the same question.

## Concurrency combinators

A fiber carries its answer and its failure row — `Fiber<A, 'er>`, with `join` re-raising what the child raised — and `Clock` can `sleep`. The combinators built on top of those do not exist yet: there is no `timeout`, no `race`, and no bounded parallel map.

**And writing one by hand does not currently work either.** A parent blocked on `Channel::receive` serialises the fibers it spawned — two 2000 ms fibers take 4.8 seconds that way against 2.8 when the parent waits on their handles — so the fan-in a race needs is decided by spawn order rather than by time. Waiting on handles is concurrent and is what to build on until that is fixed.

`Channel` also has no `select` (waiting on the first of several) and no zero-capacity rendezvous. `Channel::bounded(0)` gets a capacity of one rather than a rendezvous, deliberately.

## What a nursery actually does

Three things about nurseries are not yet what the [concurrency
reference](/docs/reference/concurrency/) describes. All three were found by
measuring rather than by reading, and a reader who builds on the prose will
meet them as flakiness.

**A bounded nursery admits `limit + 1` children.** `bounded_nursery(4)` runs
five at once. `Fiber::spawn` *starts* the child, and `nursery.adopt` is what
blocks when the nursery is full — so by the time the bound is applied, the work
is already running. Treat the number as a bound on children the nursery is
holding, not on work in flight, and subtract one where the limit is a real
resource such as a connection pool.

**A child's failure is noticed in adoption order, so cancelling the siblings
can be arbitrarily late.** A nursery reaps handles oldest-first, so a failure in
the third child is invisible until the first two have finished. With twelve
2-second jobs and a failure at 10 ms, the siblings were cancelled after 21 ms
when the doomed child was adopted first and after 2044 ms when it was adopted
third — and how many siblings were still running to cancel varied between runs.
The group does collapse, and every child is still waited for; *when* it
collapses is not yet a promise.

**The two fiber backends are distinguishable.** The reference says twice that a
program cannot tell which it has. It can: under `KHORA_FIBERS=scheduler` a
fiber inside `clock.sleep` is woken by a cancellation and its sleep returns
early, and under the default thread backend the sleep runs to completion. That
is a 180× difference in wall clock on the same source, and it means the default
cannot be changed without notice yet.

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
