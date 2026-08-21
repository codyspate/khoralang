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
pub mod traits;
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
    /// A type applied to arguments where the head is not yet a constructor:
    /// `Self<A>` in a higher-kinded trait, or `F<B>` at a call site before `F`
    /// is known.
    ///
    /// The head is a [`Type::Param`] when rigid and a [`Type::Var`] when the
    /// caller still gets to choose it. Solving that variable against a concrete
    /// `Option<Int>` is what decides `F := Option` and `B := Int`, and the
    /// application collapses into an ordinary [`Type::Adt`] as soon as it does —
    /// so nothing downstream of instance selection ever sees one.
    Applied { head: Box<Type>, args: Vec<Type> },
    /// A fixed-length product, as in `(Int, Bool)`.
    ///
    /// The empty tuple is `Unit`, not `Tuple(vec![])`, so there is exactly one
    /// spelling of "no information".
    Tuple(Vec<Type>),
    /// An associated type projected off another type: `Self::Item`.
    ///
    /// Normalizes to whatever the owner's impl bound the name to as soon as the
    /// owner is known — `Range::Item` becomes `Int` given
    /// `impl Iterator for Range { type Item = Int; }`. Until then it stands for
    /// itself, and unifies only with the same projection.
    Assoc { owner: Box<Type>, name: String },
    /// A type-level integer, as in `Matrix<3, 4>`.
    ///
    /// Unifies only with an equal value, which is what turns a shape mismatch
    /// into a compile error instead of a runtime assertion.
    Const(i64),
    /// The type of `return`, `break` and a diverging branch. Compatible with
    /// everything, because control never reaches the consumer.
    Never,
    /// Stands in for an expression whose type could not be determined —
    /// usually downstream of an error already reported. Compatible with
    /// everything, so one mistake does not cascade.
    Unknown,
    // Phase 4 adds Row(..).
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
            // `_` rather than an internal number: this is what a Rust or
            // TypeScript developer already reads as "not pinned down yet".
            Type::Var(_) => write!(f, "_"),
            Type::Const(n) => write!(f, "{n}"),
            Type::Assoc { owner, name } => write!(f, "{owner}::{name}"),
            Type::Applied { head, args } => {
                let inner: Vec<String> = args.iter().map(Type::to_string).collect();
                write!(f, "{head}<{}>", inner.join(", "))
            }
            // `(Int,)` for the one-element case, so it is not read as a
            // parenthesised `Int` - the same disambiguation Rust and Python use.
            Type::Tuple(items) => {
                let inner: Vec<String> = items.iter().map(Type::to_string).collect();
                let trailing = if items.len() == 1 { "," } else { "" };
                write!(f, "({}{trailing})", inner.join(", "))
            }
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
    /// The traits each generic parameter requires, in the order declared.
    ///
    /// Parallel to `generics` rather than a map, so the parameter a bound
    /// belongs to is positional — which is how instantiation already matches
    /// arguments to parameters.
    pub bounds: Vec<Vec<String>>,
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
    /// The traits and impls this file declares.
    pub traits: traits::Traits,
    /// The kind of every named type, so an impl can be checked against the
    /// kind its trait requires.
    pub kinds: HashMap<String, traits::Kind>,
}

impl TypeMap {
    fn variants_of(&self, type_name: &str) -> Vec<&VariantInfo> {
        self.variants.iter().filter(|v| v.type_name == type_name).collect()
    }

    /// A constructor, found by the type it belongs to *and* its own name.
    ///
    /// Both halves are required. Case names are not unique across a program —
    /// two types may each have a `Some` — and `Resolution::Variant` carries the
    /// type for exactly this reason. Looking one up by its bare name resolves
    /// `Maybe::Some` to `Option::Some` whenever `Option` was declared first,
    /// which is a wrong tag rather than an error.
    pub fn variant_of(&self, type_name: &str, case: &str) -> Option<&VariantInfo> {
        self.variants.iter().find(|v| v.type_name == type_name && v.name == case)
    }

}

#[salsa::tracked(returns(ref))]
pub fn type_map(db: &dyn Db, file: SourceFile) -> TypeMap {
    let parse = khora_db::parse(db, file);
    let mut map = TypeMap::default();
    // Which of each type's parameters are const, so `Matrix<const R, const C>`
    // gets the kind `Int -> Int -> *` rather than `* -> * -> *`.
    let mut consts: HashMap<String, Vec<bool>> = HashMap::new();

    for decl in parse.source_file().decls() {
        match decl {
            ast::Decl::Fn(f) => {
                let Some(name) = f.name().and_then(|n| n.ident()) else { continue };
                let generics = generic_names(f.type_params().as_ref());
                let bounds = bound_lists(f.type_params().as_ref());
                let params = f
                    .params()
                    .map(|list| {
                        list.params().map(|p| type_of_syntax(p.ty().as_ref(), &generics)).collect()
                    })
                    .unwrap_or_default();
                let ret = f
                    .return_type()
                    .map_or(Type::Unit, |t| type_of_syntax(Some(&t), &generics));
                map.signatures.insert(name, Signature { generics, bounds, params, ret });
            }
            ast::Decl::Type(t) => {
                let Some(type_name) = t.name().and_then(|n| n.ident()) else { continue };
                let generics = generic_names(t.type_params().as_ref());
                let is_const: Vec<bool> = t
                    .type_params()
                    .map(|p| p.params().map(|g| g.is_const()).collect())
                    .unwrap_or_default();
                consts.insert(type_name.clone(), is_const);
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

    map.signatures.extend(traits::impl_signatures(&parse.source_file()));
    map.traits = traits::collect(&parse.source_file());

    // What this file imported, under the names it uses for them. Without this
    // a cross-module call resolves and then type checks against nothing, which
    // is a *false pass* — strictly worse than the unresolved-name error it
    // replaced.
    import_types(db, file, &mut map, &mut consts);

    map.kinds = traits::kinds(&map.adts, &consts);
    map
}

/// Copies the declarations a file imported into its own view.
///
/// Reads only the *defining* file's `type_map`, so this stays incremental: a
/// body edit in one module cannot invalidate another module's types.
fn import_types(
    db: &dyn Db,
    file: SourceFile,
    map: &mut TypeMap,
    consts: &mut HashMap<String, Vec<bool>>,
) {
    let scope = khora_hir::file_scope(db, file);
    let Some(root) = khora_db::source_root(db) else { return };
    let graph = khora_hir::module_graph(db, root);

    for origin in &scope.origins {
        let (local, module, name, kind) =
            (&origin.local, &origin.module, &origin.name, &origin.kind);
        let Some(source) = graph.file(module) else { continue };
        if source == file {
            continue;
        }
        let exported = type_map(db, source);

        match kind {
            khora_hir::ItemKind::Function => {
                // `entry` rather than `insert`: a file's own declaration wins
                // over an import of the same name, which is what shadowing
                // means everywhere else in the language.
                if let Some(signature) = exported.signatures.get(name.as_str()) {
                    map.signatures.entry(local.clone()).or_insert_with(|| signature.clone());
                }
            }
            khora_hir::ItemKind::Type => {
                if let Some(generics) = exported.adts.get(name.as_str()) {
                    if !map.adts.contains_key(local.as_str()) {
                        map.adts.insert(local.clone(), generics.clone());
                        consts.insert(local.clone(), vec![false; generics.len()]);
                    }
                }
                map.variants.extend(
                    exported.variants.iter().filter(|v| &v.type_name == name).cloned(),
                );
                // A type's own methods are part of the type, exactly as its
                // constructors are. Requiring a second import for them would
                // be ceremony for no decision.
                map.traits
                    .inherent
                    .extend(exported.traits.inherent.iter().filter(|i| &i.head == name).cloned());
                let own = format!("#{name}::");
                for (key, signature) in &exported.signatures {
                    if key.starts_with(&own) {
                        map.signatures.insert(key.clone(), signature.clone());
                    }
                }
            }
            khora_hir::ItemKind::Trait => {
                if let Some(def) = exported.traits.traits.get(name.as_str()) {
                    map.traits.traits.insert(local.clone(), def.clone());
                }
                // A trait's impls travel with it: an imported `Show` is useless
                // if the impls that satisfy it stayed behind.
                map.traits
                    .impls
                    .extend(exported.traits.impls.iter().filter(|i| &i.trait_name == name).cloned());
                for (key, signature) in &exported.signatures {
                    if key.starts_with(&format!("{name}::"))
                        || key.starts_with(&format!("{name}#"))
                    {
                        map.signatures.insert(key.clone(), signature.clone());
                    }
                }
                if let Some(kind) = exported.kinds.get(name.as_str()) {
                    map.kinds.insert(local.clone(), kind.clone());
                }
            }
            _ => {}
        }
    }
}

/// Points at the part of two large types that actually disagrees.
///
/// Unification reports the innermost conflicting pair, which on its own reads
/// as "expected `3`, found `4`" and leaves the reader hunting for where either
/// number came from. The caller leads with the whole types; this adds the
/// detail, and adds nothing when the conflict *is* the whole type, since
/// repeating it would say the same thing twice.
fn disagreement(outer: (&Type, &Type), inner: (&Type, &Type)) -> String {
    if outer == inner {
        return String::new();
    }
    match inner {
        (Type::Const(_), Type::Const(_)) => {
            format!("; dimension `{}` does not match `{}`", inner.0, inner.1)
        }
        _ => format!("; `{}` does not match `{}`", inner.0, inner.1),
    }
}

/// The traits each parameter requires, positionally matched to
/// [`generic_names`]. A parameter with no bounds contributes an empty list, so
/// the two are always the same length.
fn bound_lists(params: Option<&ast::TypeParams>) -> Vec<Vec<String>> {
    params
        .map(|p| {
            p.params()
                .filter(|g| g.name().and_then(|n| n.ident()).is_some())
                .map(|g| traits::bound_names(g.bounds().as_ref()))
                .collect()
        })
        .unwrap_or_default()
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
/// unrecognized becomes [`Type::Unknown`], which suppresses follow-on errors.
fn type_of_syntax(ty: Option<&ast::Type>, generics: &[String]) -> Type {
    let Some(ty) = ty else { return Type::Unknown };
    match ty {
        ast::Type::Unit(_) => Type::Unit,
        // `(Int, Bool) -> Int`. The parameter list parses as whatever shape the
        // parentheses made of it: a tuple for several, a paren for one, a unit
        // for none. All three mean the same thing here.
        ast::Type::Fn(f) => {
            let params = match f.param_type() {
                Some(ast::Type::Tuple(t)) => {
                    t.elements().map(|e| type_of_syntax(Some(&e), generics)).collect()
                }
                Some(ast::Type::Unit(_)) | None => Vec::new(),
                Some(ast::Type::Paren(p)) => {
                    vec![type_of_syntax(p.inner().as_ref(), generics)]
                }
                Some(other) => vec![type_of_syntax(Some(&other), generics)],
            };
            let ret = type_of_syntax(f.return_type().as_ref(), generics);
            Type::Fn { params, ret: Box::new(ret) }
        }
        // A bare integer in type position is a const-generic argument.
        ast::Type::Literal(l) => l.value().map(Type::Const).unwrap_or(Type::Unknown),
        ast::Type::Tuple(t) => {
            let items: Vec<Type> =
                t.elements().map(|e| type_of_syntax(Some(&e), generics)).collect();
            if items.is_empty() { Type::Unit } else { Type::Tuple(items) }
        }
        ast::Type::Path(p) => {
            let name = p.path().map(|p| p.text_path()).unwrap_or_default();
            let args: Vec<Type> = p
                .type_args()
                .map(|a| a.args().map(|t| type_of_syntax(Some(&t), generics)).collect())
                .unwrap_or_default();

            // `T::Item` where `T` is a parameter in scope is a projection, not
            // a type whose name happens to contain `::`.
            if let Some((owner, assoc)) = name.split_once("::") {
                if generics.iter().any(|g| g == owner) {
                    return Type::Assoc {
                        owner: Box::new(Type::Param(owner.to_string())),
                        name: assoc.to_string(),
                    };
                }
            }

            match name.as_str() {
                "Int" => Type::Int,
                "Bool" => Type::Bool,
                "String" => Type::Str,
                "" => Type::Unknown,
                other if generics.iter().any(|g| g == other) => {
                    if args.is_empty() {
                        Type::Param(other.to_string())
                    } else {
                        Type::Applied {
                            head: Box::new(Type::Param(other.to_string())),
                            args,
                        }
                    }
                }
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
    /// created the variables and solved them. Monomorphization reads it to
    /// find out which specializations a body needs.
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

    /// This body's types with `mapping` applied, which is one specialization.
    pub fn specialized(&self, mapping: &HashMap<&str, Type>) -> BodyTypes {
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
            bounds: Vec::new(),
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
            unifier: Unifier::new().with_assoc(types.traits.assoc_bindings()),
            lambdas: Vec::new(),
            errors: Vec::new(),
        };
        checker.check_function();
        checker.check_bounds();
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

/// Everything wrong with the traits and impls a file declares.
///
/// Separate from `check_file` because none of it depends on a function body:
/// an impl is well-formed or it is not, whatever any caller does with it.
#[salsa::tracked(returns(ref))]
pub fn trait_errors(db: &dyn Db, file: SourceFile) -> Vec<HirError> {
    let types = type_map(db, file);
    traits::check(&types.traits, &types.kinds, &types.signatures)
}

struct Checker<'a> {
    types: &'a TypeMap,
    body: &'a Body,
    signature: &'a Signature,
    locals: HashMap<LocalId, Type>,
    exprs: HashMap<ExprId, Type>,
    instantiations: HashMap<ExprId, (String, Vec<Type>)>,
    unifier: Unifier,
    /// The type of each lambda currently being inferred, innermost last, so
    /// that a recursive closure can refer to itself before its body is done.
    lambdas: Vec<Type>,
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
                Mismatch::Types { expected: inner, found: got } => {
                    let inner = self.unifier.zonk(&inner);
                    let got = self.unifier.zonk(&got);
                    let detail = disagreement((&expected, &actual), (&inner, &got));
                    let head = format!("this function returns `{expected}`,");
                    format!("{head} but its body has type `{actual}`{detail}")
                }
                // The other mismatches are whole sentences of their own, so
                // they are joined rather than folded into "but its body ...",
                // which produced "but its body `A` is a type the caller
                // chooses".
                other => format!("this function returns `{expected}`; {other}"),
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
                let variant = variant_case(&resolution)
                    .and_then(|(t, n)| self.types.variant_of(&t, &n))
                    .cloned();
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
                // Destructuring only knows the component types when the
                // scrutinee is a tuple of the same width; a mismatch is
                // reported where the two are unified, not here.
                for (i, field) in fields.iter().enumerate() {
                    let component = match ty {
                        Type::Tuple(items) => items.get(i).cloned().unwrap_or(Type::Unknown),
                        _ => Type::Unknown,
                    };
                    self.bind_pattern(*field, &component);
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
                let message = match why {
                    Mismatch::Types { expected: inner, found: got } => {
                        let inner = self.unifier.zonk(&inner);
                        let got = self.unifier.zonk(&got);
                        let outer = self.unifier.zonk(expected);
                        let whole = self.unifier.zonk(found);
                        let head =
                            Mismatch::Types { expected: outer.clone(), found: whole.clone() };
                        let detail = disagreement((&outer, &whole), (&inner, &got));
                        format!("{context}: {head}{detail}")
                    }
                    other => format!("{context}: {other}"),
                };
                self.error(message, range);
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
                let types: Vec<Type> = items.iter().map(|i| self.infer(*i)).collect();
                if types.is_empty() { Type::Unit } else { Type::Tuple(types) }
            }
            Expr::Match { scrutinee, arms } => self.infer_match(scrutinee, &arms, range),
            Expr::Lambda { params, body, .. } => {
                // A parameter with no annotation gets a variable, so the type
                // is settled by how the lambda is used: `map(xs, (x) => x + 1)`
                // learns `x: Int` from `map`'s signature, not from the lambda.
                let types: Vec<Type> = params
                    .iter()
                    .map(|p| {
                        let ty = self.unifier.fresh();
                        self.bind_pattern(*p, &ty);
                        ty
                    })
                    .collect();

                // The whole type exists before the body is checked, because a
                // recursive closure mentions itself inside it. The result is a
                // variable the body then solves.
                let result = self.unifier.fresh();
                let whole =
                    Type::Fn { params: types, ret: Box::new(result.clone()) };
                self.lambdas.push(whole.clone());
                let ret = self.infer(body);
                self.lambdas.pop();

                self.require(&result, &ret, "this closure's body", range);
                whole
            }
            // Inside its own body, a closure's name is the closure.
            Expr::LambdaSelf => {
                self.lambdas.last().cloned().unwrap_or(Type::Unknown)
            }
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
            if let Some((owner, case)) = variant_case(&resolution) {
                if let Some(variant) = self.types.variant_of(&owner, &case).cloned() {
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

        if let Expr::Field { base, name } = self.body.expr(callee).clone() {
            if let Some(ty) = self.infer_method_call(callee, base, &name, args, range) {
                return ty;
            }
        }

        // Resolved first: a callee's type is often a *variable solved to* a
        // function rather than a function, and matching the shape without
        // following the variable silently treats it as uncallable.
        let inferred = self.infer(callee);
        let callee_ty = self.unifier.shallow(&inferred);
        let Type::Fn { params, ret } = callee_ty else {
            for arg in args {
                self.infer(*arg);
            }
            // Silent for a type that is not known yet: `Unknown` is downstream
            // of an error already reported, and a variable may still turn out
            // to be a function. Anything else is a real mistake, and one that
            // became reachable the moment functions became values.
            if !matches!(callee_ty, Type::Unknown | Type::Var(_) | Type::Never) {
                let zonked = self.unifier.zonk(&callee_ty);
                self.error(format!("`{zonked}` is not a function, so it cannot be called"), range);
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

    /// Resolves `receiver.method(args)` through the traits in scope.
    ///
    /// Returns `None` when the receiver has a *field* of that name, so a record
    /// holding a function keeps working — the field reading is the more
    /// specific one and wins, exactly as it does in Rust.
    fn infer_method_call(
        &mut self,
        callee: ExprId,
        receiver: ExprId,
        method: &str,
        args: &[ExprId],
        range: TextRange,
    ) -> Option<Type> {
        let inferred = self.infer(receiver);
        let self_ty = self.unifier.zonk(&inferred);

        // A receiver whose type is still open cannot select an impl. Saying so
        // is better than picking one and being wrong about it later.
        if matches!(self_ty, Type::Unknown | Type::Var(_) | Type::Never) {
            return None;
        }

        // A type's own method wins over a trait's. Adding a trait to a program
        // must not silently change what an existing call does.
        if let Some(own) = self.types.traits.inherent_method(&self_ty, method) {
            let key = traits::method_key("", &own.head, method);
            return Some(self.call_signature(callee, &key, &self_ty, args, range));
        }

        // Inside a generic function the receiver is rigid, and the only methods
        // it has are the ones its bounds promise. `F<B>` counts: the methods
        // available on it are the ones `F`'s bounds promise, which is what makes
        // `f(v).map(..)` work inside a `traverse`.
        let rigid = match &self_ty {
            Type::Param(p) => Some(p.clone()),
            Type::Applied { head, .. } => match &**head {
                Type::Param(p) => Some(p.clone()),
                _ => None,
            },
            _ => None,
        };
        if let Some(param) = rigid {
            return Some(
                self.infer_bounded_method(callee, &param, &self_ty, method, args, range),
            );
        }

        let (def, imp) = match traits::method_source(&self.types.traits, &self_ty, method) {
            Ok(found) => found,
            // Records do not exist yet, so there is no field that could hold a
            // function and no other reading of `x.f()`. When they land, the
            // field is checked before this and only reaches here if absent.
            Err(traits::MethodError::Unknown) => {
                for arg in args {
                    self.infer(*arg);
                }
                self.error(format!("`{self_ty}` has no method `{method}`"), range);
                return Some(Type::Unknown);
            }
            Err(traits::MethodError::NotImplemented(owners)) => {
                self.error(
                    format!(
                        "`{self_ty}` does not implement `{}`, which is where `{method}` comes from",
                        owners.join("` or `")
                    ),
                    range,
                );
                return Some(Type::Unknown);
            }
            Err(traits::MethodError::Ambiguous(names)) => {
                self.error(
                    format!(
                        "`{method}` is declared by `{}`, and `{self_ty}` implements more than one",
                        names.join("` and `")
                    ),
                    range,
                );
                return Some(Type::Unknown);
            }
        };

        let key = format!("{}::{method}", def.name);
        let _ = imp;
        Some(self.call_signature(callee, &key, &self_ty, args, range))
    }

    /// A method reached through a bound rather than through an impl.
    ///
    /// `fn f<T: Eq>(a: T, b: T) { a.eq(b) }` has no impl to select — `T` is
    /// whatever the caller passes — so the *trait's* signature is used, and
    /// which impl runs is settled by monomorphization.
    fn infer_bounded_method(
        &mut self,
        callee: ExprId,
        param: &str,
        receiver: &Type,
        method: &str,
        args: &[ExprId],
        range: TextRange,
    ) -> Type {
        let declared = self.bounds_on(param);
        let available = traits::with_supertraits(&self.types.traits, &declared);
        let found = available.iter().find_map(|name| {
            let def = self.types.traits.traits.get(name)?;
            def.method(method).map(|m| (def.name.clone(), m.signature.clone()))
        });

        let Some((trait_name, _)) = found else {
            for arg in args {
                self.infer(*arg);
            }
            self.error(
                if declared.is_empty() {
                    format!(
                        "`{param}` is a type the caller chooses and has no bounds, so it has no \
                         method `{method}`; add one, as `{param}: Trait`"
                    )
                } else {
                    format!(
                        "no method `{method}` on `{param}`, whose bounds are `{}`",
                        declared.join("` + `")
                    )
                },
                range,
            );
            return Type::Unknown;
        };

        let key = format!("{trait_name}::{method}");
        self.call_signature(callee, &key, receiver, args, range)
    }

    /// Checks a call against `key`'s signature with `Self` bound to `self_ty`.
    fn call_signature(
        &mut self,
        callee: ExprId,
        key: &str,
        self_ty: &Type,
        args: &[ExprId],
        range: TextRange,
    ) -> Type {
        let Some(signature) = self.signature_for(key, self_ty) else {
            for arg in args {
                self.infer(*arg);
            }
            return Type::Unknown;
        };

        // `Self` is the method's first type argument, so a call through a
        // trait carries the one fact that decides which impl runs. It reaches
        // monomorphization the same way every other type argument does.
        let (ty, type_args) =
            self.unifier.instantiate_with(&signature.generics, &signature.as_fn());
        self.instantiations.insert(callee, (key.to_string(), type_args));
        let Type::Fn { params, ret } = ty else { return Type::Unknown };

        // Bind `Self` by unifying the *receiver parameter* with the receiver,
        // not by assigning the receiver's type to `Self` directly. For `Eq` the
        // parameter is `Self` and the two are the same thing; for `Functor` it
        // is `Self<A>`, and only unifying through it decides `Self := Option`
        // and `A := Int` rather than the nonsense `Self := Option<Int>`.
        if let Some(receiver) = params.first() {
            let _ = self.unifier.unify(receiver, self_ty);
        }

        // The receiver is the first parameter, and it is already checked: it is
        // what selected this signature. Only the written arguments remain.
        let expected = params.get(1..).unwrap_or(&[]);
        if args.len() != expected.len() {
            self.error(
                format!(
                    "`{key}` takes {} argument(s) after the receiver, but {} were given",
                    expected.len(),
                    args.len()
                ),
                range,
            );
        }
        for (arg, want) in args.iter().zip(expected) {
            self.expect(*arg, want, "this argument");
        }
        *ret
    }

    /// The trait's signature for a method key, with `Self` still a parameter.
    fn signature_for(&self, key: &str, _self_ty: &Type) -> Option<Signature> {
        self.types.signatures.get(key).cloned()
    }

    /// The traits the enclosing function requires of `param`.
    fn bounds_on(&self, param: &str) -> Vec<String> {
        self.signature
            .generics
            .iter()
            .position(|g| g == param)
            .and_then(|i| self.signature.bounds.get(i))
            .cloned()
            .unwrap_or_default()
    }

    /// Reports every trait bound this body left unsatisfied.
    ///
    /// Runs after inference rather than during it: a bound is a question about
    /// a *solved* type argument, and asking it while the argument is still a
    /// variable would report whichever call happened to be visited first.
    fn check_bounds(&mut self) {
        let mentions: Vec<(ExprId, String, Vec<Type>)> = self
            .instantiations
            .iter()
            .map(|(id, (name, args))| (*id, name.clone(), args.clone()))
            .collect();

        for (id, name, args) in mentions {
            let Some(signature) = self.types.signatures.get(name.as_str()) else { continue };
            let bounds = signature.bounds.clone();
            let range = self.body.range(id);

            for (arg, required) in args.iter().zip(&bounds) {
                let arg = self.unifier.zonk(arg);
                for wanted in required {
                    // A trait that does not exist is reported where it is
                    // written, not once per use of the function.
                    if !self.types.traits.traits.contains_key(wanted) {
                        continue;
                    }
                    if !self.satisfies(wanted, &arg) {
                        self.error(
                            format!("`{arg}` does not implement `{wanted}`, which `{name}` requires"),
                            range,
                        );
                    }
                }
            }
        }
    }

    /// Whether `ty` implements `wanted`, here in this body.
    ///
    /// A rigid parameter has no impl to find: what it satisfies is whatever the
    /// enclosing signature promised about it, which is why this is a method on
    /// the checker rather than on `Traits`.
    fn satisfies(&self, wanted: &str, ty: &Type) -> bool {
        match ty {
            // Not solved, or downstream of an error already reported.
            Type::Unknown | Type::Var(_) | Type::Never => true,
            Type::Param(p) => {
                let declared = self.bounds_on(p);
                traits::with_supertraits(&self.types.traits, &declared)
                    .iter()
                    .any(|t| t == wanted)
            }
            other => self.types.traits.satisfies(wanted, other),
        }
    }

    /// The type of `Owner::name`, where `Owner` is a trait or a bounded type
    /// parameter and `name` is one of the trait's functions.
    ///
    /// `Self` is left as a fresh variable when the owner is a trait, so the
    /// expected type decides which impl runs — `Applicative::pure(x)` in a
    /// position wanting `Option<Int>` resolves to `Option`'s. When the owner is
    /// a type parameter, `Self` is that parameter and the choice is the
    /// caller's.
    fn type_of_trait_item(&mut self, at: ExprId, owner: &str, name: &str) -> Type {
        let bounds = self.bounds_on(owner);
        let candidates: Vec<String> = if bounds.is_empty() {
            vec![owner.to_string()]
        } else {
            traits::with_supertraits(&self.types.traits, &bounds)
        };

        let found = candidates.iter().find_map(|t| {
            let def = self.types.traits.traits.get(t)?;
            def.method(name).map(|_| t.clone())
        });
        let Some(trait_name) = found else {
            let range = self.body.range(at);
            self.error(
                if bounds.is_empty() {
                    format!("`{owner}` is not a trait with a function named `{name}`")
                } else {
                    format!(
                        "no function `{name}` on `{owner}`, whose bounds are `{}`",
                        bounds.join("` + `")
                    )
                },
                range,
            );
            return Type::Unknown;
        };

        let key = format!("{trait_name}::{name}");
        let Some(signature) = self.types.signatures.get(key.as_str()).cloned() else {
            return Type::Unknown;
        };
        let (ty, type_args) =
            self.unifier.instantiate_with(&signature.generics, &signature.as_fn());

        // A type parameter names itself as `Self`; a trait leaves it open for
        // the surrounding expression to decide.
        if !bounds.is_empty() {
            if let Some(chosen) = type_args.first() {
                let _ = self.unifier.unify(chosen, &Type::Param(owner.to_string()));
            }
        }
        self.instantiations.insert(at, (key, type_args));
        ty
    }

    fn type_of_resolution(&mut self, at: ExprId, resolution: &khora_hir::Resolution) -> Type {
        match resolution {
            khora_hir::Resolution::TraitItem { owner, name } => {
                let (owner, name) = (owner.clone(), name.clone());
                self.type_of_trait_item(at, &owner, &name)
            }
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
                match self.types.variant_of(type_name, name) {
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

    /// A constructor carrying the types of its payload, so specialization can
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
                match variant_case(resolution).and_then(|(t, n)| self.types.variant_of(&t, &n)) {
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


/// The type a constructor belongs to, and the constructor's own name.
///
/// Always prefer this to [`variant_name`] when looking a constructor up: the
/// name alone is ambiguous across types.
fn variant_case(resolution: &khora_hir::Resolution) -> Option<(String, String)> {
    match resolution {
        khora_hir::Resolution::Variant { type_name, name, .. } => {
            Some((type_name.clone(), name.clone()))
        }
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
    // An import that resolved to nothing is the most useful thing to say about
    // a file full of "cannot find" errors downstream of it.
    all.extend(khora_hir::file_scope(db, file).errors.iter().cloned());
    for (_, body) in khora_hir::body::bodies(db, file) {
        all.extend(body.errors.iter().cloned());
    }
    all.extend(trait_errors(db, file).iter().cloned());
    all.extend(check_file(db, file).iter().cloned());
    all
}
