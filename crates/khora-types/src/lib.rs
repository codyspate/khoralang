//! Type inference.
//!
//! Not implemented yet. Planned shape:
//!
//! - Algorithm W over a Hindley-Milner core, extended with Leijen/Remy scoped
//!   row polymorphism for the `R` and `E` channels of `Effect<A, R, E>`.
//! - `Type::Constructor(Name, Vec<Type>)`, `Type::Row(Fields, TailVar)`,
//!   `Type::Var(Index)`, `Type::Const(Int)` for const generics.
//! - Row subtraction for `Effect.provide` / `Effect.provide_layer`, and the
//!   `R = {}` obligation that makes `Effect.run_native` legal.
//! - Exhaustiveness and reachability checking for `match`.
//!
//! Variance (`+A`, `-R`, `+E`) is already parsed and reaches this crate through
//! `khora_syntax::ast::TypeParam::variance`.
