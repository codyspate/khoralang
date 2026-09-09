//! Patterns: what they bind, and whether they cover everything.
//!
//! Binding walks the pattern against the scrutinee's type and records what each
//! name got. Coverage is `usefulness`, which wants patterns in its own form —
//! `to_pattern` is the translation, and the reason exhaustiveness and
//! reachability come out of one algorithm.

use super::*;

impl<'a> Checker<'a> {
    /// Records the type of every binding a pattern introduces.
    pub(super) fn bind_pattern(&mut self, pat: PatId, ty: &Type) {
        match self.body.pat(pat).clone() {
            Pat::Bind(local) => {
                self.locals.insert(local, ty.clone());
            }
            Pat::TupleStruct { resolution, fields } => {
                let variant = variant_case(&resolution)
                    .and_then(|(h, t, n)| self.types.variant_of(h.as_ref(), &t, &n))
                    .cloned();
                // Field types are declared against the type's own parameters,
                // so they have to be read at the scrutinee's instantiation:
                // matching `Option<Int>` binds `v` to `Int`, not to `A`.
                let mapping = variant
                    .as_ref()
                    .map(|v| self.substitution_for(&v.type_name, ty))
                    .unwrap_or_default();
                let borrowed: HashMap<&str, Type> =
                    mapping.iter().map(|(k, v)| (k.as_str(), v.clone())).collect();

                for (i, field) in fields.iter().enumerate() {
                    let declared = variant
                        .as_ref()
                        .and_then(|v| v.fields.get(i).cloned())
                        .unwrap_or(Type::Unknown);
                    let field_ty = unify::substitute(&declared, &borrowed);
                    self.bind_pattern(*field, &field_ty);
                }
            }
            // **Named, so a field may be left out and the order means
            // nothing.** `VariantInfo::labels` said matching "never needed
            // these"; a record pattern is the thing that does, and the index
            // it finds there is what says which declared type the binding got.
            Pat::Record { resolution, fields } => {
                let variant = variant_case(&resolution)
                    .and_then(|(h, t, n)| self.types.variant_of(h.as_ref(), &t, &n))
                    .cloned();
                let mapping = variant
                    .as_ref()
                    .map(|v| self.substitution_for(&v.type_name, ty))
                    .unwrap_or_default();
                let borrowed: HashMap<&str, Type> =
                    mapping.iter().map(|(k, v)| (k.as_str(), v.clone())).collect();

                for (label, field) in fields.iter() {
                    let declared = variant.as_ref().and_then(|v| {
                        v.labels.iter().position(|l| l == label).and_then(|i| v.fields.get(i))
                    });
                    let field_ty = match declared {
                        Some(declared) => unify::substitute(declared, &borrowed),
                        None => {
                            if let Some(v) = variant.as_ref() {
                                self.error(
                                    format!("`{}` has no field `{label}`", v.name),
                                    self.body.pat_range(*field),
                                );
                            }
                            Type::Unknown
                        }
                    };
                    self.bind_pattern(*field, &field_ty);
                }
            }
            Pat::Tuple(fields) => {
                // **A tuple pattern against something that is not a tuple is
                // an error here**, and used to be an error nowhere.
                //
                //     let (a, b) = 5;
                //
                // checked clean with two unused-binding warnings. The comment
                // that used to sit here said a mismatch is "reported where the
                // two are unified" -- and nothing unifies a `let`'s pattern
                // with its initializer's type, so the bindings took `Unknown`
                // and the program was refused later by the code generator,
                // against a line with nothing wrong with it, in a message
                // ending "this is a gap in the compiler worth reporting". It
                // was.
                //
                // Only when the type is settled. An unsolved variable is not a
                // mismatch, it is inference that has not got there yet, and
                // the `Unknown` audit at the end of checking is what reports
                // the ones that never do.
                let settled = self.unifier.shallow(ty);
                match &settled {
                    Type::Tuple(items) if items.len() == fields.len() => {}
                    Type::Tuple(items) => {
                        let message = format!(
                            "this pattern takes a value apart into {}, but `{settled}` has {}",
                            pieces(fields.len()),
                            items.len()
                        );
                        self.error(message, self.body.pat_range(pat));
                        self.broken_pats.insert(pat);
                    }
                    // Inference has not settled it, so there is nothing to
                    // disagree with yet.
                    Type::Unknown | Type::Var(_) | Type::Never => {}
                    other => {
                        let message = format!(
                            "this pattern takes a value apart into {}, but `{other}` is \
                             not a tuple",
                            pieces(fields.len())
                        );
                        self.error(message, self.body.pat_range(pat));
                        self.broken_pats.insert(pat);
                    }
                }

                for (i, field) in fields.iter().enumerate() {
                    let component = match &settled {
                        Type::Tuple(items) => items.get(i).cloned().unwrap_or(Type::Unknown),
                        _ => Type::Unknown,
                    };
                    self.bind_pattern(*field, &component);
                }
            }
            Pat::Wildcard | Pat::Literal(_) | Pat::Path(_) | Pat::Missing => {}
        }
    }

    /// Remembers a `match` to check once the types have settled.
    ///
    /// **Not checked here**, because the scrutinee's type is still being
    /// inferred: see [`Checker::settle_coverage`] for what asking too early
    /// cost. The arms are cloned rather than borrowed because the check runs
    /// after this walk is over.
    pub(super) fn check_match_coverage(
        &mut self,
        scrutinee_ty: &Type,
        arms: &[khora_hir::body::MatchArm],
        range: TextRange,
    ) {
        self.coverage.push((scrutinee_ty.clone(), arms.to_vec(), range));
    }

    /// Whether this pattern, or one nested inside it, has already been
    /// reported on by [`Self::bind_pattern`].
    ///
    /// Nested, because the pattern a reader wrote is not always the arm's
    /// own: `for (k, v) in ..` becomes `Step::Yield($rest, (k, v))`, and the
    /// tuple that does not fit is two levels down.
    fn pat_is_broken(&self, pat: PatId) -> bool {
        if self.broken_pats.contains(&pat) {
            return true;
        }
        match self.body.pat(pat) {
            Pat::TupleStruct { fields, .. } => {
                fields.iter().any(|f| self.pat_is_broken(*f))
            }
            Pat::Tuple(fields) => fields.iter().any(|f| self.pat_is_broken(*f)),
            Pat::Record { fields, .. } => {
                fields.iter().any(|(_, f)| self.pat_is_broken(*f))
            }
            Pat::Bind(_) | Pat::Wildcard | Pat::Literal(_) | Pat::Path(_) | Pat::Missing => false,
        }
    }

    /// A `let` whose pattern can fail.
    ///
    /// **The backend refuses this and `khora check` did not**, so
    /// `let Option::Some(x) = f();` type-checked clean and then failed to
    /// build -- and `scripts/backend-rules.txt` says in its own words where a
    /// refusal a passing program can reach belongs. It was never asked,
    /// because the script that asks read `.fail(` calls one line at a time and
    /// that message wraps onto its own. Roadmap 16.
    ///
    /// Refutability is exhaustiveness over one arm: a `let` is a `match` with
    /// a single pattern and nowhere to send a value that does not fit, so the
    /// same witness search answers both questions and there is no second
    /// notion of coverage to keep in step with the first.
    pub(super) fn report_let_refutability(&mut self, pat: khora_hir::body::PatId, ty: &Type) {
        let column = column_type(self.types, ty);
        if matches!(column, ColumnType::Unknown) {
            return;
        }
        // Same reason as in `report_match_coverage`: a pattern already
        // reported on describes a shape the value does not have, so coverage
        // answers about the wrong thing.
        if self.pat_is_broken(pat) {
            return;
        }

        let patterns = vec![self.to_pattern(pat)];
        let types = self.types;
        let resolve = move |name: &str| -> ColumnType {
            let ty = if name == BOOL_TYPE { Type::Bool } else { Type::adt(name) };
            column_type(types, &ty)
        };

        let missing = usefulness::missing_patterns(&patterns, &column, &resolve);
        if missing.is_empty() {
            return;
        }
        let names: Vec<String> = missing.iter().map(|p| p.to_string()).collect();
        self.error(
            format!(
                "this pattern can fail, so it needs a `match` rather than a `let` — a \
                 `let` has nowhere to send a value that does not match. Not covered: `{}`",
                names.join("`, `")
            ),
            self.body.pat_range(pat),
        );
    }

    pub(super) fn report_match_coverage(
        &mut self,
        scrutinee_ty: &Type,
        arms: &[khora_hir::body::MatchArm],
        range: TextRange,
    ) {
        // A guard can fail, so a guarded arm covers nothing for the purposes of
        // exhaustiveness. Excluding them keeps the check sound.
        let unguarded: Vec<&khora_hir::body::MatchArm> =
            arms.iter().filter(|a| a.guard.is_none()).collect();
        let patterns: Vec<Pattern> =
            unguarded.iter().map(|a| self.to_pattern(a.pat)).collect();

        let column = column_type(self.types, scrutinee_ty);
        if matches!(column, ColumnType::Unknown) {
            return;
        }

        // **A pattern already reported on says nothing about coverage.** It
        // bound `Unknown` and kept the shape it was written with, so the
        // spaces it leaves are spaces in a type it does not belong to; see
        // [`Checker::broken_pats`] for the `for` loop this showed up in.
        if unguarded.iter().any(|arm| self.pat_is_broken(arm.pat)) {
            return;
        }

        // Named types are expanded lazily: an ADT may contain itself, so
        // resolving eagerly would not terminate.
        // Named types expand lazily: an ADT may contain itself, so resolving
        // eagerly would not terminate. Captures the map, not the checker, so
        // reporting can still borrow `self` mutably.
        let types = self.types;
        let resolve = move |name: &str| -> ColumnType {
            let ty =
                if name == BOOL_TYPE { Type::Bool } else { Type::adt(name) };
            column_type(types, &ty)
        };

        let missing = usefulness::missing_patterns(&patterns, &column, &resolve);
        if !missing.is_empty() {
            let names: Vec<String> = missing.iter().map(|p| p.to_string()).collect();
            self.error(
                format!("this `match` is not exhaustive: pattern `{}` not covered", names.join("`, `")),
                range,
            );
        }

        for index in usefulness::unreachable_arms(&patterns, &column, &resolve) {
            let Some(arm) = unguarded.get(index) else { continue };
            // **Say why, when the why is the trap.** A bare name in a pattern
            // is a *binding*, so `Red => ..` where `Colour::Red` was meant
            // matches every colour and the arm after it is unreachable. The
            // program compiles and answers `Red`'s body for green, which is
            // the worst shape a mistake can have -- and reporting only the
            // symptom points at the arm that is right.
            //
            // Looked for among the arms *before* this one, since only those
            // can be what swallowed it, and only where the name is one the
            // scrutinee's own type declares: a binding called `n` is somebody
            // capturing the value and means nothing is wrong.
            let swallowed = unguarded[..index].iter().find_map(|earlier| {
                let Pat::Bind(local) = self.body.pat(earlier.pat) else { return None };
                let name = &self.body.local(*local).name;
                let ColumnType::Finite(ctors) = &column else { return None };
                ctors
                    .iter()
                    .any(|ctor| matches!(ctor, Ctor::Variant { name: case, .. } if case == name))
                    .then(|| name.clone())
            });
            // The scrutinee's own type, so the suggestion is a line somebody
            // can type rather than a shape to fill in.
            let owner = match scrutinee_ty {
                Type::Adt { name, .. } => name.clone(),
                other => other.to_string(),
            };
            match swallowed {
                Some(name) => self.error(
                    format!(
                        "this arm is unreachable: an earlier arm is the bare name `{name}`, \
                         which binds every value rather than matching the case -- write it \
                         qualified, as `{owner}::{name}`"
                    ),
                    self.body.range(arm.body),
                ),
                None => self.error("this arm is unreachable", self.body.range(arm.body)),
            }
        }
    }

    /// A constructor carrying the types of its payload, so specialization can
    pub(super) fn to_pattern(&self, pat: PatId) -> Pattern {
        match self.body.pat(pat) {
            // A binding matches everything, exactly like `_`.
            Pat::Wildcard | Pat::Bind(_) | Pat::Missing => Pattern::Wildcard,
            Pat::Literal(lit) => Pattern::Constructor {
                ctor: match lit {
                    Literal::Bool(b) => Ctor::Bool(*b),
                    Literal::Int(n) => Ctor::Literal(n.clone()),
                    Literal::Float(n) => Ctor::Literal(n.clone()),
                    Literal::Str(s) => Ctor::Literal(format!("\"{s}\"")),
                    // Quoted the way it was written, so two arms matching the
                    // same character are the same constructor and two matching
                    // different ones are not. `'a'` and `"a"` must not collide,
                    // which is why the quote is part of the key.
                    Literal::Char(c) => Ctor::Literal(format!("'{c}'")),
                },
                fields: Vec::new(),
            },
            Pat::Path(resolution) | Pat::TupleStruct { resolution, .. } => {
                let sub = match self.body.pat(pat) {
                    Pat::TupleStruct { fields, .. } => {
                        fields.iter().map(|f| self.to_pattern(*f)).collect()
                    }
                    _ => Vec::new(),
                };
                match variant_case(resolution)
                    .and_then(|(h, t, n)| self.types.variant_of(h.as_ref(), &t, &n))
                {
                    Some(v) => Pattern::Constructor { ctor: ctor_for(self.types, v), fields: sub },
                    None => Pattern::Wildcard,
                }
            }
            // **Put back in the declaration's order**, because usefulness
            // works positionally and a record pattern does not: it may write
            // its fields in any order and leave any of them out. One left out
            // constrains nothing, which is a wildcard.
            Pat::Record { resolution, fields } => {
                match variant_case(resolution)
                    .and_then(|(h, t, n)| self.types.variant_of(h.as_ref(), &t, &n))
                {
                    Some(v) => {
                        let sub = v
                            .labels
                            .iter()
                            .map(|label| {
                                fields
                                    .iter()
                                    .find(|(l, _)| l == label)
                                    .map(|(_, p)| self.to_pattern(*p))
                                    .unwrap_or(Pattern::Wildcard)
                            })
                            .collect();
                        Pattern::Constructor { ctor: ctor_for(self.types, v), fields: sub }
                    }
                    None => Pattern::Wildcard,
                }
            }
            Pat::Tuple(fields) => Pattern::Constructor {
                ctor: Ctor::Tuple(fields.len()),
                fields: fields.iter().map(|f| self.to_pattern(*f)).collect(),
            },
        }
    }
}

/// `2 pieces`, and `1 piece` — because a message that says "1 pieces" reads as
/// a machine wrote it, which is the impression this whole file works against.
fn pieces(count: usize) -> String {
    if count == 1 { "1 piece".to_string() } else { format!("{count} pieces") }
}

