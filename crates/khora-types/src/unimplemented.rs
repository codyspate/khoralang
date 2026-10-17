//! A function declared without a body, and used where code generation will
//! emit the use.
//!
//! **What this prevents: `khora check` passing a program `khora build`
//! refuses.** A signature written ahead of its implementation is a useful
//! thing to have and not a thing to call. Code generation refuses the call --
//! "`helper` has no body, so there is nothing to call" -- and it is right to,
//! but `khora check` stops before code generation, so the program checked
//! clean, the editor stayed green, and the build failed.
//!
//! **The uses the build reaches, by the build's own rule.** Code generation
//! emits every concrete function and a generic one only when something
//! reaches it ([`crate::mono`]'s roots). So a use in a concrete body is
//! refused, and a use in a generic body only when a chain of calls from a
//! concrete body reaches that generic -- `fn later<A>(a: A) -> Int {
//! helper(1) }` with nothing calling `later` builds and runs, and is not
//! refused. Methods count: `impl P { fn twice(self) -> Int; }` called as
//! `p.twice()` fails to build the same way.
//!
//! Not the specialization walk itself, though it would be exact: it made
//! `khora check` of a ten-line program take 12 s instead of 1.6 s in a debug
//! build, and an editor runs `check` on every keystroke. What this costs
//! instead is a pass over the bodies reachable from concrete ones, once per
//! source root, following path calls and the method calls the checker
//! recorded. It does not resolve which impl a call through a trait bound
//! selects, so a generic impl reached only that way is not followed; a call
//! to a bodiless function inside one passes `check` and fails the build, as
//! every such call did before.
//!
//! Not `std`: its bodiless functions are the intrinsics, which code
//! generation writes at each call site, and a program cannot add one. A
//! program's own bodiless `print`, `assert`, `assert_that` or `attempt` is
//! one too when it is called with the arguments code generation lowers by
//! name -- `examples/core_demo` declares its own `print` -- and only then. So
//! is a bodiless method on a type whose operations code generation lowers by
//! the type's name (`Ptr`, `String`, `Array`, the integers, ...), whichever
//! module declares the `impl`; those are left to the build.

use std::collections::HashSet;

use khora_db::{Db, SourceFile, SourceRoot};
use khora_hir::body::{Body, Expr, ExprId};
use khora_hir::{HirError, ItemKind, ModulePath, Resolution};
use khora_syntax::ast;

use crate::{BodyTypes, Type};

/// Every bodiless function a program declares outside `std` and without
/// `extern`: free functions by `(module, name)`, and an impl's methods by
/// `(module, key)` with the key a method call records (`#P::twice`).
#[salsa::tracked(returns(ref))]
pub fn unimplemented_functions(db: &dyn Db, root: SourceRoot) -> HashSet<(ModulePath, String)> {
    let mut out = HashSet::new();
    for file in root.files(db) {
        let Some(module) = khora_hir::item_map(db, *file).module.clone() else { continue };
        if module.segments().first().is_some_and(|first| first == "std") {
            continue;
        }
        for decl in khora_db::parse(db, *file).source_file().decls() {
            match decl {
                ast::Decl::Fn(f) if !f.is_extern() && f.body().is_none() => {
                    if let Some(name) = f.name().and_then(|n| n.ident()) {
                        out.insert((module.clone(), name));
                    }
                }
                ast::Decl::Impl(imp) => {
                    let key = khora_hir::body::impl_key(&imp);
                    let head = key.rsplit('#').next().unwrap_or_default();
                    if lowered_by_owner(head) {
                        continue;
                    }
                    for f in imp.functions().filter(|f| f.body().is_none()) {
                        if let Some(name) = f.name().and_then(|n| n.ident()) {
                            out.insert((module.clone(), format!("{key}::{name}")));
                        }
                    }
                }
                _ => {}
            }
        }
    }
    out
}

/// Whether `name`, a body in `file`, is a generic function: one code
/// generation emits only when something reaches it.
fn is_generic(db: &dyn Db, file: SourceFile, name: &str) -> bool {
    crate::type_map(db, file).signatures.get(name).is_some_and(|s| !s.generics.is_empty())
}

/// The generic functions a concrete function reaches through calls, by
/// `(module, name)`: the ones code generation emits.
#[salsa::tracked(returns(ref))]
pub fn reached_generics(db: &dyn Db, root: SourceRoot) -> HashSet<(ModulePath, String)> {
    let file_of = |module: &ModulePath| khora_hir::module_graph(db, root).file(module);

    let mut reached: HashSet<(ModulePath, String)> = HashSet::new();
    let mut queue: Vec<(SourceFile, String)> = Vec::new();
    for file in root.files(db) {
        for (name, _) in khora_hir::body::bodies(db, *file) {
            if !is_generic(db, *file, name) {
                queue.push((*file, name.clone()));
            }
        }
    }
    let mut visited: HashSet<(SourceFile, String)> = HashSet::new();
    while let Some((file, name)) = queue.pop() {
        if !visited.insert((file, name.clone())) {
            continue;
        }
        let Some((_, body)) = khora_hir::body::bodies(db, file).iter().find(|(n, _)| *n == name)
        else {
            continue;
        };
        for (_, expr) in body.exprs() {
            let Expr::Path(Resolution::Item { module, name, kind: ItemKind::Function }) = expr
            else {
                continue;
            };
            let Some(home) = file_of(module) else { continue };
            if is_generic(db, home, name) && reached.insert((module.clone(), name.clone())) {
                queue.push((home, name.clone()));
            }
        }
        // A method call -- `x.method()` or `Trait::method(x)` -- is recorded by
        // the checker under the impl's key (`Wrap#Box::get`), and a generic
        // impl's methods are emitted only through one. Followed into every
        // file that has a body by that key, since the call site does not say
        // which module holds the impl.
        let checked = crate::queries::checked(db, file);
        let Some((_, types)) = checked.bodies.iter().find(|(n, _)| *n == name) else { continue };
        for (_, (key, _)) in types.instantiations() {
            if !key.contains('#') {
                continue;
            }
            for home in root.files(db) {
                let has = khora_hir::body::bodies(db, *home).iter().any(|(n, _)| n == key);
                let Some(module) = khora_hir::item_map(db, *home).module.clone() else { continue };
                if has && is_generic(db, *home, key) && reached.insert((module, key.clone())) {
                    queue.push((*home, key.clone()));
                }
            }
        }
    }
    reached
}

/// Every use, in `file`, that code generation will emit of a function
/// declared with no body and without `extern`.
pub(crate) fn unimplemented_function_errors(db: &dyn Db, file: SourceFile) -> Vec<HirError> {
    let Some(root) = khora_db::source_root(db) else { return Vec::new() };
    let unimplemented = unimplemented_functions(db, root);
    if unimplemented.is_empty() {
        return Vec::new();
    }
    let here = khora_hir::item_map(db, file).module.clone();
    let checked = crate::queries::checked(db, file);

    let mut found = Vec::new();
    for (function, body) in khora_hir::body::bodies(db, file) {
        if is_generic(db, file, function) {
            let reached = reached_generics(db, root);
            let live = here.as_ref().is_some_and(|m| reached.contains(&(m.clone(), function.clone())));
            if !live {
                continue;
            }
        }
        let Some((_, types)) = checked.bodies.iter().find(|(n, _)| n == function) else { continue };
        let methods: Vec<(ExprId, &String)> = types
            .instantiations()
            .filter(|(_, (key, _))| key.contains('#'))
            .map(|(site, (key, _))| (*site, key))
            .collect();
        for (id, expr) in body.exprs() {
            let name = match expr {
                Expr::Path(Resolution::Item { module, name, .. })
                    if unimplemented.contains(&(module.clone(), name.clone())) =>
                {
                    name.clone()
                }
                // `p.twice()`: the checker recorded the impl's key at the call.
                _ => match methods.iter().find(|(site, _)| *site == id) {
                    Some((_, key)) if unimplemented.iter().any(|(_, k)| k == *key) => {
                        crate::traits::readable_key(key).to_string()
                    }
                    _ => continue,
                },
            };
            if lowered_by_name(body, types, id) {
                continue;
            }
            found.push(HirError {
                message: format!(
                    "`{name}` has no body, so there is nothing to call. Give it one, or \
                     write `extern fn` if it is a C symbol to be found at link time"
                ),
                range: body.range(id),
            });
        }
    }
    found
}

/// Whether code generation implements the methods of the type `head` by
/// name, whatever module declares them: `lower/calls.rs`'s `intrinsic`
/// dispatches `String::byte_length` or `Ptr::null` on the owner's name and
/// not on where the `impl` is, so a program that declares its own
/// `impl Ptr { fn null() -> Ptr; }` -- the codegen crate's tests do -- builds. A bodiless method on one of these types is therefore
/// left to the build, which knows the exact operations; checking names here
/// as well would be a second copy of that table to drift.
fn lowered_by_owner(head: &str) -> bool {
    matches!(
        head,
        "Region"
            | "Fiber"
            | "Fibers"
            | "SharedFn"
            | "Shared"
            | "Channel"
            | "Handoff"
            | "Array"
            | "Char"
            | "String"
            | "Float"
            | "Ptr"
            | "Int"
            | "I64"
    ) || crate::IntKind::parse(head).is_some()
}

/// Whether `id` is the callee of a call code generation lowers itself, by
/// name, whatever declares it -- and with the arguments it can lower.
///
/// Matches `lower/calls.rs`, `lower/text.rs` and `lower/failure.rs`: `print`
/// of an `Int`, `Float`, `Bool` or `String`; `assert` of a `Bool`;
/// `assert_that` of a `Bool` and a `String`; `attempt` of a function.
/// Anything else with those names is an ordinary call to a function with no
/// body, which the build refuses.
fn lowered_by_name(body: &Body, types: &BodyTypes, id: ExprId) -> bool {
    let Expr::Path(Resolution::Item { name, .. }) = body.expr(id) else { return false };
    let Some(args) = body.exprs().find_map(|(_, expr)| match expr {
        Expr::Call { callee, args } if *callee == id => Some(args),
        _ => None,
    }) else {
        return false;
    };
    let ty = |n: usize| types.of(args[n]);
    match (name.as_str(), args.len()) {
        ("print", 1) => matches!(ty(0), Type::Int | Type::Float | Type::Bool | Type::Str),
        ("assert", 1) => matches!(ty(0), Type::Bool),
        ("assert_that", 2) => matches!((ty(0), ty(1)), (Type::Bool, Type::Str)),
        ("attempt", 1) => matches!(ty(0), Type::Fn { .. }),
        _ => false,
    }
}
