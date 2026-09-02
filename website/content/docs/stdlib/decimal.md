---
title: Decimals
sidebar:
  order: 2
---

`Decimal` is exact base-10 arithmetic, and it is deliberately not `Float`. Use
`Float` for approximate numerical work; use `Decimal` when the base-10
representation is part of being correct — money, tax, anything a schema or a
regulator specified in decimal digits.

```khora
let subtotal: Decimal = 19.99d;
let tax_rate: Decimal = 0.0825d;
```

The `d` suffix is the literal; the module needs
`import std::decimal::{Decimal};` for it to have a type. The suffix is visible
in the source on purpose, so a reader can tell at a glance which kind of
arithmetic an API is doing.

## Arithmetic is by method, comparison is by operator

`Decimal` compares with the ordinary operators, because `Eq` and `Ord` are
traits and `Decimal` implements them — **by value**, so `1.50d` and `1.5d` are
the same number:

```khora
if paid == owed { settle(invoice) }
if amount < limit { approve(amount) }
```

Arithmetic is by name. `+`, `-` and `*` belong to the primitive numeric types,
and adding a trait per operator is a language change Khora has not taken:

```khora
let total = subtotal.add(shipping).sub(discount);
let tax = 19.99d |> Decimal::mul(0.0825d) |> Decimal::rounded(2, Rounding::HalfEven);
```

Both forms are the same call. The pipeline is the shape a chain of money
operations tends to take, and is worth knowing for that reason.

## Scale is part of the number

`mul` **adds** the scales: two numbers with four decimal places give one with
eight. `add` brings both operands up to the larger scale. That is what makes
the arithmetic exact, and it is also what makes a long chain overflow sooner
than people expect. `rounded` is the way back:

```khora
let charge = total.rounded(2, Rounding::HalfEven);
```

`Show` prints every place the scale says, so `Decimal::scaled(150, 2)` is
`1.50` and not `1.5`. A price to two places stays a price to two places, which
is what makes a column of them line up.

`divide` is the one operation that cannot be exact, so it takes the scale and
the rounding mode and answers `Option<Decimal>` — `None` when the divisor is
zero. Everything else stops the program rather than returning a number that is
not the answer; see [Traps](/docs/reference/traps/).

## How much fits

The significand is 128 bits — 38 digits. That covers every fiat computation,
`NUMERIC` as real schemas declare it, and an eighteen-decimal token balance.

It matters because alignment happens before addition, so what has to fit is not
the two numbers but the *aligned* ones:

```khora
let notional = 100000000.00d;   // scale 2
let rate = 0.000000000001d;     // scale 12

let together = notional.add(rate);  // needs the notional at scale 12
```

At 64 bits that stopped the program, on two numbers a rates desk writes down
every day. Going past 38 digits still stops it — the answer would be a
different number, and a different total is the failure this type exists to
prevent.

## A column of them

```khora
let total = Decimal::total(rows);              // every one added up
let shown = Decimal::total(rows).at_scale(2);  // and at the column's width
```

`total` of nothing is `Decimal::zero()`, which has scale nought and prints as
`0`; `at_scale` only ever raises the scale, so it cannot round a total that was
already wider. `Decimal::zero_at(2)` is the empty total where the shape matters
from the start. `abs`, `min` and `max` are there too, comparing by value, so
`1.5d` and `1.50d` are one number to them as well.

## See also

- [`std::decimal` API](/docs/stdlib/api/decimal/) — every operation, with its
  signature.
- [Decimal literals](/docs/reference/lexical-structure/#decimal-literals) — the
  `d` suffix in the grammar.
- [Traps](/docs/reference/traps/) — what overflow does, and why it is not a
  wrapped answer.
