//! The `khora` toolchain driver.
//!
//! Only the front-end commands are wired up so far: everything past parsing
//! reports honestly that it is not implemented rather than pretending.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use khora_db::{KhoraDatabase, SourceFile, SourceRoot};
use khora_diagnostics::render_parse_errors;

#[derive(Parser)]
#[command(name = "khora", version, about = "The Khora language toolchain")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Parse a file and report diagnostics.
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
    match run() {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::FAILURE,
        Err(err) => {
            eprintln!("khora: {err:#}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<bool> {
    let cli = Cli::parse();
    match cli.command {
        Command::Check { paths } => check(&paths),
        Command::Fmt { paths, check } => fmt(&paths, check),
        Command::Lex { path } => lex(&path).map(|()| true),
        Command::Parse { path, no_trivia } => parse_cmd(&path, no_trivia).map(|()| true),
        Command::Build { path, out } => build(&path, out.as_deref()),
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

    let mut clean = true;
    let mut total = 0usize;
    for (path, input) in &inputs {
        let parse = khora_db::parse(&db, *input);
        let text = input.text(&db);
        debug_assert_eq!(parse.syntax().text().to_string(), text);
        if !parse.errors().is_empty() {
            clean = false;
            total += parse.errors().len();
            eprintln!("{}\n", render_parse_errors(path, text, parse.errors()));
        }
    }

    if clean {
        println!("checked {} file(s): no syntax errors", files.len());
    } else {
        eprintln!("{total} error(s) across {} file(s)", files.len());
    }
    Ok(clean)
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

/// Compiles a single file to a native executable.
///
/// Semantic errors are reported through the same renderer `check` uses, so a
/// diagnostic reads identically whichever command surfaced it.
#[cfg(feature = "llvm")]
fn build(path: &Path, out: Option<&Path>) -> Result<bool> {
    let files = collect_sources(std::slice::from_ref(&path.to_path_buf()))?;
    let [source] = files.as_slice() else {
        anyhow::bail!(
            "`khora build` takes exactly one `.kh` file; found {}.              Multi-file programs need the module graph, which is phase 3.",
            files.len()
        )
    };

    let text = read(source)?;
    let db = KhoraDatabase::new();
    let input = SourceFile::new(&db, source.clone(), text.clone());

    let parse = khora_db::parse(&db, input);
    if !parse.errors().is_empty() {
        eprintln!("{}
", render_parse_errors(source, &text, parse.errors()));
        return Ok(false);
    }

    let target = out.map(Path::to_path_buf).unwrap_or_else(|| {
        let stem = source.file_stem().unwrap_or_default();
        source.with_file_name(stem).with_extension(std::env::consts::EXE_EXTENSION)
    });

    match khora_codegen_llvm::compile(&db, input, &target) {
        Ok(()) => {
            println!("built {}", target.display());
            Ok(true)
        }
        Err(errors) => {
            for err in &errors {
                eprintln!("error: {}", err.message);
            }
            eprintln!("{} error(s)", errors.len());
            Ok(false)
        }
    }
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

fn collect_sources(paths: &[PathBuf]) -> Result<Vec<PathBuf>> {
    let roots: Vec<PathBuf> =
        if paths.is_empty() { vec![PathBuf::from(".")] } else { paths.to_vec() };

    let mut out = Vec::new();
    for root in roots {
        if root.is_dir() {
            walk(&root, &mut out)?;
        } else {
            out.push(root);
        }
    }
    out.sort();
    Ok(out)
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
            out.push(path);
        }
    }
    Ok(())
}
