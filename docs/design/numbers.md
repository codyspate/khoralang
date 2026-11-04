# Numbers

Phase 6.2 and 6.3. Two decisions here were made without being asked about, and
both are flagged as such: overflow was decided in advance, `Float`'s traits were
not.

## Overflow traps, in every build

`+`, `-` and `*` on `Int` stop the program when the result does not fit. Not
"in debug", not "unless optimized" — always.

**Unary `-` is the same operation, from zero.** `-x` on a signed type's
minimum stops the program as `x * -1` does, because the minimum has no
positive twin, and `-x` on an unsigned value stops for anything but zero. Left
unchecked, `-x` answered the minimum itself where `x * -1` stopped — two
spellings of one product disagreeing silently. A negated *literal* is not a
negation: `-128` is one `I8` and is folded before code generation.

Rust's split (panic in debug, wrap in release) is the one this audience knows,
and it is the one rejected. A program that passes its tests and then wraps in
production is the failure worth spending a branch on, and two behaviors put
the difference exactly where it is most expensive to find. Swift traps
everywhere and is not thought of as slow.

The branch is cheap by construction: LLVM's `with.overflow` intrinsics return
the result and the flag from one instruction, so nothing is computed twice, and
phase 9 can remove many of the checks outright.

**Wrapping is still reachable, by name.** `Int::wrapping_add`,
`wrapping_sub` and `wrapping_mul` do what the operators used to, for the places
that genuinely want it — a hash, a checksum, a pseudo-random number. Asking for
it explicitly is what lets the trap be the default without being in the way;
`Map::slot` in `std::core` is the first caller and a fair example of the whole
category.

`/` and `%` trap too, on both the ways they can go wrong: a zero divisor, and
the minimum over minus one — whose quotient is one past the maximum. Unsigned
division cannot overflow, so only the first check is generated for it.

Both are *undefined* in LLVM, and what they do on hardware is a fault with no
message attached: a bare `0xC0000094` or a `SIGFPE`, naming neither the line
nor the value. A trap that says which is strictly better, whatever the eventual
answer to the next question is.

**And the next question is open.** A trap says a division by zero is a
programmer's mistake, which it usually is — Go panics and Rust panics. But a
divisor that came from a file or a socket is *data*, and the honest answer
there is the error channel: `raises DivideByZero`, handled where the data came
in. That would put a row on every function containing a division, which is a
real cost and not obviously worth paying for the common case. `Int::checked_div`
returning an `Option` is the middle answer, and it is the one built: every
integer type has `checked_add`, `checked_sub`, `checked_mul`, `checked_div` and
`checked_neg` (and the signed ones `checked_abs`), each `None` exactly where the
operator traps. They lower to the same `with.overflow` intrinsic as the
operator, with the flag turned into the `Option`'s tag instead of a branch to
the trap. `Decimal` has `checked_add`, `checked_sub`, `checked_mul` and
`checked_divide`, which share their computation with the trapping forms in the
runtime. The trap stays the default; a number from outside the program is
the case for asking.

## Bit operations are methods, for now

`Int::xor`, `and`, `or`, `shl`, `shr`. Operators would be nicer and are five
new tokens, one of which (`>>`) has to be told apart from the end of two nested
type arguments. None of that is hard; none of it was what a hash function was
waiting for.

`shr` is arithmetic — a negative number stays negative. A logical shift is what
`Int` cannot express and an unsigned fixed-width type will.

Shifting by 64 or more is undefined in LLVM, so the count is masked to the
width. Silently, and deliberately: every shift would otherwise need a branch,
and there is no answer for `x << 64` that is more right than any other.

## `Float` is IEEE, and implements neither `Eq` nor `Ord`

**This one was decided without being asked**, under the standing instruction to
decide and flag rather than stop. It is the most reversible kind of decision —
adding an impl later breaks nothing — but it is a real choice and here is the
reasoning.

`==`, `<` and the rest are **primitive** on `Float` and mean what IEEE says.
`0.1 + 0.2 == 0.3` is false. `NaN == NaN` is false, and `NaN != NaN` is true.
Every one of Go, Rust and TypeScript does exactly this, so the tie-breaker in
`docs/vision.md` decides it without further argument.

The traits are a different question. `Eq` in `std::core` is an equivalence —
code that is generic over it may reasonably assume `x == x` — and a `Float`
does not satisfy that. So:

> **The operator is primitive; the trait is for lawful equality.** `Float` has
> the first and not the second.

This is Rust's `PartialEq`/`Eq` split arriving at the same place without the
second trait, and Khora can afford it because `==` never went through `Eq` in
the first place: `impl Eq for Int` is written *in terms of* `==`, not the other
way round.

What it costs: a function taking `A: Eq` cannot be given a `Float`, and a
`Float` cannot be a key in anything that hashes. Both are correct — a NaN key
is a lost entry in any language — and if a total order is ever wanted, an
explicit `total_cmp` that orders NaN somewhere definite is the way to ask for
it, exactly as Rust does.

## No mixing, and no promotion

`1 + 2.0` is an error. The left operand decides which arithmetic is being done
and the right must match, which is what Go and Rust both do and what stops a
rounding surprise from being invisible. There is no implicit widening between
integer types either, for the same reason, and there will not be when the
fixed-width types land.

## The fixed-width integers

`U8`, `U16`, `U32`, `U64`, `I8`, `I16`, `I32`. **`I64` is not among them** — it
is a second spelling of `Int`, because two 64-bit signed integers would mean a
conversion between them that can never fail and never does anything.

Everything is at the type's own width, which is the only thing that makes any
of it worth having. A `U8` addition traps at 255, not at 2^63. `255 < 100` is
false, because an unsigned type compares unsigned. `>>` brings in zeros for an
unsigned type and copies of the sign bit for a signed one — the logical shift
`Int` could never express. And an `Array<U8>` is **one byte per element**: the
array header carries the element's stride, so a byte buffer costs what a byte
buffer should. `Bool` came along for the ride and is a byte now too.

### A literal takes the type being asked of it

`let b: U8 = 65`, and the `56` in `U8::wrapping_add(b, 56)`. Without it every
byte in a table would be `U8::of(65)`, which is not a language with bytes so
much as a language that can describe them.

It is a **hint**, not a demand: consumed by the first expression that reads it,
and re-armed only where a type passes through unchanged — the branches of an
`if`, the tail of a block, the arms of a `match`, the left operand of an
arithmetic operator. Anywhere else it would leak into a subexpression that
means something different: the `0` in `array[0]` is an index however the
element is going to be used.

The hint also reaches a call's *arguments*, by way of its result. Nothing in
`Array::new(length, fill)` says the fill is a `U8` until `Array<A>` has met
`Array<U8>`, so the expected type solves the return first and the arguments
learn from it. A literal that then does not fit is a compile error, which is
the overflow trap made earlier: `let b: U8 = 300` has one right answer and
truncating it silently to 44 is the kind of thing that is found in production.

**The sign is part of the literal.** `-128` is an `I8`, even though `128` is
not — a negated literal is checked as one number rather than as a negation
applied to another, and there is no other way to write that type's smallest
value.

**A based literal is the number it names, not a bit pattern.** `0xFF`, `0b1010`
and `0o17` take the hint exactly as `255`, `10` and `15` do, so `let b: I8 =
0xFF` is refused for the reason `let b: I8 = 255` is, and `-0x80` is the same
`I8` as `-128`. Reading `0xFF` as an `I8`'s `-1` would make a literal's value
depend on the type it lands in, which is the silent narrowing the rest of this
section exists to refuse; a program that means the bits writes
`I8::wrapping(0xFF)`. The one cost is that `Int`'s top bit cannot be written as
`0x8000_0000_0000_0000`, which is past `Int::MAX`; it is a `U64`, or the
negated literal `-0x8000_0000_0000_0000`, which is `Int::MIN`. A negated
literal is one number in every signed type, `Int` included, so each type's
minimum is written the way it reads, in either base.

The same rule holds for a literal *pattern*, against the type of the value it
is matched against: `0x100 =>` on a `U8` is refused as `let b: U8 = 0x100` is.
It is checked once the types settle, because a lambda's parameter is still a
variable where its `match` is read. The code generator builds the constant at
the value's width, and that keeps only the low bits, so a literal that
reached it unchecked would match the wrong value with no error. One once did:
`0x100 =>` matched a `U8` holding `0`. The code generator therefore checks
the range again and stops the build instead of narrowing.

**Negation is checked with the same overflow intrinsic as subtraction, and
that costs about what `+` does:** in a release loop of `wrapping_add(acc, -x)`
over an `Array<Int>`, 4 instructions and about 10% of the cycles per element
over an unchecked `neg`. The unchecked form folds the load into the
accumulating `sub`. The checked form holds `x` in a register, negates it to
get the flag, branches, then adds. Spelling the check as a compare against the
minimum compiles to the same `neg; jo`, at the same count.

### Conversions go through `Int`

`U8::of(n)` traps if `n` does not fit; `U8::wrapping(n)` truncates; `U8::to_int`
goes back. Four methods per type instead of one for each of the forty-two
ordered pairs. `U8` to `U32` is two steps, which is more to type and never
wrong, and the pairs that deserve one step can be given one later.

`U64` is the only type here that holds numbers `Int` cannot, so it is the only
one whose *widening* conversion can fail — and the only one with a
`wrapping_to_int` for reading the bits instead.

The checked narrowing is one rule for all fourteen combinations rather than
fourteen bounds written by hand: narrow it, widen it back the way the target's
signedness says, and require the same number.

## Decimal, and why its absence is the loudest one

`docs/positioning.md` opens by saying Khora is "particularly well suited to
financial reconciliation and audit-heavy systems". There is no exact decimal
type in this document, in `std`, or anywhere in the tree. A reconciliation
engine that cannot represent ten pence is not a candidate for reconciling
anything, and `Float`'s own section above already explains why it is not the
answer: it is IEEE, and it implements neither `Eq` nor `Ord` precisely because
its equality is a trap.

**This is `std`'s, unambiguously**, and it is one of the few items on the
Phase 12 list that is. `ecosystem.md`'s rule asks what every package would
otherwise re-derive subtly wrong; two libraries with two incompatible `Decimal`
types cannot exchange a price, and a language where the money type comes from a
package is a language with several money types. It is also partly *language*
rather than library, because a literal needs a syntax.

Four decisions, and the first is the only one that must be made before anything
is written.

**What it represents.** A scaled integer — an `i128` significand and a decimal
exponent — is what financial systems actually use, and it is what
IEEE 754-2008's `decimal128`, SQL `NUMERIC`, Java's `BigDecimal` and .NET's
`decimal` all approximate. Arbitrary precision is the other option and it puts
an allocation behind every addition, which for a type that appears in every row
of a ledger is the wrong default. A fixed 128-bit significand gives 38 digits,
which is more than any currency needs and less than a general mathematics
library wants — and this is a money type, not a mathematics one.

**What overflow does.** The same as every other number in this document: it
traps. §"Overflow traps, in every build" argues that case and none of the
argument changes here.

**What division does.** It cannot be exact, so it must take a rounding mode and
a scale rather than pretending. Half-even is the default because it is what
accounting standards specify and what every other decimal type defaults to;
naming the alternatives explicitly is the point.

**Which operators it has — decided.** `+`, `-`, `*` and unary `-` on two
`Decimal`s are `Decimal::add`, `sub`, `mul` and `negate`: the checker records
the method at the operator as it records `Eq::eq` at a `==`, so an operator is
the method's call, with the method's answer, scale and trap. `/` is refused and
names `Decimal::divide`, because a quotient needs the scale and the rounding an
operator has nowhere to write. Mixing is refused too: `price + 1` names
`Decimal::of_int`, for the reason `1 + 2.0` is an error.

It is a special case, not an `Add` trait. A trait answers the same question for
every type at the cost of three others nobody had asked — whether `Int +
Decimal` is an impl, whether generic code may bound on `Add`, and what a
missing impl says on every record — and one library type did not justify
answering them. The special case can become a trait later without any program
changing meaning. `checked_add` and its siblings stay the typed refusal: an
operator traps; `checked_*` refuses.

**How a literal is written — decided.** `0.01` stays a `Float`, and a decimal
is `0.01d` or `Decimal::of(...)`.

The alternative was to make bare decimals `Decimal` and have `Float` be the
annotated one. It was rejected on the grounds `positioning.md` opens with:

> Khora is a general-purpose language whose design is particularly well suited
> to financial reconciliation and audit-heavy systems. **It is not a
> finance-specific language.**

A language where `0.1 + 0.2` is exact is a *finance* language, whatever its
documentation says. It would be the first thing anybody noticed and the last
thing they forgot, and it would tax every program that does geometry, physics,
statistics or graphics to spare the ones that do money. Finance is the domain
this language is *tested* against because it is the domain its authors know
well enough to judge the result — that is a reason to make `Decimal` excellent,
not a reason to make `Float` strange. An engineer arriving from Go or
TypeScript should find the numbers where they left them.

**`0.01d` is the language's only literal suffix** — built in 13.5 — and it is
worth being clear that this was a real addition rather than a convention
already in use.
`docs/grammar.ebnf` has `IntLiteral ::= [0-9] [0-9_]*` and
`FloatLiteral ::= IntLiteral "." IntLiteral ( ("e"|"E") ("+"|"-")? [0-9]+ )?`,
and no suffix anywhere; every other numeric type is *constructed*, as
`I32::of(0)` and `U64::wrapping(-1)`. So the grammar grows

```
DecimalLiteral ::= ( IntLiteral | FloatLiteral ) "d" ;
```

and `Literal` gains it. `1d` is a `Decimal` too, because whole amounts of money
are the common case and `1.00d` should not be mandatory. Three lexer notes for
whoever implements it: the suffix goes after any exponent, so `1.5e3d` is one
token; nothing may lex between the digits and the `d`, so `1 d` is not a
decimal; and if hexadecimal literals are ever added, the suffix must apply only
to decimal numerals, or `0x1d` becomes ambiguous with a decimal `0x1`.

**All three are tests.** The first two are in
`crates/khora-syntax/tests/decimal_literal.rs`; the third is
`a_d_in_a_hexadecimal_literal_is_a_digit` in `crates/khora-syntax/tests/bases.rs`,
where `0x1d` is the integer 29 and no `Decimal`.

Two things the implementation had to decide that this section did not.

**The scale is the number of digits written**, not the value's magnitude, so
`1.50d` is `scaled(150, 2)` and keeps its trailing zero. That is the same
reading `Show for Decimal` already takes: a price to two places stays a price
to two places, and dropping the zero is how a total stops lining up in a
column.

**An exponent moves the point rather than producing a negative scale.**
`Decimal::of_parts` refuses a negative scale — it is a large number spelled
confusingly — so `1.5e3d` is fifteen hundred at scale zero, not fifteen at
scale minus two.

It desugars to `Decimal::of_parts(high, low, scale)` during HIR lowering, so
nothing downstream has a case for it — two halves rather than one significand,
because the significand is a hundred and twenty-eight bits and `Int` is
sixty-four; `9_000_000_000_000_000_000_000.00d` is a legal price, and `Decimal` must be in scope exactly as `List`
must be for `[a, b]` — `desugar.rs`'s note explains why that is the rule rather
than a name the compiler knows and the program cannot see.

**The suffix and the constructor are not two spellings of one thing**, and that
is the argument for having the suffix at all. `0.01d` is parsed exactly by the
compiler and cannot fail. `Decimal::of("0.01")` parses at run time, costs
something at every evaluation, and must return a `Result` because the string
might not be a number. Without the suffix there would be no way to write an
exact decimal *constant*, which for this type is most of the point — and the
obvious workaround, going through a `Float`, throws away the exactness that is
the entire reason the type exists.

A total constructor is worth having beside the fallible one:
`Decimal::scaled(110, 2)` for a significand and an exponent, which cannot fail
and is what a decoder reading a database column or a wire format actually
wants. `Decimal::of_parts(high, low, 2)` is the same thing for a significand
too wide for an `Int`, which is what a 128-bit `NUMERIC` column is.

What is *not* in `std`: currency codes, exchange rates, allocation and rounding
policies for splitting a payment, and anything that knows what a fiscal quarter
is. Those are a package, by the same rule that keeps the type here.

### What was built

`std/decimal.kh`, and it is a hundred and twenty-eight bits of significand
after all — see §"Sixty-four bits, and why it had to be a hundred and
twenty-eight" below for what that cost and why it was not optional. `add`,
`sub`, `mul` and the rescaling are thin wrappers over runtime calls that trap
on overflow like every other number here. `Show` prints every place the scale
says, so `1.50` stays `1.50` — a price to two places is a price to two places,
and dropping the zero is how a total stops lining up in a column.

**Comparing does not line the scales up**, which took a correction. It used to,
which meant `==` on a hundred million and a rate to twelve places asked for a
multiplication by `10^12` and stopped the program — an equality that traps,
in the type sold on the fact that `Float`'s cannot be trusted. Order is a
question about two numbers rather than a third number that has to exist, so it
is answered in the runtime's wider arithmetic, where running out of room is
itself the answer: a value too large to scale is larger than one that fits.

**Every operation on the significand is in the runtime**, because the
significand is wider than any integer this language has. Khora holds it as two
`Int` fields and never does arithmetic on them; `khora_decimal_add` and the
rest take `hi, lo` and work in a Rust `i128`. Division needs more even than
that: reaching a scale means computing `left * 10^n / right`, and with a
128-bit significand that numerator wants two hundred and fifty-six bits, so the
runtime carries it as a pair of `u128`s with a widening multiply and a
shift-and-subtract division.

**And when none of them fits, it stops the program.** It used to saturate, on
a written-down argument that a total which quietly became a different total is
the failure the type exists to prevent — which is a description of saturating.
`Decimal::divide(10d, 1d, 18, HalfEven)` answered `9.223372036854775807`: in
range, printed to eighteen places, and balancing against itself. Somebody
writing a reconciler found it in the first ten minutes. §"Overflow traps, in
every build" above is the argument, and division was never outside it: `None`
is for a divisor that turned out to be zero, which is a thing data does, and
not for an answer that does not exist.

### Sixty-four bits, and why it had to be a hundred and twenty-eight

The significand shipped at sixty-four, against what this document asked for, on
an argument that reads well and is wrong: Khora has no 128-bit integer, adding
one to carry a money type would be a large language change for a small gain,
and eighteen significant digits is every currency amount anybody transacts.

**True about amounts and false about arithmetic on them.** `add` brings both
operands to the larger scale before adding, so

```khora
let notional = 100000000.00d;   // significand 10_000_000_000, scale 2
let rate     = 0.000000000001d; // scale 12
```

needs the notional at scale twelve — `1e10 x 10^10 = 1e20` against a ceiling of
`9.22e18`. It stopped the program. A hundred million against a twelve-decimal
rate is unremarkable finance, not an exotic case, and `mul` is worse because it
adds the scales, which is what makes it exact. An eighteen-decimal token
balance ran out at 9.2 whole units.

So the significand is a hundred and twenty-eight bits, which covers that case,
`NUMERIC` as real schemas declare it, and the token balance.

**The language did not change.** `IntKind` still stops at sixty-four and
`Type::Int` is still `i64`. A `Decimal` holds `{ hi, lo, scale }` — the
significand's two halves — and Khora never takes them apart. That is what makes
this a library change rather than a language one, and it is why the objection
this document recorded ("Khora has no 128-bit integer") does not apply to a
significand the runtime holds.

**A 128-bit answer comes back in two calls.** `docs/design/ffi.md` §1 allows
only scalars and pointers across the boundary, because how a sixteen-byte
aggregate returns is a decision LLVM makes for a struct type and rustc makes
for a `repr(C)` one, and on x86-64 Windows they disagree silently — errata 35,
which cost a day. So each operation returns the low half and leaves the high
one where `khora_decimal_high()` finds it, and every wrapper in `std::decimal`
takes it on the very next line. The same shape `khora_spawn_capture` and
`khora_spawn_take` already use.

**What it costs.** Three `Int` fields rather than two: twenty-four bytes,
unboxed, no allocation and no pointer chase. TypeScript's Effect `BigDecimal`
is a `BigInt` plus a number, which is at least two objects and a heap
allocation per arithmetic result. The point of the width was never to match an
arbitrary-precision library; it was to stop trapping on ordinary finance while
staying a value that fits in registers.

**And the limit is still a limit.** Thirty-eight digits, and going past stops
the program rather than wrapping or saturating. During the widening the runtime
briefly clamped a requested scale to thirty-eight, so `divide(x, y, 100,
HalfEven)` computed to thirty-eight places and labeled the answer as having a
hundred — a wrong number wearing the right hat, which is the exact failure this
type exists to prevent. It is a trap now, and there is a test.

## Not yet

- **`Float32`.** One float type is enough until something needs the other. That
  was going to be `std::ai`'s tensors, which were deleted unused, so the demand
  now has to come from something real or not at all.
- **Bytes back into a string.** `String::bytes` goes one way; the other way has
  to answer what happens to bytes that are not UTF-8, and the honest answer is
  a `Result` rather than a trap — bytes off a socket are data, not a
  programmer's mistake.
- **Checked division.** `/` and `%` still fault on zero rather than raising.
- **`checked_div` and friends.** See above: trapping is right for a bug and
  wrong for untrusted data, and an `Option`-returning form is what closes the
  gap without putting a row on every function that divides.
- **`U64` arithmetic above `Int`'s range goes through `U64`'s own methods.**
  A literal up to `0xFFFF_FFFF_FFFF_FFFF` is a `U64`, and it prints and hashes
  as itself; converting one to `Int` above `Int::MAX` still stops the program,
  and `U64::wrapping_to_int` reads the bits instead.
