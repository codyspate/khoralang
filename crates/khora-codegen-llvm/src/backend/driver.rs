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

pub(super) fn build(
    db: &dyn Db,
    root: SourceRoot,
    out: &Path,
    entry_point: Entry,
    stop: Stop,
) -> Result<(), Vec<HirError>> {
    let files = root.files(db);
    let mut diagnostics: Vec<HirError> = Vec::new();
    for file in files {
        diagnostics.extend(khora_types::diagnostics(db, *file).iter().cloned());
    }
    if !diagnostics.is_empty() {
        return Err(diagnostics);
    }

    let machine = target_machine()?;

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
    backend.single_threaded =
        entry_point == Entry::Main && !program_can_spawn(mono, |instance| {
            let home = mono.home(&instance.symbol())?;
            khora_hir::body::bodies(db, home)
                .iter()
                .find(|(n, _)| n == &instance.function)
                .map(|(_, b)| b)
        });

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
        crate::lower::emit_function(
            &mut backend,
            &instance.symbol(),
            body,
            Some(&plan),
            instance_types,
            mono,
        );
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
        crate::lower::emit_closure(&mut backend, &site, body, Some(&plan), owner_types, mono);
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
    backend.finish(&machine, out, stop)
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
fn target_machine() -> Result<TargetMachine, Vec<HirError>> {
    Target::initialize_native(&InitializationConfig::default())
        .map_err(|e| vec![backend_error(format!("initializing the native target: {e}"))])?;

    let triple = TargetMachine::get_default_triple();
    let target = Target::from_triple(&triple).map_err(|e| {
        vec![backend_error(format!(
            "resolving target {}: {e}",
            triple.as_str().to_string_lossy()
        ))]
    })?;
    target
        .create_target_machine(
            &triple,
            CPU,
            FEATURES,
            OptimizationLevel::Default,
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
