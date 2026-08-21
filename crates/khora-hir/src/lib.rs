//! AST to HIR lowering.
//!
//! Nothing here is implemented yet. The crate exists to fix the boundary:
//! everything downstream consumes HIR and never touches `khora-syntax`, so the
//! CST stays free to keep whitespace, comments and broken input for the LSP.
//!
//! Planned responsibilities, in order:
//!
//! 1. **Name resolution.** Decide what each link of an `a.b.c` chain means.
//!    The parser deliberately leaves `Effect.map`, `report.risk` and
//!    `RiskLevel.Low` as identical `FIELD_EXPR` chains, because the "universal
//!    dot" rule makes them syntactically indistinguishable. See item 10 of
//!    `docs/errata.md` — this is the first thing that has to be settled.
//! 2. **Pipe desugaring.** `x |> f(a)` becomes `f(x, a)`; `x |> f(_, a)`
//!    becomes `f(x, a)` with the placeholder consuming the piped value instead
//!    of the leading position. A pipeline stage with more than one `_` is an
//!    error.
//! 3. **Capability lowering.** `:label.member` becomes a projection out of the
//!    requirement row, which is what gives `Effect.provide` something to
//!    subtract.
//! 4. **Match compilation.** Lower arms to a decision tree, which is also what
//!    the exhaustiveness check in `khora-types` consumes.
