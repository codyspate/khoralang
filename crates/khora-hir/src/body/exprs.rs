//! Expressions, from syntax tree to HIR.
//!
//! Mostly one arm per `ast::Expr`. What is not one-to-one lives in `desugar`,
//! and what needs a scope of its own lives in `lambda` — this is the ordinary
//! shape-preserving half, plus the two operators that are not: `|>`, which
//! rewrites into a call, and assignment, which has to decide whether its target
//! is a place.

use super::*;

impl<'a> Ctx<'a> {
    pub(super) fn lower_block(&mut self, block: &ast::Block) -> ExprId {
        self.scopes.push(Vec::new());
        let mut stmts = Vec::new();

        for stmt in block.stmts() {
            match stmt {
                ast::Stmt::Let(let_decl) => {
                    // The initializer is lowered before the binding is
                    // declared, so `let x = x;` refers to the outer `x`.
                    //
                    // A lambda is the exception: `let go = fn n => .. go(..)`
                    // has to see its own name, or a recursive closure cannot be
                    // written at all. The name resolves to the closure itself,
                    // not to a binding that does not exist yet.
                    let recursive = match let_decl.initializer() {
                        Some(ast::Expr::Lambda(_)) => {
                            let_decl.pat().and_then(|p| match p {
                                ast::Pat::Ident(i) => i.name().and_then(|n| n.ident()),
                                _ => None,
                            })
                        }
                        _ => None,
                    };
                    let init = let_decl.initializer().map(|e| match (&e, recursive) {
                        (ast::Expr::Lambda(l), Some(name)) => {
                            let range = e.syntax().text_range();
                            self.lower_lambda_named(l, Some(name), range)
                        }
                        _ => self.lower_expr(&e),
                    });
                    let pat = match let_decl.pat() {
                        Some(p) => self.lower_pat(&p, let_decl.is_mut()),
                        None => self.add_pat(Pat::Missing, let_decl.syntax().text_range()),
                    };
                    let ty = let_decl.ty().as_ref().map(TypeRef::of_syntax);
                    stmts.push(Stmt::Let { pat, ty, init });
                }
                ast::Stmt::Expr(expr_stmt) => {
                    if let Some(e) = expr_stmt.expr() {
                        let id = self.lower_expr(&e);
                        stmts.push(Stmt::Expr(id));
                    }
                }
            }
        }

        let tail = block.tail_expr().map(|e| self.lower_expr(&e));
        self.scopes.pop();
        self.add_expr(Expr::Block { stmts, tail }, block.syntax().text_range())
    }

    pub(super) fn lower_expr(&mut self, expr: &ast::Expr) -> ExprId {
        let range = expr.syntax().text_range();
        match expr {
            ast::Expr::Literal(e) => {
                let text = e.syntax().text().to_string();
                // The one literal that is not a value of its own: a decimal is
                // a call, desugared here so nothing downstream has a case.
                if e.syntax().first_token().is_some_and(|t| t.kind() == khora_syntax::SyntaxKind::DECIMAL_LIT) {
                    return self.lower_decimal(&text, range);
                }
                // Either delimiter interpolates. A reader who has written
                // `"${name}"` will write it in a backtick literal too and be
                // right, and one string with two escaping rules is worse than
                // one rule.
                let quoted = text.starts_with('"') || text.starts_with('`');
                if quoted && has_interpolation(&text) {
                    return self.lower_interpolation(&text, range);
                }
                match literal_of(e.syntax()) {
                    Some(lit) => self.add_expr(Expr::Literal(lit), range),
                    None => self.add_expr(Expr::Missing, range),
                }
            }
            ast::Expr::Path(e) => self.lower_path_expr(e, range),
            ast::Expr::Block(b) => self.lower_block(b),
            ast::Expr::Paren(e) => match e.syntax().children().find_map(ast::Expr::cast) {
                Some(inner) => self.lower_expr(&inner),
                None => self.add_expr(Expr::Missing, range),
            },
            ast::Expr::Unit(_) => self.add_expr(Expr::Unit, range),
            ast::Expr::Field(e) => {
                let base = match e.base() {
                    Some(b) => self.lower_expr(&b),
                    None => self.add_expr(Expr::Missing, range),
                };
                let name = e.field().and_then(|f| f.ident()).unwrap_or_default();
                self.add_expr(Expr::Field { base, name }, range)
            }
            ast::Expr::Call(e) => {
                let callee = match e.callee() {
                    Some(c) => self.lower_expr(&c),
                    None => self.add_expr(Expr::Missing, range),
                };
                let args = e
                    .args()
                    .map(|list| list.args().map(|a| self.lower_expr(&a)).collect())
                    .unwrap_or_default();
                self.add_call(Expr::Call { callee, args }, range)
            }
            ast::Expr::Pipe(e) => self.lower_pipe(e, range),
            ast::Expr::Flow(e) => self.lower_flow(e, range),
            ast::Expr::Bin(e) => self.lower_binary(e, range),
            ast::Expr::Assign(e) => {
                let target = match e.target() {
                    Some(t) => self.lower_expr(&t),
                    None => self.add_expr(Expr::Missing, range),
                };
                self.check_assignable(target, range);
                let value = match e.value() {
                    Some(v) => self.lower_expr(&v),
                    None => self.add_expr(Expr::Missing, range),
                };
                self.add_expr(Expr::Assign { target, value }, range)
            }
            ast::Expr::Prefix(e) => {
                let op = match e.syntax().first_token().map(|t| t.text().to_string()).as_deref() {
                    Some("-") => UnOp::Neg,
                    _ => UnOp::Not,
                };

                // **`-99.95d` is a decimal literal, minus and all.**
                //
                // A decimal literal is not a value of its own — it desugars to
                // `Decimal::scaled(units, scale)` — so a minus in front of one
                // was an ordinary negation applied to a call, and negation is
                // `Int`, `Float` and the fixed widths. The result was
                //
                //     error: negation: expected `Int`, found `Decimal`
                //     error: this argument: expected `Decimal`, found `Int`
                //
                // for `-99.95d`, while `-99.95` and `-5` compiled in the same
                // program: the one type where a negative number cannot be
                // written is the one for money. Every refund and every credit
                // in somebody's test data was `neg(99.95d)`.
                //
                // Folded into the literal here, which is exactly what the
                // checker already does for a fixed-width integer — `-128` is
                // an `I8` because the minus belongs to the number, not to an
                // operation on it. `Decimal::negate` remains the way to negate
                // a decimal that is not a literal.
                if op == UnOp::Neg {
                    if let Some(operand) = e.operand() {
                        let is_decimal = operand
                            .syntax()
                            .first_token()
                            .is_some_and(|t| t.kind() == khora_syntax::SyntaxKind::DECIMAL_LIT);
                        if is_decimal {
                            let text = operand.syntax().text().to_string();
                            return self.lower_decimal(&format!("-{text}"), range);
                        }
                    }
                }

                let operand = match e.operand() {
                    Some(o) => self.lower_expr(&o),
                    None => self.add_expr(Expr::Missing, range),
                };
                self.add_expr(Expr::Unary { op, operand }, range)
            }
            ast::Expr::If(e) => {
                let condition = match e.condition() {
                    Some(c) => self.lower_expr(&c),
                    None => self.add_expr(Expr::Missing, range),
                };
                let then_branch = match e.then_branch() {
                    Some(b) => self.lower_block(&b),
                    None => self.add_expr(Expr::Missing, range),
                };
                let else_branch = e.else_branch().map(|b| self.lower_expr(&b));
                self.add_expr(Expr::If { condition, then_branch, else_branch }, range)
            }
            ast::Expr::Match(e) => self.lower_match(e, range),
            ast::Expr::While(e) => {
                let condition = match e.condition() {
                    Some(c) => self.lower_expr(&c),
                    None => self.add_expr(Expr::Missing, range),
                };
                self.loop_depth += 1;
                let body = match e.body() {
                    Some(b) => self.lower_block(&b),
                    None => self.add_expr(Expr::Missing, range),
                };
                self.loop_depth -= 1;
                self.add_expr(Expr::While { condition, body }, range)
            }
            ast::Expr::Loop(e) => {
                self.loop_depth += 1;
                let body = match e.body() {
                    Some(b) => self.lower_block(&b),
                    None => self.add_expr(Expr::Missing, range),
                };
                self.loop_depth -= 1;
                self.add_expr(Expr::Loop { body }, range)
            }
            ast::Expr::Break(e) => {
                if self.loop_depth == 0 {
                    self.error("`break` outside a loop", range);
                }
                let value = e.value().map(|v| self.lower_expr(&v));
                self.add_expr(Expr::Break(value), range)
            }
            ast::Expr::Continue(_) => {
                if self.loop_depth == 0 {
                    self.error("`continue` outside a loop", range);
                }
                self.add_expr(Expr::Continue, range)
            }
            ast::Expr::Return(e) => {
                let value = e.value().map(|v| self.lower_expr(&v));
                self.add_expr(Expr::Return(value), range)
            }
            ast::Expr::List(e) => {
                let items = e.syntax().children().filter_map(ast::Expr::cast).collect::<Vec<_>>();
                let ids: Vec<ExprId> = items.iter().map(|i| self.lower_expr(i)).collect();
                self.lower_list(ids, range)
            }
            ast::Expr::Tuple(e) => {
                let items = e.syntax().children().filter_map(ast::Expr::cast).collect::<Vec<_>>();
                let ids = items.iter().map(|i| self.lower_expr(i)).collect();
                self.add_expr(Expr::Tuple(ids), range)
            }
            ast::Expr::Placeholder(_) => {
                self.error("`_` is only meaningful in a pipeline stage", range);
                self.add_expr(Expr::Missing, range)
            }
            // Outside the phase 2 subset. Named individually so a later phase
            // can find every site.
            ast::Expr::For(e) => self.lower_for(e, range),
            ast::Expr::Lambda(e) => self.lower_lambda(e, range),
            ast::Expr::Record(e) => {
                let fields = self.lower_record_fields(e);
                self.add_expr(Expr::Record { owner: None, fields }, range)
            }
            ast::Expr::Raise(e) => {
                let error = match e.value() {
                    Some(v) => self.lower_expr(&v),
                    None => self.add_expr(Expr::Missing, range),
                };
                self.add_expr(Expr::Raise(error), range)
            }
            ast::Expr::Try(e) => {
                let inner = match e.operand() {
                    Some(v) => self.lower_expr(&v),
                    None => self.add_expr(Expr::Missing, range),
                };
                self.add_expr(Expr::Try(inner), range)
            }
            // `handler for Ledger { .. }` is a record literal whose type the
            // syntax names. Nothing about it is special at runtime: it builds
            // the same object a bare literal would.
            ast::Expr::Handler(e) => {
                let owner = e.effect().map(|p| p.text_path());
                let fields = e
                    .operations()
                    .map(|r| self.lower_record_fields(&r))
                    .unwrap_or_default();
                self.add_expr(Expr::Record { owner, fields }, range)
            }
            ast::Expr::Catch(e) => {
                let inner = match e.operand() {
                    Some(inner) => self.lower_expr(&inner),
                    None => self.add_expr(Expr::Missing, range),
                };
                let arms = self.lower_arms(e.arms(), range);
                self.add_expr(Expr::Catch { inner, arms }, range)
            }
            // `with { ledger: h } { body }` and `body with { ledger: h }`
            // are one thing: a region in which the labels are bound. That is
            // an ordinary block of `let`s, which is why installation needs no
            // runtime of its own and why an inner `with` shadows an outer one.
            ast::Expr::With(e) => {
                let body = e.body();
                // `expr with Mock` names a context, the same as the block form
                // does — and `expr with Mock { ai: stub }` names one *and*
                // overrides part of it. The context's bindings come first and
                // the written ones after, so the ordinary rule decides: later
                // shadows earlier, exactly as it does inside one row.
                let contexts: Vec<ast::Path> = e.contexts().collect();
                let (mut row, by_type) = self.installed_paths(contexts.into_iter());
                row.extend(fields_of(e.row().as_ref()));
                self.lower_installation(row, by_type, body.as_ref(), range)
            }
            ast::Expr::WithBlock(e) => {
                let body = e.body().map(ast::Expr::Block);
                let contexts: Vec<ast::Path> = e.contexts().collect();
                let (row, by_type) = if contexts.is_empty() {
                    (fields_of(e.row().as_ref()), Vec::new())
                } else {
                    self.installed_paths(contexts.into_iter())
                };
                self.lower_installation(row, by_type, body.as_ref(), range)
            }
        }
    }

    pub(super) fn lower_path_expr(&mut self, e: &ast::PathExpr, range: TextRange) -> ExprId {
        let segments: Vec<String> = e
            .syntax()
            .children()
            .find_map(ast::Path::cast)
            .map(|p| p.segments().filter_map(|s| s.ident()).collect())
            .unwrap_or_default();
        self.lower_segments(segments, range)
    }

    /// The same resolution, from a bare `Path` rather than a `PathExpr`.
    ///
    /// `with MyDatabase { .. }` has only the path — the grammar puts no
    /// expression around it — and installing a handler value has to resolve
    /// that name exactly as any other mention of it would.
    pub(super) fn lower_path(&mut self, path: &ast::Path, range: TextRange) -> ExprId {
        let segments: Vec<String> =
            path.segments().filter_map(|s| s.ident()).collect();
        self.lower_segments(segments, range)
    }

    fn lower_segments(&mut self, segments: Vec<String>, range: TextRange) -> ExprId {
        // A bare name is a local first — shadowing is what people expect.
        if let [only] = segments.as_slice() {
            if let Some(local) = self.lookup(only) {
                return self.add_expr(Expr::Local(local), range);
            }
            if self.is_own_lambda(only) {
                return self.add_expr(Expr::LambdaSelf, range);
            }
            if self.is_enclosing_lambda(only) {
                // Reaching an *outer* lambda from an inner one would capture
                // it, and a closure that holds another closure holding it is a
                // cycle. Named functions have no closure object, so recursion
                // through one costs nothing.
                self.error(
                    format!(
                        "`{only}` is a closure being defined further out; only a closure's own \
                         name is in scope inside it. Use a named `fn` for mutual recursion"
                    ),
                    range,
                );
                return self.add_expr(Expr::Unresolved(only.clone()), range);
            }
            // A module-level `let` is a constant, so a mention of one *is* its
            // initializer. Checked before the item table, because the item is
            // only there to make the name resolve and to catch duplicates —
            // there is no function to call and no global to load.
            if let Some(expanded) = self.expand_constant(only, range) {
                return expanded;
            }
            if let Some(item) = self.map.item(only) {
                return self.add_expr(
                    Expr::Path(crate::Resolution::Item {
                        module: self
                            .map
                            .module
                            .clone()
                            .unwrap_or_else(|| crate::ModulePath::new(vec![])),
                        name: item.name.clone(),
                        kind: item.kind,
                    }),
                    range,
                );
            }
            if let Some(resolution) = self.scope.get(only) {
                return self.add_expr(Expr::Path(resolution.clone()), range);
            }
            self.error(format!("cannot find `{only}` in this scope"), range);
            return self.add_expr(Expr::Unresolved(only.clone()), range);
        }

        // `F::pure` or `Applicative::pure`. Checked before module paths
        // because a type parameter in scope, or a trait declared here, is a
        // more specific reading than a module that happens to share the name.
        if let [owner, name] = segments.as_slice() {
            let is_param = self.generics.iter().any(|g| g == owner);
            let is_trait =
                self.map.item(owner).is_some_and(|i| i.kind == crate::ItemKind::Trait);
            if is_param || is_trait {
                return self.add_expr(
                    Expr::Path(crate::Resolution::TraitItem {
                        owner: owner.clone(),
                        name: name.clone(),
                    }),
                    range,
                );
            }
        }

        // A `::` path names a constructor or an item in another module.
        if let [type_name, case] = segments.as_slice() {
            if let Some(v) = self
                .map
                .variants_of(type_name)
                .chain(self.scope.variants_of(type_name))
                .find(|v| &v.name == case)
            {
                return self.add_expr(
                    Expr::Path(crate::Resolution::Variant {
                        module: self.home_of_type(type_name),
                        type_name: v.type_name.clone(),
                        name: v.name.clone(),
                    }),
                    range,
                );
            }
        }

        // A function the type declares for itself: `User::new()`. After the
        // constructors, so `Option::Some` still means the constructor even if
        // an `impl Option` happens to declare a `Some`. Which function it is
        // stays the checker's question, exactly as for `Applicative::pure` —
        // hence the same resolution.
        if let [owner, name] = segments.as_slice() {
            // An `effect` names a type too — the type of its handlers — so
            // `Scope::root()` reads the same way `Router::new()` does.
            let is_type = |kind: crate::ItemKind| {
                matches!(kind, crate::ItemKind::Type | crate::ItemKind::Effect)
            };
            let declared_here = self.map.item(owner).is_some_and(|i| is_type(i.kind))
                || crate::BUILTIN_TYPES.contains(&owner.as_str());
            let imported = matches!(
                self.scope.get(owner),
                Some(crate::Resolution::Item { kind, .. }) if is_type(*kind)
            );
            if declared_here || imported {
                return self.add_expr(
                    Expr::Path(crate::Resolution::TraitItem {
                        owner: owner.clone(),
                        name: name.clone(),
                    }),
                    range,
                );
            }
        }

        // Cross-module resolution needs the module graph, which is a source
        // root away; phase 2 programs are single-module.
        self.error(
            format!("cannot resolve `{}` in this scope", segments.join("::")),
            range,
        );
        self.add_expr(Expr::Unresolved(segments.join("::")), range)
    }

    /// The labelled expressions of a record literal.
    pub(super) fn lower_record_fields(&mut self, e: &ast::RecordExpr) -> Vec<(String, ExprId)> {
        e.fields()
            .filter_map(|f| {
                let label = f.name()?.ident()?;
                let range = f.syntax().text_range();
                let value = match f.value() {
                    Some(v) => self.lower_expr(&v),
                    None => self.add_expr(Expr::Missing, range),
                };
                Some((label, value))
            })
            .collect()
    }

    pub(super) fn check_assignable(&mut self, target: ExprId, range: TextRange) {
        match self.body.exprs[target.index()] {
            Expr::Local(local) => {
                // A capture is a copy, so writing to it would change the
                // closure's own copy and nothing else — silently, which is the
                // worst way for it to behave. Saying so is better than either
                // lying or quietly promoting the capture to a shared cell.
                if self.is_captured(local) {
                    let name = self.body.locals[local.index()].name.clone();
                    self.error(
                        format!(
                            "cannot assign to `{name}` inside a closure: it is captured by \
                             value, so the assignment would not be visible outside"
                        ),
                        range,
                    );
                    return;
                }
                let local = &self.body.locals[local.index()];
                if !local.is_mut {
                    let name = local.name.clone();
                    self.error(
                        format!("cannot assign to `{name}`, which is not declared `mut`"),
                        range,
                    );
                }
            }
            Expr::Field { .. } => {}
            Expr::Missing => {}
            _ => self.error("this expression cannot be assigned to", range),
        }
    }

    pub(super) fn lower_binary(&mut self, e: &ast::BinExpr, range: TextRange) -> ExprId {
        let op = e.op().and_then(|t| BinOp::from_token(t.text()));
        let lhs = match e.lhs() {
            Some(l) => self.lower_expr(&l),
            None => self.add_expr(Expr::Missing, range),
        };
        let rhs = match e.rhs() {
            Some(r) => self.lower_expr(&r),
            None => self.add_expr(Expr::Missing, range),
        };
        match op {
            Some(op) => self.add_expr(Expr::Binary { op, lhs, rhs }, range),
            None => self.add_expr(Expr::Missing, range),
        }
    }

    /// `x |> f(a)` becomes `f(x, a)`; `x |> f(_, a)` becomes `f(x, a)` with the
    /// placeholder taking the piped value instead of the leading position.
    ///
    /// The pipeline exists only in the syntax; nothing downstream needs to know
    /// a call was written this way.
    pub(super) fn lower_pipe(&mut self, e: &ast::PipeExpr, range: TextRange) -> ExprId {
        let piped = match e.lhs() {
            Some(l) => self.lower_expr(&l),
            None => self.add_expr(Expr::Missing, range),
        };

        let Some(rhs) = e.rhs() else {
            return self.add_expr(Expr::Missing, range);
        };

        self.pipe_into(piped, &rhs, range)
    }

    /// One pipeline stage, over a value that is already lowered.
    ///
    /// Split out of [`Self::lower_pipe`] for the flow operator, whose first
    /// stage pipes from a parameter the source never wrote. **Sharing this
    /// function is what makes `||> a |> b` and `fn x => x |> a |> b` the same
    /// program** rather than two things that happen to agree today: call
    /// insertion, the `_` placeholder, and where a `!` ends up are decided
    /// here, once.
    pub(super) fn pipe_into(
        &mut self,
        piped: ExprId,
        rhs: &ast::Expr,
        range: TextRange,
    ) -> ExprId {
        let rhs = rhs.clone();

        // `x |> f(y)!` means `f(x, y)!`. The other reading — `x |> (f(y)!)` —
        // is never meaningful: `f(y)` on the right of a pipe is a call with a
        // hole in it, so a `!` there has nothing to mark. Unwrapping the mark
        // here and putting it back around the finished call is what the reader
        // means and is where the branch actually belongs.
        let (rhs, marked) = match &rhs {
            ast::Expr::Try(t) => match t.operand() {
                Some(inner) => (inner, true),
                None => (rhs, false),
            },
            _ => (rhs, false),
        };
        let mark = |ctx: &mut Self, call: ExprId| {
            if marked { ctx.add_expr(Expr::Try(call), range) } else { call }
        };

        match &rhs {
            ast::Expr::Call(call) => {
                let callee = match call.callee() {
                    Some(c) => self.lower_expr(&c),
                    None => self.add_expr(Expr::Missing, range),
                };

                let written: Vec<ast::Expr> =
                    call.args().map(|l| l.args().collect()).unwrap_or_default();
                let placeholders: Vec<usize> = written
                    .iter()
                    .enumerate()
                    .filter(|(_, a)| matches!(a, ast::Expr::Placeholder(_)))
                    .map(|(i, _)| i)
                    .collect();

                if placeholders.len() > 1 {
                    self.error(
                        "a pipeline stage may use `_` at most once".to_string(),
                        rhs.syntax().text_range(),
                    );
                }

                let mut args = Vec::with_capacity(written.len() + 1);
                if placeholders.is_empty() {
                    args.push(piped);
                    for a in &written {
                        let id = self.lower_expr(a);
                        args.push(id);
                    }
                } else {
                    let first = placeholders[0];
                    for (i, a) in written.iter().enumerate() {
                        if i == first {
                            args.push(piped);
                        } else {
                            let id = self.lower_expr(a);
                            args.push(id);
                        }
                    }
                }
                let call = self.add_call(Expr::Call { callee, args }, range);
                mark(self, call)
            }
            // `x |> f` is `f(x)`.
            _ => {
                let callee = self.lower_expr(&rhs);
                let call = self.add_call(Expr::Call { callee, args: vec![piped] }, range);
                mark(self, call)
            }
        }
    }
}
