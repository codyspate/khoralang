---
title: Capabilities
sidebar:
  order: 9
---

Capabilities represent external authority required by a computation and appear in a function's `with` row.

A capability may represent database access, a clock, tracing, filesystem authority, or another effectful service. Handlers provide implementations for a lexical scope.

Capabilities are not global service locators: the function type records that authority is required, generic/effectful composition preserves the row, and tests can provide alternate handlers for the same contract.

Prefer capability interfaces that express domain-relevant operations over large bags of unrelated infrastructure methods. Narrow capabilities make authority easier to audit and tests easier to construct.

Compile-time permission to declare foreign operations is a separate concern from capability requirements and does not make the runtime a security sandbox.
