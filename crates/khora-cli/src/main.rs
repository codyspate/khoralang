//! The `khora` toolchain driver.
//!
//! Only the front-end commands are wired up so far: everything past parsing
//! reports honestly that it is not implemented rather than pretending.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use khora_syntax::ParseError;

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
    /// Compile to a native executable.
    Build {
        #[arg(default_value = ".")]
        path: PathBuf,
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
        Command::Lex { path } => lex(&path).map(|()| true),
        Command::Parse { path, no_trivia } => parse_cmd(&path, no_trivia).map(|()| true),
        Command::Build { path } => {
            let _ = path;
            anyhow::bail!(
                "`khora build` needs the LLVM backend, which is not implemented yet \
                 (crates/khora-codegen-llvm). `khora check` works today."
            )
        }
    }
}

fn check(paths: &[PathBuf]) -> Result<bool> {
    let files = collect_sources(paths)?;
    if files.is_empty() {
        anyhow::bail!("no `.kh` files found");
    }

    let mut clean = true;
    let mut total = 0usize;
    for file in &files {
        let text = read(file)?;
        let parse = khora_syntax::parse(&text);
        debug_assert_eq!(parse.syntax().text().to_string(), text);
        for err in parse.errors() {
            clean = false;
            total += 1;
            eprintln!("{}", render(file, &text, err));
        }
    }

    if clean {
        println!("checked {} file(s): no syntax errors", files.len());
    } else {
        eprintln!("{total} error(s) across {} file(s)", files.len());
    }
    Ok(clean)
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

/// Renders a diagnostic as `path:line:col: message` with the offending line.
fn render(path: &Path, text: &str, err: &ParseError) -> String {
    let offset = usize::from(err.range.start());
    let (line_no, line_start) = text[..offset.min(text.len())]
        .char_indices()
        .filter(|(_, c)| *c == '\n')
        .fold((1usize, 0usize), |(n, _), (i, _)| (n + 1, i + 1));
    let line_end = text[line_start..].find('\n').map_or(text.len(), |i| line_start + i);
    let col = text[line_start..offset.min(line_end)].chars().count() + 1;
    let width = usize::from(err.range.len()).max(1);

    format!(
        "{}:{}:{}: error: {}\n  |\n  | {}\n  | {}{}",
        path.display(),
        line_no,
        col,
        err.message,
        &text[line_start..line_end],
        " ".repeat(col - 1),
        "^".repeat(width),
    )
}
