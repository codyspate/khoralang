# Calling Khora from C

Roadmap 12.6. `docs/design/ffi.md` settles how Khora calls out; this is the
other direction, and it is not symmetric.

**Nothing here is built.** This is the argument that has to be settled first,
because two of its three questions are answered by facts already in the tree
and the third is a decision somebody has to take deliberately. The one place I
will not decide alone is marked as such at the end.

## Why it is worth doing

`docs/design/compatibility.md` is right that there is no stable Khora ABI, and
that is a different question from whether a Khora library can be *called*.
Exporting a C ABI is how a language gets used without anybody rewriting
anything: a Python extension, a Node addon, a plugin for something in C++. It
costs a calling convention and a lifetime story rather than a language feature,
and it is the cheapest adoption path there is.

It is also the direction that makes Khora's pitch testable by someone who is
not going to rewrite their service in it. A risk calculation with exact decimals
called from the Python they already have is a smaller ask than a rewrite, and it
is the same code either way.

## 1. What may cross — already answered

`ffi.md` §1 lists it: scalars by value, `Ptr` as an opaque address, and nothing
else. No ADTs, no `String`, no closures, no generics, no tagged returns.

That list was derived for calls *out*, from errata 35 — a 16-byte aggregate
comes back differently depending on whether LLVM or rustc described the struct,
they disagree on x86-64 Windows, and every failing test reported as passing.
The rule that came out of it is that only scalars and pointers cross between
generated code and anything else.

**It applies unchanged in this direction, and the compiler already enforces
it.** A signature the C ABI cannot carry is an error naming the type and the
reason. An export surface is the same check applied to a function that has a
body rather than one that does not.

## 2. Who owns what comes back — answered by a precedent already in `std`

This is the question that has no counterpart in `ffi.md`, because calling out
Khora keeps ownership of everything: §3a lends a buffer for exactly the
duration of the call, and the lifetime is the call.

Coming in, the caller wants a *result*. If that result is bytes — and it usually
is — something has to own them.

The wrong answer is for Khora to allocate and the C side to free. That means a
`khora_free` in the header, an allocator shared across the boundary, and a
lifetime rule nobody reads. It is the source of most of the bugs in every FFI
that has one.

**The right answer is already in `std/core.kh`:**

```khora
extern fn khora_float_text(value: Float, into: Ptr, capacity: Int) -> Int;
```

The caller provides the buffer and its capacity; the callee fills what fits and
returns the length it wanted. A caller that guessed too small calls again with a
bigger one. No shared allocator, no free function to forget, no ownership to
document — the buffer belongs to whoever made it, for as long as they say.

So an exported function returning text takes `(into: Ptr, capacity: Int)` and
returns an `Int`. That is not a special convention invented for exports; it is
the one the runtime and `std` already use between themselves.

**A Khora object cannot come back at all**, and should not learn how. A handle
would mean a `Ptr` into Khora's heap held by C across calls, with a reference
count only Khora can adjust and a lifetime only C knows — which is the whole
class of bug `Ptr` is defined to exclude. If it is ever wanted, it wants its
own argument and not an extension of this one.

## 3. When does the runtime start — a real question with a dull answer

A Khora program's `main` starts the runtime: the heap, and the single-threaded
flag the code generator decided on. A library has no `main`, and the first
exported call can arrive on a thread the runtime has never seen.

Three options, and the third wins on the same grounds each time:

**An explicit `khora_init` in the header.** Honest, and one more thing for the
caller to get wrong — the failure mode is a crash on the first call from anyone
who missed it in the README.

**A static constructor.** Runs before `main` in the host, works for everybody,
and is exactly the mechanism that makes shared-library load order a debugging
subject. No.

**Lazy, on first entry, once.** Every exported function begins by ensuring the
runtime exists. The cost is one relaxed atomic load per call against a
`std::sync::Once`, which is nothing next to a cross-language call, and there is
nothing for a caller to forget. `khora-rt` already has the pieces: `current.rs`
hands any thread that has not entered a fiber its own root, which is the same
problem solved the same way.

An explicit `khora_shutdown` should exist anyway, for a host that wants to
release the heap deliberately, but nothing should require it.

## 4. What a trap does here — **and this is where 12.8 gets harder**

`docs/design/traps.md` decided that a trap ends the process, and leaned on one
argument in particular: an external supervisor restarts it, and that is what
operators actually run.

**That argument does not survive this document.** A Python extension that
aborts the interpreter is not a restartable service; it is a library that took
down a process it did not own, belonging to somebody who never agreed to run a
supervisor. The same is true of a Node addon and of a plugin in someone's
editor.

Three honest responses, and I do not think the first two are enough:

- *Say so in the header.* Real, and weak — nobody reads a comment about
  aborting until after it has aborted.
- *Note that `khora_overflow` is a bug in Khora code, so the fix is upstream.*
  True and unhelpful to the host that just died.
- *Treat an export boundary as the containment boundary that a fiber is not.*
  An exported function is a natural unit: it has a single entry, a scalar
  return, and no counted values live across it from the caller's side. That is
  a much smaller problem than containing a trap mid-fiber — but it still needs
  unwinding to run the drops between the trap and the boundary, which is §3 of
  `traps.md` and is phase-sized.

**So this is a genuine cost of 12.6 that 12.8 did not price**, and it should be
recorded there rather than discovered when the first Python extension segfaults.
It does not change 12.8's decision — the mechanism is still the blocker — but it
removes one of that decision's three supports for this use, and 12.8's §6 lists
"a target makes process restart unavailable" as one of three things that would
overturn it. A library inside somebody else's process is that target.

## 5. What is built, and how

Mechanically small, given the above:

- `khora build --lib` writes a shared library rather than an executable: the
  same object, linked with `-shared`, plus the runtime archive.
- Exported functions get an unmangled C symbol beside the `kh$…` one, as a
  thin wrapper that ensures the runtime and forwards.
- A `.h` is generated from the same signatures the checker already validated.
  Generated rather than written, because a header that can drift from the
  source is a header that will.

## The one open question: how a function says it is exported

Three answers, and the third is my recommendation, but this is close enough to
the language surface that I would rather ask than decide.

**A. Every `export fn` with a C-compatible signature.** No new syntax at all.
Also no way to *not* export one, and a package's public Khora API and its C ABI
are then forced to be the same set — which they are not, because one is
governed by `compatibility.md`'s semver rules and the other is a promise about
machine layout.

**B. A marker in the source**, `export "C" fn …` or an attribute. Explicit at
the definition, familiar from Rust, and the most likely thing a reader would
guess. It is a language-surface change, and it puts a packaging decision in the
middle of the code.

**C. A manifest section.**

```toml
[lib]
exports = ["price", "risk_of"]
```

No language change; the list of symbols a library promises is a packaging fact
and lives with the other packaging facts, next to `[permissions]`, which is
already a manifest-side statement about what code may do. It also makes the ABI
reviewable in one place, which is what somebody auditing a shared library
wants — and `khora sbom` and `[permissions]` both argue that this project
already treats the manifest as where cross-boundary promises live.

Its weakness is real: a name in a manifest is not checked against the source by
the reader's eye, only by the compiler, and a typo is a build error rather than
an obvious one.

I lean to **C**. It is the only one of the three that does not either force the
Khora API and the C ABI to be the same set or put a packaging decision in a
function signature.
