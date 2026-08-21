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

    let entry = mono.instances.iter().find(|(i, _)| i.function == "main").map(|(i, _)| i.symbol());
    backend.emit_c_main(entry.as_deref());
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
        let Type::Fn { params, ret } = types.of(id).clone() else { continue };
        let captured: Vec<(khora_hir::body::LocalId, Type)> =
            captures.iter().map(|l| (*l, types.local(*l).clone())).collect();
        // An unsolved variable here means nothing ever pinned the type down —
        // `let f = fn x => x;` with `f` unused. That is an ambiguity in the
        // program, not a limit of the backend, and saying which it is decides
        // whether the reader looks for a missing annotation or a missing
        // feature.
        let unsolved = params
            .iter()
            .chain(std::iter::once(&*ret))
            .any(|t| matches!(t, Type::Var(_)));
        if backend.declare_closure(symbol, id, params, *ret, captured).is_none() {
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
    pub captures: Vec<(khora_hir::body::LocalId, Type)>,
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
            Type::Bool => Some(self.ctx.bool_type().into()),
            // A closure is a heap object holding its function pointer and its
            // captures, so a value of function type is a pointer to one.
            Type::Str | Type::Adt { .. } | Type::Fn { .. } => {
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
            Type::Tuple(_) | Type::Const(_) | Type::Applied { .. } | Type::Assoc { .. } => None,
            Type::Never | Type::Unknown => None,
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
        let mut params: Vec<BasicMetadataTypeEnum<'ctx>> = Vec::new();
        for param in &signature.params {
            params.push(self.llvm_type(param)?.into());
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
        let ty = self.function_type(&signature).ok_or_else(|| {
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
    pub fn declare_closure(
        &mut self,
        owner: &str,
        expr: khora_hir::body::ExprId,
        params: Vec<Type>,
        ret: Type,
        captures: Vec<(khora_hir::body::LocalId, Type)>,
    ) -> Option<usize> {
        let index = self.closures.len();
        let symbol = format!("{owner}$$lambda{}", self.closures_by_owner.get(owner).map_or(0, Vec::len));

        let ptr = self.ctx.ptr_type(AddressSpace::default());
        let mut llvm_params: Vec<BasicMetadataTypeEnum<'ctx>> = vec![ptr.into()];
        for param in &params {
            llvm_params.push(self.llvm_type(param)?.into());
        }
        let fn_type = match &ret {
            Type::Unit => self.ctx.void_type().fn_type(&llvm_params, false),
            other => self.llvm_type(other)?.fn_type(&llvm_params, false),
        };

        let f = self.module.add_function(&mangle(&symbol), fn_type, Some(Linkage::Internal));
        self.functions.insert(symbol.clone(), f);
        self.defined.insert(symbol.clone());
        self.instance_signatures.insert(
            symbol.clone(),
            Signature {
                generics: Vec::new(),
                bounds: Vec::new(),
                params: params.clone(),
                ret: ret.clone(),
            },
        );

        self.closures.push(ClosureSite {
            owner: owner.to_string(),
            expr,
            symbol,
            ret,
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
        let fn_type = match &signature.ret {
            Type::Unit => self.ctx.void_type().fn_type(&params, false),
            other => self.llvm_type(other)?.fn_type(&params, false),
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
            // named function captures nothing.
            let args: Vec<BasicMetadataValueEnum<'ctx>> = (0..signature.params.len())
                .filter_map(|i| f.get_nth_param(i as u32 + 1))
                .map(|v| v.into())
                .collect();
            let call =
                self.builder.build_call(target, &args, "forward").expect("forwarding a call");

            match signature.ret {
                Type::Unit => {
                    self.builder.build_return(None).expect("returning from an adapter");
                }
                _ => {
                    let value = call.try_as_basic_value().basic();
                    match value {
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
    fn instantiated_variants(&self, ty: &Type) -> Vec<VariantInfo> {
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
        let i32_type = self.ctx.i32_type();
        let main = self.module.add_function("main", i32_type.fn_type(&[], false), None);
        let entry = self.ctx.append_basic_block(main, "entry");
        self.builder.position_at_end(entry);

        let call = self.builder.build_call(khora_main, &[], "result").expect("calling main");
        let code = match signature.ret {
            Type::Int => {
                let value = call
                    .try_as_basic_value()
                    .basic()
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
