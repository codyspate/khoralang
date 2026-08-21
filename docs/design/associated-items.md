# D2 — What `a.b` means

**Status:** proposed. Blocks Phase 2.1 (name resolution). Wants a sign-off
before implementation, because it fixes the shape of every dotted name in the
language.

## The problem

§1.1 gives one spelling to four different things:

> **Universal Dot Notation (`.`):** Consistent `.` symbol for namespaces, static
> enum constructors, record field access, and method invocations.

So these are syntactically identical, and the reference program uses all four:

```
std.core.Option              // a module path
report.risk                  // a field of a record value
RiskLevel.Critical("freeze") // a constructor of a variant type
Prompt.new()                 // an item associated with a type
```

The parser deliberately refuses to guess: `a.b.c` in expression position is a
`FIELD_EXPR` chain and nothing more (errata #10). Something has to decide, and
it has to be a rule a person can hold in their head.

## The rule

**Resolve the leftmost name first; what it is decides what the dot means.**

Given `a.b`, resolve `a` in the current scope, in this order:

1. **A local binding** — a `let`, a parameter, or a capability label bound by a
   `with` clause. Then `a` is a value, and `b` is:
   1. a **field**, if `a`'s type is a record with a field named `b`;
   2. otherwise an **associated item** of `a`'s type, called with `a` as its
      first argument — so `x.f(y)` means `f(x, y)`.
2. **A module in scope** — then `b` is an item that module exports.
3. **A type in scope** — then `b` is:
   1. a **constructor**, if `b` names a case of that variant type;
   2. otherwise an **associated item** of that type.

Locals shadow modules and types, as they do everywhere else. Fields are checked
before associated items so that a capability keeps working: `ledger.get_history`
*must* be the field holding the operation, not a method resolved from scope.

That single ordering resolves every case in the reference program:

| Expression | Resolved as |
| --- | --- |
| `std.core.Option` | module path (2) |
| `report.risk` | field projection (1i) |
| `ledger.get_history(id)` | field projection, then call (1i) — capability operation |
| `req.params.get("id")` | field, then associated item on `Params` (1i then 1ii) |
| `RiskLevel.Critical("x")` | constructor (3i) |
| `Prompt.new()` | associated item on a type (3ii) |
| `Scope.root` | associated *value* on a type (3ii) |

## Declaring associated items

Rule 3ii needs a way to attach items to a type. Two candidates:

**Companion namespace (recommended for now).** A `type T` implicitly opens a
namespace `T`, and `pub fn`s in the same module may be declared into it. This is
close to what Elm and Gleam do with module-per-type, and needs no new construct
beyond a way to say which type an item belongs to.

**`impl` blocks.** `impl Prompt { pub fn new() -> Prompt { .. } }`. More
familiar to Rust users and more obviously right once typeclasses land in Phase 3
(decision A4), since instance declarations will want a similar shape.

These are not exclusive, and the second is likely where we end up. The reason to
not decide now is that **Phase 2 does not need associated items at all** — see
below — so the choice can be made alongside typeclasses, when the constraints
are visible, rather than guessed at now.

## What Phase 2 actually needs

The vertical slice deliberately excludes records, generics and effects. That
leaves only two of the four cases:

- **module paths** (rule 2)
- **variant constructors** (rule 3i)

Field projection arrives with records, and associated items with typeclasses.
So name resolution can implement rules 2 and 3i now, and report a clear
"not yet supported" for the rest, without the shape of the rule changing later.

That is the reason this document unblocks Phase 2.1 without settling everything.

## Consequences worth accepting deliberately

- **`x.f(y)` is uniform function call syntax**, not a distinct method dispatch
  mechanism. There is one kind of function. This keeps `|>` and `.` from being
  two competing ways to chain, and it means an associated item can be defined
  for a type you do not own.
- **Adding a field can shadow an associated item**, because fields are checked
  first. This is the price of capabilities resolving correctly, and it should be
  a lint rather than an error.
- **A local named `List` shadows the type `List`.** Normal, but the diagnostic
  when it happens must say so plainly, since the failure will look mystifying.

## Open sub-questions

- **Typeclass method resolution.** Once A4 lands, rule 1ii and 3ii have to
  consider instances, not just items in scope. Coherence rules are D6.
- **`Schema.Spec` (D3).** `forall <Schema> . (Prompt, Schema.Spec) -> Schema`
  projects an associated *type* off a type *variable*. That is rule 3ii applied
  to something not yet known, and is the hardest case in the language. It needs
  associated types on typeclasses, and it is the reason the `impl` shape above
  is probably the right one.
- **Glob imports and ambiguity.** If `import a.*` and `import b.*` both export
  `f`, referring to `f` should be an error naming both, not a silent pick.
