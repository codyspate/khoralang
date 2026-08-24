//! Monomorphization: turning generic functions into concrete ones.
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

use khora_db::{Db, SourceFile, SourceRoot};
use khora_hir::{HirError, ModulePath};
use text_size::TextRange;

use crate::traits::{self, Traits};
use crate::{unify, BodyTypes, Type, TypeMap};

/// How deep a chain of generic calls may go before we call it non-terminating.
///
/// Generous: real code nests a handful deep. Anything past this is polymorphic
/// recursion, which has no finite set of instances.
const MAX_DEPTH: usize = 64;

/// One function, specialized at one set of type arguments.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Instance {
    /// The module whose source defines this function.
    ///
    /// Part of the identity, not decoration: two modules may each declare a
    /// `helper`, and a whole-program compilation emits both.
    pub module: ModulePath,
    pub function: String,
    pub args: Vec<Type>,
}

impl Instance {
    /// The symbol this instance is emitted under: `std$core$unwrap_or$Int`.
    ///
    /// Qualified by the *defining* module so that two importers of one
    /// instantiation agree on a name and it is emitted once, and so that two
    /// modules may each declare a `helper` without colliding.
    pub fn symbol(&self) -> String {
        let mut out = String::new();
        for segment in self.module.segments() {
            out.push_str(segment);
            out.push('$');
        }
        out.push_str(&self.function);
        if !self.args.is_empty() {
            let args: Vec<String> = self.args.iter().map(mangle).collect();
            out.push('$');
            out.push_str(&args.join("$"));
        }
        out
    }
}

/// A type as it appears in a symbol name.
fn mangle(ty: &Type) -> String {
    match ty {
        Type::Int => "Int".to_string(),
        Type::Fixed(kind) => kind.name(),
        Type::Ptr => "Ptr".to_string(),
        Type::Bool => "Bool".to_string(),
        Type::Str => "String".to_string(),
        Type::Unit => "Unit".to_string(),
        // The module is part of the name here for the reason it is part of the
        // type: two modules may each declare a `Point`, and a symbol that only
        // carried the spelling gave both instantiations one name and emitted
        // one of them. Errata 46. Segments are joined with `$` like everything
        // else in a mangled name, since `:` is not a symbol character
        // everywhere Khora will eventually link.
        Type::Adt { name, home, args } => {
            let mut mangled = match home {
                Some(home) => format!("{}${name}", home.segments().join("$")),
                None => name.clone(),
            };
            if !args.is_empty() {
                let inner: Vec<String> = args.iter().map(mangle).collect();
                mangled = format!("{mangled}${}", inner.join("$"));
            }
            mangled
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

    // The module is the mention's; the caller replaces it with whichever
    // module actually defines the impl.
    Some(Instance {
        module: instance.module.clone(),
        function: traits::method_key(trait_name, &head, method),
        args,
    })
}

/// [`select_impl`] against the whole program's traits, then the file's own.
///
/// The file first, so that a type with a local impl resolves to it without a
/// search; the program second, because an impl for a type this file never saw
/// is the ordinary case for anything generic in a library.
fn select_impl_in(whole: &Traits, local: &TypeMap, mention: &Instance) -> Option<Instance> {
    select_impl(local, mention).or_else(|| {
        let borrowed = TypeMap { traits: whole.clone(), ..local.clone() };
        select_impl(&borrowed, mention)
    })
}

/// Whether a name refers to a trait's own function rather than to an impl's.
fn is_trait_method(traits: &Traits, name: &str) -> bool {
    match name.split_once("::") {
        Some((t, m)) => traits.traits.get(t).is_some_and(|d| d.method(m).is_some()),
        None => false,
    }
}

/// Every specialization a file needs, with the types each one is compiled at.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Instances {
    pub instances: Vec<(Instance, BodyTypes)>,
    pub errors: Vec<HirError>,
    /// The symbol each trait-method mention was resolved to.
    ///
    /// Recorded during the walk rather than recomputed at each use, so that
    /// what code generation emits and what monomorphization decided cannot
    /// drift apart.
    pub resolved: HashMap<Instance, String>,
    /// The file each emitted symbol's body lives in.
    ///
    /// Code generation cannot find it otherwise: an instance reached through
    /// an import is defined in a module the emitting file never parsed.
    pub home: HashMap<String, SourceFile>,
    /// What each call site resolves to, by `(emitting symbol, call site)`.
    ///
    /// Resolving a mention needs the *scope it was written in* to say which
    /// module defines it, which only this walk knows. Recording the answer
    /// keeps code generation from having to reconstruct it.
    pub calls: HashMap<(String, khora_hir::body::ExprId), String>,
}

impl Instances {
    pub fn get(&self, instance: &Instance) -> Option<&BodyTypes> {
        self.instances.iter().find(|(i, _)| i == instance).map(|(_, t)| t)
    }

    /// The symbol the call at `site` inside `owner` should target.
    ///
    /// `None` when the site is not a call through a signature at all.
    pub fn callee(&self, owner: &str, site: khora_hir::body::ExprId) -> Option<String> {
        self.calls.get(&(owner.to_string(), site)).cloned()
    }

    /// The file defining the body emitted under `symbol`.
    pub fn home(&self, symbol: &str) -> Option<SourceFile> {
        self.home.get(symbol).copied()
    }
}

/// Everything one compilation needs to know about a file while walking it.
struct Unit<'a> {
    module: ModulePath,
    file: SourceFile,
    types: &'a TypeMap,
    checked: &'a crate::Checked,
    scope: &'a khora_hir::FileScope,
}

impl Unit<'_> {
    fn body(&self, name: &str) -> Option<&BodyTypes> {
        self.checked.bodies.iter().find(|(n, _)| n == name).map(|(_, t)| t)
    }
}

/// Computes the specializations a single file needs.
///
/// Kept for callers that check one file in isolation. A whole compilation goes
/// through [`program_instances`], which is the same walk over every module.
#[salsa::tracked(returns(ref))]
pub fn instances(db: &dyn Db, file: SourceFile) -> Instances {
    walk(db, &[file])
}

/// Computes the specializations a whole program needs.
///
/// **Whole-program, not per-file.** A generic function is compiled by
/// substituting its type arguments into its body, so the body has to be
/// available wherever it is instantiated — a module importing `Option` gets
/// `unwrap_or` specialized from `std::core`'s source, not from an object file.
/// That is the constraint C++ templates and Rust generics have too, and it is
/// why a symbol carries the module that *defines* it: two importers of one
/// instantiation must agree on a symbol so it is emitted once.
#[salsa::tracked(returns(ref))]
pub fn program_instances(db: &dyn Db, root: SourceRoot) -> Instances {
    walk(db, root.files(db))
}

fn walk(db: &dyn Db, files: &[SourceFile]) -> Instances {
    let units: Vec<Unit<'_>> = files
        .iter()
        .map(|f| Unit {
            module: khora_hir::item_map(db, *f)
                .module
                .clone()
                .unwrap_or_else(|| ModulePath::new(Vec::new())),
            file: *f,
            types: crate::type_map(db, *f),
            checked: crate::checked(db, *f),
            scope: khora_hir::file_scope(db, *f),
        })
        .collect();

    // **Which impl a call resolves to is a whole-program question.** A generic
    // function is compiled once per type it is used at, and the type — with its
    // impls — is very often in a module the generic has never heard of:
    // `std::ai`'s `extract<A: Extract>` is specialized at an `AnalysisReport`
    // declared by the application, and `std::ai` cannot see that impl.
    //
    // Looking only in the file the *body* lives in is therefore wrong in the
    // direction that matters most, and it failed the way a missing impl always
    // fails here: the trait's own declaration has no body, so the code
    // generator said there was nothing to call.
    //
    // Merged rather than searched unit by unit, because a trait's declaration
    // and its impls are routinely in different files and `select_impl` needs
    // both at once.
    let mut whole = Traits::default();
    for unit in &units {
        for (name, def) in &unit.types.traits.traits {
            whole.traits.entry(name.clone()).or_insert_with(|| def.clone());
        }
        whole.impls.extend(unit.types.traits.impls.iter().cloned());
        whole.inherent.extend(unit.types.traits.inherent.iter().cloned());
    }

    let mut out = Instances::default();
    let mut seen: HashSet<Instance> = HashSet::new();

    // Roots: every function that is already concrete, in every module. A
    // generic function with no use has no instances, which is the right answer
    // — there is nothing to emit for a shape nobody asked for.
    let mut queue: Vec<(Instance, usize, usize)> = Vec::new();
    for (index, unit) in units.iter().enumerate() {
        for (name, _) in &unit.checked.bodies {
            if is_trait_method(&unit.types.traits, name) {
                continue;
            }
            if unit.types.signatures.get(name.as_str()).is_some_and(|s| !s.generics.is_empty()) {
                continue;
            }
            queue.push((
                Instance {
                    module: unit.module.clone(),
                    function: name.clone(),
                    args: Vec::new(),
                },
                index,
                0,
            ));
        }
    }

    while let Some((instance, index, depth)) = queue.pop() {
        if !seen.insert(instance.clone()) {
            continue;
        }
        let unit = &units[index];
        if depth > MAX_DEPTH {
            out.errors.push(HirError {
                message: format!(
                    "`{}` needs endlessly many specializations; a generic function \
                     that calls itself at a larger type cannot be compiled",
                    instance.function
                ),
                range: TextRange::empty(0.into()),
            });
            continue;
        }

        let Some(generic) = unit.body(&instance.function) else { continue };
        let generics = unit
            .types
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
        let specialized = generic.specialized(&mapping);
        let owner = instance.symbol();

        for (site, (callee, args)) in specialized.instantiations() {
            let resolved: Vec<Type> =
                args.iter().map(|a| unify::substitute(a, &mapping)).collect();

            // A call written against a trait is emitted as a call to the impl.
            let mention = Instance {
                module: unit.module.clone(),
                function: callee.clone(),
                args: resolved.clone(),
            };
            let (name, args) = match select_impl_in(&whole, unit.types, &mention) {
                Some(chosen) => (chosen.function, chosen.args),
                None => (callee.clone(), resolved),
            };

            // Which module defines it decides the symbol, and a name reached
            // through an import is defined somewhere this file never parsed.
            match defining(&units, index, &name) {
                Some((home, original)) => {
                    let target =
                        Instance { module: units[home].module.clone(), function: original, args };
                    out.calls.insert((owner.clone(), *site), target.symbol());
                    out.resolved.insert(mention, target.symbol());
                    queue.push((target, home, depth + 1));
                }
                // Nothing defines it in Khora, so it is a C symbol the file
                // declared — the runtime's `print`, say — called by its own
                // name.
                None => {
                    out.calls.insert((owner.clone(), *site), name);
                }
            }
        }

        out.home.insert(owner, unit.file);
        out.instances.push((instance, specialized));
    }

    // Deterministic order: the same source must produce the same object file.
    out.instances.sort_by_key(|(i, _)| i.symbol());
    out
}

/// Which unit defines `name`, as seen from the unit at `from`, and what that
/// module calls it.
///
/// The second half matters because of aliases: `import lib::{double as twice}`
/// records mentions under `twice`, and the body is `double`.
fn defining(units: &[Unit<'_>], from: usize, name: &str) -> Option<(usize, String)> {
    if units[from].body(name).is_some() {
        return Some((from, name.to_string()));
    }

    // A name the file imported, under whatever it calls it. The body lives
    // under the *defining* module's spelling, which an alias changes.
    if let Some(origin) = units[from].scope.origin(name) {
        if let Some(home) = units.iter().position(|u| u.module == origin.module) {
            if units[home].body(&origin.name).is_some() {
                return Some((home, origin.name.clone()));
            }
        }
    }

    // A trait or impl method carries a compound key of its own —
    // `Show#Int::show`, or `#Int::show` for an inherent one — which no import
    // names. It traveled in with its trait, so look for the key itself in the
    // other modules.
    //
    // **Only for compound keys.** This used to search every unit for any name,
    // which meant a name the calling file neither defines nor imported was
    // matched against whatever module happened to declare one first. That is
    // wrong for exactly the case it looks harmless in: an `extern fn` is a
    // declaration with no body, so `extern fn close(handle: I32) -> I32` in
    // `socket_linux.kh` fell past both branches above and bound to the private
    // `close(file: Ptr)` in `std::fs`. Every POSIX build emitted
    // `call void @kh$std$fs$close(i32 %handle)` and LLVM rejected the module.
    //
    // Windows hid it for as long as it existed, because `socket_windows.kh`
    // spells the same call `closesocket`. `khora-codegen-llvm/tests/portability.rs`
    // is what makes that class of bug visible from any host now.
    //
    // A bare name that this file neither defines nor imported is not a Khora
    // function. It is a C symbol the file declared, and the caller's `None`
    // branch already does the right thing with it.
    if !name.contains('#') {
        return None;
    }

    units
        .iter()
        .enumerate()
        .find(|(index, unit)| *index != from && unit.body(name).is_some())
        .map(|(index, _)| (index, name.to_string()))
}
