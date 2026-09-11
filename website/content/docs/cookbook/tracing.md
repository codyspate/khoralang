---
title: Tracing
sidebar:
  order: 11
---

Wrap an operation in `around(tracer, name, fn () => ...)` and the span is
started before it and finished when it returns, raises, or is cancelled.

The vocabulary is in `std::trace`; exporters and vendor protocols live in
packages, so application code programs against `Tracer` regardless of where
completed spans are sent.

## Complete example

This module implements a small console tracer and uses `around` to guarantee the span is finished with the lifetime of the operation:

```khora
module main;

import std::core::{Option, print};
import std::random::{Random};
import std::trace::{Context, Span, Status, Tracer, around, current};

fn console_tracer(random: Random) -> Tracer {
  handler for Tracer {
    start: fn (name, _attributes) => {
      let id = random.int();

      let span = match current() {
        // Inside a span already: keep its trace and record it as the parent.
        Option::Some(inside) => {
          context: { trace_high: inside.trace_high, trace_low: inside.trace_low,
                     span: id, sampled: inside.sampled },
          parent: inside.span,
          name: name,
        },
        // At the top: begin a new trace.
        Option::None => {
          context: { trace_high: random.int(), trace_low: random.int(),
                     span: id, sampled: true },
          parent: 0,
          name: name,
        },
      };

      print("start ${name}: trace=${span.context.trace_id()} span=${span.context.span_id()}");
      span
    },

    finish: fn (span, status) => {
      match status {
        Status::Ok =>
          print("finish span: ${span.name}"),

        Status::Failed(reason) =>
          print("fail span ${span.name}: ${reason}"),
      }
    },

    event: fn (span, name, _attributes) =>
      print("event ${span.name}: ${name}"),
  }
}

fn calculate(tracer: Tracer) -> Int {
  print("doing work");
  around(tracer, "inner", fn () => 42)
}

pub fn main() {
  let tracer = console_tracer(Random::real());
  let result = around(tracer, "calculate", fn () => calculate(tracer));

  print("result = ${result}");
}
```

```text
start calculate: trace=bf0d815f869cef498398871c54234801 span=6fdab51de189b88d
doing work
start inner: trace=bf0d815f869cef498398871c54234801 span=d52ddfe412d2d513
finish span: inner
finish span: calculate
result = 42
```

The application decides which tracer implementation to construct. `around` owns the span lifetime:

```khora
let result = around(tracer, "calculate", fn () => calculate(tracer));
```

It starts the span before running `calculate` and registers cleanup so the span is finished when the operation returns, raises, or is cancelled. A caller should not rely on a later `tracer.finish(...)` line running after arbitrary fallible work.

## `start` has to ask what it is inside

The one thing a handler must do is call [`current`](/docs/stdlib/api/trace/) in
`start`, and it is why `console_tracer` takes a `Random`: a span needs an id
nobody else has, and a handler may be handed to another fiber, so it cannot
count with a `mut` field. Capturing a capability is what a handler is allowed
to do.

A `start` that instead writes

```khora
{ context: Context::none(), parent: 0, name: name }
```

compiles and produces no trace. `Context::none()` is an all-zero context, so
`current()` inside the body still answers `None`, every span is an unparented
root, the two spans above land in different traces, and — because
[`Log`](/docs/cookbook/logging/#correlating-with-a-trace) reads `current()` to
decide whether to write `trace_id` and `span_id` — no log line carries any ids
either. `Context::none()` is for a *context*, such as an absent or malformed
incoming header; it is not a starting point for a span.

The parent shows the same thing. Zero is how `Span::parent` says "root", so a
nested `around` that writes `parent: 0` starts a second trace rather than a
child span.

## Report `Result` failures on the span

When an operation reports its domain failure as `Result<A, E>` and `E: Show`, use `around_result` instead of manually inspecting the result only for tracing:

```khora
let result = around_result(
  tracer,
  "load user",
  fn () => repository.load(user_id),
);
```

`around_result` finishes successful results with `Status::Ok` and renders an `Err` into `Status::Failed`. Cancellation and raised failures still use the structured cleanup path.

## Trace context at an HTTP boundary

`Context` understands the W3C `traceparent` representation. An HTTP boundary can parse an incoming header without accepting malformed partial context:

```khora
let incoming = match request.header("traceparent") {
  Option::None => Context::none(),
  Option::Some(header) => match Context::of_traceparent(header) {
    Option::None => Context::none(),
    Option::Some(context) => context,
  },
};
```

A valid context can be rendered for an outgoing request with:

```khora
let header = incoming.to_traceparent();
```

The tracing model is designed so context associated with structured fiber work survives suspension and scheduler movement rather than depending on an OS-thread-local variable.

## Disabled tracing

Use the shipped no-op tracer when tracing is intentionally disabled:

```khora
let tracer = Tracer::none();
```

For exact `Tracer`, `Span`, `Context`, `Attribute`, `Status`, `around`, and `around_result` declarations, see the [tracing API reference](/docs/stdlib/api/trace/). For cancellation-safe cleanup generally, see [Cancellation-safe resources](/docs/cookbook/cancellation-safe-resources/).
