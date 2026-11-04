//! Capabilities and constants: the two things a body inherits from its file.
//!
//! A `with` block lowers to an ordinary block of `let`s — that is the whole of
//! installing a handler at run time, per `docs/design/effect-runtime.md` §2 —
//! and which bindings could supply which label is recorded here, because it can
//! only be answered while the scope stack is live.
//!
//! A module-level `let` is a constant rather than a global, so a mention of one
//! is lowered into its initializer. Which is why the expansion stack exists:
//! `let a = b; let b = a;` is a cycle, and inlining is what turns a cycle into
//! a stack overflow rather than a diagnostic.

use super::*;

impl<'a> Ctx<'a> {
    /// The bindings of the `context` a `with Mock { .. }` names.
    ///
    /// A context is a named `with` row and nothing else, so installing one is
    /// installing its bindings: same `let`s, same labels, same subtraction.
    /// Sorts a path after `with` into the two things it can be.
    ///
    /// A `context` contributes its labeled bindings. Anything else is a
    /// handler value installed by its type, which is not an error to be
    /// resolved here -- `lower_installation` resolves the name the same way
    /// any other mention of it would, and reports it missing if it is.
    ///
    /// A context wins where a name is both, because that is what the name
    /// means today. `docs/design/capability-installation.md`.
    pub(super) fn installed_paths(
        &mut self,
        named: impl Iterator<Item = ast::Path>,
    ) -> (Vec<ast::RecordExprField>, Vec<ast::Path>) {
        let mut row = Vec::new();
        let mut by_type = Vec::new();
        for path in named {
            match self.context_bindings(&path) {
                Some(bindings) => row.extend(bindings),
                None => by_type.push(path),
            }
        }
        (row, by_type)
    }

    /// `None` when no context has that name, which is not an error: the path
    /// may be naming a handler *value* to install by type instead, and only
    /// the caller knows whether that is still open.
    pub(super) fn context_bindings(
        &mut self,
        named: &ast::Path,
    ) -> Option<Vec<ast::RecordExprField>> {
        let name = named.text_path();
        self.contexts.iter().find(|(n, _)| n == &name).map(|(_, decl)| decl.bindings().collect())
    }

    /// A mention of a module-level `let`, lowered into what it was bound to.
    ///
    /// `None` when the name is not one, which is the ordinary case.
    ///
    /// **Inlined rather than called.** A `let` at module level is a named
    /// expression — Rust's `const`, not its `static` — so there is no
    /// initialization order to get wrong, no global to release at exit, and no
    /// shared state for two fibers to reach. It costs one copy of the
    /// expression per mention, which for the handlers this exists to name is
    /// the right answer anyway: two `with` blocks should get two handlers.
    pub(super) fn expand_constant(&mut self, name: &str, range: TextRange) -> Option<ExprId> {
        let decl = self.constants.iter().find(|(n, _)| n == name).map(|(_, d)| d.clone())?;

        // `const a = b; const b = a;` would otherwise be a stack overflow, and a
        // stack overflow is not a diagnostic.
        //
        // **One cycle, one error.** Every constant on it reaches it from its
        // own body, and a constant that merely names one reaches it too, so
        // the error is given only on entering the loop at the member declared
        // first; and it names the loop alone, not the path into it, so each
        // body that finds it says the same thing at the same place.
        if let Some(at) = self.expanding.iter().position(|n| n == name) {
            let members = &self.expanding[at..];
            let declared = |n: &String| self.constants.iter().position(|(c, _)| c == n);
            let first = members.iter().min_by_key(|n| declared(n));
            if first.is_some_and(|first| first == name) {
                let loop_ = members.join("` -> `");
                self.error(
                    format!("`{name}` is defined in terms of itself: `{loop_}` -> `{name}`"),
                    range,
                );
            }
            return Some(self.add_expr(Expr::Missing, range));
        }

        // A mutable global would be shared state two fibers could reach, which
        // `docs/design/memory.md` §5a does not allow to cross. The check that
        // used to be here is now in the parser, where `const mut` is rejected
        // as it is written — a constant has no `mut` to ask about.

        let Some(initializer) = decl.initializer() else {
            self.error(format!("`{name}` has no value to stand for"), range);
            return Some(self.add_expr(Expr::Missing, range));
        };

        let outer = self.enter_module_level();
        self.expanding.push(name.to_string());
        let id = self.lower_expr(&initializer);
        self.expanding.pop();
        self.leave_module_level(outer);
        Some(id)
    }

    /// Puts away everything the body being lowered has open, so that what is
    /// lowered next sees only the file it is in.
    ///
    /// **What this prevents: a constant meaning something different at each
    /// mention.** Its initializer is lowered in the middle of whatever names
    /// it, and every piece of that body's state reached it. The closure around
    /// a mention made an unresolved name an invented capability, declared into
    /// a scope stack that had been emptied -- a compiler panic. The loop
    /// around one let `const X = break;` break out of it, the closure being
    /// defined around one was what a free name in the initializer resolved
    /// to, the function's type parameters were in scope, and a mention inside
    /// `${..}` moved the initializer's ranges by the hole's offset, past the
    /// end of the file.
    ///
    /// [`ModuleLevel`] holds every field of [`Ctx`] that is per-body state.
    /// Nothing checks that: a field added to `Ctx` for a body has to be added
    /// there too, or a constant sees it. What stays visible is what a
    /// module-level declaration can see: the file's items, imports, contexts
    /// and constants, and the stack of constants being expanded -- which is
    /// also how lowering knows it is inside an initializer.
    ///
    /// The fresh scope stack has one scope open rather than none, so that
    /// whatever declares a name at the initializer's top level has somewhere
    /// to put it rather than panicking, as the invented capability did.
    ///
    /// `site` stays as it was. It decides how a missing capability is
    /// explained, and inside an initializer nothing asks:
    /// `lower_segments` only files a name for that question outside one.
    pub(super) fn enter_module_level(&mut self) -> ModuleLevel {
        ModuleLevel {
            scopes: std::mem::replace(&mut self.scopes, vec![Vec::new()]),
            in_scope: std::mem::take(&mut self.in_scope),
            generics: std::mem::take(&mut self.generics),
            loop_depth: std::mem::take(&mut self.loop_depth),
            lambdas: std::mem::take(&mut self.lambdas),
            lambda_names: std::mem::take(&mut self.lambda_names),
            lambda_evidence: std::mem::take(&mut self.lambda_evidence),
            range_shift: std::mem::take(&mut self.range_shift),
        }
    }

    /// Restores what [`Ctx::enter_module_level`] put away.
    pub(super) fn leave_module_level(&mut self, outer: ModuleLevel) {
        let ModuleLevel {
            scopes,
            in_scope,
            generics,
            loop_depth,
            lambdas,
            lambda_names,
            lambda_evidence,
            range_shift,
        } = outer;
        self.scopes = scopes;
        self.in_scope = in_scope;
        self.generics = generics;
        self.loop_depth = loop_depth;
        self.lambdas = lambdas;
        self.lambda_names = lambda_names;
        self.lambda_evidence = lambda_evidence;
        self.range_shift = range_shift;
    }

    /// `with { ledger: h } { .. }` — the labels bound over a region.
    ///
    /// `by_type` are paths naming handler *values*: `with MyDatabase { .. }`.
    /// They are bound first, so an explicitly labeled binding in the same
    /// `with` shadows one -- the same "later wins" rule the labels follow
    /// among themselves.
    ///
    /// Each is bound under the path as written. No callee asks for a
    /// capability by that name, so the binding is invisible to the ordinary
    /// by-label lookup; what makes it reachable is its type, and the checker
    /// does that. `docs/design/capability-installation.md`.
    pub(super) fn lower_installation(
        &mut self,
        row: Vec<ast::RecordExprField>,
        by_type: Vec<ast::Path>,
        body: Option<&ast::Expr>,
        range: TextRange,
    ) -> ExprId {
        self.scopes.push(Vec::new());
        let outer = self.in_scope.len();

        let mut stmts = Vec::new();
        let mut labels = Vec::new();

        for path in by_type {
            let name = path.text_path();
            let at = self.shifted(path.syntax().text_range());
            let before = self.body.errors.len();
            let value = self.lower_path(&path, at);
            // **A name that resolves to nothing here gets a `with`-shaped
            // message.** Ordinary resolution says "cannot find `Nope` in this
            // scope", which is true and unhelpful in the one position where a
            // reader may well have meant a `context` and mistyped it. The
            // error is rewritten rather than added to, because two complaints
            // about one name is worse than either.
            if matches!(self.body.expr(value), Expr::Unresolved(_)) {
                if let Some(reported) = self.body.errors.get_mut(before) {
                    reported.message = format!(
                        "cannot find `{name}` in this scope; `with` takes a handler value \
                         or the name of a `context`"
                    );
                }
            }
            let local = self.declare(name.clone(), false, path.syntax().text_range());
            self.in_scope.push((name, local));
            self.body.by_type.push(local);
            let pat = self.add_pat(Pat::Bind(local), path.syntax().text_range());
            stmts.push(Stmt::Let { pat, ty: None, init: Some(value) });
        }
        for field in row {
            let value = match field.value() {
                Some(v) => self.lower_expr(&v),
                None => self.add_expr(Expr::Missing, range),
            };
            // Declared after its own initializer, so `with { ledger: ledger }`
            // means the outer one — the same rule every `let` follows.
            let pat = match field.name().and_then(|n| n.ident()) {
                Some(label) => {
                    labels.push(label.clone());
                    let local = self.declare(label.clone(), false, field.syntax().text_range());
                    self.in_scope.push((label, local));
                    self.add_pat(Pat::Bind(local), field.syntax().text_range())
                }
                None => self.add_pat(Pat::Wildcard, field.syntax().text_range()),
            };
            stmts.push(Stmt::Let { pat, ty: None, init: Some(value) });
        }

        let tail = body.map(|b| self.lower_expr(b));
        self.scopes.pop();
        self.in_scope.truncate(outer);

        let block = self.add_expr(Expr::Block { stmts, tail }, range);
        self.body.installs.insert(block, labels);
        block
    }
}
