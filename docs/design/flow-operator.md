# D16 — the flow operator, `||>`

**Proposed, not built.** This is the specification and the case for it, written
down while it is fresh so the decision is ready when the evidence is.

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

**The spelling.** `||` is logical-or and is used about fifteen times in `std`
(`if digit < 0 || digit > 9`). `||>` is unambiguous to the parser — it appears
only in prefix position, and `a || > b` has no valid parse — but a reader
scanning `||>` sees `||` first, and that cost is paid at every use forever.
`fn |> a |> b` is worth weighing against it: one character longer, no
collision, and it reuses a keyword that already means "a function starts here".
Against that, `||>` *looks* like a pipeline with an open left end, which is
exactly what it is, and it is the spelling the name was chosen with.

**The flow operator is greedy**, consuming every following `|>`. So piping the
function it makes somewhere else needs parentheses: `(||> a) |> b`. That is the
only sensible rule and it needs to be a documented one with a test, rather than
an accident of the parser.

**A one-stage flow should be a lint.** `List::map(||> normalize)` and
`List::map(normalize)` behave identically and the first allocates a closure.
The documentation will say to prefer the second; a diagnostic says it where it
matters.

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

## Why it is not built yet

Syntax is the one decision that cannot be walked back — `compatibility.md` is
the reason, and 13.11 is the item that exists to hold the public surface to it.
The cost of waiting is small and the cost of a spelling somebody regrets is
permanent.

**The trigger:** write 13.18's HTTP + Postgres + tracing service, the first
program in this repository large enough to have real pipelines. Build this when
that application wants it — by then the spelling will have been felt rather
than guessed.
