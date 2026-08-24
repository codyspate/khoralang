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
    /// `with { ledger: h } { .. }` — the labels bound over a region.
    /// The bindings of the `context` a `with Mock { .. }` names.
    ///
    /// A context is a named `with` row and nothing else, so installing one is
    /// installing its bindings: same `let`s, same labels, same subtraction.
    pub(super) fn context_bindings(
        &mut self,
        named: &ast::Path,
        range: TextRange,
    ) -> Vec<ast::RecordExprField> {
        let name = named.text_path();
        match self.contexts.iter().find(|(n, _)| n == &name) {
            Some((_, decl)) => decl.bindings().collect(),
            None => {
                self.error(
                    format!("cannot find a `context` named `{name}` in this file"),
                    range,
                );
                Vec::new()
            }
        }
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

    pub(super) fn lower_installation(
        &mut self,
        row: Vec<ast::RecordExprField>,
        body: Option<&ast::Expr>,
        range: TextRange,
    ) -> ExprId {
        self.scopes.push(Vec::new());
        let outer = self.in_scope.len();

        let mut stmts = Vec::new();
        let mut labels = Vec::new();
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
                    self.add_pat(Pat::Bind(local))
                }
                None => self.add_pat(Pat::Wildcard),
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
