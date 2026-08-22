# Numbers

Phase 6.2 and 6.3. Two decisions here were made without being asked about, and
both are flagged as such: overflow was decided in advance, `Float`'s traits were
not.

## Overflow traps, in every build

`+`, `-` and `*` on `Int` stop the program when the result does not fit. Not
"in debug", not "unless optimized" — always.

Rust's split (panic in debug, wrap in release) is the one this audience knows,
and it is the one rejected. A program that passes its tests and then wraps in
production is the failure worth spending a branch on, and two behaviours put
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

`/` and `%` are **not yet checked**, and that is a gap rather than a decision.
Division by zero faults, and `Int::MIN / -1` overflows. Both want the error
channel rather than a hardware fault, and `raises` exists now, so the shape of
the answer is clear and only the work is missing.

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

## Not yet

- **`Float32`.** One float type is enough until something needs the other, and
  `std::ai`'s tensors are where it will come from.
- **Bytes back into a string.** `String::bytes` goes one way; the other way has
  to answer what happens to bytes that are not UTF-8, and the honest answer is
  a `Result` rather than a trap — bytes off a socket are data, not a
  programmer's mistake.
- **Checked division.** `/` and `%` still fault on zero rather than raising.
- **Negation does not trap.** `-x` where `x` is the type's minimum wraps to
  itself. It is the one value that cannot be negated, and the only way to
  *write* it is as a negated literal, which is folded into the constant before
  code generation sees it — so the gap is narrow, and it is still a gap.
- **`U64` arithmetic above `Int`'s range is awkward to write**, because a
  literal that large cannot be spelled: it does not fit the `i128` the range
  check parses into. `U64::wrapping(-1)` is the way to say "all ones" today.
