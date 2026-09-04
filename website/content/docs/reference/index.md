---
title: Language reference
sidebar:
  order: 0
---

Every Khora construct, one page per topic. If you know the name of the thing
you are looking for, this section should answer "what does it look like, and
what are the rules?" without reading compiler source or a design document.

Every construct has concrete Khora code here — a construct should never exist
only in prose or only in the parser.

## Reading it the first time

The pages are lookup-oriented, but they are ordered so that a straight read
works. Each opens with what the construct is for before it lists the shapes it
accepts.

**The syntax of ordinary programs**

1. [Expressions](./expressions/) — literals, calls, records, blocks, `let`,
   lambdas, `|>` and `||>`, postfix `!`.
2. [Control flow](./control-flow/) — `if`, `match`, `while`, `for`, `loop`,
   `break`, `return`.
3. [Types](./types/) — records, variants, tuples, wrappers, function types.
4. [Patterns](./patterns/) — matching and destructuring, everywhere they appear.
5. [Generics](./generics/) and [Traits](./traits/) — parameters, bounds,
   associated types, row variables.

**What makes Khora different**

6. [Failures](./failures/) — `raises`, `raise`, `!`, `catch`, `attempt`. What
   may go wrong is in the type.
7. [Effects and rows](./effects/) and [Capabilities](./capabilities/) —
   `effect`, `handler`, `with`, `context`. What authority a function needs is in
   the type too, and the two rows are independent.

**Lifetimes and concurrent work**

8. [Memory and resources](./memory-and-resources/) — regions, `scoped`,
   `acquire`, cleanup on every exit.
9. [Concurrency](./concurrency/) — fibers, nurseries, cancellation.
10. [Sharing](./sharing/) — `Shared`, `Channel`, `SharedFn`, and what may cross
    a fiber boundary.

If you have not built a Khora program yet, start with [Getting
Started](/docs/getting-started/) and come back once the toolchain workflow
works. Arriving from TypeScript + Effect, Go or Rust, the [migration
pages](/docs/migration/) map familiar concepts onto these.

## Source and declarations

- [Lexical structure](./lexical-structure/) — identifiers, row variables, numeric/string literals, interpolation, comments, keywords, and punctuation.
- [Declarations](./declarations/) — `module`, `import`, `pub`, `const`, `type`, `derive`, `fn`, `extern fn`, `effect`, `context`, `trait`, `impl`, `test`, and `bench`.
- [Grammar and precedence](./grammar/) — compact grammar shapes, operator precedence, and parsing rules that disambiguate similar forms.

## Expressions and control flow

- [Expressions](./expressions/) — literals, calls, fields, records, record update, tuples, lists, blocks, `let`, assignment, lambdas, `|>`, `||>`, postfix `!`, `catch`, handlers, and `with`.
- [Control flow](./control-flow/) — `if`, `match`, guards, `while`, `for`, `loop`, `break`, `continue`, `return`, and typed-failure exits.
- [Patterns](./patterns/) — wildcard, binding, literal, constructor, record, tuple, guarded, `let`, `for`, and `catch` patterns.

## Types and abstraction

- [Types](./types/) — path, unit, tuple, record, mutable-field, variant, function, row, generic, literal, opaque, wrapper, and `forall` types.
- [Generics](./generics/) — type parameters, bounds, const generics, row variables, higher-kinded use, explicit `forall`, and variance.
- [Traits](./traits/) — trait declarations, supertraits, associated types, trait `impl`, inherent `impl`, bounds, and `derive(...)`.

## Effects, authority, and failure

- [Effects and rows](./effects/) — `effect` declarations and the relationship between `with` and `raises` rows.
- [Capabilities](./capabilities/) — handlers, capability rows, postfix/block `with`, named `context`, sequential composition, and overrides.
- [Failures](./failures/) — `raises`, `raise`, postfix `!`, pattern-based `catch`, failure translation, `attempt`, and collecting every reason rather than the first.

## Runtime-facing language rules

- [Memory and resources](./memory-and-resources/) — `Region`, `Scope`, `scoped`, `acquire`, finalizer order, and cleanup on structured exits.
- [Sharing](./sharing/) — `Share`, `Shared`, `Changed`, `Channel`, `SharedFn`, critical-section rules, and cross-fiber values.
- [Concurrency](./concurrency/) — `Fiber`, `Nursery`, `nursery`, `bounded_nursery`, cancellation, suspension, and structured ownership.
- [Traps](./traps/) — checked overflow, bounds failures, process-fatal behavior, backtraces, and exported-call containment.
- [FFI](./ffi/) — importing with `extern fn`, exporting with `pub extern fn`, `Ptr`, borrowed buffers, and the C ABI boundary.

## Working on a program

- [Modules and packages](./modules-and-packages/) — `src/bin`, dependencies, the lockfile, publishing, and package boundaries.
- [The manifest](./manifest/) — every table in `khora.toml`: the required `[toolchain]` pin, `[package]`, workspaces, permissions, dependencies, lints, and tasks.
- [Testing and benchmarks](./testing/) — `khora test` and `khora bench`, supplying capabilities to a test, build profiles, and CI.
- [Lints](./lints/) — the twelve checks `khora check` runs, their default levels, and how to set them in `[lints]`.
- [Debugging a program](./debugging/) — backtraces, debug information, what a debugger can and cannot be relied on for.

## The release itself

- [Compatibility and stability](./compatibility/) — what `0.x` promises, what counts as a breaking change, and what 1.0 is waiting for.

## Where the other sections start

The Reference is the language. Two other sections answer different questions
about the same program:

- The [Standard Library](/docs/stdlib/) is what ships with the toolchain —
  prose for the modules that need it, and generated pages carrying every
  exported declaration. The Reference gives the syntax of a capability or a
  fiber; the library reference gives the complete operations of `Random`,
  `Clock`, `Shared`, `Channel`, `List`, the HTTP types and the rest.
- The [Cookbook](/docs/cookbook/) is a whole task, working, end to end —
  bounded concurrency, a database transaction, decoding untrusted input.
