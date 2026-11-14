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
//! **The uses a build of everything would reach.** Code generation emits what
//! a build's entry points reach ([`crate::mono::Roots`]); this asks the wider
//! question of what any concrete body reaches, so a use in a concrete body is
//! refused whether or not `main` calls that body, and a use in a generic body
//! only when a chain of calls from a concrete body reaches that generic --
//! `fn later<A>(a: A) -> Int { helper(1) }` with nothing calling `later`
//! builds and runs, and is not refused. Refusing a concrete body nothing calls
//! is stricter than the build, which compiles no code for it: the program is
//! wrong as written, and becomes a failed build the moment something calls
//! it. Methods count: `impl P { fn twice(self) -> Int; }` called as
//! `p.twice()` fails to build the same way.
//!
//! Not the specialization walk itself, though it would be exact: it made
//! `khora check` of a ten-line program take 12 s instead of 1.6 s in a debug
//! build, and an editor runs `check` on every keystroke. What this costs
//! instead is a pass over the bodies reachable from concrete ones, once per
//! source root, following path calls and the method calls the checker
//! recorded. A call through a trait on a concrete type is read as the impl
//! for that type's head (see `impl_method_key`). A call through a trait
//! *bound* inside a generic is not resolved, so a generic impl reached only
//! that way is not followed; a call to a bodiless function inside one passes
//! `check` and fails the build, as every such call did before.
//!
//! Not `std`: its bodiless functions are the intrinsics, which code
//! generation writes at each call site, and a program cannot add one. A
//! program's own bodiless `print`, `assert` or `assert_that` is one too when
//! it is declared as std's is and called with the arguments code generation
//! lowers by name -- `examples/core_demo` declares its own `print` -- and only
//! then; its own `attempt`, `within_deadline` or `contain_run` never is (see
//! `lowered_by_name`). So is a bodiless method on a type whose operations
//! code generation lowers by the type's name (`Ptr`, `String`, `Array`, the
//! integers, ...), whichever module declares the `impl`; those are left to
//! the build.

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
        if crate::standard_library::is_std_file(db, *file) {
            continue;
        }
        let Some(module) = khora_hir::item_map(db, *file).module.clone() else { continue };
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
                            if is_named_intrinsic(head, &name) {
                                continue;
                            }
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
        let methods: Vec<(ExprId, String)> = types
            .instantiations()
            .filter_map(|(site, (key, args))| impl_method_key(key, args).map(|k| (*site, k)))
            .collect();
        for (id, expr) in body.exprs() {
            let name = match expr {
                Expr::Path(Resolution::Item { module, name, .. })
                    if unimplemented.contains(&(module.clone(), name.clone())) =>
                {
                    name.clone()
                }
                // `p.twice()`: the checker recorded the impl's key at the call,
                // or the trait's for a call through a trait.
                _ => match methods.iter().find(|(site, _)| *site == id) {
                    Some((_, key)) if unimplemented.iter().any(|(_, k)| k == key) => {
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

/// The impl method a call recorded as `(key, type arguments)` runs, keyed as
/// [`unimplemented_functions`] keys it, when that can be read off the call.
///
/// An inherent or static call records the impl's key itself (`#P::twice`).
///
/// A call through a trait -- `"ab".size()`, `Size::size(x)` -- records the
/// *trait's* (`Size::size`) with `Self` as its first argument, and the impl is
/// `Size#<head of Self>::size`. Without this second form a bodiless method in
/// `impl Size for String` checked clean and failed to build with
/// "`Size#String::size` has no body", located at a doc comment in an unrelated
/// std file.
///
/// What it does not read: a `Self` with no head yet (a call inside a generic,
/// which the build resolves per instance), and a trait with arguments, whose
/// impl key spells them (`Convert<String>#Int`) as the call does not. Both
/// pass `check` and fail the build, as every such call did before.
fn impl_method_key(key: &str, args: &[Type]) -> Option<String> {
    if key.contains('#') {
        return Some(key.to_string());
    }
    let (trait_name, method) = key.split_once("::")?;
    let head = crate::traits::head_of(args.first()?)?;
    Some(crate::traits::method_key(trait_name, &head, method))
}

/// Whether code generation implements every method of the type `head` by
/// name, whatever module declares them: `lower/calls.rs`'s `intrinsic`
/// hands any call on these owners to the owner's own lowering, which refuses
/// a name it does not know in its own words. A bodiless method on one of
/// these types is therefore left to the build.
///
/// `String`, `Ptr`, `Char` and `Float` are not here: code generation lowers
/// only some of their names ([`NAMED_INTRINSICS`]), and any other bodiless
/// method on them is a function with no body, refused here as it is by the
/// build.
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
            | "Selection"
            | "Array"
            | "Int"
            | "I64"
    ) || crate::IntKind::parse(head).is_some()
}

/// The methods code generation lowers itself on owners it dispatches by
/// method name, as `(owner, name)`.
///
/// **One table for `khora check` and for the build.** `khora check` left
/// every bodiless method on these owners to the build, so a program's own
/// `impl String { fn data(self) -> Ptr; }` called as `String::data(s)`
/// checked clean and failed to build with "`#String::data` has no body". The
/// code generator reads this table before it dispatches on these owners, so
/// a name added there and not here is refused by both, and cannot drift into
/// a second disagreement.
pub const NAMED_INTRINSICS: &[(&str, &str)] = &[
    ("Char", "code"),
    ("Char", "of"),
    ("Char", "from_code"),
    ("Float", "to_int"),
    ("Ptr", "null"),
    ("Ptr", "is_null"),
    ("String", "with_data"),
    ("String", "with_c_string"),
    ("String", "from_bytes"),
    ("String", "bytes"),
    ("String", "byte"),
    ("String", "byte_length"),
    ("String", "slice"),
    ("String", "find"),
];

/// Whether `owner::name` is one of [`NAMED_INTRINSICS`].
pub fn is_named_intrinsic(owner: &str, name: &str) -> bool {
    NAMED_INTRINSICS.iter().any(|(o, n)| *o == owner && *n == name)
}

/// Whether `id` is the callee of a call code generation lowers itself, by
/// name -- and with the arguments it can lower.
///
/// Matches `lower/calls.rs`, `lower/text.rs` and `lower/failure.rs`. Reached
/// only for a function declared bodiless *outside* the standard library
/// ([`unimplemented_functions`] leaves std's files out, through
/// [`crate::standard_library::is_std_file`]), so the question here is which
/// program declarations may stand in for an intrinsic:
///
/// - **`print`, `assert` and `assert_that`: when the declaration says what
///   std's does** -- one `Int`, `Float`, `Bool` or `String` (with a `String`
///   after it, for `assert_that`) in, nothing out, no row. A test fixture
///   that compiles without std declares `fn print(value: Int);`, and so does
///   `examples/core_demo`; that is the same function under the same
///   contract. One that declares `-> String` is not: code generation lowers
///   the call as std's and the program reads its `()` as a string, which
///   crashed.
/// - **`attempt`, `within_deadline` and `contain_run`: never.** Each one's
///   answer is built by code generation to std's type, and a program's own
///   declaration of another type -- `-> Result<A, String>` over an error
///   that is not a `String` -- read one value's bytes as another's. A program
///   that wants one of these imports std's.
fn lowered_by_name(body: &Body, types: &BodyTypes, id: ExprId) -> bool {
    let Expr::Path(Resolution::Item { name, .. }) = body.expr(id) else { return false };
    let Some(args) = body.exprs().find_map(|(_, expr)| match expr {
        Expr::Call { callee, args } if *callee == id => Some(args),
        _ => None,
    }) else {
        return false;
    };
    let ty = |n: usize| types.of(args[n]);
    let answers_nothing = matches!(
        types.of(id),
        Type::Fn { ret, requires, raises, .. }
            if **ret == Type::Unit && crate::is_empty_row(requires) && crate::is_empty_row(raises)
    );
    match (name.as_str(), args.len()) {
        ("print", 1) => {
            answers_nothing && matches!(ty(0), Type::Int | Type::Float | Type::Bool | Type::Str)
        }
        ("assert", 1) => answers_nothing && matches!(ty(0), Type::Bool),
        ("assert_that", 2) => answers_nothing && matches!((ty(0), ty(1)), (Type::Bool, Type::Str)),
        _ => false,
    }
}
