---
title: Testing
sidebar:
  order: 11
---

Khora's effect and capability model makes tests explicit about the outside world they depend on.

Pure functions need ordinary input/output tests. Effectful code should usually be tested by supplying small handlers or in-memory implementations for the capabilities the unit uses.

For example, application code depending on `Db`, `Clock`, or tracing should not need a real database, wall clock, or telemetry backend merely to exercise its domain decisions. A test can provide a deterministic handler for the same capability contract.

## Test behavior, not implementation plumbing

Prefer assertions over returned values and externally visible effects rather than over internal compiler/runtime details. A database transaction test, for example, should verify commit versus rollback behavior and the failure returned to the caller.

## Include failure and cancellation paths

Khora's strongest guarantees matter when control flow does not return normally. Tests for resources, transactions, and concurrent code should include typed failure and cancellation, not only the successful path.

## Compiler-backed examples

Public documentation examples that claim to compile should eventually be checked by `khora doc --check`. Until that command lands, documentation changes should be validated against the compiler manually or in site CI where practical.
