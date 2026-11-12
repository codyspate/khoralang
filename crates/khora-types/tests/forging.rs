//! A dependency cannot make a real capability for itself.
//!
//! **What these guard: a function whose signature says it touches nothing,
//! in a package somebody depends on, reading the clock or the file system
//! anyway.** `Clock::real()` builds the handler out of nothing, so on its own
//! the row checking every other test here relies on cannot see it. Each
//! program below is compiled with `std` and a package `app` that depends on a
//! package `dep`, the way `khora check` hands them to the checker: the
//! resolver's list of the dependency's files is declared, and the module paths
//! are not trusted.

use std::path::{Path, PathBuf};

use khora_db::{DependencyFiles, KhoraDatabase, SourceFile, SourceRoot};

fn std_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..").join("std")
}

fn sources(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).expect("the source directory should exist").flatten() {
        let path = entry.path();
        if path.is_dir() {
            sources(&path, out);
        } else if path.extension().is_some_and(|e| e == "kh")
            && khora_db::selected_for_target(&path, khora_db::host_target())
        {
            out.push(path);
        }
    }
}

/// How the driver describes the program.
#[derive(Clone, Copy)]
enum Driver {
    /// As `khora check` does: the dependency's files are declared.
    Resolved,
    /// As a test harness that resolved nothing does: no declaration, so the
    /// checker reads which packages are roots from the imports.
    Unresolved,
}

/// The diagnostics of `app.kh` and `dep.kh`, compiled together with `std`.
fn diagnostics(app: &str, dep: &str, driver: Driver) -> (Vec<String>, Vec<String>) {
    let mut paths = Vec::new();
    sources(&std_dir(), &mut paths);
    paths.sort();

    let db = KhoraDatabase::new();
    let mut files: Vec<SourceFile> = paths
        .iter()
        .map(|p| SourceFile::new(&db, p.clone(), std::fs::read_to_string(p).expect("readable")))
        .collect();
    let app = SourceFile::new(&db, PathBuf::from("app/src/main.kh"), app.to_string());
    let dep = SourceFile::new(&db, PathBuf::from("dep/src/lib.kh"), dep.to_string());
    files.push(app);
    files.push(dep);
    SourceRoot::new(&db, files);
    match driver {
        Driver::Resolved => {
            DependencyFiles::new(&db, vec![dep]);
        }
        Driver::Unresolved => {}
    }
    let messages = |f: SourceFile| -> Vec<String> {
        khora_types::diagnostics(&db, f).iter().map(|e| e.message.clone()).collect()
    };
    (messages(app), messages(dep))
}

/// A root that calls `dep::lib::sneak` and does nothing else.
const CALLER: &str = "module app::main;

import std::core::{print};
import dep::lib::{sneak};

pub fn main() -> () {
  print(\"${sneak()}\");
}
";

/// Requires `dep` to be refused for forging `effect`, and `app` to be clean.
fn refused(dep: &str, effect: &str) {
    for driver in [Driver::Resolved, Driver::Unresolved] {
        let (app, dep_errors) = diagnostics(CALLER, dep, driver);
        assert!(app.is_empty(), "the root did nothing wrong: {app:#?}");
        assert!(
            dep_errors.iter().any(|m| m.contains(&format!("makes a real `{effect}`"))
                && m.contains("only the package being built")
                && m.contains("`with {")),
            "expected `{effect}` refused in the dependency, with the `with` advice; got {dep_errors:#?}"
        );
    }
}

/// A dependency module that builds `effect` with `made` and does nothing with
/// it, imported from `module`.
fn forging(module: &str, effect: &str, made: &str) -> String {
    format!(
        "module dep::lib;

import {module}::{{{effect}}};

pub fn sneak() -> Int {{
  let _forged: {effect} = {made};
  1
}}
"
    )
}

#[test]
fn a_dependency_cannot_make_any_capability() {
    for c in khora_types::CONSTRUCTORS {
        for function in c.functions {
            let args = match *function {
                "real" => "",
                "within" | "bounded" => "1000",
                "bounded_within" => "1000, 1000",
                other => panic!("no arguments written for `{}::{other}`", c.effect),
            };
            refused(&forging(c.module, c.effect, &format!("{}::{function}({args})", c.effect)), c.effect);
        }
    }
}

/// The program the agent-choice round found: a capability made and used in a
/// function with no row.
#[test]
fn a_dependency_cannot_read_the_clock_behind_its_signature() {
    refused(
        "module dep::lib;

import std::clock::{Clock};

pub fn sneak() -> Int {
  with { clock: Clock::real() } { clock.unix_millis() }
}
",
        "Clock",
    );
}

/// **Any module of the package being built**, not only the one with `main`.
#[test]
fn the_package_being_built_may_make_one_in_any_module() {
    let app = "module app::helpers;

import std::clock::{Clock};
import std::fs::{FsRead};

pub fn now() -> Int {
  with { clock: Clock::real() } { clock.unix_millis() }
}

pub fn reads() -> FsRead { FsRead::real() }
";
    let dep = "module dep::lib;\n\npub fn sneak() -> Int { 1 }\n";
    for driver in [Driver::Resolved, Driver::Unresolved] {
        let (app_errors, dep_errors) = diagnostics(app, dep, driver);
        assert!(app_errors.is_empty(), "{app_errors:#?}");
        assert!(dep_errors.is_empty(), "{dep_errors:#?}");
    }
}

/// **A dependency's own tests may forge.** They run when the dependency is
/// the package being tested, never as part of this build.
#[test]
fn a_dependencys_test_may_make_one() {
    let dep = "module dep::lib;

import std::core::{assert};
import std::clock::{Clock};

pub fn sneak() -> Int { 1 }

test \"the clock moves\" {
  let now = with { clock: Clock::real() } { clock.unix_millis() };
  assert(now > 0);
}
";
    let (app, dep_errors) = diagnostics(CALLER, dep, Driver::Resolved);
    assert!(app.is_empty() && dep_errors.is_empty(), "{app:#?} {dep_errors:#?}");
}

// --- routes that reach `real()` without spelling it there -----------------

#[test]
fn an_alias_is_still_the_capability() {
    refused(
        "module dep::lib;

import std::clock::{Clock as C};

pub fn sneak() -> Int {
  let _forged: C = C::real();
  1
}
",
        "Clock",
    );
}

/// `let f = Clock::real; f()`: the name is refused where it is written,
/// called or not.
#[test]
fn a_function_value_is_still_the_capability() {
    refused(
        "module dep::lib;

import std::clock::{Clock};

pub fn sneak() -> Int {
  let f = Clock::real;
  with { clock: f() } { clock.unix_millis() }
}
",
        "Clock",
    );
}

/// A trait the dependency writes, implemented for `Clock` and called through
/// a generic function: the impl is the dependency's code, and it is where
/// `Clock::real` is written.
#[test]
fn a_trait_impl_is_still_the_dependencys_code() {
    refused(
        "module dep::lib;

import std::clock::{Clock};

pub trait Make {
  fn make() -> Self;
}

impl Make for Clock {
  fn make() -> Clock { Clock::real() }
}

pub fn build<C: Make>() -> C { C::make() }

pub fn sneak() -> Int {
  let c: Clock = build();
  with { clock: c } { clock.unix_millis() }
}
",
        "Clock",
    );
}

/// `C::real()` on a bare type parameter does not resolve at all: a generic
/// function cannot reach an effect's inherent functions through a parameter,
/// so a caller passing `Clock` gets nothing.
#[test]
fn a_type_parameter_cannot_reach_real() {
    let dep = "module dep::lib;

import std::clock::{Clock};

pub fn make<C>() -> C { C::real() }

pub fn sneak() -> Int {
  let c: Clock = make();
  with { clock: c } { clock.unix_millis() }
}
";
    let (_, dep_errors) = diagnostics(CALLER, dep, Driver::Resolved);
    assert!(
        dep_errors.iter().any(|m| m.contains("`C` is not a trait with a function named `real`")),
        "{dep_errors:#?}"
    );
}

/// **A capability made by the package being built and passed down is the
/// design working**, not a route around it: the dependency's signature says
/// it takes one.
#[test]
fn a_capability_passed_down_is_allowed() {
    let app = "module app::main;

import std::core::{print};
import std::clock::{Clock};
import dep::lib::{stamped};

pub fn main() -> () {
  with { clock: Clock::real() } { print(\"${stamped()}\") }
}
";
    let dep = "module dep::lib;

import std::clock::{Clock};

pub fn stamped() -> Int with { clock: Clock } { clock.unix_millis() }
";
    let (app_errors, dep_errors) = diagnostics(app, dep, Driver::Resolved);
    assert!(app_errors.is_empty() && dep_errors.is_empty(), "{app_errors:#?} {dep_errors:#?}");
}

/// **A dependency's own type that happens to be called `Clock` is not the
/// capability.** The rule follows the declaration, not the spelling.
#[test]
fn a_namesake_type_is_not_the_capability() {
    let dep = "module dep::lib;

type Clock = { n: Int };

impl Clock {
  fn real() -> Clock { { n: 7 } }
}

pub fn sneak() -> Int { Clock::real().n }
";
    let (_, dep_errors) = diagnostics(CALLER, dep, Driver::Resolved);
    assert!(dep_errors.is_empty(), "{dep_errors:#?}");
}

/// There is no re-export: `pub import` does not parse, so a dependency's
/// module cannot hand on `std`'s `real` under a name of its own. What it can
/// hand on is a function of its own that calls `real`, which is refused at
/// that call.
#[test]
fn there_is_no_re_export_to_route_through() {
    let parse = khora_syntax::parse("module dep::lib;\n\npub import std::clock::{Clock};\n");
    assert!(!parse.errors().is_empty(), "`pub import` parsed; this test's premise is gone");
    refused(
        "module dep::lib;

import std::clock::{Clock};

pub fn a_clock() -> Clock { Clock::real() }

pub fn sneak() -> Int { 1 }
",
        "Clock",
    );
}

// --- who counts as the package being built ---------------------------------

/// **A dependency named `std` is still a dependency.** The resolver accepts
/// the name and the files say `module std::..`; the declaration of which files
/// are a dependency's is what decides, not the module path.
#[test]
fn a_dependency_calling_itself_std_is_refused() {
    let mut paths = Vec::new();
    sources(&std_dir(), &mut paths);
    paths.sort();
    let db = KhoraDatabase::new();
    let mut files: Vec<SourceFile> = paths
        .iter()
        .map(|p| SourceFile::new(&db, p.clone(), std::fs::read_to_string(p).expect("readable")))
        .collect();
    let fake = SourceFile::new(
        &db,
        PathBuf::from("fake/src/evil.kh"),
        "module std::evil;

import std::clock::{Clock};

pub fn sneak() -> Int {
  with { clock: Clock::real() } { clock.unix_millis() }
}
"
        .to_string(),
    );
    let app = SourceFile::new(
        &db,
        PathBuf::from("app/src/main.kh"),
        "module app::main;

import std::core::{print};
import std::evil::{sneak};

pub fn main() -> () { print(\"${sneak()}\") }
"
        .to_string(),
    );
    files.push(fake);
    files.push(app);
    SourceRoot::new(&db, files);
    DependencyFiles::new(&db, vec![fake]);
    let said: Vec<String> = khora_types::diagnostics(&db, fake).iter().map(|e| e.message.clone()).collect();
    assert!(said.iter().any(|m| m.contains("makes a real `Clock`")), "{said:#?}");
    assert!(khora_types::diagnostics(&db, app).is_empty());
}

/// Without a declaration, a program of one package -- `std` and a file -- is
/// its own root and may make what it likes: a scratch file checked on its own
/// is the package being built.
#[test]
fn a_lone_program_is_the_package_being_built() {
    let mut paths = Vec::new();
    sources(&std_dir(), &mut paths);
    paths.sort();
    let db = KhoraDatabase::new();
    let mut files: Vec<SourceFile> = paths
        .iter()
        .map(|p| SourceFile::new(&db, p.clone(), std::fs::read_to_string(p).expect("readable")))
        .collect();
    let main = SourceFile::new(
        &db,
        PathBuf::from("scratch.kh"),
        "module scratch;

import std::clock::{Clock};

pub fn main() -> Int { with { clock: Clock::real() } { clock.unix_millis() } }
"
        .to_string(),
    );
    files.push(main);
    SourceRoot::new(&db, files);
    let said: Vec<String> = khora_types::diagnostics(&db, main).iter().map(|e| e.message.clone()).collect();
    assert!(said.is_empty(), "{said:#?}");
}

// --- the table --------------------------------------------------------------

/// **Two capabilities, two labels.** The refusal advises a row, and a
/// function that needs both kinds of randomness writes both rows: an advice
/// that gave `Random` and `SecureRandom` the same label would be advice that
/// cannot be followed twice in one signature.
#[test]
fn every_capability_is_advised_under_a_label_of_its_own() {
    let mut seen: Vec<(&str, &str)> = Vec::new();
    for c in khora_types::CONSTRUCTORS {
        if let Some((other, _)) = seen.iter().find(|(_, label)| *label == c.label) {
            panic!("`{}` and `{other}` are both advised as `{}`", c.effect, c.label);
        }
        seen.push((c.effect, c.label));
    }
}

/// The advice for `SecureRandom` is the label the capabilities page and the
/// `postgres` package use for it.
#[test]
fn secure_random_is_advised_as_nonces() {
    let (_, dep_errors) = diagnostics(CALLER, &forging("std::crypto", "SecureRandom", "SecureRandom::real()"), Driver::Resolved);
    assert!(
        dep_errors.iter().any(|m| m.contains("`with { nonces: SecureRandom }`")),
        "{dep_errors:#?}"
    );
}

/// **A `const` is a body too.** A const is lowered wherever it is mentioned,
/// so one the dependency uses itself is caught at the use. One it only
/// exports is not used anywhere in the dependency: it is lowered in the
/// importer's body, the root's, where `Clock::real` is allowed. Only walking
/// the const's own initializer, in the dependency, refuses it.
#[test]
fn a_const_is_still_the_dependencys_code() {
    let app = "module app::main;

import std::core::{print};
import dep::lib::{MAKE};

pub fn main() -> () {
  with { clock: MAKE() } { print(\"${clock.unix_millis()}\") }
}
";
    let dep = "module dep::lib;

import std::clock::{Clock};

pub const MAKE: () -> Clock = Clock::real;
";
    for driver in [Driver::Resolved, Driver::Unresolved] {
        let (_, dep_errors) = diagnostics(app, dep, driver);
        assert!(dep_errors.iter().any(|m| m.contains("makes a real `Clock`")), "{dep_errors:#?}");
    }
}

/// **A helper used only by tests is not a `test` block.** It is a function
/// like any other, callable from the package's own code, so a dependency's
/// helper that makes a clock is refused even though only a test calls it;
/// the helper takes the capability, and the test makes it.
#[test]
fn a_dependencys_test_helper_outside_a_test_block_is_refused() {
    let dep = "module dep::lib;

import std::core::{assert};
import std::clock::{Clock};

pub fn sneak() -> Int { 1 }

fn real_now() -> Int { with { clock: Clock::real() } { clock.unix_millis() } }

test \"the clock moves\" {
  assert(real_now() > 0);
}
";
    let (_, dep_errors) = diagnostics(CALLER, dep, Driver::Resolved);
    assert!(dep_errors.iter().any(|m| m.contains("makes a real `Clock`")), "{dep_errors:#?}");
}

// --- std's own implementation -----------------------------------------------

/// A use of each [`khora_types::STD_INTERNAL`] module, by an item it has.
fn internal_item(module: &str) -> &'static str {
    match module {
        "std::process::shell" => "run",
        "std::permissions::separator" => "backslash_separates",
        other => panic!("no item written for `{other}`: add one, and a reason it is internal"),
    }
}

fn names_internal(messages: &[String], module: &str) -> bool {
    messages.iter().any(|m| m.contains(&format!("`{module}` is part of `std`'s own implementation")))
}

/// **A shell with no `Process` row.** `std::process::shell` is the half of
/// `std::process` each target writes for itself; importing it skipped both
/// the capability and `[permissions] process`. Refused in the package being
/// built as much as in a dependency: neither is `std`.
#[test]
fn an_internal_std_module_is_refused_everywhere_but_std() {
    for internal in khora_types::STD_INTERNAL {
        let item = internal_item(internal.module);
        let user = |module: &str| format!("module {module};\n\nimport {}::{{{item}}};\n\npub fn sneak() -> Int {{ 1 }}\n", internal.module);
        let (app, _) = diagnostics(&user("app::main"), "module dep::lib;\n\npub fn sneak() -> Int { 1 }\n", Driver::Resolved);
        assert!(names_internal(&app, internal.module), "the root: {app:#?}");
        let (_, dep) = diagnostics(CALLER, &user("dep::lib"), Driver::Resolved);
        assert!(names_internal(&dep, internal.module), "a dependency: {dep:#?}");
        // And with nothing declared, as a harness that resolved nothing
        // compiles: an `app` file is not `std`'s by any reading.
        let (app, _) = diagnostics(&user("app::main"), "module dep::lib;\n\npub fn sneak() -> Int { 1 }\n", Driver::Unresolved);
        assert!(names_internal(&app, internal.module), "the root, unresolved: {app:#?}");
    }
}

/// A qualified call needs no import, if it resolves: today it does not, so the
/// import is the only door, and if it ever does resolve this test asks that
/// the walk over bodies refuse it.
#[test]
fn a_qualified_path_into_an_internal_module_is_refused() {
    let dep = "module dep::lib;

pub fn sneak() -> Int {
  std::process::shell::run(\"echo from the dependency\")
}
";
    let (_, dep_errors) = diagnostics(CALLER, dep, Driver::Resolved);
    let unresolved = dep_errors.iter().any(|m| m.contains("cannot resolve `std::process::shell::run`"));
    assert!(unresolved || names_internal(&dep_errors, "std::process::shell"), "{dep_errors:#?}");
}

/// **`module std::evil;` does not make a file `std`'s.** Which files are the
/// standard library is where they are on disk.
#[test]
fn a_file_calling_itself_std_cannot_name_an_internal_module() {
    let app = "module std::evil;

import std::process::shell::{run};

pub fn sneak() -> Int { run(\"true\") }
";
    let (app_errors, _) = diagnostics(app, "module dep::lib;\n\npub fn sneak() -> Int { 1 }\n", Driver::Resolved);
    assert!(names_internal(&app_errors, "std::process::shell"), "{app_errors:#?}");
}

/// `std::process` imports `std::process::shell`, and that is what the module
/// is for.
#[test]
fn std_itself_may_name_its_internal_modules() {
    let mut paths = Vec::new();
    sources(&std_dir(), &mut paths);
    paths.sort();
    let db = KhoraDatabase::new();
    let files: Vec<SourceFile> = paths
        .iter()
        .map(|p| SourceFile::new(&db, p.clone(), std::fs::read_to_string(p).expect("readable")))
        .collect();
    SourceRoot::new(&db, files.clone());
    DependencyFiles::new(&db, Vec::new());
    let mut users = 0;
    for file in files {
        let text = file.text(&db);
        if khora_types::STD_INTERNAL.iter().any(|i| text.contains(&format!("import {}::", i.module))) {
            users += 1;
        }
        let said: Vec<String> = khora_types::diagnostics(&db, file).iter().map(|e| e.message.clone()).collect();
        assert!(
            !said.iter().any(|m| m.contains("is part of `std`'s own implementation")),
            "{}: {said:#?}",
            file.path(&db).display()
        );
    }
    assert!(users >= 2, "std names its internal modules in {users} file(s); this test reads the wrong std");
}

/// The modules of `std` declared by more than one file, one per target, that
/// a program is meant to use.
const PER_TARGET_PUBLIC: [&str; 1] = [
    // The raw network: what a program, and `postgres`, dials through, under
    // `[permissions] network`.
    "std::net::socket",
];

/// **Every module `std` splits by target is decided**: one target's half of
/// another module is in `STD_INTERNAL`, and a module a program is meant to
/// use is in `PER_TARGET_PUBLIC` with its reason.
#[test]
fn every_per_target_std_module_is_decided() {
    fn all(dir: &Path, out: &mut Vec<PathBuf>) {
        for entry in std::fs::read_dir(dir).expect("std").flatten() {
            let path = entry.path();
            if path.is_dir() {
                all(&path, out);
            } else if path.extension().is_some_and(|e| e == "kh") {
                out.push(path);
            }
        }
    }
    let mut paths = Vec::new();
    all(&std_dir(), &mut paths);
    let mut declared: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    for path in &paths {
        let text = std::fs::read_to_string(path).expect("readable");
        if let Some(module) = text.lines().find_map(|l| l.strip_prefix("module ")) {
            *declared.entry(module.trim_end_matches(';').trim().to_string()).or_default() += 1;
        }
    }
    let split: Vec<&String> = declared.iter().filter(|(_, n)| **n > 1).map(|(m, _)| m).collect();
    assert!(split.len() >= 3, "found only {split:?}; the scan has stopped reading std");
    for module in split {
        let internal = khora_types::STD_INTERNAL.iter().any(|i| i.module == module);
        assert!(
            internal || PER_TARGET_PUBLIC.contains(&module.as_str()),
            "`{module}` is declared once per target and is in neither `STD_INTERNAL` nor \
             `PER_TARGET_PUBLIC`: decide whether a program may import it"
        );
    }
    for internal in khora_types::STD_INTERNAL {
        assert!(declared.contains_key(internal.module), "`{}` is in the table and not in std", internal.module);
    }
}

// --- `Log::json` ---------------------------------------------------------------

/// **A logger stamped from the real clock says it holds one.** `Log::json`
/// reads the wall clock for every line, so a function that builds one
/// needs `clock: Clock`, and a dependency that logs with a real timestamp
/// says so in its signature.
#[test]
fn log_json_needs_the_clock() {
    let dep = "module dep::lib;

import std::log::{Log, Severity, info};

pub fn sneak() -> Int {
  with { log: Log::json(Severity::Info) } { info(\"hello\") };
  1
}
";
    let (_, dep_errors) = diagnostics(CALLER, dep, Driver::Resolved);
    assert!(
        dep_errors.iter().any(|m| m.contains("`clock: Clock`")),
        "`Log::json` with no clock in scope: {dep_errors:#?}"
    );
    let given = "module dep::lib;

import std::clock::{Clock};
import std::log::{Log, Severity, info};

pub fn sneak() -> Int with { clock: Clock } {
  with { log: Log::json(Severity::Info) } { info(\"hello\") };
  1
}
";
    let (_, dep_errors) = diagnostics(CALLER, given, Driver::Resolved);
    assert!(dep_errors.is_empty(), "with the clock passed down: {dep_errors:#?}");
}

// --- the table (constructors) -------------------------------------------------

/// The std constructors deliberately left open: none of them reaches the
/// machine on its own.
const OPEN: [(&str, &str); 7] = [
    // A seeded generator is arithmetic; the seed is the caller's.
    ("Random", "seeded"),
    // A log writes to standard error, which every program may. `json` stamps
    // each line from the clock in its `with` row, so it reads no clock the
    // caller did not hand it.
    ("Log", "none"),
    ("Log", "json"),
    ("Log", "json_using"),
    ("Log", "plain"),
    ("Tracer", "none"),
    // The program's own region, which belongs to the fiber that roots it.
    ("Scope", "root"),
];

/// **Every public function in `std` that builds an effect's handler is
/// decided**: in `CONSTRUCTORS`, or in `OPEN` above with a reason. A new
/// capability whose author forgot the table fails here rather than being
/// open to every dependency by default.
#[test]
fn every_constructor_in_std_is_decided() {
    let mut paths = Vec::new();
    sources(&std_dir(), &mut paths);
    let texts: Vec<String> = paths.iter().map(|p| std::fs::read_to_string(p).expect("readable")).collect();
    let effects: Vec<String> = texts
        .iter()
        .flat_map(|t| t.lines().filter_map(|l| l.strip_prefix("pub effect ")).map(|r| {
            r.split(|c: char| !c.is_alphanumeric() && c != '_').next().unwrap_or_default().to_string()
        }))
        .collect();
    let mut undecided = Vec::new();
    let mut found = 0;
    for text in &texts {
        let mut inside: Option<&str> = None;
        for line in text.lines() {
            if let Some(rest) = line.strip_prefix("impl ") {
                let head = rest.split(|c: char| !c.is_alphanumeric() && c != '_').next().unwrap_or_default();
                inside = (!rest.contains(" for ") && effects.iter().any(|e| e == head)).then_some(head);
                continue;
            }
            let Some(effect) = inside else { continue };
            let Some(rest) = line.trim_start().strip_prefix("pub fn ") else { continue };
            let name = rest.split(|c: char| !c.is_alphanumeric() && c != '_').next().unwrap_or_default();
            if !rest.trim_end().trim_end_matches('{').trim_end().ends_with(&format!("-> {effect}")) {
                continue;
            }
            found += 1;
            let listed = khora_types::CONSTRUCTORS
                .iter()
                .any(|c| c.effect == effect && c.functions.contains(&name));
            if !listed && !OPEN.contains(&(effect, name)) {
                undecided.push(format!("{effect}::{name}"));
            }
        }
    }
    assert!(found >= 15, "the scan found only {found} constructors; it has stopped reading std");
    assert!(
        undecided.is_empty(),
        "these build an effect's handler and are in neither `khora_types::CONSTRUCTORS` nor \
         this test's `OPEN`: {undecided:?}"
    );
    // And the table names nothing std does not have.
    for c in khora_types::CONSTRUCTORS {
        for f in c.functions {
            assert!(
                texts.iter().any(|t| t.contains(&format!("pub fn {f}(")) && t.contains(&format!("impl {} {{", c.effect))),
                "`{}::{f}` is in the table and not in std",
                c.effect
            );
        }
    }
}
