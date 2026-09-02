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
rustc -O -o bench/loadgen.exe bench/loadgen.rs
rustc -O -o bench/control_keepalive.exe bench/control_keepalive.rs
cargo run -p khora-cli --features llvm -- build bench/service
KHORA_PROFILE=release python bench/measure.py
```

`measure.py` starts each server in turn, walks a ladder against it, repeats the
chosen rung, and prints a table with the machine and the date on it. It reports
what failed instead of a number when a run does not settle, which is the whole
difference between it and what came before.

One server on its own:

```bash
./bench/service/build/service.exe &
./bench/loadgen.exe --port 18952 --label service --connections 32 --seconds 5
```

Ports are fixed so two of these cannot be measured at once by accident:
`floor` 18950, `render` 18951, `service` 18952. The Rust controls take a port
on the command line. `--watch-pid` samples the server's resident memory while
the run is under way.

## Every number here was wrong until 2026-09-02

`bench/load.py` produced every figure this file used to carry, and all of them
were between two and twelve times too high. It ran one process per connection,
each timing itself, and divided the total by the duration it was *asked* for
rather than the one it took -- and on Windows forty-eight Python processes take
fifty-two seconds to get through a four-second run. The workers barely
overlapped, each measured a nearly idle server, and the report was one
connection's rate multiplied by the number of connections.

That is why no ceiling was ever found. A rig whose output is proportional to
its own worker count cannot flatten, so the ladder in `compare.py` refused to
settle every time and the conclusion drawn was that the client could not
saturate the servers. The conclusion came from the artifact. `docs/errata.md`
77 has the full account, including how a server that counts what it answers
settled it in twenty lines.

`load.py` and `compare.py` are gone. `loadgen.rs` and `measure.py` replace
them.

## What was measured

16-core Windows desktop, release builds, 32 connections, generator on the same
machine, six-second runs, mean of five. **These numbers travel with that
sentence or they do not travel.**

| | req/s | p50 | p99 | peak RSS | |
| --- | --- | --- | --- | --- | --- |
| C#, ASP.NET Core (Kestrel) | 268,397 | 101us | 252us | 976 KB | |
| Khora `floor` | > 255,707 | 121us | 216us | 676 KB | generator still climbing |
| Khora `render` | 241,064 | 127us | 245us | 608 KB | |
| Rust control, thread per connection | 202,814 | 150us | 313us | 764 KB | |
| Go, `net/http` | 188,869 | 103us | 1,385us | 992 KB | |
| Khora, `std::net::http` | 174,360 | 156us | 543us | 576 KB | |
| Java, JDK `HttpServer` | > 116,050 | 236us | 665us | 900 KB | ladder still climbing |
| Node, `node:http` | 39,223 | 687us | 3,910us | 996 KB | spread 1.16x |

Rows with a `>` are lower bounds and say why in the last column. `floor` is
fast enough that the generator is still gaining when given more of the machine.
Java's ladder was still climbing at 128 connections, which is a JIT that had
not finished with the handler.

### What the differences say

**`service` against `floor` is the library.** 174,360 against more than
255,707: the whole of `std::net::http` -- request parsing, the header map, the
router, response building -- costs about a third of the throughput of a socket
loop that does none of it. `render` at 241,064 puts most of what is left in
reading the request rather than writing the answer. These are the comparisons
these servers were built for and they are the ones worth the most, because all
three are the same language on the same runtime in the same sitting.

**`floor` against the Rust control is the runtime**, and the control is a
thread per connection, which is not the fastest way to write this server in
Rust. Read it as "the runtime is not what limits either of them" rather than as
a win.

**Against other languages, Khora's `Router` is mid-table.** It is about 8 per
cent under Go's `net/http`, 35 per cent under Kestrel, roughly four times Node
and comfortably ahead of the JDK's server. This repository previously claimed
"at least 6x Kestrel and at least 10x Go" on the strength of the old rig; the
truth is the other way round for both. Each peer is that language's *ordinary*
server rather than a tuned one -- Node's is single-threaded by design and would
be several times faster behind `cluster`, the JDK's is not what a Java service
ships on, and `fasthttp` is faster than `net/http` -- so read the table as
"what you get when you write the obvious thing".

**Latency is not throughput.** Go answers the median request faster than Khora
does (103us against 156us) and the slowest one much more slowly (1,385us
against 543us). A server chosen on peak rate alone would have missed both.

**Memory is flat.** Peak resident set is under a megabyte for every server
here, and for Khora it does not grow with connections: `service` holds 584 KB
at 32 connections and less at 128.

### The four conditions, checked

`/docs/performance/` sets out what would have to be true before a throughput
number is published. `measure.py` checks all four and prints what failed
instead of a number:

**The generator is not the bottleneck.** Its rate stops changing when given
more of the machine -- against the control, 68k, 122k, 181k, 208k at one, two,
four and eight threads, then flat at twelve, sixteen and twenty-four. Eight is
the default for that reason. Where a server is fast enough that this stops
being true, the row is marked and the figure is a lower bound.

**The ladder flattens.** From 16 connections to 128 the rate is level while
median latency rises with concurrency -- `service` runs 176k, 180k, 177k, 176k.
Constant throughput with latency proportional to queueing is what a saturated
server looks like, and it is the shape the old rig could never produce.

**It repeats.** Spread across sittings is 1.03x to 1.05x for the servers that
pass. The figure that disqualified the old rig was 1.85x.

**The machine and the date are printed with the number** by `loadgen` itself,
so a figure cannot be separated from its circumstances by being copied.

## Where the number came from

Phase 9's parser work is measured with `khora bench` rather than over a socket,
so it is unaffected by any of the above: an 80-byte HTTP request parse went
from 2,440ns to 1,555ns, and a browser's fourteen-header request from 14,560ns
to 7,345ns.

## Against other languages

`bench/peers/` holds the same `/health` route in Go, Node, C# and Java, each
using that language's ordinary server rather than a hand-rolled socket loop,
because the comparison worth making is against what a team would actually
write. `measure.py` runs them alongside the Khora servers, so the table above
is one sitting rather than several stitched together.

`--release` made no difference to `std::net::http` when that was last checked:
1.73M optimised against 1.76M unoptimised under the old rig. Both figures are
retired with the rig, but the *equality* between them is a ratio within one
sitting, which is the one thing that rig could measure, so the conclusion
stands: for this workload the time is in the kernel rather than in the
generated code and the profile has nothing to work on. Worth knowing before
anybody attributes a benchmark result to an optimiser.

### What these peers are not

Each is the language's *ordinary* server. Node's is single-threaded by design
and would be several times faster behind `cluster`; Java's JDK server is not
what a Java service ships on, and Netty or Undertow would be far above it;
`net/http` is Go's real answer and a specialised library like `fasthttp` is
faster. Read the table as "what you get when you write the obvious thing",
which is the comparison a team actually faces, not as a ranking of runtimes.

### What a second machine would still buy

The generator and the servers share sixteen cores, and two of the rows above
are lower bounds because of it. A generator on its own machine would remove
the last of the doubt and let `floor` be measured rather than bounded. Roadmap
13.23.

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
