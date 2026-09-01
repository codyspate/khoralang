---
title: Performance
description: What is measured, how, and why no throughput numbers are published yet.
sidebar:
  order: 1
---

**No requests-per-second figure for Khora is published on this site.** This
page says what is measured, how to run it yourself, and why the numbers stay in
the repository until they mean something.

That is not modesty. A benchmark number without the ladder that produced it is
unfalsifiable, and an unfalsifiable number in a language's marketing is the
thing that makes every other claim in the documentation worth less.

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

## Why nothing is published

**The load generator is the limit, not the servers.**

`bench/load.py` runs in the same process on the same machine as the thing it is
measuring, and on the hardware used most recently neither fiber implementation
could be driven to a ceiling at all: at 320 concurrent connections both were
still climbing roughly linearly. A rate that is still climbing is the client's
rate. `bench/compare.py` exists to refuse to report one.

The same configuration also does not repeat. One implementation at 160
connections gave 948k in one sitting and 1,760k in the next — **1.85× apart,
same binary, same machine, minutes apart.** That spread is wider than most of
the differences anybody would want to read off a table.

Both facts are recorded in `docs/design/fibers.md`, with the dated numbers and
their limits, and the harness is [tracked as a known
gap](/docs/limitations/#the-fiber-scheduler).

## Running them yourself

```bash
cargo build -p khora-rt
cargo run -p khora-cli --features llvm -- build bench/service
./bench/service/build/service.exe &
python bench/load.py 18952 "service"
```

Ports are fixed so that two cannot be measured at once by accident: `floor` is
18950, `render` 18951, `service` 18952.

**Build with `KHORA_PROFILE=release`** for anything you intend to quote. A debug
build is the default everywhere in this toolchain, deliberately — a language
being brought up should give a readable crash before it gives a fast one — and a
debug number is not a number about the language.

`bench/README.md` in the repository has the rest, including what each server
does and does not do.

## What would have to be true to publish one

1. A load generator that is not the bottleneck — a separate machine, or a
   generator that can saturate the server on this one.
2. A ladder of concurrencies where the rate flattens, so that the top of the
   ladder is the server's answer and not the client's.
3. The same configuration repeating across sittings, to within something much
   tighter than 1.85×.
4. The machine, the profile and the date printed beside the number.

Until all four hold, the honest thing to publish is this page.
