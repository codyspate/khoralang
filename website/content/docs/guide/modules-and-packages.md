---
title: Modules and packages
sidebar:
  order: 12
---

Khora packages group source code, dependencies, and compiler configuration behind a `khora.toml` manifest. Modules give names and visibility to the source inside a package.

## Package manifest

A minimal package looks like:

```toml
[package]
name = "orders"
version = "0.1.0"
edition = "2026"
```

Source lives under `src/`. An executable package normally has a module containing `main`.

See [Your first Khora project](/docs/getting-started/first-project/) for a complete minimal package.

## Modules, paths, and fields

Khora separates compile-time paths from runtime field access:

- use `::` for modules, types, constructors, and associated items;
- use `.` for projecting a field from a runtime value.

That keeps a name such as `http::Response` visibly different from a field access such as `response.status`.

Imports bring names into the current module explicitly. For example:

```khora
import std::core::{print};
```

Trait visibility can affect method and operator resolution, so an imported trait is part of the local meaning of an expression rather than something discovered from a process-wide registry.

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

## Public API design

Export the domain types and operations callers need; keep implementation helpers private. Capabilities are especially useful at package boundaries because a signature can state the external authority a package needs without exposing how a particular application provides it.

Adding a required capability or a new recoverable failure to a public function changes what callers must provide or handle, so treat those changes as part of the package's public compatibility surface.

## Source compatibility and native ABI

Khora packages distribute source and are compiled as part of the consuming program. Whole-program monomorphization and native optimization mean Khora does not define a stable Khora-to-Khora binary ABI for separately compiled package binaries.

When a stable binary boundary is required, use the C-compatible `extern` FFI boundary documented in [FFI](/docs/reference/ffi/).

Continue with [Testing](/docs/guide/testing/) for package tests or the [Language Reference](/docs/reference/) for exact module and type-system rules.
