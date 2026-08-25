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
//! **Not variables.** A `DILocalVariable` needs a `DIType` for every Khora
//! type, which means describing the heap layout — the header, the tag, the
//! field words — in DWARF. That is a second piece of work of comparable size
//! and it is worth having; it is not worth blocking line tables on, because a
//! backtrace without variables is most of the value and no variables at all is
//! none of it. Every function therefore shares one empty subroutine type.
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
    AsDIScope, DICompileUnit, DIFile, DIFlagsConstants, DISubprogram, DISubroutineType,
    DWARFEmissionKind, DWARFSourceLanguage, DebugInfoBuilder,
};
use inkwell::module::{FlagBehavior, Module};
use inkwell::values::FunctionValue;
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
        Debug { builder, unit, files: HashMap::new(), signature, current: None }
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
