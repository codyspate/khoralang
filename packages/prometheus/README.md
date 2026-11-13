# prometheus

Counters, gauges and histograms for a Khora service, served in Prometheus'
text exposition format (version 0.0.4) on a route of the service's own router.

`std` carries traces and logs and leaves metrics to a package: their hard part
is aggregation and how a value is read over time, which is the monitoring
system's business, and this is the one most services use.

## Using it

```khora
import std::core::{ChildFailed, List, SharedFn};
import std::clock::{Clock};
import std::net::http::{HttpError, Request, Response, Router};
import prometheus::metrics::{MetricError, Registry, default_buckets};
import prometheus::serve::{MountError, mount};

pub fn main() -> () raises HttpError + ChildFailed + MetricError + MountError {
  let registry = Registry::new();
  let requests = registry.counter("http_requests_total", "Requests answered.", ["route", "status"])!;
  let seconds = registry.histogram("http_request_seconds", "Time to answer.", ["route"], default_buckets())!;
  let ok = requests.labels(["/orders", "200"])!;
  let timing = seconds.labels(["/orders"])!;
  with { clock: Clock::real() } {
    Router::new()
      |> Router::get("/orders", SharedFn::of(fn (_request: Request) => {
           let began = clock.monotonic_millis();
           let answer = Response::text(200, "[]");
           ok.inc();
           timing.observe(Int::to_float(clock.monotonic_millis() - began) / 1000.0);
           answer
         }))
      |> mount(registry)!
      |> Router::listen(8080)!
  }
}
```

`mount` adds `GET /metrics`, answered with the content type
`text/plain; version=0.0.4; charset=utf-8`. `handler(registry)` is the same
handler on its own, for a router that wants it at another path or behind a
check. There is no server in this package: the service's own `listen` serves
the route, under the service's own `[permissions] listen` grant, and this
package asks for no permission and builds no capability.

**Mount it after every other route.** A router tries the route mounted last
first, so a catch-all such as `GET /:page` mounted after `mount` answers
`/metrics` itself, with its own body and content type, and the scrape
endpoint is lost with nothing said at startup.

**`mount` refuses a router that already answers `GET /metrics`**, with
`MountError::AlreadyMounted`. Mounted after one, it would replace it: a
service's own `/metrics` would vanish, and a second `mount` would serve only
the second registry. One registry, mounted once, holds every metric. Only the
exact pattern `/metrics` under `GET` is checked; a catch-all mounted later
is not, which is why the order above matters. `handler(registry)` is not
checked at all: where it is mounted is the caller's to arrange.

## The three kinds

- **`Counter`** only goes up: `inc()`, `add(by)`. An `add` below zero or of
  `NaN` raises `MetricError::Negative` and leaves the counter as it was:
  Prometheus reads a counter that went down as a restart and counts the whole
  value again, and a counter that has added `NaN` is `NaN` for good.
- **`Gauge`** goes both ways: `set`, `inc`, `dec`, `add`.
- **`Histogram`** counts observations into buckets fixed when it is declared.
  `default_buckets()` is Prometheus' own set, 5 ms to 10 s. Bounds must
  strictly increase; `+Inf` is always added. It renders `_bucket{le=..}`
  lines, cumulative, ending in `+Inf`, then `_sum` and `_count`.

No summaries: quantiles need a sketch, and Prometheus recommends histograms.

## Labels

A metric is declared with label names, and each combination of values is a
series of its own:

```khora
let requests = registry.counter("http_requests_total", "", ["method", "status"])!;
requests.labels(["GET", "200"])!.inc();
```

`labels` raises `MetricError::LabelCount` when the number of values is not the
number of names. Called on the metric itself, `inc`, `set` and `observe` are
for a metric declared with no labels, and raise `LabelCount` otherwise.

Label values may hold anything: a backslash, a double quote and a line feed
are escaped on the way out. Help text has its backslashes and line feeds
escaped.

The method is `labels`, not `with` as in some other clients, because `with` is
a Khora keyword.

**Label values must come from a bounded set.** Every distinct set of label
values is a series, and a series is kept for the life of the process: there
is no limit on how many a metric holds and no way to remove one. A label
value taken from a request, such as a raw path, a user id or a query string,
makes a series per distinct value. Measured with 100,000 distinct values on
one counter: about 400,000 more live objects, resident memory up by about
27 MB (7 MB to 34 MB), and one scrape of 3 MB. A histogram series holds more
than a counter's. Label a request by its route pattern (`/orders/:id`), not
by the path a client asked for.

## Names

Checked when the metric is declared, so a mistake is an error in front of the
person who made it rather than a target Prometheus drops at the next scrape:

- a metric name matches `[a-zA-Z_:][a-zA-Z0-9_:]*`;
- a label name matches `[a-zA-Z_][a-zA-Z0-9_]*` and does not begin with `__`,
  which Prometheus keeps for its own labels;
- a histogram may not have a label named `le`, which it writes itself;
- a label name may not appear twice, and a registry holds one metric per name;
- no two metrics may write a series with the same name. A histogram `lat`
  writes `lat_bucket`, `lat_sum` and `lat_count`, so a gauge or counter by
  any of those names is refused, whichever is declared first. A counter is
  read as one family with and without `_total`, so `jobs` and `jobs_total`
  are refused together too, as are `lat_count_total` and the histogram
  `lat`. The error, `MetricError::Collides`, names both metrics and the
  series they share. Two such series in one scrape make Prometheus refuse
  the scrape or keep one of the two at random.

Name a counter with `_total` on the end, as Prometheus' conventions ask.

## Many fibers

Every series is a `Shared` cell, and every change is one `Shared::update`, so
changes from many fibers at once are never lost: 100 fibers making 10,000
increments each count exactly 1,000,000, on both fiber backends. A histogram
series keeps its buckets, sum and count in one cell, so a scrape never sees a
`_count` that disagrees with its `+Inf` bucket.

**What it costs.** One `inc` on a series held in a variable is one
`Shared::update`: a lock taken and released, a call through a closure, and
one allocation for the new value. Measured with `perf stat` on one fiber with
nothing else running, release build, as the difference between twenty
million increments and ten million, median of five:

| backend   | user instructions per `inc` | time per `inc` |
| --------- | --------------------------- | -------------- |
| threads   | 465                         | 53 ns          |
| scheduler | 510                         | 67 ns          |

That is uncontended. Under contention a fiber waits its turn for the lock, so
a hundred fibers incrementing one series take turns rather than running side
by side.

`labels` costs more: it reads the table of series, compares the values
against it, and allocates the list it was given; with one label, `labels`
then `inc` is three allocations to `inc`'s one. On a hot path, call `labels`
once and keep what it answers, as the example above does.

## Rendering

`render()` writes metrics in name order and series in label-value order, so an
idle service gives the same bytes twice. Each series is read once, as its line
is written; a scrape is not an instant across every metric, and no client
library's is.

Numbers are written as the shortest decimal that reads back as the same
double, and as `+Inf`, `-Inf` and `NaN`. One whose plain decimal would run
past 32 characters, such as `1e300`, `5e-324` or `1.2345678901234567e-20`,
is written with an exponent and the same shortest digits; every double reads
back as itself. Histogram bounds that would be written alike are refused, so
no two buckets of a series share an `le`.

A counter's `add` takes `+Inf`, as the other clients' do, and the counter
stays at `+Inf` from then on.

## Not here

OpenMetrics, exemplars, a push gateway, and process metrics (CPU and memory
need operating-system calls this package does not make).

## Tests

`khora test packages/prometheus` runs the package's own tests: each kind,
labels and their escaping, name checks, histogram buckets and `+Inf`, and the
whole of `render()` compared byte for byte against text written by hand.

The `prometheus::` module of the LLVM test suite covers what those cannot: 100
fibers × 10,000 changes on one counter and one histogram on both fiber
backends, scrapes taken while eight fibers observe (none torn), fibers racing
to create one series, a `GET /metrics` through a real router, and every
shape of the output read back by two real parsers:

- `KHORA_PROMETHEUS_PYTHON`, set to a Python that can import
  `prometheus_client` 0.26.0, reads it with that library's parser;
- `KHORA_PROMETHEUS_GO`, set to a `go` binary, builds the checker in
  `crates/khora-codegen-llvm/tests/fixtures/prometheus_go` (pinned
  `go.mod`/`go.sum`) and reads it with Prometheus' own `expfmt` parser and
  the server's scrape parser, `textparse`, which also reports duplicate
  series. The Python parser accepts scrapes these two refuse.

CI sets both in the `backend (ubuntu-latest)` job.
