---
title: From Go
sidebar:
  order: 2
---

Go and Khora share an interest in deployable native services and operational simplicity, but they make different tradeoffs in the type system and concurrency model.

## Errors

Go returns ordinary error values by convention. Khora places recoverable failure in a `raises` row, so a caller cannot silently forget that a function may fail.

## Dependencies

Go commonly passes interfaces or concrete dependencies explicitly. Khora can represent external authority as capabilities in a `with` row and provide implementations with handlers.

## Concurrency

Go makes goroutines easy to start; their lifetime is conventionally managed by contexts, wait groups, and application discipline. Khora makes concurrency structured around nurseries so parent/child lifetime and cancellation are part of the model.

## Memory

Go uses a tracing garbage collector. Khora uses reference counting plus compiler ownership/reuse analysis and does not require a tracing GC at runtime.

## What remains familiar

Khora aims to preserve the parts service developers value in Go: direct-looking code, straightforward deployment, fast startup, understandable operational behavior, and a small number of runtime concepts visible in application code.
