//! DWARF line tables: what a backtrace needs to name a line of Khora.
//!
//! Before this, `khora_overflow` and `khora_bounds_fail` said what happened and
//! not where. A trap in a program of any size gave a message, an address, and
//! nothing to connect the two — no `lldb`, no stepping, no file and line. For a
//! language whose pitch includes running services, "it overflowed somewhere"
//! is not a diagnosis.
//!
//! # What this emits, and what it does not
//!
//! **Line tables and subprograms.** Every emitted function gets a
//! `DISubprogram` naming its source file and line, and every expression sets
//! the builder's debug location before it lowers. That is enough for a
//! backtrace to read `risk_analyzer.kh:88`, for `lldb` to step, and for a
//! profiler to attribute samples to source.
//!
//! **And locals, by name.** Every slot `allocate_slots` makes gets a
//! `DILocalVariable` and a `dbg.declare`, so `lldb` lists the variables in a
//! frame and prints the scalar ones. See [`Debug::type_of`] for what a Khora
//! type becomes and where that stops short: a boxed value is a *pointer* with
//! the right name and the right address, and not a struct a debugger can walk
//! into. Describing the heap layout — header, tag, field words — is a third
//! piece of work, and the second was worth having without it: a frame that
//! lists `units`, `scale` and `total` with two of them readable is most of the
//! distance from a bare backtrace.
//!
//! Every function still shares one subroutine type. A `DISubroutineType` is
//! the *signature*, which a backtrace does not print, so paying for it would
//! buy nothing a caller cannot already see.
//!
//! # Why the mapping is per file rather than per program
//!
//! A build is whole-program: `std` and the application are one LLVM module, and
//! a specialization of `List::map` belongs to `std/list.kh` however deep in an
//! application it was called from. So the compile unit is the entry module and
//! each source file gets its own `DIFile`, keyed by path. A backtrace that
//! walks out of user code and into `std` says so.
//!
//! # Reproducibility
//!
//! §6.1 wants bit-for-bit reproducible builds, and debug info is where absolute
//! paths get baked into an artifact. The paths here are the ones `SourceRoot`
//! was given — the same ones diagnostics print — so a build is reproducible
//! exactly to the extent that the invocation is, and no more. Making it better
//! than that means a path-remapping flag, which is a real thing to want and is
//! recorded in the roadmap rather than guessed at here.

use std::collections::HashMap;
use std::path::Path;

use inkwell::context::Context;
use inkwell::debug_info::{
    AsDIScope, DICompileUnit, DIFile, DIFlagsConstants, DISubprogram, DISubroutineType, DIType,
    DWARFEmissionKind, DWARFSourceLanguage, DebugInfoBuilder,
};
use inkwell::module::{FlagBehavior, Module};
use inkwell::values::FunctionValue;
use inkwell::values::AsValueRef;
use khora_types::Type;
use text_size::TextRange;

/// The debug-info state for one module under construction.
pub(crate) struct Debug<'ctx> {
    builder: DebugInfoBuilder<'ctx>,
    unit: DICompileUnit<'ctx>,
    /// One `DIFile` per source path, and the line-start offsets that turn a
    /// `TextRange` into a line and column.
    files: HashMap<String, (DIFile<'ctx>, Lines)>,
    /// Shared by every function — see the module docs on why there are no
    /// parameter types yet.
    signature: DISubroutineType<'ctx>,
    /// The function currently being emitted, and the file it came from.
    current: Option<(DISubprogram<'ctx>, String)>,
    /// One `DIType` per Khora type, by how it prints.
    ///
    /// Cached because a `DIType` is metadata and two of them for one type are
    /// two entries in the debug information rather than one, on every local of
    /// that type in the program.
    types: HashMap<String, DIType<'ctx>>,
    /// The pointee every boxed type points at. See [`Debug::type_of`].
    opaque: Option<DIType<'ctx>>,
}

impl<'ctx> Debug<'ctx> {
    /// Starts a compile unit for `module`, named for the entry source file.
    pub(crate) fn new(
        module: &Module<'ctx>,
        ctx: &'ctx Context,
        entry: &Path,
        is_msvc: bool,
    ) -> Debug<'ctx> {
        // Without this flag LLVM drops every `!dbg` on the floor and the
        // verifier says nothing, which is a silent no-op rather than an error.
        module.add_basic_value_flag(
            "Debug Info Version",
            FlagBehavior::Warning,
            ctx.i32_type().const_int(3, false),
        );
        if is_msvc {
            // A Windows target gets CodeView, which is what its debuggers read.
            module.add_basic_value_flag(
                "CodeView",
                FlagBehavior::Warning,
                ctx.i32_type().const_int(1, false),
            );
        } else {
            module.add_basic_value_flag(
                "Dwarf Version",
                FlagBehavior::Warning,
                ctx.i32_type().const_int(4, false),
            );
        }

        let (name, directory) = split(entry);
        let (builder, unit) = module.create_debug_info_builder(
            true,
            // There is no `DW_LANG_Khora`, and inventing one means every
            // existing debugger treats the file as unknown rather than as
            // something it can almost read. C is the honest lie: the line
            // tables are exactly what C's are.
            DWARFSourceLanguage::C,
            &name,
            &directory,
            concat!("khora ", env!("CARGO_PKG_VERSION")),
            false,
            "",
            0,
            "",
            DWARFEmissionKind::Full,
            0,
            false,
            false,
            "",
            "",
        );
        let signature = builder.create_subroutine_type(
            unit.get_file(),
            None,
            &[],
            DIFlagsConstants::PUBLIC,
        );
        Debug {
            builder,
            unit,
            files: HashMap::new(),
            signature,
            current: None,
            types: HashMap::new(),
            opaque: None,
        }
    }

    /// The `DIFile` for `path`, created on first sight.
    fn file(&mut self, path: &str, text: &str) -> DIFile<'ctx> {
        if let Some((file, _)) = self.files.get(path) {
            return *file;
        }
        let (name, directory) = split(Path::new(path));
        let file = self.builder.create_file(&name, &directory);
        self.files.insert(path.to_string(), (file, Lines::of(text)));
        file
    }

    /// Opens a subprogram for `function` and makes it the current scope.
    ///
    /// `name` is the Khora name rather than the mangled symbol: the symbol is
    /// what the linker wants and the name is what a backtrace should read, and
    /// `create_function` takes both.
    pub(crate) fn enter(
        &mut self,
        function: FunctionValue<'ctx>,
        name: &str,
        symbol: &str,
        path: &str,
        text: &str,
        at: TextRange,
    ) {
        let file = self.file(path, text);
        let (line, _) = self.files[path].1.locate(at);
        let subprogram = self.builder.create_function(
            self.unit.as_debug_info_scope(),
            name,
            Some(symbol),
            file,
            line,
            self.signature,
            false,
            true,
            line,
            DIFlagsConstants::PUBLIC,
            false,
        );
        function.set_subprogram(subprogram);
        self.current = Some((subprogram, path.to_string()));
    }

    /// Closes the current subprogram.
    ///
    /// Emitting an instruction with a location belonging to a *different*
    /// function's scope is one of the few things LLVM's verifier rejects
    /// outright, so the scope is cleared rather than left standing.
    pub(crate) fn leave(&mut self) {
        self.current = None;
    }

    /// The location for one source range, in the current function's scope.
    ///
    /// `None` before any function has been entered, which is every helper the
    /// backend emits for itself — drop glue, thunks, the C entry point. Those
    /// have no Khora source to point at, and inventing a line for them would
    /// put a backtrace inside a file at a line that says something else.
    pub(crate) fn location(
        &self,
        ctx: &'ctx Context,
        at: TextRange,
    ) -> Option<inkwell::debug_info::DILocation<'ctx>> {
        let (subprogram, path) = self.current.as_ref()?;
        let (line, column) = self.files.get(path)?.1.locate(at);
        Some(self.builder.create_debug_location(
            ctx,
            line,
            column,
            subprogram.as_debug_info_scope(),
            None,
        ))
    }

    /// What a Khora type looks like to a debugger.
    ///
    /// **Scalars are described exactly; everything else is a pointer.** An
    /// `Int` is a 64-bit signed integer and prints as one, a `Bool` prints as
    /// `true`, a `Float` as a number. A `String`, a record, a closure — every
    /// counted heap value — becomes a pointer with the Khora type's name on
    /// it, so a frame shows `answer: Result<Int, DbError> = 0x7f...` rather
    /// than omitting `answer`.
    ///
    /// The honest limit: a debugger cannot follow that pointer into the object.
    /// Doing so means describing `KhoraHeader` and every ADT's field layout as
    /// DWARF structs — real, and a larger job than this one, and worth
    /// separating because the name and the address are most of what a frame is
    /// read for.
    fn type_of(&mut self, ty: &Type) -> DIType<'ctx> {
        let key = ty.to_string();
        if let Some(found) = self.types.get(&key) {
            return *found;
        }
        // DWARF's `DW_ATE_*` encodings. Spelled as numbers because that is what
        // the C API takes and naming them here would be a second vocabulary.
        const SIGNED: u32 = 0x05;
        const UNSIGNED: u32 = 0x07;
        const FLOAT: u32 = 0x04;
        const BOOLEAN: u32 = 0x02;
        const UTF: u32 = 0x10;

        let made = match ty {
            Type::Int => self.basic("Int", 64, SIGNED),
            Type::Bool => self.basic("Bool", 8, BOOLEAN),
            Type::Char => self.basic("Char", 32, UTF),
            Type::Float => self.basic("Float", 64, FLOAT),
            Type::Fixed(kind) => {
                let bits = u64::from(kind.bits);
                let encoding = if kind.signed { SIGNED } else { UNSIGNED };
                self.basic(&kind.name(), bits, encoding)
            }
            // `Ptr` is an address the other side owns, and `()` is not a value
            // — both are a word as far as a frame is concerned.
            Type::Ptr | Type::Unit => self.basic(&key, 64, UNSIGNED),
            _ => {
                let pointee = self.opaque_object();
                self.builder
                    .create_pointer_type(&key, pointee, 64, 64, inkwell::AddressSpace::default())
                    .as_type()
            }
        };
        self.types.insert(key, made);
        made
    }

    fn basic(&mut self, name: &str, bits: u64, encoding: u32) -> DIType<'ctx> {
        self.builder
            .create_basic_type(name, bits, encoding, DIFlagsConstants::PUBLIC)
            .expect("a named basic type")
            .as_type()
    }

    /// What every boxed value points at.
    ///
    /// One byte, unnamed as a layout: a debugger asked to dereference gets a
    /// byte rather than a lie about the object's shape. When `KhoraHeader` and
    /// the ADT layouts are described this is what they replace.
    fn opaque_object(&mut self) -> DIType<'ctx> {
        if let Some(found) = self.opaque {
            return found;
        }
        let made = self.basic("khora_object", 8, 0x07);
        self.opaque = Some(made);
        made
    }

    /// Names one local, so a debugger can list it in the frame.
    ///
    /// `slot` is the `alloca` the local lives in, which is what makes this
    /// `dbg.declare` rather than `dbg.value`: the address is stable for the
    /// whole frame and does not have to be tracked through optimization.
    pub(crate) fn declare_local(
        &mut self,
        name: &str,
        slot: inkwell::values::PointerValue<'ctx>,
        ty: &Type,
        at: TextRange,
        ctx: &'ctx Context,
        block: inkwell::basic_block::BasicBlock<'ctx>,
    ) {
        let Some((subprogram, path)) = self.current.clone() else { return };
        let Some((file, lines)) = self.files.get(&path) else { return };
        let (file, line) = (*file, lines.locate(at).0);
        let di_ty = self.type_of(ty);
        let variable = self.builder.create_auto_variable(
            subprogram.as_debug_info_scope(),
            name,
            file,
            line,
            di_ty,
            // Kept even when nothing reads it: a variable the program never
            // uses is exactly the one somebody is stopped in the debugger
            // asking about.
            true,
            DIFlagsConstants::ZERO,
            0,
        );
        let Some(location) = self.location(ctx, at) else { return };
        let expression = self.builder.create_expression(Vec::new());

        // **Called through `llvm_sys` rather than through inkwell**, and not by
        // preference. Since LLVM 19 a `dbg.declare` is a *debug record* and not
        // an instruction; inkwell 0.10 aliases the C entry point to the record
        // one — correctly — and then wraps its return in an `InstructionValue`,
        // whose constructor asserts `value.is_instruction()`. Every test that
        // compiles a function with a local died on that assertion inside the
        // crate. The record itself is created correctly; only the wrapper is
        // wrong, so the fix is to not use the wrapper.
        //
        // SAFETY: every pointer comes from a live inkwell value whose lifetime
        // is `'ctx`, which outlives this call, and the arguments are in the
        // order `LLVMDIBuilderInsertDeclareRecordAtEnd` documents. The returned
        // record is owned by the module and is not ours to free.
        unsafe {
            inkwell::llvm_sys::debuginfo::LLVMDIBuilderInsertDeclareRecordAtEnd(
                self.builder.as_mut_ptr(),
                slot.as_value_ref(),
                variable.as_mut_ptr(),
                expression.as_mut_ptr(),
                location.as_mut_ptr(),
                block.as_mut_ptr(),
            );
        }
    }

    /// Resolves the metadata. **Before verification**, which reads it.
    pub(crate) fn finalize(&self) {
        self.builder.finalize();
    }
}

/// Where each line of one source file starts, for turning an offset into a
/// line and column.
///
/// Built once per file rather than scanned per lookup: a body of any size asks
/// this for every expression it lowers, and counting newlines from the top each
/// time is the difference between a debug build and a slow one.
struct Lines {
    /// Byte offset of the first character of each line, ascending.
    starts: Vec<usize>,
    length: usize,
}

impl Lines {
    fn of(text: &str) -> Lines {
        let mut starts = vec![0];
        starts.extend(text.match_indices('\n').map(|(i, _)| i + 1));
        Lines { starts, length: text.len() }
    }

    /// One-based line and column for the start of `range`.
    ///
    /// Columns count bytes rather than characters. DWARF's column is a
    /// debugger's cursor position, and every consumer of it counts the same way
    /// the compiler's own diagnostics would need to — but diagnostics count
    /// characters, so this is deliberately the other one, and it matters only
    /// for a line with non-ASCII before the cursor.
    fn locate(&self, range: TextRange) -> (u32, u32) {
        let offset = usize::from(range.start()).min(self.length);
        let line = self.starts.partition_point(|start| *start <= offset).max(1);
        let column = offset - self.starts[line - 1] + 1;
        (line as u32, column as u32)
    }
}

/// A path split the way `DIFile` wants it: file name, containing directory.
fn split(path: &Path) -> (String, String) {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string_lossy().into_owned());
    let directory = path
        .parent()
        .map(|p| p.to_string_lossy().into_owned())
        .filter(|p| !p.is_empty())
        .unwrap_or_else(|| ".".to_string());
    (name, directory)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(offset: u32) -> TextRange {
        TextRange::empty(offset.into())
    }

    #[test]
    fn the_first_character_is_line_one_column_one() {
        let lines = Lines::of("abc\ndef\n");
        assert_eq!(lines.locate(at(0)), (1, 1));
    }

    #[test]
    fn a_newline_starts_the_next_line() {
        let lines = Lines::of("abc\ndef\nghi");
        assert_eq!(lines.locate(at(3)), (1, 4), "the newline itself ends line one");
        assert_eq!(lines.locate(at(4)), (2, 1));
        assert_eq!(lines.locate(at(8)), (3, 1));
        assert_eq!(lines.locate(at(10)), (3, 3));
    }

    /// An offset past the end is clamped rather than panicking. Nothing should
    /// produce one, and a debug build that aborts because a range was wrong is
    /// a worse failure than a location that is off.
    #[test]
    fn an_offset_past_the_end_lands_on_the_last_line() {
        let lines = Lines::of("abc\ndef");
        let (line, _) = lines.locate(at(999));
        assert_eq!(line, 2);
    }

    #[test]
    fn an_empty_file_is_still_line_one() {
        assert_eq!(Lines::of("").locate(at(0)), (1, 1));
    }

    #[test]
    fn a_path_splits_into_name_and_directory() {
        let (name, dir) = split(Path::new("/home/x/std/list.kh"));
        assert_eq!(name, "list.kh");
        assert_eq!(dir, "/home/x/std");
        // A bare file name still gets a directory, because DWARF wants one.
        let (name, dir) = split(Path::new("a.kh"));
        assert_eq!(name, "a.kh");
        assert_eq!(dir, ".");
    }
}
