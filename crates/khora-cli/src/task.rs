//! `khora task <name>`.
//!
//! The `[tasks]` table has been parsed since the manifest existed, and
//! `khora_pkg::tasks::plan` has ordered it and refused cycles for just as
//! long. Nothing ran it. Roadmap 14.18.
//!
//! **This was `khora run` for about a day.** `run` is what every other
//! toolchain calls "build this program and start it", and a language that
//! spent it on a task runner would be surprising in the one command a
//! newcomer types first. Renamed while the cost was one commit.
//!
//! # What a task is allowed to be
//!
//! A `run` line, handed to the platform shell. `docs/project.md` §4.1 replaces
//! arbitrary build-time host code with sandboxed WASM plugins, and it is worth
//! being exact about why that argument does not reach here: it is about a
//! *dependency* running code on your machine because you fetched it. A task
//! runs only when somebody types its name in a manifest they are standing in.
//! Resolution does not reach it, building does not reach it, and a
//! dependency's `[tasks]` table is never read at all — `khora_pkg::resolve`
//! looks at `[dependencies]` and nothing else.
//!
//! The command goes to `cmd /C` on Windows and `sh -c` elsewhere, so a task
//! meant to be portable should invoke `khora` rather than shell built-ins.
//!
//! # What runs, given a name
//!
//! Three clauses, in order:
//!
//! 1. A declared task with a `run` runs it.
//! 2. A task with no `run` whose name is one of the toolchain's verbs — the
//!    `BUILT_IN` list — runs that verb. `[tasks.test] depends_on = ["lint"]`
//!    means "lint first, then test", not "instead of test".
//!
//!    `lint` runs `khora check`, because that is where the lints run:
//!    `khora_lint::findings` is called from the check, and there is no
//!    `khora lint`. Printed as the substitution it is rather than done
//!    quietly. `fmt` *formats* — a task wanting the check writes
//!    `run = "khora fmt . --check"`, because a verb that does not do what its
//!    name says is worse than one that surprises you into a diff.
//! 3. Anything else is a grouping and runs nothing of its own. `ci` exists to
//!    depend on three other things.
//!
//! # Across a workspace
//!
//! At a root, the task runs in every member that has something to run for it,
//! and **in dependency order**: a member that another member depends on goes
//! first. That order is not guessed — the resolver already reports which
//! directories each member compiles, so a member whose directory is among
//! another's dependencies is one that has to go first.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context, Result};
use khora_manifest::{Manifest, Task};

/// Runs `goal` in the package or workspace at `path`.
pub fn run(path: &Path, goal: &str, members: Option<&[PathBuf]>) -> Result<bool> {
    // **A task the root itself declares is a workspace task**, and runs once,
    // at the root. `ci` at the top of a monorepo means "run the pipeline", not
    // "run something called ci in each of eight members" -- and a `[tasks]`
    // table in a root manifest that was silently never read would be one more
    // piece of configuration that parses and does nothing.
    if members.is_some() {
        let own = tasks_of(path)?;
        if own.contains_key(goal) {
            return one(path, &own, goal);
        }
    }

    match members {
        Some(members) => {
            let ordered = in_dependency_order(members);
            let mut ran = 0usize;
            for member in &ordered {
                let tasks = tasks_of(member)?;
                if !has_work(&tasks, goal) {
                    continue;
                }
                println!("== run {goal} in {}", member.display());
                ran += 1;
                if !one(member, &tasks, goal)? {
                    println!("\n`{goal}` failed in {}", member.display());
                    return Ok(false);
                }
            }
            if ran == 0 {
                // Not an error, and not silence either. A workspace command
                // that did nothing has to say so, or a green tick means
                // "nothing to do" and "everything passed" interchangeably.
                bail!(
                    "no member has anything to run for `{goal}`. Declare it under `[tasks]` \
                     in the members it belongs to"
                );
            }
            println!("\n`{goal}` ran in {ran} member(s)");
            Ok(true)
        }
        None => {
            let tasks = tasks_of(path)?;
            one(path, &tasks, goal)
        }
    }
}

/// Prints the tasks a manifest declares, with the built-ins after them.
pub fn list(path: &Path) -> Result<()> {
    let tasks = tasks_of(path)?;
    if tasks.is_empty() {
        println!("no `[tasks]` in {}", path.display());
    } else {
        let width = tasks.keys().map(String::len).max().unwrap_or(0);
        for (name, task) in &tasks {
            match &task.description {
                Some(what) => println!("  {name:<width$}  {what}"),
                None => println!("  {name}"),
            }
        }
    }
    println!("\nalways available: {}", khora_pkg::tasks::BUILT_IN.join(", "));
    Ok(())
}

/// Runs `goal` and everything it needs, in one package.
fn one(directory: &Path, tasks: &BTreeMap<String, Task>, goal: &str) -> Result<bool> {
    for name in khora_pkg::tasks::plan(tasks, goal)? {
        match step(tasks, &name) {
            Step::Shell(command) => {
                println!("$ {command}");
                if !shell(directory, &command)? {
                    return Ok(false);
                }
            }
            Step::BuiltIn(verb) => {
                if verb == name {
                    println!("$ khora {verb}");
                } else {
                    println!("$ khora {verb}   (`{name}` runs inside it)");
                }
                if !khora(directory, &verb)? {
                    return Ok(false);
                }
            }
            // A grouping. Reported, because a task that ran nothing and said
            // nothing is one somebody will assume did something.
            Step::Grouping => println!("- {name} (nothing of its own to run)"),
        }
    }
    Ok(true)
}

/// What running one name amounts to.
enum Step {
    Shell(String),
    BuiltIn(String),
    Grouping,
}

fn step(tasks: &BTreeMap<String, Task>, name: &str) -> Step {
    if let Some(command) = tasks.get(name).and_then(|task| task.run.clone()) {
        return Step::Shell(command);
    }
    match verb_for(name) {
        Some(verb) => Step::BuiltIn(verb.to_string()),
        None => Step::Grouping,
    }
}

/// The toolchain verb a built-in task name runs.
///
/// `lint` is the one that is not itself: the lints run inside `khora check`
/// and there is no `khora lint`. `docs/project.md` §4.1's own example has
/// `depends_on = ["lint", "test", "build"]`, so the name has to mean
/// something, and "the lints pass" is what it means.
fn verb_for(name: &str) -> Option<&'static str> {
    match name {
        "build" => Some("build"),
        "check" => Some("check"),
        "fmt" => Some("fmt"),
        "test" => Some("test"),
        "lint" => Some("check"),
        _ => None,
    }
}

/// Whether `goal` would run anything at all here.
///
/// A member with no `[tasks]` and a goal that is not a built-in has nothing to
/// contribute, and running the plan there would fail with "no task called" for
/// a member the person was not asking about.
fn has_work(tasks: &BTreeMap<String, Task>, goal: &str) -> bool {
    tasks.contains_key(goal) || khora_pkg::tasks::BUILT_IN.contains(&goal)
}

/// The `[tasks]` table of the manifest in `directory`.
fn tasks_of(directory: &Path) -> Result<BTreeMap<String, Task>> {
    let manifest = directory.join("khora.toml");
    if !manifest.is_file() {
        return Ok(BTreeMap::new());
    }
    let parsed = Manifest::load(&manifest).map_err(|why| anyhow::anyhow!("{why}"))?;
    Ok(parsed.manifest.tasks)
}

/// Runs `command` through the platform shell, in `directory`.
fn shell(directory: &Path, command: &str) -> Result<bool> {
    let mut process = if cfg!(windows) {
        let mut c = Command::new("cmd");
        c.arg("/C").arg(command);
        c
    } else {
        let mut c = Command::new("sh");
        c.arg("-c").arg(command);
        c
    };
    let status = process
        .current_dir(directory)
        .status()
        .with_context(|| format!("running `{command}`"))?;
    Ok(status.success())
}

/// Runs one of the toolchain's own verbs, as this very binary.
///
/// `current_exe` rather than the name `khora`, because the one on the `PATH`
/// may be a different version, or may not be there at all -- somebody running
/// a freshly built compiler out of `target/debug` should get that one.
fn khora(directory: &Path, verb: &str) -> Result<bool> {
    let exe = std::env::current_exe().context("finding this executable")?;
    let status = Command::new(exe)
        .arg(verb)
        .arg(".")
        .current_dir(directory)
        .status()
        .with_context(|| format!("running `khora {verb}`"))?;
    Ok(status.success())
}

/// Members sorted so that a member another one depends on comes first.
///
/// **Not guessed.** A member whose directory is among another member's
/// resolved dependency directories is one that has to go first, and the
/// resolver reports those because a build needs them. A cycle -- which the
/// resolver would have refused anyway -- falls back to the workspace's own
/// order rather than looping.
fn in_dependency_order(members: &[PathBuf]) -> Vec<PathBuf> {
    let store = khora_pkg::Store::open().ok();
    let mut needs: Vec<(PathBuf, BTreeSet<PathBuf>)> = Vec::new();
    for member in members {
        let mut directories = BTreeSet::new();
        if let Some(store) = &store {
            if let Ok(resolution) = khora_pkg::resolve(&member.join("khora.toml"), store, false) {
                for directory in resolution.directories() {
                    directories.insert(canonical(&directory));
                }
            }
        }
        needs.push((member.clone(), directories));
    }

    let mut ordered: Vec<PathBuf> = Vec::new();
    let mut placed: BTreeSet<PathBuf> = BTreeSet::new();
    // Repeated passes rather than a topological sort with its own bookkeeping:
    // a workspace has tens of members, and a pass that places nothing means a
    // cycle, which is the only case that needs saying anything about.
    while ordered.len() < needs.len() {
        let before = ordered.len();
        for (member, directories) in &needs {
            let key = canonical(member);
            if placed.contains(&key) {
                continue;
            }
            let waiting = needs.iter().any(|(other, _)| {
                let other_key = canonical(other);
                other_key != key && !placed.contains(&other_key) && directories.contains(&other_key)
            });
            if !waiting {
                ordered.push(member.clone());
                placed.insert(key);
            }
        }
        if ordered.len() == before {
            for (member, _) in &needs {
                if !placed.contains(&canonical(member)) {
                    ordered.push(member.clone());
                }
            }
            break;
        }
    }
    ordered
}

fn canonical(path: &Path) -> PathBuf {
    path.canonicalize().map(khora_manifest::readable).unwrap_or_else(|_| path.to_path_buf())
}
