---
title: Tracing
sidebar:
  order: 4
---

Khora's tracing vocabulary lives in `std`; exporters belong in packages.

A request entering through HTTP should parse valid W3C `traceparent` context, create or continue a span, and make that context part of the request fiber. Child fibers inherit the relevant context so a scheduler steal or suspension does not break the trace.

## No-op by default

Tracing should be cheap when disabled. Applications can install a real tracer handler at the boundary while libraries program against the common `Tracer` capability and vocabulary.

## Put spans around meaningful operations

Good spans correspond to work an operator cares about: an inbound request, database query/transaction, external RPC, or substantial background job. Avoid creating spans around every tiny pure helper merely because instrumentation is available.

## Export outside `std`

OpenTelemetry/OTLP and vendor-specific exporters should implement the standard tracing contract as packages. That lets the core vocabulary remain stable while protocols and vendor SDKs evolve independently.

Malformed incoming trace context should be rejected rather than partially interpreted and attached to the wrong trace.
