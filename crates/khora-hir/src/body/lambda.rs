//! Lambdas, and what they capture.
//!
//! A capture is a local declared outside the lambda, which the mark taken when
//! it started makes cheap to decide. The subtle case is a lambda naming
//! *itself*: that resolves to `LambdaSelf` and compiles to the closure the call
//! came through, so direct recursion needs no capture and makes no cycle.
//! `docs/design/memory.md` §3.

use super::*;

impl<'a> Ctx<'a> {
    /// `(a, b) => body`.
    ///
    /// Captures are found by scanning the expressions the body created rather
    /// than by walking its tree. Lowering is depth-first and the arena is
    /// append-only, so every expression belonging to this lambda — including
    /// ones inside a lambda nested in it — sits above the mark taken here. That
    /// makes nested capture fall out for free: an inner lambda's free variable
    /// is still free in the outer one, and one scan finds both.
    pub(super) fn lower_lambda(&mut self, e: &ast::LambdaExpr, range: TextRange) -> ExprId {
        self.lower_lambda_named(e, None, range)
    }

    /// As `lower_lambda`, with the name the lambda was bound to if it has one.
    pub(super) fn lower_lambda_named(
        &mut self,
        e: &ast::LambdaExpr,
        name: Option<String>,
        range: TextRange,
    ) -> ExprId {
        let local_mark = self.body.locals.len();
        let expr_mark = self.body.exprs.len();

        self.scopes.push(Vec::new());
        self.lambdas.push(local_mark);
        self.lambda_names.push(name);

        let declared: Vec<ast::Param> =
            e.params().map(|list| list.params().collect()).unwrap_or_default();
        let param_types: Vec<Option<TypeRef>> =
            declared.iter().map(|p| p.ty().as_ref().map(TypeRef::of_syntax)).collect();
        let params: Vec<PatId> = declared
            .iter()
            .map(|p| {
                let range = p.syntax().text_range();
                match p.name().and_then(|n| n.ident()) {
                    Some(name) => {
                        let local = self.declare(name, false, range);
                        self.add_pat(Pat::Bind(local), range)
                    }
                    None => self.add_pat(Pat::Wildcard, range),
                }
            })
            .collect();

        // A lambda is its own function as far as `return` and `break` are
        // concerned: `return` leaves the lambda, and a `break` inside one
        // cannot target a loop outside it.
        let outer_loops = std::mem::take(&mut self.loop_depth);
        let body = match e.body() {
            Some(b) => self.lower_expr(&b),
            None => self.add_expr(Expr::Missing, range),
        };
        self.loop_depth = outer_loops;

        self.lambdas.pop();
        self.lambda_names.pop();
        self.scopes.pop();

        let mut captures: Vec<LocalId> = Vec::new();
        for expr in &self.body.exprs[expr_mark..] {
            if let Expr::Local(local) = expr {
                if local.index() < local_mark && !captures.contains(local) {
                    captures.push(*local);
                }
            }
        }

        let id = self.add_expr(Expr::Lambda { params, param_types, body, captures }, range);
        self.body.lambda_marks.insert(id, local_mark);
        id
    }

    /// `||> a |> b |> c` — the flow operator.
    ///
    /// Desugars to `fn x => x |> a |> b |> c` and then stops existing. Nothing
    /// past this function knows a flow was written: inference, effect rows,
    /// failure rows, capture analysis, monomorphization and code generation
    /// all see the lambda they would have seen if somebody had typed it.
    ///
    /// The scaffolding is `lower_lambda_named`'s, deliberately duplicated
    /// rather than shared, because the two differ in exactly the interesting
    /// place: a lambda reads its parameters out of the source and this one
    /// invents its only parameter. Threading an "invent a parameter" flag
    /// through the other function would hide that difference in a boolean.
    ///
    /// **The parameter's name cannot collide with anything.** A space is not
    /// valid in an identifier, so no source can declare `flow value` and no
    /// source can refer to it; the name exists so that a debugger and an error
    /// message have something readable to show. The reference is built here as
    /// an `Expr::Local` rather than resolved by name, so nothing looks it up
    /// anyway.
    pub(super) fn lower_flow(&mut self, e: &ast::FlowExpr, range: TextRange) -> ExprId {
        let local_mark = self.body.locals.len();
        let expr_mark = self.body.exprs.len();

        self.scopes.push(Vec::new());
        self.lambdas.push(local_mark);
        self.lambda_names.push(None);

        let local = self.declare("flow value".to_string(), false, range);
        let params = vec![self.add_pat(Pat::Bind(local), range)];

        let outer_loops = std::mem::take(&mut self.loop_depth);
        let mut value = self.add_expr(Expr::Local(local), range);
        let mut stages = 0;
        for stage in e.stages() {
            value = self.pipe_into(value, &stage, stage.syntax().text_range());
            stages += 1;
        }
        if stages == 0 {
            // The parser has already said what was wrong. Producing the
            // identity rather than `Missing` keeps one bad flow from becoming
            // a second error everywhere its result is used.
            value = self.add_expr(Expr::Missing, range);
        }
        self.loop_depth = outer_loops;

        self.lambdas.pop();
        self.lambda_names.pop();
        self.scopes.pop();

        let mut captures: Vec<LocalId> = Vec::new();
        for expr in &self.body.exprs[expr_mark..] {
            if let Expr::Local(found) = expr {
                if found.index() < local_mark && !captures.contains(found) {
                    captures.push(*found);
                }
            }
        }

        let id = self.add_expr(
            Expr::Lambda { params, param_types: vec![None], body: value, captures },
            range,
        );
        self.body.lambda_marks.insert(id, local_mark);
        id
    }

    /// Whether `name` is the name of the lambda currently being lowered.
    pub(super) fn is_own_lambda(&self, name: &str) -> bool {
        matches!(self.lambda_names.last(), Some(Some(own)) if own == name)
    }

    /// Whether `name` is the name of a lambda further out than the innermost.
    pub(super) fn is_enclosing_lambda(&self, name: &str) -> bool {
        let outer = self.lambda_names.len().saturating_sub(1);
        self.lambda_names[..outer].iter().any(|n| n.as_deref() == Some(name))
    }

    /// Whether `local` belongs to a scope outside the lambda being lowered.
    pub(super) fn is_captured(&self, local: LocalId) -> bool {
        self.lambdas.last().is_some_and(|mark| local.index() < *mark)
    }
}
