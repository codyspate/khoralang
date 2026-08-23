//! Assembling one LLVM module, and turning it into an executable.
//!
//! This owns everything that is per *module* — the runtime declarations, the
//! variant tag assignment, the function table, the per-type `drop_fields`
//! routines and the C entry point. Per *function* lowering lives in
//! [`crate::lower`].
//!
//! # Symbol names
//!
//! Khora functions are emitted as `kh$<name>`. Two reasons, both boring and
//! both load bearing:
//!
//! - Khora's `main` is not C's `main`. The generated executable needs a C
//!   `main` returning `i32`, and it calls the Khora one, so the two cannot
//!   share a symbol.
//! - An unprefixed name would collide with the C library the executable links
//!   against — a Khora `fn read` or `fn open` would quietly become someone
//!   else's `read` or `open`.
//!
//! `$` is legal in COFF and ELF symbols and is not something a C library
//! exports, which makes the prefix collision-proof from both directions.
//! Module-qualified mangling waits for cross-module linking; phase 2 programs
//! are a single module.
//!
//! # Functions declared without a body
//!
//! `docs/errata.md` #5 makes a function's body optional, so `fn print(v: Int);`
//! is a declaration with no definition. The backend treats those as **externs**
//! and emits a call to the unmangled C symbol — which is what makes the runtime
//! reachable from Khora source at all, and is how the leak test calls
//! `khora_live_count`. `print` is the one exception: it is an intrinsic
//! dispatched on its argument type, because there is no prelude yet to declare
//! three differently-typed printers.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use inkwell::builder::Builder;
use inkwell::context::Context;
use inkwell::module::{Linkage, Module};
use inkwell::targets::{
    CodeModel, FileType, InitializationConfig, RelocMode, Target, TargetMachine,
};
use inkwell::types::{BasicMetadataTypeEnum, BasicType, BasicTypeEnum, FunctionType};
use inkwell::values::{BasicMetadataValueEnum,BasicValueEnum, FunctionValue, PointerValue};
use inkwell::{AddressSpace, OptimizationLevel};

use khora_db::{Db, SourceFile, SourceRoot};
use khora_hir::HirError;
use khora_perceus::is_boxed;
use khora_types::{Signature, Type, TypeMap, VariantInfo};
use text_size::TextRange;

use crate::runtime::{self, Runtime};
use crate::toolchain;

/// CPU and feature set to generate for.
///
/// Deliberately generic rather than the host's, matching `spike.rs`. §6.1
/// requires bit-for-bit reproducible builds, which host-specific instruction
/// selection would break — CPU tuning belongs behind an explicit target flag,
/// never a silent default.
const CPU: &str = "generic";
const FEATURES: &str = "";

/// Compiles one file to a native executable at `out`.
///
/// Type checking comes first and is absolute: if `khora_types::diagnostics`
/// reports anything, those errors are returned and nothing is emitted. Every
/// stage below assumes a well-typed program, and would otherwise turn a type
/// error into a miscompilation rather than a message.
///
/// The returned errors are also how the backend reports what it cannot yet
/// lower. Those carry the source range of the offending expression; failures in
/// LLVM itself or in the linker have no source position and are reported
/// against the start of the file.
///
/// # What it writes
///
/// The executable at `out`, and the object it was linked from, at `out` with
/// `.o` appended. The object is kept rather than cleaned up: when a generated
/// program misbehaves, disassembling it is the first thing anyone does. Setting
/// `KHORA_EMIT_LLVM` also writes the module as `.ll` beside them, before
/// verification, so a module that fails to verify can still be read.
///
/// # What it needs on disk
///
/// `clang` under `LLVM_SYS_221_PREFIX`, and `khora-rt`'s static archive, which
/// [`crate::toolchain::runtime_archive`] locates. A missing archive is an error
/// naming the command that produces it, not a link failure full of undefined
/// symbols from Rust's `std`.
pub fn compile(db: &dyn Db, root: SourceRoot, out: &Path) -> Result<(), Vec<HirError>> {
    build(db, root, out, Entry::Main)
}

/// Compiles the program's *tests* to an executable that runs them.
///
/// The same program, with a different entry point: instead of calling `main`,
/// it registers every `test` block and hands them to the runner, which gives
/// each one a fiber of its own. Everything else — the same monomorphization,
/// the same lowering — is shared, because a test body is a function body.
pub fn compile_tests(db: &dyn Db, root: SourceRoot, out: &Path) -> Result<(), Vec<HirError>> {
    build(db, root, out, Entry::Tests)
}

/// Which entry point an executable gets.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Entry {
    /// Call `main`, and its result is the exit status.
    Main,
    /// Run every test, and whether they all passed is the exit status.
    Tests,
}

fn build(
    db: &dyn Db,
    root: SourceRoot,
    out: &Path,
    entry_point: Entry,
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
    for (instance, instance_types) in &mono.instances {
        let Some(body) = body_of(instance) else { continue };
        // Planned per *specialization*: `A` is unboxed in the generic body and
        // a counted pointer at `A = List<Int>`, so one plan for both is wrong
        // for whichever it was not made for.
        let plan = khora_perceus::plan(body, instance_types);
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
        let plan = khora_perceus::plan(body, owner_types);
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
        Entry::Tests => {
            // In written order, per file, which is the order a reader expects
            // a report in even though the runs themselves overlap.
            let mut tests: Vec<(String, String)> = Vec::new();
            for file in files {
                for test in &khora_hir::item_map(db, *file).tests {
                    let Some((instance, _)) =
                        mono.instances.iter().find(|(i, _)| i.function == test.key)
                    else {
                        continue;
                    };
                    tests.push((instance.symbol(), test.name.clone()));
                }
            }
            backend.emit_test_main(&tests);
        }
    }
    backend.emit_pending_thunks();
    backend.emit_pending_drop_glue();

    if !backend.errors.is_empty() {
        return Err(backend.errors);
    }
    backend.finish(&machine, out)
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
            if !out
                .variants
                .iter()
                .any(|v| v.type_name == variant.type_name && v.name == variant.name)
            {
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

/// Why this type cannot cross the C ABI, or `None` if it can.
///
/// **Scalars and pointers only.** The rule comes from errata 35, where a
/// 16-byte aggregate crossed between generated code and the runtime and the
/// two sides disagreed about how one comes back — silently, in the direction
/// that made every failing test report as passing. The runtime is only the
/// first foreign library; a binding the user writes is the same boundary.
///
/// `docs/design/ffi.md` has the full contract.
fn foreign_obstacle(ty: &Type) -> Option<&'static str> {
    match ty {
        Type::Int | Type::Fixed(_) | Type::Float | Type::Bool | Type::Ptr => None,
        Type::Str => Some(
            "a `String` is a reference-counted heap object with a header the C side knows \
             nothing about; pass its bytes and length instead",
        ),
        Type::Adt { .. } | Type::Tuple(_) => Some(
            "a Khora object is a reference-counted heap allocation, so the foreign side \
             would get a pointer it cannot read and a reference it cannot release",
        ),
        Type::Fn { .. } => Some(
            "a closure is a heap object holding its captures, and C expects a bare function \
             pointer",
        ),
        Type::Param(_) | Type::Applied { .. } | Type::Assoc { .. } | Type::Var(_) => Some(
            "a generic function has no single machine signature, and there is no body to \
             specialize",
        ),
        Type::Unit => Some("`()` is not a value; a foreign function may only *return* it"),
        Type::Row { .. } | Type::Const(_) | Type::Never | Type::Unknown => {
            Some("it is not a type the C ABI has")
        }
    }
}

/// Why this whole signature cannot be a foreign function's, if it cannot.
///
/// Checked where the call is generated rather than at the declaration, so an
/// unused binding to something this target does not have is not an error on a
/// target that does not need it.
pub(crate) fn foreign_signature_obstacle(signature: &Signature) -> Option<String> {
    if !signature.generics.is_empty() {
        return Some(
            "it is generic, and a generic function has no single machine signature".to_string(),
        );
    }
    if can_raise(signature) {
        return Some(
            "it can raise, and a fallible function returns a tagged pair — which is exactly \
             the aggregate that must not cross (errata 35). C reports failure in its return \
             value, and the wrapper that turns that into a raise belongs in Khora"
                .to_string(),
        );
    }
    for param in &signature.params {
        if let Some(why) = foreign_obstacle(param) {
            return Some(format!("its parameter of type `{param}` cannot cross: {why}"));
        }
    }
    if !matches!(signature.ret, Type::Unit) {
        if let Some(why) = foreign_obstacle(&signature.ret) {
            return Some(format!("its return type `{}` cannot cross: {why}", signature.ret));
        }
    }
    None
}

/// Whether a signature's `raises` row has anything in it.
pub(crate) fn can_raise(signature: &Signature) -> bool {
    match &signature.raises {
        Type::Row { fields, tail } => !fields.is_empty() || tail.is_some(),
        _ => false,
    }
}

/// The capabilities a signature requires, in the order they are passed.
///
/// Sorted by label, which `Type::row` already guarantees, so the caller and
/// the callee agree on the order without it being recorded anywhere.
pub(crate) fn evidence_of(signature: &Signature) -> Vec<(String, Type)> {
    match &signature.requires {
        Type::Row { fields, .. } => fields.clone(),
        _ => Vec::new(),
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
            RelocMode::Default,
            CodeModel::Default,
        )
        .ok_or_else(|| vec![backend_error("creating the target machine")])
}

/// The key the shared closure `drop_fields` is cached under. Not a legal Khora
/// type name, so it can never collide with an ADT's.
const CLOSURE_GLUE: &str = "$closure";

/// A type as it appears in a generated symbol name.
fn mangle_type(ty: &Type) -> String {
    ty.to_string().replace(['<', '>', ',', ' '], "$").replace("$$", "$")
}

/// The tag an adapter closure carries. Far above any real closure site, so the
/// shared `drop_fields` switch never has a case for it — which is right, since
/// an adapter captures nothing.
pub(crate) const CLOSURE_ADAPTER_TAG: u64 = u32::MAX as u64;

/// A closure's field 0 is its function pointer; captures start after it.
pub(crate) const CLOSURE_CAPTURE_BASE: usize = 1;

/// Everything shared by every function in the module under construction.
pub(crate) struct Backend<'ctx> {
    pub ctx: &'ctx Context,
    pub module: Module<'ctx>,
    pub builder: Builder<'ctx>,
    pub rt: Runtime<'ctx>,
    pub types: TypeMap,
    /// Khora function name to the LLVM function, whether definition or extern.
    functions: HashMap<String, FunctionValue<'ctx>>,
    /// Which names the file actually defines. Anything else it declares is an
    /// extern.
    defined: HashSet<String>,
    /// Specialized signatures, by mangled symbol. See `signature_of`.
    instance_signatures: HashMap<String, Signature>,
    /// Per-ADT `drop_fields` routines. `None` records a type that owns no
    /// references, so drop sites pass a null callback rather than calling a
    /// routine that would do nothing.
    /// Keyed by the *instantiated* type, not the type's name. `Box<String>`
    /// owns a reference and `Box<Int>` does not, so one routine per name would
    /// be wrong for whichever of them it was not written for.
    drop_glue: HashMap<String, Option<FunctionValue<'ctx>>>,
    /// Glue routines declared but not yet given a body. Emitting one while a
    /// function body is being lowered would move the builder out from under
    /// the caller, so the work is queued instead — see
    /// [`Backend::emit_pending_drop_glue`].
    pending_glue: Vec<Type>,
    /// Every lambda site in the program, in discovery order. A site's index in
    /// this list is the tag its closure objects carry, which is how the shared
    /// closure drop routine knows which captures a given closure holds.
    closures: Vec<ClosureSite>,
    /// The closure sites belonging to one emitted function, by its symbol.
    closures_by_owner: HashMap<String, Vec<usize>>,
    /// Adapters that let a named function be used as a value, by the symbol
    /// each one forwards to.
    thunks: HashMap<String, FunctionValue<'ctx>>,
    /// Adapters declared but not yet given a body, for the same reason
    /// `pending_glue` exists.
    pending_thunks: Vec<String>,
    /// Trampolines that take a tagged return apart, by how many arguments the
    /// callee takes. See [`Backend::tagged_trampoline`].
    trampolines: HashMap<usize, FunctionValue<'ctx>>,
    /// One change shim per value type, keyed by how the type prints.
    change_shims: HashMap<String, FunctionValue<'ctx>>,
    /// The same, for the pair of types `Shared::modify` moves.
    modify_shims: HashMap<String, FunctionValue<'ctx>>,
    /// A program-wide id for each error type, assigned on first sight. It is
    /// the `which` of a tagged return, so 1 is the lowest: 0 means the call
    /// did not raise. See `docs/design/effect-runtime.md` §2.
    error_ids: HashMap<String, u32>,
    /// The releaser a wildcard `catch` calls, declared on first use.
    ///
    /// `khora.release_error(which, word)`. See [`Backend::release_error`].
    error_releaser: Option<FunctionValue<'ctx>>,
    pub errors: Vec<HirError>,
}

/// One `(x) => ..` in the program, lifted to a function of its own.
#[derive(Clone)]
pub(crate) struct ClosureSite {
    /// The symbol of the emitted function the lambda was written inside. A
    /// lambda in a generic function appears once per specialization, because
    /// its captures have different types in each.
    pub owner: String,
    pub expr: khora_hir::body::ExprId,
    pub symbol: String,
    pub ret: Type,
    /// What the body can raise. A closure cannot charge its failures to
    /// whoever wrote it — by the time it is called that function has returned
    /// — so the row is part of its type, and a non-empty one means the lifted
    /// function returns the tagged pair like any other fallible one.
    pub raises: Type,
    /// What the closure is *handed*, as opposed to what it captured.
    ///
    /// Usually empty — a capability in scope where the lambda was written is
    /// captured like any other binding. What lands here is one that did not
    /// exist yet at that point, supplied by whoever calls the closure:
    /// `nursery(fn () => serve()!)`. `docs/design/capability-passing.md`.
    pub requires: Type,
    pub captures: Vec<(khora_hir::body::LocalId, Type)>,
}

impl ClosureSite {
    /// The requirement row, as the thing `evidence_of` reads.
    ///
    /// A shim rather than a second copy of the field access, so the closure and
    /// the named function ask the same function what order the labels go in.
    pub(crate) fn requires_signature(&self) -> Signature {
        Signature {
            is_extern: false,
            generics: Vec::new(),
            bounds: Vec::new(),
            requires: self.requires.clone(),
            raises: Type::empty_row(),
            params: Vec::new(),
            ret: Type::Unit,
        }
    }
}

impl<'ctx> Backend<'ctx> {
    fn new(
        ctx: &'ctx Context,
        name: &str,
        types: TypeMap,
        machine: &TargetMachine,
    ) -> Backend<'ctx> {
        let module = ctx.create_module(name);
        module.set_triple(&machine.get_triple());
        // Bind the target data rather than chaining: the `DataLayout` borrows
        // it, and a temporary here is a use-after-free the borrow checker
        // cannot see through the FFI.
        let target_data = machine.get_target_data();
        module.set_data_layout(&target_data.get_data_layout());

        let rt = Runtime::declare(ctx, &module);
        Backend {
            ctx,
            module,
            builder: ctx.create_builder(),
            rt,
            types,
            functions: HashMap::new(),
            defined: HashSet::new(),
            instance_signatures: HashMap::new(),
            drop_glue: HashMap::new(),
            pending_glue: Vec::new(),
            closures: Vec::new(),
            closures_by_owner: HashMap::new(),
            thunks: HashMap::new(),
            pending_thunks: Vec::new(),
            trampolines: HashMap::new(),
            change_shims: HashMap::new(),
            modify_shims: HashMap::new(),
            error_ids: HashMap::new(),
            error_releaser: None,
            errors: Vec::new(),
        }
    }

    // -----------------------------------------------------------------------
    // Diagnostics
    // -----------------------------------------------------------------------

    pub fn error(&mut self, message: impl Into<String>, range: TextRange) {
        self.errors.push(HirError { message: message.into(), range });
    }

    // -----------------------------------------------------------------------
    // Types
    // -----------------------------------------------------------------------

    /// The machine representation of a Khora type.
    ///
    /// `Unit` is a word rather than nothing at all. Making it void would mean
    /// every expression's lowering returns an optional value and every consumer
    /// handles the absent case, to represent something no program can observe;
    /// an ignored `i64` costs one register the optimizer deletes. Functions
    /// returning `Unit` still return `void`, because that is a real ABI
    /// difference rather than an internal convenience.
    pub fn llvm_type(&self, ty: &Type) -> Option<BasicTypeEnum<'ctx>> {
        match ty {
            Type::Int | Type::Unit => Some(self.ctx.i64_type().into()),
            Type::Float => Some(self.ctx.f64_type().into()),
            // A `U8` is an `i8`, so an array of them is packed rather than one
            // byte per word. Signedness is not in the LLVM type — it is in the
            // instruction — so `U8` and `I8` share this and differ at every
            // `div`, `shr` and ordering comparison.
            Type::Fixed(kind) => Some(self.int_width(kind.bits.into()).into()),
            Type::Bool => Some(self.ctx.bool_type().into()),
            // A closure is a heap object holding its function pointer and its
            // captures, so a value of function type is a pointer to one. `Ptr`
            // is a pointer that is only a pointer: no header, no count.
            Type::Ptr | Type::Str | Type::Adt { .. } | Type::Fn { .. } => {
                Some(self.ctx.ptr_type(AddressSpace::default()).into())
            }
            // A variable or a rigid parameter reaching code generation means
            // inference left something unsolved, or a generic function was not
            // monomorphized. Both are compiler bugs rather than user errors, so
            // there is no representation to pick here.
            Type::Var(_) | Type::Param(_) => None,
            // Tuples type check but have no layout yet; `lower` reports that
            // in the one place it can happen, rather than here.
            // A projection reaching here never normalized, which means the
            // owner was never pinned down. That is a type error reported
            // elsewhere, not a shape the backend could pick.
            // A row is a compile-time description of what a function needs,
            // not a value: nothing is ever emitted holding one.
            Type::Tuple(_)
            | Type::Const(_)
            | Type::Applied { .. }
            | Type::Assoc { .. }
            | Type::Row { .. } => None,
            Type::Never | Type::Unknown => None,
        }
    }

    /// `{ i32 which, i64 payload }` — what a fallible function returns.
    ///
    /// `which` is 0 for an ordinary return and otherwise the error's type id,
    /// so one field carries both "did this raise" and "raise of what". A bare
    /// bit would answer the first question and leave `catch` unable to handle
    /// part of a row.
    pub fn tagged_type(&self) -> inkwell::types::StructType<'ctx> {
        self.ctx.struct_type(&[self.ctx.i32_type().into(), self.ctx.i64_type().into()], false)
    }

    /// The id of an error type, assigning one if this is the first sight of it.
    ///
    /// Encounter order within a single whole-program module, which is
    /// deterministic for a given program and never crosses a module boundary
    /// — there is no separate compilation yet, and when there is, this becomes
    /// a link-time numbering rather than a lazy one.
    /// Releases an error whose type is not known where it is caught.
    ///
    /// `catch { _ => .. }` handles the whole row, tail and all, so the arm has
    /// no static type to select drop glue from — and the row may be `'e`, which
    /// nothing at this point in the pipeline can enumerate either. Dropping the
    /// object with a null callback would free the object and leak every boxed
    /// field inside it, once per caught error, which on a server's failure path
    /// is a leak per request rather than a bounded one.
    ///
    /// So the dispatch is deferred to a function emitted once, at the end,
    /// when every error type in the program has an id: a `switch` on `which`
    /// whose cases each release the word as the type that id belongs to. The
    /// caller only has to know the id, which is the one thing it does know.
    ///
    /// [`Backend::emit_error_releaser`] is the definition.
    pub fn release_error(&mut self) -> FunctionValue<'ctx> {
        if let Some(existing) = self.error_releaser {
            return existing;
        }
        let signature = self.ctx.void_type().fn_type(
            &[self.ctx.i32_type().into(), self.ctx.i64_type().into()],
            false,
        );
        let function = self.module.add_function("khora.release_error", signature, None);
        self.error_releaser = Some(function);
        function
    }

    /// Defines the releaser, if anything asked for it.
    ///
    /// Emitted after every function and every lifted closure, because lowering
    /// is what assigns error ids and one more may be assigned by the last body
    /// compiled.
    pub fn emit_error_releaser(&mut self) {
        let Some(function) = self.error_releaser else { return };
        let entry = self.ctx.append_basic_block(function, "entry");
        let done = self.ctx.append_basic_block(function, "done");

        let which = function.get_nth_param(0).expect("which").into_int_value();
        let word = function.get_nth_param(1).expect("word").into_int_value();

        // By id, so the switch reads in the order the ids were handed out and
        // two compilations of the same program emit the same function.
        let mut known: Vec<(String, u32)> =
            self.error_ids.iter().map(|(n, i)| (n.clone(), *i)).collect();
        known.sort_by_key(|(_, id)| *id);

        let mut cases = Vec::with_capacity(known.len());
        for (name, id) in &known {
            let block = self.ctx.append_basic_block(function, &format!("release.{name}"));
            self.builder.position_at_end(block);
            let ty = Type::adt(name);
            if is_boxed(&ty) {
                let value = self.word_to_value(word, &ty);
                let glue = self.drop_glue(&ty);
                let drop = self.rt.drop;
                self.builder
                    .build_call(drop, &[value.into(), glue.into()], "")
                    .expect("releasing a caught error");
            }
            self.builder.build_unconditional_branch(done).expect("leaving a release case");
            cases.push((self.ctx.i32_type().const_int(u64::from(*id), false), block));
        }

        self.builder.position_at_end(entry);
        self.builder.build_switch(which, done, &cases).expect("dispatching on the error type");

        // Anything with no id owns nothing this function knows how to release.
        // A cancellation reaches here only if a caller passed one on purpose;
        // it carries no payload, so doing nothing is right.
        self.builder.position_at_end(done);
        self.builder.build_return(None).expect("returning from the releaser");
    }

    pub fn error_id(&mut self, name: &str) -> u32 {
        if let Some(id) = self.error_ids.get(name) {
            return *id;
        }
        let id = self.error_ids.len() as u32 + 1;
        self.error_ids.insert(name.to_string(), id);
        id
    }

    /// A value as the one word a tagged return carries it in.
    ///
    /// Every Khora value fits: an `Int` is already one, a `Bool` widens, and
    /// everything boxed is a pointer.
    pub fn to_word(&self, value: BasicValueEnum<'ctx>) -> inkwell::values::IntValue<'ctx> {
        match value {
            BasicValueEnum::PointerValue(p) => self
                .builder
                .build_ptr_to_int(p, self.ctx.i64_type(), "word")
                .expect("a pointer as a word"),
            BasicValueEnum::IntValue(i) if i.get_type().get_bit_width() < 64 => self
                .builder
                .build_int_z_extend(i, self.ctx.i64_type(), "word")
                .expect("widening to a word"),
            BasicValueEnum::IntValue(i) => i,
            other => other.into_int_value(),
        }
    }

    /// The inverse: a word read back as a value of `ty`.
    pub fn word_to_value(
        &self,
        word: inkwell::values::IntValue<'ctx>,
        ty: &Type,
    ) -> BasicValueEnum<'ctx> {
        match self.llvm_type(ty) {
            Some(BasicTypeEnum::PointerType(p)) => self
                .builder
                .build_int_to_ptr(word, p, "unword")
                .expect("a word as a pointer")
                .into(),
            Some(BasicTypeEnum::IntType(i)) if i.get_bit_width() < 64 => self
                .builder
                .build_int_truncate(word, i, "unword")
                .expect("narrowing from a word")
                .into(),
            _ => word.into(),
        }
    }

    /// The zero value of a type: `null` for a pointer, `0` otherwise.
    ///
    /// Every local slot starts here. A boxed slot holding null is what makes an
    /// unconditional `drop` safe on a path where the binding was never reached
    /// — the runtime documents null tolerance for exactly this.
    pub fn zero_value(&self, ty: &Type) -> BasicValueEnum<'ctx> {
        match self.llvm_type(ty) {
            Some(BasicTypeEnum::PointerType(p)) => p.const_null().into(),
            Some(BasicTypeEnum::IntType(i)) => i.const_zero().into(),
            _ => self.ctx.i64_type().const_zero().into(),
        }
    }

    /// The value standing for `()`.
    pub fn unit_value(&self) -> BasicValueEnum<'ctx> {
        self.ctx.i64_type().const_zero().into()
    }

    /// A null pointer, for a drop with no field routine.
    pub fn null_pointer(&self) -> PointerValue<'ctx> {
        self.ctx.ptr_type(AddressSpace::default()).const_null()
    }

    fn function_type(&self, signature: &Signature) -> Option<FunctionType<'ctx>> {
        self.shaped(signature, false)
    }

    /// The machine type of a function, as a Khora definition or as a foreign
    /// declaration.
    ///
    /// The two differ in exactly one way, and it is the whole of decision 3 in
    /// `docs/design/ffi.md`: **a `with` clause on a foreign function is a
    /// permission, and nothing is appended to the call.** A C function has no
    /// use for a Khora record of closures, so passing one would be meaningless;
    /// but requiring it is how the boundary is governed, since nothing can open
    /// a file without holding `Fs` and `Fs` is not something a function can
    /// conjure.
    fn shaped(&self, signature: &Signature, foreign: bool) -> Option<FunctionType<'ctx>> {
        let mut params: Vec<BasicMetadataTypeEnum<'ctx>> = Vec::new();
        for param in &signature.params {
            params.push(self.llvm_type(param)?.into());
        }
        // Capabilities are ordinary parameters, appended after the written
        // ones in label order. The row is sorted, so both sides agree without
        // anything being written down twice.
        if !foreign {
            for (_, capability) in evidence_of(signature) {
                params.push(self.llvm_type(&capability)?.into());
            }
        }
        // A function that can raise returns a tagged word instead of its
        // value: `{ i1 raised, i64 payload }`. One word suffices because every
        // Khora value is word-sized — the same fact `store_field` relies on —
        // and two fields come back in registers rather than through memory.
        //
        // No unwinder, no landing pads, no personality routine: a raise is a
        // return with a tag. `docs/design/effect-runtime.md` §2.
        if can_raise(signature) {
            return Some(self.tagged_type().fn_type(&params, false));
        }
        Some(match &signature.ret {
            Type::Unit => self.ctx.void_type().fn_type(&params, false),
            other => self.llvm_type(other)?.fn_type(&params, false),
        })
    }

    // -----------------------------------------------------------------------
    // ADTs
    // -----------------------------------------------------------------------

    /// The variants of an ADT, in declaration order.
    ///
    /// Order is the whole point: a variant's index in this list *is* its tag,
    /// which is what `match` switches on and what a constructor stores. It is
    /// declaration order because `khora_types::type_map` pushes variants as it
    /// reads them, and nothing between here and there sorts them.
    pub fn variants_of(&self, type_name: &str) -> Vec<VariantInfo> {
        self.types.variants.iter().filter(|v| v.type_name == type_name).cloned().collect()
    }

    /// A constructor's tag, and the fields it carries.
    /// A constructor's tag and fields, found by its type *and* its own name.
    ///
    /// The type is not optional. Case names repeat across a program, and a tag
    /// is an index within one type's variant list, so a lookup by bare name
    /// silently returns another type's tag — which is a `match` taking the
    /// wrong arm rather than a diagnostic.
    pub fn variant_of(&self, type_name: &str, case: &str) -> Option<(u32, VariantInfo)> {
        let variants = self.variants_of(type_name);
        let tag = variants.iter().position(|v| v.name == case)?;
        Some((tag as u32, variants[tag].clone()))
    }

    // -----------------------------------------------------------------------
    // Functions
    // -----------------------------------------------------------------------

    /// Declares a function the file defines, under its mangled name.
    /// The signature to compile `name` against.
    ///
    /// A specialization is registered under its mangled symbol with its type
    /// arguments already substituted, so the backend never sees a rigid
    /// parameter. Anything not registered is a plain function under its own
    /// name.
    pub fn signature_of(&self, name: &str) -> Option<khora_types::Signature> {
        self.instance_signatures
            .get(name)
            .cloned()
            .or_else(|| self.types.signatures.get(name).cloned())
    }

    pub fn register_instance(&mut self, symbol: &str, signature: khora_types::Signature) {
        self.instance_signatures.insert(symbol.to_string(), signature);
    }

    fn declare_definition(&mut self, name: &str) {
        let Some(signature) = self.signature_of(name) else { return };
        let Some(ty) = self.function_type(&signature) else {
            self.error(
                format!(
                    "`{name}` cannot be compiled: every parameter and the return type need a \
                     type the backend knows (`Int`, `Bool`, `String`, `()` or an ADT)"
                ),
                TextRange::empty(0.into()),
            );
            return;
        };
        let f = self.module.add_function(&mangle(name), ty, Some(Linkage::External));
        self.functions.insert(name.to_string(), f);
        self.defined.insert(name.to_string());
    }

    /// The LLVM function for a call to `name`, declaring an extern on demand.
    ///
    /// A name the file declares but does not define is a C symbol: the
    /// generated code calls it unmangled, which is what lets Khora source reach
    /// the runtime. Nothing checks that the symbol exists — the linker does
    /// that, and its message names the symbol.
    ///
    /// # The trap
    ///
    /// `add_function` with a name the module already has does **not** fail. It
    /// silently appends a suffix, so declaring `khora_print_int` from Khora
    /// source on top of the runtime's own declaration produces a call to
    /// `khora_print_int.3` — which nothing defines, and which surfaces as an
    /// undefined symbol from the linker with no hint of where the `.3` came
    /// from. Always look the name up first.
    pub fn callee(&mut self, name: &str) -> Result<FunctionValue<'ctx>, String> {
        if let Some(f) = self.functions.get(name) {
            return Ok(*f);
        }
        let signature = self
            .signature_of(name)
            .ok_or_else(|| format!("`{name}` has no signature to call through"))?;
        // Three kinds of function reach this point, and only one of them is a
        // call to somewhere else.
        //
        // A Khora body is defined here and needs nothing said about it. An
        // `extern fn` is a C symbol, and what may cross to one is a much
        // shorter list than what the backend can represent. And anything else
        // with no body is a **declaration nobody has kept** — a signature
        // written ahead of its implementation, which is a useful thing to have
        // and not a thing to call. That last kind used to be treated as a C
        // symbol silently, so a misspelled name became `undefined symbol` from
        // the linker rather than a sentence about the program.
        //
        // Checked where the call is generated rather than at the declaration,
        // so a signature written ahead of its implementation is only an error
        // once something tries to run it.
        let foreign = !self.is_defined(name);
        if foreign {
            if !signature.is_extern {
                return Err(format!(
                    "`{name}` has no body, so there is nothing to call. Give it one, or \
                     write `extern fn` if it is a C symbol to be found at link time"
                ));
            }
            if let Some(why) = foreign_signature_obstacle(&signature) {
                return Err(format!(
                    "`{name}` is `extern`, so it crosses the C ABI, and {why}. \
                     Only scalars and pointers cross — `docs/design/ffi.md`"
                ));
            }
        }
        let ty = self.shaped(&signature, foreign).ok_or_else(|| {
            format!(
                "`{name}` has a parameter or return type the backend cannot represent yet; \
                 phase 2 handles `Int`, `Bool`, `String`, `()` and ADTs"
            )
        })?;

        let f = match self.module.get_function(name) {
            Some(existing) if existing.get_type() == ty => existing,
            Some(_) => {
                return Err(format!(
                    "`{name}` is already provided by the runtime with a different signature; \
                     declaring it here would silently call something else"
                ))
            }
            None => self.module.add_function(name, ty, Some(Linkage::External)),
        };
        self.functions.insert(name.to_string(), f);
        Ok(f)
    }

    /// Whether the file defines this name, as opposed to only declaring it.
    pub fn is_defined(&self, name: &str) -> bool {
        self.defined.contains(name)
    }

    /// The function a definition was declared as, for giving it a body.
    pub fn definition(&self, name: &str) -> Option<FunctionValue<'ctx>> {
        self.defined.contains(name).then(|| self.functions[name])
    }

    // -----------------------------------------------------------------------
    // Drop glue
    // -----------------------------------------------------------------------

    // -----------------------------------------------------------------------
    // Closures
    // -----------------------------------------------------------------------

    /// Records a lambda site and declares the function it lifts to.
    ///
    /// The lifted function takes the closure object as its first argument and
    /// the lambda's own parameters after it, which is what makes an indirect
    /// call possible without knowing anything about the captures at the call
    /// site.
    /// `shape` is the lambda's `Type::Fn` — all four of what it takes, gives
    /// back, can fail with and has to be handed, which travel together because
    /// they are one type.
    pub fn declare_closure(
        &mut self,
        owner: &str,
        expr: khora_hir::body::ExprId,
        shape: Type,
        captures: Vec<(khora_hir::body::LocalId, Type)>,
    ) -> Option<usize> {
        let Type::Fn { params, ret, raises, requires } = shape else { return None };
        let (params, ret, raises, requires) = (params, *ret, *raises, *requires);
        let index = self.closures.len();
        let symbol = format!("{owner}$$lambda{}", self.closures_by_owner.get(owner).map_or(0, Vec::len));

        let ptr = self.ctx.ptr_type(AddressSpace::default());
        let mut llvm_params: Vec<BasicMetadataTypeEnum<'ctx>> = vec![ptr.into()];
        for param in &params {
            llvm_params.push(self.llvm_type(param)?.into());
        }
        // After the written parameters, in label order — the same convention a
        // named function follows, and the one `invoke_closure_at` already
        // builds its call around.
        let handed: Vec<(String, Type)> = match &requires {
            Type::Row { fields, .. } => fields.clone(),
            _ => Vec::new(),
        };
        for (_, ty) in &handed {
            llvm_params.push(self.llvm_type(ty)?.into());
        }
        let fallible =
            matches!(&raises, Type::Row { fields, tail } if !fields.is_empty() || tail.is_some());
        let fn_type = if fallible {
            self.tagged_type().fn_type(&llvm_params, false)
        } else {
            match &ret {
                Type::Unit => self.ctx.void_type().fn_type(&llvm_params, false),
                other => self.llvm_type(other)?.fn_type(&llvm_params, false),
            }
        };

        let f = self.module.add_function(&mangle(&symbol), fn_type, Some(Linkage::Internal));
        self.functions.insert(symbol.clone(), f);
        self.defined.insert(symbol.clone());
        self.instance_signatures.insert(
            symbol.clone(),
            Signature {
                is_extern: false,
                generics: Vec::new(),
                bounds: Vec::new(),
                requires: requires.clone(),
                raises: raises.clone(),
                params: params.clone(),
                ret: ret.clone(),
            },
        );

        self.closures.push(ClosureSite {
            owner: owner.to_string(),
            expr,
            symbol,
            ret,
            raises,
            requires,
            captures,
        });
        self.closures_by_owner.entry(owner.to_string()).or_default().push(index);
        Some(index)
    }

    /// The closure site for a lambda expression inside `owner`.
    pub fn closure_at(&self, owner: &str, expr: khora_hir::body::ExprId) -> Option<&ClosureSite> {
        self.closures_by_owner
            .get(owner)?
            .iter()
            .map(|i| &self.closures[*i])
            .find(|c| c.expr == expr)
    }

    /// The tag a closure site's objects carry.
    pub fn closure_tag(&self, owner: &str, expr: khora_hir::body::ExprId) -> Option<u32> {
        self.closures_by_owner
            .get(owner)?
            .iter()
            .find(|i| self.closures[**i].expr == expr)
            .map(|i| *i as u32)
    }

    pub fn closure_sites(&self) -> Vec<ClosureSite> {
        self.closures.clone()
    }

    /// One of LLVM's `*.with.overflow` intrinsics, declared on first use.
    ///
    /// Each returns `{ i64, i1 }` — the result and whether it wrapped — so the
    /// check is a branch on a flag the same instruction already produced.
    /// The LLVM integer type of a given width.
    ///
    /// Only four widths exist, so this is a match rather than
    /// `custom_width_int_type` — which takes a `NonZero` and hands back a
    /// `Result` for a question that cannot fail here.
    pub fn int_width(&self, bits: u32) -> inkwell::types::IntType<'ctx> {
        match bits {
            8 => self.ctx.i8_type(),
            16 => self.ctx.i16_type(),
            32 => self.ctx.i32_type(),
            _ => self.ctx.i64_type(),
        }
    }

    pub fn overflow_intrinsic(&mut self, name: &str, bits: u32) -> FunctionValue<'ctx> {
        if let Some(f) = self.module.get_function(name) {
            return f;
        }
        let width = self.int_width(bits);
        let pair = self.ctx.struct_type(&[width.into(), self.ctx.bool_type().into()], false);
        self.module.add_function(
            name,
            pair.fn_type(&[width.into(), width.into()], false),
            Some(Linkage::External),
        )
    }

    /// A shim that calls a fallible function and hands back its tag.
    ///
    /// The runtime cannot call a fallible Khora function directly. Its return
    /// is a 16-byte aggregate, and how one of those comes back is a target
    /// decision that LLVM makes for `{ i32, i64 }` and rustc makes for a
    /// `repr(C)` struct of the same shape — on x86-64 Windows they disagree,
    /// and the disagreement is silent: the tag reads as zero and every failure
    /// looks like a pass.
    ///
    /// So nothing but scalars crosses the boundary. The aggregate is taken
    /// apart on *this* side, where both halves of the call are LLVM's and
    /// agree by construction, and the runtime gets an `i32` back and a
    /// pointer to write the payload through.
    ///
    /// `arity` is how many arguments the callee takes: a test takes none, a
    /// fiber's thunk takes its closure. One shim per arity, not per callee.
    /// The shim `khora_shared_update` calls the change function through.
    ///
    /// The runtime cannot know `A`. It has the value as the one word every
    /// Khora value fits in, and a closure whose parameter and result are `A` —
    /// so the conversion happens here, once per `A`, on the side of the
    /// boundary that knows what `A` is. Only scalars and pointers cross, which
    /// is the same rule the foreign-function interface follows.
    ///
    /// `uint64_t shim(void *code, void *closure, uint64_t value)`.
    pub fn change_shim(&mut self, value_ty: &Type) -> Option<FunctionValue<'ctx>> {
        let key = value_ty.to_string();
        if let Some(f) = self.change_shims.get(&key) {
            return Some(*f);
        }

        let ptr = self.ctx.ptr_type(AddressSpace::default());
        let i64_type = self.ctx.i64_type();
        let f = self.module.add_function(
            &format!("kh$change{}", self.change_shims.len()),
            i64_type.fn_type(&[ptr.into(), ptr.into(), i64_type.into()], false),
            Some(Linkage::Internal),
        );
        self.change_shims.insert(key, f);

        let saved = self.builder.get_insert_block();
        let entry = self.ctx.append_basic_block(f, "entry");
        self.builder.position_at_end(entry);

        let code = f.get_nth_param(0).expect("a code pointer").into_pointer_value();
        let closure = f.get_nth_param(1).expect("the closure").into_pointer_value();
        let word = f.get_nth_param(2).expect("the value").into_int_value();

        let llvm_ty = self.llvm_type(value_ty)?;
        let given = self.word_to_value(word, value_ty);
        let callee_type = llvm_ty.fn_type(&[ptr.into(), llvm_ty.into()], false);
        let produced = self
            .builder
            .build_indirect_call(callee_type, code, &[closure.into(), given.into()], "changed")
            .expect("calling a change function")
            .try_as_basic_value()
            .basic()
            .expect("a change function gives back a value");
        let back = self.to_word(produced);
        self.builder.build_return(Some(&back)).expect("handing back the new value");

        if let Some(block) = saved {
            self.builder.position_at_end(block);
        }
        Some(f)
    }

    /// The shim `khora_shared_modify` calls its change function through.
    ///
    /// [`Backend::change_shim`] with one more thing to do. The change function
    /// gives back a `Changed<A, B>` — one heap object holding the new state and
    /// the answer — and the runtime cannot take a Khora record apart, so it is
    /// taken apart here, where the layout is known. Two words come out where
    /// only one can be returned, so the answer goes through a pointer.
    ///
    /// The record itself is released: it was built to carry two values across
    /// one call and nothing holds it afterwards.
    ///
    /// `uint64_t shim(void *code, void *closure, uint64_t value, uint64_t *answer)`.
    pub fn modify_shim(&mut self, state: &Type, answer: &Type) -> Option<FunctionValue<'ctx>> {
        let key = format!("{state}=>{answer}");
        if let Some(f) = self.modify_shims.get(&key) {
            return Some(*f);
        }

        let ptr = self.ctx.ptr_type(AddressSpace::default());
        let i64_type = self.ctx.i64_type();
        let f = self.module.add_function(
            &format!("kh$modify{}", self.modify_shims.len()),
            i64_type.fn_type(&[ptr.into(), ptr.into(), i64_type.into(), ptr.into()], false),
            Some(Linkage::Internal),
        );
        self.modify_shims.insert(key, f);

        let saved = self.builder.get_insert_block();
        let entry = self.ctx.append_basic_block(f, "entry");
        self.builder.position_at_end(entry);

        let code = f.get_nth_param(0).expect("a code pointer").into_pointer_value();
        let closure = f.get_nth_param(1).expect("the closure").into_pointer_value();
        let word = f.get_nth_param(2).expect("the value").into_int_value();
        let out = f.get_nth_param(3).expect("somewhere for the answer").into_pointer_value();

        let state_ty = self.llvm_type(state)?;
        let given = self.word_to_value(word, state);
        let callee_type = ptr.fn_type(&[ptr.into(), state_ty.into()], false);
        let pair = self
            .builder
            .build_indirect_call(callee_type, code, &[closure.into(), given.into()], "changed")
            .expect("calling a change function")
            .try_as_basic_value()
            .basic()
            .expect("a change function gives back a record")
            .into_pointer_value();

        // Field order is declaration order, and `Changed` declares `state`
        // first. Both are duplicated out of the record before it goes.
        let next = self.read_from(pair, 0, state);
        let result = self.read_from(pair, 1, answer);
        let glue = self.drop_glue(&Type::adt("Changed"));
        self.builder
            .build_call(self.rt.drop, &[pair.into(), glue.into()], "")
            .expect("releasing the carrier");

        let result = self.to_word(result);
        self.builder.build_store(out, result).expect("handing back the answer");
        let next = self.to_word(next);
        self.builder.build_return(Some(&next)).expect("handing back the new state");

        if let Some(block) = saved {
            self.builder.position_at_end(block);
        }
        Some(f)
    }

    /// One field of a record, with a reference of its own.
    ///
    /// The shims are outside `Lower`, which is where the ordinary field read
    /// lives, so this is the small part of it they need.
    fn read_from(
        &mut self,
        object: PointerValue<'ctx>,
        index: u64,
        ty: &Type,
    ) -> BasicValueEnum<'ctx> {
        let slot = runtime::field_pointer(self.ctx, &self.builder, object, index);
        let llvm = self.llvm_type(ty).unwrap_or_else(|| self.ctx.i64_type().into());
        let value = self.builder.build_load(llvm, slot, "field").expect("loading a field");
        if is_boxed(ty) {
            self.builder
                .build_call(self.rt.dup, &[value.into()], "")
                .expect("keeping a field past its record");
        }
        value
    }

    pub fn tagged_trampoline(&mut self, arity: usize) -> FunctionValue<'ctx> {
        if let Some(f) = self.trampolines.get(&arity) {
            return *f;
        }

        let ptr = self.ctx.ptr_type(AddressSpace::default());
        let i32_type = self.ctx.i32_type();
        let mut params: Vec<BasicMetadataTypeEnum<'ctx>> = vec![ptr.into()];
        params.extend(std::iter::repeat_n(BasicMetadataTypeEnum::from(ptr), arity));
        params.push(ptr.into());

        let f = self.module.add_function(
            &format!("kh$tagged_call{arity}"),
            i32_type.fn_type(&params, false),
            Some(Linkage::Internal),
        );
        self.trampolines.insert(arity, f);

        // Emitted at once rather than queued: it calls nothing that has to be
        // discovered first, and it borrows the builder for four instructions.
        let saved = self.builder.get_insert_block();
        let entry = self.ctx.append_basic_block(f, "entry");
        self.builder.position_at_end(entry);

        let code = f.get_nth_param(0).expect("a code pointer").into_pointer_value();
        let args: Vec<BasicMetadataValueEnum<'ctx>> =
            (0..arity).filter_map(|i| f.get_nth_param(i as u32 + 1)).map(|v| v.into()).collect();
        let out = f
            .get_nth_param(arity as u32 + 1)
            .expect("somewhere to put the payload")
            .into_pointer_value();

        let callee_params: Vec<BasicMetadataTypeEnum<'ctx>> =
            std::iter::repeat_n(BasicMetadataTypeEnum::from(ptr), arity).collect();
        let callee_type = self.tagged_type().fn_type(&callee_params, false);
        let result = self
            .builder
            .build_indirect_call(callee_type, code, &args, "outcome")
            .expect("calling a fallible function")
            .try_as_basic_value()
            .basic()
            .expect("a fallible function returns a tagged value")
            .into_struct_value();

        let which = self
            .builder
            .build_extract_value(result, 0, "which")
            .expect("reading the tag")
            .into_int_value();
        let payload =
            self.builder.build_extract_value(result, 1, "payload").expect("reading the payload");
        self.builder.build_store(out, payload).expect("handing back the payload");
        self.builder.build_return(Some(&which)).expect("handing back the tag");

        if let Some(block) = saved {
            self.builder.position_at_end(block);
        }
        f
    }

    /// The adapter that lets `symbol` be used as a closure.
    ///
    /// A named function and a closure have different shapes: the closure is
    /// called through a pointer with its own object as the first argument, and
    /// the named function has no such argument. Rather than give every function
    /// in the program that parameter — which would cost every ordinary call to
    /// pay for a feature it does not use — a one-line adapter is emitted for
    /// the functions actually used as values, and it forwards.
    pub fn thunk(&mut self, symbol: &str) -> Option<FunctionValue<'ctx>> {
        if let Some(f) = self.thunks.get(symbol) {
            return Some(*f);
        }
        let signature = self.signature_of(symbol)?;

        let ptr = self.ctx.ptr_type(AddressSpace::default());
        let mut params: Vec<BasicMetadataTypeEnum<'ctx>> = vec![ptr.into()];
        for param in &signature.params {
            params.push(self.llvm_type(param)?.into());
        }
        // The adapter wears the same shape the real function does, evidence
        // and tag included, so forwarding is a straight pass-through. A
        // function value's convention follows its *type*, and its type says
        // what it needs and how it can fail.
        for (_, ty) in evidence_of(&signature) {
            params.push(self.llvm_type(&ty)?.into());
        }
        let fn_type = if can_raise(&signature) {
            self.tagged_type().fn_type(&params, false)
        } else {
            match &signature.ret {
                Type::Unit => self.ctx.void_type().fn_type(&params, false),
                other => self.llvm_type(other)?.fn_type(&params, false),
            }
        };

        let f = self.module.add_function(
            &format!("kh$fnval${symbol}"),
            fn_type,
            Some(Linkage::Internal),
        );
        self.thunks.insert(symbol.to_string(), f);
        self.pending_thunks.push(symbol.to_string());
        Some(f)
    }

    /// Gives every queued adapter its body: call the real function, return it.
    fn emit_pending_thunks(&mut self) {
        while let Some(symbol) = self.pending_thunks.pop() {
            let Some(f) = self.thunks.get(&symbol).copied() else { continue };
            let Ok(target) = self.callee(&symbol) else { continue };
            let Some(signature) = self.signature_of(&symbol) else { continue };

            let entry = self.ctx.append_basic_block(f, "entry");
            self.builder.position_at_end(entry);

            // Skip the closure argument: the adapter ignores it, because a
            // named function captures nothing. Everything after it — the
            // written parameters and then the evidence — forwards in order.
            let forwarded = signature.params.len() + evidence_of(&signature).len();
            let args: Vec<BasicMetadataValueEnum<'ctx>> = (0..forwarded)
                .filter_map(|i| f.get_nth_param(i as u32 + 1))
                .map(|v| v.into())
                .collect();
            let call =
                self.builder.build_call(target, &args, "forward").expect("forwarding a call");

            // A fallible target hands back the tagged pair, which the adapter
            // returns unchanged — it has nothing to add and nowhere to send an
            // error of its own.
            let returns_value = can_raise(&signature) || signature.ret != Type::Unit;
            match call.try_as_basic_value().basic().filter(|_| returns_value) {
                Some(value) => {
                    self.builder
                        .build_return(Some(&value))
                        .expect("returning from an adapter");
                }
                None => {
                    self.builder.build_return(None).expect("returning from an adapter");
                }
            }
        }
    }

    /// The `drop_fields` routine shared by every closure.
    ///
    /// One routine switching on the tag, exactly as an ADT's does and for the
    /// same reason: a drop site knows only that it holds a value of *some*
    /// function type, and two lambdas with the same signature capture entirely
    /// different things. The tag is what distinguishes them.
    fn closure_glue(&mut self) -> PointerValue<'ctx> {
        if !self.closures.iter().any(|c| c.captures.iter().any(|(_, t)| is_boxed(t))) {
            return self.null_pointer();
        }
        if let Some(Some(f)) = self.drop_glue.get(CLOSURE_GLUE) {
            return f.as_global_value().as_pointer_value();
        }

        let void = self.ctx.void_type();
        let ptr = self.ctx.ptr_type(AddressSpace::default());
        let f = self.module.add_function(
            "kh$drop_fields$closure",
            void.fn_type(&[ptr.into()], false),
            Some(Linkage::Internal),
        );
        self.drop_glue.insert(CLOSURE_GLUE.to_string(), Some(f));
        // The closure routine is shared, so it is queued under a name no
        // Khora type can have rather than under an instantiation.
        self.pending_glue.push(Type::adt(CLOSURE_GLUE.to_string()));
        f.as_global_value().as_pointer_value()
    }

    /// Emits the shared closure `drop_fields`.
    fn emit_closure_glue(&mut self) {
        let f = match self.drop_glue.get(CLOSURE_GLUE) {
            Some(Some(f)) => *f,
            _ => return,
        };
        let object = f.get_nth_param(0).expect("drop_fields takes an object").into_pointer_value();

        let entry = self.ctx.append_basic_block(f, "entry");
        let done = self.ctx.append_basic_block(f, "done");

        let mut cases = Vec::new();
        for (tag, site) in self.closure_sites().into_iter().enumerate() {
            let owned: Vec<(usize, Type)> = site
                .captures
                .iter()
                .enumerate()
                .filter(|(_, (_, ty))| is_boxed(ty))
                // Field 0 holds the function pointer, so capture `i` is field
                // `i + 1`.
                .map(|(i, (_, ty))| (i + CLOSURE_CAPTURE_BASE, ty.clone()))
                .collect();
            if owned.is_empty() {
                continue;
            }

            let block = self.ctx.append_basic_block(f, &format!("drop.{}", site.symbol));
            cases.push((self.ctx.i32_type().const_int(tag as u64, false), block));
            self.builder.position_at_end(block);

            for (index, field_ty) in owned {
                let slot = runtime::field_pointer(self.ctx, &self.builder, object, index as u64);
                let value = self
                    .builder
                    .build_load(self.ctx.ptr_type(AddressSpace::default()), slot, "captured")
                    .expect("loading a captured field");
                let glue = self.drop_glue(&field_ty);
                self.builder
                    .build_call(self.rt.drop, &[value.into(), glue.into()], "")
                    .expect("dropping a capture");
            }
            self.builder.build_unconditional_branch(done).expect("branch to the return");
        }

        self.builder.position_at_end(entry);
        let tag = runtime::load_tag(self.ctx, &self.builder, object);
        self.builder.build_switch(tag, done, &cases).expect("switching on a closure tag");

        self.builder.position_at_end(done);
        self.builder.build_return(None).expect("returning from drop_fields");
    }

    /// The `drop_fields` argument for dropping a value of this type.
    ///
    /// Returns a null function pointer for anything that owns no references —
    /// a `String`, an `Int`, or an ADT whose every field is a machine word.
    /// The runtime treats null as "nothing to release", so a drop site never
    /// needs to know which case it is in.
    pub fn drop_glue(&mut self, ty: &Type) -> PointerValue<'ctx> {
        if matches!(ty, Type::Fn { .. }) {
            return self.closure_glue();
        }
        let Type::Adt { name, .. } = ty else { return self.null_pointer() };

        // A region's release is the runtime's, not one generated from a field
        // layout: its finalizers live Rust-side because deferring grows the
        // list, and nothing in Khora grows a value in place. Everything else
        // about a region is ordinary — reference counted, released by the same
        // `khora_drop` every other object goes through — which is what makes
        // its finalizers run on the paths that already release a local.
        if name == runtime::REGION_TYPE {
            return self.rt.region_release.as_global_value().as_pointer_value();
        }

        // A fiber handle's release joins the fiber. Same reasoning as a
        // region's, and the same payoff: the paths that already release a
        // binding are the paths a child has to be waited for on.
        if name == runtime::FIBER_TYPE {
            return self.rt.fiber_release.as_global_value().as_pointer_value();
        }

        // A nursery's release cancels its children and waits for them.
        if name == runtime::FIBERS_TYPE {
            return self.rt.fibers_release.as_global_value().as_pointer_value();
        }

        // A cell's release lets go of what is in it. The runtime's, because
        // the value sits behind a lock it owns, and it was told how to release
        // it when the cell was opened rather than here — generated code cannot
        // reach through a `Mutex`.
        if name == runtime::SHARED_TYPE {
            return self.rt.shared_release.as_global_value().as_pointer_value();
        }

        // An array's release loops over its elements. The loop is the
        // runtime's because the length is a run-time value; what to do with
        // one element is generated, and travels in the object.
        if name == runtime::ARRAY_TYPE {
            return self.rt.array_release.as_global_value().as_pointer_value();
        }

        let key = ty.to_string();

        if let Some(cached) = self.drop_glue.get(&key) {
            return match cached {
                Some(f) => f.as_global_value().as_pointer_value(),
                None => self.null_pointer(),
            };
        }

        // Read the fields at *this* instantiation. A generic type's declared
        // field is a parameter, which is never boxed, so asking the declaration
        // whether `Box<A>` owns anything always answers no — and every
        // `Box<String>` in the program leaks its contents.
        let variants = self.instantiated_variants(ty);
        if !variants.iter().any(|v| v.fields.iter().any(is_boxed)) {
            self.drop_glue.insert(key, None);
            return self.null_pointer();
        }

        let void = self.ctx.void_type();
        let ptr = self.ctx.ptr_type(AddressSpace::default());
        let f = self.module.add_function(
            &format!("kh$drop_fields${}", mangle_type(ty)),
            void.fn_type(&[ptr.into()], false),
            Some(Linkage::Internal),
        );
        // Recorded before the body exists so a recursive type — a list whose
        // tail is a list — reaches this cache instead of declaring itself
        // again forever.
        self.drop_glue.insert(key, Some(f));
        self.pending_glue.push(ty.clone());
        let _ = name;
        f.as_global_value().as_pointer_value()
    }

    /// Gives every queued `drop_fields` routine its body.
    ///
    /// Must run after all function bodies: emitting one repositions the shared
    /// builder, and inkwell's builder carries its insertion point as hidden
    /// state, so doing this mid-body would silently append a caller's next
    /// instruction to the glue routine instead.
    fn emit_pending_drop_glue(&mut self) {
        while let Some(ty) = self.pending_glue.pop() {
            match &ty {
                Type::Adt { name, .. } if name == CLOSURE_GLUE => self.emit_closure_glue(),
                _ => self.emit_drop_glue(&ty),
            }
        }
    }

    /// An ADT's variants with this instantiation's arguments substituted in.
    ///
    /// `Box<String>` has a `String` field; the *declaration* only says `A`.
    /// A type's variants with its arguments substituted in.
    ///
    /// The declared field of a generic type is a parameter, and a parameter is
    /// never boxed — so anything that reads the declaration instead of this
    /// sees `V` where the instantiation has a `String`. Drop glue got that
    /// wrong first and leaked; field access got it wrong second and loaded a
    /// pointer as an integer.
    pub(crate) fn instantiated_variants(&self, ty: &Type) -> Vec<VariantInfo> {
        let Type::Adt { name, args } = ty else { return Vec::new() };
        let declared = self.variants_of(name);
        let generics = self.types.adts.get(name).cloned().unwrap_or_default();
        if args.is_empty() || generics.is_empty() {
            return declared;
        }
        let mapping: HashMap<&str, Type> = generics
            .iter()
            .map(String::as_str)
            .zip(args.iter().cloned())
            .collect();
        declared
            .into_iter()
            .map(|mut v| {
                v.fields = v
                    .fields
                    .iter()
                    .map(|f| khora_types::unify::substitute(f, &mapping))
                    .collect();
                v
            })
            .collect()
    }

    /// Emits one type's `drop_fields`.
    ///
    /// **One routine per type, switching on the tag** — never one per variant.
    /// A drop site knows only the static type of what it is releasing, so a
    /// routine that assumed one variant's fields would read past the end of a
    /// smaller sibling: `Nil` has no tail to load, and the byte after it
    /// belongs to the allocator. The runtime documentation says this outright,
    /// and it is the single most expensive mistake available here.
    fn emit_drop_glue(&mut self, ty: &Type) {
        let f = match self.drop_glue.get(&ty.to_string()) {
            Some(Some(f)) => *f,
            _ => return,
        };
        let object = f.get_nth_param(0).expect("drop_fields takes an object").into_pointer_value();

        let entry = self.ctx.append_basic_block(f, "entry");
        let done = self.ctx.append_basic_block(f, "done");

        let mut cases = Vec::new();
        for (tag, variant) in self.instantiated_variants(ty).into_iter().enumerate() {
            let owned: Vec<(usize, Type)> = variant
                .fields
                .iter()
                .enumerate()
                .filter(|(_, ty)| is_boxed(ty))
                .map(|(i, ty)| (i, ty.clone()))
                .collect();
            // A variant with nothing to release needs no case at all: the
            // switch's default already falls through to the return.
            if owned.is_empty() {
                continue;
            }

            let block = self.ctx.append_basic_block(f, &format!("drop.{}", variant.name));
            cases.push((self.ctx.i32_type().const_int(tag as u64, false), block));
            self.builder.position_at_end(block);

            for (index, field_ty) in owned {
                let slot = runtime::field_pointer(self.ctx, &self.builder, object, index as u64);
                let value = self
                    .builder
                    .build_load(self.ctx.ptr_type(AddressSpace::default()), slot, "child")
                    .expect("loading an owned field");
                let glue = self.drop_glue(&field_ty);
                self.builder
                    .build_call(self.rt.drop, &[value.into(), glue.into()], "")
                    .expect("dropping a field");
            }
            self.builder.build_unconditional_branch(done).expect("branch to the return");
        }

        self.builder.position_at_end(entry);
        let tag = runtime::load_tag(self.ctx, &self.builder, object);
        self.builder.build_switch(tag, done, &cases).expect("switching on a tag");

        self.builder.position_at_end(done);
        self.builder.build_return(None).expect("returning from drop_fields");
    }

    // -----------------------------------------------------------------------
    // Entry point
    // -----------------------------------------------------------------------

    /// Ends the region that lasts as long as the program does.
    ///
    /// On the failing path too: a finalizer that runs only when nothing went
    /// wrong is not a finalizer, and an uncaught raise is exactly when closing
    /// a file matters.
    fn close_root_region(&mut self) {
        let close = self.rt.region_close_root;
        self.builder.build_call(close, &[], "").expect("closing the root region");
    }

    /// Emits a `main` that hands every test to the runner.
    ///
    /// `int main(int argc, char **argv)`, and the call that keeps them.
    ///
    /// The arguments a process was started with arrive exactly once, here, and
    /// are gone. Everything else about a program's environment has a function
    /// to ask — `getenv`, `time` — so this is the one thing the runtime has to
    /// hold on to, and the first thing generated code does is hand it over.
    fn entry_point(&mut self) -> FunctionValue<'ctx> {
        let i32_type = self.ctx.i32_type();
        let ptr = self.ctx.ptr_type(AddressSpace::default());
        self.module.add_function(
            "main",
            i32_type.fn_type(&[i32_type.into(), ptr.into()], false),
            None,
        )
    }

    /// Hands `argc` and `argv` to the runtime before anything else runs.
    fn remember_arguments(&mut self, main: FunctionValue<'ctx>) {
        let (Some(argc), Some(argv)) = (main.get_nth_param(0), main.get_nth_param(1)) else {
            return;
        };
        self.builder
            .build_call(self.rt.args_set, &[argc.into(), argv.into()], "")
            .expect("recording the command line");
    }

    /// Registration is a loop of calls rather than a table, because a table
    /// would need a layout agreed with the runtime and this needs nothing: the
    /// name is a pointer and a length, and the body is a function pointer.
    fn emit_test_main(&mut self, tests: &[(String, String)]) {
        let main = self.entry_point();
        let entry = self.ctx.append_basic_block(main, "entry");
        self.builder.position_at_end(entry);
        self.remember_arguments(main);

        for (symbol, name) in tests {
            let Some(function) = self.functions.get(symbol).copied() else { continue };
            let text = self
                .builder
                .build_global_string_ptr(name, "test.name")
                .expect("a test's name")
                .as_pointer_value();
            let len = self.ctx.i64_type().const_int(name.len() as u64, false);
            let code = function.as_global_value().as_pointer_value();
            // The trampoline, not the test itself: a tagged return does not
            // cross into the runtime. See `tagged_trampoline`.
            let call = self.tagged_trampoline(0).as_global_value().as_pointer_value();
            self.builder
                .build_call(
                    self.rt.test_register,
                    &[text.into(), len.into(), code.into(), call.into()],
                    "",
                )
                .expect("registering a test");
        }

        let status = self
            .builder
            .build_call(self.rt.test_run, &[], "status")
            .expect("running the tests")
            .try_as_basic_value()
            .basic()
            .expect("the runner returns a status")
            .into_int_value();
        // The root region ends here too: a test that deferred something to it
        // is as entitled to have it run as `main` is.
        self.close_root_region();
        self.builder.build_return(Some(&status)).expect("returning from main");
    }

    /// Emits the C `main` the operating system actually starts.
    ///
    /// Khora's `main` is an ordinary Khora function; this is the shim that
    /// gives it the signature a C runtime expects. An `Int` result becomes the
    /// exit code, truncated to `i32` because that is all a process status
    /// carries — a Khora `main` returning 2^32 exits 0, which is the same thing
    /// C does and worth knowing about.
    fn emit_c_main(&mut self, entry: Option<&str>) {
        let Some(entry) = entry else {
            self.error(
                "this program has no `main` function, so there is nothing to run",
                TextRange::empty(0.into()),
            );
            return;
        };
        let Some(signature) = self.signature_of(entry) else {
            self.error(
                "this program has no `main` function, so there is nothing to run",
                TextRange::empty(0.into()),
            );
            return;
        };
        if !self.is_defined(entry) {
            self.error(
                "`main` is declared without a body, so there is nothing to run",
                TextRange::empty(0.into()),
            );
            return;
        }
        if !signature.params.is_empty() {
            self.error(
                "`main` cannot take parameters yet; command-line arguments arrive with the \
                 standard library",
                TextRange::empty(0.into()),
            );
            return;
        }

        let khora_main = self.functions[entry];
        let raises = self.signature_of(entry).is_some_and(|s| can_raise(&s));
        let i32_type = self.ctx.i32_type();
        let main = self.entry_point();
        let entry = self.ctx.append_basic_block(main, "entry");
        self.builder.position_at_end(entry);
        self.remember_arguments(main);

        let call = self.builder.build_call(khora_main, &[], "result").expect("calling main");

        // An entry point that can raise has nowhere to hand the error, so an
        // uncaught raise is a failing exit. This is what makes a program that
        // raises runnable at all before `catch` lands, and it is the behaviour
        // a shell expects either way.
        let result = if raises {
            let tagged = call
                .try_as_basic_value()
                .basic()
                .expect("a fallible main returns a tagged value")
                .into_struct_value();
            let which = self
                .builder
                .build_extract_value(tagged, 0, "which")
                .expect("reading the tag")
                .into_int_value();
            let flag = self
                .builder
                .build_int_compare(
                    inkwell::IntPredicate::NE,
                    which,
                    self.ctx.i32_type().const_zero(),
                    "raised",
                )
                .expect("testing the tag");
            let word = self
                .builder
                .build_extract_value(tagged, 1, "payload")
                .expect("reading the payload")
                .into_int_value();

            let failed = self.ctx.append_basic_block(main, "raised");
            let ok = self.ctx.append_basic_block(main, "ok");
            self.builder.build_conditional_branch(flag, failed, ok).expect("branching on the tag");

            self.builder.position_at_end(failed);
            self.close_root_region();
            // A cancellation that reached the entry point and an error that
            // did are different outcomes, and worth telling apart from
            // outside: 130 is 128 + SIGINT, which is what a shell already
            // means by "interrupted".
            let cancelled_which =
                self.ctx.i32_type().const_int(runtime::CANCELLED_WHICH, false);
            let was_cancelled = self
                .builder
                .build_int_compare(
                    inkwell::IntPredicate::EQ,
                    which,
                    cancelled_which,
                    "cancelled",
                )
                .expect("testing for a cancellation");
            let status = self
                .builder
                .build_select(
                    was_cancelled,
                    i32_type.const_int(runtime::CANCELLED_EXIT, false),
                    i32_type.const_int(1, false),
                    "status",
                )
                .expect("choosing an exit status");
            self.builder.build_return(Some(&status)).expect("exiting on an uncaught raise");

            self.builder.position_at_end(ok);
            Some(word.into())
        } else {
            call.try_as_basic_value().basic()
        };

        let code = match signature.ret {
            Type::Int => {
                let value = result
                    .expect("an `Int` main returns a value")
                    .into_int_value();
                self.builder
                    .build_int_truncate(value, i32_type, "exit")
                    .expect("truncating the exit code")
            }
            Type::Unit => i32_type.const_zero(),
            other => {
                self.error(
                    format!(
                        "`main` returns `{other}`, but an entry point must return `Int` or `()`"
                    ),
                    TextRange::empty(0.into()),
                );
                i32_type.const_zero()
            }
        };
        self.close_root_region();
        self.builder.build_return(Some(&code)).expect("returning from main");
    }

    // -----------------------------------------------------------------------
    // Emission
    // -----------------------------------------------------------------------

    /// Verifies the module, writes an object and links an executable.
    fn finish(self, machine: &TargetMachine, out: &Path) -> Result<(), Vec<HirError>> {
        // Dumped before verification, so that a module which fails to verify is
        // still there to be read — that is precisely when it is wanted.
        if std::env::var_os("KHORA_EMIT_LLVM").is_some() {
            let _ = self.module.print_to_file(with_suffix(out, ".ll"));
        }

        self.module.verify().map_err(|e| {
            vec![backend_error(format!(
                "the generated module is not valid LLVM IR, which is a compiler bug:\n{e}"
            ))]
        })?;

        let object = with_suffix(out, ".o");
        machine
            .write_to_file(&self.module, FileType::Object, &object)
            .map_err(|e| vec![backend_error(format!("writing {}: {e}", object.display()))])?;

        toolchain::link_with_runtime(&[&object], out).map_err(|e| vec![backend_error(e)])
    }
}

/// The symbol a Khora function is emitted under. See the module documentation.
fn mangle(name: &str) -> String {
    format!("kh${name}")
}

/// `out` with a suffix appended to the whole file name.
///
/// Appended rather than substituted for the extension: `app.exe` must become
/// `app.exe.o`, not `app.o`, or a program named `app.o` would overwrite its own
/// object file halfway through being linked from it.
fn with_suffix(out: &Path, suffix: &str) -> PathBuf {
    let mut name = out.as_os_str().to_os_string();
    name.push(suffix);
    PathBuf::from(name)
}

/// A failure with no source position: LLVM, the linker, or a missing entry
/// point.
///
/// The signature of [`compile`] gives one error channel, and it is the one the
/// front end uses, so these are reported against the start of the file. A
/// renderer will show the first line; the message has to carry the detail.
fn backend_error(message: impl Into<String>) -> HirError {
    HirError { message: message.into(), range: TextRange::empty(0.into()) }
}
