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

A foreign function is one written `extern fn`:

```khora
extern fn fopen(path: Ptr, mode: Ptr) -> Ptr;
```

**Scalars, by value.** `Int` and `I64` are `int64_t`. `U8`…`I32` are the C type
of the same name and width. `Float` is `double`. `Bool` is a C `_Bool`. `()` as
a return type is `void`, and is not allowed as a parameter.

**Pointers, as addresses.** `Ptr` is a C `void *`: opaque, not counted, not
dereferenceable from Khora, and **never a pointer into Khora's own heap**. It
exists so a foreign library can hand back a handle — a `FILE *`, an
`SSL_CTX *` — and be given it again later.

`Ptr::null` and `Ptr::is_null` are the whole of what one can do, which is
deliberate. Since nothing turns a Khora value into a pointer, a dangling `Ptr`
is not something the language can express: every pointer that exists came from
the other side, and its lifetime is that side's business.

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

### Three kinds of declaration, and only one of them is C

Before `extern`, a function without a body *was* a foreign function, silently.
That is the same trap as errata 36 and 39 — the language accepting something
and quietly meaning something else — and it had the worst possible symptom: a
misspelled name became a C symbol nobody defines, and the only sign was
`undefined symbol` from the linker. No line, no file, no mention of Khora.

There are three kinds, and now they are three things:

| Written | Means |
| --- | --- |
| `fn f() -> Int { .. }` | a Khora body |
| `extern fn f() -> Int;` | a C symbol, found at link time |
| `fn f() -> Int;` | **a promise nobody has kept yet** |

The third is not an error. A signature written ahead of its implementation is a
useful thing to have — `std::net::http` is nothing but those, and the reference
application typechecks against them. The checker takes it on trust. *Calling*
one is where the promise comes due, and the code generator says so:

> `` `calculat_total` has no body, so there is nothing to call. Give it one, or
> write `extern fn` if it is a C symbol to be found at link time``

Almost every language with a foreign interface makes you say it: Rust's
`extern "C" { }`, TypeScript's `declare`, C#'s `extern`, Java's `native`,
Kotlin's `external`, Zig's `extern`, Haskell's `foreign import`. Go allows the
bodyless form but refuses it unless the package really does contain assembly,
so a typo is a compile error rather than a link error. The one language where a
bodyless declaration silently means "elsewhere" is C — and C is where the
undefined-symbol experience comes from.

`extern` is a **contextual** keyword, recognised only where a `fn` declaration
begins. It costs nothing to make it one, and it means adding the word could not
break a program that was already using it for something.

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

### And `errno` in particular cannot cross, now that a fiber is not a thread

The example above reads the error out of the return value, which is why it
works. **Reading it out of `errno` would not.**

`errno` is thread-local. A fiber is not: Phase 11 moves it between workers at
every suspension, and a suspension happens at any safepoint, which is every
loop back-edge in a program that can spawn. So a shim that sets `errno` and
returns, and a caller that reads `errno` afterwards, are only reliably on the
same thread if nothing between them can suspend — and nothing in the type
system says that.

The same applies to Windows' `GetLastError` and `WSAGetLastError`, and to any
library that keeps its last error in thread-local storage — which is most of
the ones that keep it anywhere but the return value.

This was found in 11G by writing a shim that reported a timed-out receive the
way the kernel used to, with `EAGAIN` beside the `-1`. On Linux the caller read
`ETIMEDOUT` instead, and the fix was to stop making the promise: `std::net`
looks at the sign of the return, and `std::fs` says outright that C's error
numbers are "deliberately coarse" and a table it declines to know. Nothing in
Khora reads `errno` today, and this is the reason it should stay that way.

Where a library genuinely has no other channel, the error must be read **inside
the shim**, on the same side of the boundary as the call that set it, and
returned as a value.

### The one thread-local that is read from Khora, and why it is allowed

`khora_decimal_high()` returns the upper half of the last 128-bit answer, out of
a thread-local this runtime sets. That is the shape the section above rules out,
so the exception needs its condition stated rather than assumed.

The reason it exists is rule 1: a `Decimal`'s significand is a hundred and
twenty-eight bits, and an aggregate must not cross, so an operation can only
return half of it. The rest has to be fetched.

What makes it safe is not that it is our own runtime — `errno` would be no
worse if it were. It is that **the read is the next instruction**. Every wrapper
in `std/decimal.kh` is

```khora
let lo = khora_decimal_add(...);
let hi = khora_decimal_high();
```

and the argument above turns on suspension: a fiber moves between workers at a
safepoint, and the only safepoint generated code emits is a loop back-edge
(`lower.rs`'s `back_edge`). There is no back-edge between two adjacent `let`s,
so there is no point at which the fiber can change workers, so the thread-local
is the same one.

That is a real proof and a narrow one. It does **not** license reading a
foreign library's thread-local after a call, because that argument needs the
value to be set and read on the two sides of a single statement boundary that
the compiler is known not to interrupt — which is a property of this runtime's
own paired entry points and of nothing else. `khora_spawn_capture` and
`khora_spawn_take` are the other pair, and the same condition holds for them.

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

## 3a. Lending a buffer: the lifetime is the call

`Array::with_data(self, body)` and `String::with_data(self, body)` hand the
body a `Ptr` and a count, for the duration of the call and no longer.

```khora
fn read_into(fd: I32, buf: Array<U8>) -> Int {
  buf.with_data(fn (p, n) => sys_read(fd, p, n))
}
```

**The bound is a call because no other bound is right.** The obvious
`Array::data(self) -> Ptr` is a dangling pointer the compiler creates for you:
Perceus releases the array at its last *use*, and that use is the `data` call
itself. Widening it to a scope does not help either — the innermost scope is
wrong for a pointer taken inside one branch of an `if`, and the function's own
scope is wrong for a loop, which would hold one live buffer per iteration. A
body is the only bound that is right in all three.

The array is released by a **scope** rather than by a statement after the call,
so a body that raises does not leak it. That is errata 34, which has now been
the answer three times.

The count is the number of *elements*, the same number `Array::length` gives,
so a body that wants bytes multiplies by the width itself. For an `Array<U8>`
and for a `String` the two are the same, which is the common case.

**Only an array of numbers can be lent.** An `Array<A>` of Khora objects holds
reference-counted pointers, and handing those across is the mistake the whole
boundary exists to prevent; the compiler refuses it by name.

What this does *not* do is stop the pointer escaping — a body can write it into
a `mut` field and read it afterwards. That is the same line Rust draws:
obtaining a pointer is safe, and what happens on the far side of the boundary
is the binding author's responsibility. What the borrow removes is the
*accidental* case, the one the compiler creates behind your back and nobody
would think to look for.

### And a C string is a copy

`String::with_c_string(self, body)` lends the bytes with a zero after them.
A copy, necessarily: a Khora string knows its length instead of carrying a
terminator, and there is nowhere to append one to a borrowed view. The copy
lives exactly as long as the call, released by the same scope discipline.

A string with a zero *inside* it is not refused. C will see a shorter string
than Khora has, which is what C strings are; inventing a rule here that the
boundary does not have would be pretending the difference is smaller than it
is.

## 4. Foreign resources are counted values

This needed nothing new at all. `acquire(value, release)` in `std::core`
registers a release with the enclosing `Scope`; a `Scope` is a region; and a
region runs its deferred work on every way out. So:

```khora
let file = acquire(open_file(path), fn f => { fclose(f); });
```

and the file closes on every path out of the enclosing region — including a
raise passing through it, and including a cancellation. Not because a file is
special, but because *everything* is a counted value and this is what counted
values already did.

`a_file_is_closed_on_the_error_path` in `tests/files.rs` is the proof, and it
does not take Khora's word for it: the program opens a real file, registers the
close, raises, and lets the raise leave the region — and then the *test* deletes
the file, which Windows refuses to do while a handle is open. The delete
succeeding is the close having happened.

## What this reaches, today

`fopen`, `fread`, `fclose`, `strlen` — ISO C, spelled the same on every target
Khora has, and `FILE *` is exactly what `Ptr` is for. `tests/files.rs` reads a
real file into an `Array<U8>` with no Rust anywhere in between. Seven
declarations is the whole of it.

That is most of phase 7's exit criterion. The socket half is missing for a
reason that is not about the boundary: a socket is not ISO C. It is Winsock or
it is Berkeley sockets, and the two do not even agree on what a socket *is* — a
`SOCKET` is a `UINT_PTR` and a file descriptor is an `int`.

**Choosing between them is no longer the obstacle.** A source file whose name
ends in `_windows`, `_linux`, `_macos` or `_posix` is compiled only on those
targets, so `socket_windows.kh` and `socket_posix.kh` may both declare
`module std::net::socket;` and at most one is ever in the build. What remains
is writing them, which is a `std::net` and therefore phase 8's.

### Why a file name and not an attribute

The rule is Go's, and for Go's reasons. An `#[if(windows)]` attribute puts two
targets' code in one file, so every reader reads both, the compiler parses
both, and the arrival of a third target grows a nest of conditions in the
middle of otherwise ordinary code. A suffix keeps each target's version whole
and readable on its own, and makes *which files did this build use?* a question
`ls` can answer.

What it deliberately does not allow is a differing *fragment*. If two targets
share ninety per cent of a file, the other ten belongs behind a function they
both call — which is what a reader would want regardless.

A file named outright on the command line is read whichever target it names.
Asking for a file by name is asking for it, and refusing would leave no way to
check the other target's version at all.

## Still open

- **Callbacks.** A Khora closure cannot be a C function pointer, but a
  *top-level* Khora function very nearly can: it has a symbol and a machine
  signature. The likely answer is that only a non-capturing, non-raising,
  top-level function may be passed as a callback, which is a restriction the
  type system can state. `qsort` and every `on_event` want it; nothing does
  yet.

  **Whoever builds this has to answer one more thing, and the type system
  cannot.** `docs/design/scheduler.md` §8 says a fiber may not suspend inside
  an `extern` call: a suspended stack moves to another worker, and C frames
  sitting on it resume on a thread that is not the one that made them — wrong
  `errno`, wrong thread-locals, a lock held by nobody. Today that is
  unenforced and unreachable, because there is no way for foreign code to
  re-enter Khora. A callback is exactly that way.

  The cheap answer does not work. "A callback must have an empty `with` row"
  sounds sufficient and is not: `std::net::socket`'s `receive` and `connect_to`
  declare no capability at all and suspend, because they take raw handles
  rather than a capability. **Suspension is not in the effect row**, so no
  signature can be read to mean "this cannot suspend".

  So the mechanism has to be dynamic: a per-thread depth the code generator
  raises around a foreign call and the scheduler checks before it parks, so a
  callback that tries to suspend stops the program with a message naming what
  it did instead of migrating a C stack. That costs two counter updates per
  foreign call, which is nothing beside a foreign call. It is not built,
  because building a guard against something nothing can express yet would be
  untestable machinery — and it is written here rather than in a commit
  message so that the feature cannot land without meeting it.
  `docs/design/soundness.md`.
- Nothing, for now. `extern` landed; see below.
