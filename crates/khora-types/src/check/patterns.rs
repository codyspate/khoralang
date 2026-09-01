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
                        self.error(
                            format!(
                                "this pattern takes a value apart into {}, but `{settled}` \
                                 has {}",
                                pieces(fields.len()),
                                items.len()
                            ),
                            self.body.pat_range(pat),
                        );
                    }
                    // Inference has not settled it, so there is nothing to
                    // disagree with yet.
                    Type::Unknown | Type::Var(_) | Type::Never => {}
                    other => {
                        self.error(
                            format!(
                                "this pattern takes a value apart into {}, but `{other}` is \
                                 not a tuple",
                                pieces(fields.len())
                            ),
                            self.body.pat_range(pat),
                        );
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
            if let Some(arm) = unguarded.get(index) {
                self.error("this arm is unreachable", self.body.range(arm.body));
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

