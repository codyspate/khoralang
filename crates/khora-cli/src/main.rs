//! The `khora` toolchain driver.
//!
//! One binary for everything a person does with the language: check, format,
//! build, test, bench, document, add a dependency, and pick which version of
//! itself a project gets. `khora toolchain` is the part that manages the
//! others, so there is nothing else to install first.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use khora_db::{KhoraDatabase, SourceFile, SourceRoot};
use khora_diagnostics::{
    render_hir_errors, render_hir_errors_as, render_parse_errors, Severity,
};
use khora_manifest::LintLevel;

mod affected;
#[cfg(feature = "llvm")]
mod cache;
mod task;
mod workspace_cmds;

#[derive(Parser)]
#[command(
    name = "khora",
    version = khora_toolchain::RUNNING,
    about = "The Khora language toolchain"
)]
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
        /// Only the members a diff since this revision can reach.
        ///
        /// A branch, a tag or a commit -- anything `git diff` takes. Exact
        /// rather than heuristic: the resolver already knows which packages
        /// each member compiles. A changed file that belongs to no member and
        /// to nothing a member depends on selects *everything*, and says which
        /// file did it. Only at a workspace root. `docs/roadmap.md` 14.16.
        #[arg(long, value_name = "REV")]
        since: Option<String>,
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
        /// Only the members a diff since this revision can reach.
        ///
        /// A branch, a tag or a commit -- anything `git diff` takes. Exact
        /// rather than heuristic: the resolver already knows which packages
        /// each member compiles. A changed file that belongs to no member and
        /// to nothing a member depends on selects *everything*, and says which
        /// file did it. Only at a workspace root. `docs/roadmap.md` 14.16.
        #[arg(long, value_name = "REV")]
        since: Option<String>,
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
        /// Build a shared library instead of an executable.
        ///
        /// Its interface is the `pub extern fn`s, and a C header is written
        /// beside it. `docs/design/c-export.md`.
        #[arg(long)]
        lib: bool,
        /// Optimize, drop debug information, and be reproducible.
        ///
        /// The default profile is `debug`: unoptimized, with debug information,
        /// which is what a crash you are about to read wants. `KHORA_PROFILE`
        /// says the same thing to `khora test` and `khora bench`, which have no
        /// flag of their own. `docs/design/profiles.md`.
        #[arg(long)]
        release: bool,
        /// Compile even if the cache already has this exact build.
        ///
        /// The output still goes into the cache. For measuring how long a
        /// build takes, and for doubting the cache -- `docs/design/cache.md`
        /// says why doubting it should be answerable by rebuilding.
        #[arg(long)]
        no_cache: bool,
    },
    /// Write a software bill of materials for a package's dependencies.
    ///
    /// CycloneDX 1.5, JSON, on standard output unless `--out` names a file.
    /// The document is a pure function of `khora.toml` and `khora.lock` — no
    /// timestamp — so two runs over unchanged input produce identical bytes
    /// and a diff means something changed.
    Sbom {
        /// A directory containing a `khora.toml`, or a file beside one.
        #[arg(default_value = ".")]
        path: PathBuf,
        /// Where to write it. Standard output by default.
        #[arg(short, long)]
        out: Option<PathBuf>,
    },
    /// Generate API documentation from `///` and `//!` comments.
    ///
    /// One markdown page per module, written into `--out`. That directory is
    /// *owned* by this command: pages for modules that no longer exist are
    /// deleted, so a stale page cannot outlive the code it documented.
    Doc {
        /// One or more `.kh` files, or directories to walk.
        #[arg(default_value = "std")]
        paths: Vec<PathBuf>,
        /// Where the pages go.
        #[arg(short, long, default_value = "website/content/docs/stdlib/api")]
        out: PathBuf,
        /// Report what would change and write nothing.
        ///
        /// The exit status is what CI reads: non-zero means the checked-in
        /// documentation no longer matches the source.
        #[arg(long)]
        check: bool,
    },
    /// Add a package to this project, or fetch what it already asks for.
    ///
    /// A git URL is the whole address; there is no registry to look a name up
    /// in. Over editing `khora.toml` by hand, this finds out the package's real
    /// name and whether it offers itself at all *before* writing the entry.
    Install {
        /// The git URL of the repository holding the package.
        ///
        /// Left out, this fetches and locks whatever `khora.toml` already
        /// declares -- the thing to run after cloning a project.
        url: Option<String>,
        /// The branch, tag or commit to depend on.
        #[arg(long, default_value = "main")]
        rev: String,
        /// Where in the repository the package is, if it is not the root.
        #[arg(long)]
        subdir: Option<String>,
        /// The project to add it to.
        #[arg(long, default_value = ".")]
        path: PathBuf,
    },
    /// Compile the program and run it.
    ///
    /// What `cargo run` is for: the shortest path from a source file to its
    /// output. The build goes through the cache, so the second run of an
    /// unchanged program starts almost immediately.
    ///
    /// Arguments after `--` go to the program, not to `khora`. The program's
    /// exit status becomes this command's, so `khora run` is usable in a
    /// script the way running the executable directly would be.
    Run {
        /// A `.kh` file, or a directory containing one.
        #[arg(default_value = ".")]
        path: PathBuf,
        /// Optimize, and drop debug information.
        #[arg(long)]
        release: bool,
        /// Compile even if the cache already has this exact build.
        #[arg(long)]
        no_cache: bool,
        /// Arguments for the program.
        #[arg(last = true)]
        args: Vec<String>,
    },
    /// Run a task from `[tasks]`, and everything it depends on first.
    ///
    /// With no name, lists what there is to run. At a workspace root the task
    /// runs in every member that has something to run for it, in dependency
    /// order.
    ///
    /// A task's `run` line goes to the platform shell. That is not build-time
    /// code execution coming back: nothing reaches a task except somebody
    /// typing its name, and a dependency's `[tasks]` table is never read.
    /// `docs/design/tasks.md`.
    Task {
        /// The task to run. Omitted, lists them.
        name: Option<String>,
        /// The package or workspace root.
        #[arg(long, default_value = ".")]
        path: PathBuf,
        /// Only the members a diff since this revision can reach.
        ///
        /// The same selection `khora check --since` makes. Only at a
        /// workspace root.
        #[arg(long, value_name = "REV")]
        since: Option<String>,
    },
    /// Scaffold a new package.
    ///
    /// Inside a workspace that shares a version, the manifest it writes says
    /// `version.workspace = true` -- a scaffold that hard-codes one into a
    /// monorepo where everything else inherits has quietly created the drift
    /// the inheritance existed to prevent.
    New {
        /// The directory to create. Its name is the package's name.
        path: PathBuf,
        /// Offer it as a library: `publish = true` and a `src/lib.kh`.
        #[arg(long)]
        lib: bool,
    },
    /// Explain why a package is in the build.
    ///
    /// Every chain from something you declared down to it, shortest first. A
    /// package reached three ways has three reasons, and printing one of them
    /// is how somebody removes a dependency and finds it still there.
    Why {
        /// The package to explain.
        name: String,
        /// The package or workspace to ask about.
        #[arg(long, default_value = ".")]
        path: PathBuf,
    },
    /// Draw the dependency graph.
    Graph {
        /// The package or workspace root.
        #[arg(long, default_value = ".")]
        path: PathBuf,
        /// Emit Graphviz `dot` instead of a tree.
        #[arg(long)]
        dot: bool,
    },
    /// Show what the build cache holds, or empty it.
    ///
    /// One entry per (sources, compiler, linker, runtime, target, profile).
    /// `docs/design/cache.md`.
    Cache {
        /// Delete every entry.
        #[arg(long)]
        clear: bool,
    },
    /// Get the newest Khora and make it the default.
    ///
    /// `khora toolchain install` plus `khora toolchain default`, which is what
    /// somebody wants almost every time. The version this replaces stays
    /// installed, so going back is `khora toolchain default <old>`.
    Update {
        /// Consider release candidates too.
        #[arg(long)]
        pre: bool,
    },
}

fn main() -> ExitCode {
    // Before anything else, including argument parsing: a project pinning a
    // version whose flags this build does not recognise must still work.
    hand_over_if_pinned();

    match dispatch() {
        Ok(code) => code,
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
    /// Download and unpack a published release.
    ///
    /// With no version, the newest stable one. Installing a version does not
    /// switch to it; `khora toolchain default` does that.
    Install {
        /// The version to get. Defaults to the newest release.
        version: Option<String>,
        /// Consider release candidates too.
        ///
        /// A candidate is published as a pre-release, which the newest-stable
        /// lookup skips. This means "candidates as well", not "candidates
        /// only": the day after a stable release, the newest release of any
        /// kind is that stable one.
        #[arg(long)]
        pre: bool,
    },
    /// Choose which version a directory with no pin gets.
    ///
    /// A project's own `[toolchain]` pin always wins over this.
    Default {
        /// The version to use. Left out, this says which one is set.
        version: Option<String>,
        /// Go back to using whatever is on the path.
        #[arg(long, conflicts_with = "version")]
        none: bool,
    },
    /// Register a Khora executable as the toolchain for a version.
    ///
    /// For a version you built yourself. `install` is the one that downloads.
    Link {
        /// The version it will be known as.
        version: String,
        /// The executable to register. It is copied, not pointed at.
        path: PathBuf,
    },
    /// Remove an installed toolchain.
    #[command(alias = "unlink")]
    Remove { version: String },
    /// Say which toolchain this directory would use, and why.
    Which {
        #[arg(default_value = ".")]
        path: PathBuf,
    },
}

/// Runs the command, and says what to exit with.
///
/// **An `ExitCode` rather than a bool**, because `khora run` has to hand back
/// the program's own status. Every other command still answers "did it work",
/// and is mapped at the bottom; only the one that needs a number returns one.
fn dispatch() -> Result<ExitCode> {
    let cli = Cli::parse();
    let worked = match cli.command {
        Command::Check { paths, since } => check(&paths, since.as_deref()),
        Command::Fmt { paths, check, since } => fmt(&paths, check, since.as_deref()),
        Command::Lex { path } => lex(&path).map(|()| true),
        Command::Parse { path, no_trivia } => parse_cmd(&path, no_trivia).map(|()| true),
        Command::Build { path, out, lib, release, no_cache } => {
            build(&path, out.as_deref(), lib, release, no_cache)
        }
        Command::Cache { clear } => cache_command(clear).map(|()| true),
        Command::Sbom { path, out } => sbom(&path, out.as_deref()).map(|()| true),
        Command::Doc { paths, out, check } => doc(&paths, &out, check),
        Command::Install { url, rev, subdir, path } => {
            install(url.as_deref(), &rev, subdir.as_deref(), &path).map(|()| true)
        }
        Command::Lsp => lsp().map(|()| true),
        Command::Mcp => mcp().map(|()| true),
        Command::Toolchain { command } => toolchain(command),
        Command::Run { path, release, no_cache, args } => {
            return run_program(&path, release, no_cache, &args)
        }
        Command::Task { name, path, since } => match name {
            Some(name) => {
                let members = match workspace_members(std::slice::from_ref(&path)) {
                    Some(members) => {
                        Some(narrow(std::slice::from_ref(&path), &members, since.as_deref())?)
                    }
                    None if since.is_some() => anyhow::bail!(
                        "`--since` selects members of a workspace, and this is not a \
                         workspace root. Run it where the `[workspace]` table is"
                    ),
                    None => None,
                };
                task::run(&path, &name, members.as_deref())
            }
            None => task::list(&path).map(|()| true),
        },
        Command::New { path, lib } => workspace_cmds::new(&path, lib).map(|()| true),
        Command::Why { name, path } => workspace_cmds::why(&path, &name).map(|()| true),
        Command::Graph { path, dot } => {
            let members = workspace_members(std::slice::from_ref(&path));
            workspace_cmds::graph(&path, members.as_deref(), dot).map(|()| true)
        }
        Command::Update { pre } => update(pre),
        Command::Test { path, filter } => test(&path, filter.as_deref()),
        Command::Bench { path, filter } => bench(&path, filter.as_deref()),
    }?;
    Ok(if worked { ExitCode::SUCCESS } else { ExitCode::FAILURE })
}

/// Parse and type check.
///
/// **A workspace root fans out over its members**, one at a time, each as its
/// own package. That is not an optimisation to skip: a member's dependencies
/// come from *its* manifest, so checking a whole directory as one compilation
/// resolves one manifest for several programs and finds neither the
/// dependency nor the reason it was missing. `scripts/baseline.sh` had that
/// loop written in shell, with a comment explaining the workaround.
fn check(paths: &[PathBuf], since: Option<&str>) -> Result<bool> {
    let paths = &here_if_empty(paths);
    if let Some(members) = workspace_members(paths) {
        let members = narrow(paths, &members, since)?;
        return over_members(&members, "check", |directory| {
            check_one(std::slice::from_ref(directory))
        });
    }
    if since.is_some() {
        anyhow::bail!(
            "`--since` selects members of a workspace, and this is not a workspace root. \
             Run it where the `[workspace]` table is"
        );
    }
    check_one(paths)
}

/// The members a `--since` diff can reach, reporting what it left out.
///
/// Without `--since`, everything, unchanged. **The report is not optional**: a
/// command that quietly ran a third of what its name implies is one nobody can
/// read a green tick from, and the skipped list is how somebody checks the
/// answer against what they believe they changed.
fn narrow(paths: &[PathBuf], members: &[PathBuf], since: Option<&str>) -> Result<Vec<PathBuf>> {
    let Some(since) = since else { return Ok(members.to_vec()) };
    let root = paths.first().cloned().unwrap_or_else(|| PathBuf::from("."));
    let selection = affected::select(&root, members, since)?;

    if let Some(file) = &selection.everything_because {
        println!(
            "every member, because {} is not inside any of them or anything they depend on",
            file.display()
        );
        return Ok(selection.members);
    }
    if selection.members.is_empty() {
        println!("no member is affected by the changes since {since}");
    } else if !selection.skipped.is_empty() {
        println!(
            "{} of {} member(s) affected since {since}; skipping {}",
            selection.members.len(),
            members.len(),
            selection
                .skipped
                .iter()
                .map(|p| p.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    Ok(selection.members)
}

/// `paths`, or the working directory when it is empty.
///
/// **`khora check` and `khora check .` have to be the same command.** They
/// were not: an empty list reached `collect_sources`, which substitutes `.`
/// there and nowhere else, so the bare form walked the whole repository as one
/// compilation while the explicit form fanned out over eight members. Two
/// commands, one name, and the difference was a character somebody did or did
/// not type.
///
/// Normalised here rather than by giving the argument a `default_value`,
/// because clap's default would make `paths` non-empty before anything could
/// tell the two apart -- which is the right answer, and this says why in a
/// place a reader will find it.
fn here_if_empty(paths: &[PathBuf]) -> Vec<PathBuf> {
    if paths.is_empty() {
        vec![PathBuf::from(".")]
    } else {
        paths.to_vec()
    }
}

/// The members of the workspace `paths` names, if it names exactly one and
/// that one is a root.
///
/// One path, because `khora check a b` is a request about two things and
/// fanning either of them out would make the report incomprehensible. And only
/// a root named *directly*: being inside a workspace does not mean a bare
/// `khora check .` in one member should check all of them.
fn workspace_members(paths: &[PathBuf]) -> Option<Vec<PathBuf>> {
    let [only] = paths else { return None };
    let manifest = if only.is_dir() { only.join("khora.toml") } else { return None };
    let found = khora_manifest::read_workspace(&manifest).ok().flatten()?;
    if found.members.is_empty() {
        return None;
    }
    Some(found.members)
}

/// Runs `each` over every member, reporting per member and failing if any did.
///
/// **Every member runs even after one fails.** A monorepo command that stopped
/// at the first failure would make a reader fix one thing, run again, and find
/// the next — which is the loop the shell script had and the reason it was
/// worth replacing.
fn over_members(
    members: &[PathBuf],
    verb: &str,
    mut each: impl FnMut(&PathBuf) -> Result<bool>,
) -> Result<bool> {
    let mut all_clean = true;
    let mut failed = Vec::new();
    for member in members {
        println!("== {} {}", verb, member.display());
        match each(member) {
            Ok(true) => {}
            Ok(false) => {
                all_clean = false;
                failed.push(member.clone());
            }
            Err(why) => {
                // Reported and carried on with, for the reason above. The
                // status still says the run failed.
                eprintln!("khora: {}: {why:#}", member.display());
                all_clean = false;
                failed.push(member.clone());
            }
        }
    }

    // The count either way, so a clean run says how much it covered. "8
    // members clean" is a different claim from "clean", and the second is what
    // a workspace command silently makes when a pattern matched nothing.
    if all_clean {
        println!("\n{} member(s) clean", members.len());
    } else {
        println!(
            "\n{} of {} member(s) failed: {}",
            failed.len(),
            members.len(),
            failed.iter().map(|p| p.display().to_string()).collect::<Vec<_>>().join(", ")
        );
    }
    Ok(all_clean)
}

fn check_one(paths: &[PathBuf]) -> Result<bool> {
    report_manifest_warnings(paths.first().map(PathBuf::as_path));

    let files = collect_sources(paths)?;
    if files.is_empty() {
        anyhow::bail!("no `.kh` files found");
    }

    // Through the query database even for a one-shot run, so there is no
    // second code path to drift from the one the language server uses.
    let db = KhoraDatabase::new();
    let mut inputs = Vec::with_capacity(files.len());
    for path in &files {
        let text = read(path)?;
        inputs.push((path, SourceFile::new(&db, path.clone(), text)));
    }
    SourceRoot::new(&db, inputs.iter().map(|(_, f)| *f).collect());

    // Read once. A file outside any package gets the defaults, so `khora check
    // scratch.kh` works without a manifest.
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
            // Half of what a lint sees downstream of a type error is an
            // artefact of it.
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
/// A lint the manifest does not mention warns: this set is quiet enough to be
/// worth hearing and not worth failing a build over.
///
/// A manifest that cannot be read contributes nothing rather than failing the
/// command — complaining about the manifest is `khora check`'s job, not every
/// other command's.
/// Prints what the manifest audit noticed, if anything.
///
/// **Nothing printed these until now.** `khora-manifest` has read every
/// manifest a second time as a tree of keys since it existed, compared it
/// against a hand-written schema, and produced a `Warning` per key the schema
/// does not describe -- and every caller dropped the vector on the floor. A
/// whole module, its schema and its tests, arriving nowhere. Found while
/// giving `explicit-semicolons` a good message to be removed with, which is
/// its own small lesson about who reads a diagnostic. Roadmap 14.20b.
///
/// On stderr and never fatal: a manifest written against a newer toolchain has
/// to stay buildable by an older one, which is the whole reason the audit
/// warns rather than erroring.
fn report_manifest_warnings(start: Option<&Path>) {
    let Some(manifest_path) = start.and_then(nearest_manifest) else { return };
    let Ok(parsed) = khora_manifest::Manifest::load(&manifest_path) else { return };
    for warning in &parsed.warnings {
        eprintln!("warning: {}: {warning}", manifest_path.display());
    }
}

/// The `[fmt]` settings governing `start`, or the formatter's own defaults.
///
/// A manifest that cannot be read contributes nothing rather than failing the
/// command, the same rule `lint_levels` follows: complaining about the manifest
/// is `khora check`'s job, and two commands reporting one error differently is
/// worse than one reporting it.
fn fmt_options(start: Option<&Path>) -> khora_fmt::Options {
    let Some(manifest_path) = start.and_then(nearest_manifest) else {
        return khora_fmt::Options::default();
    };
    let Ok(parsed) = khora_manifest::Manifest::load(&manifest_path) else {
        return khora_fmt::Options::default();
    };
    let Some(table) = parsed.manifest.fmt else { return khora_fmt::Options::default() };
    match table.indent_style {
        Some(khora_manifest::IndentStyle::Tab) => khora_fmt::Options::tabs(),
        // Spaces either way: `indent-width` on its own is a width in spaces,
        // because nobody writes `indent-width = 4` meaning four tabs.
        Some(khora_manifest::IndentStyle::Space) | None => match table.indent_width {
            Some(width) => khora_fmt::Options::spaces(width),
            None => khora_fmt::Options::default(),
        },
    }
}

fn lint_levels(start: Option<&Path>) -> std::collections::BTreeMap<String, LintLevel> {
    let mut out = std::collections::BTreeMap::new();
    let Some(manifest_path) = start.and_then(nearest_manifest) else { return out };
    let Ok(parsed) = khora_manifest::Manifest::load(&manifest_path) else { return out };

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
            "khora mcp speaks the Model Context Protocol on stdin and stdout, so it is \
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
                    "nothing installed here; this is Khora {} and it is the only one \
                     that will run.\n\n    khora toolchain install <version>   \
                     get a published release\n    \
                     khora toolchain link <version> <path>   register one you built",
                    khora_toolchain::RUNNING,
                );
                return Ok(true);
            }
            let default = khora_toolchain::install::default_version();
            // **The one on the path is a toolchain too**, and it is not under
            // `toolchains/`: `install.sh` unpacks it into `~/.khora` directly,
            // and that is the whole point — it is the bootstrap, and everything
            // installed afterwards sits beside it rather than replacing it.
            // Leaving it out of this list makes "go back to the one I had"
            // look impossible after the first `khora update`.
            if !installed.iter().any(|t| t.version == khora_toolchain::RUNNING) {
                // No default at all also means this one: an unpinned directory
                // runs whatever is on the path, which is this.
                let chosen = match default.as_deref() {
                    None | Some(khora_toolchain::RUNNING) => "running, default",
                    Some(_) => "running",
                };
                println!("{}  ({chosen}, on your path)", khora_toolchain::RUNNING);
            }
            for entry in installed {
                let mut notes = Vec::new();
                if entry.version == khora_toolchain::RUNNING {
                    notes.push("running");
                }
                if default.as_deref() == Some(entry.version.as_str()) {
                    notes.push("default");
                }
                let notes =
                    if notes.is_empty() { String::new() } else { format!("  ({})", notes.join(", ")) };
                println!("{}{notes}", entry.version);
            }
            Ok(true)
        }
        ToolchainCommand::Install { version, pre } => {
            let wanted = match version {
                Some(exact) => khora_toolchain::install::Wanted::Exactly(exact),
                None if pre => khora_toolchain::install::Wanted::Newest,
                None => khora_toolchain::install::Wanted::Latest,
            };
            let version = khora_toolchain::install::resolve(&wanted)?;
            if version == khora_toolchain::RUNNING {
                println!("Khora {version} is already what is running.");
            }
            println!("Khora {version} for {}", khora_toolchain::install::TARGET);
            println!("  downloading and verifying");
            let at = khora_toolchain::install::install(&version)?;
            println!("  installed at {}", at.display());

            // The first toolchain installed becomes the default, because
            // somebody who has installed exactly one of something did not mean
            // to leave it unused.
            if khora_toolchain::install::default_version().is_none() {
                khora_toolchain::install::set_default(&version)?;
                println!("  and is now the default");
            } else {
                println!("\nUse it everywhere:  khora toolchain default {version}");
                println!("Or in one project:  [toolchain] version = \"{version}\" in khora.toml");
            }
            Ok(true)
        }
        ToolchainCommand::Default { version, none } => {
            if none {
                khora_toolchain::install::clear_default()?;
                println!("no default; whatever is on the path runs.");
                return Ok(true);
            }
            match version {
                Some(version) => {
                    khora_toolchain::install::set_default(&version)?;
                    println!("Khora {version} is now the default.");
                }
                None => match khora_toolchain::install::default_version() {
                    Some(version) => println!("{version}"),
                    None => println!(
                        "no default; whatever is on the path runs. This is Khora {}.",
                        khora_toolchain::RUNNING
                    ),
                },
            }
            Ok(true)
        }
        ToolchainCommand::Link { version, path } => {
            let at = khora_toolchain::link(&version, &path)?;
            println!("registered Khora {version} at {}", at.display());
            Ok(true)
        }
        ToolchainCommand::Remove { version } => {
            khora_toolchain::unlink(&version)?;
            // A default naming something that is gone would refuse every
            // command in every unpinned directory, which is a hard state to get
            // out of when the command that fixes it is one of them.
            if khora_toolchain::install::default_version().as_deref() == Some(version.as_str()) {
                khora_toolchain::install::clear_default()?;
                println!("removed Khora {version}, which was the default; there is no default now");
            } else {
                println!("removed Khora {version}");
            }
            Ok(true)
        }
        ToolchainCommand::Which { path } => {
            // Both halves, and which one answered: "0.2.0 because this project
            // says so" and "0.2.0 because you chose it once" are different
            // facts, and saying which is the whole point of this command.
            let pinned = khora_toolchain::pinned_version(&path);
            let because =
                if pinned.is_some() { "this project pins it" } else { "it is your default" };
            // **The same rule `hand_over_if_pinned` uses**, or this reports a
            // handover that will not happen. A command whose whole job is to
            // say what would run has to agree with what runs.
            let default = if managed_binary() {
                khora_toolchain::install::default_version()
            } else {
                None
            };
            if pinned.is_none() && khora_toolchain::install::default_version().is_some() && default.is_none()
            {
                println!(
                    "no pin here. Your default is Khora {}, but this is a build of the \
                     compiler rather than an installed one, so it runs as itself: Khora {}.",
                    khora_toolchain::install::default_version().unwrap_or_default(),
                    khora_toolchain::RUNNING
                );
                return Ok(true);
            }
            match pinned.or(default) {
                None => println!(
                    "no pin here and no default, so whatever is on the path runs. \
                     This is Khora {}.",
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
                            println!("{wanted}, because {because} — and that is what is running")
                        }
                        khora_toolchain::Decision::Handover(t) => println!(
                            "{wanted}, because {because}, at {}\nthis is {}, which would hand over",
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

/// `khora update`.
///
/// The newest release, installed and made the default. The version it replaces
/// is left on disk: an update that deletes the thing you were using is one you
/// cannot undo at the moment you discover you need to.
fn update(pre: bool) -> Result<bool> {
    let wanted = if pre {
        khora_toolchain::install::Wanted::Newest
    } else {
        khora_toolchain::install::Wanted::Latest
    };
    let version = khora_toolchain::install::resolve(&wanted)?;

    if version == khora_toolchain::RUNNING
        && khora_toolchain::install::default_version().as_deref() == Some(version.as_str())
    {
        println!("Khora {version} is the newest release, and is what you have.");
        return Ok(true);
    }

    println!("Khora {} → {version}", khora_toolchain::RUNNING);
    println!("  downloading and verifying");
    khora_toolchain::install::install(&version)?;
    khora_toolchain::install::set_default(&version)?;
    println!("  installed, and now the default");
    println!("\nGo back with:  khora toolchain default {}", khora_toolchain::RUNNING);
    Ok(true)
}

/// Whether the running executable is one `KHORA_HOME` manages.
///
/// True for the bootstrap `~/.khora/bin/khora` the installer unpacks and for
/// anything under `~/.khora/toolchains/`; false for a `target/debug/khora`
/// somebody built. See [`hand_over_if_pinned`] for what turns on it.
///
/// Both paths are canonicalized, because on Windows one side arrives with a
/// `\\?\` prefix and the other without, and comparing them as written answers
/// `false` for a binary that is plainly inside the directory. A path that
/// cannot be canonicalized — a home that does not exist yet, which is the
/// ordinary case before the first install — answers `false`, which is the safe
/// direction: it declines to redirect rather than redirecting wrongly.
fn managed_binary() -> bool {
    let Ok(home) = khora_toolchain::home() else { return false };
    let Ok(home) = home.canonicalize() else { return false };
    let Ok(exe) = std::env::current_exe() else { return false };
    let Ok(exe) = exe.canonicalize() else { return false };
    exe.starts_with(&home)
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
    // rather than the project, and handing it over makes the situation it
    // exists for unrecoverable: inside a project whose pinned version is
    // missing, the pin would refuse to let the command that installs it run.
    // `which` would also report on the toolchain that answered rather than on
    // the decision being asked about.
    // `khora update` is the other one, for the same reason: it is how a broken
    // default gets replaced.
    if matches!(std::env::args().nth(1).as_deref(), Some("toolchain") | Some("update")) {
        return;
    }

    // A handover already happened, so we are what was asked for. Re-deciding
    // here is how a mislinked toolchain becomes an infinite chain of `exec`s
    // that presents as a hang.
    let active = std::env::var(khora_toolchain::ACTIVE).ok();
    let here = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let pin = khora_toolchain::pinned_version(&here);
    // **A missing pin and a missing default are not the same failure.** A pin
    // is what the project requires, so a version that is not installed stops
    // the command. A default is a preference expressed once, possibly on
    // another day about a toolchain since removed — refusing every command in
    // every unpinned directory over that would be a machine somebody has to
    // repair before they can use it.
    let from_pin = pin.is_some();
    // **A machine default may only redirect a managed binary**, and a compiler
    // you built and ran by path is not one.
    //
    // Without this, anybody who develops Khora *and* has Khora installed loses
    // their own build silently: `khora update` writes a version into
    // `~/.khora/default`, and from then on `./target/debug/khora` hands every
    // command to the installed release. It surfaced as this repository's own
    // baseline failing with "no linker found" against
    // `~/.khora/toolchains/0.1.0-rc.1/std/ai.kh` — a path from a toolchain
    // nobody in that command had mentioned.
    //
    // A **pin** still redirects anything, because that is a project stating a
    // requirement rather than a machine stating a preference, and the one
    // project that would suffer for it is this one, which pins nothing.
    //
    // rustup draws the same line by a different mechanism: its shims are what
    // redirect, and a binary you built yourself is never a shim.
    let wanted = match pin {
        Some(version) => Some(version),
        None if managed_binary() => khora_toolchain::install::default_version(),
        None => None,
    };

    let installed = khora_toolchain::installed().unwrap_or_default();
    let decision = khora_toolchain::decide(
        wanted.as_deref(),
        khora_toolchain::RUNNING,
        active.as_deref(),
        &installed,
    );

    match decision {
        khora_toolchain::Decision::Proceed => {}
        khora_toolchain::Decision::Missing { wanted, .. } if !from_pin => {
            eprintln!(
                "khora: your default is Khora {wanted}, which is not installed. \
                 Running {} instead.\n       Fix it with `khora toolchain install {wanted}` \
                 or `khora toolchain default --none`.",
                khora_toolchain::RUNNING
            );
        }
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
            "khora lsp speaks the Language Server Protocol on stdin and stdout, so it is \
             waiting for a `Content-Length` header rather than for you. Point an editor \
             at it instead."
        );
    }
    let stdin = std::io::stdin();
    let mut input = stdin.lock();
    let stdout = std::io::stdout();
    let mut output = stdout.lock();
    khora_lsp::serve(&mut input, &mut output)
}

/// Formats files in place, or reports which would change.
fn fmt(paths: &[PathBuf], check: bool, since: Option<&str>) -> Result<bool> {
    let paths = &here_if_empty(paths);
    if let Some(members) = workspace_members(paths) {
        let members = narrow(paths, &members, since)?;
        let verb = if check { "check formatting" } else { "format" };
        return over_members(&members, verb, |directory| {
            fmt_one(std::slice::from_ref(directory), check)
        });
    }
    if since.is_some() {
        anyhow::bail!(
            "`--since` selects members of a workspace, and this is not a workspace root. \
             Run it where the `[workspace]` table is"
        );
    }
    fmt_one(paths, check)
}

fn fmt_one(paths: &[PathBuf], check: bool) -> Result<bool> {
    // **Only what was named**, and not `collect_sources`, which is the
    // compiler's question: an entry point's package, its dependencies, and the
    // standard library. Formatting needs none of that -- a file is formatted
    // by itself -- and pulling it in meant `khora fmt examples/ledger_service`
    // reformatted `std`. That was invisible for as long as `[fmt]` was read by
    // nobody and every file used the same two spaces; the moment a member
    // could ask for four, it would have rewritten the standard library in a
    // member's style. Roadmap 14.20a.
    let roots: Vec<PathBuf> =
        if paths.is_empty() { vec![PathBuf::from(".")] } else { paths.to_vec() };
    let mut files = Vec::new();
    for root in &roots {
        gather(root, &mut files)?;
    }
    files.sort();
    files.dedup();
    if files.is_empty() {
        anyhow::bail!("no `.kh` files found");
    }
    let options = fmt_options(paths.first().map(PathBuf::as_path));

    let mut changed = Vec::new();
    let mut failed = 0usize;
    for path in &files {
        let src = read(path)?;
        match khora_fmt::format_with(&src, &options) {
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
/// environment**, so the executable left behind behaves the same when somebody
/// runs it directly.
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
fn build(
    path: &Path,
    out: Option<&Path>,
    lib: bool,
    release: bool,
    no_cache: bool,
) -> Result<bool> {
    one_program(path, "build")?;
    let (db, inputs, root) = load(path)?;

    // `--release` wins over the variable, and the variable is how everything
    // without a flag says it. Passed rather than set, because the linker asks
    // the same question later and has to be given the same answer.
    let profile = if release {
        khora_codegen_llvm::Profile::Release
    } else {
        khora_codegen_llvm::Profile::from_env()
    };

    // The binary is named after the module holding `main`, or after the one
    // file when there is only one.
    let entry = inputs
        .iter()
        .find(|(_, text, _)| text.contains("fn main("))
        .or_else(|| inputs.first())
        .expect("at least one source");
    let target = out.map(Path::to_path_buf).unwrap_or_else(|| {
        let stem = entry.0.file_stem().unwrap_or_default();
        entry.0.with_file_name(stem).with_extension(library_extension(lib))
    });

    // **Everything the output depends on**, which is the source plus the
    // toolchain that turns it into bytes. `cache::Cache` is where the argument
    // for each field is; the short version is that a cache whose key is only
    // the source is a cache that hands you last week's compiler's answer.
    let sources: Vec<(PathBuf, String)> =
        inputs.iter().map(|(path, text, _)| (path.clone(), text.clone())).collect();
    let wanted = cache::Inputs {
        sources: &sources,
        profile: profile.name(),
        debug_info: profile.debug_info(),
        kind: if lib { cache::Kind::Library } else { cache::Kind::Executable },
    };
    // A cache that cannot be opened is a cache that is not used. Never fatal:
    // see the module comment.
    let store = cache::Cache::open().ok();
    let key = store.as_ref().and_then(|store| store.key(&wanted));
    // **A build that is not cached says so.** The key needs the compiler, the
    // linker and the runtime archive to be identifiable, and if one of them is
    // not this build silently becomes uncacheable -- and so does the next one,
    // and nobody ever finds out why the cache is not working. The no-linker
    // case is about to fail loudly anyway, so the extra line costs nothing
    // where it is not wanted.
    if store.is_some() && key.is_none() {
        eprintln!(
            "khora: the toolchain could not be identified, so this build is not cached"
        );
    }

    if let (true, Some(store), Some(key)) = (cache::Cache::explaining(), &store, &key) {
        let _ = store;
        eprintln!("khora: cache key {key}");
    }
    if !no_cache {
        if let (Some(store), Some(key)) = (&store, &key) {
            match store.lookup(key) {
                Ok(hit) => match cache::Cache::place(&hit, &target) {
                    Ok(()) => {
                        println!(
                            "reused {} from the cache [{}, {}]",
                            target.display(),
                            &hit.key[..12],
                            profile.name()
                        );
                        if lib {
                            println!("header {}", target.with_extension("h").display());
                        }
                        return Ok(true);
                    }
                    // The entry was there and could not be put where it was
                    // wanted. Falling through to a real build is the answer a
                    // person would give.
                    Err(why) => eprintln!("khora: the cache had this build but {why:#}"),
                },
                Err(miss) => {
                    if cache::Cache::explaining() {
                        eprintln!("khora: cache miss, {miss}");
                        let held = store.keys();
                        eprintln!(
                            "khora: the cache holds {} entr(y/ies): {}",
                            held.len(),
                            held.join(", ")
                        );
                    }
                }
            }
        }
    }

    let outcome = if lib {
        khora_codegen_llvm::compile_library_with(&db, root, &target, profile)
    } else {
        khora_codegen_llvm::compile_with(&db, root, &target, profile)
    };
    match outcome {
        Ok(()) => {
            let what = if lib { "library" } else { "built" };
            println!(
                "{what} {} from {} module(s) [{}]",
                target.display(),
                inputs.len(),
                profile.name()
            );
            if lib {
                println!("header {}", target.with_extension("h").display());
            }
            if let (Some(store), Some(key)) = (&store, &key) {
                let header = lib.then(|| target.with_extension("h"));
                if let Err(why) = store.store(key, &target, header.as_deref()) {
                    eprintln!("khora: the build worked and did not go into the cache: {why:#}");
                }
            }
            Ok(true)
        }
        Err(errors) => {
            report_build_errors(&inputs, &errors);
            Ok(false)
        }
    }
}

/// What a shared library is called here, or an executable's extension.
///
/// Not `std::env::consts::DLL_EXTENSION` alone: a Unix shared object also wants
/// the `lib` prefix, which is what `-lname` looks for.
///
/// Only `build` asks, and `build` needs the backend.
#[cfg(feature = "llvm")]
fn library_extension(lib: bool) -> &'static str {
    if lib { std::env::consts::DLL_EXTENSION } else { std::env::consts::EXE_EXTENSION }
}

/// `khora run`: compile the program and start it.
///
/// **The build goes through `build`, cache and all**, rather than having a
/// compile path of its own. A `run` that could disagree with `build` about
/// what the program is would be the worst kind of convenience, and the cache
/// is what makes the second run fast enough for this to be the command
/// somebody reaches for.
///
/// The program's exit status becomes ours. That is the whole point of a
/// runner: `khora run` in a script has to behave the way running the
/// executable would, including when the program fails.
#[cfg(feature = "llvm")]
fn run_program(path: &Path, release: bool, no_cache: bool, args: &[String]) -> Result<ExitCode> {
    one_program(path, "run")?;
    let target = executable_for(path)?;
    if !build(path, Some(&target), false, release, no_cache)? {
        // `build` has already said what was wrong, at the offending line.
        return Ok(ExitCode::FAILURE);
    }

    // Separated from the program's own output, because the next thing on the
    // terminal belongs to the program and not to the toolchain.
    println!("running {}\n", target.display());
    // Flushed before handing the terminal over: the child writes to the same
    // stdout, and a buffered line of ours arriving after its first line is
    // the kind of interleaving nobody can debug.
    use std::io::Write;
    let _ = std::io::stdout().flush();

    let status = std::process::Command::new(&target)
        .args(args)
        .status()
        .with_context(|| format!("running {}", target.display()))?;

    match status.code() {
        Some(code) => Ok(ExitCode::from(u8::try_from(code.rem_euclid(256)).unwrap_or(1))),
        // Killed by a signal, which has no exit code to forward. 1 rather
        // than 0, because the program did not finish.
        None => {
            eprintln!("khora: {} did not exit normally", target.display());
            Ok(ExitCode::FAILURE)
        }
    }
}

/// Refuses a workspace root where exactly one program is meant.
///
/// **Picking a member for somebody is how they end up running the wrong one
/// for ten minutes.** `khora build` at this repository's root used to choose
/// whichever member's source happened to contain `fn main(` first, which was
/// `bench/floor`, silently -- and then print a path deep inside a directory
/// nobody had named. `khora check` and `khora fmt` fan out because doing all
/// of them is a sensible reading of "check the workspace"; building all of
/// them into one executable is not a reading of anything.
#[cfg(feature = "llvm")]
fn one_program(path: &Path, verb: &str) -> Result<()> {
    let Some(members) = workspace_members(std::slice::from_ref(&path.to_path_buf())) else {
        return Ok(());
    };
    let names: Vec<String> = members.iter().map(|m| m.display().to_string()).collect();
    anyhow::bail!(
        "this is a workspace root, so there is no one program to {verb}. Name a member: {}",
        names.join(", ")
    )
}

/// Where `khora run` puts the executable it is about to start.
///
/// The same place `khora build` would, so the two share a cache entry and an
/// output rather than each having their own idea of where a program lives.
#[cfg(feature = "llvm")]
fn executable_for(path: &Path) -> Result<PathBuf> {
    let files = collect_sources(std::slice::from_ref(&path.to_path_buf()))?;
    let entry = files
        .iter()
        .find(|file| read(file).is_ok_and(|text| text.contains("fn main(")))
        .or_else(|| files.first())
        .with_context(|| format!("no `.kh` files under {}", path.display()))?;
    let stem = entry.file_stem().unwrap_or_default();
    Ok(entry.with_file_name(stem).with_extension(library_extension(false)))
}

/// `khora run`, in a build that cannot compile anything.
#[cfg(not(feature = "llvm"))]
fn run_program(
    _path: &Path,
    _release: bool,
    _no_cache: bool,
    _args: &[String],
) -> Result<ExitCode> {
    anyhow::bail!(
        "this `khora` was built without the LLVM backend, so there is nothing to run. \
         Rebuild with `--features llvm`; see docs/llvm-setup.md."
    )
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
/// A span is only meaningful against the file it came from, and these errors
/// do not carry one — so the first source is used and the count is printed
/// either way.
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
fn build(
    _path: &Path,
    _out: Option<&Path>,
    _lib: bool,
    _release: bool,
    _no_cache: bool,
) -> Result<bool> {
    anyhow::bail!(
        "this `khora` was built without the LLVM backend. \
         Rebuild with `--features llvm`; see docs/llvm-setup.md."
    )
}

/// `khora cache`, in a build that has never produced an artifact to cache.
#[cfg(not(feature = "llvm"))]
fn cache_command(_clear: bool) -> Result<()> {
    anyhow::bail!(
        "this `khora` was built without the LLVM backend, so it has never built anything \
         to cache. Rebuild with `--features llvm`; see docs/llvm-setup.md."
    )
}

/// `khora cache`: what is in it, or nothing in it.
///
/// **No eviction policy, deliberately.** A cache that decides for itself what
/// to throw away needs a rule -- least recently used, a size budget -- and a
/// wrong rule is a cache that evicts the entry somebody was about to hit.
/// `--clear` is the whole of the management story until somebody's disk says
/// otherwise, and the numbers this prints are how they will know.
#[cfg(feature = "llvm")]
fn cache_command(clear: bool) -> Result<()> {
    let store = cache::Cache::open()?;
    if clear {
        let (entries, bytes) = store.size();
        store.clear()?;
        println!("cleared {entries} entr(y/ies), {} back", human(bytes));
        return Ok(());
    }
    let (entries, bytes) = store.size();
    println!("{}", store.root().display());
    println!("{entries} entr(y/ies), {}", human(bytes));
    if entries > 0 {
        println!("\n`khora cache --clear` empties it.");
    }
    Ok(())
}

/// A byte count somebody can read at a glance.
#[cfg(feature = "llvm")]
fn human(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KB", "MB", "GB"];
    let mut size = bytes as f64;
    let mut unit = 0;
    while size >= 1024.0 && unit + 1 < UNITS.len() {
        size /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{size:.1} {}", UNITS[unit])
    }
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
///    resolved relative to that manifest. A `version` entry would need a
///    registry, which 13.13 decided against for now.
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
            if let Some(package) = enclosing_package(root) {
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
    let parsed = match khora_manifest::Manifest::load(&manifest_path) {
        Ok(parsed) => parsed,
        // A manifest that is wrong *at a place* -- syntax, or a key holding
        // the wrong kind of value -- carries a line and a column, and is
        // `khora check` on the manifest's to report. Two commands reporting
        // the same error differently is worse than one reporting it.
        Err(why) if why.location().is_some() => return Ok(Vec::new()),
        // Everything else has no line to point at because it is not about a
        // line: a `workspace = true` with no root, a member the root does not
        // list. **Nothing else will say so**, and the alternative is what this
        // used to do -- return no dependencies and let the compiler report
        // fifteen type errors in a program that was fine.
        Err(why) => anyhow::bail!("{why}"),
    };

    let store = khora_pkg::Store::open()?;
    let resolution = khora_pkg::resolve(&manifest_path, &store, locked_requested())?;

    check_extern_allowlist(&parsed.manifest.permissions, &resolution)?;

    // A workspace has one lockfile, at the root. One left behind in a member
    // is not read any more, and a lockfile that silently stopped being read is
    // the kind of thing somebody finds out about during an incident. Said
    // rather than deleted: removing a committed file is the reader's call.
    for stray in &resolution.stray_locks {
        eprintln!(
            "khora: {} is no longer read -- the workspace root holds the only lockfile \
             now. Delete it.",
            stray.display()
        );
    }

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

/// `khora sbom` — a bill of materials for what a package actually builds
/// against.
///
/// Rendered from the *resolution* rather than from a `khora.lock` read off
/// disk, so the document describes what a build here would use rather than
/// what a lockfile last recorded. Those differ exactly when the lockfile is
/// stale, which is the case an audit most wants not to be misled about; pass
/// `--locked` to refuse that difference instead of absorbing it.
///
/// A package with no dependencies still gets a document. An empty bill of
/// materials is a fact about the package, and a tool that produces nothing
/// cannot be told apart from one that failed.
fn sbom(path: &Path, out: Option<&Path>) -> Result<()> {
    let manifest_path = nearest_manifest(path).with_context(|| {
        format!(
            "no `khora.toml` in {} or any directory above it, and a bill of materials is              about a package",
            path.display()
        )
    })?;
    let parsed =
        khora_manifest::Manifest::load(&manifest_path).map_err(|e| anyhow::anyhow!("{e}"))?;

    let store = khora_pkg::Store::open()?;
    let resolution = khora_pkg::resolve(&manifest_path, &store, locked_requested())?;

    // An SBOM names the thing it describes, so a workspace root has nothing to
    // put at the top of one. Refused rather than filled in with the directory
    // name: a bill of materials is a document somebody may hand to an auditor,
    // and a made-up subject is worse than no document.
    let subject = parsed.manifest.package().ok_or_else(|| {
        anyhow::anyhow!(
            "{} is a workspace root rather than a package, so there is nothing for an SBOM \
             to be *about*. Run this in a member directory.",
            manifest_path.display()
        )
    })?;
    let document = khora_pkg::cyclonedx(&resolution.lockfile, &subject.name, &subject.version);
    match out {
        Some(file) => std::fs::write(file, document)
            .with_context(|| format!("writing {}", file.display())),
        None => {
            print!("{document}");
            Ok(())
        }
    }
}

/// A page per module, out of the comments already in the source.
///
/// **The output directory is owned by this command.** Everything it produces is
/// a pure function of the input -- no timestamp, no version, no path from this
/// machine -- and anything under `out` that this run did not produce is
/// deleted. Both halves are needed for the same reason: a generated tree is
/// only reviewable if regenerating it after no change produces no diff, and a
/// page for a module somebody deleted is worse than no page at all.
fn doc(paths: &[PathBuf], out: &Path, check: bool) -> Result<bool> {
    let files = documentable(paths)?;
    if files.is_empty() {
        anyhow::bail!("no `.kh` files found");
    }

    let mut read = Vec::new();
    for path in &files {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("reading {}", path.display()))?;
        let parsed = khora_syntax::parse(&text);
        if !parsed.ok() {
            anyhow::bail!(
                "{} does not parse, so there is nothing to document. Run `khora check` on it",
                path.display()
            );
        }
        read.push(khora_doc::module_of(&parsed.source_file()));
    }

    let mut pages: BTreeMap<PathBuf, String> = BTreeMap::new();
    let mut undocumented: Vec<String> = Vec::new();
    for module in khora_doc::merge(read) {
        let Some(module_path) = module.path.clone() else { continue };
        if module.doc.is_empty() {
            undocumented.push(module_path.clone());
        }
        // `std::net::socket` becomes `net/socket.md`, so the sidebar nests the
        // way the module path does. The leading segment is the package and is
        // already the name of the directory this writes into.
        let mut segments: Vec<&str> = module_path.split("::").collect();
        if segments.len() > 1 {
            segments.remove(0);
        }
        let mut file = out.to_path_buf();
        for segment in &segments[..segments.len() - 1] {
            file.push(segment);
        }
        file.push(format!("{}.md", segments[segments.len() - 1]));
        pages.insert(file, khora_doc::markdown(&module));
    }

    let stale = existing_pages(out).into_iter().filter(|p| !pages.contains_key(p));
    let mut changed: Vec<String> = Vec::new();
    for path in stale {
        changed.push(format!("  delete {}", path.display()));
        if !check {
            let _ = std::fs::remove_file(&path);
        }
    }
    for (path, page) in &pages {
        let before = std::fs::read_to_string(path).ok();
        // **Compared with the line endings normalised.** Pages are written
        // with `\n`; a checkout on Windows with `core.autocrlf` set hands them
        // back with `\r\n`, and a byte comparison then says every page is
        // stale for ever. Found by the gate failing on all fifteen pages
        // immediately after a rebase, with `git diff` showing nothing --
        // which is exactly the way a gate stops being believed.
        if before.as_deref().map(unified) == Some(unified(page)) {
            continue;
        }
        changed.push(format!(
            "  {} {}",
            if before.is_some() { "update" } else { "write " },
            path.display()
        ));
        if !check {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)
                    .with_context(|| format!("creating {}", parent.display()))?;
            }
            std::fs::write(path, page)
                .with_context(|| format!("writing {}", path.display()))?;
        }
    }

    if !check {
        prune_empty(out);
    }

    for module in &undocumented {
        eprintln!("warning: `{module}` has no `//!` block, so its page has no introduction");
    }

    if check {
        if changed.is_empty() {
            println!("{} page(s) up to date", pages.len());
            return Ok(true);
        }
        println!("{} page(s) out of date:", changed.len());
        for line in &changed {
            println!("{line}");
        }
        println!("Run `khora doc` and commit the result.");
        return Ok(false);
    }

    println!("{} page(s) in {}", pages.len(), out.display());
    Ok(true)
}

/// The `.kh` files to document, which is not the same set a build would use.
///
/// Two differences from [`collect_sources`], and both matter.
///
/// **Every platform's files, not this machine's.** A build reads
/// `socket_windows.kh` and never opens `socket_linux.kh`; documentation has to
/// read all three, or the published reference for `std::net::socket` is
/// whichever platform the person who ran the command happened to be on, and
/// `--check` fails in CI for no reason anybody can see. [`khora_doc::merge`]
/// puts the variants back together.
///
/// **No `std` and no dependencies.** A build needs them to resolve a name;
/// documentation of `packages/postgres` that also emitted fifteen pages of
/// `std` would be documenting something nobody asked about.
fn documentable(paths: &[PathBuf]) -> Result<Vec<PathBuf>> {
    let roots: Vec<PathBuf> =
        if paths.is_empty() { vec![PathBuf::from(".")] } else { paths.to_vec() };
    let mut out = Vec::new();
    for root in &roots {
        if root.is_dir() {
            every_source(root, &mut out)?;
        } else {
            out.push(root.clone());
        }
    }
    // Sorted, because `merge` settles a disagreement between two files of one
    // module by taking the earlier, and "earlier" has to mean something stable.
    out.sort();
    out.dedup();
    Ok(out)
}

fn every_source(dir: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
    for entry in std::fs::read_dir(dir).with_context(|| format!("reading {}", dir.display()))? {
        let path = entry?.path();
        if path.is_dir() {
            if path.file_name().is_some_and(|n| n == "target" || n == ".git") {
                continue;
            }
            every_source(&path, out)?;
        } else if path.extension().is_some_and(|e| e == "kh") {
            out.push(path);
        }
    }
    Ok(())
}

/// The same text whatever a checkout did to its line endings.
fn unified(text: &str) -> String {
    text.replace("\r\n", "\n")
}

/// Every `.md` already under `out`, so the ones this run did not write can go.
fn existing_pages(out: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut stack = vec![out.to_path_buf()];
    while let Some(here) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&here) else { continue };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|e| e == "md") {
                found.push(path);
            }
        }
    }
    found.sort();
    found
}

/// Removes directories left behind by a deleted module.
fn prune_empty(out: &Path) {
    let Ok(entries) = std::fs::read_dir(out) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            prune_empty(&path);
            let _ = std::fs::remove_dir(&path);
        }
    }
}

/// `khora install [url]` -- add a dependency, or fetch the declared ones.
///
/// Two commands sharing a name, and they belong together: both end at "the
/// lockfile now matches the manifest, and everything it names is on this
/// machine". With a URL the manifest gains a line first.
///
/// A bare name is refused rather than guessed at. There is no registry, so
/// there is nothing that could turn `postgres` into an address, and inventing
/// a table of well-known names here would be a registry with none of the parts
/// that make one trustworthy.
fn install(url: Option<&str>, rev: &str, subdir: Option<&str>, path: &Path) -> Result<()> {
    let manifest_path = nearest_manifest(path).with_context(|| {
        format!(
            "no `khora.toml` in {} or any directory above it, and installing is about a \
             project",
            path.display()
        )
    })?;
    let store = khora_pkg::Store::open()?;

    if let Some(url) = url {
        if !looks_like_a_url(url) {
            anyhow::bail!(
                "`{url}` is not a URL, and there is no registry to look a name up in yet.\n\
                 Install by address instead:\n    \
                 khora install https://example.com/some/repo.git\n\
                 and if the package sits inside the repository rather than at its root, \
                 add `--subdir <directory>`"
            );
        }
        let done = khora_pkg::install(&manifest_path, url, rev, subdir, &store)?;
        let verb = done.outcome.verb();
        println!(
            "{verb} {} {} in {}",
            done.name,
            done.version,
            manifest_path.display()
        );
        println!("  {url} at {}", done.revision);
        println!("  import {}::...", done.name);
    }

    let resolution = khora_pkg::resolve(&manifest_path, &store, locked_requested())?;
    let count = resolution.packages.len();
    println!(
        "{count} {} resolved",
        if count == 1 { "package" } else { "packages" }
    );
    Ok(())
}

/// Whether an argument is an address rather than a bare name.
///
/// Deliberately loose. This only decides which of two error messages a person
/// gets; git is the one that says whether an address works.
fn looks_like_a_url(argument: &str) -> bool {
    argument.contains("://")
        || argument.starts_with("git@")
        || argument.starts_with('.')
        || argument.starts_with('/')
}

/// The directory of the package `start` is part of.
///
/// **The nearest manifest is not necessarily a package.** A workspace root has
/// no `[package]`, and treating its directory as one made `khora check` on a
/// single file compile every member of the workspace: the file's *package* was
/// the whole repository. So a manifest without a `[package]` is walked past,
/// exactly as a directory without a manifest is.
fn enclosing_package(start: &Path) -> Option<PathBuf> {
    let mut here = start.parent();
    while let Some(directory) = here {
        let candidate = directory.join("khora.toml");
        if candidate.is_file() {
            let declares_package = khora_manifest::Manifest::load(&candidate)
                .is_ok_and(|parsed| parsed.manifest.package.is_some());
            if declares_package {
                // An empty path is the manifest in the working directory,
                // which is `.` rather than nowhere.
                return Some(if directory.as_os_str().is_empty() {
                    PathBuf::from(".")
                } else {
                    directory.to_path_buf()
                });
            }
        }
        here = directory.parent();
    }
    None
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
