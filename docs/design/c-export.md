# Calling Khora from C

Roadmap 12.6. `docs/design/ffi.md` settles how Khora calls out; this is the
other direction, and it is not symmetric.

**Built.** `khora build --lib` writes a shared library, a header beside it, and
a C symbol per `export extern fn`. The spelling was the one question this
document left open, and §6 records how it was settled and why.

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

**Lazy, on first entry, once.** Nothing for a caller to forget, and one relaxed
load against a `Once` is nothing beside a cross-language call.

**And it turned out not to be needed at all** — see §7. There is nothing to
start: the heap allocates on demand, `current.rs` already hands a thread that
has not entered a fiber its own root, and the only thing `main` does eagerly is
*narrow* the runtime by declaring the program single-threaded, which a library
must never do. So an exported wrapper is a forwarding call and no prologue. The
option survives here because it is the one to reach for if an export ever does
need setup, and because the reasoning against the other two does not change.

An explicit `khora_shutdown` may still be worth having for a host that wants to
release the heap deliberately. Nothing requires it, and nothing offers it yet.

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

## 6. How a function says it is exported — **`export extern fn`**

```khora
export extern fn price(units: Int, scale: Int) -> Int {
  units * scale
}
```

Two words the language already has, no new keyword, no string literal.

The alternative I first recommended was a `[lib] exports = [...]` manifest
section, and it was wrong. Khora's own line, from `permissions.md`
§"Why the line falls there", is that a *decidable fact about the program* goes
where the compiler can keep it and a *policy about values and trust* goes in
the manifest. `check_extern_allowlist` shows the two layers working: the source
declares `extern fn`, and the manifest permits it **by package name**. The
manifest never names an individual function.

"Is this part of the C ABI?" is a per-item, decidable, type-level fact — it
constrains that function's signature to scalars and `Ptr`, forbids generics,
forbids raising. A manifest list would have been the first key naming functions
rather than packages, imposing a constraint on a source line that gives no hint
it is special. It would also split visibility across two files, when `export fn`
is already in the source and the keyword audit renamed `pub` to `export`
precisely *for* that coherence.

Against the audit's three questions, `export extern fn` passes on its own
terms and is more coherent here than Rust's `extern "C"` is there: Rust
overloads one spelling for both directions, and Khora already has `import` and
`export` as its direction vocabulary. `"C"` was dropped because Rust needs it
to choose between `"system"`, `"stdcall"` and the rest, and Khora has one ABI —
syntax for a variation that does not exist is what question 3 exists to catch.

**A body is what distinguishes the directions**, which is a rule that already
existed: errata 5 makes a body optional, so `extern fn` without one is a symbol
to find at link time and with one is a symbol to publish. `export` in front
makes that explicit rather than leaving a reader to notice that a body reversed
the arrow.

The trust layer stays available if it is ever wanted: whether a *dependency*
may put symbols into your library is package-keyed policy, the same shape as
the extern allowlist, and belongs in the manifest exactly where that does.

## 7. What building it turned up

**Windows publishes only what carries `dllexport`.** The library built, the
header generated, the import library was written, and `lld-link` told the first
C caller `undefined symbol: price`. Every artifact present, nothing reachable.
`DLLStorageClass::Export` on each wrapper; a no-op on ELF and Mach-O, where a
shared object's symbols are visible already. This is the same shape as 12.4's
four silent failures, and the reason the test suite compiles and runs a real C
program rather than inspecting the object.

**A library is never single-threaded.** `Backend::single_threaded` is set when
the program cannot spawn a fiber, and reference counting then skips atomics.
The host decides which of *its* threads calls in, and may use several — so a
`--lib` build must never claim it, whatever the Khora code contains. It falls
out of `Entry::Library` failing the comparison in `driver.rs`, which is
load-bearing rather than incidental: getting it wrong is a data race in a
refcount.

**No runtime start is needed**, which was §3's question and turned out to have
a duller answer than any of its three options. The heap is lazy and
`SINGLE_THREADED` defaults to the atomic answer, so an exported wrapper is a
forwarding call and nothing else.

**Two functions cannot publish one symbol.** The C namespace is flat, so two
`export extern fn price` in different modules are a collision a linker resolves
by picking one — silently, and not necessarily the same one twice. Refused by
name.
