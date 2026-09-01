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

### A package's other programs

`src/main.kh` is the package's program. A package that needs more than one — a
migration, a backfill, a one-off report — puts each in its own file under
`src/bin/`:

```text
myapp/
  khora.toml
  src/
    main.kh          -> build/myapp.exe
    shared.kh        the modules both use
    bin/
      backfill.kh    -> build/backfill.exe
      report.kh      -> build/report.exe
```

`khora build .` builds all of them. Each is its own compilation: it gets the
package's modules and **not** the other programs, which is what stops two
`main` functions from meeting. A program is named after its file, so
`src/bin/backfill.kh` is `build/backfill.exe`.

One file per program. A program that needs several modules of its own is a
package, and that is the shape to reach for rather than a directory inside
`src/bin`.

`khora run .` runs the package's own program. To run one of the others, name
it:

```bash
khora run src/bin/backfill.kh
```

A package with no `src/main.kh` has no default program, and `khora run .` says
which ones it has instead of reporting that it has none.

### Fields need the type imported

Importing a function that *returns* a record is not enough to read the record's fields. The type has to be in scope too:

```khora
// Not enough: `origin()` is callable, but `.x` is not readable.
import shapes::{origin};

// Both.
import shapes::{Point, origin};
```

The rule is that a field is looked up on a type, and a type the file cannot name is a type it knows nothing about — including what fields it has. The compiler says so directly (``Point is not in scope here, so nothing is known about its fields``), but it is easier to import the pair from the start.

The same applies to a trait's methods and to an effect's operations: a capability can arrive in your function without its type being named anywhere, and calling an operation on it needs the effect imported.

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

Package dependencies are source dependencies: Khora fetches and compiles the package's source, and there are no binary artifacts to publish or trust. Declare one in `[dependencies]`:

```toml
[dependencies]
# From a git repository, at a branch, tag or commit.
postgres = { git = "https://github.com/codyspate/khoralang", rev = "main", subdir = "packages/postgres" }

# From a directory on this machine, for a package you are also editing.
shared = { path = "../shared" }
```

`subdir` is for a repository that holds more than the one package — a git URL names a repository, and the two are only the same thing in the simplest layout.

There is no registry yet, so `version = "..."` has nothing to resolve against. Use `git` for anything you did not write and `path` for anything you did.

`khora build` resolves what it needs, so there is no fetch step to remember. To add a dependency without editing the manifest by hand:

```bash
khora install https://github.com/codyspate/khoralang --subdir packages/postgres
```

That finds out the package's real name and whether it offers itself at all *before* writing the entry — two things you cannot check by typing a line into `[dependencies]`. `--rev` takes a branch, tag or commit and defaults to `main`. With no URL, `khora install` fetches and locks whatever the manifest already declares, which is the command to run after cloning a project.

`khora why <package>` explains what pulled something in, and `khora graph` draws the whole thing.

### The lockfile

Resolution writes `khora.lock`, and a locked build never asks the network again:

```toml
[[package]]
name = "postgres"
source = "git"
url = "https://github.com/codyspate/khoralang"
revision = "0fcf6d65a2cf8c1b1636da586ed47839152c315c"
path = "packages/postgres"
checksum = "001bf5bf28448ba94bd6c08d2a8a3c55535692be5c61113fbfd94df74ff1ff55"
```

A branch name resolves to the commit it pointed at, and the commit is what is recorded — so `rev = "main"` is a convenience at the moment you add the dependency, not a moving target afterwards.

**The checksum is verified, not just recorded.** Every resolution hashes what arrived and compares it against the lockfile. If the same commit id ever produces different bytes, the build stops and says so rather than compiling what turned up.

Commit the lockfile for applications and other projects where reproducible builds matter. When a dependency is intentionally updated, review the lockfile change like any other dependency change.

### Publishing a package

A package is consumable when its manifest says so:

```toml
[package]
name = "postgres"
version = "0.1.0"
publish = true
```

**Absent means no.** Publishing here is passive — a pushed repository is already fetchable — so the marker records an intention rather than granting a permission. What it prevents is depending on somebody's application, or their unfinished experiment, because it happened to sit in a repository you fetched.

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