# otlp

An OpenTelemetry exporter for `std::trace`, over OTLP/HTTP JSON.

`std::trace` says explicitly that OTLP is not `std`'s job: a wire protocol with
a release cadence of its own does not belong in a standard library that promises
not to break. This is the other half of that sentence — the exporter every
service needs, in a package it can pin.

## Using it

```khora
import std::clock::{Clock};
import std::net::http::{HttpClient};
import std::random::{Random};
import otlp::exporter::{Exporter};

pub fn main() -> Int raises HttpError + ChildFailed {
  with { client: HttpClient::real() } {
    nursery(fn () =>
      Exporter::running("checkout", "http://localhost:4318",
        Clock::real(), Random::real(), fn tracer =>
          Router::new()
            |> Router::get("/health", SharedFn::of(health))
            |> Router::listen(8080)!))!
  };
  0
}
```

`endpoint` is the collector's base URL with no path; `/v1/traces` is appended,
which is where the specification puts it. The default OTLP/HTTP port is 4318.

## What it does when the collector is down

**Drops spans, and keeps serving.** The queue is `dropping`, so a service whose
collector has gone away runs at full speed and loses spans rather than blocking
a request on an observability backend. A tracer that can stall the thing it is
measuring is a tracer that takes production down.

A failed POST is not raised either: the exporting fiber has nobody to tell, and
raising would either kill it or need a handler the service never asked for.

## Two things it does not do yet

**Spans have no parents.** `Tracer` has no operation that says "inside this
one", so a nested `around` starts a second trace rather than a child span. The
wire format handles parents — `wire.kh` renders `parentSpanId` when a span has
one, and a test covers it — so what is missing is a way for the effect to say
so, which is `std::trace`'s to add.

**Attributes given to `start` are not exported.** They are handed to the
operation and this handler drops them, because `finish` is where a span becomes
a report and the two are separate calls. Pairing them the way the start *time*
is paired would carry them; it has not been needed yet.

Both are limits of what `std::trace` can express rather than of the protocol,
and both are visible in `exporter.kh` where they happen.

## JSON, not protobuf

The specification defines both over HTTP and every collector that speaks
protobuf also speaks this. Protobuf buys bytes on the wire and costs a schema
compiler, a code generator and a dependency this repository does not have. A
span is a few hundred bytes either way and they are sent in batches of 64.
`wire.kh` is the one file that would change.

## Tests

`khora test packages/otlp` runs them. They assert against the rendered bytes
rather than against the values going in, because every mistake this format
invites is a rendering mistake: a trimmed id, a timestamp sent as a number, a
`parentSpanId` of sixteen zeros on a root span. One of them found a real bug on
its first run — the resource's `service.name` was sent as a bare string where
OTLP wants an `AnyValue` object, which a collector drops silently.
