//! Patterns, and the arms they belong to.
//!
//! A pattern binds names into the enclosing scope, so lowering one is as much
//! about the scope stack as about the tree. Resolving a constructor path is
//! here too, because a pattern is the one place a bare name might be a
//! constructor rather than a binding — and getting that backwards silently
//! turns a typo into a catch-all.

use super::*;

impl<'a> Ctx<'a> {
    pub(super) fn lower_pat(&mut self, pat: &ast::Pat, is_mut: bool) -> PatId {
        let range = pat.syntax().text_range();
        match pat {
            ast::Pat::Wildcard(_) => self.add_pat(Pat::Wildcard, range),
            ast::Pat::Ident(p) => match p.name().and_then(|n| n.ident()) {
                Some(name) => {
                    let local = self.declare(name, is_mut, range);
                    self.add_pat(Pat::Bind(local), range)
                }
                None => self.add_pat(Pat::Missing, range),
            },
            ast::Pat::Literal(p) => match literal_of(p.syntax()) {
                Some(lit) => self.add_pat(Pat::Literal(lit), range),
                None => self.add_pat(Pat::Missing, range),
            },
            ast::Pat::Path(p) => {
                let resolution = self.resolve_pattern_path(p.path().as_ref(), range);
                self.add_pat(Pat::Path(resolution), range)
            }
            ast::Pat::TupleStruct(p) => {
                let resolution = self.resolve_pattern_path(p.path().as_ref(), range);
                let fields = p.fields().map(|f| self.lower_pat(&f, is_mut)).collect();
                self.add_pat(Pat::TupleStruct { resolution, fields }, range)
            }
            ast::Pat::Tuple(p) => {
                let fields = p.fields().map(|f| self.lower_pat(&f, is_mut)).collect();
                self.add_pat(Pat::Tuple(fields), range)
            }
            // **`{ name }` binds `name`; `{ name: pattern }` matches it.**
            // The shorthand is the common one and it is the reason a field
            // holds an optional sub-pattern rather than a required one: with
            // no pattern written there is nothing to lower, and the field's
            // own label is what the binding is called.
            ast::Pat::Record(p) => {
                let resolution = self.resolve_record_path(p.path().as_ref(), range);
                let fields = p
                    .fields()
                    .filter_map(|f| {
                        let name = f.name()?.ident()?;
                        let sub = match f.pat() {
                            Some(sub) => self.lower_pat(&sub, is_mut),
                            None => {
                                let field = f.syntax().text_range();
                                let local = self.declare(name.clone(), is_mut, field);
                                self.add_pat(Pat::Bind(local), field)
                            }
                        };
                        Some((name, sub))
                    })
                    .collect();
                self.add_pat(Pat::Record { resolution, fields }, range)
            }
        }
    }

    /// A pattern path names a constructor. Only same-file constructors resolve
    /// in phase 2, which is enough for the vertical slice.
    pub(super) fn resolve_pattern_path(
        &mut self,
        path: Option<&ast::Path>,
        range: TextRange,
    ) -> crate::Resolution {
        let segments: Vec<String> = path
            .map(|p| p.segments().filter_map(|s| s.ident()).collect())
            .unwrap_or_default();

        if let [type_name, case] = segments.as_slice() {
            if let Some(v) = self
                .map
                .variants_of(type_name)
                .chain(self.scope.variants_of(type_name))
                .find(|v| &v.name == case)
            {
                return crate::Resolution::Variant {
                    module: self.home_of_type(type_name),
                    type_name: v.type_name.clone(),
                    name: v.name.clone(),
                };
            }
        }

        // **A constructor named after its own type**, which is what
        // `type UserId = Int;` has: `match id { UserId(v) => v }` is one
        // segment, not two, because there is no case to name apart from the
        // type. Spelling it `UserId::UserId(v)` would be true and nobody would
        // write it.
        //
        // Only where the type really has a constructor of that name, so this
        // cannot turn a mistyped binding into a constructor: a bare name is an
        // `Ident` pattern and never reaches here.
        if let [only] = segments.as_slice() {
            if let Some(v) = self
                .map
                .variants_of(only)
                .chain(self.scope.variants_of(only))
                .find(|v| &v.name == only)
            {
                return crate::Resolution::Variant {
                    module: self.home_of_type(only),
                    type_name: v.type_name.clone(),
                    name: v.name.clone(),
                };
            }
        }

        self.error(self.why_no_constructor(&segments), range);
        crate::Resolution::Unsupported("unresolved constructor")
    }

    /// Why a constructor path found nothing.
    ///
    /// **`cannot find constructor `Result::Ok`` was the one "not in scope"
    /// message in this compiler that did not name the fix.** Its neighbours
    /// all do — ``derive(Show)` needs `Show` in scope; import it from
    /// `std::core``, ``for` needs `Step` and `Iterator` in scope; import them
    /// from `std::core``, ``JsonError` is not in scope here … add it to an
    /// `import`` — and a reader who has been told the fix four times reads the
    /// fifth as a different kind of problem.
    ///
    /// Two different mistakes hide behind one sentence, and they have
    /// different fixes: the *type* is not in scope (an import), or the type is
    /// and the *case* is misspelled (a name). Which one it is, is exactly what
    /// the two lookups above already established.
    fn why_no_constructor(&self, segments: &[String]) -> String {
        let path = segments.join("::");
        let [type_name, case] = segments else {
            return format!("cannot find constructor `{path}`");
        };

        let cases: Vec<&str> = self
            .map
            .variants_of(type_name)
            .chain(self.scope.variants_of(type_name))
            .map(|v| v.name.as_str())
            .collect();
        if cases.is_empty() {
            return format!(
                "cannot find constructor `{path}`: `{type_name}` is not in scope here \
                 — add it to an `import`"
            );
        }

        // The type is here and the case is not, so the reader wants the list,
        // not an import. Named rather than counted: a sum type has few cases,
        // and seeing them is usually seeing the typo.
        let known: Vec<String> = cases.iter().map(|c| format!("`{c}`")).collect();
        format!(
            "cannot find constructor `{path}`: `{type_name}` has no case `{case}`. \
             It has {}",
            known.join(" and ")
        )
    }

    /// The path in `Type { .. }`, which may name a record type directly.
    ///
    /// **A record type is its own one case and is deliberately not a
    /// constructor.** `collect_decl` records a self-named constructor for
    /// `type UserId = Int` and pointedly not for `type Pair = { a, b }`,
    /// because `Pair(3, "hi")` is not how one is made -- a record names its
    /// fields. In a *pattern* the same name has no other reading, and
    /// `khora-types` already keeps a record type as one `VariantInfo` named
    /// after the type, so this resolves to the case the checker and the
    /// backend both look for.
    ///
    /// Everything else -- `Type::Case { .. }` for a variant with named fields,
    /// and a wrapper's self-named case -- is the ordinary pattern path.
    pub(super) fn resolve_record_path(
        &mut self,
        path: Option<&ast::Path>,
        range: TextRange,
    ) -> crate::Resolution {
        let segments: Vec<String> = path
            .map(|p| p.segments().filter_map(|s| s.ident()).collect())
            .unwrap_or_default();

        if let [only] = segments.as_slice() {
            let is_case = self
                .map
                .variants_of(only)
                .chain(self.scope.variants_of(only))
                .any(|v| &v.name == only);
            let names_a_type = self
                .map
                .item(only)
                .is_some_and(|i| matches!(i.kind, crate::ItemKind::Type))
                || self.scope.origins.iter().any(|o| &o.local == only);
            if !is_case && names_a_type {
                return crate::Resolution::Variant {
                    module: self.home_of_type(only),
                    type_name: only.clone(),
                    name: only.clone(),
                };
            }
        }

        self.resolve_pattern_path(path, range)
    }

    pub(super) fn lower_match(&mut self, e: &ast::MatchExpr, range: TextRange) -> ExprId {
        let scrutinee = match e.scrutinee() {
            Some(s) => self.lower_expr(&s),
            None => self.add_expr(Expr::Missing, range),
        };

        let arms = self.lower_arms(e.arms(), range);
        self.add_expr(Expr::Match { scrutinee, arms }, range)
    }

    /// The arms of a `match` or a `catch`, which are the same thing: a pattern,
    /// an optional guard, and a body, with the pattern's bindings scoped to
    /// that arm alone.
    pub(super) fn lower_arms(
        &mut self,
        arms: impl Iterator<Item = ast::MatchArm>,
        range: TextRange,
    ) -> Vec<MatchArm> {
        arms.map(|arm| {
            self.scopes.push(Vec::new());
            let pat = match arm.pat() {
                Some(p) => self.lower_pat(&p, false),
                None => self.add_pat(Pat::Missing, arm.syntax().text_range()),
            };
            let guard = arm.guard().and_then(|g| g.condition()).map(|c| self.lower_expr(&c));
            let body = match arm.body() {
                Some(b) => self.lower_expr(&b),
                None => self.add_expr(Expr::Missing, range),
            };
            self.scopes.pop();
            MatchArm { pat, guard, body }
        })
        .collect()
    }
}
