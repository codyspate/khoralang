---
title: prometheus
description: Counters, gauges and histograms, served for Prometheus to scrape.
---

Metrics for a Khora service: counters, gauges and histograms, rendered in
Prometheus' text exposition format (version 0.0.4) and served on a route of
the service's own router.

```toml
[dependencies]
prometheus = { git = "https://github.com/codyspate/khoralang", rev = "main", subdir = "packages/prometheus" }
```

The package is not yet in a tagged release: pin a commit of `main` until it
is, and build it with a toolchain from the same commit.

**[`packages/prometheus/README.md`](https://github.com/codyspate/khoralang/blob/main/packages/prometheus/README.md)
is the reference.** This page is the overview and a worked example.

## A service that counts its requests

An HTTP service that counts requests by route and status, times them in a
histogram, and answers `GET /metrics`:

<!-- unchecked: imports the package, which the page checker does not load; prometheus::the_documented_service_answers_as_documented builds and runs it -->
```khora
module shop::main;

import std::core::{ChildFailed, List, SharedFn};
import std::clock::{Clock};
import std::net::http::{HttpError, Request, Response, Router};
import prometheus::metrics::{Counter, Histogram, MetricError, Registry, default_buckets};
import prometheus::serve::{MountError, mount};

/// The two metrics every route reports into.
type Meters = { requests: Counter, seconds: Histogram };

/// Counts `answer` under `route` and its status, and records how long it
/// took since `began`.
fn measured(meters: Meters, route: String, began: Int, answer: Response) -> Response
  with { clock: Clock }
  raises MetricError
{
  let took = Int::to_float(clock.monotonic_millis() - began) / 1000.0;
  meters.requests.labels([route, Int::to_string(answer.status)])!.inc();
  meters.seconds.labels([route])!.observe(took);
  answer
}

fn orders(meters: Meters, _request: Request) -> Response
  with { clock: Clock }
  raises MetricError
{
  let began = clock.monotonic_millis();
  measured(meters, "/orders", began, Response::text(200, "[]"))!
}

fn refunds(meters: Meters, _request: Request) -> Response
  with { clock: Clock }
  raises MetricError
{
  let began = clock.monotonic_millis();
  measured(meters, "/refunds", began, Response::text(403, "refunds are closed"))!
}

pub fn main() -> () raises HttpError + ChildFailed + MetricError + MountError {
  let registry = Registry::new();
  let meters: Meters = {
    requests: registry.counter("http_requests_total", "Requests answered, by route and status.", ["route", "status"])!,
    seconds: registry.histogram("http_request_duration_seconds", "Time to answer, by route.", ["route"], default_buckets())!,
  };
  with { clock: Clock::real() } {
    Router::new()
      |> Router::get("/orders", SharedFn::of(fn request => orders(meters, request)!))
      |> Router::post("/refunds", SharedFn::of(fn request => refunds(meters, request)!))
      |> mount(registry)!
      |> Router::listen(8080)!
  }
}
```

After two `GET /orders` and one `POST /refunds`, a scrape of `/metrics`
answers, with the content type `text/plain; version=0.0.4; charset=utf-8`
(the bucket lines between `0.005` and `+Inf` are left out here):

```text
# HELP http_request_duration_seconds Time to answer, by route.
# TYPE http_request_duration_seconds histogram
http_request_duration_seconds_bucket{route="/orders",le="0.005"} 2
http_request_duration_seconds_bucket{route="/orders",le="+Inf"} 2
http_request_duration_seconds_sum{route="/orders"} 0
http_request_duration_seconds_count{route="/orders"} 2
http_request_duration_seconds_bucket{route="/refunds",le="0.005"} 1
http_request_duration_seconds_bucket{route="/refunds",le="+Inf"} 1
http_request_duration_seconds_sum{route="/refunds"} 0
http_request_duration_seconds_count{route="/refunds"} 1
# HELP http_requests_total Requests answered, by route and status.
# TYPE http_requests_total counter
http_requests_total{route="/orders",status="200"} 2
http_requests_total{route="/refunds",status="403"} 1
```

The sums are 0 because `monotonic_millis` counts whole milliseconds and
these handlers answer in less than one.

The route is the pattern, not the path a client asked for. Every distinct
set of label values is a series kept for the life of the process, with no
limit and no way to remove one, so label values must come from a bounded
set: a scanner walking random paths would otherwise make one series per
path. 100,000 distinct values on one counter measured about 27 MB of
resident memory and a 3 MB scrape.

`mount` adds `GET /metrics` to the router; `handler(registry)` is the same
handler for a router that wants it at another path. Mount it after every
other route: the router tries the last route mounted first, so a catch-all
such as `GET /:page` mounted after it would answer `/metrics` itself. A
router that already answers `GET /metrics`, from an earlier `mount` or a
route of its own, is refused with `MountError::AlreadyMounted` rather than
having that route replaced. There is no server in the package. The
service's own `listen` serves the route, under its own
`[permissions] listen` grant, and the package itself needs no permission.

## The three kinds

- **`Counter`** only goes up, with `inc` and `add`. An `add` below zero or of
  `NaN` raises `MetricError::Negative` and leaves the counter alone:
  Prometheus reads a counter that went down as a restart, and counts the
  whole value again.
- **`Gauge`** goes both ways, with `set`, `inc`, `dec` and `add`.
- **`Histogram`** counts observations into buckets fixed when it is
  declared. `default_buckets()` is Prometheus' own set, 5 ms to 10 s; bounds
  must strictly increase, and `+Inf` is always added.

There are no summaries: quantiles need a sketch, and Prometheus recommends
histograms in their place.

## Labels and names

`labels` takes one value per declared label name, in order, and answers that
series, creating it on first use. A wrong number of values raises
`MetricError::LabelCount`. Label values may hold anything; a backslash, a
double quote and a line feed are escaped when the text is written.

The method is `labels` rather than `with`, which some other clients use,
because `with` is a Khora keyword.

Names are checked when a metric is declared, so a mistake raises a
`MetricError` where it is made instead of costing a scrape: a metric name
matches `[a-zA-Z_:][a-zA-Z0-9_:]*`, a label name matches `[a-zA-Z_][a-zA-Z0-9_]*`
and does not begin with `__`, a histogram has no label named `le`, no label
name is declared twice, and a registry holds one metric per name. No two
metrics may write a series of the same name: a histogram `lat` writes
`lat_bucket`, `lat_sum` and `lat_count`, so a metric by any of those names is
refused, and a counter `jobs` and a counter `jobs_total` are one family to a
scraper and are refused together. Name counters with `_total` on the end.

## Many fibers, and what it costs

Every series is a `Shared` cell and every change is one `Shared::update`, so
changes from many fibers at once are never lost: 100 fibers making 10,000
increments each count exactly 1,000,000, on both fiber backends. A histogram
series keeps its buckets, sum and count in one cell, so a scrape never sees a
`_count` that disagrees with its `+Inf` bucket.

One `inc` on a series held in a variable is a lock taken and released and a
call through a closure, and one allocation. Uncontended, on one fiber in a
release build, that measured 465 user instructions and 53 ns on the thread
backend, and 510 instructions and 67 ns on the scheduler. Under contention a
fiber waits its turn for the lock.

`labels` costs more than that, because it looks the values up among the
metric's series. On a path that runs per request with a fixed set of label
values, call `labels` once and keep what it answers.

## What it does not do

No OpenMetrics, no exemplars, no push gateway, and no process metrics: CPU
time and resident memory need operating-system calls this package does not
make.
