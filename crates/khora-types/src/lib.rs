//! Type checking and inference.
//!
//! Bodies are inferred by unification ([`unify`]) against declared signatures.
//! Signatures stay explicit at function boundaries — that is the decision in
//! `docs/design/associated-items.md` and it is what keeps errors local — but
//! everything inside a body is solved.
//!
//! Row unification for effects arrives in phase 4; the shape [`Type`] needs for
//! it is noted where it will go.

pub mod mono;
pub mod unify;
pub mod usefulness;

use std::collections::HashMap;

use khora_db::{Db, SourceFile};
use khora_hir::body::{BinOp, Body, Expr, ExprId, Literal, LocalId, Pat, PatId, Stmt, UnOp};
use khora_hir::HirError;
use khora_syntax::ast::{self};
use text_size::TextRange;
use unify::{Mismatch, Unifier};
use usefulness::{ColumnType, Ctor, FieldType, Pattern};

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Type {
    Int,
    Bool,
    Str,
    Unit,
    /// A user-declared variant type, with its type arguments.
    Adt { name: String, args: Vec<Type> },
    Fn { params: Vec<Type>, ret: Box<Type> },
    /// A hole inference is free to fill.
    Var(unify::TypeVar),
    /// A type parameter the *caller* chose. Rigid: the body of a generic
    /// function cannot decide what it is. See `unify`.
    Param(String),
    /// The type of `return`, `break` and a diverging branch. Compatible with
    /// everything, because control never reaches the consumer.
    Never,
    /// Stands in for an expression whose type could not be determined —
    /// usually downstream of an error already reported. Compatible with
    /// everything, so one mistake does not cascade.
    Unknown,
    // Phase 4 adds Row(..); const generics add Const(i64).
}

impl std::fmt::Display for Type {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Type::Int => write!(f, "Int"),
            Type::Bool => write!(f, "Bool"),
            Type::Str => write!(f, "String"),
            Type::Unit => write!(f, "()"),
            Type::Adt { name, args } if args.is_empty() => write!(f, "{name}"),
            Type::Adt { name, args } => {
                let inner: Vec<String> = args.iter().map(|a| a.to_string()).collect();
                write!(f, "{name}<{}>", inner.join(", "))
            }
            Type::Param(name) => write!(f, "{name}"),
            Type::Var(v) => write!(f, "?{v}"),
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
    /// A nullary ADT, which is what most of the phase 2 subset used.
    pub fn adt(name: impl Into<String>) -> Type {
        Type::Adt { name: name.into(), args: Vec::new() }
    }
}

/// A function's declared signature.
///
/// `generics` names the rigid parameters. Inside the body they stay rigid; at
/// a call site they are instantiated to fresh variables, which is what lets two
/// calls to the same generic function have unrelated types.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Signature {
    pub generics: Vec<String>,
    pub params: Vec<Type>,
    pub ret: Type,
}

impl Signature {
    /// The signature as a function type, with its parameters still rigid.
    pub fn as_fn(&self) -> Type {
        Type::Fn { params: self.params.clone(), ret: Box::new(self.ret.clone()) }
    }
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
    /// Generic parameters of each declared type, by name.
    pub adts: HashMap<String, Vec<String>>,
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
                let generics = generic_names(f.type_params().as_ref());
                let params = f
                    .params()
                    .map(|list| {
                        list.params().map(|p| type_of_syntax(p.ty().as_ref(), &generics)).collect()
                    })
                    .unwrap_or_default();
                let ret = f
                    .return_type()
                    .map_or(Type::Unit, |t| type_of_syntax(Some(&t), &generics));
                map.signatures.insert(name, Signature { generics, params, ret });
            }
            ast::Decl::Type(t) => {
                let Some(type_name) = t.name().and_then(|n| n.ident()) else { continue };
                let generics = generic_names(t.type_params().as_ref());
                map.adts.insert(type_name.clone(), generics.clone());
                if let Some(ast::Type::Variant(v)) = t.definition() {
                    for case in v.cases() {
                        let Some(name) = case.name().and_then(|n| n.ident()) else { continue };
                        let fields = case
                            .fields()
                            .map(|list| {
                                list.fields()
                                    .map(|f| type_of_syntax(f.ty().as_ref(), &generics))
                                    .collect()
                            })
                            .or_else(|| {
                                case.tuple_fields().map(|list| {
                                    list.types()
                                        .map(|t| type_of_syntax(Some(&t), &generics))
                                        .collect()
                                })
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

fn generic_names(params: Option<&ast::TypeParams>) -> Vec<String> {
    params
        .map(|p| p.params().filter_map(|g| g.name().and_then(|n| n.ident())).collect())
        .unwrap_or_default()
}

/// Maps written syntax to a type.
///
/// `generics` are the names in scope as rigid parameters — a bare `A` inside
/// `fn f<A>(..)` is [`Type::Param`], not an undeclared ADT. Anything else
/// unrecognised becomes [`Type::Unknown`], which suppresses follow-on errors.
fn type_of_syntax(ty: Option<&ast::Type>, generics: &[String]) -> Type {
    let Some(ty) = ty else { return Type::Unknown };
    match ty {
        ast::Type::Unit(_) => Type::Unit,
        ast::Type::Path(p) => {
            let name = p.path().map(|p| p.text_path()).unwrap_or_default();
            let args: Vec<Type> = p
                .type_args()
                .map(|a| a.args().map(|t| type_of_syntax(Some(&t), generics)).collect())
                .unwrap_or_default();

            match name.as_str() {
                "Int" => Type::Int,
                "Bool" => Type::Bool,
                "String" => Type::Str,
                "" => Type::Unknown,
                other if generics.iter().any(|g| g == other) => Type::Param(other.to_string()),
                other => Type::Adt { name: other.to_string(), args },
            }
        }
        _ => Type::Unknown,
    }
}

/// Every type the checker worked out for one body.
///
/// The checker computes these on its way to a verdict, and code generation
/// cannot work without them. Publishing them here is what stops a second
/// implementation of the same rules existing downstream and drifting.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct BodyTypes {
    exprs: HashMap<ExprId, Type>,
    locals: HashMap<LocalId, Type>,
    /// Which instantiation each mention of a generic function chose.
    ///
    /// Recorded here because the checker is the only place that knows: it
    /// created the variables and solved them. Monomorphisation reads it to
    /// find out which specialisations a body needs.
    instantiations: HashMap<ExprId, (String, Vec<Type>)>,
}

impl BodyTypes {
    /// The type of an expression. `Unknown` for anything the checker could not
    /// determine, which is also what an id it never visited reports.
    pub fn of(&self, id: ExprId) -> &Type {
        self.exprs.get(&id).unwrap_or(&Type::Unknown)
    }

    pub fn local(&self, id: LocalId) -> &Type {
        self.locals.get(&id).unwrap_or(&Type::Unknown)
    }

    /// The generic function this expression mentions, and at what arguments.
    pub fn instantiation(&self, id: ExprId) -> Option<&(String, Vec<Type>)> {
        self.instantiations.get(&id)
    }

    pub fn instantiations(&self) -> impl Iterator<Item = (&ExprId, &(String, Vec<Type>))> {
        self.instantiations.iter()
    }

    /// This body's types with `mapping` applied, which is one specialisation.
    pub fn specialised(&self, mapping: &HashMap<&str, Type>) -> BodyTypes {
        BodyTypes {
            exprs: self
                .exprs
                .iter()
                .map(|(k, v)| (*k, unify::substitute(v, mapping)))
                .collect(),
            locals: self
                .locals
                .iter()
                .map(|(k, v)| (*k, unify::substitute(v, mapping)))
                .collect(),
            instantiations: self
                .instantiations
                .iter()
                .map(|(k, (name, args))| {
                    let args = args.iter().map(|a| unify::substitute(a, mapping)).collect();
                    (*k, (name.clone(), args))
                })
                .collect(),
        }
    }
}

/// The result of checking one file: the verdict, and the working.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Checked {
    pub errors: Vec<HirError>,
    /// Per function, in declaration order.
    pub bodies: Vec<(String, BodyTypes)>,
}

/// Checks a file, keeping both the diagnostics and the types.
///
/// One query rather than two so the work is done once; the accessors below are
/// what callers normally want.
#[salsa::tracked(returns(ref))]
pub fn checked(db: &dyn Db, file: SourceFile) -> Checked {
    let types = type_map(db, file);
    let mut out = Checked::default();

    for (name, body) in khora_hir::body::bodies(db, file) {
        let signature = types.signatures.get(name).cloned().unwrap_or(Signature {
            generics: Vec::new(),
            params: Vec::new(),
            ret: Type::Unknown,
        });
        let mut checker = Checker {
            types,
            body,
            signature: &signature,
            locals: HashMap::new(),
            exprs: HashMap::new(),
            instantiations: HashMap::new(),
            unifier: Unifier::new(),
            errors: Vec::new(),
        };
        checker.check_function();
        out.errors.extend(checker.errors);
        // Published types are zonked: a consumer should never see a variable,
        // and code generation cannot do anything with one.
        let exprs = checker.exprs.iter().map(|(k, v)| (*k, checker.unifier.zonk(v))).collect();
        let locals = checker.locals.iter().map(|(k, v)| (*k, checker.unifier.zonk(v))).collect();
        let instantiations = checker
            .instantiations
            .iter()
            .map(|(k, (n, args))| {
                let args = args.iter().map(|a| checker.unifier.zonk(a)).collect();
                (*k, (n.clone(), args))
            })
            .collect();
        out.bodies.push((name.clone(), BodyTypes { exprs, locals, instantiations }));
    }
    out
}

/// The type of every expression and binding, per function.
pub fn body_types(db: &dyn Db, file: SourceFile) -> &Vec<(String, BodyTypes)> {
    &checked(db, file).bodies
}

/// Type errors for one file, and nothing else.
///
/// Kept separate from lowering errors so "does this type-check" stays a
/// question with its own answer; [`diagnostics`] is what a driver wants.
pub fn check_file(db: &dyn Db, file: SourceFile) -> &Vec<HirError> {
    &checked(db, file).errors
}

struct Checker<'a> {
    types: &'a TypeMap,
    body: &'a Body,
    signature: &'a Signature,
    locals: HashMap<LocalId, Type>,
    exprs: HashMap<ExprId, Type>,
    instantiations: HashMap<ExprId, (String, Vec<Type>)>,
    unifier: Unifier,
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
        if let Err(why) = self.unifier.unify(&expected, &actual) {
            let expected = self.unifier.zonk(&expected);
            let actual = self.unifier.zonk(&actual);
            let range = self.body.range(root);
            // The plain mismatch would read "expected `Int`, found `Bool`",
            // which repeats what the sentence already said.
            let message = match why {
                Mismatch::Types { .. } => format!(
                    "this function returns `{expected}`, but its body has type `{actual}`"
                ),
                other => format!("this function returns `{expected}`, but its body {other}"),
            };
            self.error(message, range);
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
                for field in fields {
                    self.bind_pattern(field, &Type::Unknown);
                }
            }
            Pat::Wildcard | Pat::Literal(_) | Pat::Path(_) | Pat::Missing => {}
        }
    }

    /// Infers `id` and requires it to fit `expected`.
    fn expect(&mut self, id: ExprId, expected: &Type, context: &str) -> Type {
        let actual = self.infer(id);
        let range = self.body.range(id);
        self.require(expected, &actual, context, range);
        actual
    }

    /// Requires two types to be equal, reporting `context` if they are not.
    ///
    /// `context` is a noun phrase: the mismatch supplies the detail after it,
    /// so the two read as one sentence.
    fn require(&mut self, expected: &Type, found: &Type, context: &str, range: TextRange) -> bool {
        match self.unifier.unify(expected, found) {
            Ok(()) => true,
            Err(why) => {
                // Zonk first: a message naming `?3` instead of `Int` is useless,
                // and unification may have solved the variable on the way to
                // discovering the conflict.
                let why = match why {
                    Mismatch::Types { expected, found } => Mismatch::Types {
                        expected: self.unifier.zonk(&expected),
                        found: self.unifier.zonk(&found),
                    },
                    other => other,
                };
                self.error(format!("{context}: {why}"), range);
                false
            }
        }
    }

    fn infer(&mut self, id: ExprId) -> Type {
        let ty = self.infer_uncached(id);
        self.exprs.insert(id, ty.clone());
        ty
    }

    fn infer_uncached(&mut self, id: ExprId) -> Type {
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
            Expr::Path(resolution) => self.type_of_resolution(id, &resolution),
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
                        if !self.require(&then_ty, &else_ty, "`if` branches disagree", range) {
                            return Type::Unknown;
                        }
                        if matches!(then_ty, Type::Never) { else_ty } else { then_ty }
                    }
                    // Without an `else`, the branch is only well typed if it
                    // produces nothing — the same rule `match` follows.
                    None => {
                        self.require(
                            &Type::Unit,
                            &then_ty,
                            "an `if` without `else` must produce `()`",
                            range,
                        );
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
                        if self.unifier.unify(&expected, &Type::Unit).is_err() {
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
                let lhs_range = self.body.range(lhs);
                self.require(&Type::Int, &left, "arithmetic", lhs_range);
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

    /// Maps a type's parameters onto the arguments `ty` supplies.
    ///
    /// Falls back to fresh variables when the scrutinee is not the expected
    /// ADT — usually downstream of another error, where inventing a variable
    /// keeps one mistake from becoming several.
    fn substitution_for(&mut self, type_name: &str, ty: &Type) -> HashMap<String, Type> {
        let generics = self.types.adts.get(type_name).cloned().unwrap_or_default();
        let args = match self.unifier.zonk(ty) {
            Type::Adt { name, args } if name == type_name => args,
            _ => Vec::new(),
        };
        generics
            .iter()
            .enumerate()
            .map(|(i, g)| {
                let arg = args.get(i).cloned().unwrap_or_else(|| self.unifier.fresh());
                (g.clone(), arg)
            })
            .collect()
    }

    /// A fresh instance of an ADT, and the substitution that produced it.
    ///
    /// The substitution is what lets a constructor's declared field types be
    /// read at the same instantiation as the result: for `Some(1)` the field is
    /// `?0` and the result `Option<?0>`, and unifying the argument solves both.
    fn instantiate_adt(&mut self, name: &str) -> (Type, HashMap<String, Type>) {
        let generics = self.types.adts.get(name).cloned().unwrap_or_default();
        let mapping: HashMap<String, Type> =
            generics.iter().map(|g| (g.clone(), self.unifier.fresh())).collect();
        let args = generics.iter().map(|g| mapping[g].clone()).collect();
        (Type::Adt { name: name.to_string(), args }, mapping)
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
                    let (result, mapping) = self.instantiate_adt(&variant.type_name);
                    let borrowed: HashMap<&str, Type> =
                        mapping.iter().map(|(k, v)| (k.as_str(), v.clone())).collect();
                    for (arg, declared) in args.iter().zip(&variant.fields) {
                        let expected = unify::substitute(declared, &borrowed);
                        self.expect(*arg, &expected, "this argument");
                    }
                    return result;
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

    fn type_of_resolution(&mut self, at: ExprId, resolution: &khora_hir::Resolution) -> Type {
        match resolution {
            khora_hir::Resolution::Item { name, .. } => {
                // Each mention gets its own copy of the signature, so two calls
                // to the same generic function do not constrain each other.
                match self.types.signatures.get(name).cloned() {
                    Some(sig) => {
                        let (ty, args) =
                            self.unifier.instantiate_with(&sig.generics, &sig.as_fn());
                        self.instantiations.insert(at, (name.clone(), args));
                        ty
                    }
                    None => Type::Unknown,
                }
            }
            khora_hir::Resolution::Variant { type_name, name, .. } => {
                // A nullary constructor is a value; one with a payload is
                // reached through a call, handled in `infer_call`.
                match self.types.variant(name) {
                    Some(_) => self.instantiate_adt(type_name).0,
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
            match result.clone() {
                None => result = Some(arm_ty),
                Some(expected) => {
                    let range = self.body.range(arm.body);
                    if self.require(&expected, &arm_ty, "match arms disagree", range) {
                        if matches!(expected, Type::Never) {
                            result = Some(arm_ty);
                        }
                    } else {
                        result = Some(Type::Unknown);
                    }
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

    /// A constructor carrying the types of its payload, so specialisation can
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
            Pat::Path(resolution) | Pat::TupleStruct { resolution, .. } => {
                let sub = match self.body.pat(pat) {
                    Pat::TupleStruct { fields, .. } => {
                        fields.iter().map(|f| self.to_pattern(*f)).collect()
                    }
                    _ => Vec::new(),
                };
                match variant_name(resolution).and_then(|n| self.types.variant(&n)) {
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

/// expand nested patterns to the right column types.
fn ctor_for(_types: &TypeMap, variant: &VariantInfo) -> Ctor {
    Ctor::Variant {
        name: variant.name.clone(),
        fields: variant.fields.iter().map(field_type).collect(),
    }
}

fn field_type(ty: &Type) -> FieldType {
    match ty {
        Type::Adt { name, .. } => FieldType::Named(name.clone()),
        Type::Bool => FieldType::Named(BOOL_TYPE.to_string()),
        Type::Int | Type::Str => FieldType::Unbounded,
        _ => FieldType::Opaque,
    }
}

fn column_type(types: &TypeMap, ty: &Type) -> ColumnType {
    match ty {
        Type::Bool => ColumnType::Finite(vec![Ctor::Bool(true), Ctor::Bool(false)]),
        Type::Int | Type::Str => ColumnType::Unbounded,
        Type::Adt { name, .. } => {
            let variants = types.variants_of(name);
            if variants.is_empty() {
                ColumnType::Unknown
            } else {
                ColumnType::Finite(
                    variants.iter().map(|v| ctor_for(types, v)).collect(),
                )
            }
        }
        _ => ColumnType::Unknown,
    }
}


/// `Bool` has constructors but is not an ADT, so the resolver needs a name for
/// it. Lowercase, which no declared type can be.
const BOOL_TYPE: &str = "bool";

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
