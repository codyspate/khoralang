---
title: Foreign function interface
sidebar:
  order: 24
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

Potentially blocking native work must use an API/runtime boundary intended for blocking work rather than occupying a scheduler worker indefinitely.

An ordinary foreign call also cannot secretly suspend Khora while foreign stack or thread-affine state remains live around the call.

## Libraries may be called from several host threads

A library build cannot assume that the host calls it from only one OS thread. Python, Node, C/C++, or another embedding process controls which host threads enter exported functions.

Design exported code and foreign state accordingly; do not infer single-threaded execution merely because the Khora library itself never spawns a fiber.

## Traps at an exported boundary

The default trap policy is still process-fatal. An embedding host can explicitly opt into the export-boundary containment API when it cannot allow a Khora bug to terminate the host process.

Containment is deliberately narrower than general fiber/request containment and has restrictions, including the fact that an exported call that spawns a fiber cannot be safely contained this way.

See [Traps](/docs/reference/traps/) for the exact host-side form and current containment rules, [Memory and resources](/docs/reference/memory-and-resources/) for cleanup, and [Concurrency](/docs/reference/concurrency/) for thread migration after suspension.
