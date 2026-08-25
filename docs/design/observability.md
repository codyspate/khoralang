# Observability

A service that cannot be traced is not a candidate, and `docs/positioning.md`
claims Khora should be one wherever a team compares Go, backend TypeScript or
application Rust. Nothing in `std` emits a log line today, let alone a span.

This decides the shape. It does not decide a vendor, and most of what people
mean by "observability" is deliberately not here.

## The rule this follows

`docs/design/ecosystem.md` §"What `std` reserves" already settles the general
question, and it is worth restating because it does all the work:

> It can be the framework, and every alternative starts from a socket; or it can
> be the layer underneath one... **The middle layer is the one that matters.** A
> router is a matter of taste and a weekend; framing a request correctly is
> neither. It is also the part that fails in production rather than in testing.

So the question is never "is tracing important". It is **what would every
package otherwise re-derive, subtly wrong, in a way that only fails in
production?** For observability there are exactly two such things, and neither
of them is an exporter.

## What Khora has that makes this different

Three mechanisms, and none of them is a library.

**A capability is an interception point.** Every effect is a record of closures,
so instrumenting one is another handler:

```khora
fn traced(inner: Fs, tracer: Tracer) -> Fs {
  handler for Fs {
    read: fn path => tracer.around("fs.read", fn () => inner.read(path)),
    write: fn (path, bytes) => tracer.around("fs.write", fn () => inner.write(path, bytes)),
  }
}
```

Every existing caller of `Fs` is now instrumented and not one of them changed.
This is what OpenTelemetry achieves with bytecode agents and monkey-patched
prototypes, and here it falls out of capability-passing. All external authority
is reachable this way, because that is what a capability *is*.

**A fiber's lifetime is a span's lifetime.** Phase 11 gives every fiber an
identity that survives migration between workers, and a nursery gives it a
parent. A span opened at spawn and closed at completion produces a correct trace
tree with no context threading anywhere — which is the part Go gets wrong, where
`ctx` is passed by hand and forgetting is silent.

**The compiler can insert spans.** 11B already emits `khora_safepoint` at every
loop back-edge and measured it under `bench/service`'s noise floor, so the
machinery for "code generation inserts a runtime call" exists and is paid for. A
build flag that wraps every function with a non-empty `with` row is the same
mechanism. It should stay opt-in: it is the difference between tracing the
program's boundaries and tracing its every step, and only the first is free
enough to leave on.

## What `std` owns

The two things that fail in production and would be re-derived wrong:

**Propagation.** A span's parent must survive a fiber spawn, a steal onto
another worker, a wake from the reactor, a hand-off to the blocking pool, and a
cancellation. That is not a library concern — it is a property of the scheduler,
and a package cannot implement it. It is also exactly where every other
ecosystem's tracing leaks: a task spawned without its context, a callback that
loses its parent, a thread-local read after a thread hop. Khora's answer is
`docs/design/scheduler.md`'s: the fiber carries it, and the runtime moves it
when it moves the fiber.

**Making an unsampled span free.** If tracing costs when it is off, it gets
turned off, and then it does not exist. With Perceus an unsampled span must
allocate nothing at all — which is a constraint on the record type and the
sampling decision's position, not an optimisation to add later. The decision has
to be taken at span *start*, in the handler, so that nothing downstream is
built.

Plus the vocabulary, because two packages that disagree about what a span is
cannot compose into one trace:

- `Span`, `SpanContext`, `Attributes`, `Event`, `Link`, `Status`;
- `Severity` and a log record;
- counter, gauge and histogram instruments;
- W3C `traceparent` and `tracestate`, parse and render;
- the `Tracer`, `Meter` and `Logger` effects;
- a no-op handler, and a recording one for tests;
- the **sink**: what a finished record looks like on its way out.

## What `std` does not own

OTLP framing. Datadog's agent protocol. Prometheus scrape endpoints. Batching,
retry, compression, back-pressure to the collector, and every sampling policy
beyond a head sampler.

All of it is a vendor's release cadence attached to a network protocol, which is
the same argument that keeps Postgres out — see `ecosystem.md`. A package can be
wrong about OTLP and be fixed in an afternoon; `std` cannot.

## The data model, and why it is OpenTelemetry's

Datadog spans are a service, a resource, an operation and flat string tags.
OpenTelemetry spans carry typed attributes, events, links and a status.
**Rendering OTel into Datadog loses information; the reverse cannot be done at
all** — which is why Datadog ships an OTel exporter and not the other way round.

So the model is the superset, and the narrower vendor drops what it cannot hold.
That is a one-way decision and it is the right way round.

An attribute value is the smallest thing that matches OTel's `AnyValue` without
inheriting its variance:

```khora
export data Value {
  Text(String),
  Int(Int),
  Float(Float),
  Bool(Bool),
  List(List<Value>),
}
```

`derive(Attributes)` on a record is the obvious ergonomic follow-on and should
wait until the hand-written form has been used enough to know what it should
produce.

**The sink takes finished, immutable records.** Not live handles. An exporter
then cannot hold a span open, cannot mutate one after it is reported, and cannot
be the reason a trace is wrong. Transforming to OTLP or to Datadog becomes a
pure function over data, which is a shape this language should be good at.

## Cost

A span per function call at service rates is real money, and the honest position
is that the default must be boundaries rather than calls:

- spans at effect boundaries and fiber lifetimes by default;
- head sampling in the handler, so an unsampled span allocates nothing;
- the compiler flag for finer granularity, off unless asked for.

`bench/service` is the instrument. The safepoint's number — 800,730 req/s
without against 796,116 / 781,456 / 784,215 with, a spread wider than the gap —
is the standard the tracing default should be held to, and the reason to measure
before believing anything in this section.

## Open questions

**Scoped spans want a polymorphic effect field.** `around: (String, () -> A) -> A`
is generic in `A`, and an effect field that is itself polymorphic is a different
feature from ordinary generics — a rank-2 type. Without it the surface is a
`start`/`finish` pair, which leaks whenever somebody forgets. Two mitigations
exist and neither is free: most spans come from the runtime and from capability
wrappers rather than by hand, so the exposed surface is small; and the linter
(roadmap 10.3) can carry a "span started and not finished" rule. **This wants
deciding before the API is written, not after.**

**How much of this the runtime must know.** If `khora-rt` emits fiber-lifetime
spans, it needs the span type, and that is a coupling between the runtime and a
`std` data type that nothing else in the tree has. The alternative is a thinner
runtime event — ids, a timestamp, a name — with the rich model assembled above
it. The thinner one is probably right and it is not obvious.

**Metrics may not belong in the first cut.** Traces and logs share a propagation
problem; metrics do not, and their hard part is aggregation and temporality,
which is much closer to being a package's business. Shipping `Tracer` and
`Logger` first and letting `Meter` wait for a real user is the smaller mistake.

## What this document does not decide

Whether logging is a separate effect or a span event with a severity. Whether
`std::net::http` grows automatic server spans or a package wraps it. What the
default sampler's rate is. Whether a Khora program can be a Prometheus target
without a package. All of these are worth an argument and none of them changes
the boundary above.
