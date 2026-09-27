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
                if self.body.written_binds.contains(&pat) {
                    self.bare_names.push((pat, ty.clone()));
                }
                self.locals.insert(local, ty.clone());
            }
            Pat::TupleStruct { resolution, fields } => {
                let variant = variant_case(&resolution)
                    .and_then(|(h, t, n)| self.types.variant_of(h.as_ref(), &t, &n))
                    .cloned();
                if let Some(v) = variant.as_ref() {
                    if !self.pattern_fits(pat, &v.type_name, ty) {
                        for field in &fields {
                            self.bind_pattern(*field, &Type::Unknown);
                        }
                        return;
                    }
                }
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
                if let Some(v) = variant.as_ref() {
                    if !self.pattern_fits(pat, &v.type_name, ty) {
                        for (_, field) in &fields {
                            self.bind_pattern(*field, &Type::Unknown);
                        }
                        return;
                    }
                }
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
            Pat::Path(resolution) => {
                let owner = variant_case(&resolution)
                    .and_then(|(h, t, n)| self.types.variant_of(h.as_ref(), &t, &n))
                    .map(|v| v.type_name.clone());
                if let Some(owner) = owner {
                    self.pattern_fits(pat, &owner, ty);
                }
            }
            Pat::Wildcard | Pat::Literal(_) | Pat::Missing => {}
        }
    }

    /// Whether a constructor pattern of `owner` can match a value of `ty`,
    /// reporting it when it cannot.
    ///
    /// **Without this a pattern was trusted to name the right type**, and
    /// the field types were read off its declaration whatever the value was.
    /// `match 3 { Option::Some(v) => .. }` then reached the code generator and
    /// panicked it; `Option<Big>` matched with `Result::Ok(v)` built, ran,
    /// and read `v` from a `Some`'s layout, a wrong answer with no error.
    ///
    /// Unified with a fresh instance of `owner` rather than compared, so a
    /// value whose type is still a variable takes the pattern's type -- the
    /// way a lambda parameter matched on learns what it is -- and a rigid
    /// parameter is refused by the unifier's own rule. An `Unknown` or a
    /// `Never` is already somebody else's error, or cannot arrive, so neither
    /// is compared.
    fn pattern_fits(&mut self, pat: PatId, owner: &str, ty: &Type) -> bool {
        let settled = self.unifier.shallow(ty);
        if matches!(settled, Type::Unknown | Type::Never) {
            return true;
        }
        let (expected, _) = self.instantiate_adt(owner);
        if self.unifier.unify(&expected, &settled).is_ok() {
            return true;
        }
        let found = self.unifier.zonk(&settled);
        self.error(
            format!("this pattern is a `{owner}` case, and the value here is a `{found}`"),
            self.body.pat_range(pat),
        );
        self.broken_pats.insert(pat);
        false
    }

    /// A bare name in a pattern that is the name of one of its value's cases.
    ///
    /// **A bare name binds**, so `Red => "warm"` over a `Colour` matched
    /// every colour and answered "warm" for green. Where it was not followed
    /// by an arm it made unreachable, nothing was reported but that `Red` was
    /// never read -- a warning, on a program that built and gave a wrong
    /// answer, gone as soon as the arm read the name.
    ///
    /// Refused rather than resolved to the case. Resolving it would make what
    /// a name means depend on the type of the value it meets, so a case added
    /// to an upstream type would turn a working binding into a case in a
    /// program nobody edited. Refusing makes the same addition an error.
    ///
    /// Asked after the body, for `settle_coverage`'s reason: the value's type
    /// may be a variable when the pattern is bound. A type still unknown then
    /// is not asked about -- `check_unknowns` speaks for it. Nullary and
    /// payload cases alike, since `NotFound => ..` swallows a `NotFound(p)`
    /// the same way.
    ///
    /// **Costs** one lookup per written binding whose value is a named type,
    /// and for a type this file never imported, a `type_map` of the module
    /// that declares it -- a query, so paid once per module, not per binding.
    pub(crate) fn settle_bare_names(
        &mut self,
        declared_elsewhere: &dyn Fn(&khora_hir::ModulePath, &str) -> Vec<crate::VariantInfo>,
    ) {
        for (pat, ty) in std::mem::take(&mut self.bare_names) {
            let Pat::Bind(local) = self.body.pat(pat) else { continue };
            let name = self.body.local(*local).name.clone();
            let settled = self.unifier.zonk(&ty);
            let Some(case) = self.case_named(&settled, &name, declared_elsewhere) else {
                // A2: see `refuse_capitalised_binding`. Deleting this one
                // line removes it.
                self.refuse_capitalised_binding(pat, &name, &settled);
                continue;
            };
            let BareCase { owner, arity, labelled, imported, only } = case;
            // **The type's only case**: a record, or `type UserId = Int`. The
            // binding matched exactly what the case would have, so the answer
            // was right and only what it looked like was wrong. `_`, or a
            // lower-case name, says the same thing without looking like a case.
            if only {
                self.error(
                    format!(
                        "`{name}` is the name of `{owner}`'s only case, and a bare name in a \
                         pattern binds rather than matching it. Write `_` to match any \
                         `{owner}`, or a lower-case name to bind it"
                    ),
                    self.body.pat_range(pat),
                );
                self.broken_pats.insert(pat);
                continue;
            }
            // The pattern that matches the case, spelled so it compiles as
            // written: a record type's own case is `Name {}`, a payload takes
            // one `_` per field, and a type's self-named case one segment.
            let head = if owner == name { name.clone() } else { format!("{owner}::{name}") };
            let written = if owner == name && labelled {
                format!("{name} {{}}")
            } else if arity == 0 {
                head
            } else {
                format!("{head}({})", vec!["_"; arity].join(", "))
            };
            // Qualifying needs the type in scope, and this file may never
            // have named it -- the value arrived from a call.
            let import = match imported {
                Some(home) => format!(" (with `{owner}` imported from `{home}`)"),
                None => String::new(),
            };
            self.error(
                format!(
                    "`{name}` is a case of `{owner}`, and a bare name in a pattern binds \
                     rather than matching one -- this would match every `{owner}`. Write \
                     `{written}`{import} to match the case, or pick another name to bind \
                     the value"
                ),
                self.body.pat_range(pat),
            );
            self.broken_pats.insert(pat);
        }
    }

    /// The case of `settled` called `name`, if its type declares one.
    ///
    /// **Looks past this file's imports.** A `catch` over `load(n)!` meets a
    /// `LoadError` nobody here named, and a `match` on `colour(n)` a `Shade`;
    /// checked against the scope, those names are nothing and the binding
    /// swallows in silence. That is not resolving a name the source wrote --
    /// which is why it may look outside the file's scope -- it is asking what
    /// the value holds, and the declaring module is the one that knows.
    fn case_named(
        &self,
        settled: &Type,
        name: &str,
        declared_elsewhere: &dyn Fn(&khora_hir::ModulePath, &str) -> Vec<crate::VariantInfo>,
    ) -> Option<BareCase> {
        let Type::Adt { name: owner, home, .. } = settled else { return None };
        let in_scope: Vec<crate::VariantInfo> =
            self.types.variants_of(home.as_ref(), owner).into_iter().cloned().collect();
        let (cases, imported) = match (in_scope.is_empty(), home) {
            (false, _) => (in_scope, None),
            (true, Some(home)) => {
                (declared_elsewhere(home, owner), Some(home.segments().join("::")))
            }
            (true, None) => return None,
        };
        let case = cases.iter().find(|v| v.name == name)?;
        Some(BareCase {
            owner: owner.clone(),
            arity: case.fields.len(),
            labelled: case.labels.iter().any(|l| !l.is_empty()),
            imported,
            only: cases.len() == 1,
        })
    }

    /// **A2 -- the owner's decision, kept apart so it can be dropped.**
    ///
    /// A capitalised bare name in a pattern that is no case of its value's
    /// type. [`Checker::settle_bare_names`] catches `Red` over a `Colour`;
    /// this catches the two catch-alls it cannot: a typo, `Gren => ..` for
    /// `Colour::Green`, and a `const`, `FAVOURITE => ..`, which binds a new
    /// name rather than comparing against the constant. Both built with only
    /// an `unused-binding` warning, which the arm reading the name removed.
    ///
    /// **Costs** letter case a meaning in patterns, where elsewhere it is a
    /// convention: a lower-case typo (`gren`) still binds, and a program
    /// that binds with a capitalised name has to rename it.
    ///
    /// To drop A2: delete this function, its call in `settle_bare_names`, and
    /// the `mod a2` block in `khora-types/tests/bare_patterns.rs`.
    fn refuse_capitalised_binding(&mut self, pat: PatId, name: &str, settled: &Type) {
        if !name.chars().next().is_some_and(char::is_uppercase) {
            return;
        }
        // An unsettled type is `check_unknowns`' to report, and the message
        // below has to name one.
        if matches!(settled, Type::Unknown | Type::Var(_)) {
            return;
        }
        self.error(
            format!(
                "`{name}` binds the value, because it is no case of `{settled}` -- and a \
                 capitalised name in a pattern reads as a case. Bind it with a lower-case \
                 name, or write the case it was meant to be in full"
            ),
            self.body.pat_range(pat),
        );
        self.broken_pats.insert(pat);
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
            // A bare case name before this arm is not what made it
            // unreachable: `settle_bare_names` refused that name and marked
            // it broken, and a broken pattern skips this check above.
            self.error("this arm is unreachable", self.body.range(arm.body));
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

/// What [`Checker::settle_bare_names`] needs to write the pattern that
/// matches a case, so that the suggestion compiles as written.
struct BareCase {
    owner: String,
    /// Payload fields, one `_` each in the suggestion.
    arity: usize,
    /// Named fields: a record type's own case is written `Name {}`.
    labelled: bool,
    /// The declaring module, where this file never imported the type.
    imported: Option<String>,
    /// The type has no other case, so the binding matched what the case
    /// would have and only its spelling misleads.
    only: bool,
}

