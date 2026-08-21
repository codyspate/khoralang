//! Type checking for the phase 2 subset.
//!
//! Not inference yet. Phase 2 is monomorphic, so this checks bodies against
//! declared signatures rather than solving for them; Algorithm W arrives in
//! phase 3 and row unification in phase 4. [`Type`] is shaped with those in
//! view — the variants they need are noted where they will go.

pub mod usefulness;

use std::collections::HashMap;

use khora_db::{Db, SourceFile};
use khora_hir::body::{BinOp, Body, Expr, ExprId, Literal, LocalId, Pat, PatId, Stmt, UnOp};
use khora_hir::HirError;
use khora_syntax::ast::{self};
use text_size::TextRange;
use usefulness::{ColumnType, Ctor, Pattern};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Type {
    Int,
    Bool,
    Str,
    Unit,
    /// A user-declared variant type, by name.
    Adt(String),
    Fn { params: Vec<Type>, ret: Box<Type> },
    /// The type of `return`, `break` and a diverging branch. Compatible with
    /// everything, because control never reaches the consumer.
    Never,
    /// Stands in for an expression whose type could not be determined —
    /// usually downstream of an error already reported. Compatible with
    /// everything, so one mistake does not cascade.
    Unknown,
    // Phase 3 adds Var(u32) and Const(i64); phase 4 adds Row(..).
}

impl std::fmt::Display for Type {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Type::Int => write!(f, "Int"),
            Type::Bool => write!(f, "Bool"),
            Type::Str => write!(f, "String"),
            Type::Unit => write!(f, "()"),
            Type::Adt(name) => write!(f, "{name}"),
            Type::Never => write!(f, "Never"),
            Type::Unknown => write!(f, "?"),
            Type::Fn { params, ret } => {
                let ps: Vec<String> = params.iter().map(|p| p.to_string()).collect();
                write!(f, "({}) -> {ret}", ps.join(", "))
            }
        }
    }
}

impl Type {
    /// Whether a value of `self` is acceptable where `expected` is wanted.
    ///
    /// `Unknown` and `Never` are compatible with anything, for opposite
    /// reasons: the first because we already failed and do not want to fail
    /// twice, the second because control does not arrive.
    fn compatible_with(&self, expected: &Type) -> bool {
        match (self, expected) {
            (Type::Unknown, _) | (_, Type::Unknown) => true,
            (Type::Never, _) | (_, Type::Never) => true,
            (a, b) => a == b,
        }
    }
}

/// A function's declared signature.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Signature {
    pub params: Vec<Type>,
    pub ret: Type,
}

/// A variant of an ADT and the types of its payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VariantInfo {
    pub type_name: String,
    pub name: String,
    pub fields: Vec<Type>,
}

/// Signatures and ADT shapes for one file.
///
/// Read from the syntax tree rather than from `ItemMap`, which records what
/// exists but not what shape it has. Keeping that in one place here avoids
/// growing a HIR type layer before generics force its shape.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TypeMap {
    pub signatures: HashMap<String, Signature>,
    pub variants: Vec<VariantInfo>,
}

impl TypeMap {
    fn variants_of(&self, type_name: &str) -> Vec<&VariantInfo> {
        self.variants.iter().filter(|v| v.type_name == type_name).collect()
    }

    fn variant(&self, name: &str) -> Option<&VariantInfo> {
        self.variants.iter().find(|v| v.name == name)
    }
}

#[salsa::tracked(returns(ref))]
pub fn type_map(db: &dyn Db, file: SourceFile) -> TypeMap {
    let parse = khora_db::parse(db, file);
    let mut map = TypeMap::default();

    for decl in parse.source_file().decls() {
        match decl {
            ast::Decl::Fn(f) => {
                let Some(name) = f.name().and_then(|n| n.ident()) else { continue };
                let params = f
                    .params()
                    .map(|list| list.params().map(|p| type_of_syntax(p.ty().as_ref())).collect())
                    .unwrap_or_default();
                let ret = f.return_type().map_or(Type::Unit, |t| type_of_syntax(Some(&t)));
                map.signatures.insert(name, Signature { params, ret });
            }
            ast::Decl::Type(t) => {
                let Some(type_name) = t.name().and_then(|n| n.ident()) else { continue };
                if let Some(ast::Type::Variant(v)) = t.definition() {
                    for case in v.cases() {
                        let Some(name) = case.name().and_then(|n| n.ident()) else { continue };
                        let fields = case
                            .fields()
                            .map(|list| {
                                list.fields().map(|f| type_of_syntax(f.ty().as_ref())).collect()
                            })
                            .or_else(|| {
                                case.tuple_fields()
                                    .map(|list| list.types().map(|t| type_of_syntax(Some(&t))).collect())
                            })
                            .unwrap_or_default();
                        map.variants.push(VariantInfo {
                            type_name: type_name.clone(),
                            name,
                            fields,
                        });
                    }
                }
            }
            _ => {}
        }
    }
    map
}

/// Maps written syntax to a type. Anything outside the phase 2 subset becomes
/// [`Type::Unknown`], which suppresses follow-on errors.
fn type_of_syntax(ty: Option<&ast::Type>) -> Type {
    let Some(ty) = ty else { return Type::Unknown };
    match ty {
        ast::Type::Unit(_) => Type::Unit,
        ast::Type::Path(p) => {
            let name = p.path().map(|p| p.text_path()).unwrap_or_default();
            match name.as_str() {
                "Int" => Type::Int,
                "Bool" => Type::Bool,
                "String" => Type::Str,
                "" => Type::Unknown,
                other => Type::Adt(other.to_string()),
            }
        }
        _ => Type::Unknown,
    }
}

/// Type errors for one file, and nothing else.
///
/// Kept separate from lowering errors so "does this type-check" stays a
/// question with its own answer; [`diagnostics`] is what a driver wants.
#[salsa::tracked(returns(ref))]
pub fn check_file(db: &dyn Db, file: SourceFile) -> Vec<HirError> {
    let types = type_map(db, file);
    let bodies = khora_hir::body::bodies(db, file);

    let mut errors = Vec::new();
    for (name, body) in bodies {
        let signature = types.signatures.get(name).cloned().unwrap_or(Signature {
            params: Vec::new(),
            ret: Type::Unknown,
        });
        let mut checker = Checker {
            types,
            body,
            signature: &signature,
            locals: HashMap::new(),
            errors: Vec::new(),
        };
        checker.check_function();
        errors.extend(checker.errors);
    }
    errors
}

struct Checker<'a> {
    types: &'a TypeMap,
    body: &'a Body,
    signature: &'a Signature,
    locals: HashMap<LocalId, Type>,
    errors: Vec<HirError>,
}

impl<'a> Checker<'a> {
    fn error(&mut self, message: impl Into<String>, range: TextRange) {
        self.errors.push(HirError { message: message.into(), range });
    }

    fn check_function(&mut self) {
        for (i, pat) in self.body.params.iter().enumerate() {
            let ty = self.signature.params.get(i).cloned().unwrap_or(Type::Unknown);
            self.bind_pattern(*pat, &ty);
        }

        let Some(root) = self.body.root else { return };
        let actual = self.infer(root);
        let expected = self.signature.ret.clone();
        if !actual.compatible_with(&expected) {
            self.error(
                format!("this function returns `{expected}`, but its body has type `{actual}`"),
                self.body.range(root),
            );
        }
    }

    /// Records the type of every binding a pattern introduces.
    fn bind_pattern(&mut self, pat: PatId, ty: &Type) {
        match self.body.pat(pat).clone() {
            Pat::Bind(local) => {
                self.locals.insert(local, ty.clone());
            }
            Pat::TupleStruct { resolution, fields } => {
                let variant = variant_name(&resolution).and_then(|n| self.types.variant(&n)).cloned();
                for (i, field) in fields.iter().enumerate() {
                    let field_ty = variant
                        .as_ref()
                        .and_then(|v| v.fields.get(i).cloned())
                        .unwrap_or(Type::Unknown);
                    self.bind_pattern(*field, &field_ty);
                }
            }
            Pat::Tuple(fields) => {
                for field in fields {
                    self.bind_pattern(field, &Type::Unknown);
                }
            }
            Pat::Wildcard | Pat::Literal(_) | Pat::Path(_) | Pat::Missing => {}
        }
    }

    fn expect(&mut self, id: ExprId, expected: &Type, context: &str) -> Type {
        let actual = self.infer(id);
        if !actual.compatible_with(expected) {
            self.error(
                format!("{context} expects `{expected}`, found `{actual}`"),
                self.body.range(id),
            );
        }
        actual
    }

    fn infer(&mut self, id: ExprId) -> Type {
        let range = self.body.range(id);
        match self.body.expr(id).clone() {
            Expr::Missing | Expr::Unsupported(_) | Expr::Unresolved(_) => Type::Unknown,
            Expr::Unit => Type::Unit,
            Expr::Literal(lit) => match lit {
                Literal::Int(_) => Type::Int,
                Literal::Float(_) => Type::Unknown,
                Literal::Str(_) => Type::Str,
                Literal::Bool(_) => Type::Bool,
            },
            Expr::Local(local) => self.locals.get(&local).cloned().unwrap_or(Type::Unknown),
            Expr::Path(resolution) => self.type_of_resolution(&resolution),
            Expr::Field { base, .. } => {
                self.infer(base);
                Type::Unknown
            }
            Expr::Unary { op, operand } => match op {
                UnOp::Neg => self.expect(operand, &Type::Int, "negation"),
                UnOp::Not => self.expect(operand, &Type::Bool, "`!`"),
            },
            Expr::Binary { op, lhs, rhs } => self.infer_binary(op, lhs, rhs),
            Expr::Assign { target, value } => {
                let target_ty = self.infer(target);
                self.expect(value, &target_ty, "this assignment");
                Type::Unit
            }
            Expr::Call { callee, args } => self.infer_call(callee, &args, range),
            Expr::Block { stmts, tail } => {
                // A statement that diverges makes the whole block diverge:
                // `{ return 0; }` has type Never, not `()`, or an `if` whose
                // branch returns would wrongly disagree with the other branch.
                let mut diverged = false;
                for stmt in stmts {
                    match stmt {
                        Stmt::Let { pat, init } => {
                            let ty = init.map(|e| self.infer(e)).unwrap_or(Type::Unknown);
                            diverged |= matches!(ty, Type::Never);
                            self.bind_pattern(pat, &ty);
                        }
                        Stmt::Expr(e) => {
                            diverged |= matches!(self.infer(e), Type::Never);
                        }
                    }
                }
                let tail_ty = tail.map(|t| self.infer(t)).unwrap_or(Type::Unit);
                if diverged {
                    Type::Never
                } else {
                    tail_ty
                }
            }
            Expr::If { condition, then_branch, else_branch } => {
                self.expect(condition, &Type::Bool, "an `if` condition");
                let then_ty = self.infer(then_branch);
                match else_branch {
                    Some(else_id) => {
                        let else_ty = self.infer(else_id);
                        if !then_ty.compatible_with(&else_ty) {
                            self.error(
                                format!(
                                    "`if` branches disagree: one is `{then_ty}`, the other `{else_ty}`"
                                ),
                                range,
                            );
                            return Type::Unknown;
                        }
                        if matches!(then_ty, Type::Never) { else_ty } else { then_ty }
                    }
                    // Without an `else`, the branch is only well typed if it
                    // produces nothing — the same rule `match` follows.
                    None => {
                        if !then_ty.compatible_with(&Type::Unit) {
                            self.error(
                                format!(
                                    "an `if` without `else` must produce `()`, but this branch \
                                     has type `{then_ty}`"
                                ),
                                range,
                            );
                        }
                        Type::Unit
                    }
                }
            }
            Expr::While { condition, body } => {
                self.expect(condition, &Type::Bool, "a `while` condition");
                self.infer(body);
                Type::Unit
            }
            Expr::Loop { body } => {
                self.infer(body);
                // A `loop` yields whatever `break` carries; without tracking
                // that in phase 2 it is left open rather than guessed.
                Type::Unknown
            }
            Expr::Break(value) => {
                if let Some(v) = value {
                    self.infer(v);
                }
                Type::Never
            }
            Expr::Continue => Type::Never,
            Expr::Return(value) => {
                let expected = self.signature.ret.clone();
                match value {
                    Some(v) => {
                        self.expect(v, &expected, "this `return`");
                    }
                    None => {
                        if !Type::Unit.compatible_with(&expected) {
                            self.error(
                                format!("this function returns `{expected}`, so `return` needs a value"),
                                range,
                            );
                        }
                    }
                }
                Type::Never
            }
            Expr::List(items) => {
                for item in items {
                    self.infer(item);
                }
                Type::Unknown
            }
            Expr::Tuple(items) => {
                for item in items {
                    self.infer(item);
                }
                Type::Unknown
            }
            Expr::Match { scrutinee, arms } => self.infer_match(scrutinee, &arms, range),
        }
    }

    fn infer_binary(&mut self, op: BinOp, lhs: ExprId, rhs: ExprId) -> Type {
        match op {
            BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Rem => {
                // `+` is also string concatenation, which the reference
                // program relies on.
                let left = self.infer(lhs);
                if op == BinOp::Add && matches!(left, Type::Str) {
                    self.expect(rhs, &Type::Str, "string concatenation");
                    return Type::Str;
                }
                if !left.compatible_with(&Type::Int) {
                    self.error(
                        format!("arithmetic expects `Int`, found `{left}`"),
                        self.body.range(lhs),
                    );
                }
                self.expect(rhs, &Type::Int, "arithmetic");
                Type::Int
            }
            BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Gt | BinOp::Le | BinOp::Ge => {
                let left = self.infer(lhs);
                self.expect(rhs, &left, "this comparison");
                Type::Bool
            }
            BinOp::And | BinOp::Or => {
                self.expect(lhs, &Type::Bool, "a logical operator");
                self.expect(rhs, &Type::Bool, "a logical operator");
                Type::Bool
            }
        }
    }

    fn infer_call(&mut self, callee: ExprId, args: &[ExprId], range: TextRange) -> Type {
        // A constructor call builds its ADT.
        if let Expr::Path(resolution) = self.body.expr(callee).clone() {
            if let Some(name) = variant_name(&resolution) {
                if let Some(variant) = self.types.variant(&name).cloned() {
                    if args.len() != variant.fields.len() {
                        self.error(
                            format!(
                                "`{}` takes {} argument(s), but {} were given",
                                variant.name,
                                variant.fields.len(),
                                args.len()
                            ),
                            range,
                        );
                    }
                    for (arg, expected) in args.iter().zip(&variant.fields) {
                        self.expect(*arg, expected, "this argument");
                    }
                    return Type::Adt(variant.type_name);
                }
            }
        }

        let callee_ty = self.infer(callee);
        let Type::Fn { params, ret } = callee_ty else {
            for arg in args {
                self.infer(*arg);
            }
            return Type::Unknown;
        };

        if args.len() != params.len() {
            self.error(
                format!("this call takes {} argument(s), but {} were given", params.len(), args.len()),
                range,
            );
        }
        for (arg, expected) in args.iter().zip(&params) {
            self.expect(*arg, expected, "this argument");
        }
        *ret
    }

    fn type_of_resolution(&mut self, resolution: &khora_hir::Resolution) -> Type {
        match resolution {
            khora_hir::Resolution::Item { name, .. } => self
                .types
                .signatures
                .get(name)
                .map(|s| Type::Fn { params: s.params.clone(), ret: Box::new(s.ret.clone()) })
                .unwrap_or(Type::Unknown),
            khora_hir::Resolution::Variant { type_name, name, .. } => {
                match self.types.variant(name) {
                    // A nullary constructor is a value; one with a payload is
                    // reached through a call, handled in `infer_call`.
                    Some(v) if v.fields.is_empty() => Type::Adt(type_name.clone()),
                    Some(_) => Type::Adt(type_name.clone()),
                    None => Type::Unknown,
                }
            }
            khora_hir::Resolution::Unsupported(_) => Type::Unknown,
        }
    }

    fn infer_match(
        &mut self,
        scrutinee: ExprId,
        arms: &[khora_hir::body::MatchArm],
        range: TextRange,
    ) -> Type {
        let scrutinee_ty = self.infer(scrutinee);

        let mut result: Option<Type> = None;
        for arm in arms {
            self.bind_pattern(arm.pat, &scrutinee_ty);
            if let Some(guard) = arm.guard {
                self.expect(guard, &Type::Bool, "a match guard");
            }
            let arm_ty = self.infer(arm.body);
            match &result {
                None => result = Some(arm_ty),
                Some(expected) if arm_ty.compatible_with(expected) => {
                    if matches!(expected, Type::Never) {
                        result = Some(arm_ty);
                    }
                }
                Some(expected) => {
                    self.error(
                        format!("match arms disagree: expected `{expected}`, found `{arm_ty}`"),
                        self.body.range(arm.body),
                    );
                    result = Some(Type::Unknown);
                }
            }
        }

        self.check_match_coverage(&scrutinee_ty, arms, range);
        result.unwrap_or(Type::Unknown)
    }

    fn check_match_coverage(
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

        let column = self.column_type(scrutinee_ty);
        if matches!(column, ColumnType::Unknown) {
            return;
        }

        let missing = usefulness::missing_patterns(&patterns, &column);
        if !missing.is_empty() {
            let names: Vec<String> = missing.iter().map(|p| p.to_string()).collect();
            self.error(
                format!("this `match` is not exhaustive: pattern `{}` not covered", names.join("`, `")),
                range,
            );
        }

        for index in usefulness::unreachable_arms(&patterns, &column) {
            if let Some(arm) = unguarded.get(index) {
                self.error("this arm is unreachable", self.body.range(arm.body));
            }
        }
    }

    fn column_type(&self, ty: &Type) -> ColumnType {
        match ty {
            Type::Bool => ColumnType::Finite(vec![Ctor::Bool(true), Ctor::Bool(false)]),
            Type::Int | Type::Str => ColumnType::Unbounded,
            Type::Adt(name) => {
                let variants = self.types.variants_of(name);
                if variants.is_empty() {
                    ColumnType::Unknown
                } else {
                    ColumnType::Finite(
                        variants
                            .iter()
                            .map(|v| Ctor::Variant { name: v.name.clone(), arity: v.fields.len() })
                            .collect(),
                    )
                }
            }
            _ => ColumnType::Unknown,
        }
    }

    fn to_pattern(&self, pat: PatId) -> Pattern {
        match self.body.pat(pat) {
            // A binding matches everything, exactly like `_`.
            Pat::Wildcard | Pat::Bind(_) | Pat::Missing => Pattern::Wildcard,
            Pat::Literal(lit) => Pattern::Constructor {
                ctor: match lit {
                    Literal::Bool(b) => Ctor::Bool(*b),
                    Literal::Int(n) => Ctor::Literal(n.clone()),
                    Literal::Float(n) => Ctor::Literal(n.clone()),
                    Literal::Str(s) => Ctor::Literal(format!("\"{s}\"")),
                },
                fields: Vec::new(),
            },
            Pat::Path(resolution) => match variant_name(resolution) {
                Some(name) => Pattern::Constructor {
                    ctor: Ctor::Variant { name, arity: 0 },
                    fields: Vec::new(),
                },
                None => Pattern::Wildcard,
            },
            Pat::TupleStruct { resolution, fields } => match variant_name(resolution) {
                Some(name) => Pattern::Constructor {
                    ctor: Ctor::Variant { name, arity: fields.len() },
                    fields: fields.iter().map(|f| self.to_pattern(*f)).collect(),
                },
                None => Pattern::Wildcard,
            },
            Pat::Tuple(fields) => Pattern::Constructor {
                ctor: Ctor::Tuple(fields.len()),
                fields: fields.iter().map(|f| self.to_pattern(*f)).collect(),
            },
        }
    }
}

fn variant_name(resolution: &khora_hir::Resolution) -> Option<String> {
    match resolution {
        khora_hir::Resolution::Variant { name, .. } => Some(name.clone()),
        _ => None,
    }
}

/// Every semantic diagnostic for one file: name resolution and lowering
/// errors from `khora-hir`, then type errors.
///
/// Lowering errors come first because a name that did not resolve makes the
/// type error that follows it noise.
#[salsa::tracked(returns(ref))]
pub fn diagnostics(db: &dyn Db, file: SourceFile) -> Vec<HirError> {
    let mut all: Vec<HirError> = khora_hir::item_map(db, file).errors.clone();
    for (_, body) in khora_hir::body::bodies(db, file) {
        all.extend(body.errors.iter().cloned());
    }
    all.extend(check_file(db, file).iter().cloned());
    all
}
