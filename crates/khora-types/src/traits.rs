//! Traits, impls, and the kinds that decide which of them fit together.
//!
//! `docs/design/typeclasses.md` settles the shape of all of this: Rust's
//! spelling, Rust's coherence rules, static dispatch through the existing
//! monomorphization pass, and higher kinds with no notation of their own.
//!
//! # Why kinds are here at all
//!
//! A trait says how it uses `Self`: `Eq` writes `Self`, `Functor` writes
//! `Self<A>`. That difference is the whole kind system a reader sees — `Eq` can
//! be implemented for `Int` and `Functor` cannot, and the compiler works out
//! which without anyone writing `* -> *`. Scala makes you write `F[_]`; the
//! information is already in the trait body.
//!
//! # What is deliberately not here
//!
//! The orphan rule. It is decided — an impl needs the trait or the type to be
//! local — but cannot be *checked* until traits resolve across packages, and
//! enforcing it now would reject `impl Show for Int` in a file with no way to
//! say where `Show` came from. `docs/errata.md`.

use std::collections::HashMap;
use std::fmt;

use khora_hir::HirError;
use khora_syntax::ast::{self, AstNode};
use text_size::TextRange;

use crate::{type_of_syntax, Signature, Type};

/// What a type is, before you ask what values it has.
///
/// `Int` is a type. `Option` is not — it is a function from a type to a type,
/// and applying it to `Int` gives one. `Matrix` takes two *numbers* rather than
/// two types, which is why `Nat` is a kind of its own rather than a `Type`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Kind {
    /// `*` — an ordinary type, one that values can have.
    Type,
    /// The kind of a const-generic argument, as `3` in `Matrix<3, 4>`.
    Nat,
    /// `K -> L` — a constructor. `Option : * -> *`.
    Arrow(Box<Kind>, Box<Kind>),
}

impl Kind {
    /// The kind of a constructor taking `params`, each of the given kind.
    pub fn function(params: Vec<Kind>) -> Kind {
        params.into_iter().rev().fold(Kind::Type, |acc, p| Kind::Arrow(Box::new(p), Box::new(acc)))
    }

    /// How many arguments this kind takes before it is a type.
    pub fn arity(&self) -> usize {
        match self {
            Kind::Arrow(_, rest) => 1 + rest.arity(),
            _ => 0,
        }
    }
}

impl fmt::Display for Kind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Kind::Type => write!(f, "*"),
            Kind::Nat => write!(f, "Int"),
            Kind::Arrow(from, to) => write!(f, "{from} -> {to}"),
        }
    }
}

/// One function a trait requires.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MethodDef {
    pub name: String,
    /// The signature as written, with `Self` left as a rigid parameter.
    pub signature: Signature,
    /// True when the trait supplies a body, so an impl may omit it.
    pub has_default: bool,
    pub range: TextRange,
}

/// A declared trait.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TraitDef {
    pub name: String,
    /// The trait's own type parameters: `A` for `trait Convert<A>`.
    ///
    /// **Nothing recorded these, so `A` existed only in the source text.** An
    /// impl at a concrete argument was then checked against a signature still
    /// mentioning `A` and told ``convert` returns `String` here, but `Convert`
    /// declares `A`` — a trait parameter cannot be substituted by a checker
    /// that does not know it is one. These are the left-hand side of that
    /// substitution; [`ImplDef::trait_args`] is the right.
    pub type_params: Vec<String>,
    /// Traits an implementing type must also implement: `trait Ord: Eq`.
    pub supertraits: Vec<String>,
    pub assoc_types: Vec<String>,
    pub methods: Vec<MethodDef>,
    /// Inferred from how the trait's own signatures use `Self`.
    pub self_kind: Kind,
    pub range: TextRange,
}

impl TraitDef {
    pub fn method(&self, name: &str) -> Option<&MethodDef> {
        self.methods.iter().find(|m| m.name == name)
    }
}

/// One `impl Trait for Type { .. }`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImplDef {
    pub trait_name: String,
    /// The arguments the impl gives the trait's own parameters: `[String]` for
    /// `impl Convert<String> for Int`, `[Param("A")]` for
    /// `impl<A> Convert<A> for Wrapper`.
    ///
    /// Empty for a trait that takes none, which is every trait `Traits::find`
    /// was written for and why an argument-blind lookup stayed correct for so
    /// long.
    pub trait_args: Vec<Type>,
    /// The trait half of this impl's method keys, arguments included:
    /// `Convert<String>`.
    ///
    /// Equal to `trait_name` whenever the trait takes no arguments, so every
    /// key a program had before is unchanged. Built by
    /// [`khora_hir::body::trait_key`], which is also what lowering keys the
    /// bodies under — the two must agree character for character or a method
    /// resolves to a signature with no body.
    pub trait_key: String,
    /// The implementing type, with the impl's own parameters rigid: `Option<A>`
    /// for `impl<A> Eq for Option<A>`.
    pub self_type: Type,
    /// The impl's own type parameters, which are what make `impl<A> Eq for
    /// Option<A>` cover every `A` without being a blanket impl.
    pub generics: Vec<String>,
    /// What each of those parameters must itself implement.
    ///
    /// **Kept, because the impl is only as good as these.**
    /// `impl<A: Show, E: Show> Show for Result<A, E>` says a `Result` can be
    /// shown *when its two halves can*, and dropping the condition made
    /// `Result<Int, UserError>` satisfy `Show` for a `UserError` that has
    /// none. The checker passed and monomorphisation found it, which is the
    /// check/build split roadmap 14.30 exists to close.
    pub bounds: Vec<(String, Vec<String>)>,
    pub methods: Vec<String>,
    pub assoc_types: Vec<(String, Type)>,
    pub range: TextRange,
    /// Whether this file wrote the impl, or imported it.
    ///
    /// An imported impl has already been checked where it was written, and
    /// checking it again here asks the wrong question — `impl Share for Fibers`
    /// is legitimate in `std::core` and would be a forgery anywhere else, so
    /// the same impl has two answers depending on which file is looking.
    pub local: bool,
}

impl ImplDef {
    /// The key an impl is found by: the head constructor of its self type.
    ///
    /// Resolution is nominal, so this is a name and never a shape.
    pub fn head(&self) -> Option<String> {
        head_of(&self.self_type)
    }

    /// Whether this impl is the one for a trait used at `args`.
    ///
    /// `impl Convert<String> for Int` answers at `[String]` and not at
    /// `[Bool]`. `impl<A> Convert<A> for Wrapper` answers at both, because `A`
    /// is the impl's own parameter and stands for whatever was asked for.
    ///
    /// An empty `args` is a caller with nothing to say about the arguments, and
    /// every impl answers it -- see [`Traits::find_at`]. So is an argument the
    /// caller has not solved: a `Var` or an `Unknown` is the absence of an
    /// answer rather than a wrong one, and rejecting on it would report "no
    /// impl" for a program whose only problem is that inference is not finished.
    pub fn answers_at(&self, args: &[Type]) -> bool {
        if args.is_empty() {
            return true;
        }
        self.trait_args.len() == args.len()
            && self.trait_args.iter().zip(args).all(|(mine, wanted)| match wanted {
                Type::Unknown | Type::Var(_) | Type::Never => true,
                wanted => match mine {
                    Type::Param(p) if self.generics.iter().any(|g| g == p) => true,
                    mine => mine == wanted,
                },
            })
    }

    /// The type this impl is *for*: its name, and the module that declared it.
    ///
    /// **The head alone is not a type.** `impl Show for Entry` in two modules
    /// is two impls for two types, and everything that deduplicated,
    /// merged or searched impls by head treated them as one -- so a program
    /// holding `std::schema::Entry` and its own `Entry` kept whichever was
    /// seen first and lost the other. The one that survived did not match the
    /// receiver, so the call fell back to the trait's own bodyless method and
    /// the build ended with ``Show::show` has no body` pointing at a blank
    /// line. Errata 62.
    ///
    /// `None` for a home nothing recorded, which is not a name collision but
    /// the absence of information: a type with no home compares equal to
    /// another with no home, which is the old behaviour and the right default.
    pub fn target(&self) -> Option<(String, Option<khora_hir::ModulePath>)> {
        Some((self.head()?, home_of(&self.self_type)))
    }
}

/// The head constructor of a type, or `None` for one that has no name.
pub fn head_of(ty: &Type) -> Option<String> {
    match ty {
        Type::Int => Some("Int".to_string()),
        Type::Float => Some("Float".to_string()),
        Type::Fixed(kind) => Some(kind.name()),
        Type::Ptr => Some("Ptr".to_string()),
        Type::Bool => Some("Bool".to_string()),
        Type::Char => Some("Char".to_string()),
        Type::Str => Some("String".to_string()),
        Type::Unit => Some("()".to_string()),
        Type::Adt { name, .. } => Some(name.clone()),
        Type::Tuple(items) => Some(format!("({},)", items.len())),
        // An application whose head is already a constructor names that
        // constructor; one whose head is still a variable names nothing yet.
        Type::Applied { head, .. } => head_of(head),
        _ => None,
    }
}

/// Which module declared the type at the head of `ty`, if it is one that
/// carries a home.
///
/// Half of a type's identity. [`head_of`] is the other half, and on its own it
/// is not enough: a program with two types named `Entry` has two, and an impl
/// belongs to exactly one of them.
pub fn home_of(ty: &Type) -> Option<khora_hir::ModulePath> {
    match ty {
        Type::Adt { home, .. } => home.clone(),
        Type::Applied { head, .. } => home_of(head),
        _ => None,
    }
}

/// A type's own methods, declared by `impl Type { .. }` with no trait.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InherentImpl {
    /// The head constructor the methods belong to: `User` for `impl User`.
    pub head: String,
    pub self_type: Type,
    pub generics: Vec<String>,
    /// Every method the block declares.
    pub methods: Vec<String>,
    /// The subset carrying `export`, which is what another module may call.
    ///
    /// **`export` on a method used to be decoration** — parsed, and read by
    /// nothing. `std` omitted it on 317 methods and `packages/postgres` wrote
    /// it on all of theirs, and both were correct, which is the state a
    /// keyword ends up in when it means nothing. Roadmap 13.11,
    /// `docs/design/std-surface.md`.
    pub exported: Vec<String>,
    /// Whether this arrived by import rather than being written here.
    ///
    /// A module can always call its own methods, so `export` is a statement
    /// about *other* modules and this is what tells them apart. Set by
    /// [`crate::map::import_inherent`], which is the only way a foreign impl
    /// gets here.
    pub foreign: bool,
    pub range: TextRange,
}

impl InherentImpl {
    /// Whether a file holding this may call `method`.
    pub fn visible(&self, method: &str) -> bool {
        !self.foreign || self.exported.iter().any(|m| m == method)
    }
}

/// Every trait and impl a file declares.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Traits {
    pub traits: HashMap<String, TraitDef>,
    pub impls: Vec<ImplDef>,
    /// Methods a type declares for itself, needing no trait.
    pub inherent: Vec<InherentImpl>,
}

impl Traits {
    /// Whether this file knows anything at all about a type's methods.
    ///
    /// **The question is "did anything arrive", not "is there such a method".**
    /// A type can reach a file without its name: `nursery(..)` installs a
    /// capability whose type comes from `std::core`'s signature, and nothing
    /// about `Nursery` has to be imported for that call to check. Calling an
    /// operation on it is another matter, because a trait's methods need the
    /// trait in scope -- so a lookup that finds nothing means one of two
    /// unrelated things, and only this tells them apart. Nothing here at all
    /// is a missing import; a head that is here without the method is a
    /// spelling mistake.
    pub fn knows(&self, head: &str) -> bool {
        self.impls.iter().any(|i| i.head().as_deref() == Some(head))
            || self.inherent.iter().any(|i| i.head == head)
            || self.traits.contains_key(head)
    }

    /// The impl of `trait_name` covering `ty`, if one exists.
    ///
    /// Matching is on the head constructor: `impl<A> Eq for Option<A>` answers
    /// for every `Option<..>`, and no impl answers for a type variable, because
    /// which impl applies is not yet known.
    ///
    /// The trait's own arguments are not asked about, which is the right
    /// question for every caller that has none to ask -- "does `Int` implement
    /// `Convert` at all" -- and the wrong one for choosing between
    /// `Convert<String>` and `Convert<Bool>`. [`Traits::find_at`] is that
    /// question.
    pub fn find(&self, trait_name: &str, ty: &Type) -> Option<&ImplDef> {
        self.find_at(trait_name, &[], ty)
    }

    /// The impl of `trait_name` at `args` covering `ty`.
    ///
    /// **An empty `args` means "at any arguments", not "at none".** Almost
    /// every caller asks about a trait that takes none, where the two readings
    /// coincide and the filter is inert. A caller that has solved the trait's
    /// arguments -- monomorphization, once inference has settled what the
    /// result of `convert` is used as -- passes them, and gets the one impl
    /// that promised them rather than whichever was collected first.
    ///
    /// An argument still open on the caller's side is not a reason to reject an
    /// impl: it carries no information, and refusing here would turn "not
    /// decided yet" into "no impl", which is a different diagnostic pointing at
    /// a different line.
    pub fn find_at(&self, trait_name: &str, args: &[Type], ty: &Type) -> Option<&ImplDef> {
        let head = head_of(ty)?;
        let named = self
            .impls
            .iter()
            .filter(|i| i.trait_name == trait_name && i.head().as_deref() == Some(head.as_str()))
            .filter(|i| i.answers_at(args));

        // **The first impl whose type is the receiver's, not the first whose
        // name is.** Two modules may each declare an `Entry`, and this used to
        // hand back whichever was recorded first: the caller then failed to
        // match its parameters against the receiver, gave up, and emitted a
        // call to the trait's own bodyless method. Errata 62.
        let mut fallback = None;
        for imp in named {
            let mut solved = std::collections::HashMap::new();
            if crate::unify::match_params(&imp.self_type, ty, &imp.generics, &mut solved) {
                return Some(imp);
            }
            // Kept in case nothing matches. A receiver carrying no home, or a
            // self type this cannot line up with, is the case that resolved by
            // name before and still should: refusing here would turn a working
            // program into `has no body`, which is the failure being fixed.
            fallback.get_or_insert(imp);
        }
        fallback
    }

    /// Every `type Name = Value` the impls in scope declare, in the shape the
    /// unifier needs to normalize a projection.
    pub fn assoc_bindings(&self) -> Vec<crate::unify::AssocBinding> {
        self.impls
            .iter()
            .filter_map(|imp| Some((imp, imp.head()?)))
            .flat_map(|(imp, head)| {
                imp.assoc_types.iter().map(move |(name, value)| crate::unify::AssocBinding {
                    head: head.clone(),
                    name: name.clone(),
                    generics: imp.generics.clone(),
                    self_type: imp.self_type.clone(),
                    value: value.clone(),
                })
            })
            .collect()
    }

    /// Whether `ty` implements `trait_name`, following supertraits.
    ///
    /// **The impl's own bounds are part of the answer.** Finding
    /// `impl<A: Show, E: Show> Show for Result<A, E>` by its head says a
    /// `Result` *can* be shown; whether this one can depends on what is in it.
    /// Without the second half, `Result<Int, UserError>` satisfied `Show` for a
    /// `UserError` that had none, `khora check` passed, and the build failed at
    /// the far end with `Show::show has no body` -- a message about the trait
    /// rather than about the type, pointing at no line in particular.
    pub fn satisfies(&self, trait_name: &str, ty: &Type) -> bool {
        self.satisfied_within(trait_name, &[], ty, 0)
    }

    /// The same question about a trait used at particular arguments.
    ///
    /// `Int` satisfies `Convert` -- it implements it once -- without satisfying
    /// `Convert<Bool>`, and a caller holding the arguments is entitled to the
    /// narrower answer. [`Traits::satisfies`] is the wider one, which is what a
    /// bound written as a bare name asks for.
    pub fn satisfies_at(&self, trait_name: &str, args: &[Type], ty: &Type) -> bool {
        self.satisfied_within(trait_name, args, ty, 0)
    }

    /// The same question, with a depth to stop a cycle.
    ///
    /// A well-formed program cannot loop here -- each step is a *smaller* type,
    /// since a bound is on a parameter of the head just matched -- but a
    /// malformed one should get an error rather than a stack overflow, and this
    /// runs on every hole in every file.
    fn satisfied_within(&self, trait_name: &str, args: &[Type], ty: &Type, depth: usize) -> bool {
        const DEEPEST: usize = 32;
        let Some(found) = self.find_at(trait_name, args, ty) else { return false };
        if depth >= DEEPEST || found.bounds.is_empty() {
            return true;
        }
        let bindings = bind_parameters(&found.self_type, ty, &found.generics);
        found.bounds.iter().all(|(parameter, wanted)| {
            let Some(actual) = bindings.get(parameter.as_str()) else { return true };
            wanted.iter().all(|w| match actual {
                // Not settled, still rigid, or downstream of an error already
                // reported: the caller's own `satisfies` answers for these, and
                // guessing here would blame the wrong expression.
                Type::Unknown | Type::Var(_) | Type::Never | Type::Param(_) => true,
                // A bound on an impl's parameter is written as a bare trait
                // name, so there are no arguments to carry into the recursion.
                settled => self.satisfied_within(w, &[], settled, depth + 1),
            })
        })
    }

    /// A method `ty` declares for itself, if it has one by that name.
    ///
    /// Checked *before* traits: a type's own method wins over a trait method of
    /// the same name, which is the rule that keeps adding a trait from silently
    /// changing what an existing call does.
    pub fn inherent_method(&self, ty: &Type, method: &str) -> Option<&InherentImpl> {
        let head = head_of(ty)?;
        self.inherent
            .iter()
            .find(|i| i.head == head && i.methods.iter().any(|m| m == method) && i.visible(method))
    }

    /// A method that is there and that this file may not call.
    ///
    /// Only for the diagnostic: "there is no such method" and "there is, and
    /// it is not exported" send a reader to two different places, and the
    /// second is a one-word fix in a file they may not have thought to open.
    pub fn inherent_hidden(&self, ty: &Type, method: &str) -> Option<&InherentImpl> {
        let head = head_of(ty)?;
        self.inherent
            .iter()
            .find(|i| i.head == head && i.methods.iter().any(|m| m == method) && !i.visible(method))
    }
}

/// The kind of each type a file declares, plus the built-ins.
pub fn kinds(adts: &HashMap<String, Vec<String>>, consts: &HashMap<String, Vec<bool>>) -> HashMap<String, Kind> {
    let mut out: HashMap<String, Kind> = HashMap::new();
    for name in ["Int", "Bool", "String"] {
        out.insert(name.to_string(), Kind::Type);
    }
    for (name, params) in adts {
        let is_const = consts.get(name);
        let kinds: Vec<Kind> = params
            .iter()
            .enumerate()
            .map(|(i, _)| match is_const.and_then(|c| c.get(i)) {
                Some(true) => Kind::Nat,
                _ => Kind::Type,
            })
            .collect();
        out.insert(name.clone(), Kind::function(kinds));
    }
    out
}

/// Reads the traits and impls a file declares.
///
/// Signatures keep `Self` as a rigid parameter; substituting it is what an impl
/// is for. The kind of `Self` is whatever the widest application in the trait
/// requires — `Self<A>` anywhere in `Functor` makes `Self : * -> *`.
pub fn collect(source: &ast::SourceFile, homes: &crate::TypeHomes) -> Traits {
    let mut out = Traits::default();

    for decl in source.decls() {
        match decl {
            ast::Decl::Trait(t) => {
                let Some(name) = t.name().and_then(|n| n.ident()) else { continue };
                let own = crate::generic_names(t.type_params().as_ref());
                // `Self` is in scope throughout the trait, as a parameter the
                // impl chooses. That is exactly what a rigid parameter is.
                let mut scope = vec!["Self".to_string()];
                scope.extend(own.iter().cloned());

                let assoc_types: Vec<String> =
                    t.assoc_types().filter_map(|a| a.name().and_then(|n| n.ident())).collect();

                let methods: Vec<MethodDef> = t
                    .functions()
                    .filter_map(|f| method_def(&f, &scope, homes))
                    .collect();

                let self_kind = self_kind(&methods);
                out.traits.insert(
                    name.clone(),
                    TraitDef {
                        name,
                        type_params: own.clone(),
                        supertraits: bound_names(t.supertraits().as_ref()),
                        assoc_types,
                        methods,
                        self_kind,
                        range: t.syntax().text_range(),
                    },
                );
            }
            ast::Decl::Impl(i) if i.is_inherent() => {
                let generics = crate::generic_names(i.type_params().as_ref());
                let self_type = type_of_syntax(i.self_type().as_ref(), &generics, homes);
                let Some(head) = head_of(&self_type) else { continue };
                out.inherent.push(InherentImpl {
                    head,
                    self_type,
                    generics,
                    methods: i
                        .functions()
                        .filter_map(|f| f.name().and_then(|n| n.ident()))
                        .collect(),
                    exported: i
                        .functions()
                        .filter(|f| f.is_exported())
                        .filter_map(|f| f.name().and_then(|n| n.ident()))
                        .collect(),
                    foreign: false,
                    range: i.syntax().text_range(),
                });
            }
            ast::Decl::Impl(i) => {
                let generics = crate::generic_names(i.type_params().as_ref());
                let Some(written) = i.trait_() else { continue };
                let Some(trait_name) = written_head(&written) else { continue };
                // `Convert<String>` keeps its `<String>` in syntax; it was
                // dropped here, and everything downstream then had only the
                // head name to work from.
                let trait_args = written_args(&written, &generics, homes);
                let trait_key =
                    khora_hir::body::trait_key(&written).unwrap_or_else(|| trait_name.clone());
                let self_type = type_of_syntax(i.self_type().as_ref(), &generics, homes);
                let methods: Vec<String> =
                    i.functions().filter_map(|f| f.name().and_then(|n| n.ident())).collect();
                let assoc_types: Vec<(String, Type)> = i
                    .assoc_types()
                    .filter_map(|a| {
                        let name = a.name().and_then(|n| n.ident())?;
                        Some((name, type_of_syntax(a.definition().as_ref(), &generics, homes)))
                    })
                    .collect();
                let bounds: Vec<(String, Vec<String>)> = i
                    .type_params()
                    .iter()
                    .flat_map(|p| p.params())
                    .filter_map(|g| {
                        let name = g.name()?.ident()?;
                        let wanted = bound_names(g.bounds().as_ref());
                        (!wanted.is_empty()).then_some((name, wanted))
                    })
                    .collect();
                out.impls.push(ImplDef {
                    trait_name,
                    trait_args,
                    trait_key,
                    self_type,
                    generics,
                    bounds,
                    methods,
                    assoc_types,
                    range: i.syntax().text_range(),
                    local: true,
                });
            }
            _ => {}
        }
    }
    out
}

fn method_def(
    f: &ast::FnDecl,
    scope: &[String],
    homes: &crate::TypeHomes,
) -> Option<MethodDef> {
    let name = f.name()?.ident()?;
    let own = crate::generic_names(f.type_params().as_ref());
    let own_bounds = crate::bound_lists(f.type_params().as_ref());
    let mut generics = scope.to_vec();
    generics.extend(own.iter().cloned());

    let params = f
        .params()
        .map(|list| {
            list.params()
                .map(|p| match p.ty() {
                    Some(ty) => type_of_syntax(Some(&ty), &generics, homes),
                    // A bare `self` means `self: Self`, as in Rust.
                    None if p.name().and_then(|n| n.ident()).as_deref() == Some("self") => {
                        Type::Param("Self".to_string())
                    }
                    None => Type::Unknown,
                })
                .collect()
        })
        .unwrap_or_default();
    let ret = f.return_type().map_or(Type::Unit, |t| type_of_syntax(Some(&t), &generics, homes));
    let requires =
        crate::row_of_syntax(
            f.with_clause().and_then(|c| c.row()).as_ref(),
            crate::syntax::RowClause::Requires,
            &generics,
            homes,
        );
    let raises =
        crate::row_of_syntax(
            f.raises_clause().and_then(|c| c.row()).as_ref(),
            crate::syntax::RowClause::Raises,
            &generics,
            homes,
        );

    Some(MethodDef {
        name,
        signature: Signature {
            is_extern: f.is_extern(),
            generics: own,
            bounds: own_bounds,
            requires,
            raises,
            params,
            ret,
        },
        has_default: f.body().is_some(),
        range: f.syntax().text_range(),
    })
}

/// The kind `Self` must have, read off how the trait's signatures apply it.
fn self_kind(methods: &[MethodDef]) -> Kind {
    let mut arity = 0usize;
    for m in methods {
        for ty in m.signature.params.iter().chain(std::iter::once(&m.signature.ret)) {
            arity = arity.max(applied_arity(ty, "Self"));
        }
    }
    Kind::function(vec![Kind::Type; arity])
}

/// The largest number of arguments `param` is applied to anywhere in `ty`.
fn applied_arity(ty: &Type, param: &str) -> usize {
    match ty {
        Type::Applied { head, args } => {
            let here = match &**head {
                Type::Param(p) if p == param => args.len(),
                _ => 0,
            };
            args.iter()
                .map(|a| applied_arity(a, param))
                .max()
                .unwrap_or(0)
                .max(here)
        }
        Type::Adt { args, .. } => args.iter().map(|a| applied_arity(a, param)).max().unwrap_or(0),
        Type::Tuple(items) => items.iter().map(|a| applied_arity(a, param)).max().unwrap_or(0),
        Type::Fn { params, ret, .. } => params
            .iter()
            .chain(std::iter::once(&**ret))
            .map(|a| applied_arity(a, param))
            .max()
            .unwrap_or(0),
        _ => 0,
    }
}

/// Whether `param` appears anywhere in `ty`.
///
/// Asked of `Self` in a trait method's signature, to tell "nothing at this
/// call decides which impl" from "nothing *anywhere* could": a method that
/// never mentions `Self` has no impl to be chosen for it at all, and telling
/// its caller to annotate the result is advice that cannot work.
pub fn mentions_param(ty: &Type, param: &str) -> bool {
    match ty {
        Type::Param(p) => p == param,
        Type::Adt { args, .. } | Type::Tuple(args) => {
            args.iter().any(|a| mentions_param(a, param))
        }
        Type::Applied { head, args } => {
            mentions_param(head, param) || args.iter().any(|a| mentions_param(a, param))
        }
        Type::Fn { params, ret, requires, raises } => {
            params.iter().any(|a| mentions_param(a, param))
                || mentions_param(ret, param)
                || mentions_param(requires, param)
                || mentions_param(raises, param)
        }
        Type::Row { fields, tail } => {
            fields.iter().any(|(_, t)| mentions_param(t, param))
                || tail.as_deref().is_some_and(|t| mentions_param(t, param))
        }
        Type::Assoc { owner, .. } => mentions_param(owner, param),
        _ => false,
    }
}

/// The trait names in a bound list, ignoring anything that is not a plain name.
pub fn bound_names(bounds: Option<&ast::TypeBounds>) -> Vec<String> {
    bounds
        .map(|b| b.types().filter_map(|t| written_head(&t)).collect())
        .unwrap_or_default()
}

/// What an impl's parameters stand for, given the type it was found for.
///
/// `Result<A, E>` against `Result<Int, UserError>` gives `A = Int`,
/// `E = UserError`. Positional and shallow, which is all a nominal resolution
/// needs: the head has already matched, so only the arguments are in question.
fn bind_parameters<'a>(
    pattern: &Type,
    concrete: &'a Type,
    generics: &[String],
) -> std::collections::HashMap<String, &'a Type> {
    let mut out = std::collections::HashMap::new();
    let (Type::Adt { args: from, .. }, Type::Adt { args: to, .. }) = (pattern, concrete) else {
        return out;
    };
    for (slot, value) in from.iter().zip(to) {
        if let Type::Param(name) = slot {
            if generics.iter().any(|g| g == name) {
                out.insert(name.clone(), value);
            }
        }
    }
    out
}

/// The type arguments of a written type: `[Int]` for `Option<Int>`.
///
/// Read straight off the path rather than by converting the whole type, because
/// a trait is not a type: `Convert` has no declaration for `named_type` to
/// resolve, and only the arguments are wanted here.
fn written_args(ty: &ast::Type, generics: &[String], homes: &crate::TypeHomes) -> Vec<Type> {
    let ast::Type::Path(path) = ty else { return Vec::new() };
    path.type_args()
        .map(|a| a.args().map(|t| type_of_syntax(Some(&t), generics, homes)).collect())
        .unwrap_or_default()
}

/// The head name of a written type: `Option` for `Option<Int>`.
fn written_head(ty: &ast::Type) -> Option<String> {
    match ty {
        ast::Type::Path(p) => p.path().map(|p| p.text_path()),
        _ => None,
    }
}

/// The key an impl's method is known by: `Eq#Int::eq`.
///
/// Matches `khora_hir::body::impl_key`, which is what the body is recorded
/// under. `#` cannot occur in a Khora identifier, so neither half can collide
/// with a name a program chose.
pub fn method_key(trait_name: &str, head: &str, method: &str) -> String {
    format!("{trait_name}#{head}::{method}")
}

/// A signature key as somebody should read it.
///
/// [`method_key`] separates the trait from the type with a `#`, and an
/// inherent impl has no trait -- so the key of `Dict::insert` is
/// `#Dict::insert`, and that is what a reader was shown:
///
/// ```text
/// error: `Colour` does not implement `Ord`, which `#Dict::insert` requires
/// ```
///
/// The `#` is this module's punctuation and means nothing outside it. A
/// message that shows it is asking somebody to know how the compiler stores
/// things in order to read a sentence about their own program.
pub fn readable_key(key: &str) -> &str {
    match key.split_once('#') {
        // `Trait#Head::method` -- the trait is the interesting half and is
        // already named elsewhere in every message that gets here, so the
        // qualified method is what to show.
        Some((_, rest)) => rest,
        None => key,
    }
}

/// The signature of each impl method, keyed by [`method_key`].
///
/// Read from the impl's own written signature rather than derived from the
/// trait's, so that a mismatch between the two is a *diagnosable difference*
/// rather than something the checker silently papers over.
pub fn impl_signatures(
    source: &ast::SourceFile,
    homes: &crate::TypeHomes,
) -> HashMap<String, Signature> {
    let mut out = HashMap::new();

    // A trait's own signatures, keyed `Trait::method`, with `Self` still rigid.
    // These are what a call through a *bound* is checked against, since which
    // impl runs is not known until monomorphization.
    for decl in source.decls() {
        let ast::Decl::Trait(t) = decl else { continue };
        let Some(name) = t.name().and_then(|n| n.ident()) else { continue };
        let own = crate::generic_names(t.type_params().as_ref());
        let mut scope = vec!["Self".to_string()];
        scope.extend(own.iter().cloned());
        for f in t.functions() {
            let Some(def) = method_def(&f, &scope, homes) else { continue };
            // `Self` first, then the trait's own parameters, then the
            // method's. The trait's belong here because they are chosen per
            // *call*, exactly as `Self` is: `Convert::convert(n)` has to solve
            // `A` from what the result is used as, and a parameter left out of
            // this list stays rigid and can only ever disagree with the answer.
            let mut generics = vec!["Self".to_string()];
            generics.extend(own.iter().cloned());
            generics.extend(def.signature.generics.iter().cloned());
            // `Self: ThisTrait` is what a default body relies on when it calls
            // another of the trait's functions on `self`. Stating it here means
            // the ordinary bound machinery discharges it, with no special case
            // anywhere else.
            let mut bounds = vec![vec![name.clone()]];
            let mut trait_bounds = crate::bound_lists(t.type_params().as_ref());
            trait_bounds.resize(own.len(), Vec::new());
            bounds.extend(trait_bounds);
            bounds.extend(def.signature.bounds.iter().cloned());
            out.insert(
                format!("{name}::{}", def.name),
                Signature { generics, bounds, ..def.signature },
            );
        }
    }

    // A type's own methods, keyed `#User::birthday`.
    for decl in source.decls() {
        let ast::Decl::Impl(i) = decl else { continue };
        if !i.is_inherent() {
            continue;
        }
        let generics = crate::generic_names(i.type_params().as_ref());
        let self_type = type_of_syntax(i.self_type().as_ref(), &generics, homes);
        let Some(head) = head_of(&self_type) else { continue };
        let mut scope = vec!["Self".to_string()];
        scope.extend(generics.iter().cloned());
        for f in i.functions() {
            let Some(def) = method_def(&f, &scope, homes) else { continue };
            let mapping: HashMap<&str, Type> =
                [("Self", self_type.clone())].into_iter().collect();
            let mut own = generics.clone();
            own.extend(def.signature.generics.iter().cloned());
            // The impl's own bounds, not empty ones. `impl<K: Hash, V> Map<K, V>`
            // says every method here may use `K`'s `Hash`, exactly as a bound
            // written on the method would — and dropping them made a bound on
            // an impl block parse, mean nothing, and say nothing about it.
            let mut bounds = crate::bound_lists(i.type_params().as_ref());
            bounds.resize(generics.len(), Vec::new());
            bounds.extend(def.signature.bounds.iter().cloned());
            out.insert(
                method_key("", &head, &def.name),
                Signature {
                    is_extern: def.signature.is_extern,
                    generics: own,
                    bounds,
                    requires: crate::unify::substitute(&def.signature.requires, &mapping),
                    raises: crate::unify::substitute(&def.signature.raises, &mapping),
                    params: def
                        .signature
                        .params
                        .iter()
                        .map(|p| crate::unify::substitute(p, &mapping))
                        .collect(),
                    ret: crate::unify::substitute(&def.signature.ret, &mapping),
                },
            );
        }
    }

    for decl in source.decls() {
        let ast::Decl::Impl(i) = decl else { continue };
        if i.is_inherent() {
            continue;
        }
        let Some(written) = i.trait_() else { continue };
        let Some(trait_name) = written_head(&written) else { continue };
        let trait_key = khora_hir::body::trait_key(&written).unwrap_or_else(|| trait_name.clone());
        let generics = crate::generic_names(i.type_params().as_ref());
        let self_type = type_of_syntax(i.self_type().as_ref(), &generics, homes);
        let Some(head) = head_of(&self_type) else { continue };

        let mut scope = vec!["Self".to_string()];
        scope.extend(generics.iter().cloned());
        for f in i.functions() {
            let Some(def) = method_def(&f, &scope, homes) else { continue };
            // Inside an impl, `Self` *is* the implementing type, so it is
            // substituted away here — nothing downstream of instance selection
            // should ever have to think about it again.
            let mapping: HashMap<&str, Type> =
                [("Self", self_type.clone())].into_iter().collect();
            // The impl's own parameters come first, because instance selection
            // solves them from the receiver before the method's own arguments
            // are known: `impl<A> Functor for Option<A>` learns `A` from the
            // receiver's type, and only then is `map<B>` instantiated.
            let mut own = generics.clone();
            own.extend(def.signature.generics.iter().cloned());
            // The impl's own bounds, not empty ones. `impl<K: Hash, V> Map<K, V>`
            // says every method here may use `K`'s `Hash`, exactly as a bound
            // written on the method would — and dropping them made a bound on
            // an impl block parse, mean nothing, and say nothing about it.
            let mut bounds = crate::bound_lists(i.type_params().as_ref());
            bounds.resize(generics.len(), Vec::new());
            bounds.extend(def.signature.bounds.iter().cloned());
            let signature = Signature {
                is_extern: def.signature.is_extern,
                generics: own,
                bounds,
                requires: crate::unify::substitute(&def.signature.requires, &mapping),
                raises: crate::unify::substitute(&def.signature.raises, &mapping),
                params: def
                    .signature
                    .params
                    .iter()
                    .map(|p| crate::unify::substitute(p, &mapping))
                    .collect(),
                ret: crate::unify::substitute(&def.signature.ret, &mapping),
            };
            out.insert(method_key(&trait_key, &head, &def.name), signature);
        }
    }
    out
}

/// The trait providing `method` for `ty`, together with its impl.
///
/// `None` when no trait in scope has such a method for this type, and the
/// error the caller reports depends on which of those two it was.
pub fn method_source<'a>(
    traits: &'a Traits,
    ty: &Type,
    method: &str,
) -> Result<(&'a TraitDef, &'a ImplDef), MethodError> {
    let candidates: Vec<(&TraitDef, &ImplDef)> = traits
        .traits
        .values()
        .filter(|t| t.method(method).is_some())
        .filter_map(|t| traits.find(&t.name, ty).map(|i| (t, i)))
        .collect();

    match candidates.len() {
        0 => {
            // Distinguish "no such method anywhere" from "the method exists but
            // this type does not implement its trait": the fixes are different.
            let owners: Vec<String> = traits
                .traits
                .values()
                .filter(|t| t.method(method).is_some())
                .map(|t| t.name.clone())
                .collect();
            if owners.is_empty() {
                Err(MethodError::Unknown)
            } else {
                Err(MethodError::NotImplemented(owners))
            }
        }
        1 => Ok(candidates[0]),
        _ => {
            let mut names: Vec<String> = candidates.iter().map(|(t, _)| t.name.clone()).collect();
            names.sort();
            Err(MethodError::Ambiguous(names))
        }
    }
}

/// Why a method could not be resolved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MethodError {
    /// No trait in scope declares a function of that name.
    Unknown,
    /// Some trait declares it, but this type implements none of them.
    NotImplemented(Vec<String>),
    /// Several traits declare it and the type implements more than one.
    Ambiguous(Vec<String>),
}

/// Every trait in `names`, plus everything they require, transitively.
///
/// `T: Ord` satisfies a bound of `Eq` because `trait Ord: Eq` says every `Ord`
/// is an `Eq`. Cycles terminate: a trait already seen is not followed again.
pub fn with_supertraits(traits: &Traits, names: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut queue: Vec<String> = names.to_vec();
    while let Some(name) = queue.pop() {
        if out.contains(&name) {
            continue;
        }
        if let Some(def) = traits.traits.get(&name) {
            queue.extend(def.supertraits.iter().cloned());
        }
        out.push(name);
    }
    out
}

/// Everything wrong with the traits and impls a file declares.
///
/// Checked here rather than during inference because none of it depends on a
/// function body: a trait is well-formed or it is not, and saying so before
/// anything is inferred keeps the diagnostics about the declaration rather than
/// about some call site that happened to touch it.
pub fn check(
    traits: &Traits,
    kinds: &HashMap<String, Kind>,
    signatures: &HashMap<String, Signature>,
    may_vouch_for: &dyn Fn(&Type) -> bool,
    declares: &dyn Fn(&Type) -> bool,
) -> Vec<HirError> {
    let mut errors = Vec::new();

    for (i, own) in traits.inherent.iter().enumerate() {
        // Two impls may cover one type — splitting methods across blocks is
        // ordinary — but one name may not be declared twice for it.
        for method in &own.methods {
            let declared = traits.inherent[..=i]
                .iter()
                .filter(|o| o.head == own.head)
                .flat_map(|o| o.methods.iter())
                .filter(|m| *m == method)
                .count();
            if declared > 1 {
                errors.push(HirError {
                    message: format!("`{}` already has a method named `{method}`", own.head),
                    range: own.range,
                });
            }
        }
    }

    for (i, imp) in traits.impls.iter().enumerate() {
        let Some(def) = traits.traits.get(&imp.trait_name) else {
            errors.push(HirError {
                message: format!("`{}` is not a trait in scope", imp.trait_name),
                range: imp.range,
            });
            continue;
        };

        // A generic trait has to be given its arguments, and exactly as many
        // as it declares. Checked before anything else reads `trait_args`,
        // because every later check pairs a parameter with an argument
        // positionally, and a short list leaves a parameter standing -- which
        // surfaces as a signature mismatch about a name the reader never wrote.
        if imp.trait_args.len() != def.type_params.len() {
            let what = imp.head().unwrap_or_else(|| "this type".to_string());
            let spelled = if def.type_params.is_empty() {
                format!("impl {} for {what}", def.name)
            } else {
                format!("impl {}<{}> for {what}", def.name, def.type_params.join(", "))
            };
            errors.push(HirError {
                message: format!(
                    "`{}` takes {} type argument(s), but this impl gives {}; write `{spelled}`",
                    def.name,
                    def.type_params.len(),
                    imp.trait_args.len()
                ),
                range: imp.range,
            });
            continue;
        }

        // One impl per trait per type -- per *arguments* too, since
        // `Convert<String>` and `Convert<Bool>` are two different promises
        // about `Int` and a program is entitled to both. The second impl at the
        // same arguments is the error, and the message names them, because
        // otherwise it describes a legal pair and an illegal one identically.
        if traits.impls[..i]
            .iter()
            .any(|o| o.trait_key == imp.trait_key && o.target() == imp.target())
        {
            let what = imp.head().unwrap_or_else(|| "this type".to_string());
            errors.push(HirError {
                message: format!(
                    "`{}` is already implemented for `{what}`; there can be only one impl \
                     of a trait for a type",
                    imp.trait_key
                ),
                range: imp.range,
            });
            continue;
        }

        // A `Share` impl *asserts* rather than provides, and everything
        // downstream trusts it without being able to check it. So it may only
        // be written where there is nothing to check: a type declared with no
        // body, whose behaviour lives in the runtime or across the C ABI.
        //
        // **Or one whose only obstacle is a `Ptr`.** A `mut` field is something
        // the compiler can see, so an assertion there overrides knowledge —
        // `impl Share` for a record with a `mut` field hands two fibers a value
        // they can both write, with the compiler's blessing. Foreign memory is
        // the opposite: `false` is a conservative default rather than a
        // finding, and the module that put the pointer across the ABI is the
        // only thing that knows what is behind it.
        // **Only the file that declares a type may assert its shareability.**
        // Otherwise the marker is forgeable: declare a trait of your own
        // spelled `Share`, write `impl<A> Share for Array<A>`, and an array —
        // which `Array::set` writes — becomes something two fibers may hold.
        //
        // An imported impl is skipped rather than re-judged: it was checked
        // where it was written, and asking again from here would answer
        // differently.
        if imp.trait_name == crate::SHARE && imp.local && !declares(&imp.self_type) {
            let what = imp.head().unwrap_or_else(|| "this type".to_string());
            errors.push(HirError {
                message: format!(
                    "`Share` cannot be implemented for `{what}` here: it is declared in \
                     another module, and only the module that declares a type may \
                     assert that two fibers may hold it"
                ),
                range: imp.range,
            });
            continue;
        }
        if imp.trait_name == crate::SHARE && imp.local && !may_vouch_for(&imp.self_type) {
            let what = imp.head().unwrap_or_else(|| "this type".to_string());
            errors.push(HirError {
                message: format!(
                    "`Share` cannot be implemented for `{what}`: this compiler can see \
                     what `{what}` holds, so it decides for itself whether two fibers may \
                     have it. An impl is for a type declared with no body, or one whose \
                     only obstacle is a `Ptr` — foreign memory nothing here can judge"
                ),
                range: imp.range,
            });
            continue;
        }

        check_kind(imp, def, kinds, &mut errors);
        check_methods(imp, def, &mut errors);
        check_assoc_types(imp, def, &mut errors);
        check_signatures(imp, traits, signatures, &mut errors);
    }

    errors
}

/// The kind left after applying `n` arguments.
fn kind_after(kind: &Kind, n: usize) -> Kind {
    let mut current = kind;
    for _ in 0..n {
        match current {
            Kind::Arrow(_, rest) => current = rest,
            _ => break,
        }
    }
    current.clone()
}

/// A trait that applies `Self` cannot be implemented for a type that takes no
/// arguments, and vice versa.
fn check_kind(
    imp: &ImplDef,
    def: &TraitDef,
    kinds: &HashMap<String, Kind>,
    errors: &mut Vec<HirError>,
) {
    let wanted = &def.self_kind;
    let Some(head) = imp.head() else { return };
    let Some(declared) = kinds.get(&head) else { return };

    // `Option<A>` is `Option` applied once: the written arguments have already
    // discharged that much of the constructor's kind.
    let applied = match &imp.self_type {
        Type::Adt { args, .. } => args.len(),
        _ => 0,
    };
    if applied > declared.arity() {
        errors.push(HirError {
            message: format!(
                "`{head}` takes {} type argument(s), but {applied} were given",
                declared.arity()
            ),
            range: imp.range,
        });
        return;
    }
    // What is left of the constructor after the written arguments. Built by
    // stripping arrows rather than by counting them: `Vector<const N: Int>` has
    // kind `Int -> *`, and rebuilding from an arity would forget the `Int` and
    // let it stand in for a `* -> *` trait.
    let remaining = kind_after(declared, applied);

    if &remaining != wanted {
        // Naming the type *as written* matters here: `Option` and `Option<A>`
        // have different kinds, and the fix is usually to drop the arguments.
        let head = &imp.self_type;
        let hint = if applied > 0 && wanted.arity() == declared.arity() {
            format!("; write `impl {} for {}`", def.name, imp.head().unwrap_or_default())
        } else {
            String::new()
        };
        errors.push(HirError {
            message: format!(
                "`{}` is implemented for a type of kind `{wanted}`, but `{head}` has kind \
                 `{remaining}`{hint}",
                def.name
            ),
            range: imp.range,
        });
    }
}

fn check_methods(imp: &ImplDef, def: &TraitDef, errors: &mut Vec<HirError>) {
    let missing: Vec<&str> = def
        .methods
        .iter()
        .filter(|m| !m.has_default && !imp.methods.iter().any(|n| n == &m.name))
        .map(|m| m.name.as_str())
        .collect();
    if !missing.is_empty() {
        errors.push(HirError {
            message: format!(
                "this impl is missing `{}` from `{}`",
                missing.join("`, `"),
                def.name
            ),
            range: imp.range,
        });
    }

    for name in &imp.methods {
        if def.method(name).is_none() {
            errors.push(HirError {
                message: format!("`{}` has no function named `{name}`", def.name),
                range: imp.range,
            });
        }
    }
}

/// A trait's own type parameters, or none for a name that is not a trait here.
///
/// An unknown trait is reported by [`check`] before anything reaches this, so
/// the empty answer only ever feeds a check that is already going to be quiet.
fn trait_params<'a>(traits: &'a Traits, name: &str) -> &'a [String] {
    traits.traits.get(name).map(|d| d.type_params.as_slice()).unwrap_or(&[])
}

/// Every method an impl declares must have the signature the trait promised.
///
/// `impl_signatures` deliberately reads what the impl *wrote* rather than what
/// the trait promised, so a disagreement is a diagnosable difference. This is
/// what reads it: without the check, a trait promising `-> Bool` against an
/// impl returning `Int` surfaces as invalid LLVM IR blamed on the compiler.
fn check_signatures(
    imp: &ImplDef,
    traits: &Traits,
    signatures: &HashMap<String, Signature>,
    errors: &mut Vec<HirError>,
) {
    let Some(head) = imp.head() else { return };
    let normalizer = crate::unify::Unifier::new().with_assoc(traits.assoc_bindings());

    for method in &imp.methods {
        let Some(declared) = signatures.get(&format!("{}::{}", imp.trait_name, method)) else {
            continue;
        };
        let Some(written) = signatures.get(&method_key(&imp.trait_key, &head, method)) else {
            continue;
        };

        // Put both sides in the same names: `Self` becomes the implementing
        // type, the trait's own parameters become the arguments this impl gave
        // them, and the trait's method parameters take the impl's spelling of
        // them, so `fn map<A, B>` and `fn map<X, Y>` compare equal.
        //
        // The middle one is what makes `impl Convert<String> for Int` check at
        // all: `A` is answered by `String` here, and comparing the trait's
        // `-> A` against the impl's `-> String` without that substitution can
        // only ever report a difference. `impl<A> Convert<A> for Wrapper` goes
        // through the same line and maps `A` to the impl's own `A`, which is
        // the parameter it already was.
        let mut mapping: HashMap<&str, Type> = HashMap::new();
        mapping.insert("Self", imp.self_type.clone());
        let params = trait_params(traits, &imp.trait_name);
        for (param, arg) in params.iter().zip(&imp.trait_args) {
            mapping.insert(param.as_str(), arg.clone());
        }
        // `Self`, then the trait's own parameters, then the method's: the
        // order `impl_signatures` writes them in.
        let trait_own = declared.generics.get(1 + params.len()..).unwrap_or(&[]);
        let impl_own = written.generics.get(imp.generics.len()..).unwrap_or(&[]);
        for (from, to) in trait_own.iter().zip(impl_own) {
            mapping.insert(from.as_str(), Type::Param(to.clone()));
        }

        let expect = |ty: &Type| normalizer.zonk(&crate::unify::substitute(ty, &mapping));

        if declared.params.len() != written.params.len() {
            errors.push(HirError {
                message: format!(
                    "`{method}` takes {} parameter(s) in `{}`, but this impl declares {}",
                    declared.params.len(),
                    imp.trait_name,
                    written.params.len()
                ),
                range: imp.range,
            });
            continue;
        }

        for (i, (want, got)) in declared.params.iter().zip(&written.params).enumerate() {
            let want = expect(want);
            if &want != got {
                let which = if i == 0 {
                    "the receiver of".to_string()
                } else {
                    format!("parameter {} of", i + 1)
                };
                errors.push(HirError {
                    message: format!(
                        "{which} `{method}` is `{got}` here, but `{}` declares `{want}`",
                        imp.trait_name
                    ),
                    range: imp.range,
                });
            }
        }

        let want = expect(&declared.ret);
        if want != written.ret {
            errors.push(HirError {
                message: format!(
                    "`{method}` returns `{}` here, but `{}` declares `{want}`",
                    written.ret, imp.trait_name
                ),
                range: imp.range,
            });
        }
    }
}

fn check_assoc_types(imp: &ImplDef, def: &TraitDef, errors: &mut Vec<HirError>) {
    let missing: Vec<&str> = def
        .assoc_types
        .iter()
        .filter(|n| !imp.assoc_types.iter().any(|(m, _)| m == *n))
        .map(String::as_str)
        .collect();
    if !missing.is_empty() {
        errors.push(HirError {
            message: format!(
                "this impl is missing the associated type `{}` from `{}`",
                missing.join("`, `"),
                def.name
            ),
            range: imp.range,
        });
    }

    for (name, _) in &imp.assoc_types {
        if !def.assoc_types.contains(name) {
            errors.push(HirError {
                message: format!("`{}` has no associated type named `{name}`", def.name),
                range: imp.range,
            });
        }
    }
}
