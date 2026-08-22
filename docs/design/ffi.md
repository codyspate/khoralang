# The foreign boundary

Phase 7, and the decision for D8. Per A6 and `docs/design/ecosystem.md`, the
boundary is the C ABI and nothing else: Khora's libraries are written in Khora,
and a short list of things nobody should write twice are bound rather than
reimplemented.

Three questions were open. This answers them.

> **A foreign function takes and returns scalars and pointers, requires
> capabilities without receiving them, and cannot raise. Everything else is a
> Khora wrapper's job.**

## 1. What may cross, and in what layout

**Scalars, by value.** `Int` and `I64` are `int64_t`. `U8`…`I32` are the C type
of the same name and width. `Float` is `double`. `Bool` is a C `_Bool`. `()` as
a return type is `void`, and is not allowed as a parameter.

**Pointers, as addresses.** This is what `Ptr` is for, and it does not exist
yet — it is the first thing 7.3 needs. Until it does, a foreign function can
only take numbers, which is enough to prove the boundary works and not enough
to do anything with it.

**Nothing else.** In particular:

- **No ADTs, records or `String`.** A Khora object is a reference-counted heap
  allocation with a header the C side knows nothing about, and handing one over
  gives the foreign function a pointer it cannot read and a reference it cannot
  release. Passing a `String`'s *bytes* is a `Ptr` and a length, written out.
- **No closures.** A Khora closure is a heap object holding a function pointer
  and its captures, called through an adapter. C expects a bare function
  pointer. Callbacks are question 3.
- **No generics.** A generic function has no single machine signature, and
  monomorphization has nothing to specialize a body it does not have.
- **No tagged returns**, which is question 2.

### Why the list is this short

Errata 35, and it cost a day. A tagged return is `{ i32, i64 }`, and **how a
16-byte aggregate comes back is a target decision that LLVM makes for the
struct type and rustc makes for a `repr(C)` struct of the same shape.** On
x86-64 Windows they disagree, silently, and the tag arrived as zero — so every
failing test reported as passing.

The rule that came out of it was "only scalars and pointers cross between
generated code and the runtime", and the runtime is just the first foreign
library. Everything above is that rule applied to a boundary the user writes
rather than one the compiler generates.

**The check is now in the compiler.** A function declared without a body is a
foreign function, and its signature is verified where the call is generated: a
parameter or return the C ABI cannot carry is an error naming the type and the
reason, not a pointer quietly handed over. Before this, `fn f(p: Pair) -> Int;`
compiled and passed a refcounted heap object to C; only the missing symbol
stopped it, and a symbol that happened to exist would have been worse.

## 2. Failure: a foreign function cannot raise

`raises` on a foreign declaration is refused. Two reasons, and the first alone
would be enough:

1. A fallible Khora function returns a tagged pair, which is exactly the
   aggregate errata 35 says must not cross.
2. **C does not have an error channel.** It has a return value that means
   something, and *what* it means differs per library: negative is an errno,
   zero is failure, `NULL` is failure, the real answer is in a pointer
   parameter, or you call `GetLastError` afterwards.

So the translation is written in Khora, where it can be read:

```khora
extern fn sys_read(fd: I32, into: Ptr, len: Int) -> Int;

fn read(fd: I32, into: Ptr, len: Int) -> Int raises IoError {
  let n = sys_read(fd, into, len);
  if n < 0 { raise IoError::Errno(-n) } else { n }
}
```

Three lines, and every reader can see which convention this library uses. A
compiler-level mapping would have to be configured to say the same thing, in a
syntax nobody knows, and would be wrong for the next library.

## 3. Capabilities: required, not passed

**A `with` clause on a foreign function is a permission, and nothing is
appended to the call.**

For an ordinary Khora function, `with { ledger: Ledger }` means the caller
passes a `Ledger` — evidence is an argument, appended after the written ones in
label order. A C function has no use for a Khora record of closures, so
passing one would be meaningless at best.

But the *requirement* is worth everything. It is how the boundary is governed:

```khora
extern fn sys_open(path: Ptr, len: Int, flags: I32) -> I32
  with { fs: Fs };
```

Nothing can open a file without holding `Fs`, and `Fs` is not something a
function can conjure — it is passed in, from `main`, through every frame that
needs it, visible in every signature on the way. That is D4's teeth. The
manifest's `[permissions]` becomes a claim about which capabilities `main`
constructs, and the type system carries it the rest of the way, with no runtime
check and no sandbox.

It also means the discipline is **asserted rather than inferred** here, which is
the one place it has to be: a foreign function is opaque, so its row is a
promise the compiler takes on trust and then enforces on every caller. Get the
row wrong in a binding and the binding is lying — which is why bindings to the
operating system belong in the standard library, reviewed once, rather than
being written afresh in every package.

## 4. Foreign resources are counted values

An open file is a Khora object holding the descriptor, whose release calls the
foreign close. That is the shape a region, a fiber handle and a nursery already
have — `khora_region_release` and friends are runtime-provided drop glue on an
ordinary counted object — so 7.3 is a use of machinery that exists rather than
new machinery.

What it buys is the exit criterion: the file closes on *every* path out,
including a raise and including a cancellation, because releasing it is what
leaving the scope does.

## Still open

- **`Ptr` itself.** An opaque machine address, not counted, not dereferenceable
  from Khora. The open question is not the type but how a buffer is lent:
  `Array::data(self) -> Ptr` is a dangling pointer waiting to happen, since
  Perceus releases the array at its last *use* and that may be before the
  pointer's. A scoped borrow — `Array::with_data(self, f)`, where the array is
  provably alive across the call — is the shape that cannot go wrong, and is
  the same shape `scoped` and `nursery` already use.
- **Callbacks.** A Khora closure cannot be a C function pointer, but a
  *top-level* Khora function very nearly can: it has a symbol and a machine
  signature. The likely answer is that only a non-capturing, non-raising,
  top-level function may be passed as a callback, which is a restriction the
  type system can state. `qsort` and every `on_event` want it; nothing does
  yet.
- **`extern` as a keyword.** Today a top-level function without a body *is* a
  foreign function, silently. That is the same trap as errata 36 and 39 — the
  language accepting something and quietly meaning something else — and a
  misspelled name becomes a linker error rather than "no such function".
  `extern fn` would say it out loud. It is a change to the language's surface
  and to every test that declares `fn print(value: Int);`, so it wants a
  decision rather than a drive-by.
