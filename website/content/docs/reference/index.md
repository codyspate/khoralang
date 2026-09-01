---
title: Language reference
sidebar:
  order: 0
---

The Language Reference is the lookup-oriented description of Khora's syntax and semantic rules. The [Guide](/docs/guide/) teaches features in context; the Reference gives the accepted forms directly.

Every language construct should have concrete Khora code here. If you know the name of the construct, this section should be enough to answer “what does it look like?” without reading compiler source or internal design documents.

## Source and declarations

- [Lexical structure](./lexical-structure/) — identifiers, row variables, numeric/string literals, interpolation, comments, keywords, and punctuation.
- [Declarations](./declarations/) — `module`, `import`, `pub`, `const`, `type`, `derive`, `fn`, `extern fn`, `effect`, `context`, `trait`, `impl`, `test`, and `bench`.
- [Grammar and precedence](./grammar/) — compact grammar shapes, operator precedence, and parsing rules that disambiguate similar forms.

## Expressions and control flow

- [Expressions](./expressions/) — literals, calls, fields, records, tuples, lists, blocks, `let`, assignment, lambdas, `|>`, `||>`, postfix `!`, `catch`, handlers, and `with`.
- [Control flow](./control-flow/) — `if`, `match`, guards, `while`, `for`, `loop`, `break`, `continue`, `return`, and typed-failure exits.
- [Patterns](./patterns/) — wildcard, binding, literal, constructor, record, tuple, guarded, `let`, `for`, and `catch` patterns.

## Types and abstraction

- [Types](./types/) — path, unit, tuple, record, mutable-field, variant, function, row, union, generic, literal, opaque, and `forall` types.
- [Generics](./generics/) — type parameters, bounds, const generics, row variables, higher-kinded use, explicit `forall`, and variance.
- [Traits](./traits/) — trait declarations, supertraits, associated types, trait `impl`, inherent `impl`, bounds, and `derive(...)`.

## Effects, authority, and failure

- [Effects and rows](./effects/) — `effect` declarations and the relationship between `with` and `raises` rows.
- [Capabilities](./capabilities/) — handlers, capability rows, postfix/block `with`, named `context`, sequential composition, and overrides.
- [Failures](./failures/) — `raises`, `raise`, postfix `!`, pattern-based `catch`, failure translation, `attempt`, and collection of failures.

## Runtime-facing language rules

- [Memory and resources](./memory-and-resources/) — `Region`, `Scope`, `scoped`, `acquire`, finalizer order, and cleanup on structured exits.
- [Sharing](./sharing/) — `Share`, `Shared`, `Changed`, `Channel`, `SharedFn`, critical-section rules, and cross-fiber values.
- [Concurrency](./concurrency/) — `Fiber`, `Nursery`, `nursery`, `bounded_nursery`, cancellation, suspension, and structured ownership.
- [Traps](./traps/) — checked overflow, bounds failures, process-fatal behavior, backtraces, and exported-call containment.
- [FFI](./ffi/) — importing with `extern fn`, exporting with `pub extern fn`, `Ptr`, borrowed buffers, and the C ABI boundary.

## Working on a program

- [Lints](./lints/) — the twelve checks `khora check` runs, their default levels, and how to set them in `[lints]`.
- [Debugging a program](./debugging/) — backtraces, debug information, what a debugger can and cannot be relied on for.

## The release itself

- [Compatibility and stability](./compatibility/) — what `0.x` promises, what counts as a breaking change, and what 1.0 is waiting for.

## Exact library declarations

Language syntax tells you how to express a call or type. Concrete library signatures live in the [Standard Library API reference](/docs/stdlib/api/core/), generated from the declarations shipped with the toolchain.

For example, the Reference explains the syntax and semantic rules of a capability, shared value, or fiber; the stdlib reference gives the complete operations of `Random`, `Clock`, `Shared`, `Channel`, `Fiber`, `List`, HTTP types, and other shipped APIs.
