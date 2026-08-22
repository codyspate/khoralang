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

## Not yet

- **The fixed-width types themselves.** `Int` is the only integer there is, so
  there are still no bytes — and `Array<U8>` is what a string index and every
  wire format need. This is the largest remaining piece of phase 6.
- **`Float32`.** One float type is enough until something needs the other, and
  `std::ai`'s tensors are where it will come from.
- **Literal inference.** An integer literal is `Int` and a decimal literal is
  `Float`, full stop. `let x: U8 = 5` will want a literal that takes its type
  from context, which is a defaulting problem of the same shape as the deferred
  projections in D3 — and solvable the same way.
