---
title: Cookbook
sidebar:
  order: 0
---

The Cookbook combines Khora language features and shipped standard-library APIs into complete application patterns. Each recipe includes the imports, types, functions, and boundary wiring needed to understand the example as a whole instead of showing an isolated call with important pieces omitted.

Start with the recipe closest to the problem you are solving:

<!-- Ordered as the sidebar is: each bullet's position here is the `sidebar.order`
     in that recipe's frontmatter. They drifted apart once -- two recipes shared
     order 6, two numbers went unused, and `logging.md` was missing from this
     list entirely -- so if you add a recipe, add it in both places. -->

- [Build an HTTP service](/docs/cookbook/http-service/) — route requests with `Router`, `Request`, `Response`, and `SharedFn`.
- [Build a typed JSON API](/docs/cookbook/json-api/) — parse JSON, decode typed request bodies, derive encoders/decoders, and return JSON responses.
- [Run a database transaction](/docs/cookbook/database-transactions/) — use the portable `Db` capability and `transaction` so failure and cancellation cannot leak an open transaction.
- [Take work off a queue safely](/docs/cookbook/taking-work-off-a-queue/) — register a job with a region before the first `!` after it, so a cancellation cannot drop work nobody knows the fiber was holding.
- [Bound concurrent work](/docs/cookbook/bounded-concurrency/) — use `Fiber`, `Nursery`, and `bounded_nursery` to turn a concurrency limit into backpressure.
- [Make resource cleanup cancellation-safe](/docs/cookbook/cancellation-safe-resources/) — pair `scoped` and `acquire` so cleanup runs on return, failure, and cancellation.
- [Decode untrusted input](/docs/cookbook/decoding-input/) — describe a value's shape once with `std::schema`, decode it from anywhere, and report every bad field at once.
- [Load application configuration](/docs/cookbook/configuration/) — read settings with `std::config`, report every bad key at once, and keep secrets out of the logs with `Redacted`.
- [Retry a flaky call](/docs/cookbook/retrying/) — pick a `Schedule`, drive it with `retry_while`, and test the whole thing in under a millisecond.
- [Test code that uses capabilities](/docs/cookbook/testing-capabilities/) — supply deterministic handlers without global mutation or test-only branches in application code.
- [Trace an operation](/docs/cookbook/tracing/) — implement a `Tracer`, wrap work with `around`, and keep span lifetime structured.
- [Log, and correlate it with traces](/docs/cookbook/logging/) — emit one JSON object per line with `std::log`, and carry the trace and span identifiers onto every line inside a span.

## How to read the examples

The examples use current Khora syntax and the public APIs shipped in `std`. They favor small complete modules over framework-style pseudocode. When a recipe depends on a provider outside `std`—for example a concrete PostgreSQL connection—it says so at the construction boundary instead of inventing a driver API.

For exact declarations after you understand the pattern, use the [Standard Library API reference](/docs/stdlib/). For the language rules underneath the pattern, use the [Language Reference](/docs/reference/).
