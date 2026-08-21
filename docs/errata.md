# Specification errata

Findings from implementing the front end against the language specification.
Each entry states what the spec says, why it cannot be implemented as written,
and what `crates/khora-syntax` does instead.

## 1. Generic argument order in `std.effect` and `std.ai` is corrupted

§2.1 defines the core type as `Effect<+A, -R, +E>` — value, capability row,
error channel. The listings in §3 do not follow it. Several signatures have
their type arguments shuffled and their delimiters displaced:

| As published | Intended |
| --- | --- |
| `Effect<A, Never {},>` | `Effect<A, {}, Never>` |
| `Effect<Never, E {},>` | `Effect<Never, {}, E>` |
| `Effect<T, 'r Never T label: { \| },>` | `Effect<T, { label: T \| 'r }, Never>` |
| `Effect<B, + E1 E2 R1 R2 { \| } },>` | `Effect<B, { R1 \| R2 }, E1 + E2>` |
| `Layer<R1, E2 R2,>` | `Layer<R1, R2, E2>` |
| `Tensor<D: Device, Scalar Shape: Tuple, Type:>` | `Tensor<D: Device, Shape: Tuple, T: Scalar>` |
| `matmul<D: Device, Int, K: M: N: Scalar T: const>` | `matmul<D: Device, const M: Int, const K: Int, const N: Int, T: Scalar>` |

The corruption looks mechanical — tokens rotated within each parameter list —
and one signature survived intact: `embed: ... -> Effect<Embedding<Dim, F32>, {}, ModelError>`
in `LLMService`. That surviving line agrees with §2.1, so `A, R, E` is taken as
authoritative and every signature in `std/` is written in that order.

`std/effect.kh` also adds `map_error`, which §4.2 calls three times but §3.1
never declares.

## 2. `->` is used by the grammar the lexical rules forbid

§1.1 says "No `::` or `->` symbol clutter", but `FunctionType` in §1.2 and every
function signature in §3 use `->`. Implemented with `->`; the prohibition is
read as applying to path separators only.

## 3. Capability references (`:label.member`) are undeclared syntax

§4.2 writes `ask(:ledger.get_history)` and `ask(:ai.extract(_, AnalysisReport.spec))`.
No production in §1.2 introduces a leading `:`. It is parsed as
`CapabilityExpr ::= ":" Path`, producing a `CAPABILITY_EXPR` node.

There is a second, deeper problem here that the parser cannot settle. Under the
pipe rule in §1.1, `x |> ask(:ledger.get_history)` desugars to
`ask(x, :ledger.get_history)`, but §3.1 declares `ask` as taking a single
`Label`. Either `ask` is variadic in a way the signature does not show, or the
intended spelling is `x |> :ledger.get_history`. The reference program is
transcribed verbatim, so this is a *type* error waiting for `khora-types`, not a
syntax error.

## 4. `LayerDecl` is referenced but never defined

`TopLevelDecl ::= TypeDecl | FunctionDecl | LayerDecl | LetDecl` in §1.2, yet no
`LayerDecl` production exists, and §4.2 declares layers as ordinary `let`
bindings with a `Layer<...>` annotation. `LayerDecl` is dropped; layers are
`LetDecl`s.

## 5. Opaque type and signature-only declarations have no production

§1.2 requires `TypeDecl` to have `= TypeDef` and `FunctionDecl` to have
`= BlockExpr`. §3 relies on neither: `pub type Effect<+A, -R, +E>;` declares an
abstract type, and `pub fn succeed<A>(value: A) -> Effect<A, {}, Never>;`
declares a signature with no body. Both right-hand sides are optional in the
implemented grammar.

## 6. Misplaced parenthesis in `VariantType`

Published: `VariantType ::= ( "|" Ident ( "(" RecordFields | TupleFields ")" )? )+`

The alternation straddles the parentheses, so `"(" RecordFields` and
`TupleFields ")"` are the two branches. Corrected to
`"(" ( RecordFields | TupleFields ) ")"`.

## 7. `PlaceholderExpr` is used but never defined

`PipeExpr` refers to `PlaceholderExpr`; no production defines it. Implemented as
a bare `_` in any argument position, yielding `PLACEHOLDER_EXPR`. Binding it to
the piped value is a lowering concern (`khora-hir`), not a syntactic one.

## 8. Row merge and error union need surface syntax

§2.2 specifies `R_combined = R1 ∪ R2` and `E_combined = E1 ∪ E2` but gives no
notation. The residue in the corrupted signatures (`{ | }` and a stray `+`)
suggests two different spellings were intended, and that is what is implemented:
`{ R1 | R2 }` for row merge, `E1 + E2` for error union.

## 9. `{` is ambiguous between a record literal and a block

Both `RecordInit` and `BlockExpr` are `PrimaryExpr` alternatives, so
`match x { ... }` and `f({ a: 1 })` cannot both parse under an LL(1) reading.
Resolved with two rules:

- `{` opens a record literal when it is immediately followed by `}` or by
  `Ident :`; otherwise it opens a block.
- Inside a `match` scrutinee, `{` always opens the arm list. Wrap the scrutinee
  in parentheses to pass a record literal.

## 10. Member functions used by the reference program are never declared

§4.2 calls `Prompt.new`, `Prompt.system`, `Prompt.user`, `Layer.succeed`,
`Layer.merge`, `Tensor.zeros`, `Response.json`, `Router.new`, `Router.post`,
`Router.listen`, `AnalysisReport.spec` and `params.get`. None appear in §3, and
the spec never says whether `Type.member` denotes an associated item, a module
function, or a record projection — even though §1.1 gives all three the same
`.` spelling.

The parser therefore does not commit: `a.b.c` in expression position is a
`FIELD_EXPR` chain, and name resolution in `khora-hir` decides what each link
means. Deciding that is a prerequisite for the type checker.

`std/net/http.kh` is not specified at all; the signatures there are reconstructed
from usage and should be treated as provisional.

## 11. `Never` and `Label` are used but never declared

`std.effect` refers to both without defining them; `Float`, `Int`, `String` and
`List` are likewise assumed. They are declared as opaque types in `std/effect.kh`
so the corpus is self-contained; the real definitions belong in a `std.prelude`.
