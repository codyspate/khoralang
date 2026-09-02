---
title: Declarations
sidebar:
  order: 2
---

This page lists the declaration forms accepted at module scope. Local bindings and expression forms are covered in [Expressions](./expressions/).

## Module declaration

```khora
module app::users;
```

A module path uses `::` between segments. The module declaration belongs at the beginning of the source file.

## Imports

Selected names:

```khora
import std::core::{List, Option, print};
```

Aliased name:

```khora
import app::storage::{User as StoredUser};
```

Glob import:

```khora
import app::prelude::*;
```

Grouped imports allow a trailing comma.

## Visibility

`pub` makes a declaration visible outside its declaring module:

```khora
pub fn parse(input: String) -> Value {
  // ...
}
```

Without `pub`, a top-level declaration is module-private. Current Khora uses `pub`; `export` is not the public-visibility keyword.

## Constants

```khora
const NAME = "khora";
pub const DEFAULT_PORT: Int = 8080;
```

General form:

```text
pub? const Pattern (: Type)? = Expr ;
```

A module-level binding is a `const`, not a `let`. `const mut` is invalid; Khora has no mutable global binding.

Reach for `const` for a genuine constant — a limit, a protocol value, a fixed
default — and for `let` for a value computed while a function runs.

## Type declarations

Alias or structural definition:

```khora
pub type UserId = Int;

pub type User = {
  id: UserId,
  name: String,
};
```

Variant type:

```khora
pub type Result<A, E> =
  | Ok(value: A)
  | Err(error: E);
```

Opaque declaration:

```khora
pub type Handle;
```

General form:

```text
derive(...)? pub? type Name<TypeParams>? (= TypeDefinition)? ;
```

## `derive(...)`

A derive clause appears immediately before the type it applies to:

```khora
derive(Eq, Ord, Show, Hash)
pub type Point = {
  x: Int,
  y: Int,
};
```

The syntax accepts a comma-separated list of trait names and an optional trailing comma. The compiler-supported derivable traits are `Eq`, `Ord`, `Show`, `Hash`, `ToJson`, and `FromJson`.

The trait must be in scope, so `derive(Show)` needs `Show` imported from
`std::core`. Derive where the implementation follows from the fields, and write
an `impl` where the behaviour is a domain decision rather than a structural
consequence of the data.

A field's type decides whether the derive is available at all, and a missing
impl is sometimes the point:

- `List<A>` has `Show` and `Eq` when `A` does, so a record holding a list
  derives both, and `ToJson`/`FromJson` from `std::json` on the same terms.
- `Redacted<A>` has `Show` — it prints `<redacted>` — and deliberately no
  `ToJson`. A record holding a secret stays printable and refuses to serialise,
  so the build stops rather than the payload leaking. It has no `Eq` either:
  comparing two secrets byte by byte is how a timing side channel gets written
  by somebody who was not writing one.

## Function declarations

Definition:

```khora
pub fn add(left: Int, right: Int) -> Int {
  left + right
}
```

Generic and effectful definition:

```khora
pub fn load<A>(id: Id) -> A
  with { store: Store }
  raises StoreError
{
  store.load(id)!
}
```

Signature without a body:

```khora
fn intrinsic(value: Int) -> Int;
```

General form:

```text
pub? fn Name<TypeParams>? (Params) (-> Type)? EffectClause* (Block | ;)
```

There is no `=` between a function signature and its block body, and a function definition has no semicolon after the block.

The clauses are read in order. `pub fn load<A>(id: Id) -> A with { store: Store }
raises StoreError` says: given an `Id` it produces an `A`, it requires a
capability named `store` implementing `Store`, and it may fail with
`StoreError`. The two rows are independent dimensions of the type — see
[Capabilities](./capabilities/) and [Failures](./failures/).

A public signature states both. A private helper can usually let the compiler
infer them, and inference is why a row variable appears in a signature nobody
wrote one into.

## External C functions

Declare a C symbol with contextual `extern` before `fn`:

```khora
extern fn strlen(ptr: Ptr) -> U64;
```

A public external function can form part of a generated C-compatible library surface:

```khora
pub extern fn khora_add(left: Int, right: Int) -> Int;
```

See [FFI](./ffi/) for ABI rules and supported boundary types.

## Effect declarations

```khora
pub effect Store {
  load: Id -> User raises StoreError,
  save: User -> () raises StoreError,
}
```

General form:

```text
pub? effect Name<TypeParams>? {
  operation: FunctionType,
  ...
}
```

Effect members are named function types separated by commas.

## Context declarations

```khora
pub context Production {
  scope: Scope::root(),
  db: postgres_db()!,
  store: sql_store(),
}
```

General form:

```text
pub? context Name {
  label: Expr,
  ...
}
```

Bindings are evaluated in order; a later binding may use capabilities introduced above it.

## Trait declarations

```khora
pub trait Named {
  fn name(self) -> String;
}
```

With supertraits and an associated type:

```khora
pub trait Iterator: Show {
  type Item;
  fn next(self) -> Step<Self, Self::Item>;
}
```

Trait functions may be signatures or have default bodies.

## Implementations

Trait implementation:

```khora
impl Named for User {
  fn name(self) -> String {
    self.name
  }
}
```

Inherent implementation:

```khora
impl User {
  pub fn display_name(self) -> String {
    self.name
  }
}
```

Generic implementations may put type parameters after `impl`:

```khora
impl<A: Show> Show for Box<A> {
  fn show(self) -> String {
    self.value.show()
  }
}
```

## Associated type definitions

Inside a trait:

```khora
type Item;
type Item: Show;
```

Inside an implementation:

```khora
type Item = User;
```

## Tests

```khora
test "addition works" {
  assert(add(20, 22) == 42);
}
```

General form:

```text
test "name" Block
```

## Benchmarks

```khora
bench "parse payload" {
  parse_payload(fixture)!;
}
```

General form:

```text
bench "name" Block
```

`test`, `bench`, `context`, `extern`, and `derive` are contextual keywords in their declaration positions rather than globally reserved identifiers.