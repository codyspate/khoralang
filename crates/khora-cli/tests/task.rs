//! `khora task <name>`.
//!
//! The `[tasks]` DAG was parsed, ordered and cycle-checked for a long time
//! before anything ran it, so these tests are about the running: what a name
//! resolves to, what order members go in, and what a run that did nothing
//! says. Roadmap 14.18.

use std::path::{Path, PathBuf};
use std::process::Command;

/// A workspace root with `packages/*` as its members.
fn workspace(name: &str) -> PathBuf {
    let root = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("run").join(name);
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).expect("a scratch directory");
    std::fs::write(root.join("khora.toml"), "[workspace]\nmembers = [\"packages/*\"]\n")
        .expect("the root manifest");
    root
}

/// A package with a manifest body of the caller's choosing.
fn package(at: &Path, name: &str, extra: &str) {
    std::fs::create_dir_all(at.join("src")).expect("a package directory");
    std::fs::write(
        at.join("khora.toml"),
        format!(
            "[package]\nname = \"{name}\"\nversion = \"0.1.0\"\npublish = true\n{extra}"
        ),
    )
    .expect("a manifest");
    std::fs::write(
        at.join("src").join("lib.kh"),
        format!("module {name}::lib;\n\npub fn go() -> Int {{\n  1\n}}\n"),
    )
    .expect("a module");
}

fn run(at: &Path, args: &[&str]) -> (bool, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_khora"))
        .args(args)
        .current_dir(at)
        .output()
        .expect("could not run `khora`");
    let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&out.stderr));
    (out.status.success(), text)
}

/// An `echo` that works on both shells this runs through.
fn echo(what: &str) -> String {
    format!("echo {what}")
}

#[test]
fn a_task_runs_its_command() {
    let root = workspace("one_task");
    let member = root.join("packages").join("alpha");
    package(&member, "alpha", &format!("\n[tasks.greet]\nrun = \"{}\"\n", echo("marker-one")));

    let (ok, output) = run(&member, &["task", "greet"]);
    assert!(ok, "{output}");
    assert!(output.contains("marker-one"), "the command's own output: {output}");
}

#[test]
fn a_dependency_runs_first() {
    let root = workspace("order");
    let member = root.join("packages").join("alpha");
    package(
        &member,
        "alpha",
        &format!(
            "\n[tasks.first]\nrun = \"{}\"\n\n[tasks.second]\nrun = \"{}\"\ndepends_on = [\"first\"]\n",
            echo("marker-first"),
            echo("marker-second")
        ),
    );

    let (ok, output) = run(&member, &["task", "second"]);
    assert!(ok, "{output}");
    let first = output.find("marker-first").expect("the dependency ran");
    let second = output.find("marker-second").expect("the goal ran");
    assert!(first < second, "the dependency should run first:\n{output}");
}

#[test]
fn a_failing_step_stops_the_run() {
    let root = workspace("failing");
    let member = root.join("packages").join("alpha");
    package(
        &member,
        "alpha",
        &format!(
            "\n[tasks.bad]\nrun = \"exit 3\"\n\n[tasks.after]\nrun = \"{}\"\ndepends_on = [\"bad\"]\n",
            echo("marker-after")
        ),
    );

    let (ok, output) = run(&member, &["task", "after"]);
    assert!(!ok, "a failing step should fail the run: {output}");
    assert!(!output.contains("marker-after"), "the later step ran anyway: {output}");
}

#[test]
fn a_grouping_task_says_it_ran_nothing() {
    // Otherwise a task that exists only to depend on other things looks, from
    // the output, like a task that did something.
    let root = workspace("grouping");
    let member = root.join("packages").join("alpha");
    package(
        &member,
        "alpha",
        &format!("\n[tasks.one]\nrun = \"{}\"\n\n[tasks.all]\ndepends_on = [\"one\"]\n", echo("m")),
    );

    let (ok, output) = run(&member, &["task", "all"]);
    assert!(ok, "{output}");
    assert!(output.contains("nothing of its own to run"), "{output}");
}

#[test]
fn a_built_in_name_runs_the_toolchains_own_verb() {
    let root = workspace("built_in");
    let member = root.join("packages").join("alpha");
    package(&member, "alpha", "\n[tasks.ci]\ndepends_on = [\"check\"]\n");

    let (ok, output) = run(&member, &["task", "ci"]);
    assert!(ok, "{output}");
    assert!(output.contains("no errors"), "`khora check` should have run: {output}");
}

#[test]
fn lint_runs_the_check_and_says_that_is_what_it_did() {
    // There is no `khora lint` -- the lints run inside the check -- and
    // §4.1's own example depends on `lint`, so the substitution has to happen
    // and has to be visible.
    let root = workspace("lint");
    let member = root.join("packages").join("alpha");
    package(&member, "alpha", "");

    let (ok, output) = run(&member, &["task", "lint"]);
    assert!(ok, "{output}");
    assert!(output.contains("`lint` runs inside it"), "{output}");
}

#[test]
fn an_unknown_task_names_what_there_is() {
    let root = workspace("unknown");
    let member = root.join("packages").join("alpha");
    package(&member, "alpha", "\n[tasks.greet]\nrun = \"echo hi\"\n");

    let (ok, output) = run(&member, &["task", "nonsense"]);
    assert!(!ok, "{output}");
    assert!(output.contains("no task called `nonsense`"), "{output}");
}

#[test]
fn a_cycle_is_named_rather_than_recursed_into() {
    let root = workspace("cycle");
    let member = root.join("packages").join("alpha");
    package(
        &member,
        "alpha",
        "\n[tasks.a]\ndepends_on = [\"b\"]\n\n[tasks.b]\ndepends_on = [\"a\"]\n",
    );

    let (ok, output) = run(&member, &["task", "a"]);
    assert!(!ok, "{output}");
    assert!(output.contains("depends on itself"), "{output}");
    assert!(output.contains("->"), "the loop should be drawn: {output}");
}

#[test]
fn a_workspace_runs_the_task_in_dependency_order() {
    // `beta` depends on `alpha`, so `alpha` goes first -- and that is not
    // alphabetical luck, because the next test reverses it.
    let root = workspace("ws_order");
    package(&root.join("packages").join("alpha"), "alpha", &format!("\n[tasks.g]\nrun = \"{}\"\n", echo("marker-alpha")));
    package(
        &root.join("packages").join("beta"),
        "beta",
        &format!(
            "\n[dependencies]\nalpha = {{ path = \"../alpha\" }}\n\n[tasks.g]\nrun = \"{}\"\n",
            echo("marker-beta")
        ),
    );

    let (ok, output) = run(&root, &["task", "g"]);
    assert!(ok, "{output}");
    let alpha = output.find("marker-alpha").expect("alpha ran");
    let beta = output.find("marker-beta").expect("beta ran");
    assert!(alpha < beta, "alpha is depended on and should go first:\n{output}");
    assert!(output.contains("ran in 2 member(s)"), "{output}");
}

#[test]
fn the_order_follows_the_graph_and_not_the_alphabet() {
    let root = workspace("ws_reverse");
    package(
        &root.join("packages").join("alpha"),
        "alpha",
        &format!(
            "\n[dependencies]\nbeta = {{ path = \"../beta\" }}\n\n[tasks.g]\nrun = \"{}\"\n",
            echo("marker-alpha")
        ),
    );
    package(&root.join("packages").join("beta"), "beta", &format!("\n[tasks.g]\nrun = \"{}\"\n", echo("marker-beta")));

    let (ok, output) = run(&root, &["task", "g"]);
    assert!(ok, "{output}");
    let alpha = output.find("marker-alpha").expect("alpha ran");
    let beta = output.find("marker-beta").expect("beta ran");
    assert!(beta < alpha, "beta is depended on now and should go first:\n{output}");
}

#[test]
fn a_goal_no_member_has_is_an_error_rather_than_a_green_tick() {
    let root = workspace("nothing_to_do");
    package(&root.join("packages").join("alpha"), "alpha", "");

    let (ok, output) = run(&root, &["task", "deploy"]);
    assert!(!ok, "a run that did nothing should not look like a pass: {output}");
    assert!(output.contains("no member has anything to run for `deploy`"), "{output}");
}

#[test]
fn listing_shows_descriptions_and_the_built_ins() {
    let root = workspace("listing");
    let member = root.join("packages").join("alpha");
    package(
        &member,
        "alpha",
        "\n[tasks.greet]\ndescription = \"say hello\"\nrun = \"echo hi\"\n",
    );

    let (ok, output) = run(&member, &["task"]);
    assert!(ok, "{output}");
    assert!(output.contains("greet"), "{output}");
    assert!(output.contains("say hello"), "{output}");
    assert!(output.contains("always available"), "{output}");
}

#[test]
fn a_task_the_root_declares_runs_once_at_the_root() {
    // `ci` at the top of a monorepo means "run the pipeline", not "run
    // something called ci in each of eight members".
    let root = workspace("root_task");
    std::fs::write(
        root.join("khora.toml"),
        format!(
            "[workspace]{n}members = [{q}packages/*{q}]{n}{n}[tasks.ci]{n}run = {q}{cmd}{q}{n}",
            n = "
",
            q = '"',
            cmd = echo("marker-root")
        ),
    )
    .expect("the root manifest");
    package(
        &root.join("packages").join("alpha"),
        "alpha",
        &format!("{n}[tasks.ci]{n}run = {q}{cmd}{q}{n}", n = "
", q = '"', cmd = echo("marker-member")),
    );

    let (ok, output) = run(&root, &["task", "ci"]);
    assert!(ok, "{output}");
    assert!(output.contains("marker-root"), "{output}");
    assert!(!output.contains("marker-member"), "the root task should not fan out: {output}");
}
