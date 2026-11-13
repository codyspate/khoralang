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
let opposite = -amount;
```

Negation is the same subtraction from zero: `-x` traps on a signed type's
minimum, which has no positive twin, as `x * -1` does, and on any unsigned
value but zero. A negated literal is one number rather than a negation, so
`-128` is an `I8` and `-9223372036854775808` an `Int`, and neither traps.

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
| `Decimal` arithmetic | `+`, `-`, `*` or prefix `-`, or the methods they call, overflow — including `+` and `-` on scales more than 38 apart; division, rescaling or truncation overflows |
| `Shared::update` / `Shared::modify` | the change function re-enters the same cell, which would otherwise deadlock — see [Sharing](./sharing/) |

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

**The message covers every stack Khora code runs on**: the one the program started on, a fiber's on either backend, and a test's under `khora test`, on Linux, macOS and Windows. Reporting from an exhausted stack needs room set aside before the fault, and the runtime sets it aside on each of those threads, as an alternate signal stack. That includes a stack that runs out while the runtime is starting a thread or a program: the C library blocks signals for the moment it does that, and a fault in that moment cannot be reported, so the runtime checks for 16 KB of stack first and, when it is not there, stops the program before the start with the message above. On macOS a thread's stack overflow arrives as a different signal from the main thread's, and both are reported. A C library that starts its own threads deep in a Khora stack is outside what the runtime can check.

**How deep is deep.** A fiber's stack is eight megabytes on either backend, the same as `main` gets from a default `ulimit -s` on Linux and macOS. What that buys depends on the frame: a function that matches a list cell and recurses on the tail went about 100,000 calls deep in a debug build and 260,000 in a release build on x86-64 Linux, in `main` and on a fiber alike. A larger stack is not available, by setting or by `spawn` option. A walk deeper than that is one to write as a loop.

Khora does not guarantee tail-call optimization, so a function that recurses once per element of its input uses a frame per element. See [Known limitations](/docs/limitations/) for what that means for `List` in practice, and which operations are unaffected.

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

## Ask for `None` instead of a trap

When an operand came from outside the program — an amount in a request, a
length in a header — an answer that does not fit is bad input, not a bug. Each
integer type and `Decimal` has `checked_*` forms that answer `None` exactly
where the operator or method would trap, and the same number everywhere else:

```khora
fn line_total(price: Int, quantity: Int) -> Result<Int, String> {
  match Int::checked_mul(price, quantity) {
    Option::Some(total) => Result::Ok(total),
    Option::None => Result::Err("400: the quantity is too large"),
  }
}
```

| type | checked forms |
| --- | --- |
| `Int`, `I8`, `I16`, `I32` | `checked_add`, `checked_sub`, `checked_mul`, `checked_div`, `checked_neg`, `checked_abs` |
| `U8`, `U16`, `U32`, `U64` | `checked_add`, `checked_sub`, `checked_mul`, `checked_div`, `checked_neg` |
| `Decimal` | `checked_add`, `checked_sub`, `checked_mul`, `checked_divide` |

`checked_div` is `None` for a zero divisor and for a signed type's minimum over
`-1`; `checked_neg` and `checked_abs` are `None` for a signed type's minimum;
an unsigned `checked_neg` is `Some(0)` for zero and `None` for anything else.
`checked_neg` is `None` exactly where prefix `-` traps.
`Decimal::checked_divide` is `None` for a zero divisor and for a quotient that
does not fit, and `Decimal::checked_add` and `checked_sub` for two scales more
than 38 apart. Each integer form is the same overflow-checking instruction the
operator uses, with the `Option` built from its flag. An operator traps;
`checked_*` refuses.

## Default runtime behavior

By default, a trap:

1. writes a diagnostic to standard error;
2. identifies the operation or invalid index;
3. identifies the spawned fiber when the trap occurred on one;
4. terminates the process with status 134, unless a [`contain`](#containing-a-trap-with-contain) on the same fiber takes it.

A trap with no backtrace says how to get one, so the first thing a person sees
tells them what to do next:

```
khora: Int addition overflowed on fiber 7
note: re-run with KHORA_BACKTRACE=1 to see where
```

```bash
KHORA_BACKTRACE=1 khora run .
```

`RUST_BACKTRACE=1` is honored too, because a person who has debugged a Rust
program will try it. `KHORA_BACKTRACE` is the name to use and the one the
runtime's own note gives.

When debug information is available, the trace includes Khora source frames and locations. The runtime's own frames are trimmed from the top of it, so the first frame is the line in your program that trapped. Standard output is flushed before the process ends, so what the program had already printed is not lost.

A process-fatal trap is **not** normal structured failure unwinding. Do not depend on regions, `catch`, or application finalizers to recover from it. Resource correctness must come from normal return, typed failure, and cancellation paths; a fatal trap means the process is ending.

## Containing a trap with `contain`

`std::core::contain` runs a function on the current fiber and, if it traps, answers the trap instead of ending the process:

```khora
import std::core::{Result, Trapped, contain};

match contain(fn () => charge(account, amount)!)! {
  Result::Ok(receipt) => Response::json(200, receipt),
  Result::Err(trapped) => Response::text(500, "Internal Server Error"),
}
```

`contain(body)` returns `Result<A, Trapped>`. `Trapped` has one field, `message`, the sentence the trap printed (`Int addition overflowed`, `index 11 is outside an array of 3`).

**The trap leaves the way a cancellation does.** Every frame between the trap and the `contain` is left: what each one owned is released, every region opened in them runs its finalizers once, and a `Shared::update` or `Shared::modify` whose change function trapped leaves the cell holding its old value. Nothing is left half-released, so a server can contain a trap per request indefinitely without leaking.

**Only a trap becomes `Err`.** An error raised inside `body` goes on as an error, out of `contain`, to whatever handles it; a cancellation goes on as a cancellation. A `catch` or an `attempt` inside `body` does not see a trap: it passes through them to the `contain`.

**The trap is still reported.** It is printed to standard error as it would have been, followed by `(contained; the fiber continues)`:

```
khora: Int addition overflowed on fiber 7 (contained; the fiber continues)
```

**What `body` may capture is what a spawned fiber's body may.** A `mut` record, an `Array` or a `Region` from outside cannot be captured, because a trap halfway through writing one would leave it half-written after `contain` returned. A `Shared` cell, a `Channel` and immutable values are fine; `Shared` is the way to change something from inside a `contain`, because its update is all-or-nothing.

**`contain` is called directly.** It may be imported under another name, but it cannot be bound to a variable, stored, or passed to another function: the capture rule is checked where the call is written, and a call through a value would skip it.

**So a helper that takes a closure and hands it to `contain` cannot be written.** The closure arrived under a name, and what it captured was decided where it was written, which `contain` cannot see from here. The helper is refused, whichever way it forwards the closure:

<!-- unchecked: this is the helper the compiler refuses -->
```khora
fn guard<A>(body: () -> A) -> Result<A, Trapped> {
  contain(body)
}
```

```text
error: this has to be a closure written here or a named function, so that what it closes over
can be checked — a closure that arrived under a name captured whatever it captured somewhere else
```

Wrapping it, `contain(fn () => body())`, is refused because the new closure captures `body`:

```text
error: `body` cannot be captured by `contain`'s body, which a trap may leave part-way through
writing it: `() -> A` holds a closure, and what a closure captured is not in its type
```

Write `contain` where the work is written, around the closure itself, or pass a named function: `contain(handle_one)`.

**Still fatal, with status 134:**

- a trap on a fiber with no `contain` of its own. A spawned fiber, and each branch of a `race`, is its own fiber and does not inherit its parent's `contain`; the branches of a `select` run on the selecting fiber and do.
- a trap in a finalizer, including one inside a `contain` written in the finalizer: nothing is contained while a finalizer runs. A finalizer that the unwind of a contained trap runs, and that traps itself, ends the process too; stderr then shows the first trap's `(contained; the fiber continues)` line, written when that trap was taken, just before the second trap's line.
- the runtime's own fatal errors.
- any trap once the process has contained 1,000 in the last 60 seconds. `KHORA_TRAP_BUDGET=N` sets another count, and `N/SECONDS` another window. A bug a request can reach over and over then ends the process, where a supervisor sees it, rather than turning into a stream of 500s.

Running out of stack is not a trap and is not contained: the process prints `the stack ran out` and ends with the platform's stack-overflow status (`SIGSEGV`, which a shell reports as 139, on Unix), as in [Exit statuses](#exit-statuses). On WebAssembly nothing is contained, and `contain` runs `body` as an ordinary call.

**What it costs.** Each place that can trap is a way out of its function, as a loop or a `!` is, so a function that does checked arithmetic or indexes an array returns a tag that its callers test. Where such a function is inlined into a loop, the optimizer removes the test, and the trap path is out of the way of the code that does not trap.

## Exit statuses

A trap is one of several ways a program ends without returning from `main`, and
they are told apart from outside by the status:

| status | what happened |
| --- | --- |
| the value | `main` returned an `Int`, truncated to what the platform's status holds. `()` is 0. |
| 1 | An error reached the entry point and nothing handled it. `main` has nowhere to hand one, so it ends here. |
| 130 | A **cancellation** reached the entry point. 128 + `SIGINT`, which is what a shell already means by "interrupted". |
| 134 | A trap, everything above — and the runtime's own fatal errors. 128 + `SIGABRT`, and 134 on Windows too rather than a native abort code. |
| the platform's | The stack ran out: `SIGSEGV` on Unix, which a shell reports as 139; `STATUS_STACK_OVERFLOW` on Windows, which a POSIX shell reports as 127. |

**130 is the one that surprises people**, because a cancellation is not a
failure and there is no `catch` for it. The common way to reach it is a `join`
on a fiber that was canceled: a canceled fiber has no answer, so the join
unwinds the joiner along with it, and if the joiner is `main` the program ends
there. `join_all` joins, so it ends the same way. `Fiber::wait` waits without
asking for an answer and does not do this — reach for it when what you needed
was "not before that finishes" rather than the value, and `Fiber::detach` when
you are no longer waiting at all. A cancellation that reaches a spawned
fiber's root stops that fiber and nothing else; only one that reaches `main`
ends the program.

**The runtime's fatal errors exit 134 rather than aborting.** `abort()` leaves
the status to the platform, and on Windows a POSIX shell reports that as 127 --
which is also what it reports for a stack overflow. Two unrelated deaths
arriving as one number is what this table exists to prevent, so the runtime
picks 134 the way a trap does. Stack exhaustion is the one death that cannot
pick anything: its handler runs with the stack already gone, so it may not
allocate, lock, or call `exit`. It reports and lets the platform end the
process, which is why its row says "the platform's".

A supervisor should treat 130 as a deliberate stop and 134 as a crash. They are
different outcomes and this is where the difference is visible.

**Read them with `waitpid`, not `$?`, where the difference matters.** 128 + *N*
is a shell's way of reporting a child *killed by signal N*; a Khora program that
exits 130 was not signaled, it chose that number. POSIX keeps the two apart --
`WIFSIGNALED` is true of a real `SIGINT`, and `WIFEXITED` with status 130 is
true of this -- but a shell collapses both into one `$?`. So a script cannot
tell "somebody pressed Ctrl-C" from "a cancellation reached the entry point",
and a supervisor that cares should read the wait status rather than the shell.
**A signaled Khora program produces both, in order**: the first `SIGTERM` or
`SIGINT` becomes a cancellation and the program *exits* 130, and a second one
restores the default disposition and re-raises, so what a supervisor then sees
is a genuine `WIFSIGNALED` — not an `exit` imitating one.
On Windows there are no signals in this sense at all: the numbers above are
Khora's own, chosen to match what a Unix shell would have said.

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

## What the export boundary discards

An exported C function has a deliberately narrow signature: scalars and `Ptr` cross the boundary; exported functions are not generic and do not expose Khora values, capability rows, or typed failures to the host. That makes the exported call a boundary whose newly created Khora allocations can be discarded without leaving a Khora object in the caller.

If a guarded exported call spawns a fiber, containment is disarmed for that call. A spawned child may outlive the C call and may hold references to allocations made by it; freeing those allocations underneath the child would be unsound. In that case a trap retains the ordinary process-fatal behavior.

Inside a program, [`contain`](#containing-a-trap-with-contain) contains a trap on the fiber that hit it. The export boundary is for a host that calls into Khora from C and has no fiber of its own to put a `contain` on.

## What this means for a server

**A trap in a request handler is that request's 500.** `Router` runs each
request inside a `contain`, so a handler that overflows, indexes outside an
array or divides by zero answers `500 Internal Server Error`, and the requests
beside it and after it are answered as usual. The trap is printed to standard
error with the fiber and `(contained; the fiber continues)`; the client is told
nothing about it, because the sentence can name an index or a value.

The unit is the request, not the connection: on a keep-alive connection whose
third request traps, the first two have been answered and the fourth is read
as usual.

`Router::uncontained` turns this off, and then the first trap in a handler ends
the process with status 134, taking every request in flight with it:

```khora
Router::new()
  |> Router::uncontained
  |> Router::get("/", SharedFn::of(index))
  |> Router::listen(8080)!
```

A contained trap is still a bug. Design for it in three places.

**Validate at the boundary, not in the handler.** An integer that came from a
request is external input until a schema has read it. `std::schema` rejects a
port outside 1 to 65535 as a 400; the same value multiplied inside a handler
traps, and the client gets a 500 that says nothing useful. Arithmetic on such a
value can use the `checked_*` forms above, which answer `None` where `*` would
trap. This is the reason the standard library's own HTTP reader caps a
`Content-Length` before multiplying it, answering 413 rather than trapping.

**Watch the trap budget.** Once the process has contained 1,000 traps in 60
seconds, the next one ends it with status 134. A bug a client can reach over and
over -- or deliberately -- therefore becomes a crash a supervisor sees, rather
than a stream of 500s discovered in a log a week later. `KHORA_TRAP_BUDGET=N`
sets another count and `N/SECONDS` another window.

**Run more than one process, and expect a restart.** What `contain` leaves fatal
still ends the process: running out of stack, a trap in a finalizer or in a
fiber a handler spawned without a `contain` of its own, the runtime's own fatal
errors, and a spent budget. Status 134 is a crash as far as a supervisor,
container runtime or service manager is concerned, and the right response is a
restart. Hold no state in the process that a restart cannot rebuild or that a
second instance cannot serve from.

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
