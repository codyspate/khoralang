//! The `khora` toolchain driver.
//!
//! Only the front-end commands are wired up so far: everything past parsing
//! reports honestly that it is not implemented rather than pretending.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use khora_db::{KhoraDatabase, SourceFile, SourceRoot};
use khora_diagnostics::{
    render_hir_errors, render_hir_errors_as, render_parse_errors, Severity,
};
use khora_manifest::LintLevel;

#[derive(Parser)]
#[command(name = "khora", version, about = "The Khora language toolchain")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Parse and type check a file, reporting diagnostics.
    Check {
        /// One or more `.kh` files, or directories to walk.
        paths: Vec<PathBuf>,
    },
    /// Print the token stream.
    Lex { path: PathBuf },
    /// Print the concrete syntax tree.
    Parse {
        path: PathBuf,
        /// Hide whitespace and comment tokens.
        #[arg(long)]
        no_trivia: bool,
    },
    /// Rewrite files in canonical form.
    Fmt {
        /// One or more `.kh` files, or directories to walk.
        paths: Vec<PathBuf>,
        /// Report which files would change instead of writing them.
        #[arg(long)]
        check: bool,
    },
    /// Manage the Khora versions installed on this machine.
    Toolchain {
        #[command(subcommand)]
        command: ToolchainCommand,
    },
    /// Speak the Model Context Protocol on stdin and stdout.
    ///
    /// For an AI coding agent: no model has training data for Khora, so this
    /// lets one ask the compiler instead of guessing. Started by the agent's
    /// client, not by a person.
    Mcp,
    /// Speak the Language Server Protocol on stdin and stdout.
    ///
    /// Not for a person to run: an editor starts it. Running it by hand gets a
    /// process waiting for a `Content-Length` header, which is why it says so.
    Lsp,
    /// Compile and run the program's tests, one fiber each.
    Test {
        /// A `.kh` file, or a directory to walk.
        #[arg(default_value = ".")]
        path: PathBuf,
        /// Run only the tests whose name contains this.
        #[arg(short, long)]
        filter: Option<String>,
    },
    /// Compile and time the program's `bench` blocks.
    Bench {
        /// A `.kh` file, or a directory to walk.
        #[arg(default_value = ".")]
        path: PathBuf,
        /// Run only the benchmarks whose name contains this.
        #[arg(short, long)]
        filter: Option<String>,
    },
    /// Compile to a native executable.
    Build {
        /// A `.kh` file, or a directory containing one.
        #[arg(default_value = ".")]
        path: PathBuf,
        /// Where to write the executable. Defaults to the source file's stem.
        #[arg(short, long)]
        out: Option<PathBuf>,
    },
}

fn main() -> ExitCode {
    // Before anything else, including argument parsing: a project pinning a
    // version whose flags this build does not recognise must still work.
    hand_over_if_pinned();

    match run() {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::FAILURE,
        Err(err) => {
            eprintln!("khora: {err:#}");
            ExitCode::FAILURE
        }
    }
}

/// `khora toolchain ...`.
#[derive(Subcommand)]
enum ToolchainCommand {
    /// Show what is installed, and which one is running.
    List,
    /// Register a Khora executable as the toolchain for a version.
    ///
    /// There is no `install`, because there is nothing to download from yet.
    Link {
        /// The version it will be known as.
        version: String,
        /// The executable to register. It is copied, not pointed at.
        path: PathBuf,
    },
    /// Forget a registered toolchain.
    Unlink { version: String },
    /// Say which toolchain this directory would use, and why.
    Which {
        #[arg(default_value = ".")]
        path: PathBuf,
    },
}

fn run() -> Result<bool> {
    let cli = Cli::parse();
    match cli.command {
        Command::Check { paths } => check(&paths),
        Command::Fmt { paths, check } => fmt(&paths, check),
        Command::Lex { path } => lex(&path).map(|()| true),
        Command::Parse { path, no_trivia } => parse_cmd(&path, no_trivia).map(|()| true),
        Command::Build { path, out } => build(&path, out.as_deref()),
        Command::Lsp => lsp().map(|()| true),
        Command::Mcp => mcp().map(|()| true),
        Command::Toolchain { command } => toolchain(command),
        Command::Test { path, filter } => test(&path, filter.as_deref()),
        Command::Bench { path, filter } => bench(&path, filter.as_deref()),
    }
}

fn check(paths: &[PathBuf]) -> Result<bool> {
    let files = collect_sources(paths)?;
    if files.is_empty() {
        anyhow::bail!("no `.kh` files found");
    }

    // Everything goes through the query database, including one-shot CLI runs.
    // A second code path that parsed files directly would drift from the one
    // the language server uses, and the drift would be invisible until it bit.
    let db = KhoraDatabase::new();
    let mut inputs = Vec::with_capacity(files.len());
    for path in &files {
        let text = read(path)?;
        inputs.push((path, SourceFile::new(&db, path.clone(), text)));
    }
    SourceRoot::new(&db, inputs.iter().map(|(_, f)| *f).collect());

    // One project's policy about how loud each lint is, read once. A file
    // outside any package gets the defaults, which is right: `khora check
    // scratch.kh` should work without a manifest.
    let levels = lint_levels(paths.first().map(PathBuf::as_path));

    let mut total = 0usize;
    let mut warnings = 0usize;
    for (path, input) in &inputs {
        let parse = khora_db::parse(&db, *input);
        let text = input.text(&db);
        debug_assert_eq!(parse.syntax().text().to_string(), text);

        // A file that did not parse has no meaningful tree to check, and
        // type errors invented on top of a syntax error are noise.
        if !parse.errors().is_empty() {
            total += parse.errors().len();
            eprintln!("{}", render_parse_errors(path, text, parse.errors()));
            eprintln!();
            continue;
        }

        let semantic = khora_types::diagnostics(&db, *input);
        if !semantic.is_empty() {
            total += semantic.len();
            eprintln!("{}", render_hir_errors(path, text, semantic));
            eprintln!();
            // Lints on a file with type errors are noise: the reader has real
            // problems to fix, and half of what a lint sees downstream of one
            // is an artefact of it.
            continue;
        }

        for finding in khora_lint::findings(&db, *input) {
            let level = levels.get(finding.lint).copied().unwrap_or(LintLevel::Warn);
            if level == LintLevel::Allow {
                continue;
            }
            let error = khora_hir::HirError {
                message: format!("{} [{}]", finding.message, finding.lint),
                range: finding.range,
            };
            // A `warn` lint that prints `error:` and then exits zero teaches
            // people that the word means nothing.
            let severity = match level {
                LintLevel::Deny => Severity::Error,
                _ => Severity::Warning,
            };
            eprintln!(
                "{}",
                render_hir_errors_as(path, text, std::slice::from_ref(&error), severity)
            );
            eprintln!();
            match level {
                LintLevel::Deny => total += 1,
                _ => warnings += 1,
            }
        }
    }

    if total == 0 && warnings == 0 {
        println!("checked {} file(s): no errors", files.len());
    } else if total == 0 {
        println!("checked {} file(s): no errors, {warnings} warning(s)", files.len());
    } else {
        eprintln!("{total} error(s) across {} file(s)", files.len());
    }
    Ok(total == 0)
}

/// How loud each lint is, from the `[lints]` table nearest `start`.
///
/// A lint the manifest does not mention warns. That is the useful default for
/// this set — both are quiet enough to be worth hearing about and neither is
/// worth failing a build over — and `[lints]` is where a project disagrees.
///
/// A manifest that cannot be read contributes nothing rather than failing the
/// command, which is the same rule `dependencies_of` follows and for the same
/// reason: `khora check` on the manifest is the thing whose job it is to
/// complain about the manifest.
fn lint_levels(start: Option<&Path>) -> std::collections::BTreeMap<String, LintLevel> {
    let mut out = std::collections::BTreeMap::new();
    let Some(manifest_path) = start.and_then(nearest_manifest) else { return out };
    let Ok(text) = std::fs::read_to_string(&manifest_path) else { return out };
    let Ok(parsed) = khora_manifest::Manifest::parse(&text) else { return out };

    for (name, lint) in &parsed.manifest.lints {
        out.insert(name.clone(), lint.level);
    }
    out
}

/// Runs the MCP server over stdin and stdout.
///
/// Newline-delimited JSON, unlike `lsp`, which frames with `Content-Length`.
/// Anything this needs to say goes to stderr, because stdout is the protocol.
fn mcp() -> Result<()> {
    if std::io::IsTerminal::is_terminal(&std::io::stdin()) {
        eprintln!(
            "khora mcp speaks the Model Context Protocol on stdin and stdout, so it is 
             waiting for a JSON message rather than for you. Point an agent at it instead."
        );
    }
    let stdin = std::io::stdin();
    let mut input = stdin.lock();
    let stdout = std::io::stdout();
    let mut output = stdout.lock();
    khora_mcp::serve(&mut input, &mut output)
}

/// `khora toolchain ...`.
fn toolchain(command: ToolchainCommand) -> Result<bool> {
    match command {
        ToolchainCommand::List => {
            let installed = khora_toolchain::installed()?;
            if installed.is_empty() {
                println!(
                    "no toolchains registered; this is Khora {} and it is the only one \
                     that will run.\n\n    khora toolchain link {} <path-to-khora>",
                    khora_toolchain::RUNNING,
                    khora_toolchain::RUNNING
                );
                return Ok(true);
            }
            for entry in installed {
                let running =
                    if entry.version == khora_toolchain::RUNNING { "  (running)" } else { "" };
                println!("{}{running}", entry.version);
            }
            Ok(true)
        }
        ToolchainCommand::Link { version, path } => {
            let at = khora_toolchain::link(&version, &path)?;
            println!("registered Khora {version} at {}", at.display());
            Ok(true)
        }
        ToolchainCommand::Unlink { version } => {
            khora_toolchain::unlink(&version)?;
            println!("forgot Khora {version}");
            Ok(true)
        }
        ToolchainCommand::Which { path } => {
            match khora_toolchain::pinned_version(&path) {
                None => println!(
                    "no pin here, so whatever is on the path runs. This is Khora {}.",
                    khora_toolchain::RUNNING
                ),
                Some(wanted) => {
                    let installed = khora_toolchain::installed()?;
                    match khora_toolchain::decide(
                        Some(&wanted),
                        khora_toolchain::RUNNING,
                        None,
                        &installed,
                    ) {
                        khora_toolchain::Decision::Proceed => {
                            println!("pinned to {wanted}, which is what is running")
                        }
                        khora_toolchain::Decision::Handover(t) => println!(
                            "pinned to {wanted}, at {}\nthis is {}, which would hand over",
                            t.binary.display(),
                            khora_toolchain::RUNNING
                        ),
                        khora_toolchain::Decision::Missing { wanted, available } => {
                            println!("{}", khora_toolchain::missing_message(&wanted, &available))
                        }
                    }
                }
            }
            Ok(true)
        }
    }
}

/// Hands this invocation to the toolchain the project pins, if that is not us.
///
/// **Before `clap` sees anything**, so that a project pinning a version with
/// subcommands or flags this build has never heard of still works — which is
/// the whole point of a pin. Parsing first would reject the arguments before
/// the toolchain that understands them ever ran.
///
/// Returns only when this process should carry on. On Unix the handover
/// replaces the process; on Windows there is no `exec`, so it runs the child
/// and exits with its status. The difference is visible if you look at a
/// process tree and nowhere else.
fn hand_over_if_pinned() {
    // **`khora toolchain ...` never hands over.** It is about the machine
    // rather than the project, and handing it over makes the one situation it
    // exists for unrecoverable: standing inside a project whose pinned version
    // is missing, unable to run the command that installs it because the pin
    // refuses to let anything run. `which` has the same problem in the other
    // direction -- it would report on the toolchain that answered rather than
    // on the decision being asked about.
    if std::env::args().nth(1).as_deref() == Some("toolchain") {
        return;
    }

    // A handover already happened. Whatever we are, we are what was asked for
    // -- and re-deciding here is how a mislinked toolchain becomes an infinite
    // chain of `exec`s that presents as a hang.
    let active = std::env::var(khora_toolchain::ACTIVE).ok();
    let here = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let pin = khora_toolchain::pinned_version(&here);

    let installed = khora_toolchain::installed().unwrap_or_default();
    let decision = khora_toolchain::decide(
        pin.as_deref(),
        khora_toolchain::RUNNING,
        active.as_deref(),
        &installed,
    );

    match decision {
        khora_toolchain::Decision::Proceed => {}
        khora_toolchain::Decision::Missing { wanted, available } => {
            eprintln!("khora: {}", khora_toolchain::missing_message(&wanted, &available));
            std::process::exit(1);
        }
        khora_toolchain::Decision::Handover(target) => {
            let args: Vec<String> = std::env::args().skip(1).collect();
            let mut command = std::process::Command::new(&target.binary);
            command.args(&args).env(khora_toolchain::ACTIVE, &target.version);

            #[cfg(unix)]
            {
                use std::os::unix::process::CommandExt;
                // Only returns on failure.
                let failure = command.exec();
                eprintln!("khora: running {}: {failure}", target.binary.display());
                std::process::exit(1);
            }
            #[cfg(not(unix))]
            {
                match command.status() {
                    Ok(status) => std::process::exit(status.code().unwrap_or(1)),
                    Err(e) => {
                        eprintln!("khora: running {}: {e}", target.binary.display());
                        std::process::exit(1);
                    }
                }
            }
        }
    }
}

/// Runs the language server over stdin and stdout.
///
/// Diagnostics go nowhere near stdout, which carries the protocol; anything
/// this needs to say goes to stderr, where an editor shows it in a log.
fn lsp() -> Result<()> {
    if std::io::IsTerminal::is_terminal(&std::io::stdin()) {
        eprintln!(
            "khora lsp speaks the Language Server Protocol on stdin and stdout, so it is 
             waiting for a `Content-Length` header rather than for you. Point an editor at 
             it instead."
        );
    }
    let stdin = std::io::stdin();
    let mut input = stdin.lock();
    let stdout = std::io::stdout();
    let mut output = stdout.lock();
    khora_lsp::serve(&mut input, &mut output)
}

/// Formats files in place, or reports which would change.
fn fmt(paths: &[PathBuf], check: bool) -> Result<bool> {
    let files = collect_sources(paths)?;
    if files.is_empty() {
        anyhow::bail!("no `.kh` files found");
    }

    let mut changed = Vec::new();
    let mut failed = 0usize;
    for path in &files {
        let src = read(path)?;
        match khora_fmt::format(&src) {
            Ok(out) if out == src => {}
            Ok(out) => {
                changed.push(path.clone());
                if !check {
                    std::fs::write(path, out)
                        .with_context(|| format!("writing {}", path.display()))?;
                }
            }
            Err(errors) => {
                // A file that does not parse is left exactly as it is.
                failed += 1;
                eprintln!("{}\n", render_parse_errors(path, &src, &errors));
            }
        }
    }

    if failed > 0 {
        eprintln!("{failed} file(s) could not be parsed and were left unchanged");
    }
    if check {
        for path in &changed {
            println!("would reformat {}", path.display());
        }
        if changed.is_empty() && failed == 0 {
            println!("checked {} file(s): all formatted", files.len());
        }
        return Ok(changed.is_empty() && failed == 0);
    }

    println!("formatted {} of {} file(s)", changed.len(), files.len());
    Ok(failed == 0)
}

/// Builds the program's tests and runs them.
///
/// The executable is written beside the sources rather than into a temporary,
/// so that a failing test can be run again under a debugger without rebuilding
/// — which is the first thing anyone wants and would otherwise need a flag.
#[cfg(feature = "llvm")]
fn test(path: &Path, filter: Option<&str>) -> Result<bool> {
    harness(path, filter, "khora-tests", khora_codegen_llvm::compile_tests)
}

/// Compiles the `bench` blocks and times them.
#[cfg(feature = "llvm")]
fn bench(path: &Path, filter: Option<&str>) -> Result<bool> {
    harness(path, filter, "khora-benches", khora_codegen_llvm::compile_benches)
}

/// The signature `compile_tests` and `compile_benches` share.
///
/// Named because `harness` takes one of them and clippy is right that the
/// inline form is unreadable.
#[cfg(feature = "llvm")]
type CompileHarness =
    fn(&dyn khora_db::Db, SourceRoot, &Path) -> Result<(), Vec<khora_hir::HirError>>;

/// Builds a harness executable and runs it.
///
/// **The filter is passed on the command line rather than through the
/// environment**, so the executable this leaves behind behaves the same when
/// somebody runs it directly. A test binary that only obeys its filter when a
/// build tool sets a variable is one nobody can debug by hand.
#[cfg(feature = "llvm")]
fn harness(
    path: &Path,
    filter: Option<&str>,
    name: &str,
    compile: CompileHarness,
) -> Result<bool> {
    let (db, inputs, root) = load(path)?;
    let target = inputs
        .first()
        .expect("at least one source")
        .0
        .with_file_name(name)
        .with_extension(std::env::consts::EXE_EXTENSION);

    if let Err(errors) = compile(&db as &dyn khora_db::Db, root, &target) {
        report_build_errors(&inputs, &errors);
        return Ok(false);
    }

    let mut command = std::process::Command::new(&target);
    if let Some(want) = filter {
        command.args(["--filter", want]);
    }
    let status = command
        .status()
        .with_context(|| format!("running {}", target.display()))?;
    Ok(status.success())
}

#[cfg(not(feature = "llvm"))]
fn test(_path: &Path, _filter: Option<&str>) -> Result<bool> {
    anyhow::bail!(
        "this `khora` was built without the LLVM backend. \
         Rebuild with `--features llvm`; see docs/llvm-setup.md."
    )
}

#[cfg(not(feature = "llvm"))]
fn bench(_path: &Path, _filter: Option<&str>) -> Result<bool> {
    anyhow::bail!(
        "this `khora` was built without the LLVM backend. \
         Rebuild with `--features llvm`; see docs/llvm-setup.md."
    )
}

/// Compiles a single file to a native executable.
///
/// Semantic errors are reported through the same renderer `check` uses, so a
/// diagnostic reads identically whichever command surfaced it.
#[cfg(feature = "llvm")]
fn build(path: &Path, out: Option<&Path>) -> Result<bool> {
    let (db, inputs, root) = load(path)?;

    // The binary is named after the module holding `main`, or after the one
    // file when there is only one.
    let entry = inputs
        .iter()
        .find(|(_, text, _)| text.contains("fn main("))
        .or_else(|| inputs.first())
        .expect("at least one source");
    let target = out.map(Path::to_path_buf).unwrap_or_else(|| {
        let stem = entry.0.file_stem().unwrap_or_default();
        entry.0.with_file_name(stem).with_extension(std::env::consts::EXE_EXTENSION)
    });

    match khora_codegen_llvm::compile(&db, root, &target) {
        Ok(()) => {
            println!("built {} from {} module(s)", target.display(), inputs.len());
            Ok(true)
        }
        Err(errors) => {
            report_build_errors(&inputs, &errors);
            Ok(false)
        }
    }
}

/// Every source under `path`, parsed, in one compilation.
///
/// One compilation because monomorphization substitutes into a generic
/// function's *body*, so every module's source has to be present at once — the
/// same reason a C++ template lives in a header.
///
/// Returns `Ok(None)`-shaped failure by way of a parse-error report: a program
/// that does not parse has nothing worth compiling, and the errors are already
/// on stderr by then.
#[cfg(feature = "llvm")]
type Loaded = (KhoraDatabase, Vec<(PathBuf, String, SourceFile)>, SourceRoot);

#[cfg(feature = "llvm")]
fn load(path: &Path) -> Result<Loaded> {
    let files = collect_sources(std::slice::from_ref(&path.to_path_buf()))?;
    if files.is_empty() {
        anyhow::bail!("no `.kh` files found");
    }

    let db = KhoraDatabase::new();
    let mut inputs = Vec::with_capacity(files.len());
    for path in &files {
        let text = read(path)?;
        inputs.push((path.clone(), text.clone(), SourceFile::new(&db, path.clone(), text)));
    }
    let root = SourceRoot::new(&db, inputs.iter().map(|(_, _, f)| *f).collect());

    let mut clean = true;
    for (path, text, input) in &inputs {
        let parse = khora_db::parse(&db, *input);
        if !parse.errors().is_empty() {
            clean = false;
            eprintln!("{}", render_parse_errors(path, text, parse.errors()));
            eprintln!();
        }
    }
    if !clean {
        anyhow::bail!("{} did not parse", path.display());
    }
    Ok((db, inputs, root))
}

/// Reports what the backend refused.
///
/// Errors can come from any module, and a span is only meaningful against the
/// file it came from. Without a file on the error there is no honest way to
/// place it, so the first source is used and the count is printed either way.
#[cfg(feature = "llvm")]
fn report_build_errors(
    inputs: &[(PathBuf, String, SourceFile)],
    errors: &[khora_hir::HirError],
) {
    let (path, text, _) = &inputs[0];
    eprintln!("{}", render_hir_errors(path, text, errors));
    eprintln!();
    eprintln!("{} error(s)", errors.len());
}

#[cfg(not(feature = "llvm"))]
fn build(_path: &Path, _out: Option<&Path>) -> Result<bool> {
    anyhow::bail!(
        "this `khora` was built without the LLVM backend. \
         Rebuild with `--features llvm`; see docs/llvm-setup.md."
    )
}

fn lex(path: &Path) -> Result<()> {
    let text = read(path)?;
    let lexed = khora_syntax::LexedStr::new(&text);
    for (kind, tok) in lexed.iter() {
        println!("{kind:?} {tok:?}");
    }
    Ok(())
}

fn parse_cmd(path: &Path, no_trivia: bool) -> Result<()> {
    let text = read(path)?;
    let parse = khora_syntax::parse(&text);
    let tree = parse.debug_tree();
    if no_trivia {
        for line in tree.lines() {
            let trimmed = line.trim_start();
            if trimmed.starts_with("WHITESPACE@")
                || trimmed.starts_with("LINE_COMMENT@")
                || trimmed.starts_with("BLOCK_COMMENT@")
            {
                continue;
            }
            println!("{line}");
        }
    } else {
        print!("{tree}");
    }
    Ok(())
}

fn read(path: &Path) -> Result<String> {
    std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))
}

/// Every source file that makes up the program rooted at `paths`.
///
/// **The command line names an entry point; the manifest names everything
/// else.** `khora build ./app` is the whole of what a developer should have to
/// say — which packages it is built against is a property of the package, not
/// of the invocation, and repeating it at every call is how the two come to
/// disagree.
///
/// Three sources, in this order:
///
/// 1. What was named. A directory is walked; a file is taken as written.
/// 2. Each `path` entry in the nearest `khora.toml`'s `[dependencies]`,
///    resolved relative to that manifest. A `version` entry needs a registry,
///    which is phase 10.
/// 3. The standard library, always and without being asked — see
///    [`khora_db::standard_library`].
///
/// Deduplicated by canonical path, so naming something the manifest already
/// pulls in is harmless rather than a duplicate-module error.
fn collect_sources(paths: &[PathBuf]) -> Result<Vec<PathBuf>> {
    let roots: Vec<PathBuf> =
        if paths.is_empty() { vec![PathBuf::from(".")] } else { paths.to_vec() };

    let mut out = Vec::new();
    for root in &roots {
        gather(root, &mut out)?;
        // **An entry point names where the program starts, not everything it
        // is made of.** `khora build src/main.kh` has to compile the package
        // that file belongs to, or a program stops being buildable the moment
        // it grows a second module — which is every real program, and it
        // failed with `cannot find module` rather than with anything a reader
        // could act on.
        //
        // The package is the manifest's directory, and `walk` already declines
        // to look in `target`. Deduplication below handles the entry file
        // arriving twice.
        if root.is_file() {
            if let Some(manifest) = nearest_manifest(root) {
                // An empty parent is the manifest in the working directory,
                // which is `.` rather than nowhere.
                let package = match manifest.parent() {
                    Some(dir) if !dir.as_os_str().is_empty() => dir.to_path_buf(),
                    _ => PathBuf::from("."),
                };
                gather(&package, &mut out)?;
            }
        }
    }

    for root in &roots {
        for dependency in dependencies_of(root)? {
            gather(&dependency, &mut out)?;
        }
    }

    if let Some(std_dir) = khora_db::standard_library() {
        gather(&std_dir, &mut out)?;
    }

    // Sorted *and* deduplicated by canonical path, because the same file
    // reached two ways has two spellings — `std/core.kh` from the command line
    // and an absolute path from `standard_library()` — and two spellings of one
    // file is a duplicate-module error rather than a harmless repetition.
    let mut keyed: Vec<(PathBuf, PathBuf)> = out
        .into_iter()
        .map(|p| (std::fs::canonicalize(&p).unwrap_or_else(|_| p.clone()), p))
        .collect();
    keyed.sort();
    keyed.dedup_by(|a, b| a.0 == b.0);
    Ok(keyed.into_iter().map(|(_, path)| path).collect())
}

/// Adds a file, or everything under a directory.
fn gather(root: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
    if root.is_dir() {
        walk(root, out)
    } else {
        out.push(root.to_path_buf());
        Ok(())
    }
}

/// The directories every dependency of the manifest nearest `root` resolves to.
///
/// Transitive, through `khora-pkg`: a dependency's own manifest says what *it*
/// needs, and a git dependency is fetched into the content-addressed store and
/// pinned in `khora.lock`. Roadmap 10.2.
///
/// **A manifest that cannot be parsed contributes nothing rather than failing
/// the build.** `khora check` on the manifest is the command whose job that is,
/// and two commands reporting the same error differently is worse than one
/// reporting it. A manifest that parses and then *cannot be resolved* is
/// different: nothing else will say so, so that error is returned.
fn dependencies_of(root: &Path) -> Result<Vec<PathBuf>> {
    let Some(manifest_path) = nearest_manifest(root) else { return Ok(Vec::new()) };
    let Ok(text) = std::fs::read_to_string(&manifest_path) else { return Ok(Vec::new()) };
    if khora_manifest::Manifest::parse(&text).is_err() {
        return Ok(Vec::new());
    }

    let store = khora_pkg::Store::open()?;
    let resolution = khora_pkg::resolve(&manifest_path, &store, locked_requested())?;

    let parsed = khora_manifest::Manifest::parse(&text)
        .map_err(|e| anyhow::anyhow!("{}: {e}", manifest_path.display()))?;
    check_extern_allowlist(&parsed.manifest.permissions, &resolution)?;

    Ok(resolution.directories())
}

/// Refuses a dependency that declares `extern fn` without being allowed to.
///
/// **This is what turns the permission table from a convention into a
/// guarantee.** Every other grant is a rule about Khora code, and every
/// capability in `std` carries its requirement in its signature — so a
/// program's rows say what it can reach. `extern fn` is the door out of that:
/// a foreign declaration's effect row is a promise the compiler takes on trust,
/// and a dependency that declines to make the promise reaches the operating
/// system with nothing in its signature and nothing in yours.
///
/// `docs/design/permissions.md` has carried this as "the hole this does not
/// close yet" since D4, for a good reason — it is a rule about *which package*
/// a declaration is in, and there were no packages. There are now.
///
/// Checked here rather than in the type checker because the checker sees a flat
/// set of files: package identity exists in the resolver and nowhere else.
fn check_extern_allowlist(
    permissions: &khora_manifest::Permissions,
    resolution: &khora_pkg::Resolution,
) -> Result<()> {
    let mut refused: Vec<String> = Vec::new();

    for package in &resolution.packages {
        if permissions.may_declare_extern(&package.name) {
            continue;
        }
        for (file, function) in extern_declarations(&package.directory)? {
            refused.push(format!(
                "  `{function}` in {}, from the package `{}`",
                file.display(),
                package.name
            ));
        }
    }

    if refused.is_empty() {
        return Ok(());
    }
    anyhow::bail!(
        "these declare `extern fn`, and this project's `[permissions] extern` \
         does not list the packages they are in:\n{}\n\n\
         An `extern fn` reaches the operating system without appearing in \
         anybody's capability row, so allowing one is a decision about trust \
         rather than about types. Add the package to the list if that is what \
         you mean:\n\n    [permissions]\n    extern = [{}]",
        refused.join("\n"),
        allow_list_suggestion(permissions, resolution)
    )
}

/// What the `extern` list would have to say for this build to go through.
fn allow_list_suggestion(
    permissions: &khora_manifest::Permissions,
    resolution: &khora_pkg::Resolution,
) -> String {
    let mut names: Vec<String> =
        permissions.extern_.clone().unwrap_or_default().into_iter().collect();
    for package in &resolution.packages {
        if !permissions.may_declare_extern(&package.name) && !names.contains(&package.name) {
            names.push(package.name.clone());
        }
    }
    names.sort();
    names.iter().map(|n| format!("\"{n}\"")).collect::<Vec<_>>().join(", ")
}

/// Every `extern fn` a package declares, as (file, name).
///
/// A syntax question, so it is answered from the tree rather than by
/// type-checking a package the build may be about to refuse.
fn extern_declarations(directory: &Path) -> Result<Vec<(PathBuf, String)>> {
    use khora_syntax::ast;

    let mut files = Vec::new();
    gather(directory, &mut files)?;

    let db = KhoraDatabase::new();
    let mut out = Vec::new();
    for path in files {
        let Ok(text) = std::fs::read_to_string(&path) else { continue };
        let file = SourceFile::new(&db, path.clone(), text);
        let parse = khora_db::parse(&db, file);
        for declaration in parse.source_file().decls() {
            if let ast::Decl::Fn(f) = declaration {
                if f.is_extern() {
                    let name = f
                        .name()
                        .and_then(|n| n.ident())
                        .unwrap_or_else(|| "<unnamed>".to_string());
                    out.push((path.clone(), name));
                }
            }
        }
    }
    Ok(out)
}

/// Whether `--locked` was asked for, by flag or by `KHORA_LOCKED`.
///
/// Read here rather than threaded through every command because it is a
/// property of the run, not of the subcommand, and every path that resolves
/// wants the same answer.
fn locked_requested() -> bool {
    std::env::args().any(|a| a == "--locked") || std::env::var_os("KHORA_LOCKED").is_some()
}

/// The `khora.toml` governing `start`: in it if it is a directory, beside it if
/// it is a file, or in the nearest ancestor of either.
fn nearest_manifest(start: &Path) -> Option<PathBuf> {
    let mut here: Option<&Path> = Some(if start.is_dir() {
        start
    } else {
        start.parent().unwrap_or(Path::new("."))
    });
    while let Some(dir) = here {
        let candidate = dir.join("khora.toml");
        if candidate.is_file() {
            return Some(candidate);
        }
        here = dir.parent();
    }
    None
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
    for entry in std::fs::read_dir(dir).with_context(|| format!("reading {}", dir.display()))? {
        let path = entry?.path();
        if path.is_dir() {
            if path.file_name().is_some_and(|n| n == "target" || n == ".git") {
                continue;
            }
            walk(&path, out)?;
        } else if path.extension().is_some_and(|e| e == "kh") {
            // A file whose name carries a target suffix belongs to that target
            // only — `socket_windows.kh` is not read at all elsewhere, so two
            // files may declare the same module. `khora_db::selected_for_target`.
            if khora_db::selected_for_target(&path, khora_db::host_target()) {
                out.push(path);
            }
        }
    }
    Ok(())
}
