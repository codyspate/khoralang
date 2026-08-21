//! Monomorphisation: turning generic functions into concrete ones.
//!
//! Code generation has no representation for a type variable — `llvm_type`
//! returns nothing for one, and rightly so, since there is no machine type that
//! is "some `A`". Rather than box every generic value and pay for it forever,
//! Khora emits one copy of a generic function per set of type arguments it is
//! actually used at. Abstraction costs nothing at runtime, which is the promise
//! in `docs/vision.md`.
//!
//! # How the instances are found
//!
//! Reachability, not enumeration. Starting from the functions that are already
//! concrete, each body says which generic functions it mentions and at what
//! arguments — recorded by the checker, which is the only pass that knows,
//! since it created those variables and solved them.
//!
//! Walking a generic body substitutes the instance's own arguments first, so
//! `pair<A>` calling `id<A>` from inside `pair@Int` asks for `id@Int` and not
//! for `id@A`.
//!
//! # What is deliberately not here
//!
//! Recursion through a *changing* type argument — a `f<A>` that calls
//! `f<List<A>>` — generates instances without end. Polymorphic recursion is
//! rejected with a diagnostic rather than hanging the compiler, which is what
//! the depth limit is for.

use std::collections::{HashMap, HashSet};

use khora_db::{Db, SourceFile};
use khora_hir::HirError;
use text_size::TextRange;

use crate::traits::{self, Traits};
use crate::{unify, BodyTypes, Type, TypeMap};

/// How deep a chain of generic calls may go before we call it non-terminating.
///
/// Generous: real code nests a handful deep. Anything past this is polymorphic
/// recursion, which has no finite set of instances.
const MAX_DEPTH: usize = 64;

/// One function, specialised at one set of type arguments.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Instance {
    pub function: String,
    pub args: Vec<Type>,
}

impl Instance {
    /// The symbol this instance is emitted under.
    ///
    /// A non-generic function keeps its own name, so the common case produces
    /// exactly what it did before monomorphisation existed.
    pub fn symbol(&self) -> String {
        if self.args.is_empty() {
            return self.function.clone();
        }
        let args: Vec<String> = self.args.iter().map(mangle).collect();
        format!("{}${}", self.function, args.join("$"))
    }
}

/// A type as it appears in a symbol name.
fn mangle(ty: &Type) -> String {
    match ty {
        Type::Int => "Int".to_string(),
        Type::Bool => "Bool".to_string(),
        Type::Str => "String".to_string(),
        Type::Unit => "Unit".to_string(),
        Type::Adt { name, args } if args.is_empty() => name.clone(),
        Type::Adt { name, args } => {
            let inner: Vec<String> = args.iter().map(mangle).collect();
            format!("{name}${}", inner.join("$"))
        }
        Type::Tuple(items) => {
            let inner: Vec<String> = items.iter().map(mangle).collect();
            format!("Tuple{}${}", items.len(), inner.join("$"))
        }
        Type::Const(n) if *n < 0 => format!("_neg{}", -n),
        Type::Const(n) => n.to_string(),
        // An unsolved argument still has to mangle to something unique, since
        // two different variables both print as `_`.
        Type::Assoc { owner, name } => format!("_proj{}${name}", mangle(owner)),
        Type::Var(v) => format!("_var{v}"),
        // An application that survived here means `Self` was never chosen,
        // which is a bug in instance selection rather than in the program.
        Type::Applied { head, args } => {
            let inner: Vec<String> = args.iter().map(mangle).collect();
            format!("_app{}${}", mangle(head), inner.join("$"))
        }
        // Reaching here means an argument was never solved. The instance is
        // still distinct from every other, so a stable placeholder is enough to
        // keep symbols unique; the diagnostic comes from elsewhere.
        other => format!("_{other}").replace(['<', '>', ',', ' ', '?'], "_"),
    }
}

/// Turns a call through a trait into a call to the impl that answers it.
///
/// A method mention records the trait's key — `Show::show` — with `Self` as its
/// first type argument. That argument is what selects the impl, which is the
/// whole of nominal resolution: a name, never a shape. See
/// `docs/design/typeclasses.md`.
///
/// Returns `None` when the mention is not a trait method, or when `Self` is
/// still unsolved, in which case the caller has nothing to emit and the
/// diagnostic has already been raised elsewhere.
pub fn select_impl(types: &TypeMap, instance: &Instance) -> Option<Instance> {
    let (trait_name, method) = instance.function.split_once("::")?;
    // `#User::birthday` is already the thing to call: an inherent method has no
    // trait to resolve through and no impl to choose between.
    if trait_name.starts_with('#') {
        return None;
    }
    let def = types.traits.traits.get(trait_name)?;
    def.method(method)?;

    let self_ty = instance.args.first()?;
    let imp = types.traits.find(trait_name, self_ty)?;
    let head = imp.head()?;

    // An impl that leaves a function out is taking the trait's default, and the
    // default body is what runs. Keeping the trait's own key here is what makes
    // that work with no further machinery: the body and the signature are both
    // already recorded under it.
    if !imp.methods.iter().any(|m| m == method) {
        return None;
    }

    // The impl's own parameters are solved from the receiver: matching
    // `Option<A>` against `Option<Int>` is what tells this instance that
    // `A = Int`.
    let mut solved = std::collections::HashMap::new();
    if !unify::match_params(&imp.self_type, self_ty, &imp.generics, &mut solved) {
        return None;
    }
    let mut args: Vec<Type> = imp
        .generics
        .iter()
        .map(|g| solved.get(g).cloned().unwrap_or(Type::Unknown))
        .collect();
    args.extend(instance.args.iter().skip(1).cloned());

    Some(Instance { function: traits::method_key(trait_name, &head, method), args })
}

/// Whether a name refers to a trait's own function rather than to an impl's.
fn is_trait_method(traits: &Traits, name: &str) -> bool {
    match name.split_once("::") {
        Some((t, m)) => traits.traits.get(t).is_some_and(|d| d.method(m).is_some()),
        None => false,
    }
}

/// Every specialisation a file needs, with the types each one is compiled at.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Instances {
    pub instances: Vec<(Instance, BodyTypes)>,
    pub errors: Vec<HirError>,
    /// The symbol each trait-method mention was resolved to.
    ///
    /// Recorded during the walk rather than recomputed at each use, so that
    /// what code generation emits and what monomorphisation decided cannot
    /// drift apart.
    pub resolved: HashMap<Instance, String>,
}

impl Instances {
    pub fn get(&self, instance: &Instance) -> Option<&BodyTypes> {
        self.instances.iter().find(|(i, _)| i == instance).map(|(_, t)| t)
    }

    /// The symbol a mention at `site` inside `from` should call.
    ///
    /// `None` only when the mention was never recorded, which means it is not a
    /// call through a signature at all.
    pub fn callee(&self, from: &BodyTypes, site: khora_hir::body::ExprId) -> Option<String> {
        let (function, args) = from.instantiation(site)?;
        // No early return for an empty argument list: `Instance::symbol` already
        // answers the function's own name in that case, and a method call has no
        // other name to fall back on — the callee is a field access, not a path.
        let wanted = Instance { function: function.clone(), args: args.clone() };
        Some(self.resolved.get(&wanted).cloned().unwrap_or_else(|| wanted.symbol()))
    }
}

/// Computes the specialisations a file needs.
#[salsa::tracked(returns(ref))]
pub fn instances(db: &dyn Db, file: SourceFile) -> Instances {
    let types = crate::type_map(db, file);
    let checked = crate::checked(db, file);
    let traits = &types.traits;
    let by_name: HashMap<&str, &BodyTypes> =
        checked.bodies.iter().map(|(n, t)| (n.as_str(), t)).collect();

    let mut out = Instances::default();
    let mut seen: HashSet<Instance> = HashSet::new();

    // Roots: every function that is already concrete. A generic function with
    // no use has no instances, which is the right answer — there is nothing to
    // emit for a shape nobody asked for.
    let mut queue: Vec<(Instance, usize)> = checked
        .bodies
        .iter()
        .filter(|(name, _)| !is_trait_method(traits, name))
        .filter(|(name, _)| {
            types.signatures.get(name.as_str()).is_none_or(|s| s.generics.is_empty())
        })
        .map(|(name, _)| (Instance { function: name.clone(), args: Vec::new() }, 0))
        .collect();

    while let Some((instance, depth)) = queue.pop() {
        if !seen.insert(instance.clone()) {
            continue;
        }
        if depth > MAX_DEPTH {
            out.errors.push(HirError {
                message: format!(
                    "`{}` needs endlessly many specialisations; a generic function \
                     that calls itself at a larger type cannot be compiled",
                    instance.function
                ),
                range: TextRange::empty(0.into()),
            });
            continue;
        }

        let Some(generic) = by_name.get(instance.function.as_str()) else { continue };
        let generics = types
            .signatures
            .get(instance.function.as_str())
            .map(|s| s.generics.clone())
            .unwrap_or_default();

        // The body's own parameters, bound to what this instance was asked for.
        let mapping: HashMap<&str, Type> = generics
            .iter()
            .zip(&instance.args)
            .map(|(g, a)| (g.as_str(), a.clone()))
            .collect();
        let specialised = generic.specialised(&mapping);

        for (_, (callee, args)) in specialised.instantiations() {
            if args.is_empty() {
                continue;
            }
            let resolved: Vec<Type> =
                args.iter().map(|a| unify::substitute(a, &mapping)).collect();
            let mention = Instance { function: callee.clone(), args: resolved };

            // A call written against a trait is emitted as a call to the impl.
            let target = match select_impl(types, &mention) {
                Some(chosen) => {
                    out.resolved.insert(mention, chosen.symbol());
                    chosen
                }
                None => mention,
            };
            queue.push((target, depth + 1));
        }

        out.instances.push((instance, specialised));
    }

    // Deterministic order: the same source must produce the same object file.
    out.instances.sort_by_key(|(i, _)| i.symbol());
    out
}
