# Benchmarks

Four servers answering the same request, so that the difference between them is
one thing at a time.

| | what it does |
| --- | --- |
| `control.rs` | Rust, thread per connection, closes after each request. |
| `control_keepalive.rs` | The same, holding the connection open. |
| `floor/` | Khora: accept, read, write a fixed string, repeat. No parsing. |
| `render/` | The floor plus `Response::rendered_keeping`. No parsing. |
| `service/` | A `Router` with one route — the whole of `std::net::http`. |

`floor` against `control_keepalive` is what the **runtime** costs. `service`
against `floor` is what the **library** costs. `render` sits between them and
says how much of the library is the response rather than the request.

## Running them

```bash
cargo build -p khora-rt
cargo run -p khora-cli --features llvm -- build bench/service
./bench/service/src/main.exe &
python bench/load.py 18952 "service"
```

Ports are fixed so two of these cannot be measured at once by accident:
`floor` 18950, `render` 18951, `service` 18952. The Rust controls take a port
on the command line.

```bash
rustc -O -o bench/control_keepalive.exe bench/control_keepalive.rs
./bench/control_keepalive.exe 18953 &
python bench/load.py 18953 "rust, keep-alive"
```

## What was measured

16-core Windows desktop, load generator on the same machine, 48 reused
connections, five second runs, median of three. **These numbers travel with
that sentence or they do not travel.**

All four back to back in one sitting, after phase 9:

| | req/s |
| --- | --- |
| Rust control, keep-alive | 560,000 |
| Khora `floor` | 781,000 |
| Khora `render` | 721,000 |
| Khora `service` | 538,000 |

Runs vary by up to eight per cent on an otherwise idle machine, so a difference
smaller than that is noise. Every figure is the median of three; the `service`
runs spanned 535k to 566k, and the Rust control's spanned 551k to 635k.

**Read the four together or not at all.** The Rust control measured 653,000 in
an earlier sitting on the same machine and 560,000 in this one, which is well
outside the eight per cent and is the machine rather than the program. That is
the whole reason for running all four at once: the ratios within a sitting mean
something and the absolute figures across sittings do not.

The set before phase 9 was 653k / 758k / 734k / 507k. `service` is the only one
the phase could move — `floor` and `render` barely count references — and it
moved from 507k to 538k against a control that was slower, so the honest claim
is "the request parser got cheaper" and the size of it comes from
`docs/design/reuse.md`, not from here.

Two readings, and the second is the useful one.

**The runtime matches Rust.** `floor` measures above `control_keepalive`, and
the honest reading of that is not that Khora is faster — both are close to what
the load generator can drive, and a client and a server on sixteen shared cores
is not a clean measurement of either. What it rules out is the runtime being
the reason for anything below.

**`std::net::http` costs the gap between 758k and 507k**, and rendering the
response is about 24k of it. The rest is parsing the request.

## Where the number came from

Worth keeping, because the first three attempts to explain the gap were wrong.

Connection reuse is worth **two orders of magnitude** and was measured first for
that reason: 6,116 req/s on a fresh connection each time against 1.1M on one
held open. Anything compared against a keep-alive benchmark without keep-alive
is answering a different question.

After that, `service` sat at 152,912 and three plausible culprits — fiber
scheduling, nursery bookkeeping, map allocation — were each measured and each
worth under one per cent. What it actually was:

- **The runtime was compiled without optimisation** in every executable
  `khora build` produced, because it was found beside a `target/debug`
  compiler. Two and a half times, from three lines of `Cargo.toml`. Errata 45.
- **`String::slice` was two allocations, two copies and a UTF-8 revalidation.**
  As an intrinsic it is one allocation and one memcpy: 2,915ns to 80ns.
- **`String::index_of` reached `memmem` through two heap-allocated closures.**
  Going straight to the call: 500ns to 40ns.
- **Splitting the header block was quadratic** in the number of headers,
  because each line copied everything after it.

Parsing an eighty-byte request went from 28,310ns to 3,600ns across those.

The lesson worth carrying into the next round is in errata 45: a benchmark that
is off by a constant factor *everywhere* is a configuration bug, not a code bug,
and the way to see it is to measure one primitive against something whose cost
is already known — one call to `memmem`.

## What these do not measure

Latency distribution, behaviour under more connections than cores, cold start,
memory, or anything with a body. A `/health` route returning a fixed JSON
object is the thinnest possible request; it is chosen to isolate the library
from the handler, not because it resembles a real workload.
