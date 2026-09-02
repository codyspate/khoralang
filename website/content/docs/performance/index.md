---
title: Performance
description: What Khora's HTTP server answers, measured against a generator that is not the bottleneck, and what was wrong with every figure published before it.
sidebar:
  order: 1
---

**Khora's HTTP server answers about 174,000 requests a second on a 16-core
desktop, in 8.4 MB of memory.** That is a little under Go's standard library
on rate and between three and eighty times less memory than any of the
runtimes it is measured against. This page says what that measures, what it
does not, how to run it yourself, and why every figure published here before
September 2026 was between two and twelve times too high.

A benchmark number without the ladder that produced it is unfalsifiable, and an
unfalsifiable number in a language's marketing makes every other claim in the
documentation worth less. So the conditions a figure has to meet are written
down below, the script checks them on every run, and a server that fails one is
reported with the failure instead of with a number.

## What is measured

`bench/` holds servers that answer the same request, so that the difference
between any two of them is one thing:

| | what it is |
| --- | --- |
| `control.rs` | Rust, a thread per connection, closing after each request |
| `control_keepalive.rs` | the same, holding the connection open |
| `floor/` | Khora: accept, read, write a fixed string. No parsing |
| `render/` | the floor plus `Response::rendered_keeping`. No parsing |
| `service/` | a `Router` with one route — the whole of `std::net::http` |
| `allocator/` | not a server: what allocation costs as the heap fills |
| `iteration/` | not a server: what `for` costs against the loop written out |

The comparisons that mean something are the differences. `floor` against
`control_keepalive` is what the **runtime** costs. `service` against `floor` is
what the **library** costs. `render` sits between them and says how much of the
library is building the response rather than reading the request.

A single number for `service` on its own says nothing, which is why the list is
shaped this way.

## The numbers

16-core Windows desktop, release builds, 32 connections, generator on the same
machine, six-second runs, mean of five, 2 September 2026. **These numbers
travel with that sentence or they do not travel.**

| | req/s | p50 | p99 | peak RSS |
| --- | --- | --- | --- | --- |
| C#, ASP.NET Core (Kestrel) | 266,267 | 103us | 253us | 240 MB |
| Khora `floor` | > 234,322 | 128us | 274us | 7.4 MB |
| Khora `render` | > 234,039 | 127us | 253us | 7.8 MB |
| Rust control, thread per connection | 202,182 | 150us | 319us | 5.5 MB |
| Go, `net/http` | 185,670 | 102us | 1,538us | 21.8 MB |
| Khora, `std::net::http` | 174,201 | 161us | 554us | 8.4 MB |
| Java, JDK `HttpServer` | > 114,200 | 251us | 663us | 699 MB |
| Node, `node:http` | 39,184 | 682us | 4,101us | 86.8 MB |

`floor` and `render` are lower bounds: they are fast enough that the generator
was still gaining when given more of the machine. Java's is one too, because
its rate was still climbing at the top of the ladder, which is a JIT that had
not finished with the handler.

**Khora's HTTP server is mid-table on throughput and first on memory, and both
halves of that are the headline.** It is about six per cent under Go's standard
library and a third under Kestrel on requests a second. It does it in 8.4 MB,
against Go's 21.8, Node's 86.8, Kestrel's 240 and the JDK's 699 -- between
three and eighty times less than the runtimes it is answering as fast as. That
is what "no VM and no tracing GC" is worth, in the one column where the claim
can be checked.

Each peer is that language's *ordinary* server, not a tuned one. Node's is
single-threaded by design and would be several times faster behind `cluster`;
the JDK's is not what a Java service ships on; `fasthttp` is faster than
`net/http`. Read the table as what you get writing the obvious thing.

**Latency is not throughput.** Go answers the median request faster than Khora
and the slowest one nearly three times more slowly. A server chosen on peak
rate alone would have missed both halves of that.

### The comparisons worth the most

The cross-language row is the one people look at and the one that says least,
because eight runtimes differ in eight ways at once. The Khora rows differ in
one thing at a time and are what the servers were built for:

`service` against `floor` is **the library**: the whole of `std::net::http` --
parse, header map, route match, render -- costs about a quarter of the
throughput of a socket loop that does none of it. `render` sits with `floor`,
which puts what is left in reading the request rather than writing the answer.
`floor` against the Rust control is **the runtime**, and the control is a
thread per connection, which is not the fastest way to write it in Rust, so
read that pair as "the runtime is not what limits either" rather than as a win.

## What was published before this, and why it was wrong

Every throughput figure this project recorded before 2 September 2026 came from
a load generator that reported one connection's rate multiplied by the number
of connections, and was between two and twelve times too high.

The old rig ran one process per connection, each timing itself, and divided the
total by the duration it was *asked* for rather than the one it took. Forty-eight
Python processes on Windows take fifty-two seconds to get through a four-second
run, so the workers barely overlapped, each measured a nearly idle server, and
the divisor stayed at four. A rig whose output is proportional to its own worker
count cannot flatten, which is exactly why no ceiling was ever found and why the
spread between sittings looked like 1.85x -- process startup time was what
varied.

A server that counts what it answers settled it in twenty lines. The full
account is in [the errata](https://github.com/codyspate/khoralang/blob/main/docs/errata.md).

The replacement is `bench/loadgen.rs`: a few threads each driving many
non-blocking connections, rather than a thread or a process per connection. The
change that mattered was not the language. A blocking read parks the thread and
the kernel wakes it again on every response, about 120 microseconds on a round
trip whose median is 29; the same connection spinning on a non-blocking socket
answers five times as many requests.

## What a figure has to satisfy before it is published

1. A load generator that is not the bottleneck — a separate machine, or a
   generator that can saturate the server on this one.
2. A ladder of concurrencies where the rate flattens, so that the top of the
   ladder is the server's answer and not the client's.
3. The same configuration repeating across sittings, to within something much
   tighter than 1.85×.
4. The machine, the profile and the date printed beside the number.

All four hold for the table above, and `bench/measure.py` checks them on
every run rather than leaving them to be remembered. A server that fails one
is reported with what failed instead of with a number, which is the whole
difference between this rig and the one it replaced: the old one could not
fail, so it always produced a figure.

## Running them yourself

```bash
rustc -O -o bench/loadgen.exe bench/loadgen.rs
rustc -O -o bench/control_keepalive.exe bench/control_keepalive.rs
cargo run -p khora-cli --features llvm -- build bench/service
KHORA_PROFILE=release python bench/measure.py
```

`measure.py` starts each server in turn, walks the ladder, repeats the chosen
rung and prints the table with the machine and the date on it. One server on
its own:

```bash
./bench/service/build/service.exe &
./bench/loadgen.exe --port 18952 --label service --connections 32 --seconds 6
```

Ports are fixed so two cannot be measured at once by accident: `floor` is
18950, `render` 18951, `service` 18952. `--watch-pid` samples the server's
resident memory while the run is under way.

**Build with `KHORA_PROFILE=release`** for anything you intend to quote. A
debug build is the default everywhere in this toolchain, deliberately — a
language being brought up should give a readable crash before it gives a fast
one — and a debug number is not a number about the language. `measure.py` says
so and carries on rather than refusing, because a debug comparison between two
Khora servers is still a comparison.

`bench/README.md` in the repository has the rest, including what each server
does and does not do.

## What these do not measure

A `/health` route returning a fixed JSON object is the thinnest possible
request. It isolates the library from the handler, which is the point, and it
resembles no real workload: nothing here has a body worth parsing, a database
behind it, or a response worth rendering. Cold start is not measured, and
neither is behaviour under more connections than the machine has cores.
