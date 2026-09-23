---
title: From Rust
sidebar:
  order: 3
---

Khora shares Rust's goal of native, predictable software without requiring a tracing GC, but it deliberately does not expose Rust's ownership proof system as the ordinary programmer interface.

## There are no lifetimes to write

Memory is reference-counted, and the compiler removes the counts it can prove
are unnecessary and reuses storage it can prove is uniquely owned. So ordinary
data flow needs no annotation: no `&`, no `'a`, no `Rc<RefCell<..>>` to get a
value into two places. What you give up is the proof — Khora will keep a count
where Rust would have shown you why one was not needed.

## `Result` and dependency arguments become rows

What Rust puts in the return type and the parameter list, Khora puts in two
rows beside them: `raises` for recoverable failure, `with` for external
authority.

```rust
// Rust
fn load_user(id: Id, db: &Pool) -> Result<User, UserError> {
    let row = db.query(id)?;
    Ok(User::from(row))
}
```

```khora
// Khora
fn load_user(id: Id) -> User with { db: Db } raises UserError {
  User::of(db.query(id)!)
}
```

**`?` becomes postfix `!`, and leaving it off is a compile error** — the
compiler names the call and tells you to write it. The rows stay visible in the
function's type, and neither has to be threaded through every intermediate
value: a `?` on every call and a `db: &Pool` on every signature are the two
things this is trying not to be.

`catch` handles part of a row by name, the way `match` on a `Result` does:

```khora
let row = load_user(id)! catch {
  UserError::Missing(_) => User::anonymous(),
};
```

## `Arc<Mutex<T>>` becomes `Shared<A>`

```rust
let hits = Arc::new(Mutex::new(0));           // Rust
```

```khora
let hits = Shared::of(0);                     // Khora
```

A value captured by a spawned fiber must be `Share`. `Shared<A>` is, and takes
a lock; a record with a `mut` field is not, and the compiler refuses the
capture rather than letting two fibers race. `Shared::update` is the
read-modify-write that happens under the lock — `set(get(cell) + 1)` is two
steps and can lose an increment, exactly as in Rust.

**There is no `Rc`/`Arc` split to choose between.** Reference counts are always
atomic, so there is no cheaper single-threaded variant to reach for and no type
that changes when a value starts crossing fibers. [Memory](/docs/internals/memory/)
says what that costs.

## There is no `async fn`

Fibers are structured and I/O is direct-style, so a function is not written
twice. Suspension is something the runtime does, not a second colour of
function that divides the library in half.

## Visibility

`pub` works the way it does in Rust, in both of the places you would expect it.
A declaration without it belongs to its module, and a method without it belongs
to the module that declares the type:

```khora
pub type Counter = { n: Int };

impl Counter {
  pub fn doubled(self) -> Int { Counter::secret(self) }
  fn secret(self) -> Int { self.n * 2 }
}
```

As in Rust, a method of a *trait* implementation takes no `pub` — what makes it
reachable is the trait, not the impl. Writing the keyword on one method of an
`impl Show for T` would suggest the others were hidden.

Khora's module path separator is `::` as in Rust, but the declaration is
`module a::b;` at the top of a file and the import is `import a::b::{X};` — the
file is the module, so there is no `mod` tree to keep in step with the
directory layout.

## FFI: `extern fn`, and no `unsafe` keyword

There is no stable Khora-to-Khora ABI, for the same reason Rust has none: data
layout and calling convention are the compiler's to choose, and it reserves the
right to change them. The interoperability boundary is the C ABI, and
it is spelled the way Rust spells it minus the block. An `extern fn` with no
body is a symbol the linker must find; a `pub extern fn` with a body is a Khora
function C can call:

```khora
extern fn monotonic_ticks() -> Int;
```

What replaces `unsafe` is the manifest. `extern` in `[permissions]` names the
**packages** that may declare `extern fn` at all — `std` always may — so the
audit is one table rather than a search for a keyword, and a dependency that
starts calling into C cannot do it quietly. [FFI](/docs/reference/ffi/) has the
ABI-safe types, the rules for borrowing a buffer for the duration of one call,
what a blocking foreign call costs a fiber, and what a trap does at an exported
boundary. [Capabilities](/docs/reference/capabilities/) has the permission
table.

Khora is not intended to replace Rust for every low-level kernel or embedded
use case. Its target is native application and service software where ownership
complexity costs more than it returns — and the honest version of that trade is
on [Limitations](/docs/limitations/), which is the page to read before choosing
between them.
