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
    TargetTriple,
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
    build(db, root, out, Entry::Main, Stop::AtExecutable)
}

/// Generates and verifies a module, and stops before writing anything.
///
/// For checking that code generation works for a platform this host cannot
/// link for. Which `std` files a build selects is a per-target decision, so a
/// bug can live in a combination of modules that only one platform compiles --
/// `std::fs` and `socket_linux.kh` both declare `close`, and
/// `socket_windows.kh` does not, which hid a symbol collision from everyone
/// working on Windows until CI ran on a Mac.
///
/// Set `KHORA_TARGET` to choose the target. Verification is genuinely the last
/// portable step: an unresolved symbol or a wrong calling convention still
/// needs the real platform, and CI still builds on all three.
pub fn verify_for_target(db: &dyn Db, root: SourceRoot) -> Result<(), Vec<HirError>> {
    build(db, root, Path::new("verify-only"), Entry::Main, Stop::AtVerification)
}

/// Compiles the program's *tests* to an executable that runs them.
///
/// The same program, with a different entry point: instead of calling `main`,
/// it registers every `test` block and hands them to the runner, which gives
/// each one a fiber of its own. Everything else — the same monomorphization,
/// the same lowering — is shared, because a test body is a function body.
pub fn compile_tests(db: &dyn Db, root: SourceRoot, out: &Path) -> Result<(), Vec<HirError>> {
    build(db, root, out, Entry::Tests, Stop::AtExecutable)
}

/// Compiles the program's `bench` blocks to an executable that times them.
///
/// A third entry point rather than a flag on the test one, because a build
/// containing both would register each block with a harness that then has to
/// decide which it is — and the decision already exists, in
/// `khora_hir::TestKind`, at compile time.
pub fn compile_benches(db: &dyn Db, root: SourceRoot, out: &Path) -> Result<(), Vec<HirError>> {
    build(db, root, out, Entry::Benches, Stop::AtExecutable)
}

/// How far to take a build.
///
/// Verification is the last step that is the same on every platform. Writing an
/// object needs a target machine that can encode for the target, and linking
/// needs that target's libraries -- so a host can check another platform's code
/// generation but cannot produce a program from it.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Stop {
    /// Verify, write the object, link the executable.
    AtExecutable,
    /// Verify the module and stop. See [`crate::verify_for_target`].
    AtVerification,
}

/// Which entry point an executable gets.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Entry {
    /// Call `main`, and its result is the exit status.
    Main,
    /// Run every test, and whether they all passed is the exit status.
    Tests,
    /// Time every `bench` block and report the distribution.
    Benches,
}

// One module per backend responsibility. This was 2,306 lines in one file, and
// its banners had the same problem the rest of the crate's did — an empty
// "Drop glue" heading immediately followed by "Closures", with the glue filed
// under the latter. Roadmap 9.6.2.
//
// Rust lets an inherent impl be split across modules of one crate, so each file
// opens `impl<'ctx> Backend<'ctx>` again. The struct, `new`, `error`, `finish`
// and the small predicates other modules ask about stay here.
mod closures;
mod driver;
mod entry;
mod functions;
mod glue;
mod shims;
mod statics;
mod thunks;
mod types;

use driver::build;

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

/// A type as it appears in a generated symbol name.
fn mangle_type(ty: &Type) -> String {
    ty.to_string().replace(['<', '>', '(', ')', ',', ' '], "$").replace("$$", "$")
}

/// The tag an adapter closure carries. Far above any real closure site, so the
/// shared `drop_fields` switch never has a case for it — which is right, since
/// an adapter captures nothing.
pub(crate) const CLOSURE_ADAPTER_TAG: u64 = u32::MAX as u64;

/// A closure's field 0 is its function pointer; captures start after it.
pub(crate) const CLOSURE_CAPTURE_BASE: usize = 1;

/// Everything shared by every function in the module under construction.
pub(crate) struct Backend<'ctx> {
    /// Whether this program can ever have two threads.
    ///
    /// False when nothing reachable mentions `Fiber::spawn`, which is the only
    /// way a Khora program creates a thread. Reference counting is then plain
    /// arithmetic rather than a pair of atomics — worth 7% of an HTTP parse and
    /// 10% of a browser's, and it is D10's escape analysis in the degenerate
    /// case where there is only one fiber to escape from.
    ///
    /// **The generated `main` tells the runtime**, which aborts if a spawn ever
    /// happens anyway. Being wrong here is a data race rather than a crash, and
    /// a data race in a reference count is memory corruption a long way from
    /// its cause, so it is worth one branch on a call that starts a thread to
    /// turn it into a message. `docs/design/reuse.md` §4.
    pub single_threaded: bool,
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
    /// `extern fn` declarations, by the C symbol they name.
    ///
    /// Separate from `types.signatures` because that map is keyed by a bare
    /// function name across the whole program, and two modules may legitimately
    /// use one name for different things. `std::fs` declares a Khora
    /// `close(file: Ptr)`; `socket_linux.kh` declares `extern fn close(handle:
    /// I32)`, which is POSIX's. Merged into one map the first one wins by
    /// accident of file order, and every POSIX build compiled a call to the
    /// wrong one.
    ///
    /// They do not really share a namespace: a Khora function is emitted as
    /// `kh$std$fs$close` and a C symbol as `close`. This map is that
    /// distinction, made where the lookup happens.
    foreign_signatures: HashMap<String, Signature>,
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
    /// One `String` object per distinct literal, shared by every mention.
    static_strings: HashMap<String, PointerValue<'ctx>>,
    /// One object per field-less constructor, shared by every mention.
    static_variants: HashMap<String, PointerValue<'ctx>>,
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
            // Set by `build` once the reachable set is known. Assuming threads
            // until told otherwise is the safe direction.
            single_threaded: false,
            ctx,
            module,
            builder: ctx.create_builder(),
            rt,
            types,
            functions: HashMap::new(),
            defined: HashSet::new(),
            instance_signatures: HashMap::new(),
            foreign_signatures: HashMap::new(),
            drop_glue: HashMap::new(),
            pending_glue: Vec::new(),
            closures: Vec::new(),
            closures_by_owner: HashMap::new(),
            thunks: HashMap::new(),
            pending_thunks: Vec::new(),
            trampolines: HashMap::new(),
            change_shims: HashMap::new(),
            static_strings: HashMap::new(),
            static_variants: HashMap::new(),
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

    /// Verifies the module, and unless `stop` says otherwise writes an object
    /// and links an executable.
    fn finish(
        self,
        machine: &TargetMachine,
        out: &Path,
        stop: Stop,
    ) -> Result<(), Vec<HirError>> {
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

        if stop == Stop::AtVerification {
            return Ok(());
        }

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
