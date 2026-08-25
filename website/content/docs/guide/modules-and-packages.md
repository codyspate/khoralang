---
title: Modules and packages
sidebar:
  order: 12
---

Khora separates compile-time paths from runtime field access. Use `::` for module, type, and associated-item paths; use `.` for projecting a field from a runtime value.

That distinction keeps names such as `http::Response` visibly different from `response.status`.

A package is described by `khora.toml`. Dependencies are reproducible: the current package system resolves git sources to exact revisions and records the fetched content in `khora.lock` with a SHA-256 digest.

## Imports and traits

Bring the names and traits a module needs into scope rather than relying on process-global visibility. Trait visibility can affect method and operator resolution, so an imported trait is part of the local meaning of an expression.

## Public API design

Keep package boundaries narrow. Export domain types and operations callers need; keep implementation helpers private. Capabilities are especially useful at package boundaries because a signature can state the external authority a package needs without exposing how a particular application provides it.

The first public release will document the package/version compatibility rules alongside the package tooling. Until then, exact revisions are the reproducibility mechanism.
