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

## Arithmetic and comparison are operators

`+`, `-`, `*` and unary `-` on two `Decimal`s are `Decimal::add`, `sub`,
`mul` and `negate`: the same answer at the same scale, and the same stop when
it does not fit.

```khora
let total = subtotal + shipping - discount;
let tax = Decimal::rounded(19.99d * 0.0825d, 2, Rounding::HalfEven);
let refund = -total;
```

The methods are the same calls, and read well at the end of a pipeline:

```khora
let tax = 19.99d |> Decimal::mul(0.0825d) |> Decimal::rounded(2, Rounding::HalfEven);
```

Both operands are `Decimal`s. `price + 1` is refused, because no number
type in Khora converts silently into another; write `price + 1d`, or
`price + Decimal::of_int(count)` for a count held in an `Int`.

**Division is a method**, `Decimal::divide(a, b, scale, rounding)`, and `a / b`
is refused with a message that says so. A quotient usually has no exact
decimal form, so it needs the scale to answer at and the rounding for what is
left over, and an operator has nowhere to write either.

`Decimal` compares with the ordinary operators too, because it implements
`Eq` and `Ord` — **by value**, so `1.50d` and `1.5d` are the same number:

```khora
if paid == owed { settle(invoice) }
if amount < limit { approve(amount) }
```

An operator stops the program where its method does; `checked_add`,
`checked_sub`, `checked_mul` and `checked_divide` answer `None` instead. An
operator traps; `checked_*` refuses.

## Scale is part of the number

`*` **adds** the scales: two numbers with four decimal places give one with
eight. `+` and `-` bring both operands up to the larger scale. That is what makes
the arithmetic exact, and it is also what makes a long chain overflow sooner
than people expect. `rounded` is the way back:

```khora
let charge = Decimal::rounded(total, 2, Rounding::HalfEven);
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

let together = notional + rate;  // needs the notional at scale 12
```

Aligned, the notional needs 20 significant digits — well inside 38, and the
sort of pair a rates desk writes down every day. Going past 38 digits stops the
program: the answer would be a different number, and a different total is the
failure this type exists to prevent.

## Amounts you did not choose

Stopping the program is right when the program made the number, and wrong when
the number arrived in a request: the stop ends every other request the server
was serving. `checked_add`, `checked_sub`, `checked_mul` and `checked_divide`
answer `None` where `add`, `sub`, `mul` and `divide` stop, and the same number
everywhere else, so an absurd amount becomes a refusal of the one request that
sent it:

```khora
fn order_total(price: Decimal, quantity: Decimal, shipping: Decimal) -> Result<Decimal, String> {
  match Decimal::checked_mul(price, quantity) {
    Option::Some(subtotal) => match Decimal::checked_add(subtotal, shipping) {
      Option::Some(total) => Result::Ok(total),
      Option::None => Result::Err("400: the total is too large"),
    },
    Option::None => Result::Err("400: the quantity is too large"),
  }
}
```

`checked_divide` takes the same scale and rounding as `divide`, and is `None`
for a zero divisor as well as for a quotient that does not fit.
`checked_add` and `checked_sub` are `None` also when the two scales are more
than 38 apart, even if the answer would fit, because neither operand can be
moved that far; `+` and `-` stop there.
`Decimal::of_int(n)` is the whole number `n`, for mixing a count into one of
these.

## A column of them

```khora
let total = Decimal::total(rows);              // every one added up
let shown = Decimal::at_scale(Decimal::total(rows), 2);  // and at the column's width
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
