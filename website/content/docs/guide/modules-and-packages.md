---
title: Modules and packages
sidebar:
  order: 13
---

Khora packages group source code, dependencies, and compiler configuration behind a `khora.toml` manifest. Modules give stable paths and visibility to the declarations inside a package.

## Package manifest

A minimal package looks like:

```toml
[package]
name = "orders"
version = "0.1.0"
edition = "2026"
```

Source lives under `src/`. See [Your first Khora project](/docs/getting-started/first-project/) for a complete minimal package.

## Declare a module

A source file begins with its module path:

```khora
module app::orders;
```

`::` separates compile-time path segments. The module declaration belongs at the start of the file.

## Import selected names

Import names explicitly from another module:

```khora
import std::core::{List, Option, print};
```

A trailing comma is allowed:

```khora
import std::core::{
  List,
  Option,
  print,
};
```

## Rename an import with `as`

```khora
import app::storage::{User as StoredUser};
```

The alias is the name used in the importing module.

## Glob imports

Import every public name from a module with `::*`:

```khora
import app::prelude::*;
```

Prefer selected imports in most application code because they make dependencies visible. A glob is most useful for deliberately curated prelude-style modules.

## Paths versus fields

Khora deliberately uses different syntax for compile-time names and runtime values:

```khora
let response = http::Response::text(200, "ok");
print(Int::to_string(response.status));
```

- `::` walks modules, types, constructors, and associated items.
- `.` projects a field or calls behavior on a runtime value.

That keeps `Response::text` visibly different from `response.status`.

## Public declarations with `pub`

Top-level declarations are module-private unless marked `pub`:

```khora
const INTERNAL_LIMIT: Int = 10;

pub const DEFAULT_LIMIT: Int = 50;

fn normalize(input: String) -> String {
  input |> String::trim
}

pub fn parse(input: String) -> String {
  normalize(input)
}
```

The same modifier applies to types, traits, effects, contexts, functions, constants, and public inherent methods:

```khora
pub type User = {
  id: Int,
  name: String,
};

pub trait Named {
  fn name(self) -> String;
}
```

Current Khora spells public visibility `pub`. Older source using `export` should be updated to `pub`.

## Module-level values are `const`

`let` is a local binding. At module scope, declare a named constant with `const`:

```khora
const SERVICE_NAME = "orders";
```

Khora does not have a mutable global binding. Shared evolving state is explicit through `Shared` or a capability.

## Dependencies and lockfiles

Package dependencies are source dependencies. Git dependencies resolve to exact revisions, and `khora.lock` records the resolved content with a digest so the same package graph can be reproduced later.

Commit the lockfile for applications and other projects where reproducible builds matter. When a dependency is intentionally updated, review the lockfile change like any other dependency change.

## Toolchain pinning

A package can pin the Khora compiler it expects:

```toml
[toolchain]
version = "0.2.0"
```

The pin takes precedence over the machine default. See [Installation](/docs/getting-started/installation/#pin-a-project-to-a-compiler-version) for toolchain management.

## Package boundaries and API design

Expose the domain types and operations callers need and keep helpers private. Capabilities are useful at package boundaries because a signature can state the external authority a package needs without exposing how an application provides it.

Adding a required capability or a recoverable failure to a public function changes what callers must provide or handle, so treat those changes as part of the package's public compatibility surface.

## Native binary boundaries

Khora packages distribute source and are compiled as part of the consuming program. Whole-program monomorphization and native optimization mean Khora does not define a stable Khora-to-Khora binary ABI for separately compiled package binaries.

When a stable C-compatible boundary is required, use `extern fn` and the library build flow described in [FFI](/docs/reference/ffi/).

Continue with [Testing](./testing/) for package tests and benchmarks or the [Language Reference](/docs/reference/) for exact declaration syntax.