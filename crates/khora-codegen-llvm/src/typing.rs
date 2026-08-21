//! The type of every expression in one body.
//!
//! # Why this exists
//!
//! `khora-types` computes exactly this table while checking, and then throws it
//! away: [`khora_types::check_file`] returns diagnostics, and
//! [`khora_types::TypeMap`] carries only signatures and ADT shapes. Code
//! generation cannot work without per-expression types — a `+` is `add i64` or
//! a type error depending on them, a `let` slot is `i64` or `ptr` depending on
//! them, and `print` picks its runtime entry point from them.
//!
//! So the walk is repeated here. That is a duplication worth removing: the
//! natural fix is for `khora-types` to expose its inference table as a query,
//! which phase 3 needs anyway once inference replaces checking. Until then this
//! module mirrors `Checker::infer` and must stay in step with it.
//!
//! It is deliberately silent. Every disagreement it could report has already
//! been reported by the checker, and reporting them twice would double every
//! diagnostic in the compiler's most user-visible surface.

use std::collections::HashMap;

use khora_hir::body::{BinOp, Body, Expr, ExprId, Literal, LocalId, Pat, PatId, Stmt, UnOp};
use khora_types::{Signature, Type, TypeMap};

/// Types for one function body.
pub struct BodyTypes {
    exprs: Vec<Type>,
    locals: HashMap<LocalId, Type>,
}

impl BodyTypes {
    /// The type of an expression.
    pub fn of(&self, id: ExprId) -> &Type {
        &self.exprs[id.index()]
    }

    /// The type of a local binding.
    ///
    /// Unbound locals cannot occur in a checked program, but returning
    /// `Unknown` rather than panicking keeps a front-end bug a diagnostic
    /// instead of a crash.
    pub fn local(&self, id: LocalId) -> &Type {
        self.locals.get(&id).unwrap_or(&Type::Unknown)
    }

    /// Infers types for a body that has already been checked.
    pub fn infer(types: &TypeMap, body: &Body, signature: &Signature) -> BodyTypes {
        let mut inferrer = Inferrer {
            types,
            body,
            exprs: vec![Type::Unknown; body.expr_count()],
            locals: HashMap::new(),
        };

        for (i, pat) in body.params.iter().enumerate() {
            let ty = signature.params.get(i).cloned().unwrap_or(Type::Unknown);
            inferrer.bind(*pat, &ty);
        }
        if let Some(root) = body.root {
            inferrer.visit(root);
        }

        BodyTypes { exprs: inferrer.exprs, locals: inferrer.locals }
    }
}

struct Inferrer<'a> {
    types: &'a TypeMap,
    body: &'a Body,
    exprs: Vec<Type>,
    locals: HashMap<LocalId, Type>,
}

impl Inferrer<'_> {
    fn record(&mut self, id: ExprId, ty: Type) -> Type {
        self.exprs[id.index()] = ty.clone();
        ty
    }

    fn bind(&mut self, pat: PatId, ty: &Type) {
        match self.body.pat(pat).clone() {
            Pat::Bind(local) => {
                self.locals.insert(local, ty.clone());
            }
            Pat::TupleStruct { resolution, fields } => {
                let variant = variant_name(&resolution)
                    .and_then(|n| self.types.variants.iter().find(|v| v.name == n).cloned());
                for (i, field) in fields.iter().enumerate() {
                    let field_ty = variant
                        .as_ref()
                        .and_then(|v| v.fields.get(i).cloned())
                        .unwrap_or(Type::Unknown);
                    self.bind(*field, &field_ty);
                }
            }
            Pat::Tuple(fields) => {
                for field in fields {
                    self.bind(field, &Type::Unknown);
                }
            }
            Pat::Wildcard | Pat::Literal(_) | Pat::Path(_) | Pat::Missing => {}
        }
    }

    fn visit(&mut self, id: ExprId) -> Type {
        let ty = match self.body.expr(id).clone() {
            Expr::Missing | Expr::Unsupported(_) | Expr::Unresolved(_) => Type::Unknown,
            Expr::Unit => Type::Unit,
            Expr::Literal(lit) => match lit {
                Literal::Int(_) => Type::Int,
                Literal::Float(_) => Type::Unknown,
                Literal::Str(_) => Type::Str,
                Literal::Bool(_) => Type::Bool,
            },
            Expr::Local(local) => self.local_type(local),
            Expr::Path(resolution) => self.resolution_type(&resolution),
            Expr::Field { base, .. } => {
                self.visit(base);
                Type::Unknown
            }
            Expr::Unary { op, operand } => {
                self.visit(operand);
                match op {
                    UnOp::Neg => Type::Int,
                    UnOp::Not => Type::Bool,
                }
            }
            Expr::Binary { op, lhs, rhs } => {
                let left = self.visit(lhs);
                self.visit(rhs);
                match op {
                    // `+` is overloaded on `String`, so the result follows the
                    // left operand rather than being `Int` by construction.
                    BinOp::Add if matches!(left, Type::Str) => Type::Str,
                    BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Rem => Type::Int,
                    _ => Type::Bool,
                }
            }
            Expr::Assign { target, value } => {
                self.visit(target);
                self.visit(value);
                Type::Unit
            }
            Expr::Call { callee, args } => self.call_type(callee, &args),
            Expr::Block { stmts, tail } => {
                // A diverging statement makes the whole block diverge, exactly
                // as the checker decides it — otherwise an `if` whose branch
                // returns would look like it disagreed with the other branch.
                let mut diverged = false;
                for stmt in stmts {
                    match stmt {
                        Stmt::Let { pat, init } => {
                            let ty = init.map(|e| self.visit(e)).unwrap_or(Type::Unknown);
                            diverged |= matches!(ty, Type::Never);
                            self.bind(pat, &ty);
                        }
                        Stmt::Expr(e) => diverged |= matches!(self.visit(e), Type::Never),
                    }
                }
                let tail_ty = tail.map(|t| self.visit(t)).unwrap_or(Type::Unit);
                if diverged {
                    Type::Never
                } else {
                    tail_ty
                }
            }
            Expr::If { condition, then_branch, else_branch } => {
                self.visit(condition);
                let then_ty = self.visit(then_branch);
                match else_branch {
                    Some(else_id) => {
                        let else_ty = self.visit(else_id);
                        if matches!(then_ty, Type::Never | Type::Unknown) {
                            else_ty
                        } else {
                            then_ty
                        }
                    }
                    None => Type::Unit,
                }
            }
            Expr::While { condition, body } => {
                self.visit(condition);
                self.visit(body);
                Type::Unit
            }
            Expr::Loop { body } => {
                self.visit(body);
                // A `loop` yields whatever `break` carries, which phase 2 does
                // not track. The backend rejects a `loop` used as a value.
                Type::Unknown
            }
            Expr::Break(value) => {
                if let Some(v) = value {
                    self.visit(v);
                }
                Type::Never
            }
            Expr::Continue => Type::Never,
            Expr::Return(value) => {
                if let Some(v) = value {
                    self.visit(v);
                }
                Type::Never
            }
            Expr::List(items) | Expr::Tuple(items) => {
                for item in items {
                    self.visit(item);
                }
                Type::Unknown
            }
            Expr::Match { scrutinee, arms } => {
                let scrutinee_ty = self.visit(scrutinee);
                let mut result = Type::Never;
                for arm in &arms {
                    self.bind(arm.pat, &scrutinee_ty);
                    if let Some(guard) = arm.guard {
                        self.visit(guard);
                    }
                    let arm_ty = self.visit(arm.body);
                    if matches!(result, Type::Never) {
                        result = arm_ty;
                    }
                }
                result
            }
        };
        self.record(id, ty)
    }

    fn local_type(&self, local: LocalId) -> Type {
        self.locals.get(&local).cloned().unwrap_or(Type::Unknown)
    }

    fn resolution_type(&self, resolution: &khora_hir::Resolution) -> Type {
        match resolution {
            khora_hir::Resolution::Item { name, .. } => self
                .types
                .signatures
                .get(name)
                .map(|s| Type::Fn { params: s.params.clone(), ret: Box::new(s.ret.clone()) })
                .unwrap_or(Type::Unknown),
            khora_hir::Resolution::Variant { type_name, .. } => Type::Adt(type_name.clone()),
            khora_hir::Resolution::Unsupported(_) => Type::Unknown,
        }
    }

    fn call_type(&mut self, callee: ExprId, args: &[ExprId]) -> Type {
        let callee_ty = self.visit(callee);
        for arg in args {
            self.visit(*arg);
        }
        match callee_ty {
            // A constructor call builds its ADT; the path itself already has
            // the ADT's type, so applying it does not change it.
            Type::Adt(name) => Type::Adt(name),
            Type::Fn { ret, .. } => *ret,
            _ => Type::Unknown,
        }
    }
}

fn variant_name(resolution: &khora_hir::Resolution) -> Option<String> {
    match resolution {
        khora_hir::Resolution::Variant { name, .. } => Some(name.clone()),
        _ => None,
    }
}
