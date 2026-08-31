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
| `allocator/` | Not a server: what allocation costs in the states a heap gets into. |
| `iteration/` | Not a server: what `for` costs against the same loop written out. |

`floor` against `control_keepalive` is what the **runtime** costs. `service`
against `floor` is what the **library** costs. `render` sits between them and
says how much of the library is the response rather than the request.

## Running them

```bash
cargo build -p khora-rt
cargo run -p khora-cli --features llvm -- build bench/service
./bench/service/build/service.exe &
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

> **Every figure below is a measurement of this harness, not of the servers.**
> They were taken at 48 connections, and `bench/compare.py` later established
> that 48 Python processes cannot drive any of these servers to its limit:
> pointed at `floor`, the same rig reports 747k at 48 connections, 1.50M at 96
> and 2.43M at 160. A rate that climbs with client concurrency is the client's
> rate. The *ratios between the Khora tiers* still mean something, because all
> three were throttled by the same client; the absolute numbers do not, and the
> comparison against the Rust control means less than it looks.
>
> `bench/compare.py` is the version that walks a ladder and refuses to report a
> rate that is still climbing. See "Against other languages" below.

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

## Phase 11's scheduler, measured the same way

One sitting, one machine, `bench/service` only, 48 reused connections:

| | req/s | of threads |
| --- | --- | --- |
| fibers as threads (the default) | 816,963 | — |
| fibers on the scheduler, idle workers polling (11I) | 513,500 | 63% |
| — with a reactor thread instead (11H) | 429,000 | 55% |
| — before the reactor could be woken at all (11G) | 59,965 | 7% |

The top two rows were taken minutes apart with nothing else changed, which is
the only way this file allows a comparison to be made; the scheduler figure is
the median of 512,770 and 514,221. The lower rows are earlier sittings and are
kept for the shape of the progression rather than for their absolute values —
the thread control itself read 782,149 in the 11H sitting and 816,963 here,
which is the machine and not the program, and is why the third column matters
more than the second.

Twelve times slower became 1.6 times slower in two steps: making one `poll`
interruptible, then letting the worker that will run the fiber be the one that
notices its socket. Threads remain the default; 63% is at the lower edge of the
70–85% band `docs/design/scheduler.md` §10a set, not inside it.

**What it is not.** Not correctness — `scripts/http_conformance.sh` passes on
both, pipelining and header limits included — and not the cost of a fiber,
since the sixteen compiled fiber tests run in the same time on both to within
four per cent. It is the path from a socket becoming readable to the fiber that
wanted it running again.

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

## Against other languages

`bench/peers/` holds the same `/health` route in Go, Node, C# and Java, each
using that language's ordinary server rather than a hand-rolled socket loop,
because the comparison worth making is against what a team would actually
write. `python bench/compare.py` builds nothing and measures everything,
walking 48, 96 and 160 connections per server.

One sitting, 16-core Windows desktop, load generator on the same machine,
five-second runs, a discarded warm-up first. Khora built with `--release`.

| | req/s | what it is |
| --- | --- | --- |
| Khora, `floor` | **> 2,433,000** | accept, read, write a fixed string. No parsing |
| Rust, thread per connection | **> 2,129,000** | hand-rolled, no framework |
| Khora, `render` | **> 2,354,000** | the floor plus response rendering |
| Khora, `std::net::http` | **> 1,729,000** | a `Router`: accept, read, parse, route, render |
| C#, ASP.NET Core minimal API | 264,000 | Kestrel |
| Go, `net/http` | 159,000 | the standard library |
| Java, JDK `HttpServer` | 133,000 | `com.sun.net.httpserver` |
| Node, `node:http` | 27,000 | the standard library |

**The four figures with a `>` are not measurements.** Each of those servers
answered more the more connections it was offered — `std::net::http` reported
519k, 1.06M and 1.73M across the ladder — which is the client running out of
capacity, not the server. What they establish is a floor: Khora's HTTP server
is somewhere above 1.7 million requests a second on this machine, and this rig
cannot say where. The four without a `>` stopped moving, so those are the
servers' own numbers.

So the honest reading is **an order of magnitude, not a ratio**. Khora's
`Router` — a full parse, route match and render, written in Khora — is at
least 6× Kestrel and at least 10× Go's `net/http` here, and it is in the same
class as a hand-rolled Rust server. How much more than 6× and 10× is a
question this harness cannot answer.

**What would answer it** is a second machine, or a load generator that is not
the thing being measured. A first attempt at one in Go plateaued at 250k,
which is *below* the Python rig it was meant to replace, so it measured itself
instead; it is kept in `bench/peers/loadgen.go` with that written on it. Two
machines is the real fix and is roadmap 13.23.

### One thing the comparison did settle

`--release` made no difference to `std::net::http`: 1.73M optimised against
1.76M unoptimised, which is the same number twice. For this workload the time
is in the kernel rather than in the generated code, so the profile has nothing
to work on. That is worth knowing before anybody attributes a benchmark result
to an optimiser.

### What these peers are not

Each is the language's *ordinary* server. Node's is single-threaded by design
and would be several times faster behind `cluster`; Java's JDK server is not
what a Java service ships on, and Netty or Undertow would be far above it;
`net/http` is Go's real answer and a specialised library like `fasthttp` is
faster. Read the table as "what you get when you write the obvious thing",
which is the comparison a team actually faces, not as a ranking of runtimes.

## What these do not measure

Latency distribution, behaviour under more connections than cores, cold start,
memory, or anything with a body. A `/health` route returning a fixed JSON
object is the thinnest possible request; it is chosen to isolate the library
from the handler, not because it resembles a real workload.

## The allocator

`bench/allocator` is the odd one out — no sockets, no load generator, one
process that prints nanoseconds per object.

```bash
cargo run -p khora-cli --features llvm -- run --release bench/allocator
```

**It exists because of a report that did not reproduce.** A round of people
writing Khora programs reported that allocation ran about twenty-seven times
slower after a large number of cons cells had been freed. Ten phases here put
the heap into the states that could plausibly cause that — two million cells
live, the same two million freed, a different size allocated while they are
live and again after they are gone, three sizes interleaved, and half of a
large batch released where it was made so the free space is in holes — and
then time cons cells again against a fresh heap.

Every phase lands within a few percent of the fresh-heap number, in a debug
build and a release one:

```
cells, fresh heap        77 ms, 77 ns each
cells, two million live  66 ms, 66 ns each
cells, after 2000000 freed  64 ms, 64 ns each
cells, after those       66 ms, 66 ns each
cells, after the holes   72 ms, 72 ns each
cells, once more         65 ms, 65 ns each
```

16-core Windows desktop; `khora_alloc` goes to Rust's global allocator, which
is the system heap here, so the numbers are that allocator's and not one this
repository wrote.

This is kept rather than deleted for two reasons. Somebody who suspects the
allocator next should be able to re-run the shapes already looked at instead of
inventing them again — and if the pathology is real, the shape that shows it is
one of the ones **missing** from this list, which is a more useful thing to
know than a bare "could not reproduce".

## Iteration

```bash
cargo run -p khora-cli --features llvm -- run --release bench/iteration
```

Three million elements, summed three ways:

```
for 178 ms   while 46 ms   fold 47 ms
```

`for` desugars to a `loop` over `Step`, and `Step` is an ordinary ADT, so every
element allocates. `fold` builds a closure too and matches the hand-written
loop, which is the row that says where the cost is: one closure per *call* is
free, one `Step` per *element* is not.

Kept as the number that unboxing has to move. `docs/roadmap.md`, under "Unboxed
records", carries why that is the fix rather than desugaring `for` to internal
iteration.
