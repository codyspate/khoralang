# Polymorphic operations

> **An effect operation is rank-1. Polymorphism lives in a generic *function*
> over the effect, never in a field of the handler.**

The decision behind the last thing standing between the reference application
and a binary.

## What was written

`std::ai` declared inference as a capability, and asked one of its operations to
be polymorphic:

```khora
export effect LLMService {
  complete: Prompt -> String raises ModelError,
  extract: forall <A: Extract> . (Prompt, A::Spec) -> A raises ModelError,
  embed: forall <const Dim: Int> . String -> Embedding<Dim, F32> raises ModelError,
}
```

`extract` is the "give me a typed answer, not a blob of text" operation that
every LLM library has. The caller names a type, the type says how to describe
itself, and the answer comes back as that type:

```khora
let report = ai.extract(prompt, AnalysisReport::spec())!;
```

It is a good feature and the right thing to want. It does not compile, and it
was not going to.

## Why a field cannot be polymorphic

`ai` is a **value**: a record holding one closure per operation. `extract` is a
field of that record — one slot, in memory, at run time.

Khora compiles generics by monomorphization. `fn id<A>(x: A)` becomes `id$Int`,
`id$String`, one machine function per type actually used, chosen at each call
site because the call site knows the type. That is non-negotiable: it is how
the language avoids passing dictionaries, and dictionary-freedom is why a
capability costs a pointer rather than a vtable.

A field is one slot. "A function for every `A`" does not fit in it.

**Rust has the identical restriction**, which is worth saying because it means
this is not Khora falling short of the state of the art:

```rust
struct Model { extract: fn<A: Extract>(Prompt, A::Spec) -> A }   // not legal Rust
```

You cannot store a generic function in a struct field. Rust's answers are to
make the struct generic, or to erase the type behind `dyn` and pay for it.

## The three ways out

**Specialize the handler per type.** `LLMService` becomes a family — one
handler per `A` ever extracted, resolved whole-program. Preserves the syntax
exactly. Costs: an effect is no longer one type, a `with` block installs a set
whose size depends on the whole program, and a handler written by hand has to
enumerate the types its callers will ask for. The abstraction stops being local.

**Pass a dictionary for this one case.** `extract` takes a runtime description
of `A` and hands back something type-erased that the caller reinterprets.
Preserves the syntax. Costs the one property the whole design is built on, in
the one place where the reasoning for it ("a capability is a pointer, not a
vtable") is most load-bearing.

**Move the polymorphism out of the handler.** The operation becomes rank-1 and
the generic thing becomes an ordinary function over the effect. Costs one line
of the reference application.

## The decision

The third, and not grudgingly — it is the better design on its own terms.

A model does one thing: take a prompt, return text. That is what belongs in the
effect. Turning text into an `AnalysisReport` is *library code*, and library
code can be generic because a generic **function** monomorphizes normally.

```khora
export effect LLMService {
  complete: Prompt -> String raises ModelError,
}

export fn extract<A: Extract>(prompt: Prompt) -> A
  with { ai: LLMService }
  raises ModelError
{
  let text = ai.complete(Prompt::describing(prompt, A::spec()))!;
  A::parse(text)!
}
```

The call site loses an argument and keeps its meaning:

```khora
let report = extract(prompt)!;
```

Three things get better, and none of them is "it compiles":

- **A mock only has to fake `complete`** — one string. Before, `mock_ai` had to
  fabricate a whole `AnalysisReport` out of nothing, which tests the mock rather
  than the code under it.
- **The schema-and-parse logic becomes testable** with no model anywhere near
  it, because it is now a function rather than a hole in an interface.
- **The effect describes what an LLM is** rather than what one library wanted
  from it. `complete` is the whole of the capability; everything else is built
  on top and can be replaced without a new handler.

### `Extract` gains a `parse`

The trait only knew how to *ask*:

```khora
export trait Extract {
  type Spec;
  fn spec() -> Self::Spec;
}
```

Which was a gap regardless of any of this — something has to turn the model's
answer back into a value, and before now nothing said what. It is the same
type's business as the description, so it belongs on the same trait.

### `embed` gets the same treatment, and a correction

```khora
embed: forall <const Dim: Int> . String -> Embedding<Dim, F32> raises ModelError,
```

Same shape of problem, plus one the `forall` was hiding: **the dimension is the
model's, not the caller's.** A caller cannot choose to get 768 numbers from a
model that returns 1536. Written as a caller-chosen parameter, the type says
otherwise.

So the effect returns what the model actually produces — a vector whose length
is a run-time fact — and the shape-safe wrapper is a function that checks:

```khora
export fn embed<const Dim: Int>(text: String) -> Embedding<Dim, F32>
  with { ai: LLMService }
  raises ModelError
```

A caller who asks for the wrong `Dim` gets a `ModelError`, which is honest: the
mismatch is between the program and the model, and only one of those is
knowable at compile time.

## The rule, stated generally

**An effect operation is a function type, and a function type is rank-1.** If
an operation wants to be polymorphic, the polymorphism belongs one level out,
in a function that takes the effect as a capability.

This is a restriction on effects, not on generics, and it falls in a place with
a good reason behind it: an operation is a *value in a record*, and a value has
one type. Anything that wants many types wants a function, which is the thing
Khora already specializes.

It also has a pleasant consequence for library design. The pressure it applies
is towards effects that describe **what a resource is** rather than what its
callers happen to want, because only the first kind can be written down. That
is the pressure you want on an interface that mocks are written against.

## Not decided here

- **Whether the checker should say this.** Today `forall` in an operation
  produces a type nobody worked out, and the `Unknown` audit (errata 41) reports
  it as such — accurate, but it does not say *why* or what to do instead. A
  diagnostic naming the rule at the declaration would be better, and belongs
  with the parser change that would reject it outright.
- **Rank-2 anywhere else.** `forall` in an ordinary function's parameter type
  has the same underlying problem and no use case has asked for it yet. When one
  does, the answer is likely to be the same one.
