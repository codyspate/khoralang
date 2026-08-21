# D2 — What `a::b` and `a.b` mean

**Status:** decided. Unblocks Phase 2.1 (name resolution). The separator split is
implemented in `khora-syntax` and recorded in `docs/errata.md` entry 13; the
resolution rules below are what `khora-hir` implements.

## The problem, and what is left of it

§1.1 gave one spelling to four different things:

> **Universal Dot Notation (`.`):** Consistent `.` symbol for namespaces, static
> enum constructors, record field access, and method invocations.

So these were syntactically identical, and the reference program uses all four:

```
std.core.Option              // a module path
report.risk                  // a field of a record value
RiskLevel.Critical("freeze") // a constructor of a variant type
Prompt.new()                 // an item associated with a type
```

That rule is gone. Khora separates **compile-time paths** from **runtime
projection**:

```
std::core::Option              // `::` — a module path
report.risk                    // `.`  — a field of a record value
RiskLevel::Critical("freeze")  // `::` — a constructor of a variant type
Prompt::new()                  // `::` — an item associated with a type
```

Most of what this document used to do — disambiguate four meanings of one
operator by resolving the leftmost name and seeing what turns up — is no longer
necessary. The reader can see which group a name is in before resolving
anything. What remains is one rule per group.

## `::` — compile-time paths

**A `::` path is resolved entirely in the module graph and the type namespace.
Locals do not participate.**

Given `a::b`, resolve `a` as:

1. **A module in scope** — then `b` is an item that module exports, which may
   itself be a module, so paths nest to any depth.
2. **A type in scope** — then `b` is:
   1. a **constructor**, if `b` names a case of that variant type;
   2. otherwise an **associated item** of that type.

A local binding named `a` is *not* a candidate. `x::foo` where `x` is a `let` or
a parameter is an error that says so, rather than a mysterious failure to find
`foo`. This is a real simplification over the universal dot, where a local named
`List` shadowed the type `List` and the resulting diagnostic had to explain
itself.

## `.` — runtime projection

**`x.b` is a field of `x`, or an item declared against `x`'s type. Nothing
else.**

Given `x.b` where `x` is a value of type `T`:

1. a **field**, if `T` has a field named `b`;
2. otherwise an **item declared against `T`**, invoked with `x` as the receiver —
   method-call syntax, as in Rust, Go and TypeScript.

There is no third case. `b` being an ordinary function in scope that happens to
take a `T` is not enough; see below.

Fields are looked up before items, but the order is unobservable: a type may not
have both a field and an item named `b`, and declaring the second is an error
(also below). Stating the order is still useful to the implementer, since it
makes the common case — a plain field load — cost one lookup.

This is what keeps capabilities working without a special rule.
`ledger.get_history(id)` is case 1: an effect is a record of operations
(`docs/design/effects.md`), `ledger` is the label bound by the `with` clause, and
`get_history` is a field of it. A capability operation is a field projection and
always was.

## No uniform function call syntax

An earlier draft of this document had a third case: if `b` was not a field, then
`x.b(y)` meant `b(x, y)` for any `b` in scope. That is dropped.

- **None of Go, Rust or TypeScript has UFCS.** (Rust uses the phrase for
  `T::f(x)`, the fully qualified form of a method that is already declared on
  `T`. A free `fn f(x: T)` is never reachable as `x.f()`.) It was the single most
  surprising thing in the proposal for the audience `docs/vision.md` names, and
  nothing was gained that `|>` does not already give. This is the familiarity
  tie-breaker in that document doing exactly what it is for: the options were
  close on the merits, so the familiar one wins.
- **It makes autocomplete useless.** Under UFCS, every function in scope whose
  first parameter is a `T` is a completion on every `T`-valued expression, so `.`
  narrows nothing — you get the module, not the type's surface. Decision A7 makes
  LSP quality a product requirement, and this would have undermined the single
  most-used feature in it.
- **Nothing needed it.** No call in the reference program resolves through the
  UFCS fallback.

The cost, recorded honestly: an associated item can no longer be declared for a
type you do not own by writing a free function. Extending a foreign type is a
typeclass instance (decision A4, Phase 3) — which is where Rust puts it too, and
which has coherence rules (D6) instead of whatever happens to be imported.

`|>` is unaffected and becomes the *only* way to chain free functions.
`xs |> normalise |> summarise` composes functions; `xs.len()` asks a value about
itself. Two operators, two jobs, no overlap between them.

## A field and an item with the same name is an error

If a type has a field `flush` and an item `flush` is declared against it, that is
a **compile error at the declaration**, not a lint and not a silent precedence
rule.

```
error: `flush` is already a field of `Buffer`
  --> src/buffer.kh:24  the item declared here
note: the field is declared at src/buffer.kh:9
```

Two things about this are deliberate.

**It fires at the declaration, not at each use.** The declaration is the cause;
every `buf.flush` in the program is a symptom. Reporting at the use sites would
scatter one mistake across a hundred diagnostics and put none of them where the
fix goes. One error, at the line that has to change.

**It cannot be a parse error.** At `x.flush` the parser has no idea what `x` is,
and it never will — that is the whole reason the universal dot was ambiguous in
the first place. The check needs the collected items of a type, so it belongs to
name resolution in `khora-hir`, alongside duplicate-field and duplicate-item
checks that have the same shape.

Go rejects the same collision outright, and that is what people expect. The
earlier draft made fields win silently and suggested a lint, which trades a clear
error at the declaration for a confusing one at some use site much later, or for
no error at all.

## Worked examples

| Expression | Resolved as |
| --- | --- |
| `std::core::Option` | module path, then a type it exports |
| `import std::core::{Option, Result};` | module path, then an import list |
| `report.risk` | field projection |
| `ledger.get_history(id)` | field projection, then call — a capability operation |
| `req.params.get("id")` | field, then a method on `Params` |
| `RiskLevel::Critical("x")` | constructor of a variant type |
| `Prompt::new()` | associated item on a type |
| `Scope::root` | associated *value* on a type |
| `List::map(xs, f)` | associated item, named by its path |
| `xs.map(f)` | the same item, called as a method |
| `xs::map` | error: `xs` is a local binding, not a module or a type |

The last two rows are the same item reached two ways, which is the Rust
arrangement. Whether the receiver is spelled `self` or is simply the first
parameter falls out of how items are declared, which is still open.

## Declaring associated items

The rules above need a way to attach items to a type. Two candidates:

**Companion namespace.** A `type T` implicitly opens a namespace `T`, and `pub
fn`s in the same module may be declared into it. Close to what Elm and Gleam do
with module-per-type, and needs no new construct beyond a way to say which type
an item belongs to.

**`impl` blocks.** `impl Prompt { pub fn new() -> Prompt { .. } }`. More familiar
to Rust users — which now counts for something explicit, per the tie-breaker in
`docs/vision.md` — and more obviously right once typeclasses land in Phase 3
(decision A4), since instance declarations will want a similar shape.

These are not exclusive, and the second is likely where we end up. The reason not
to decide now is that **Phase 2 does not need associated items at all** — see
below — so the choice can be made alongside typeclasses, when the constraints are
visible, rather than guessed at now.

## What Phase 2 actually needs

The vertical slice deliberately excludes records, generics and effects. That
leaves nothing on the `.` side at all, and two cases on the `::` side:

- **module paths** (rule 1)
- **variant constructors** (rule 2i)

Field projection arrives with records, and associated items with typeclasses. So
name resolution can implement `::` rules 1 and 2i now and report a clear "not yet
supported" for `.`, without the shape of either rule changing later.

That is the reason this document unblocks Phase 2.1 without settling everything.

## Consequences worth accepting deliberately

- **`x.b(y)` is method dispatch, not sugar for a call.** There is still one kind
  of function underneath, but the syntax commits: `.` reaches into a value or its
  type, and nothing else reaches into it.
- **A type's `.` surface is closed by its declaring module.** You cannot add to
  it from outside without a typeclass. That is the price of an autocomplete list
  that means something, and of one error instead of a precedence rule.
- **A name goes one way only: path first, projection after.**
  `std::net::http::Router::new().listen(8080)` is legal;
  `router.new::listen` is not, because once `.` has produced a value there is no
  compile-time namespace left to enter. That is a cheap rule to state and it
  makes every dotted name in the language readable left to right.

## Open sub-questions

- **A module and a type with the same name.** `Option` is plausibly both. `::`
  has to prefer one or report an ambiguity; Rust keeps separate namespaces and
  disambiguates by context, which is the likely answer but is not decided here.
- **Typeclass method resolution.** Once A4 lands, `.` case 2 and `::` case 2ii
  have to consider instances, not just items declared against the type.
  Coherence rules are D6.
- **`Schema::Spec` (D3).** `forall <Schema> . (Prompt, Schema::Spec) -> Schema`
  projects an associated *type* off a type *variable*. That is `::` case 2ii
  applied to something not yet known, and it is the hardest case in the language.
  It needs associated types on typeclasses, and it is the reason the `impl` shape
  above is probably the right one.
- **Glob imports and ambiguity.** If `import a::*;` and `import b::*;` both
  export `f`, referring to `f` should be an error naming both, not a silent pick.
