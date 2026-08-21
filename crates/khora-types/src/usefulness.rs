//! Exhaustiveness and reachability for `match`.
//!
//! This is Maranget's usefulness algorithm ("Warnings for pattern matching",
//! JFP 2007). A pattern row is *useful* against a matrix of earlier rows if
//! some value matches it and none of them. Both checks fall out of that one
//! question:
//!
//! - **Exhaustiveness**: the wildcard row `_` must *not* be useful against all
//!   the arms. If it is, the witness it produces is a value nothing matches —
//!   exactly the pattern to name in the diagnostic.
//! - **Reachability**: arm *i* must be useful against arms `0..i`. If it is
//!   not, nothing can reach it.
//!
//! The matrix built here is deliberately public. `docs/roadmap.md` 2.1 note and
//! `khora-hir`'s `body` module explain why: the decision tree compiled nearer
//! codegen is derived from this same matrix, so building a tree first would
//! mean reconstructing the matrix in order to check it.

use std::fmt;

/// What a pattern tests for. Payload arity is carried so specialisation knows
/// how many columns a constructor expands into.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ctor {
    /// A variant of an ADT, with its payload arity.
    Variant { name: String, arity: usize },
    Bool(bool),
    /// A literal of an effectively unbounded type. Such a column can never be
    /// complete, so a wildcard arm is always required.
    Literal(String),
    Tuple(usize),
}

impl Ctor {
    fn arity(&self) -> usize {
        match self {
            Ctor::Variant { arity, .. } => *arity,
            Ctor::Tuple(n) => *n,
            Ctor::Bool(_) | Ctor::Literal(_) => 0,
        }
    }

    fn matches(&self, other: &Ctor) -> bool {
        match (self, other) {
            (Ctor::Variant { name: a, .. }, Ctor::Variant { name: b, .. }) => a == b,
            _ => self == other,
        }
    }
}

/// A pattern, reduced to what exhaustiveness cares about. Bindings are
/// wildcards: `x` and `_` match the same values.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Pattern {
    Wildcard,
    Constructor { ctor: Ctor, fields: Vec<Pattern> },
}

impl fmt::Display for Pattern {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Pattern::Wildcard => write!(f, "_"),
            Pattern::Constructor { ctor, fields } => {
                match ctor {
                    Ctor::Variant { name, .. } => write!(f, "{name}")?,
                    Ctor::Bool(b) => return write!(f, "{b}"),
                    Ctor::Literal(l) => return write!(f, "{l}"),
                    Ctor::Tuple(_) => {}
                }
                if fields.is_empty() {
                    return Ok(());
                }
                let inner: Vec<String> = fields.iter().map(|p| p.to_string()).collect();
                write!(f, "({})", inner.join(", "))
            }
        }
    }
}

/// The constructors a column's type can take, which is what decides whether a
/// set of patterns covers everything.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ColumnType {
    /// A closed set: an ADT's variants, or `Bool`.
    Finite(Vec<Ctor>),
    /// `Int`, `String` — no finite set of literals covers it, so a wildcard is
    /// always needed.
    Unbounded,
    /// Type unknown, usually after an earlier error. Never report on it.
    Unknown,
}

/// The type of each column, in order.
type Types = [ColumnType];

/// Rows of patterns, all the same width.
pub type Matrix = Vec<Vec<Pattern>>;

/// Patterns not covered by `arms`, as witnesses to name in a diagnostic.
///
/// Empty means the match is exhaustive.
pub fn missing_patterns(arms: &[Pattern], scrutinee: &ColumnType) -> Vec<Pattern> {
    let matrix: Matrix = arms.iter().map(|p| vec![p.clone()]).collect();
    let types = vec![scrutinee.clone()];
    match usefulness(&matrix, &[Pattern::Wildcard], &types) {
        Usefulness::Useless => Vec::new(),
        Usefulness::Useful(witnesses) => {
            witnesses.into_iter().filter_map(|mut w| w.pop()).collect()
        }
    }
}

/// Indices of arms no value can reach, because earlier arms already cover them.
pub fn unreachable_arms(arms: &[Pattern], scrutinee: &ColumnType) -> Vec<usize> {
    let types = vec![scrutinee.clone()];
    let mut unreachable = Vec::new();
    for (i, arm) in arms.iter().enumerate() {
        let earlier: Matrix = arms[..i].iter().map(|p| vec![p.clone()]).collect();
        if matches!(usefulness(&earlier, std::slice::from_ref(arm), &types), Usefulness::Useless) {
            unreachable.push(i);
        }
    }
    unreachable
}

enum Usefulness {
    Useless,
    /// Values matching `q` that the matrix does not cover.
    Useful(Vec<Vec<Pattern>>),
}

fn usefulness(matrix: &Matrix, q: &[Pattern], types: &Types) -> Usefulness {
    // No columns left: `q` is useful only if nothing has matched so far.
    if q.is_empty() {
        return if matrix.is_empty() {
            Usefulness::Useful(vec![Vec::new()])
        } else {
            Usefulness::Useless
        };
    }

    let column = types.first().cloned().unwrap_or(ColumnType::Unknown);

    match &q[0] {
        Pattern::Constructor { ctor, fields } => {
            let specialised = specialise(matrix, ctor);
            let mut sub_q = fields.clone();
            sub_q.extend_from_slice(&q[1..]);
            let sub_types = specialised_types(&column, ctor, types);

            match usefulness(&specialised, &sub_q, &sub_types) {
                Usefulness::Useless => Usefulness::Useless,
                Usefulness::Useful(witnesses) => {
                    Usefulness::Useful(rebuild(witnesses, ctor))
                }
            }
        }
        Pattern::Wildcard => {
            let present = constructors_in_first_column(matrix);
            let complete = is_complete(&column, &present);

            if complete {
                // Every constructor is accounted for, so `q` is useful only if
                // it is useful under one of them.
                let all = match &column {
                    ColumnType::Finite(ctors) => ctors.clone(),
                    _ => present.clone(),
                };
                let mut witnesses = Vec::new();
                for ctor in &all {
                    let specialised = specialise(matrix, ctor);
                    let mut sub_q = vec![Pattern::Wildcard; ctor.arity()];
                    sub_q.extend_from_slice(&q[1..]);
                    let sub_types = specialised_types(&column, ctor, types);
                    if let Usefulness::Useful(found) =
                        usefulness(&specialised, &sub_q, &sub_types)
                    {
                        witnesses.extend(rebuild(found, ctor));
                    }
                }
                if witnesses.is_empty() {
                    Usefulness::Useless
                } else {
                    Usefulness::Useful(witnesses)
                }
            } else {
                // Some constructor is missing, so a wildcard reaches values no
                // row does. Recurse on the rows that also had a wildcard here.
                let defaulted = default_matrix(matrix);
                match usefulness(&defaulted, &q[1..], &types[1.min(types.len())..]) {
                    Usefulness::Useless => Usefulness::Useless,
                    Usefulness::Useful(witnesses) => {
                        let heads = missing_constructors(&column, &present);
                        let mut out = Vec::new();
                        for w in witnesses {
                            for head in &heads {
                                let mut row = vec![head.clone()];
                                row.extend(w.iter().cloned());
                                out.push(row);
                            }
                        }
                        Usefulness::Useful(out)
                    }
                }
            }
        }
    }
}

/// Rows whose first pattern matches `ctor`, with that pattern replaced by its
/// fields.
fn specialise(matrix: &Matrix, ctor: &Ctor) -> Matrix {
    let mut out = Matrix::new();
    for row in matrix {
        let Some(first) = row.first() else { continue };
        match first {
            Pattern::Constructor { ctor: row_ctor, fields } if row_ctor.matches(ctor) => {
                let mut new_row = fields.clone();
                new_row.extend_from_slice(&row[1..]);
                out.push(new_row);
            }
            Pattern::Wildcard => {
                let mut new_row = vec![Pattern::Wildcard; ctor.arity()];
                new_row.extend_from_slice(&row[1..]);
                out.push(new_row);
            }
            _ => {}
        }
    }
    out
}

/// Rows whose first pattern is a wildcard, with that column dropped.
fn default_matrix(matrix: &Matrix) -> Matrix {
    matrix
        .iter()
        .filter(|row| matches!(row.first(), Some(Pattern::Wildcard)))
        .map(|row| row[1..].to_vec())
        .collect()
}

fn constructors_in_first_column(matrix: &Matrix) -> Vec<Ctor> {
    let mut seen: Vec<Ctor> = Vec::new();
    for row in matrix {
        if let Some(Pattern::Constructor { ctor, .. }) = row.first() {
            if !seen.iter().any(|c| c.matches(ctor)) {
                seen.push(ctor.clone());
            }
        }
    }
    seen
}

fn is_complete(column: &ColumnType, present: &[Ctor]) -> bool {
    match column {
        ColumnType::Finite(all) => all.iter().all(|c| present.iter().any(|p| p.matches(c))),
        // A tuple has exactly one constructor, so seeing it once is complete.
        ColumnType::Unknown => !present.is_empty() && present.iter().all(|c| matches!(c, Ctor::Tuple(_))),
        ColumnType::Unbounded => false,
    }
}

fn missing_constructors(column: &ColumnType, present: &[Ctor]) -> Vec<Pattern> {
    match column {
        ColumnType::Finite(all) => {
            let missing: Vec<Pattern> = all
                .iter()
                .filter(|c| !present.iter().any(|p| p.matches(c)))
                .map(|c| Pattern::Constructor {
                    ctor: c.clone(),
                    fields: vec![Pattern::Wildcard; c.arity()],
                })
                .collect();
            if missing.is_empty() {
                vec![Pattern::Wildcard]
            } else {
                missing
            }
        }
        // Nothing useful to name for an unbounded type: `_` is the witness.
        _ => vec![Pattern::Wildcard],
    }
}

/// Column types after expanding `ctor`'s payload.
///
/// Payload types are not tracked in the phase 2 subset, so sub-columns are
/// `Unknown` and never produce a nested exhaustiveness complaint. Nested
/// patterns still specialise correctly; only the *witness* is less specific.
fn specialised_types(_column: &ColumnType, ctor: &Ctor, types: &Types) -> Vec<ColumnType> {
    let mut out = vec![ColumnType::Unknown; ctor.arity()];
    if types.len() > 1 {
        out.extend_from_slice(&types[1..]);
    }
    out
}

/// Folds a constructor's fields back out of a witness row.
fn rebuild(witnesses: Vec<Vec<Pattern>>, ctor: &Ctor) -> Vec<Vec<Pattern>> {
    let arity = ctor.arity();
    witnesses
        .into_iter()
        .map(|w| {
            let fields = w[..arity.min(w.len())].to_vec();
            let mut row = vec![Pattern::Constructor { ctor: ctor.clone(), fields }];
            row.extend(w[arity.min(w.len())..].iter().cloned());
            row
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn variant(name: &str, arity: usize) -> Ctor {
        Ctor::Variant { name: name.to_string(), arity }
    }

    fn pat(ctor: Ctor) -> Pattern {
        let arity = ctor.arity();
        Pattern::Constructor { ctor, fields: vec![Pattern::Wildcard; arity] }
    }

    fn three_variants() -> ColumnType {
        ColumnType::Finite(vec![variant("A", 0), variant("B", 1), variant("C", 0)])
    }

    #[test]
    fn all_variants_covered_is_exhaustive() {
        let arms = vec![pat(variant("A", 0)), pat(variant("B", 1)), pat(variant("C", 0))];
        assert!(missing_patterns(&arms, &three_variants()).is_empty());
    }

    #[test]
    fn a_missing_variant_is_named() {
        let arms = vec![pat(variant("A", 0)), pat(variant("C", 0))];
        let missing = missing_patterns(&arms, &three_variants());
        let names: Vec<String> = missing.iter().map(|p| p.to_string()).collect();
        assert_eq!(names, vec!["B(_)"], "the witness should name the uncovered variant");
    }

    #[test]
    fn several_missing_variants_are_all_named() {
        let arms = vec![pat(variant("A", 0))];
        let missing = missing_patterns(&arms, &three_variants());
        let mut names: Vec<String> = missing.iter().map(|p| p.to_string()).collect();
        names.sort();
        assert_eq!(names, vec!["B(_)", "C"]);
    }

    #[test]
    fn a_wildcard_covers_everything() {
        let arms = vec![Pattern::Wildcard];
        assert!(missing_patterns(&arms, &three_variants()).is_empty());
    }

    #[test]
    fn booleans_need_both_cases() {
        let bools = ColumnType::Finite(vec![Ctor::Bool(true), Ctor::Bool(false)]);
        let only_true = vec![pat(Ctor::Bool(true))];
        assert_eq!(
            missing_patterns(&only_true, &bools).iter().map(|p| p.to_string()).collect::<Vec<_>>(),
            vec!["false"]
        );

        let both = vec![pat(Ctor::Bool(true)), pat(Ctor::Bool(false))];
        assert!(missing_patterns(&both, &bools).is_empty());
    }

    /// No finite set of integer literals covers `Int`, so a wildcard is always
    /// required however many are listed.
    #[test]
    fn literals_never_exhaust_an_unbounded_type() {
        let arms = vec![pat(Ctor::Literal("1".into())), pat(Ctor::Literal("2".into()))];
        assert!(!missing_patterns(&arms, &ColumnType::Unbounded).is_empty());

        let with_wildcard = vec![pat(Ctor::Literal("1".into())), Pattern::Wildcard];
        assert!(missing_patterns(&with_wildcard, &ColumnType::Unbounded).is_empty());
    }

    #[test]
    fn an_arm_after_a_wildcard_is_unreachable() {
        let arms = vec![Pattern::Wildcard, pat(variant("A", 0))];
        assert_eq!(unreachable_arms(&arms, &three_variants()), vec![1]);
    }

    #[test]
    fn a_repeated_variant_is_unreachable() {
        let arms = vec![pat(variant("A", 0)), pat(variant("B", 1)), pat(variant("A", 0))];
        assert_eq!(unreachable_arms(&arms, &three_variants()), vec![2]);
    }

    #[test]
    fn distinct_arms_are_all_reachable() {
        let arms = vec![pat(variant("A", 0)), pat(variant("B", 1)), pat(variant("C", 0))];
        assert!(unreachable_arms(&arms, &three_variants()).is_empty());
    }

    /// A nested pattern must not be treated as covering its whole constructor.
    #[test]
    fn a_nested_pattern_does_not_cover_its_siblings() {
        let inner_a = Pattern::Constructor {
            ctor: variant("B", 1),
            fields: vec![pat(variant("A", 0))],
        };
        let arms = vec![pat(variant("A", 0)), inner_a, pat(variant("C", 0))];
        // `B(A)` leaves the rest of `B`'s payload uncovered.
        assert!(
            !missing_patterns(&arms, &three_variants()).is_empty(),
            "B(A) should not exhaust B"
        );
    }
}
