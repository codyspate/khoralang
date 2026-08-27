//! The `[tasks]` DAG.

use std::collections::BTreeMap;

use khora_manifest::Task;
use khora_pkg::tasks;

fn tasks_from(pairs: &[(&str, &[&str])]) -> BTreeMap<String, Task> {
    pairs
        .iter()
        .map(|(name, needs)| {
            (
                (*name).to_string(),
                Task {
                    description: None,
                    run: None,
                    depends_on: needs.iter().map(|n| (*n).to_string()).collect(),
                },
            )
        })
        .collect()
}

#[test]
fn a_task_runs_after_what_it_depends_on() {
    let tasks = tasks_from(&[("ci", &["lint", "test"]), ("test", &["build"])]);
    let plan = tasks::plan(&tasks, "ci").expect("a plan");

    let at = |name: &str| plan.iter().position(|t| t == name).expect("in the plan");
    assert!(at("build") < at("test"), "{plan:?}");
    assert!(at("test") < at("ci"), "{plan:?}");
    assert!(at("lint") < at("ci"), "{plan:?}");
}

/// A diamond is the case where a naive walk runs something twice.
#[test]
fn a_shared_dependency_runs_once() {
    let tasks = tasks_from(&[
        ("release", &["package", "sign"]),
        ("package", &["build"]),
        ("sign", &["build"]),
    ]);
    let plan = tasks::plan(&tasks, "release").expect("a plan");
    assert_eq!(plan.iter().filter(|t| *t == "build").count(), 1, "{plan:?}");
}

/// Naming the loop rather than only reporting that there is one: `a -> b -> a`
/// is actionable and "there is a cycle" is not.
#[test]
fn a_cycle_is_named() {
    let tasks = tasks_from(&[("a", &["b"]), ("b", &["c"]), ("c", &["a"])]);
    let error = tasks::plan(&tasks, "a").expect_err("should refuse");
    let message = format!("{error:#}");
    assert!(message.contains("a -> b -> c -> a"), "unexpected message: {message}");
}

#[test]
fn a_task_depending_on_itself_is_a_cycle() {
    let tasks = tasks_from(&[("a", &["a"])]);
    assert!(tasks::plan(&tasks, "a").is_err());
}

/// §4.1's own example depends on `lint`, `test` and `build` without declaring
/// them, because those are built in.
#[test]
fn a_built_in_need_not_be_declared() {
    let tasks = tasks_from(&[("ci", &["lint", "test", "build"])]);
    let plan = tasks::plan(&tasks, "ci").expect("a plan");
    assert_eq!(plan.last().map(String::as_str), Some("ci"), "{plan:?}");
}

#[test]
fn an_unknown_dependency_lists_the_built_ins() {
    let tasks = tasks_from(&[("ci", &["deploy"])]);
    let message = format!("{:#}", tasks::plan(&tasks, "ci").expect_err("should refuse"));
    assert!(message.contains("`deploy`"), "{message}");
    assert!(message.contains("built in"), "{message}");
}

#[test]
fn an_unknown_goal_says_what_there_is() {
    let tasks = tasks_from(&[("ci", &[])]);
    let message = format!("{:#}", tasks::plan(&tasks, "nope").expect_err("should refuse"));
    assert!(message.contains("no task called `nope`"), "{message}");
    assert!(message.contains("ci"), "the message should list what exists: {message}");
}

/// A task runner whose order varies is one whose failures are irreproducible.
#[test]
fn the_order_is_the_same_every_time() {
    let tasks = tasks_from(&[("ci", &["z", "a", "m"]), ("a", &[]), ("m", &[]), ("z", &[])]);
    let first = tasks::plan(&tasks, "ci").expect("a plan");
    for _ in 0..8 {
        assert_eq!(first, tasks::plan(&tasks, "ci").expect("a plan"));
    }
}
