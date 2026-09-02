//! Assembling one LLVM module, and turning it into an executable.
//!
//! This owns everything that is per *module* — the runtime declarations, the
//! variant tag assignment, the function table, the per-type `drop_fields`
//! routines and the C entry point. Per *function* lowering lives in
//! [`crate::lower`].
//!
//! # Symbol names
//!
//! Khora functions are emitted as `kh$<module>$<name>` — `kh$std$fs$close`.
//! Two reasons for the prefix, both load-bearing:
//!
//! - Khora's `main` is not C's `main`. The executable needs a C `main`
//!   returning `i32`, and it calls the Khora one, so the two cannot share a
//!   symbol.
//! - An unprefixed name would collide with the C library the executable links
//!   against — a Khora `fn read` or `fn open` quietly becoming someone else's.
//!
//! `$` is legal in COFF and ELF symbols and is not something a C library
//! exports, which makes the prefix collision-proof from both directions.
//!
//! # Functions declared without a body
//!
//! `docs/errata.md` #5 makes a function's body optional, so `fn print(v: Int);`
//! is a declaration with no definition. Those are treated as **externs** and
//! called by their unmangled C symbol, which is what makes the runtime reachable
//! from Khora source at all. `print` is the exception: an intrinsic dispatched
//! on its argument type, since there is no prelude to declare three
//! differently-typed printers.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use inkwell::builder::Builder;
use inkwell::context::Context;
use inkwell::module::{Linkage, Module};
use inkwell::passes::PassBuilderOptions;
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
use crate::toolchain::Profile;

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
/// Type checking comes first and is absolute: anything `khora_types::diagnostics`
/// reports is returned and nothing is emitted. Every stage below assumes a
/// well-typed program and would otherwise turn a type error into a
/// miscompilation.
///
/// The same errors are how the backend reports what it cannot lower, carrying
/// the source range of the offending expression. Failures in LLVM or the linker
/// have no source position and are reported against the start of the file.
///
/// # What it writes
///
/// The executable at `out`, and the object it was linked from at `out` with
/// `.o` appended. The object is kept, because disassembling it is the first
/// thing anyone does when a generated program misbehaves. `KHORA_EMIT_LLVM`
/// also writes the module as `.ll`, *before* verification, so one that fails to
/// verify can still be read.
///
/// # What it needs on disk
///
/// `clang` under `LLVM_SYS_221_PREFIX`, and `khora-rt`'s static archive, which
/// [`crate::toolchain::runtime_archive`] locates. A missing archive is an error
/// naming the command that produces it, not a link failure full of undefined
/// symbols from Rust's `std`.
pub fn compile(db: &dyn Db, root: SourceRoot, out: &Path) -> Result<(), Vec<HirError>> {
    compile_with(db, root, out, Profile::from_env())
}

/// [`compile`], for a caller that knows which profile it wants.
///
/// The plain [`compile`] reads `KHORA_PROFILE`, which is what a build reached
/// from a test or an editor should do. `khora build --release` has been *told*,
/// and telling is not the same as arranging for a variable to be read later —
/// the linker asks the same question, and it has to get the same answer.
pub fn compile_with(
    db: &dyn Db,
    root: SourceRoot,
    out: &Path,
    profile: Profile,
) -> Result<(), Vec<HirError>> {
    build(db, root, out, Entry::Main, Stop::AtExecutable, profile)
}

/// Compiles the program's `pub extern fn`s into a shared library.
///
/// No `main`, and **never single-threaded**: the host decides which of its
/// threads calls in, so reference counting has to be atomic whether or not
/// this program can spawn a fiber of its own. That falls out of `Entry`
/// rather than being asserted here, and `build` says so where it decides.
///
/// A C header is written beside `out`, from the same signatures the checker
/// validated — generated rather than written, because a header that can drift
/// from its source is a header that will.
pub fn compile_library(db: &dyn Db, root: SourceRoot, out: &Path) -> Result<(), Vec<HirError>> {
    compile_library_with(db, root, out, Profile::from_env())
}

/// [`compile_library`], for a caller that knows which profile it wants.
pub fn compile_library_with(
    db: &dyn Db,
    root: SourceRoot,
    out: &Path,
    profile: Profile,
) -> Result<(), Vec<HirError>> {
    build(db, root, out, Entry::Library, Stop::AtExecutable, profile)
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
    build(db, root, Path::new("verify-only"), Entry::Main, Stop::AtVerification, Profile::from_env())
}

/// Compiles the program's *tests* to an executable that runs them.
///
/// The same program, with a different entry point: instead of calling `main`,
/// it registers every `test` block and hands them to the runner, which gives
/// each one a fiber of its own. Everything else — the same monomorphization,
/// the same lowering — is shared, because a test body is a function body.
pub fn compile_tests(db: &dyn Db, root: SourceRoot, out: &Path) -> Result<(), Vec<HirError>> {
    build(db, root, out, Entry::Tests, Stop::AtExecutable, Profile::from_env())
}

/// Compiles the program's `bench` blocks to an executable that times them.
///
/// A third entry point rather than a flag on the test one, because a build
/// containing both would register each block with a harness that then has to
/// decide which it is — and the decision already exists, in
/// `khora_hir::TestKind`, at compile time.
pub fn compile_benches(db: &dyn Db, root: SourceRoot, out: &Path) -> Result<(), Vec<HirError>> {
    build(db, root, out, Entry::Benches, Stop::AtExecutable, Profile::from_env())
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
    /// No entry point at all: a shared library whose `pub extern fn`s are
    /// the only way in. `docs/design/c-export.md`.
    Library,
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
mod exports;
mod functions;
mod glue;
mod header;
mod shims;
mod statics;
mod thunks;
mod types;

use driver::build;

// `foreign_obstacle` and `foreign_signature_obstacle` moved to `khora-types`
// when the export surface needed them: what may cross the C ABI is a fact
// about types, and the *checker* has to report it now that a function can be
// exported. Reached here through the re-export below.
pub(crate) use khora_types::{can_raise, foreign_signature_obstacle};

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
    /// The bare C names this module publishes, for a linker that has to be
    /// told each one. See [`crate::toolchain`] on why `--export-dynamic` is
    /// the wrong answer on a platform with a size limit.
    pub(crate) c_exports: Vec<String>,
    /// DWARF line tables, or `None` when `KHORA_DEBUG=0`. See [`crate::debug`].
    pub(crate) debug: Option<crate::debug::Debug<'ctx>>,
    /// The text of the file whose body is being emitted.
    ///
    /// **Kept whatever the profile is**, which is the point. `enter_debug_scope`
    /// already reads this text and hands it to the debug builder, and returns
    /// early when there is no debug information to build — so a release build
    /// had the file in hand and threw it away. An `assert` needs one number out
    /// of it, and needs it in both profiles.
    ///
    /// Empty when nothing is being emitted, and between bodies.
    pub(crate) source: String,
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
    ///
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
    /// Keyed by the *return* type as well as the arity, because unlike a
    /// tagged return -- always `{ i32, i64 }` -- a plain one is whatever
    /// the callee answers, and calling an `f64`-returning function through
    /// an `i64`-returning pointer reads the wrong register.
    plain_trampolines: HashMap<(usize, String), FunctionValue<'ctx>>,
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

        let rt = Runtime::declare(ctx, &module, &target_data);
        Backend {
            // Set by `build` once the reachable set is known. Assuming threads
            // until told otherwise is the safe direction.
            single_threaded: false,
            ctx,
            module,
            builder: ctx.create_builder(),
            c_exports: Vec::new(),
            // Installed by `build`, which is where the entry file's path is
            // known. `Backend::new` has a module name and not a path.
            debug: None,
            source: String::new(),
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
            plain_trampolines: HashMap::new(),
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
    // Debug information
    // -----------------------------------------------------------------------

    /// Closes the current function's debug scope, and clears the builder with
    /// it.
    ///
    /// **Both halves, or neither is any use.** `Debug::leave` forgets the
    /// subprogram, but the *builder* keeps the last location it was given, and
    /// the next function's prologue — the `alloca`s `allocate_slots` emits
    /// before a single expression is lowered — inherits it. LLVM's verifier
    /// calls that "!dbg attachment points at wrong subprogram", which is a
    /// failed build rather than a wrong backtrace, and is the right thing for
    /// it to do: a location naming another function's scope is not a slightly
    /// worse answer, it is a corrupt one.
    pub(crate) fn end_debug_scope(&mut self) {
        if self.debug.is_none() {
            return;
        }
        self.builder.unset_current_debug_location();
        if let Some(debug) = self.debug.as_mut() {
            debug.leave();
        }
    }

    /// Points the builder at `range` for the instructions that follow.
    ///
    /// A no-op with debug info off, and a no-op inside the helpers the backend
    /// emits for itself — those have no Khora source, and `Debug::location`
    /// declines rather than inventing one.
    pub(crate) fn at(&mut self, range: TextRange) {
        let Some(debug) = self.debug.as_ref() else { return };
        match debug.location(self.ctx, range) {
            Some(location) => self.builder.set_current_debug_location(location),
            // **Cleared rather than left standing.** A location belonging to a
            // function that is no longer being emitted is attached to whatever
            // instruction comes next, and LLVM's verifier rejects that — which
            // is how a stale scope turns into a failed build rather than a
            // wrong backtrace.
            None => self.builder.unset_current_debug_location(),
        }
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
        library: bool,
        profile: Profile,
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

        // **After verification, before the object.** A debug build runs
        // nothing here and never has: what a target machine does on its own is
        // instruction selection, so the IR reaching it is what was written.
        //
        // The pipeline is named rather than assembled pass by pass. LLVM's
        // `default<O2>` is maintained by people who measure it, changes with
        // every release, and is the thing every other front end runs — a
        // hand-picked list is a promise to keep picking, and the first
        // regression is silent.
        //
        // Verified again afterwards, in release only, because a pass that
        // breaks the module is a compiler bug this should catch rather than
        // hand to the assembler. It costs a walk of the module on a build that
        // has already spent longer optimizing it.
        if let Some(pipeline) = profile.pipeline() {
            let _phase = crate::timings::Phase::start("optimize");
            let options = PassBuilderOptions::create();
            self.module.run_passes(pipeline, machine, options).map_err(|e| {
                vec![backend_error(format!(
                    "the `{pipeline}` pipeline failed, which is a compiler bug:
{e}"
                ))]
            })?;
            self.module.verify().map_err(|e| {
                vec![backend_error(format!(
                    "optimization produced invalid LLVM IR, which is a compiler bug:
{e}"
                ))]
            })?;
            if std::env::var_os("KHORA_EMIT_LLVM").is_some() {
                let _ = self.module.print_to_file(with_suffix(out, ".opt.ll"));
            }
        }

        let object = with_suffix(out, ".o");
        {
            let _phase = crate::timings::Phase::start("object");
            machine
                .write_to_file(&self.module, FileType::Object, &object)
                .map_err(|e| vec![backend_error(format!("writing {}: {e}", object.display()))])?;
        }

        let _phase = crate::timings::Phase::start("link");
        toolchain::link_with_runtime(&[&object], out, library, &self.c_exports, profile)
            .map_err(|e| vec![backend_error(e)])
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
