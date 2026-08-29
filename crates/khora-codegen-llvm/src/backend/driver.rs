//! The build itself: from a source root to an object file.
//!
//! Whole-program, because a generic function is compiled by substituting into
//! its body and so every module's source has to be present at once. There is no
//! separate compilation to be had until D12 says what a compiled artifact even
//! is, and `docs/design/compatibility.md` decided there is no Khora ABI to
//! link one against.

use super::*;

/// Whether anything in the program can start a thread.
///
/// `Fiber::spawn` is the only one: `khora_fiber_spawn` is the sole runtime
/// entry point that calls `std::thread::spawn`, and a nursery adopts fibers
/// that already exist rather than making them. So the whole question is
/// whether any reachable body so much as *mentions* it.
///
/// Mentions, not calls, and over the whole expression arena rather than a walk
/// from the root. Both are the conservative direction: a `Fiber::spawn` handed
/// around as a value is still a spawn, and an expression a walk would have
/// skipped costs an optimization rather than correctness. The generated `main`
/// tells the runtime what was decided, which turns a wrong answer into an abort
/// at the first spawn instead of a data race.
fn program_can_spawn<'a>(
    mono: &'a khora_types::mono::Instances,
    body_of: impl Fn(&khora_types::mono::Instance) -> Option<&'a khora_hir::body::Body>,
) -> bool {
    mono.instances.iter().any(|(instance, _)| {
        body_of(instance).is_some_and(|body| {
            body.exprs().any(|(_, expr)| {
                matches!(
                    expr,
                    khora_hir::body::Expr::Path(khora_hir::Resolution::TraitItem { owner, name })
                        if owner == crate::runtime::FIBER_TYPE && name == "spawn"
                )
            })
        })
    })
}

/// Whether generated code may count references without atomics.
///
/// **The single most dangerous question this compiler answers**, which is why
/// it is a named function with tests rather than an expression inside `build`.
/// Answering yes wrongly is a data race in every reference count in the
/// program: memory corruption, arbitrarily far from its cause, with nothing
/// left to say what happened.
///
/// Three conditions, and each one is a way a second thread gets in.
pub(super) fn counts_non_atomically(
    db: &dyn Db,
    files: &[SourceFile],
    mono: &khora_types::mono::Instances,
    entry_point: Entry,
) -> bool {
    // Tests and benches each get a fiber of their own, and a library's host
    // chooses which of its threads calls in.
    if entry_point != Entry::Main {
        return false;
    }
    // A `main` build that publishes a symbol is a library too, whatever it was
    // built as.
    if program_publishes_a_symbol(db, files) {
        return false;
    }
    // And the original condition: a program that starts a fiber has workers.
    !program_can_spawn(mono, |instance| {
        let home = mono.home(&instance.symbol())?;
        khora_hir::body::bodies(db, home)
            .iter()
            .find(|(n, _)| n == &instance.function)
            .map(|(_, b)| b)
    })
}

/// Whether this program publishes a C symbol anybody could call.
///
/// An `pub extern fn` is one with `is_extern` *and* a body: without a body
/// it is a declaration of somebody else's symbol, which is an import rather
/// than an export. That is the same pair `build` filters exports by, said
/// earlier — here it has to be answered before any body is emitted, because
/// reference counting is chosen before the first `khora_dup` is written.
///
/// Conservative on purpose. It asks whether a symbol is *published*, not
/// whether anything calls it on another thread, because nothing here can know
/// the second and being wrong about it is memory corruption.
fn program_publishes_a_symbol(db: &dyn Db, files: &[SourceFile]) -> bool {
    files.iter().any(|file| {
        let types = khora_types::type_map(db, *file);
        khora_hir::body::bodies(db, *file).iter().any(|(name, body)| {
            body.root.is_some()
                && types.signatures.get(name.as_str()).is_some_and(|s| s.is_extern)
        })
    })
}

pub(super) fn build(
    db: &dyn Db,
    root: SourceRoot,
    out: &Path,
    entry_point: Entry,
    stop: Stop,
    profile: Profile,
) -> Result<(), Vec<HirError>> {
    let files = root.files(db);
    let mut diagnostics: Vec<HirError> = Vec::new();
    for file in files {
        diagnostics.extend(khora_types::diagnostics(db, *file).iter().cloned());
    }
    if !diagnostics.is_empty() {
        return Err(diagnostics);
    }

    let machine = target_machine(profile)?;

    // Whole-program: a generic function is compiled by substituting into its
    // body, so every module's source has to be present at once. There is no
    // separate compilation to be had until D12 says what a compiled artifact
    // even is.
    let mono = khora_types::mono::program_instances(db, root);
    if !mono.errors.is_empty() {
        return Err(mono.errors.clone());
    }

    let types = merged_types(db, files);
    let name = files
        .first()
        .and_then(|f| khora_hir::item_map(db, *f).module.as_ref().map(|m| m.to_string()))
        .unwrap_or_else(|| "khora".into());

    let context = Context::create();
    let mut backend = Backend::new(&context, &name, types.clone(), &machine);

    // **Debug info, before anything is emitted.** A `DISubprogram` has to be
    // attached to a function before that function's instructions are built,
    // and the compile unit has to exist before the first subprogram — so this
    // is as early as it can be and still know the entry file's path.
    if profile.debug_info() {
        let entry_path = files.first().map(|f| f.path(db).clone()).unwrap_or_default();
        let triple = machine.get_triple();
        let is_msvc = triple.as_str().to_string_lossy().contains("msvc");
        backend.debug =
            Some(crate::debug::Debug::new(&backend.module, &context, &entry_path, is_msvc));
    }

    // Every `extern fn` in the program, under the C symbol it names.
    //
    // `merged_types` flattens signatures into one map keyed by a bare name, so
    // a Khora `close` and POSIX's `close` collide there and the winner is
    // whichever file was walked first. These are kept apart because they never
    // shared a namespace to begin with: one is emitted `kh$std$fs$close`, the
    // other is looked up by the linker. `Backend::foreign_signatures`.
    for file in files {
        for (symbol, signature) in &khora_types::type_map(db, *file).signatures {
            if signature.is_extern {
                backend.register_foreign(symbol, signature.clone());
            }
        }
    }
    // Tests each get a fiber of their own, so only a `main` build can be
    // single-threaded.
    //
    // **A library never is.** The host chooses which of its threads calls an
    // exported function and may choose several, so reference counting has to be
    // atomic whether or not this program contains a `Fiber::spawn`.
    // `Entry::Library` failing the comparison below is what makes that true.
    //
    // **And a `main` build that publishes a symbol is a library too.**
    // `emit_c_exports` runs for every entry point, so a program with a
    // `pub extern fn` hands its address to whatever it is linked against, and a
    // C library taking a callback calls it on whichever thread it likes. The
    // spawn check alone answers "single-threaded" for that program and emits
    // non-atomic counting for a function a foreign thread can enter.
    //
    // Getting either wrong is a data race in a refcount, observable only as
    // corruption long afterwards, which is why both are conditions here rather
    // than notes somewhere.
    backend.single_threaded = counts_non_atomically(db, files, mono, entry_point);

    // One emitted function per *specialization*, not per source function: a
    // generic body has no machine representation until its type arguments are
    // known, and a generic function nobody calls is not emitted at all.
    for (instance, _) in &mono.instances {
        let home = mono.home(&instance.symbol());
        let scope = home.map(|h| khora_types::type_map(db, h)).unwrap_or(&types);
        if let Some(signature) = specialized_signature(scope, instance) {
            backend.register_instance(&instance.symbol(), signature);
        }
    }

    // Declare every definition before lowering any of them: a call site does
    // not know whether its callee has been emitted yet, and mutual recursion
    // means no ordering exists that would make it know.
    for (instance, _) in &mono.instances {
        backend.declare_definition(&instance.symbol());
    }

    let body_of = |instance: &khora_types::mono::Instance| {
        let home = mono.home(&instance.symbol())?;
        khora_hir::body::bodies(db, home)
            .iter()
            .find(|(n, _)| n == &instance.function)
            .map(|(_, b)| b)
    };

    for (instance, instance_types) in &mono.instances {
        let Some(body) = body_of(instance) else { continue };
        declare_closures(&mut backend, &instance.symbol(), body, instance_types);
    }

    // Which methods the program writes in Khora, so the reference-counting
    // planner does not take the borrow table's word about one of them. A body
    // owns its parameters and releases them, and telling its caller to lend one
    // instead is a use after free — `khora_perceus::Defined`.
    let body_names: Vec<String> = files
        .iter()
        .flat_map(|f| khora_hir::body::bodies(db, *f).iter().map(|(n, _)| n.clone()))
        .collect();
    let defined = khora_perceus::Defined::from_body_names(body_names.iter().map(String::as_str));
    for (instance, instance_types) in &mono.instances {
        let Some(body) = body_of(instance) else { continue };
        // Planned per *specialization*: `A` is unboxed in the generic body and
        // a counted pointer at `A = List<Int>`, so one plan for both is wrong
        // for whichever it was not made for.
        let plan = khora_perceus::plan(body, instance_types, &defined);
        enter_debug_scope(db, &mut backend, mono, instance, body, &instance.symbol(), None);
        crate::lower::emit_function(
            &mut backend,
            &instance.symbol(),
            body,
            Some(&plan),
            instance_types,
            mono,
        );
        backend.end_debug_scope();
    }

    // Lifted lambda bodies come after the functions that build them, because
    // the closure sites are discovered while walking those bodies.
    for site in backend.closure_sites() {
        let Some((owner, owner_types)) =
            mono.instances.iter().find(|(i, _)| i.symbol() == site.owner)
        else {
            continue;
        };
        let Some(body) = body_of(owner) else { continue };
        let plan = khora_perceus::plan(body, owner_types, &defined);
        // A lifted lambda belongs to the file its enclosing function came
        // from, and reads in a backtrace under the name of that function —
        // there is nothing else to call it, and a bare symbol would be worse.
        // Its *own* symbol and its own position, though: the lambda is a
        // separate function and gets a separate subprogram.
        enter_debug_scope(
            db,
            &mut backend,
            mono,
            owner,
            body,
            &site.symbol,
            Some(body.range(site.expr)),
        );
        crate::lower::emit_closure(&mut backend, &site, body, Some(&plan), owner_types, mono);
        backend.end_debug_scope();
    }

    // **The published C symbols, after every body exists.** A wrapper calls a
    // definition, so nothing may be emitted before the thing it forwards to.
    let exports: Vec<(String, String)> = mono
        .instances
        .iter()
        .filter(|(instance, _)| {
            backend.signature_of(&instance.symbol()).is_some_and(|s| s.is_extern)
                && body_of(instance).is_some_and(|b| b.root.is_some())
        })
        .map(|(instance, _)| (instance.function.clone(), instance.symbol()))
        .collect();
    if entry_point == Entry::Library && exports.is_empty() {
        backend.error(
            "a library has no `pub extern fn`, so nothing could call it. Mark \
             the functions that are its C interface — `docs/design/c-export.md`",
            text_size::TextRange::empty(0.into()),
        );
    }
    backend.emit_c_exports(&exports);
    if entry_point == Entry::Library && stop == Stop::AtExecutable {
        write_the_header(&backend, out, &exports);
    }

    // After every body and every lifted closure, because lowering is what
    // assigns error ids and the last one compiled may add another.
    backend.emit_error_releaser();

    match entry_point {
        Entry::Main => {
            let entry =
                mono.instances.iter().find(|(i, _)| i.function == "main").map(|(i, _)| i.symbol());
            backend.emit_c_main(entry.as_deref());
        }
        // Nothing to emit. The exported symbols *are* the entry points, and
        // `emit_c_exports` has already written them.
        Entry::Library => {}
        Entry::Tests | Entry::Benches => {
            // In written order, per file, which is the order a reader expects
            // a report in even though the test runs themselves overlap.
            let wanted = match entry_point {
                Entry::Benches => khora_hir::TestKind::Bench,
                _ => khora_hir::TestKind::Test,
            };
            let mut blocks: Vec<(String, String)> = Vec::new();
            for file in files {
                for test in &khora_hir::item_map(db, *file).tests {
                    if test.kind != wanted {
                        continue;
                    }
                    let Some((instance, _)) =
                        mono.instances.iter().find(|(i, _)| i.function == test.key)
                    else {
                        continue;
                    };
                    blocks.push((instance.symbol(), test.name.clone()));
                }
            }
            match entry_point {
                Entry::Benches => backend.emit_bench_main(&blocks),
                _ => backend.emit_test_main(&blocks),
            }
        }
    }
    backend.emit_pending_thunks();
    backend.emit_pending_drop_glue();

    if !backend.errors.is_empty() {
        return Err(backend.errors);
    }
    // **Before `finish`**, which verifies. The verifier reads debug metadata,
    // and unresolved temporaries are exactly what it objects to.
    if let Some(debug) = backend.debug.as_ref() {
        debug.finalize();
    }
    backend.finish(&machine, out, stop, entry_point == Entry::Library, profile)
}

/// Opens the debug scope for one emitted function.
///
/// The file is the *home* of the instance rather than the file being compiled:
/// a build is whole-program, so a specialization of `List::map` belongs to
/// `std/list.kh` however deep in an application it was reached from, and a
/// backtrace that walks out of user code into `std` should say so.
fn enter_debug_scope(
    db: &dyn Db,
    backend: &mut Backend<'_>,
    mono: &khora_types::mono::Instances,
    instance: &khora_types::mono::Instance,
    body: &khora_hir::body::Body,
    // **The symbol being emitted, which is not always the instance's.** A
    // lifted lambda takes its file and its display name from the function it
    // was written inside, and everything else from itself. Passing the owner's
    // symbol here attached a *second* `DISubprogram` to the owner's function,
    // silently replacing the one it already had, and every instruction in the
    // lambda then pointed at a scope belonging to a function it was not in.
    symbol: &str,
    at: Option<TextRange>,
) {
    if backend.debug.is_none() {
        return;
    }
    // Cleared on the way *in* as well as on the way out. Entering is the
    // reliable half — a `continue` in either emit loop skips the exit, and a
    // stale location surviving that is a failed build.
    backend.end_debug_scope();
    let Some(home) = mono.home(&instance.symbol()) else { return };
    // The body's first expression, which is inside the function and is the
    // best position available: a `Body` records where its expressions are and
    // not where its `fn` keyword was.
    let at = match at.or_else(|| body.root.map(|root| body.range(root))) {
        Some(at) => at,
        None => return,
    };
    let path = home.path(db).to_string_lossy().into_owned();
    let text = home.text(db).to_string();
    let Some(function) = backend.definition(symbol) else { return };
    if let Some(debug) = backend.debug.as_mut() {
        debug.enter(function, &instance.function, symbol, &path, &text, at);
    }
}

/// Writes the C header beside the library.
///
/// A failure here is reported and does not stop the build: the library itself
/// is the artifact, and a caller who cannot write a header next to it has a
/// directory problem rather than a compilation one — but saying nothing would
/// leave them with a library and no prototypes and no reason why.
fn write_the_header(backend: &Backend<'_>, out: &Path, exports: &[(String, String)]) {
    let named: Vec<(String, khora_types::Signature)> = exports
        .iter()
        .filter_map(|(name, symbol)| Some((name.clone(), backend.signature_of(symbol)?)))
        .collect();
    let stem = out.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let path = out.with_extension("h");
    if let Err(e) = std::fs::write(&path, crate::backend::header::render(&stem, &named)) {
        eprintln!("khora: the library was built but its header was not: {}: {e}", path.display());
    }
}

/// One view of every type in the program.
///
/// Each file's own map already carries what it imported, so the union repeats
/// itself. Variants are deduplicated by type and case because a *tag is an
/// index into its type's variant list* — counting `Option::Some` twice would
/// renumber `None`.
fn merged_types(db: &dyn Db, files: &[SourceFile]) -> TypeMap {
    let mut out = TypeMap::default();
    for file in files {
        let map = khora_types::type_map(db, *file);
        for (name, signature) in &map.signatures {
            out.signatures.entry(name.clone()).or_insert_with(|| signature.clone());
        }
        for variant in &map.variants {
            // Keyed by the *declaration*, not by the spelling. Deduplicating on
            // `(type_name, name)` alone kept whichever module was merged first
            // and silently dropped the other, so a program with two `Point`s
            // compiled one of them twice. Errata 46.
            if !out.variants.iter().any(|v| {
                v.type_name == variant.type_name
                    && v.name == variant.name
                    && v.home == variant.home
            }) {
                out.variants.push(variant.clone());
            }
        }
        for (name, generics) in &map.adts {
            out.adts.entry(name.clone()).or_insert_with(|| generics.clone());
        }
        for (name, kind) in &map.kinds {
            out.kinds.entry(name.clone()).or_insert_with(|| kind.clone());
        }
        for (name, def) in &map.traits.traits {
            out.traits.traits.entry(name.clone()).or_insert_with(|| def.clone());
        }
        for imp in &map.traits.impls {
            if !out.traits.impls.iter().any(|o| o.trait_name == imp.trait_name && o.head() == imp.head())
            {
                out.traits.impls.push(imp.clone());
            }
        }
        for own in &map.traits.inherent {
            if !out.traits.inherent.iter().any(|o| o.head == own.head && o.methods == own.methods) {
                out.traits.inherent.push(own.clone());
            }
        }
    }
    out
}

/// Declares the lifted function for every lambda in one emitted body.
///
/// One pass per *specialization*, not per source function: a lambda inside a
/// generic function captures different types in each instantiation, so each
/// needs a function of its own.
fn declare_closures(
    backend: &mut Backend<'_>,
    symbol: &str,
    body: &khora_hir::body::Body,
    types: &khora_types::BodyTypes,
) {
    for (id, expr) in body.exprs() {
        let khora_hir::body::Expr::Lambda { captures, .. } = expr else { continue };
        let shape = types.of(id).clone();
        let Type::Fn { params, ret, .. } = &shape else { continue };

        // The names the body mentions, and then the capabilities it uses
        // without mentioning. A `with` block lowers to a block of `let`s, so a
        // capability is an ordinary binding — but `report(n)` needs `ledger`
        // without writing it down, and the capture scan in lowering watches
        // names. Reading the checker's answer rather than re-deriving it here
        // is what keeps the two from disagreeing.
        let implicit = types.implicit_captures(id);
        let captured: Vec<(khora_hir::body::LocalId, Type)> = captures
            .iter()
            .chain(implicit.iter().filter(|l| !captures.contains(l)))
            .map(|l| (*l, types.local(*l).clone()))
            .collect();
        // An unsolved variable here means nothing ever pinned the type down —
        // `let f = fn x => x;` with `f` unused. That is an ambiguity in the
        // program, not a limit of the backend, and saying which it is decides
        // whether the reader looks for a missing annotation or a missing
        // feature.
        let unsolved = params
            .iter()
            .chain(std::iter::once(&**ret))
            .any(|t| matches!(t, Type::Var(_)));
        if backend.declare_closure(symbol, id, shape.clone(), captured).is_none() {
            backend.error(
                if unsolved {
                    "the type of this closure was never pinned down; use it somewhere that \
                     decides its argument and result types"
                        .to_string()
                } else {
                    "this closure has a parameter or result the backend cannot represent yet"
                        .to_string()
                },
                body.range(id),
            );
        }
    }
}

/// A signature with the instance's type arguments substituted in.
///
/// This is what makes a specialization compilable: the declared signature still
/// mentions rigid parameters, which have no machine representation.
fn specialized_signature(
    types: &TypeMap,
    instance: &khora_types::mono::Instance,
) -> Option<Signature> {
    let signature = types.signatures.get(&instance.function)?;
    if instance.args.is_empty() {
        return Some(signature.clone());
    }
    let mapping: HashMap<&str, Type> = signature
        .generics
        .iter()
        .zip(&instance.args)
        .map(|(g, a)| (g.as_str(), a.clone()))
        .collect();
    Some(Signature {
        // A specialization of a Khora body, so never foreign: a generic
        // `extern` has no single machine signature and is refused before this.
        is_extern: false,
        // Both rows survive to here, and both are substituted: the capability
        // row says how many extra parameters the function takes and the error
        // row whether it returns a tagged value, and a `with 'r` clause knows
        // neither until `'r` does. Copying them unsubstituted made a
        // row-polymorphic function look like it needed nothing.
        requires: khora_types::unify::substitute(&signature.requires, &mapping),
        raises: khora_types::unify::substitute(&signature.raises, &mapping),
        generics: Vec::new(),
        // A specialized signature has no parameters left, so it can carry no
        // bounds either: whatever they required was settled before this ran.
        bounds: Vec::new(),
        params: signature
            .params
            .iter()
            .map(|p| khora_types::unify::substitute(p, &mapping))
            .collect(),
        ret: khora_types::unify::substitute(&signature.ret, &mapping),
    })
}

/// The machine every module is generated for.
///
/// Built before the module rather than after it, because the module's data
/// layout comes from here and **the layout has to be in place before a single
/// instruction is built**. inkwell records each load's and store's alignment at
/// the moment it is created, from whatever layout the module has then; a module
/// with no layout yet reports `i64` as 4-byte aligned, and setting the real one
/// afterwards does not go back and fix the instructions. The result still runs
/// on x86, which is exactly what makes it easy to ship.
fn target_machine(profile: Profile) -> Result<TargetMachine, Vec<HirError>> {
    // **Every target inkwell was built with, not just this machine's.**
    // `initialize_native` is enough to compile for the host and nothing else,
    // which is what `docs/design/targets.md` recorded as the reason there was
    // no `--target` at all: there was nowhere for it to point. Initializing
    // the set costs a handful of registrations and is what makes a triple mean
    // something.
    Target::initialize_all(&InitializationConfig::default());

    // The host's triple unless `KHORA_TARGET` names another. Both halves of a
    // cross build move together — `khora_db::host_target` reads the same
    // variable to pick which `std` files are compiled — so a build cannot
    // generate for one platform while reading another's bindings.
    let triple = match khora_db::target_triple() {
        Some(named) => TargetTriple::create(&named),
        None => TargetMachine::get_default_triple(),
    };
    let target = Target::from_triple(&triple).map_err(|e| {
        vec![backend_error(format!(
            "resolving target {}: {e}. This build can emit for the targets \
             `crates/khora-codegen-llvm/Cargo.toml` enables on inkwell",
            triple.as_str().to_string_lossy()
        ))]
    })?;
    target
        .create_target_machine(
            &triple,
            CPU,
            FEATURES,
            // Instruction selection, which is a different dial from the IR
            // pipeline above it. `Default` is `-O2`'s, and it is what every
            // build has always used — a debug build that dropped to `None`
            // would be slower than the one everything here is calibrated
            // against, for a readability the IR already provides by not having
            // been optimized.
            match profile {
                Profile::Debug => OptimizationLevel::Default,
                Profile::Release => OptimizationLevel::Aggressive,
            },
            // **`PIC`, not `Default`.** `Default` means the target's
            // traditional model, which on x86-64 Linux is absolute addressing
            // -- and every mainstream distribution now builds executables as
            // position-independent, so the link fails with
            //
            //     relocation R_X86_64_32 against `.rodata.str1.1` can not be
            //     used when making a PIE object; recompile with -fPIE
            //
            // naming a flag that means nothing here, because nothing was
            // compiled with a C compiler. Windows and macOS already produce
            // position-independent code whatever this says, so asking for it
            // everywhere costs nothing and removes a per-platform branch.
            RelocMode::PIC,
            CodeModel::Default,
        )
        .ok_or_else(|| vec![backend_error("creating the target machine")])
}

/// The key the shared closure `drop_fields` is cached under. Not a legal Khora
/// type name, so it can never collide with an ADT's.
pub(super) const CLOSURE_GLUE: &str = "$closure";

#[cfg(test)]
mod tests {
    use super::*;
    use khora_db::KhoraDatabase;

    /// The decision, for one program.
    fn non_atomic(entry_point: Entry, sources: &[&str]) -> bool {
        let db = KhoraDatabase::new();
        let files: Vec<SourceFile> = sources
            .iter()
            .enumerate()
            .map(|(i, text)| SourceFile::new(&db, format!("m{i}.kh").into(), (*text).to_string()))
            .collect();
        let root = SourceRoot::new(&db, files.clone());
        let mono = khora_types::mono::program_instances(&db, root);
        assert!(mono.errors.is_empty(), "the fixture should compile: {:?}", mono.errors);
        counts_non_atomically(&db, &files, mono, entry_point)
    }

    const PLAIN: &str = "module main;\nfn main() -> Int { 0 }\n";

    /// The case the optimisation exists for.
    #[test]
    fn a_main_that_neither_spawns_nor_publishes_may_count_without_atomics() {
        assert!(non_atomic(Entry::Main, &[PLAIN]));
    }

    #[test]
    fn a_main_that_spawns_may_not() {
        let source = "module main;
pub type Fiber<A, 'r>;
impl<A, 'r> Fiber<A, 'r> { fn spawn(body: () -> A raises 'r) -> Fiber<A, 'r>; fn join(self) -> A raises 'r; }
fn work() -> () { }
fn main() -> Int { Fiber::join(Fiber::spawn(fn () => work())); 0 }
";
        assert!(!non_atomic(Entry::Main, &[source]));
    }

    /// **The audit finding.** `emit_c_exports` runs for every entry point, so a
    /// `main` build with an `pub extern fn` hands its address to whatever it
    /// is linked against — and a C library that takes a callback calls it on
    /// whichever thread it likes. Such a program never writes `Fiber::spawn`,
    /// so the spawn check alone called it single-threaded and emitted
    /// non-atomic counting for a function a foreign thread can enter.
    #[test]
    fn a_main_that_publishes_a_symbol_may_not() {
        let source = "module main;
pub extern fn price(n: Int) -> Int { n * 2 }
fn main() -> Int { 0 }
";
        assert!(
            !non_atomic(Entry::Main, &[source]),
            "an exported symbol can be called from a thread this program never made"
        );
    }

    /// An `extern fn` *without* a body is an import — somebody else's symbol,
    /// which nothing can call back into. Distinguishing the two is the whole of
    /// what `pub extern fn` means, and treating every `extern` as published
    /// would give up the optimisation for every program that reads a file.
    #[test]
    fn declaring_a_foreign_symbol_is_not_publishing_one() {
        let source = "module main;
extern fn getpid() -> Int;
fn main() -> Int { getpid(); 0 }
";
        assert!(non_atomic(Entry::Main, &[source]));
    }

    /// A published symbol anywhere in the program counts, not only in the file
    /// that happens to hold `main`.
    #[test]
    fn a_symbol_published_by_another_module_counts() {
        let library = "module lib;\npub extern fn price(n: Int) -> Int { n * 2 }\n";
        let main = "module main;\nfn main() -> Int { 0 }\n";
        assert!(!non_atomic(Entry::Main, &[library, main]));
    }

    #[test]
    fn a_library_never_counts_without_atomics() {
        let source = "module main;
pub extern fn price(n: Int) -> Int { n * 2 }
";
        assert!(!non_atomic(Entry::Library, &[source]));
    }

    #[test]
    fn tests_and_benches_never_do_either() {
        assert!(!non_atomic(Entry::Tests, &[PLAIN]));
        assert!(!non_atomic(Entry::Benches, &[PLAIN]));
    }
}
