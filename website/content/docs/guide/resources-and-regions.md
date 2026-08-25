---
title: Resources and regions
sidebar:
  order: 8
---

Resources such as sockets, files, transactions, and temporary allocations have lifetimes. Khora's region model ties cleanup to structured scope so normal return, typed failure, and cancellation can share the same cleanup path.

The important programmer-facing rule is simple: acquire a resource inside the scope that owns it, and register cleanup with that scope rather than relying on a distant caller to remember it.

A region's deferred cleanup runs when the region exits. That makes resource release part of control-flow semantics rather than an optional convention.

## Cancellation is an exit path

Structured concurrency means a fiber may be cancelled while suspended. Resource APIs must therefore be cancellation-safe: cancellation should unwind through the owning region and run finalizers instead of abandoning sockets, files, database locks, or pooled connections.

## Transactions

Database transactions are a canonical example. A successful body commits. An ordinary failure rolls back. Cancellation must also roll back before the connection returns to its pool.

That rule belongs in the shared transaction abstraction so every driver does not invent a different answer.

## Prefer scoped APIs

When designing packages, prefer an API that accepts a body to run inside an owned resource scope over an API that hands out a raw handle and hopes the caller closes it on every path.
