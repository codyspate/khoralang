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
export fn open(path: String) -> File
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
| Numeric kernels (BLAS and similar) | `std::ai` promises tensors; nobody hand-writes GEMM. |

Note what is *not* on that list: HTTP, JSON, and collections. Those are a few
weeks each and they would be better in Khora, because they can be generic over
their effects instead of being wrapped in something that is not.

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
