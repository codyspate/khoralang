---
title: Making your program faster
description: How to find out where a Khora program spends its time, what the toolchain measures for you, and what it does not.
sidebar:
  order: 2
---

This page is about **your** program. [Performance](/docs/performance/) is about
Khora's own HTTP server: what it answers, measured how, against what. That page
runs commands from the compiler's repository, so a toolchain you installed
cannot run them.

There is **no sampling profiler**. Nothing in the toolchain will point at a
line and tell you it is hot. What there is, in the order you should reach for
it:

## 1. Build with `--release` before believing any number

```sh
khora build . --release
```

The default profile is `debug`: unoptimized, with debug information, which is
what a crash you are about to read wants. `khora test` and `khora bench` have
no flag of their own and read `KHORA_PROFILE` instead:

```sh
KHORA_PROFILE=release khora bench .
```

A `bench` run in the default profile says so in its output, because a
microbenchmark of unoptimized code is a measurement of the wrong program.

**How much release buys you depends on where the time goes**, and the
relationship is not the one you might expect. A small integer loop differs by
roughly a factor of two between profiles. But the better your program gets, the
less the optimizer has left to find: one word-frequency counter measured 1.45×
between profiles in its first form, 1.12× after the hot loop moved into a
`Map`, and 1.09× once it streamed. Work done inside the standard library is
already compiled; only your own code changes between profiles.

## 2. Time a whole operation with `khora bench`

A `bench` block times a body and reports a distribution, not a single number:

```khora
bench "parsing a page of input" {
  let _ = parse(sample);
}
```

```sh
khora bench . --filter parsing
```

`--filter` takes a substring, so one name runs one benchmark. This is the right
tool when you can call the thing you care about directly.

**Put each benchmark where you can find it again.** Spreading `bench` blocks
across files is fine, and so is keeping them beside the code they measure.

## 3. Time stages inside a real program with the clock

`khora bench` measures something you can call in isolation. When the question
is "which stage of this pipeline is slow", the answer is a capability you
already have:

```khora
import std::clock::{Clock};
import std::core::{print};

pub fn main() -> Int {
  with { clock: Clock::real() } {
    let start = clock.monotonic_millis();
    let parsed = parse(input);
    let after_parse = clock.monotonic_millis();
    let counted = count(parsed);
    let after_count = clock.monotonic_millis();

    print("parse ${after_parse - start}ms, count ${after_count - after_parse}ms");
    0
  }
}
```

`monotonic_millis` is the one to use for durations. It only ever moves forward,
so it cannot be pulled backwards by a clock adjustment mid-measurement — which
a wall clock can, and then your timing is negative.

This is manual instrumentation, and it is the technique rather than a
workaround: with no profiler, narrowing by hand is how you find the stage that
costs you. Time coarse stages first, then subdivide the one that dominates.

## 4. Count allocations when the time is not in arithmetic

A program can be slow because it is allocating, and that does not look like a
hot loop. The runtime can tell you how many objects are live, through an
`extern` declaration — it is not a `std` function, so you declare the symbol
yourself:

```khora
extern fn khora_live_count() -> Int;

test "counting words allocates a constant number of objects" {
  let before = khora_live_count();
  let _ = count(sample);
  assert_that(khora_live_count() == before, "the count leaked objects");
}
```

Khora's own standard-library suite uses exactly this to pin down allocation
behaviour — see `tests/std-suite/src/vector.kh` for the shape, where it
asserts that a vector of numbers allocates a constant number of objects
however many numbers go in.

An operation whose allocation grows with its input, when it should not, is
usually a value being rebuilt rather than updated. `String` concatenation in a
loop is the common one: each `+` builds a new string, so a loop over *n* items
copies *n²* characters. That is the shape behind most "it got slower than it
should have" surprises.

## What is not here

- **No sampling profiler**, so no flamegraph and no "this line is 40% of your
  runtime". Sections 3 and 4 are how you find that out.
- **No allocation profiler.** `live_count()` gives you a number at a point, not
  a breakdown by type or site.
- **`khora build` emits DWARF on Linux**, so an external tool such as `perf`
  has symbols to work with. Nothing in CI exercises that, so treat it as
  something that may work rather than a supported workflow, the same caveat
  [Debugging](/docs/reference/debugging/) gives for a debugger.

## What to measure first

In order, because each one is cheaper than the next and rules more out:

1. Is it built `--release`? A factor of two hides a lot.
2. Does the work grow faster than the input? Double the input and time it. A
   doubling that quadruples the time is an accidentally-quadratic operation,
   and no amount of constant-factor tuning will save it.
3. Which stage? Coarse `monotonic_millis` brackets, then subdivide.
4. Is it allocating per item where it should allocate once?
