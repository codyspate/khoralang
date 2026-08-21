# Khora

A statically-typed, pure-functional systems language that compiles to native
static executables — no VM, no tracing GC. Memory is managed by Perceus
reference counting with static in-place reuse; effects, capabilities and typed
failure channels are tracked in the type system via row polymorphism.

This repository is the compiler, written in Rust.

## Status

The front end works. Everything after it is scaffolding.

| Crate | State |
| --- | --- |
| `khora-syntax` | **Working.** Lexer, lossless CST parser, typed AST, error recovery. |
| `khora-db` | **Working.** Salsa database, `SourceFile`/`SourceRoot` inputs, the `parse` query. |
| `khora-manifest` | **Working.** `khora.toml` parsing; unknown keys warn rather than abort. |
| `khora-fmt` | **Working.** Canonical formatter over the CST. |
| `khora-hir` | Not implemented. Boundary and lowering plan only. |
| `khora-types` | Not implemented. |
| `khora-perceus` | Not implemented. |
| `khora-codegen-llvm` | Not implemented. `llvm` feature off by default. |
| `khora-cli` | `check`, `fmt`, `lex`, `parse` work. `build` reports that it cannot. |

`khora check` parses the whole corpus in `std/` and `examples/` with no errors.

## Quickstart

```bash
cargo test
```

```bash
cargo run -p khora-cli -- check std examples
```

```bash
cargo run -p khora-cli -- fmt std examples --check
```

```bash
cargo run -p khora-cli -- parse examples/risk_analyzer/src/main.kh --no-trivia
```

## Layout

```
crates/
  khora-syntax/        logos lexer, rowan CST parser, typed AST
  khora-db/            salsa database, source inputs, the parse query
  khora-manifest/      khora.toml parsing
  khora-fmt/           the canonical formatter
  khora-hir/           AST -> HIR lowering, pipe and placeholder desugaring
  khora-types/         HM inference, row unification, const generics
  khora-perceus/       reference counting and in-place reuse
  khora-codegen-llvm/  inkwell backend, lld linking
  khora-cli/           the `khora` driver
docs/
  vision.md            what Khora is for; breaks ties in the roadmap
  roadmap.md           decisions, open questions, phases
  design/              decision records (effects, imperative, associated items)
  grammar.ebnf         the implemented grammar
  errata.md            where the specification is wrong, and what was done
std/                   standard library sources (.kh)
examples/              the reference application
```

## Front-end design notes

**The parser never fails.** It always returns a tree spanning the entire input
plus a list of diagnostics. Whitespace and comments are tokens in that tree, so
`parse(src).syntax().text() == src` holds for any input — including binary
garbage. This is a hard requirement for the language server and is enforced by
tests.

**Events, not direct tree building.** The parser emits a flat event stream that
is replayed into a `rowan` green tree afterwards. That indirection is what makes
`CompletedMarker::precede` possible: a finished node can be given a new parent
retroactively, which is how left-associative operators are parsed without
backtracking.

**Two syntactic ambiguities the specification leaves open** are resolved as
follows, and both are called out in `docs/errata.md`:

- `{` opens a record literal when followed by `}` or `Ident :`, and a block
  otherwise. In a `match` scrutinee it always opens the arm list.
- `a.b.c` in expression position stays an unresolved `FIELD_EXPR` chain. Under
  the "universal dot" rule a module path, an enum constructor and a record
  projection are spelled identically, so the parser refuses to guess and leaves
  it to name resolution.

Operator precedence, loosest to tightest: `|>`, `||`, `&&`, comparisons,
`+ -`, `* / %`, prefix `- !`, then call and field access.

## Next step

`khora-hir` name resolution. It blocks the type checker, and item 10 of
`docs/errata.md` explains why it cannot be deferred: until `Type.member` has a
defined meaning, half the reference program has no denotation.
