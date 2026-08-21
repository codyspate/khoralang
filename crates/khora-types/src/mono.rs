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

use crate::{unify, BodyTypes, Type};

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
        Type::Var(v) => format!("_var{v}"),
        // Reaching here means an argument was never solved. The instance is
        // still distinct from every other, so a stable placeholder is enough to
        // keep symbols unique; the diagnostic comes from elsewhere.
        other => format!("_{other}").replace(['<', '>', ',', ' ', '?'], "_"),
    }
}

/// Every specialisation a file needs, with the types each one is compiled at.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Instances {
    pub instances: Vec<(Instance, BodyTypes)>,
    pub errors: Vec<HirError>,
}

impl Instances {
    pub fn get(&self, instance: &Instance) -> Option<&BodyTypes> {
        self.instances.iter().find(|(i, _)| i == instance).map(|(_, t)| t)
    }

    /// The symbol a mention at `site` inside `from` should call.
    ///
    /// Returns `None` when the mention is not of a generic function, in which
    /// case the callee's own name is the symbol.
    pub fn callee(&self, from: &BodyTypes, site: khora_hir::body::ExprId) -> Option<String> {
        let (function, args) = from.instantiation(site)?;
        if args.is_empty() {
            return None;
        }
        Some(Instance { function: function.clone(), args: args.clone() }.symbol())
    }
}

/// Computes the specialisations a file needs.
#[salsa::tracked(returns(ref))]
pub fn instances(db: &dyn Db, file: SourceFile) -> Instances {
    let types = crate::type_map(db, file);
    let checked = crate::checked(db, file);
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
            queue.push((
                Instance { function: callee.clone(), args: resolved },
                depth + 1,
            ));
        }

        out.instances.push((instance, specialised));
    }

    // Deterministic order: the same source must produce the same object file.
    out.instances.sort_by_key(|(i, _)| i.symbol());
    out
}
