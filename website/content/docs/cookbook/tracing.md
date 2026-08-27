---
title: Tracing
sidebar:
  order: 4
---

Khora's tracing vocabulary lives in `std::trace`; exporters and vendor protocols can live in packages. Application code can program against `Tracer` and keep span lifetime structured regardless of where completed spans are eventually sent.

## Complete example

This module implements a small console tracer and uses `around` to guarantee the span is finished with the lifetime of the operation:

```khora
module main;

import std::core::{print};
import std::trace::{Context, Status, Tracer, around};

fn console_tracer() -> Tracer {
  handler for Tracer {
    start: fn (name, _attributes) => {
      print("start span: ${name}");

      {
        context: Context::none(),
        parent: 0,
        name: name,
      }
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

fn calculate() -> Int {
  print("doing work");
  42
}

pub fn main() {
  let tracer = console_tracer();
  let result = around(tracer, "calculate", calculate);

  print("result = ${Int::to_string(result)}");
}
```

The application decides which tracer implementation to construct. `around` owns the span lifetime:

```khora
let result = around(tracer, "calculate", calculate);
```

It starts the span before running `calculate` and registers cleanup so the span is finished when the operation returns, raises, or is cancelled. A caller should not rely on a later `tracer.finish(...)` line running after arbitrary fallible work.

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

Application code can keep the same tracing structure without conditional instrumentation branches throughout the program.

For exact `Tracer`, `Span`, `Context`, `Attribute`, `Status`, `around`, and `around_result` declarations, see the [tracing API reference](/docs/stdlib/api/trace/). For cancellation-safe cleanup generally, see [Cancellation-safe resources](/docs/cookbook/cancellation-safe-resources/).
