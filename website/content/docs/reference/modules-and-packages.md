---
title: Modules and packages
sidebar:
  order: 18
---

A Khora package groups source, dependencies and compiler configuration behind a
`khora.toml` manifest. Modules give stable paths and visibility to the
declarations inside it.

The syntax of `module`, `import`, `pub` and `const` is in
[Declarations](./declarations/). This page is what surrounds it: the manifest,
the programs a package builds, dependency resolution, and where the boundary of
a package falls.

## Package manifest

A minimal package:

```toml
[package]
name = "orders"
version = "0.1.0"

[toolchain]
version = "0.2.0"
```

Source lives under `src/`. [The manifest](/docs/reference/manifest/) documents
every table; [your first Khora project](/docs/getting-started/first-project/)
builds one end to end.

## Paths versus fields

Khora spells compile-time names and runtime values differently, and the
difference is load-bearing when reading unfamiliar code:

```khora
let response = http::Response::text(200, "ok");
print(Int::to_string(response.status));
```

- `::` walks modules, types, constructors and associated items;
- `.` projects a field, or calls behaviour on a runtime value.

So `Response::text` is visibly not `response.status`.

### A field needs its type imported

Importing a function that *returns* a record is not enough to read that
record's fields. The type has to be in scope as well:

```khora
// Not enough: `origin()` is callable, but `.x` is not readable.
import shapes::{origin};

// Both.
import shapes::{Point, origin};
```

A field is looked up on a type, and a type the file cannot name is one it knows
nothing about, its fields included. The compiler says so directly — `Point is
not in scope here, so nothing is known about its fields` — but importing the
pair from the start is easier.

The same applies to a trait's methods and to an effect's operations. A
capability can arrive in a function without its type being named anywhere, and
calling an operation on it needs the effect imported.

## A package's other programs

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

One file per program. A program needing several modules of its own is a
package, and that is the shape to reach for rather than a directory inside
`src/bin`.

`khora run .` runs the package's own program. To run one of the others, name
it:

```bash
khora run src/bin/backfill.kh
```

A package with no `src/main.kh` has no default program, and `khora run .` says
which ones it does have rather than reporting that it has none.

## Dependencies

Package dependencies are source dependencies. Khora fetches and compiles the
package's source; there are no binary artifacts to publish or to trust.

```toml
[dependencies]
# From a git repository, at a branch, tag or commit.
postgres = { git = "https://github.com/codyspate/khoralang", rev = "main", subdir = "packages/postgres" }

# From a directory on this machine, for a package you are also editing.
shared = { path = "../shared" }
```

`subdir` is for a repository holding more than one package — a git URL names a
repository, and the two are the same thing only in the simplest layout.

There is no registry yet, so `version = "..."` has nothing to resolve against.
Use `git` for anything you did not write and `path` for anything you did.

`khora build` resolves what it needs, so there is no fetch step to remember. To
add a dependency without editing the manifest by hand:

```bash
khora install https://github.com/codyspate/khoralang --subdir packages/postgres
```

That finds out the package's real name and whether it offers itself at all
*before* writing the entry — two things a line typed into `[dependencies]`
cannot check. `--rev` takes a branch, tag or commit and defaults to `main`.
With no URL, `khora install` fetches and locks whatever the manifest already
declares, which is the command to run after cloning a project.

`khora why <package>` explains what pulled something in, and `khora graph`
draws the whole thing.

### The lockfile

Resolution writes `khora.lock`, and a locked build never asks the network
again:

```toml
[[package]]
name = "postgres"
source = "git"
url = "https://github.com/codyspate/khoralang"
revision = "0fcf6d65a2cf8c1b1636da586ed47839152c315c"
path = "packages/postgres"
checksum = "001bf5bf28448ba94bd6c08d2a8a3c55535692be5c61113fbfd94df74ff1ff55"
```

A branch name resolves to the commit it pointed at, and the commit is what is
recorded — so `rev = "main"` is a convenience at the moment the dependency is
added, not a moving target afterwards.

**The checksum is verified, not merely recorded.** Every resolution hashes what
arrived and compares it against the lockfile. If the same commit id ever
produces different bytes, the build stops and says so rather than compiling
what turned up.

Commit the lockfile for applications and anywhere else reproducible builds
matter, and review a lockfile change like any other dependency change.

### Publishing a package

A package is consumable when its manifest says so:

```toml
[package]
name = "postgres"
version = "0.1.0"
publish = true
```

**Absent means no.** Publishing here is passive — a pushed repository is
already fetchable — so the marker records an intention rather than granting a
permission. What it prevents is depending on somebody's application, or their
unfinished experiment, because it happened to sit in a repository you fetched.

## Toolchain pinning

Every project says which compiler builds it, and the field is required:

```toml
[toolchain]
version = "0.2.0"
```

The pin takes precedence over the machine default, and a pinned version that is
not installed stops the command rather than building with a different compiler.
In a workspace it belongs at the root, where members inherit it.

`latest` and `latest.rc` are also accepted, and mean the newest toolchain
installed on this machine — not reproducible, and useful while testing.
[The manifest](/docs/reference/manifest/#toolchain--which-khora-builds-this) has
the detail; [Installation](/docs/getting-started/installation/#pin-a-project-to-a-compiler-version)
covers toolchain management.

## Package boundaries

Expose the domain types and operations callers need, and keep helpers private.
Capabilities are useful at a package boundary because a signature can state the
external authority the package needs without prescribing how an application
supplies it.

Adding a required capability or a recoverable failure to a public function
changes what callers must provide or handle, so both belong to the package's
public compatibility surface. See [Compatibility and
stability](./compatibility/).

## Native binary boundaries

Khora packages distribute source and are compiled as part of the consuming
program. Whole-program monomorphization and native optimization mean Khora
defines no stable Khora-to-Khora binary ABI for separately compiled package
binaries.

Where a stable C-compatible boundary is required, use `extern fn` and the
library build flow in [FFI](./ffi/).
