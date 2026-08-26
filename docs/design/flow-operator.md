# D16 — the flow operator, `||>`

**Built.** This was the specification; it now describes what exists. The
sections on what to settle are kept, with what was settled recorded in them,
because the reasoning is the part worth having later.

`||>` is **the flow operator**, and that is what to call it everywhere: in the
documentation, in diagnostics, and in conversation. Not "flow lambda", which
names the machinery, and not "anonymous pipeline", which is what it produces
rather than what it is.

## What it is

The flow operator starts a unary anonymous function whose argument becomes the
first value of a pipeline.

```khora
users |> List::map(
  ||> normalize
  |> validate!
  |> persist!
)
```

is exactly

```khora
users |> List::map(fn value =>
  value
  |> normalize
  |> validate!
  |> persist!
)
```

In general `||> a |> b |> c` desugars to `fn x => x |> a |> b |> c`, with a
parameter name the compiler generates and no source can collide with.

A named function still needs nothing: `List::map(normalize)`.

## Why

**The pattern is real and this project has not written it yet.** At the time of
proposing, `|>` appears fourteen times in the whole repository and every one is
a builder chain on a named value — `router |> Router::post(..)` — while
`fn x => x |> ..` appears zero times. That is not evidence against; it is
evidence that `std::core` has no free `List::map`/`filter` to chain and nothing
here has needed one.

The evidence comes from outside. In Effect TypeScript the same shape is written
`Array.map(xs, flow(funcA, funcB, funcC))`, and it is reported as frequent in
production reconciliation software — which is the domain `docs/positioning.md`
opens by naming. That is also where the name comes from.

`flow` composes function *values*; the flow operator pipes through *call
expressions*, so `|> enrich(config)` works without a second combinator for
partial application. That is a genuine improvement on the thing being copied,
not a translation of it.

## What it must not become

Deliberately narrow, and the boundaries are the design:

- A **unary** anonymous pipeline. Nothing else.
- `_` does **not** become a general placeholder expression.
- No generalized point-free syntax.
- No new effect, failure, ownership or runtime behaviour.
- `|>` precedence and call-insertion are untouched.

## How to build it

Sugar, desugared before it can reach anything semantic:

1. The lexer produces `||>` as one token.
2. The parser recognises a flow expression beginning with it.
3. HIR lowering emits an ordinary unary lambda.

From there, lambda inference, effect rows, failure rows, ownership,
monomorphization and LLVM lowering handle it without knowing it existed. **Do
not carry a new construct past lowering**, and do not special-case `!` inside
one: `||> parse!` is `fn x => x |> parse!` and its raises row follows from that
and nothing else.

The design test is that `||> a |> b |> c` and `fn x => x |> a |> b |> c`
produce the same HIR, the same type, the same `with` row and the same `raises`
row.

## Three things to settle when building it

**The spelling — settled as `||>`.** `||` is logical-or and is used about
fifteen times in `std` (`if digit < 0 || digit > 9`), so a reader scanning
`||>` sees `||` first and that cost is paid at every use. `fn |> a |> b` was
the alternative: one character longer, no collision. `||>` was chosen anyway,
because it *looks* like a pipeline with an open left end, which is exactly what
it is, and because the operator's name came from `flow` and the shape reads as
one.

The lexer is unambiguous either way: `||>` is one token, listed before `||` so
the longer match wins, and `a || > b` has no valid parse so nothing legitimate
was taken away. A test asserts that logical-or still lexes as itself, since
that is the thing the spelling puts at risk.

**The flow operator is greedy**, consuming every following `|>`. So piping the
function it makes somewhere else needs parentheses: `(||> a) |> b`. That is the
only sensible rule and it needs to be a documented one with a test, rather than
an accident of the parser.

**A one-stage flow should be a lint — not built.** `List::map(||> normalize)`
and `List::map(normalize)` behave identically and the first allocates a
closure. The documentation says to prefer the second; a diagnostic would say it
where it matters. Left for the diagnostics pass, 13.17, because it is a lint
and not a language question.

## Diagnostics

A malformed flow — `let f = ||>;`, or anything where no valid first stage
follows — reports against **the flow operator**, and never mentions the lambda
it would have become. The desugaring is an implementation detail and a person
who reads it in an error message learns something they cannot use.

## Tests it needs

Lexer, parser, HIR, type check, codegen and formatter. At minimum: one stage;
several stages; a stage with extra arguments (`||> enrich(config) |> save`);
fallible stages with the inferred `raises` asserted against the explicit-lambda
equivalent; capability-using stages with the `with` row likewise; use inside
`List::map`; nesting inside calls and blocks; formatter round trips, compact
and multiline; a missing first stage diagnosed against the flow operator; and a
regression test that ordinary `|>` parsing is unchanged.

## Formatting

Compact on one line where it fits:

```khora
List::map(||> normalize |> validate!)
```

and otherwise one stage per line, with the flow operator and the pipes that
follow it left-aligned so the shape of the pipeline is visible:

```khora
List::map(
  ||> normalize
  |> validate!
  |> enrich(config)
)
```

## What it cost

Six small changes and nothing deep, which is the whole argument for doing it as
sugar. One lexer token; one syntax kind for the node and one for the token; a
parser function of thirty lines; a lowering that builds the lambda; one line in
the formatter; and the two syntax highlighters.

The lowering shares `pipe_into` with `lower_pipe` rather than reimplementing
it. **That sharing is what makes the two spellings the same program** instead
of two things that agree today: call insertion, the `_` placeholder and where a
`!` ends up are decided in one place, once.

The parameter is called `flow value` — with a space, so no source can declare
it and no source can refer to it. The reference is built as a resolved local
rather than looked up by name, so nothing tries.

## Tests

`crates/khora-syntax/tests/flow.rs` for the token and the tree, including that
logical-or is untouched and that ordinary `|>` parsing is unchanged.
`crates/khora-codegen-llvm/tests/flow.rs` end to end, where several tests run
*both* spellings in one binary and compare — a test that only checked the
flow's answer would pass just as happily if the desugaring quietly meant
something slightly different. The formatter's are in
`crates/khora-fmt/tests/format.rs`.
