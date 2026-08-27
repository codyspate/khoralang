---
title: Language reference
sidebar:
  order: 0
---

The Language Reference is the lookup-oriented description of Khora's syntax and semantic rules. The [Guide](/docs/guide/) teaches features in context; the Reference gives the accepted forms directly.

Every language construct should have concrete Khora code here. If you know the name of the construct, this section should be enough to answer “what does it look like?” without reading compiler source or internal design documents.

## Source and declarations

- [Lexical structure](./lexical-structure.md) — identifiers, row variables, numeric/string literals, interpolation, comments, keywords, and punctuation.
- [Declarations](./declarations.md) — `module`, `import`, `pub`, `const`, `type`, `derive`, `fn`, `extern fn`, `effect`, `context`, `trait`, `impl`, `test`, and `bench`.
- [Grammar and precedence](./grammar.md) — compact grammar shapes, operator precedence, and parsing rules that disambiguate similar forms.

## Expressions and control flow

- [Expressions](./expressions.md) — literals, calls, fields, records, tuples, lists, blocks, `let`, assignment, lambdas, `|>`, `||>`, postfix `!`, `catch`, handlers, and `with`.
- [Control flow](./control-flow.md) — `if`, `match`, guards, `while`, `for`, `loop`, `break`, `continue`, `return`, and typed-failure exits.
- [Patterns](./patterns.md) — wildcard, binding, literal, constructor, record, tuple, guarded, `let`, `for`, and `catch` patterns.

## Types and abstraction

- [Types](./types.md) — path, unit, tuple, record, mutable-field, variant, function, row, union, generic, literal, opaque, and `forall` types.
- [Generics](./generics.md) — type parameters, bounds, const generics, row variables, higher-kinded use, explicit `forall`, and variance.
- [Traits](./traits.md) — trait declarations, supertraits, associated types, trait `impl`, inherent `impl`, bounds, and `derive(...)`.

## Effects, authority, and failure

- [Effects and rows](./effects.md) — `effect` declarations and the relationship between `with` and `raises` rows.
- [Capabilities](./capabilities.md) — handlers, capability rows, postfix/block `with`, named `context`, sequential composition, and overrides.
- [Failures](./failures.md) — `raises`, `raise`, postfix `!`, pattern-based `catch`, failure translation, `attempt`, and collection of failures.

## Runtime-facing language rules

- [Memory and resources](./memory-and-resources.md) — automatic memory management, regions, finalization, and resource boundaries.
- [Concurrency](./concurrency.md) — fibers, structured ownership, cancellation, and sharing rules.
- [FFI](./ffi.md) — `extern fn`, native library exports, pointers, and C-compatible boundaries.
- [Traps](./traps.md) — invariant failures that are intentionally separate from typed `raises`.

## Exact library declarations

Language syntax tells you how to express a call or type. Concrete library signatures live in the [Standard Library API reference](/docs/stdlib/api/core/), generated from the declarations shipped with the toolchain.

For example, the Reference explains the syntax of a capability and a handler; the stdlib reference tells you the actual operations of `Random`, `Clock`, `Shared`, `List`, HTTP types, and other shipped APIs.