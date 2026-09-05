# A6 — where Khora's libraries come from

Non-negotiable 6 says a new language with no libraries loses to Go and Node
regardless of merit. That still stands and is not what this reconsiders. A6
was the *answer* — first-class Rust interop, consuming crates.io — and this
replaces it.

## Why not crates.io

A6's own text already concedes the technical half: *"Sharing a backend gives
nothing across a boundary — Rust has no stable ABI, and its traits, generics
and ownership map to nothing automatically."* Three further reasons, in the
order they matter.

### It does not skip the work it looks like it skips

This is the one that decides it. You cannot hand a Khora program a byte buffer
from a Rust crate if **Khora has no byte buffers**. The entire type universe
today is `Int`, `Bool`, `String`, `()`, ADTs and tuples: no fixed-width
integers, no arrays, no floats, no mutable field anywhere.

Every one of those is needed identically whether or not a single crate is ever
bound. Interop sits *above* the missing primitives, not instead of them — so it
buys no schedule at all on the part that is actually blocking, and the apparent
shortcut is the illusion that made it attractive.

### The mapping has no bottom

- **Lifetimes have no Khora counterpart.** `&'a str` is a borrow tied to a
  scope the language cannot express. Every borrow becomes a copy or a pin.
- **Traits are not typeclasses.** Associated types, blanket impls, and generics
  with bounds in trait position each need a translation, and some have none.
- **Half of crates.io is async on tokio.** Bridging means shipping a second
  runtime and blocking a fiber on a future — workable, and a permanent second
  scheduler inside a language whose own concurrency story is a selling point.
- **Every binding is a version, forever.** Against an ecosystem that moves.

### It argues against the language

If the answer to "how do I do X in Khora" is "use the Rust crate", then *"why
not just use Rust"* gets sharper rather than softer. Rust is one of the three
languages `docs/vision.md` says Khora has to be a serious candidate against.
An ecosystem strategy that makes the strongest competitor a dependency is
answering the wrong question.

## Decided

> **The foreign boundary is the C ABI. Khora's libraries are written in Khora,
> on a standard library that is deliberately larger than most, and a short list
> of things nobody should write twice are bound rather than reimplemented.**

Not a Rust boundary — a C one. Rust crates that export `extern "C"` are
reachable through it like anything else, which keeps the option without paying
for it.

### The boundary already exists

This is why it is nearly free. Every runtime call generated code makes is
already a C ABI crossing: `khora_alloc`, `khora_drop`, `khora_region_defer`,
`khora_fiber_spawn`. The contract is written down in `khora-rt`'s module
documentation, it has a test pinning the header layout, and it works. What is
missing is letting a *program* declare one, not inventing the mechanism.

The rule for what may cross was learned the hard way (errata 35): a tagged
return is a 16-byte aggregate, LLVM and rustc disagree about how one comes
back, and the disagreement is silent. So:

> **Only scalars and pointers cross.** Integers of a known width, floats,
> pointers, and a `String`'s bytes as pointer plus length. No aggregate is
> passed or returned by value. A Khora closure crosses only behind a
> trampoline, which is what `Backend::tagged_trampoline` already is.

### A foreign resource is a reference-counted value

The hardest problem in most FFIs — when does the file get closed — Khora has
already solved twice. A region is a counted object whose release runs its
finalizers; a fiber handle is a counted object whose release joins. Both use
runtime-provided drop glue, and both run on every path out including a raise.

A foreign handle is the same shape: a Khora object holding the pointer, with a
release that calls the foreign close. So an open file closes when its binding
ends — at the end of a block, at an early `return`, or with an error passing
through — and no new rule is needed to say so.

### The declaration is where effects are asserted

A foreign function is opaque, so the compiler cannot see what it does. Its
`with` and `raises` clauses are therefore a **declaration taken on trust**, the
way `unsafe` is a promise rather than a proof:

```
pub fn open(path: String) -> File
  with { fs: FileSystem }
  raises IoError;
```

Every *caller* is then checked normally. The capability discipline holds all
the way up the program, and the one place it is asserted rather than inferred
is the one place it has to be. That also gives D4 its teeth: what a program can
reach is what its foreign declarations say, and those are a short, auditable
list.

## What Khora writes, and what it binds

**Written in Khora**, because being idiomatic matters more than being
battle-tested and because these are what effect rows are *for*:

collections, strings and encoding, JSON, time, HTTP, logging, testing,
everything a normal program touches.

**Bound**, in four categories, each with a reason that is not "it would take a
while":

| | Why |
| --- | --- |
| TLS, crypto | Correctness is a specialist matter and a bug is a breach. |
| Compression | Decades of tuned code behind an API with nothing to improve. |
| The operating system | It *is* the C ABI. There is no alternative. |
| Numeric kernels (BLAS and similar) | Nobody hand-writes GEMM. There is no tensor type in the tree to promise them against, though: `std::ai` declared one and it was deleted unused -- `docs/design/std-surface.md`. |
| An embedded database | SQLite, compiled into the runtime the way `rustls` is. A storage engine is a specialist matter for the same reason crypto is, and a program that cannot persist anything is a demo. |

**A database goes in `std` only if it is embedded.** SQLite belongs there: it is
a file format and a library, it has no wire protocol, no authentication and no
server to be a version behind. Postgres is a package — it brings connection
pooling, an authentication mechanism that is itself crypto (SCRAM-SHA-256), and
a compatibility surface that moves on somebody else's schedule. Those are
decisions an application should be able to pin, which is what a package is for.

Note also which half of that is *written* rather than bound: a wire protocol is
a protocol, so a Postgres driver's framing belongs in Khora beside HTTP and
JSON, with only the crypto bound.

Note what is *not* on that list: HTTP, JSON, and collections. Those are a few
weeks each and they would be better in Khora, because they can be generic over
their effects instead of being wrapped in something that is not.

## What `std` reserves, and what it does not

A standard library that ships a web framework can do it two ways. It can be the
framework, and every alternative starts from a socket; or it can be the layer
underneath one, and the framework it happens to ship is the first consumer of a
public API. `std::net::http` is the second, and it is worth writing down because
it is the difference between an ecosystem with one HTTP library and an ecosystem
with several.

Three layers, and only the top one is `Router`'s:

- **Codec.** `parse`, `Response::rendered`, `percent_decode`, `matches` for
  `:name` path patterns, and the `Request`/`Response`/`Method`/`Header` types.
- **Connection.** `Connection` and `Incoming`: reading until a request is whole,
  `Content-Length` framing, holding what a pipelining client sent early,
  refusing one that will not fit, and the `Connection` header. Plus
  `request_length` for anyone who wants their own buffer strategy, and
  `Transport` — three closures — so a framework can serve over a socket, over
  TLS, or over an in-memory pipe in its own tests.
- **Router.** Optional, and written against the two below it with no privileged
  access to either.

**The middle layer is the one that matters.** A router is a matter of taste and
a weekend; framing a request correctly is neither. It is also the part that
fails in production rather than in testing — a truncated request at a packet
boundary, two requests read as one — so leaving each framework to derive it
again is how an ecosystem gets three subtly broken HTTP servers. Splitting it
out immediately found a bug `Router` had carried: a pipelined second request was
read as "the client has gone" and dropped, which no test using `curl` could see,
because `curl` waits for each answer before sending the next.

**Concurrency is not in any of the three.** Nothing below `Router`'s accept loop
spawns; `Router` gives each connection a fiber and that is a decision of the
accept loop. A synchronous server, or one with a bounded pool, is written
against exactly the same `Connection`. `crates/khora-codegen-llvm/tests/http_layers.rs`
is a middleware-chain framework with no `Router`, no `Fiber` and no nursery,
kept in the suite so the layering stays true rather than merely being intended.

What `std` does still reserve is the *shape* of concurrency itself — there is
one model, and no non-blocking socket API to build a competing event loop on.
That is deliberate and is argued in Phase 11 of `docs/roadmap.md`.

## Applying the rule to what is not written yet

The section above settled `std::net::http` by asking where the middle layer is.
Phase 12 of `docs/roadmap.md` proposes several more things, and the same
question answers all of them — including two where the obvious answer is wrong.

**Most of them are not `std`'s business at all**, which is worth saying first so
the rule is not over-applied. Cross-compilation, WebAssembly, debug information,
a C export surface, compile times and what a trap does to a process are
properties of the compiler and the runtime. The rule only bites where something
could plausibly be a library.

| | middle layer? | where it goes |
| --- | --- | --- |
| `Decimal` | it *is* the vocabulary; two of them cannot exchange a price | `std`, and partly the language |
| civil date and time types | vocabulary | `std` |
| the IANA time zone database | a dataset, released several times a year | **package, or the system** |
| span and attribute types, `traceparent` | vocabulary; nothing composes without agreement | `std` |
| propagation across spawn, steal, wake and cancel | fails in production only; the scheduler's, not a library's | `std` |
| OTLP, Datadog, Prometheus exporters | a vendor's protocol and release cadence | package |
| the `Db` capability and its transaction contract | a transaction that leaks on cancellation | `std` |
| the SQLite engine | no middle layer at all | **first-party package** |
| Postgres | a wire protocol somebody else versions | package |

### Two the obvious answer gets wrong

**Time zones are not standard library material, and civil dates are.** The
types — a date, a time of day, a zoned instant, an offset — are vocabulary, and
two libraries that disagree about what a date is cannot exchange one. But the
tzdb is a *dataset*: Egypt changes its rules, IANA cuts a release, and every
binary already shipped is wrong about the future. Nothing behind this
document's compatibility promise can be updated four times a year. `std` owns
the types and the interface to a zone provider; the data comes from a package
or from the host.

**SQLite in `std` would be the mistake `std::net::http` avoided.** It is
tempting because it is a file rather than a service — no protocol, no auth, no
TLS — but that is exactly the argument against it. There is no framing to get
right, no handshake to get wrong, nothing that fails at a packet boundary. It
is all top layer, plus a quarter of a million lines of C and a question about
what a virtual file system means in an isolate. Under this document's rule that
is a framework, and it belongs in a first-party package where it can version on
its own.

The middle layer for databases exists, but it is not the engine. **It is what a
transaction does when its fiber is cancelled.** A transaction that returns
without rolling back, holding a connection and its locks, is the truncated
request at a packet boundary of this subject: it fails in production, never in
testing, and every package would answer it differently. It is also Khora's to
answer rather than the engine's, because cancellation semantics are the
language's. So `std` owns the `Db` capability, the row and value types, and
that contract — and SQLite, Postgres, D1 and an in-memory double are handlers.

`docs/design/observability.md` makes the same split for tracing and says why
the runtime forces `std`'s hand there.

## What this gives up, plainly

crates.io. The hedge A6 was making — that a new language without libraries
loses on merit-independent grounds — is real, and this does not make it go
away. It moves the risk:

- **Before:** a Rust boundary to build and maintain forever, and an ecosystem
  story that points at someone else's ecosystem.
- **Now:** more library code to write before anyone can use the language, and a
  std that has to be good rather than merely present.

That is the trade. It is taken because the first risk is permanent and the
second is finite, and because the work the second one names was on the critical
path anyway.

## What D8 becomes

Most of it dissolves. There is no ownership mapping, no trait mapping, no async
bridging. What remains is small and answerable:

1. Exactly which types may cross, and their layout. Mostly written already, in
   `khora-rt`'s module documentation.
2. How a foreign resource's lifetime is tied to a Khora binding. Answered above
   in principle; needs the declaration syntax.
3. What a `callback` looks like — a Khora function a foreign library calls back
   into. The trampoline pattern covers it; the question is what a program
   writes.
