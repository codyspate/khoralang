---
title: Traps
sidebar:
  order: 16
---

A trap is an unrecoverable programming error or violated invariant. Traps are intentionally separate from typed `raises` failures: they are not part of a function's failure row and cannot be handled with `catch`.

## Operations that trap

Checked integer arithmetic traps when the mathematical result does not fit the type:

```khora
let total = left + right;
let product = width * height;
```

Integer division and remainder trap when the divisor is zero, and when the quotient does not fit — which for a signed type is the one pair `minimum / -1`:

```khora
let each = total / people;
let over = total % people;
```

Array access traps when the index is outside the array:

```khora
let item = Array::get(items, index);
```

Other operations trap on a violated invariant, and this is the whole list:

| operation | when |
| --- | --- |
| `String::from_bytes` | the bytes are not valid UTF-8 |
| `String::byte` | the offset is outside the string |
| `Vector::at` | the index is outside the vector |
| `Decimal` arithmetic | addition, subtraction, multiplication, division, rescaling or truncation overflows |

An assertion that fails in a test is **not** a trap. `assert` reports the
failing line and the test continues to the end of the run, because a test
suite that stopped at the first failure would report one problem per run.

A trap is appropriate when continuing would mean the program itself is wrong. It is not appropriate for ordinary external input that a caller is expected to reject.

## Running out of stack

Not a trap — the operating system ends the process and there is nothing to unwind onto — but it is reported the same way, because a program that stops without saying why is the worst of both:

```
khora: the stack ran out
```

on standard error, followed by the platform's stack-overflow exit status rather than 134.

Khora does not guarantee tail-call optimisation, so a function that recurses once per element of its input uses a frame per element. See [Known limitations](/docs/limitations/) for what that means for `List` in practice, and which operations are unaffected.

## Traps are not `raises`

A function does not declare arithmetic overflow in its `raises` row:

```khora
fn area(width: Int, height: Int) -> Int {
  width * height
}
```

If the multiplication overflows, `area` traps. Adding `catch` around the call does not turn that trap into a recoverable failure:

```khora
// `catch` handles declared failures, not traps.
let result = load()! catch {
  LoadError::Missing => fallback(),
};
```

Use `raises` for expected conditions a caller can make a decision about. Use a trap for an invariant the program was required to uphold.

## Ask explicitly for wrapping arithmetic

When modular arithmetic is the intended operation—a hash, checksum, PRNG, or wire algorithm—use the wrapping operation by name instead of relying on build-mode overflow behavior:

```khora
let mixed = Int::wrapping_add(left, right);
```

Checked arithmetic traps in every build; release builds do not silently change to wrapping arithmetic.

## Default runtime behavior

By default, a trap:

1. writes a diagnostic to standard error;
2. identifies the operation or invalid index;
3. identifies the spawned fiber when the trap occurred on one;
4. terminates the process with status 134.

A trap with no backtrace says how to get one, so the first thing a person sees
tells them what to do next:

```
khora: Int addition overflowed on fiber 7
note: re-run with KHORA_BACKTRACE=1 to see where
```

```bash
KHORA_BACKTRACE=1 khora run .
```

`RUST_BACKTRACE=1` is honoured too, because a person who has debugged a Rust
program will try it. `KHORA_BACKTRACE` is the name to use and the one the
runtime's own note gives.

When debug information is available, the trace includes Khora source frames and locations. The runtime's own frames are trimmed from the top of it, so the first frame is the line in your program that trapped. Standard output is flushed before the process ends, so what the program had already printed is not lost.

A process-fatal trap is **not** normal structured failure unwinding. Do not depend on regions, `catch`, or application finalizers to recover from it. Resource correctness must come from normal return, typed failure, and cancellation paths; a fatal trap means the process is ending.

## Containment for exported C calls

A host embedding a Khora shared library may explicitly opt into trap containment at the **export boundary**:

```c
khora_set_trap_policy(1);

int64_t value = price(units, scale);

if (khora_trapped()) {
    khora_clear_trap();
    /* the host process is still running */
}
```

The default remains process-fatal. Containment occurs only after the host enables it and only while entering through a generated `pub extern fn` export wrapper.

When an exported call is contained, the call is discarded and allocations created by that guarded call are released before control returns to the host. The trap is still reported; containment prevents that one bug from terminating the embedding process.

This is **not** a general exception mechanism and is not equivalent to `raises`.

## Why containment is limited to exports

An exported C function has a deliberately narrow signature: scalars and `Ptr` cross the boundary; exported functions are not generic and do not expose Khora values, capability rows, or typed failures to the host. That makes the exported call a boundary whose newly created Khora allocations can be discarded without leaving a Khora object in the caller.

If a guarded exported call spawns a fiber, containment is disarmed for that call. A spawned child may outlive the C call and may hold references to allocations made by it; freeing those allocations underneath the child would be unsound. In that case a trap retains the ordinary process-fatal behavior.

There is currently no general request-fiber or arbitrary-fiber trap containment mechanism. Do not design application recovery around trapping inside a fiber and continuing the service.

## What this means for a server

**A trap in a request handler ends the server, not the request.** This is the
consequence of everything above, and it is worth stating plainly because the
code around a handler reads as though it were not true.

`Router::listen` serves each connection on its own fiber and wraps your handler
so that a typed failure becomes a 500 rather than a dropped connection. That
wrapper handles `raises`. A trap is not a raise: it does not unwind, so it does
not reach the wrapper, and the process exits with status 134. Every other
connection being served at that moment ends with it, including ones that had
nothing to do with the request that trapped.

Design for it in three places.

**Validate at the boundary, not in the handler.** An integer that came from a
request is external input until a schema has read it. `std::schema` rejects a
port outside 1 to 65535 as a 400; the same value multiplied inside a handler
traps and takes the process down. This is the reason the standard library's own
HTTP reader caps a `Content-Length` before multiplying it, answering 413 rather
than trapping.

**Run more than one process.** Since the blast radius of a trap is the process,
the unit of redundancy is the process. A server behind a load balancer with at
least two instances survives one trapping; a single instance does not. This is
ordinary operational practice and Khora does not change it, but a language that
contained traps per request would let you skip it, and this one does not.

**Expect a restart, and make it cheap.** Status 134 is a crash as far as a
supervisor, container runtime or service manager is concerned, and the right
response is a restart. Hold no state in the process that a restart cannot
rebuild or that a second instance cannot serve from.

What you get in exchange is that a trap is never silently swallowed. A handler
that overflows is a bug in your program, and the process ending with a message
naming the line is a louder and shorter path to fixing it than a 500 that
happens to a fraction of requests and is discovered in a log a week later.

## Validate external data before invariant operations

Untrusted input should become an ordinary value or typed failure before it reaches an operation whose precondition may trap:

```khora
fn item_at(items: Array<Item>, index: Int) -> Item
  raises InputError
{
  if index < 0 || index >= Array::length(items) {
    raise InputError::InvalidIndex(index)
  }

  Array::get(items, index)
}
```

At that boundary, an invalid client-supplied index is an expected failure; the array access after validation is an invariant the program can safely rely on.

See [Failures](/docs/reference/failures/) for recoverable errors and [FFI](/docs/reference/ffi/) for the exported C boundary.
