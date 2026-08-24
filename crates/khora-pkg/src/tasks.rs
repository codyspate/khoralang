//! The `[tasks]` DAG.
//!
//! A task names the tasks that must finish before it. This works out the order,
//! and refuses a cycle by naming it rather than recursing until the stack runs
//! out.
//!
//! # Built-ins
//!
//! `depends_on` may name a task the manifest does not declare — §4.1's own
//! example depends on `lint`, `test` and `build`, none of which it declares —
//! because those are built into the toolchain. So an unknown name is only an
//! error if it is neither declared nor built in, and the message says which
//! built-ins exist rather than only that the name is unknown.

use std::collections::{BTreeMap, BTreeSet};

use anyhow::{bail, Result};
use khora_manifest::Task;

/// Tasks the toolchain provides, which a manifest may depend on without
/// declaring.
pub const BUILT_IN: &[&str] = &["build", "check", "fmt", "lint", "test"];

/// The order to run `goal` and everything it needs.
///
/// Depth-first post-order, so a task appears after everything it depends on.
/// Ties are broken by name, so the same manifest always produces the same plan
/// — a task runner whose order varies is one whose failures are irreproducible.
pub fn plan(tasks: &BTreeMap<String, Task>, goal: &str) -> Result<Vec<String>> {
    if !tasks.contains_key(goal) && !BUILT_IN.contains(&goal) {
        bail!("no task called `{goal}`{}", suggestions(tasks));
    }

    let mut order = Vec::new();
    let mut done = BTreeSet::new();
    let mut path = Vec::new();
    visit(tasks, goal, &mut order, &mut done, &mut path)?;
    Ok(order)
}

fn visit(
    tasks: &BTreeMap<String, Task>,
    name: &str,
    order: &mut Vec<String>,
    done: &mut BTreeSet<String>,
    path: &mut Vec<String>,
) -> Result<()> {
    if done.contains(name) {
        return Ok(());
    }
    if let Some(at) = path.iter().position(|p| p == name) {
        // Naming the loop, not just its existence: `a -> b -> c -> a` is
        // actionable and "there is a cycle" is not.
        let mut loop_ = path[at..].to_vec();
        loop_.push(name.to_string());
        bail!("`{}` depends on itself: {}", name, loop_.join(" -> "));
    }

    path.push(name.to_string());
    if let Some(task) = tasks.get(name) {
        let mut needs = task.depends_on.clone();
        needs.sort();
        for need in needs {
            if !tasks.contains_key(&need) && !BUILT_IN.contains(&need.as_str()) {
                bail!(
                    "`{name}` depends on `{need}`, which is neither declared in this \
                     manifest nor built in. The built-in tasks are {}",
                    BUILT_IN.join(", ")
                );
            }
            visit(tasks, &need, order, done, path)?;
        }
    }
    path.pop();

    done.insert(name.to_string());
    order.push(name.to_string());
    Ok(())
}

fn suggestions(tasks: &BTreeMap<String, Task>) -> String {
    let mut names: Vec<&str> = tasks.keys().map(String::as_str).collect();
    names.extend(BUILT_IN);
    names.sort();
    names.dedup();
    if names.is_empty() {
        String::new()
    } else {
        format!(". There is {}", names.join(", "))
    }
}
