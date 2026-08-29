//! `khora new`, `khora why`, `khora graph`.
//!
//! Three small commands, each the answer to a question people ask constantly
//! in a monorepo and currently answer by reading files. Roadmap 14.21.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use khora_manifest::Manifest;

/// Scaffolds a package at `directory`.
///
/// **The manifest it writes depends on where it lands.** Inside a workspace
/// that supplies a shared version, the new member says `version.workspace =
/// true`, because a scaffold that writes `version = "0.1.0"` into a monorepo
/// where every other member inherits one has quietly created the drift the
/// inheritance existed to prevent.
pub fn new(directory: &Path, library: bool) -> Result<()> {
    if directory.exists() && std::fs::read_dir(directory).is_ok_and(|mut d| d.next().is_some()) {
        bail!("{} already exists and is not empty", directory.display());
    }
    let name = directory
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .filter(|n| !n.is_empty() && n != ".")
        .with_context(|| format!("{} does not name a package", directory.display()))?;
    if !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        bail!(
            "`{name}` is not a package name: a module path is made of identifiers, so letters, \
             digits and underscores. Rename the directory, or make one that is named for the \
             package"
        );
    }

    std::fs::create_dir_all(directory.join("src"))
        .with_context(|| format!("creating {}", directory.display()))?;
    // An empty manifest first, so that the membership question below has
    // something to answer. `packages/*` matches directories *with* a
    // `khora.toml`, deliberately -- see `khora_manifest::workspace` -- so a
    // directory that does not have one yet is not a member yet either.
    std::fs::write(directory.join("khora.toml"), "")
        .with_context(|| format!("creating {}", directory.join("khora.toml").display()))?;

    let shared = shared_fields(directory);
    let mut manifest = format!("[package]\nname = \"{name}\"\n");
    manifest.push_str(if shared.version { "version.workspace = true\n" } else { "version = \"0.1.0\"\n" });
    if shared.edition {
        manifest.push_str("edition.workspace = true\n");
    }
    if library {
        manifest.push_str(
            "# Offered for other people to depend on. Absent means no.\npublish = true\n",
        );
    }
    if shared.fmt {
        manifest.push_str("\n[fmt]\nworkspace = true\n");
    }
    if shared.lints {
        manifest.push_str("\n[lints]\nworkspace = true\n");
    }
    std::fs::write(directory.join("khora.toml"), manifest)
        .with_context(|| format!("writing {}", directory.join("khora.toml").display()))?;

    let (file, source) = if library {
        (
            "lib.kh",
            format!("module {name}::lib;\n\n/// What this package offers.\npub fn hello() -> String {{\n  \"hello from {name}\"\n}}\n"),
        )
    } else {
        (
            "main.kh",
            format!("module {name}::main;\n\npub fn main() -> Int {{\n  0\n}}\n"),
        )
    };
    std::fs::write(directory.join("src").join(file), source)
        .with_context(|| format!("writing src/{file}"))?;

    println!("created {} ({})", directory.display(), if library { "library" } else { "program" });
    println!("  khora.toml");
    println!("  src/{file}");
    // **A note about what listing buys, not a warning that something is
    // wrong.** The old wording -- "does not list this directory. Add it to
    // `members`." -- read as an error, and the first thing a newcomer did was
    // go and edit a file. Nothing was broken: `khora build`, `khora check`,
    // `khora test` and `khora run` all take a path and all work on a package
    // the workspace has never heard of. What membership changes is whether the
    // *workspace-wide* commands sweep it up, which is a choice rather than a
    // repair, so the line says which choice it is.
    match enclosing_root(directory) {
        Some(root) if !lists(&root, directory) => {
            println!(
                "\nThis package works as it is. Add it to `members` in {} to have \n\
                 the workspace's own commands include it.",
                root.join("khora.toml").display()
            );
        }
        Some(_) => println!("\nAlready covered by the workspace's `members`."),
        None => {}
    }
    Ok(())
}

/// Which fields the enclosing workspace root offers to share.
struct Shared {
    version: bool,
    edition: bool,
    fmt: bool,
    lints: bool,
}

fn shared_fields(directory: &Path) -> Shared {
    let none = Shared { version: false, edition: false, fmt: false, lints: false };
    let Some(root) = enclosing_root(directory) else { return none };
    let Ok(parsed) = Manifest::load(&root.join("khora.toml")) else { return none };
    let Some(table) = parsed.manifest.workspace else { return none };
    // Only offered if the directory really is a member: a scaffold that writes
    // `version.workspace = true` somewhere the root does not list produces a
    // manifest that does not load.
    if !lists(&root, directory) {
        return none;
    }
    let package = table.package.unwrap_or_default();
    Shared {
        version: package.version.is_some(),
        edition: package.edition.is_some(),
        fmt: table.fmt.is_some(),
        lints: !table.lints.is_empty(),
    }
}

/// Explains why `name` is in the build.
///
/// A chain from something the workspace declares down to the package asked
/// about, because "`postgres` is here" is not the question — "who wants it" is.
pub fn why(path: &Path, name: &str) -> Result<()> {
    let store = khora_pkg::Store::open()?;
    let manifest = manifest_of(path)?;
    let resolution = khora_pkg::resolve(&manifest, &store, false)?;

    let mut askers: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for package in &resolution.packages {
        askers
            .insert(package.name.as_str(), package.requested_by.iter().map(String::as_str).collect());
    }

    let Some(target) = resolution.packages.iter().find(|p| p.name == name) else {
        let known: Vec<&str> = resolution.packages.iter().map(|p| p.name.as_str()).collect();
        if known.is_empty() {
            bail!("nothing in this build depends on anything, so `{name}` is not here either");
        }
        bail!("`{name}` is not in this build. What is: {}", known.join(", "));
    };

    println!("{} ({})", target.name, target.source);
    // Every path back, breadth-first, so the shortest explanation comes first.
    // A package reached three ways has three reasons and printing one of them
    // is how somebody removes a dependency and finds it still there.
    let mut printed = BTreeSet::new();
    let mut queue: Vec<Vec<&str>> = vec![vec![name]];
    while let Some(chain) = queue.pop() {
        let here = *chain.last().expect("a non-empty chain");
        match askers.get(here) {
            Some(holders) if !holders.is_empty() => {
                for holder in holders {
                    if chain.contains(holder) {
                        continue;
                    }
                    let mut longer = chain.clone();
                    longer.push(holder);
                    queue.push(longer);
                }
            }
            // Nobody in `packages` asked for it, so whoever did is a member or
            // the root: the end of the chain.
            _ => {
                let mut readable: Vec<&str> = chain.clone();
                readable.reverse();
                let line = readable.join(" -> ");
                if printed.insert(line.clone()) {
                    println!("  {line}");
                }
            }
        }
    }
    Ok(())
}

/// Draws the member graph.
pub fn graph(path: &Path, members: Option<&[PathBuf]>, dot: bool) -> Result<()> {
    let store = khora_pkg::Store::open()?;
    let subjects: Vec<PathBuf> = match members {
        Some(members) => members.to_vec(),
        None => vec![path.to_path_buf()],
    };

    let mut rows: Vec<(String, Vec<String>)> = Vec::new();
    for subject in &subjects {
        let manifest = subject.join("khora.toml");
        if !manifest.is_file() {
            continue;
        }
        let name = match Manifest::load(&manifest) {
            Ok(parsed) => parsed
                .manifest
                .package()
                .map(|p| p.name.clone())
                .unwrap_or_else(|| subject.display().to_string()),
            Err(why) => bail!("{why}"),
        };
        let resolution = khora_pkg::resolve(&manifest, &store, false)?;
        // Direct dependencies only. The transitive set is what `khora why`
        // answers one package at a time; a graph that drew every edge would be
        // unreadable at exactly the size where somebody needs one.
        let direct: Vec<String> = resolution
            .packages
            .iter()
            .filter(|package| package.requested_by.contains(&name))
            .map(|package| package.name.clone())
            .collect();
        rows.push((name, direct));
    }

    if dot {
        println!("digraph khora {{");
        println!("  rankdir=LR;");
        for (name, direct) in &rows {
            println!("  {:?};", name);
            for to in direct {
                println!("  {:?} -> {:?};", name, to);
            }
        }
        println!("}}");
        return Ok(());
    }

    for (name, direct) in &rows {
        println!("{name}");
        for (at, to) in direct.iter().enumerate() {
            let last = at + 1 == direct.len();
            println!("{} {to}", if last { "└──" } else { "├──" });
        }
    }
    if rows.iter().all(|(_, direct)| direct.is_empty()) {
        println!("\nnothing depends on anything yet");
    }
    Ok(())
}

/// The manifest governing `path`.
fn manifest_of(path: &Path) -> Result<PathBuf> {
    let candidate = if path.is_dir() { path.join("khora.toml") } else { path.to_path_buf() };
    if candidate.is_file() {
        return Ok(candidate);
    }
    bail!("no `khora.toml` at {}", path.display())
}

/// The nearest workspace root above `directory`, including one at it.
fn enclosing_root(directory: &Path) -> Option<PathBuf> {
    khora_manifest::enclosing(directory).map(|found| found.root)
}

fn lists(root: &Path, directory: &Path) -> bool {
    khora_manifest::read_workspace(&root.join("khora.toml"))
        .ok()
        .flatten()
        .is_some_and(|found| found.members.iter().any(|member| same(member, directory)))
}

fn same(left: &Path, right: &Path) -> bool {
    match (left.canonicalize(), right.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => left == right,
    }
}
