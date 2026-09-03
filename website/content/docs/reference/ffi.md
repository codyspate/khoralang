---
title: Foreign function interface
sidebar:
  order: 17
---

Khora does not define a stable native Khora-to-Khora ABI. The stable interoperability boundary is the C ABI, expressed with `extern fn` and the opaque `Ptr` type.

## Import a C function

An `extern fn` declaration with no body names a C symbol that must be provided when the program is linked:

```khora
extern fn monotonic_ticks() -> Int;
extern fn read_into(handle: Ptr, into: Ptr, capacity: Int) -> Int;
```

It is called like an ordinary Khora function:

```khora
let now = monotonic_ticks();
```

Foreign declarations are subject to the package's extern permissions. That permission controls whether the package may declare the boundary; it is not process sandboxing.

## Export a Khora function to C

A public extern function **with a body** publishes a C-callable symbol:

```khora
pub extern fn price(units: Int, scale: Int) -> Int {
  units * scale
}
```

Build a shared library with:

```bash
khora build --lib
```

The toolchain writes the shared library and a C header generated from the exported signatures.

`pub extern fn` is therefore the source form for a C export; an `extern fn` without a body is a symbol Khora expects to find on the foreign side.

## ABI-safe types

The C boundary is intentionally narrow. Values may cross directly when they are C-compatible scalars or `Ptr`.

Do not put Khora-managed aggregates across the boundary. In particular, exported/imported ABI signatures do not carry Khora `String` values, algebraic data types, records, closures, generic values, or typed failure returns.

An exported function is also not generic and cannot expose a `raises` or capability requirement to its C caller:

```khora
// C-callable shape
pub extern fn sum(left: Int, right: Int) -> Int {
  left + right
}
```

Keep richer Khora modeling behind that narrow entry point and translate to/from scalar or pointer-shaped data at the boundary.

## `Ptr` is an opaque foreign address

`Ptr` represents a machine address supplied by foreign code:

```khora
pub type Ptr;

impl Ptr {
  pub fn null() -> Ptr;
  pub fn is_null(self) -> Bool;
}
```

Khora does not expose general pointer arithmetic or dereferencing through `Ptr`, and a `Ptr` is not a reference-counted pointer into the Khora heap.

A foreign handle can therefore be kept as an opaque `Ptr` and passed back to the library that created it:

```khora
extern fn open_native() -> Ptr;
extern fn close_native(handle: Ptr) -> ();
```

The foreign API still defines the handle's lifetime. Use a structured resource scope when the handle requires cleanup.

## Borrow buffers for one call

When C needs temporary access to bytes owned by Khora, lend them for the duration of a callback rather than exposing a long-lived pointer.

For strings, the standard forms are:

```khora
String::with_c_string(text, body)
```

where `body` receives a temporary `Ptr` to NUL-terminated bytes, and:

```khora
String::with_data(text, body)
```

where `body` receives `(Ptr, Int)` for the bytes and their length.

The corresponding function types preserve the callback's effects and failures while keeping the pointer lifetime inside the call:

```khora
fn with_c_string<B, 'ef, 'er>(
  self,
  body: (Ptr) -> B with 'ef raises 'er
) -> B
  with 'ef
  raises 'er

fn with_data<B, 'ef, 'er>(
  self,
  body: (Ptr, Int) -> B with 'ef raises 'er
) -> B
  with 'ef
  raises 'er
```

Do not retain those borrowed pointers after `body` returns.

For data returned **to** C, prefer caller-owned buffers: the C caller allocates memory and passes `(Ptr, capacity)`, while the Khora export fills it and returns a scalar length/status. This avoids sharing an allocator or requiring C to free Khora-managed objects.

## Blocking and suspension

A fiber may resume on a different operating-system thread after a Khora suspension. Foreign code must not retain any of the following across such a suspension unless the native API explicitly makes that safe:

- a thread-local address;
- native thread identity;
- borrowed errno-like thread state;
- a thread-affine handle whose contract requires one OS thread.

Potentially blocking native work must run on a fiber of its own rather than on the one that needs the answer:

```khora
extern fn slow_native_thing(handle: Ptr) -> Int;

fn measure(handle: Ptr) -> Int {
  Fiber::join(Fiber::spawn(fn () => slow_native_thing(handle)))!
}
```

**That is the boundary**, and it is written at the call site rather than wrapped in a helper. A fiber is an operating-system thread, so the blocking call holds a thread that is running nothing else, and the caller suspends the way it would for a socket.

There is no `blocking(body)` in `std` to reach for instead, and the reason is a property of the language rather than an omission. A closure's captures are not part of its type, so nothing at a `spawn` can tell whether what it captured may cross to another fiber — which is why `Fiber::spawn` requires the closure to be *written where it is spawned*, and why a helper taking `body` as a parameter is refused:

> `body` cannot be handed to another fiber: `() -> A` holds a closure, and what a closure captured is not in its type — so nothing here can tell whether *that* can be written.

Two consequences worth planning for. The cost is a thread and the round trip to start and join one, which is the wrong trade for a call that takes a microsecond: reach for this when the work would hold a thread long enough to matter. And it is **not a cancellation point on the far side** — a cancelled caller stops at the join while the native call runs to its end, because the runtime cannot interrupt foreign code and returning early would mean doing so while a thread still holds the caller's buffer.

When fibers become M:N over a fixed set of workers, blocking on one *will* occupy a worker, and this shape is what the runtime will be able to route to its blocking pool. A direct call to the native function is not.

An ordinary foreign call also cannot secretly suspend Khora while foreign stack or thread-affine state remains live around the call.

## Callbacks C keeps and calls later

A C caller may take the address of a `pub extern fn` and call it whenever it
likes — a signal handler, a callback registered with a library, a thread of its
own. Three questions decide whether that is safe, and all three have answers.

**The pointer lasts as long as the process.** A `pub extern fn` is an exported
symbol in the shared library, not a value that was allocated: its address is
fixed once the library is loaded and stays valid until it is unloaded. There is
nothing to free and nothing to keep alive.

**A Khora closure cannot be exported at all, and is not a callback you can
hand out.** A closure is a code pointer *and* the values it captured; the code
pointer alone cannot be called, and there is no C type for the pair. If a
foreign API wants a callback with user data, export a `pub extern fn` taking a
`Ptr` and let C pass its own context through it.

**The runtime must already be running.** An exported function may allocate,
raise, or trap, and all three need the runtime that the library's own
initialisation starts. Calling an export before the library is loaded is not a
Khora question; calling one *after* it has been unloaded is undefined in the
same way calling any unloaded symbol is.

**Re-entrancy is allowed and is an ordinary call.** C entering Khora while
another Khora call is already on the stack — a callback invoked from inside a
foreign function that Khora itself called — is a nested call on the same
thread and needs nothing special. What it does not do is join the outer call:
it is a separate exported boundary with its own error handling, so a `raises`
row is discharged there rather than travelling out through the foreign frames
in between, which could not carry it. A trap is process-fatal by default in a
callback exactly as it is anywhere else; where the host has opted into
export-boundary containment, the callback is its own boundary and not part of
the outer one.

Two rules from elsewhere on this page apply with particular force here, because
a retained callback is exactly where they get broken:

- The callback may be entered on **any** host thread, including one Khora has
  never seen (`Libraries may be called from several host threads`).
- Anything the callback was handed — a borrowed buffer, a `Ptr` into Khora
  memory — is valid for that call and no longer, so a callback that stores one
  for the next invocation is storing a dangling pointer
  (`Borrow buffers for one call`).

## Libraries may be called from several host threads

A library build cannot assume that the host calls it from only one OS thread. Python, Node, C/C++, or another embedding process controls which host threads enter exported functions.

Design exported code and foreign state accordingly; do not infer single-threaded execution merely because the Khora library itself never spawns a fiber.

## Traps at an exported boundary

The default trap policy is still process-fatal. An embedding host can explicitly opt into the export-boundary containment API when it cannot allow a Khora bug to terminate the host process.

Containment is deliberately narrower than general fiber/request containment and has restrictions, including the fact that an exported call that spawns a fiber cannot be safely contained this way.

See [Traps](/docs/reference/traps/) for the exact host-side form and current containment rules, [Memory and resources](/docs/reference/memory-and-resources/) for cleanup, and [Concurrency](/docs/reference/concurrency/) for thread migration after suspension.
