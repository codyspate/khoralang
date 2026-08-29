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
    /// A `context` contributes its labelled bindings. Anything else is a
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
        if self.expanding.iter().any(|n| n == name) {
            let loop_ = self.expanding.join("` -> `");
            self.error(
                format!("`{name}` is defined in terms of itself: `{loop_}` -> `{name}`"),
                range,
            );
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

        // Lowered in the *constant's* scope, not the mention's: a constant
        // cannot see the locals of whatever function happens to name it.
        let outer = std::mem::take(&mut self.scopes);
        let outer_in_scope = std::mem::take(&mut self.in_scope);
        self.expanding.push(name.to_string());
        let id = self.lower_expr(&initializer);
        self.expanding.pop();
        self.scopes = outer;
        self.in_scope = outer_in_scope;
        Some(id)
    }

    /// `with { ledger: h } { .. }` — the labels bound over a region.
    ///
    /// `by_type` are paths naming handler *values*: `with MyDatabase { .. }`.
    /// They are bound first, so an explicitly labelled binding in the same
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
