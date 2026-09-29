//! `method-call`: `x.m(a)` reported, and rewritten to `T::m(x, a)` where the
//! rewrite is the same call.
//!
//! Every fix meets the checks `tests/idiomatic.rs` holds the group's other
//! fixes to: the result parses, type-checks, and a second pass changes
//! nothing. That the two programs print the same, receiver first, is
//! `khora-cli/tests/fix.rs`, which needs a backend.

use khora_db::{KhoraDatabase, SourceFile};
use khora_lint::idiomatic::{apply, Fix};
use khora_lint::{findings, Finding, METHOD_CALL};

/// A module holding the declarations the cases call through, and `body`.
fn module(body: &str) -> String {
    format!(
        "module m;\n\n\
         pub trait Named {{ fn name(self) -> String; }}\n\
         pub trait Titled {{ fn name(self) -> String; }}\n\n\
         pub type Box = {{ v: Int, twice: (Int) -> Int }};\n\
         pub type Cat = {{ age: Int }};\n\
         pub type Maybe<A> = | Some(v: A) | None;\n\
         type E = | Bad;\n\n\
         impl Named for Box {{ fn name(self) -> String {{ \"box\" }} }}\n\
         impl Titled for Cat {{ fn name(self) -> String {{ \"cat\" }} }}\n\
         impl Named for Int {{ fn name(self) -> String {{ \"int\" }} }}\n\n\
         impl Box {{\n\
         \x20 pub fn add(self, a: Int, b: Int) -> Int {{ self.v + a + b }}\n\
         \x20 pub fn me(self) -> Box {{ self }}\n\
         \x20 pub fn inc(self, a: Int) -> Int {{ a + 1 }}\n\
         \x20 pub fn older(self, by: Int, loud: Bool) -> Int {{ by }}\n\
         }}\n\n\
         impl<A> Maybe<A> {{\n\
         \x20 pub fn empty(self) -> Bool {{ true }}\n\
         \x20 pub fn Some(self) -> Int {{ 1 }}\n\
         }}\n\n\
         fn make() -> Box raises E {{ {{ v: 1, twice: fn x => x }} }}\n\n\
         {body}\n"
    )
}

fn lint(text: &str) -> Vec<Finding> {
    let db = KhoraDatabase::new();
    let file = SourceFile::new(&db, "a.kh".into(), text.to_string());
    findings(&db, file).iter().filter(|f| f.lint == METHOD_CALL).cloned().collect()
}

fn checks(text: &str) {
    let db = KhoraDatabase::new();
    let file = SourceFile::new(&db, "a.kh".into(), text.to_string());
    let errors = khora_types::diagnostics(&db, file);
    assert!(errors.is_empty(), "does not type-check:\n{text}\n{errors:?}");
}

/// Every `method-call` fix, pass after pass as `khora check --fix` makes them,
/// checked after each; then nothing is left to fix.
fn fixed(text: &str) -> String {
    checks(text);
    let mut now = text.to_string();
    for _ in 0..10 {
        let found = lint(&now);
        let fixes: Vec<&Fix> = found.iter().filter_map(|f| f.fix.as_ref()).collect();
        if fixes.is_empty() {
            break;
        }
        now = apply(&now, &fixes).0;
        assert!(khora_syntax::parse(&now).errors().is_empty(), "does not parse:\n{now}");
        checks(&now);
    }
    assert!(lint(&now).iter().all(|f| f.fix.is_none()), "a fix is left after ten passes:\n{now}");
    now
}

#[test]
fn a_type_s_own_method_is_called_through_the_type() {
    let text = module("fn f(b: Box) -> Int { b.add(1, 2) }");
    assert_eq!(lint(&text).len(), 1);
    assert!(fixed(&text).contains("{ Box::add(b, 1, 2) }"), "{}", fixed(&text));
}

/// `a.f().g()`: each link is a finding, and the fix nests them.
#[test]
fn a_chain_nests() {
    let text = module("fn f(b: Box) -> Int { b.me().me().inc(1) }");
    assert_eq!(lint(&text).len(), 3);
    assert!(fixed(&text).contains("{ Box::inc(Box::me(Box::me(b)), 1) }"), "{}", fixed(&text));
}

/// A receiver that is a call, one marked `!`, and one in brackets: the
/// receiver moves into the argument list as written.
#[test]
fn a_receiver_that_is_a_call_or_an_exit_moves_whole() {
    let text = module(
        "fn f() -> Int raises E { make()!.add(1, 2) }\n\
         fn g() -> String { (1 + 2).name() }",
    );
    let out = fixed(&text);
    assert!(out.contains("{ Box::add(make()!, 1, 2) }"), "{out}");
    assert!(out.contains("{ Named::name(1 + 2) }"), "a bracket an argument does not need is dropped:\n{out}");
}

/// Two traits in scope declare `name`; each call is qualified by the trait
/// its receiver's type implements.
#[test]
fn two_traits_with_one_method_name_each_get_their_own() {
    let text = module("fn f(b: Box, c: Cat) -> String { b.name() + c.name() }");
    let out = fixed(&text);
    assert!(out.contains("Named::name(b) + Titled::name(c)"), "{out}");
}

/// A receiver whose type is a type parameter has no `T::` to write; the
/// method is the bound's, so the trait is written.
#[test]
fn a_generic_receiver_is_called_through_its_bound() {
    let text = module("fn f<A: Named>(x: A) -> String { x.name() }");
    assert!(fixed(&text).contains("{ Named::name(x) }"), "{}", fixed(&text));
}

/// A type parameter named like the trait makes `Named::` mean the parameter,
/// so the fix is withheld; the finding stays.
#[test]
fn an_owner_a_type_parameter_shadows_gets_no_fix() {
    let text = module("fn f<Named>(b: Box, x: Named) -> String { b.name() }");
    checks(&text);
    let found = lint(&text);
    assert_eq!(found.len(), 1);
    assert!(found[0].fix.is_none(), "{found:?}");
}

/// A built-in type's method, reached through a trait impl.
#[test]
fn a_built_in_receiver_is_called_through_the_trait() {
    let text = module("fn f(n: Int) -> String { n.name() }");
    assert!(fixed(&text).contains("{ Named::name(n) }"), "{}", fixed(&text));
}

/// `b.twice(4)` where `twice` is a field holding a function calls the
/// field, which the checker prefers over a method; it is not a method call.
#[test]
fn a_field_holding_a_function_is_not_a_method_call() {
    let text = module("fn f(b: Box) -> Int { b.twice(4) }");
    checks(&text);
    assert!(lint(&text).is_empty(), "{:?}", lint(&text));
}

/// A label names a parameter by position after the receiver, and keeps
/// naming it once the receiver is written as parameter 1.
#[test]
fn a_label_keeps_its_parameter() {
    let text = module("fn f(b: Box) -> Int { b.older(3, loud: true) }");
    assert!(fixed(&text).contains("{ Box::older(b, 3, loud: true) }"), "{}", fixed(&text));
}

/// The piped value keeps its slot: an explicit `_` stays, and a stage with
/// none gets one where the value went, right after the receiver.
#[test]
fn a_piped_value_keeps_its_slot() {
    let text = module(
        "fn f(b: Box) -> Int { 1 |> b.add(2) }\n\
         fn g(b: Box) -> Int { 1 |> b.add(2, _) }\n\
         fn h(b: Box) -> Int { 1 |> b.inc }\n\
         fn k(b: Box) -> Int { 1 |> b.inc |> b.inc }",
    );
    let out = fixed(&text);
    for expected in [
        "{ 1 |> Box::add(b, _, 2) }",
        "{ 1 |> Box::add(b, 2, _) }",
        "{ 1 |> Box::inc(b, _) }",
        "{ 1 |> Box::inc(b, _) |> Box::inc(b, _) }",
    ] {
        assert!(out.contains(expected), "expected `{expected}` in:\n{out}");
    }
}

/// A receiver whose type was never settled -- `Maybe::None` has no `A` --
/// keeps its finding and gets no fix: nothing is fixed on a guess.
#[test]
fn a_receiver_whose_type_is_open_gets_no_fix() {
    let text = module("fn f() -> Bool { Maybe::None.empty() }");
    checks(&text);
    let found = lint(&text);
    assert_eq!(found.len(), 1);
    assert!(found[0].fix.is_none(), "{found:?}");
}

/// `Maybe::Some` names the constructor, so a method called `Some` cannot be
/// written through the type.
#[test]
fn a_method_named_like_a_constructor_gets_no_fix() {
    let text = module("fn f() -> Int { Maybe::Some(3).Some() }");
    checks(&text);
    let found = lint(&text);
    assert_eq!(found.len(), 1);
    assert!(found[0].fix.is_none(), "{found:?}");
}

/// A comment between the receiver and the call would have nowhere to go.
#[test]
fn a_comment_inside_the_callee_gets_no_fix() {
    let text = module("fn f(b: Box) -> Int {\n  b // the box\n    .add(1, 2)\n}");
    checks(&text);
    let found = lint(&text);
    assert_eq!(found.len(), 1);
    assert!(found[0].fix.is_none(), "{found:?}");
}

/// Arguments laid out one per line keep their layout, with the receiver on
/// a line of its own.
#[test]
fn arguments_on_their_own_lines_stay_there() {
    let text = module("fn f(b: Box) -> Int {\n  b.add(\n    1,\n    2,\n  )\n}");
    assert!(fixed(&text).contains("  Box::add(\n    b,\n    1,\n    2,\n  )\n"), "{}", fixed(&text));
}

/// A file that does not parse gets nothing from this lint.
#[test]
fn a_file_that_does_not_parse_is_left_alone() {
    let text = module("fn f(b: Box) -> Int { b.add(1, 2) }\nfn broken( {");
    assert!(lint(&text).is_empty());
}

// --- an owner that is not in scope ---------------------------------------------

/// `m` declares `Box` and a function returning one; `main` imports only the
/// function. `other` is `main`'s extra text.
fn two_files(main: &str) -> (KhoraDatabase, SourceFile) {
    let db = KhoraDatabase::new();
    let util = SourceFile::new(
        &db,
        "util.kh".into(),
        "module shop::util;\n\n\
         pub type Box = { v: Int };\n\n\
         impl Box {\n  pub fn add(self, a: Int) -> Int { self.v + a }\n}\n\n\
         pub fn make() -> Box { { v: 1 } }\n"
            .to_string(),
    );
    let main = SourceFile::new(&db, "main.kh".into(), main.to_string());
    khora_db::SourceRoot::new(&db, vec![util, main]);
    (db, main)
}

/// The owner is brought in: into the braces of an import of its module.
#[test]
fn an_owner_not_in_scope_is_imported() {
    let text = "module shop::main;\n\nimport shop::util::{make};\n\nfn f() -> Int { make().add(1) }\n";
    let (db, main) = two_files(text);
    assert!(khora_types::diagnostics(&db, main).is_empty());
    let found: Vec<Finding> = findings(&db, main).iter().filter(|f| f.lint == METHOD_CALL).cloned().collect();
    assert_eq!(found.len(), 1);
    let fix = found[0].fix.as_ref().expect("a fix");
    let (out, _) = apply(text, &[fix]);
    assert_eq!(out, "module shop::main;\n\nimport shop::util::{Box, make};\n\nfn f() -> Int { Box::add(make(), 1) }\n");
    let (db, main) = two_files(&out);
    assert!(khora_types::diagnostics(&db, main).is_empty(), "{out}");
}

/// **The import would clash**: the file declares a `Box` of its own, so
/// `Box::add` here would be that one's. The finding stays, with no fix.
#[test]
fn an_import_that_would_clash_is_refused() {
    let text = "module shop::main;\n\nimport shop::util::{make};\n\n\
                type Box = { w: Int };\n\n\
                fn f() -> Int { make().add(1) }\n";
    let (db, main) = two_files(text);
    assert!(khora_types::diagnostics(&db, main).is_empty());
    let found: Vec<Finding> = findings(&db, main).iter().filter(|f| f.lint == METHOD_CALL).cloned().collect();
    assert_eq!(found.len(), 1);
    assert!(found[0].fix.is_none(), "{found:?}");
}

/// Two calls needing one import make it once, in one pass.
#[test]
fn two_calls_needing_one_import_share_it() {
    let text = "module shop::main;\n\nimport shop::util::{make};\n\n\
                fn f() -> Int { make().add(1) + make().add(2) }\n";
    let (db, main) = two_files(text);
    let found: Vec<Finding> = findings(&db, main).iter().filter(|f| f.lint == METHOD_CALL).cloned().collect();
    let fixes: Vec<&Fix> = found.iter().filter_map(|f| f.fix.as_ref()).collect();
    assert_eq!(fixes.len(), 2);
    let (out, taken) = apply(text, &fixes);
    assert_eq!(taken, 2, "{out}");
    assert!(out.contains("import shop::util::{Box, make};\n"), "{out}");
    assert!(out.contains("Box::add(make(), 1) + Box::add(make(), 2)"), "{out}");
}

/// **A type named like the trait takes `Trait::m`.** The checker looks for a
/// type's impls before it looks for a trait of the name, so where `Named` is
/// also a type with a `name` of its own, `Named::name(b)` is that type's --
/// written by hand, it is refused with "expected `Named`, found `Box`". The
/// fix is withheld.
#[test]
fn a_trait_whose_name_a_type_takes_gets_no_fix() {
    let db = KhoraDatabase::new();
    let util = SourceFile::new(
        &db,
        "util.kh".into(),
        "module shop::util;\n\n\
         pub trait Titled { fn name(self) -> String; }\n\n\
         pub type Named = { v: Int };\n\n\
         impl Titled for Named { fn name(self) -> String { \"type\" } }\n\n\
         pub fn make() -> Named { { v: 1 } }\n"
            .to_string(),
    );
    let text = "module shop::main;\n\nimport shop::util::{make, Titled};\n\n\
                trait Named { fn name(self) -> String; }\n\n\
                type Box = { v: Int };\n\n\
                impl Named for Box { fn name(self) -> String { \"box\" } }\n\n\
                fn f(b: Box) -> String { b.name() }\n\n\
                fn g() -> String { Titled::name(make()) }\n";
    let main = SourceFile::new(&db, "main.kh".into(), text.to_string());
    khora_db::SourceRoot::new(&db, vec![util, main]);
    let errors = khora_types::diagnostics(&db, main);
    assert!(errors.is_empty(), "{errors:?}");
    let found: Vec<Finding> = findings(&db, main).iter().filter(|f| f.lint == METHOD_CALL).cloned().collect();
    assert_eq!(found.len(), 1, "{found:?}");
    assert!(found[0].fix.is_none(), "{found:?}");
}

/// **An unannotated lambda parameter.** Where the lambda's expected type
/// settles it (`apply` hands `x` a `Box`), the receiver's type is known when
/// the call is checked, and `Box::inc(x, 1)` pins it no less than `x.inc(1)`
/// did. Where only a later call would decide it, the dotted form is refused
/// by the checker itself -- a method cannot be looked up on a type nobody
/// has chosen -- so there is no program to fix.
#[test]
fn an_unannotated_lambda_parameter() {
    let later = module("fn f(b: Box) -> Int {\n  let g = fn x => x.inc(1);\n  g(b)\n}");
    let db = KhoraDatabase::new();
    let file = SourceFile::new(&db, "a.kh".into(), later.clone());
    assert!(!khora_types::diagnostics(&db, file).is_empty(), "the checker refuses a method on an open type:\n{later}");
    let hinted = module("fn apply(b: Box, g: (Box) -> Int) -> Int { g(b) }\nfn f(b: Box) -> Int { apply(b, fn x => x.inc(1)) }");
    assert!(fixed(&hinted).contains("fn x => Box::inc(x, 1)"), "{}", fixed(&hinted));
}

/// The same through a method the type declares for itself (`impl Named {
/// fn name }`) rather than through a trait impl.
#[test]
fn a_trait_whose_name_a_type_s_own_method_takes_gets_no_fix() {
    let db = KhoraDatabase::new();
    let util = SourceFile::new(
        &db,
        "util.kh".into(),
        "module shop::util;\n\n\
         pub type Named = { v: Int };\n\n\
         impl Named {\n  pub fn name(self) -> String { \"type\" }\n}\n\n\
         pub fn make() -> Named { { v: 1 } }\n"
            .to_string(),
    );
    let text = "module shop::main;\n\nimport shop::util::{make};\n\n\
                trait Named { fn name(self) -> String; }\n\n\
                type Box = { v: Int };\n\n\
                impl Named for Box { fn name(self) -> String { \"box\" } }\n\n\
                fn f(b: Box) -> String { b.name() }\n\n\
                fn g() -> Int { make().v }\n";
    let main = SourceFile::new(&db, "main.kh".into(), text.to_string());
    khora_db::SourceRoot::new(&db, vec![util, main]);
    let found: Vec<Finding> = findings(&db, main).iter().filter(|f| f.lint == METHOD_CALL).cloned().collect();
    let on_b: Vec<&Finding> = found.iter().filter(|f| text[std::ops::Range::<usize>::from(f.range)].starts_with("b.")).collect();
    assert_eq!(on_b.len(), 1, "{found:?}");
    assert!(on_b[0].fix.is_none(), "{found:?}");
}

/// **An import of a built-in type changes nothing.** `import std::core::{String}`
/// is accepted and is a no-op -- `std::core` declares no `String` -- so
/// `String::trim` still names the built-in, and the fix is made (the review's
/// `string_imported_manual`). An alias onto a built-in's name is left alone.
#[test]
fn an_imported_built_in_is_the_built_in() {
    let db = KhoraDatabase::new();
    let core = SourceFile::new(
        &db,
        "core.kh".into(),
        "module std::core;\n\npub type Wrap = { v: Int };\n\nimpl String {\n  pub fn trim(self) -> String { self }\n}\n".to_string(),
    );
    let text = "module shop::main;\n\nimport std::core::{String, Wrap};\n\nfn f(w: Wrap) -> String { \" b \".trim() }\n";
    let main = SourceFile::new(&db, "main.kh".into(), text.to_string());
    khora_db::SourceRoot::new(&db, vec![core, main]);
    let errors = khora_types::diagnostics(&db, main);
    assert!(errors.is_empty(), "{errors:?}");
    let found: Vec<Finding> = findings(&db, main).iter().filter(|f| f.lint == METHOD_CALL).cloned().collect();
    assert_eq!(found.len(), 1, "{found:?}");
    let fix = found[0].fix.as_ref().expect("the imported `String` is the built-in, so the fix is made");
    let (out, _) = apply(text, &[fix]);
    assert!(out.contains("{ String::trim(\" b \") }"), "{out}");
    assert!(out.contains("import std::core::{String, Wrap};"), "no second import: {out}");
}

/// An alias onto a built-in's name, `import std::core::{Int as String}`, is
/// left with its finding and no fix: the resolver accepts it and binds
/// nothing, so `String` there is still the built-in, but a file that wrote it
/// meant something by it, and which is not the lint's to guess.
#[test]
fn an_alias_onto_a_built_in_name_gets_no_fix() {
    let db = KhoraDatabase::new();
    let core = SourceFile::new(
        &db,
        "core.kh".into(),
        "module std::core;\n\npub type Wrap = { v: Int };\n\nimpl String {\n  pub fn trim(self) -> String { self }\n}\n".to_string(),
    );
    let text = "module shop::main;\n\nimport std::core::{Int as String, Wrap};\n\nfn f(w: Wrap) -> String { \" b \".trim() }\n";
    let main = SourceFile::new(&db, "main.kh".into(), text.to_string());
    khora_db::SourceRoot::new(&db, vec![core, main]);
    let found: Vec<Finding> = findings(&db, main).iter().filter(|f| f.lint == METHOD_CALL).cloned().collect();
    assert_eq!(found.len(), 1, "{found:?}");
    assert!(found[0].fix.is_none(), "{found:?}");
}
