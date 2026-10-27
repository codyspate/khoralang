//! The first error a reader sees, for five mistakes that used to send them the
//! wrong way.
//!
//! Each test asserts the three things a reader acts on: the words that name the
//! cause, the span it points at, and that it comes *first*. An agent or a
//! newcomer fixes the first error and recompiles, so a right message in second
//! place is a wrong message. Where a message suggests a spelling, a test also
//! checks that the suggestion compiles: advice that does not work is what two
//! of these were.

use std::path::{Path, PathBuf};

use khora_db::{KhoraDatabase, SourceFile, SourceRoot};

fn std_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..").join("std")
}

fn sources(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).expect("the std directory should exist").flatten() {
        let path = entry.path();
        if path.is_dir() {
            sources(&path, out);
        } else if path.extension().is_some_and(|e| e == "kh") {
            out.push(path);
        }
    }
}

/// One diagnostic: its message and the source text it underlines.
#[derive(Debug)]
struct Said {
    message: String,
    span: String,
}

/// Every diagnostic for `program`, compiled together with `std`, in the order
/// `khora check` prints them.
fn said(program: &str) -> Vec<Said> {
    let mut paths = Vec::new();
    sources(&std_dir(), &mut paths);
    paths.sort();
    let db = KhoraDatabase::new();
    let mut files: Vec<SourceFile> = paths
        .iter()
        .map(|p| {
            let text = std::fs::read_to_string(p).expect("std should be readable");
            SourceFile::new(&db, p.clone(), text)
        })
        .collect();
    let mine = SourceFile::new(&db, PathBuf::from("program.kh"), program.to_string());
    files.push(mine);
    SourceRoot::new(&db, files);
    khora_types::diagnostics(&db, mine)
        .iter()
        .map(|e| Said {
            message: e.message.clone(),
            span: program[usize::from(e.range.start())..usize::from(e.range.end())].to_string(),
        })
        .collect()
}

/// The first diagnostic, which has to exist.
fn first(program: &str) -> Said {
    said(program).into_iter().next().unwrap_or_else(|| panic!("expected an error:\n{program}"))
}

fn assert_clean(program: &str) {
    let found = said(program);
    assert!(found.is_empty(), "the suggested spelling should compile, got {found:#?}\n{program}");
}

// --- 1. a trait written where a type goes ----------------------------------

const TRAIT_AS_TYPE: &str = "module h7c::main;
import std::core::{Show, print};

fn render_one(item: Show) -> String {
  Show::show(item)
}

pub fn main() -> Int {
  print(render_one(\"hello\"));
  0
}
";

/// **``Show` does not implement `Show`` made the checker look broken.** A
/// Go or TypeScript engineer writes an interface as a parameter type; the
/// message has to say that `Show` is a trait and how a function over any
/// `Show` is written here.
#[test]
fn a_trait_as_a_parameter_type_is_named_as_a_trait() {
    let found = said(TRAIT_AS_TYPE);
    let head = found.first().expect("an error");
    assert!(head.message.contains("`Show` is a trait, not a type"), "{found:#?}");
    assert!(head.message.contains("fn render_one<A: Show>(item: A)"), "{found:#?}");
    assert_eq!(head.span, "Show", "{found:#?}");
    // The two follow-ons -- the body's "does not implement" and the caller's
    // "expected `Show`, found `String`" -- are the same mistake again.
    assert_eq!(found.len(), 1, "one mistake, one error: {found:#?}");
}

#[test]
fn the_generic_spelling_it_suggests_compiles() {
    assert_clean(&TRAIT_AS_TYPE.replace(
        "fn render_one(item: Show) -> String",
        "fn render_one<A: Show>(item: A) -> String",
    ));
}

/// Buried in a type argument it was accepted in silence, and the program
/// compiled.
#[test]
fn a_trait_inside_a_type_argument_is_reported_too() {
    let program = "module m;
import std::core::{Show, List};

fn count(items: List<Show>) -> Int { 0 }
";
    let head = first(program);
    assert!(head.message.contains("`Show` is a trait, not a type"), "{head:#?}");
    assert!(head.message.contains("fn count<A: Show>(items: List<A>)"), "{head:#?}");
    assert_eq!(head.span, "Show");
    assert_clean(&program.replace("fn count(items: List<Show>)", "fn count<A: Show>(items: List<A>)"));
}

/// Two traits in one parameter list get one generic each, so the spelling
/// still compiles when it is copied for the first of them.
#[test]
fn two_traits_in_one_signature_get_two_parameters() {
    let program = "module m;
import std::core::{Show, Eq};

fn both<A>(x: A, a: Show, b: Eq) -> Int { 0 }
";
    let head = first(program);
    assert!(head.message.contains("fn both<A, B: Show, C: Eq>(x: A, a: B, b: C)"), "{head:#?}");
    assert_clean(&program.replace(
        "fn both<A>(x: A, a: Show, b: Eq)",
        "fn both<A, B: Show, C: Eq>(x: A, a: B, b: C)",
    ));
}

/// A closure's parameter is a parameter too, and a trait there is the same
/// mistake: one error, at the name, and nothing about the closure "never
/// pinned down" because of it. `=> s` matters: the result is the parameter,
/// so nothing else solves it.
#[test]
fn a_trait_as_a_closure_parameter_is_one_error() {
    let program = "module m;
import std::core::{Show};

fn f() -> Int {
  let g = fn (s: Show) => s;
  0
}
";
    let found = said(program);
    assert_eq!(found.len(), 1, "{found:#?}");
    assert!(found[0].message.contains("`Show` is a trait, not a type"), "{found:#?}");
    assert_eq!(found[0].span, "Show");
}

/// **The gate that keeps a trait's uses quiet is per function.** A
/// file-wide gate hid a genuine "never worked out" in another function --
/// the audit that exists because such holes have reached code generation.
#[test]
fn a_trait_as_a_type_hides_nothing_in_another_function() {
    let program = "module m;
import std::core::{Show};

pub type Spec = { fields: String };
pub trait Extract { type Spec; fn spec() -> Self::Spec; }
pub effect Model {
  extract: forall <A: Extract> . (A::Spec) -> A,
}
fn render_one(item: Show) -> String { item.show() }
fn use_it() -> Int with { model: Model } { model.extract({ fields: \"x\" }); 0 }
";
    let found = said(program);
    assert_eq!(found.len(), 2, "{found:#?}");
    assert!(found[0].message.contains("`Show` is a trait, not a type"), "{found:#?}");
    assert!(found[1].message.contains("never worked out"), "{found:#?}");
    assert_eq!(found[1].span, "model.extract", "{found:#?}");
}

/// A hole in one function's signature reaches its callers as a value, and
/// auditing a caller blamed a call on that value for a mistake only the
/// signature made ("nothing here decides what type `#Map::len` is used at").
/// The caller is skipped; the signature's error stands.
#[test]
fn a_hole_in_a_callee_signature_is_not_blamed_on_the_caller() {
    let program = "import std::core::{List, Option, Result, print};
pub type Map<K, V> = { keys: List<K>, vals: List<V> };
fn mk() -> Map<String, Int> { { keys: List::Nil, vals: List::Nil } }
pub fn main() -> Int {
  let m = mk();
  m.len()
}
";
    let found = said(program);
    assert!(found.iter().any(|e| e.message.contains("cannot find type `Map`")), "{found:#?}");
    assert!(!found.iter().any(|e| e.message.contains("nothing here decides")), "{found:#?}");
}

/// **`fn make<A: Eq>() -> A` reads as the fix and does not compile**: the
/// caller picks `A`. A trait as a return type is told to name the type.
#[test]
fn a_trait_as_a_return_type_is_told_to_name_the_type() {
    let program = "module m;\nimport std::core::{Eq};\n\nfn make() -> Eq { 3 }\n";
    let head = first(program);
    assert!(head.message.contains("`Eq` is a trait, not a type"), "{head:#?}");
    assert!(head.message.contains("cannot return \"any `Eq`\""), "{head:#?}");
    assert!(!head.message.contains("fn f<A"), "{head:#?}");
    assert_clean(&program.replace("-> Eq", "-> Int"));
}

/// A record field gets the declaration rewritten with a bounded parameter,
/// and that spelling compiles.
#[test]
fn a_trait_as_a_field_type_suggests_a_bounded_type_parameter() {
    let program = "module m;\nimport std::core::{Show};\n\n/// A box.\npub type Box = { item: Show };\n";
    let head = first(program);
    assert!(
        head.message.contains("`type Box<A: Show> = { item: A }`"),
        "{head:#?}"
    );
    assert_clean(&program.replace("pub type Box = { item: Show };", "pub type Box<A: Show> = { item: A };"));
}

// --- 3b. a hint somebody wrote keeps its place ----------------------------

/// **An annotation is the intent, not a guess.** With
/// `let total: Int = first(names, 0)`, checking `names` before the
/// annotation blamed the `0`, which was right. One error, at `names`.
#[test]
fn a_written_annotation_blames_the_wrong_local() {
    for (program, span) in [
        (
            "module m;\nimport std::core::{List};\nfn first<A>(xs: List<A>, d: A) -> A { d }\nfn f() -> Int {\n  let names = List::Cons(\"a\", List::Nil);\n  let total: Int = first(names, 0);\n  total\n}\n",
            "names",
        ),
        (
            "module m;\nfn same<A>(a: A, b: A) -> A { a }\nfn f() -> Int {\n  let s = \"text\";\n  let n: Int = same(s, 4);\n  n\n}\n",
            "s",
        ),
    ] {
        let found = said(program);
        assert_eq!(found.len(), 1, "{found:#?}\n{program}");
        assert_eq!(found[0].span, span, "{found:#?}\n{program}");
    }
}

/// Where a trait belongs -- a bound, a supertrait, the trait of an `impl` --
/// nothing is said.
#[test]
fn a_trait_where_a_trait_belongs_is_not_reported() {
    assert_clean(
        "module m;
import std::core::{Show, Eq};

trait Named: Show {
  type Key: Eq;
  fn name(self) -> String;
}

pub type Box<A> = { v: A };

impl<A: Show> Show for Box<A> {
  fn show(self) -> String { \"box\" }
}

fn render<A: Show + Eq>(x: A) -> String { Show::show(x) }
",
    );
}

// --- 2. a capability used without a `with` ---------------------------------

const MISSING_WITH: &str = "module p2::main;
import std::clock::{Clock};

fn private_helper(x: Int) -> Int {
  clock.monotonic_millis() + x
}

pub fn main() -> Int {
  0
}
";

/// **``cannot find `clock`; did you mean `Clock`?`` sent the reader to the
/// type.** `clock` is the conventional name of the `Clock` capability, and
/// what is missing is the `with` clause that would put it in scope.
#[test]
fn a_capability_used_without_a_with_says_where_it_comes_from() {
    let found = said(MISSING_WITH);
    let head = found.first().expect("an error");
    assert!(head.message.contains("`clock` is a capability"), "{found:#?}");
    assert!(head.message.contains("with { clock: Clock }"), "{found:#?}");
    assert!(head.message.contains("clock: Clock"), "{found:#?}");
    assert!(!head.message.contains("did you mean `Clock`"), "{found:#?}");
    assert_eq!(head.span, "clock");
}

#[test]
fn both_spellings_it_suggests_compile() {
    assert_clean(&MISSING_WITH.replace(
        "fn private_helper(x: Int) -> Int {",
        "fn private_helper(x: Int) -> Int with { clock: Clock } {",
    ));
    assert_clean(&MISSING_WITH.replace(
        "fn private_helper(x: Int) -> Int {",
        "fn private_helper(x: Int, clock: Clock) -> Int {",
    ));
}

/// Without the import, the effect's name is not in scope either, so the
/// message says where it comes from -- and only then.
#[test]
fn a_capability_whose_effect_is_not_imported_names_the_import() {
    let program = "module m;

fn elapsed(x: Int) -> Int {
  clock.monotonic_millis() + x
}
";
    let head = first(program);
    assert!(head.message.contains("`import std::clock::{Clock};`"), "{head:#?}");
    assert!(!first(MISSING_WITH).message.contains("import"), "already imported there");
    assert_clean(&program.replace(
        "module m;\n",
        "module m;\nimport std::clock::{Clock};\n",
    ).replace(
        "fn elapsed(x: Int) -> Int {",
        "fn elapsed(x: Int) -> Int with { clock: Clock } {",
    ));
}

/// Inside a lambda the name is resolved later, against the row the lambda is
/// offered; a label nothing offers gets the same advice there.
#[test]
fn so_does_one_inside_a_closure() {
    let program = "module p2::main;
import std::clock::{Clock};
import std::core::{List};

fn stamp(xs: List<Int>) -> List<Int> {
  List::map(xs, fn x => clock.monotonic_millis() + x)
}
";
    let head = first(program);
    assert!(head.message.contains("`clock` is a capability"), "{head:#?}");
    assert!(head.message.contains("with { clock: Clock }"), "{head:#?}");
    assert_clean(&program.replace(
        "fn stamp(xs: List<Int>) -> List<Int> {",
        "fn stamp(xs: List<Int>) -> List<Int> with { clock: Clock } {",
    ));
}

/// A label the program itself uses for a capability counts, whatever its
/// type is called.
#[test]
fn a_label_another_function_declares_is_recognized() {
    let program = "module m;
import std::clock::{Clock};

fn timed(x: Int) -> Int with { timer: Clock } {
  timer.monotonic_millis() + x
}

fn untimed(x: Int) -> Int {
  timer.monotonic_millis() + x
}
";
    let head = first(program);
    assert!(head.message.contains("`timer` is a capability"), "{head:#?}");
    assert!(head.message.contains("with { timer: Clock }"), "{head:#?}");
    assert_eq!(head.span, "timer");
}

/// **`main` cannot take a `with` clause or a parameter**, so advice to add
/// one met a second refusal. An entry point installs the capability, and the
/// message says so in a spelling that compiles.
#[test]
fn a_capability_used_in_main_says_to_install_it() {
    let program = "module m;
import std::clock::{Clock};

pub fn main() -> Int {
  clock.monotonic_millis()
}
";
    let head = first(program);
    assert!(head.message.contains("`clock` is a capability"), "{head:#?}");
    assert!(head.message.contains("with { clock: Clock::real() } { .. }"), "{head:#?}");
    assert!(!head.message.contains("after the return type"), "{head:#?}");
    assert!(!head.message.contains("among the parameters"), "{head:#?}");
    assert_eq!(head.span, "clock");
    assert_clean(&program.replace(
        "  clock.monotonic_millis()\n",
        "  with { clock: Clock::real() } { clock.monotonic_millis() }\n",
    ));
}

/// A test is an entry point too: nothing calls it to hand it a capability.
#[test]
fn a_capability_used_in_a_test_says_to_install_it() {
    let program = "module m;
import std::clock::{Clock};

test \"ticks\" {
  let _ = clock.monotonic_millis();
}
";
    let head = first(program);
    assert!(head.message.contains("with { clock: Clock::real() } { .. }"), "{head:#?}");
    assert!(!head.message.contains("among the parameters"), "{head:#?}");
}

/// **A typo of a local is a typo, even when the typo is a capability's
/// label.** `log`, `env`, `random`, `reads` are labels in `std` and ordinary
/// variable names too; beside a local `logs` the reader meant `logs`.
#[test]
fn a_typo_of_a_local_keeps_did_you_mean_even_for_a_capability_label() {
    for (program, near) in [
        ("module m;\nfn f() -> Int {\n  let logs = 3;\n  log + 1\n}\n", "logs"),
        ("module m;\nfn f(lg: Int) -> Int {\n  log + 1\n}\n", "lg"),
        ("module m;\nfn f() -> Int {\n  let envs = 3;\n  env + 1\n}\n", "envs"),
        ("module m;\nfn f() -> Int {\n  let read = 2;\n  reads + 1\n}\n", "read"),
        (
            "module m;\nfn f() -> Int {\n  let logs = 3;\n  let g = fn x => log + x;\n  g(1)\n}\n",
            "logs",
        ),
    ] {
        let head = first(program);
        assert!(head.message.contains(&format!("did you mean `{near}`?")), "{head:#?}\n{program}");
        assert!(!head.message.contains("capability"), "{head:#?}\n{program}");
    }
}

/// A misspelling that is not a capability's name is still a misspelling.
#[test]
fn an_ordinary_typo_still_gets_did_you_mean() {
    let head = first(
        "module m;
import std::core::{print};

fn f() -> () {
  prnt(\"x\")
}
",
    );
    assert!(head.message.contains("did you mean `print`?"), "{head:#?}");
    assert!(!head.message.contains("capability"), "{head:#?}");
}

// --- 3. a closure's result blamed on an argument ---------------------------

const DEFER_RESULT: &str = "module h2a::main;
import std::core::{Region, Shared};

pub fn main() -> Int {
  let r = Region::open();
  let state = Shared::of(\"ready\");
  Region::defer(r, fn () => Shared::update(state, fn s => \"${s}!\"));
  0
}
";

/// **The mismatch was the closure's result, and it was blamed on `state`.**
/// `defer` wants a closure returning `()`, and `Shared::update` returns the
/// new value. The expected `()` reached `update`'s type parameter before
/// `state` did, so `state` was told it was the wrong type and `s` was told it
/// had no `Show`.
#[test]
fn a_closure_returning_the_wrong_thing_is_blamed_for_its_result() {
    let found = said(DEFER_RESULT);
    let head = found.first().expect("an error");
    assert!(head.message.contains("`Region::defer`"), "{found:#?}");
    assert!(head.message.contains("expected `()`, found `String`"), "{found:#?}");
    assert!(head.message.contains("let _ ="), "{found:#?}");
    assert_eq!(head.span, "Shared::update(state, fn s => \"${s}!\")", "{found:#?}");
    assert!(!found.iter().any(|s| s.message.contains("Shared<()>")), "{found:#?}");
    assert_eq!(found.len(), 1, "{found:#?}");
}

#[test]
fn the_spelling_it_suggests_compiles() {
    assert_clean(&DEFER_RESULT.replace(
        "fn () => Shared::update(state, fn s => \"${s}!\")",
        "fn () => { let _ = Shared::update(state, fn s => \"${s}!\"); }",
    ));
}

// --- 5. a missing import, and the errors that only follow from it ----------

const MAP_UNIMPORTED: &str = "module m;
import std::core::{print, List};

fn tally(words: List<String>) -> Map<String, Int> {
  let counts = Map::new();
  counts.insert(\"a\", 1);
  counts
}

fn report(counts: Map<String, Int>) -> Int {
  print(\"${counts.len()} keys\");
  if counts.holds(\"a\") { 1 } else { 0 }
}
";

/// **One missing import, one error per place it was needed.** Calling a
/// method on a value whose type never resolved added ``cannot be the receiver
/// of `#Map::len`: expected `Map<_, _>`, found `Map<String, Int>`` per call --
/// about sixty in one real program, each reading as a disagreement between
/// two real types.
#[test]
fn a_method_on_an_unimported_type_adds_no_error_of_its_own() {
    let found = said(MAP_UNIMPORTED);
    assert!(!found.iter().any(|s| s.message.contains("cannot be the receiver")), "{found:#?}");
    assert!(found.iter().all(|s| s.span.starts_with("Map")), "{found:#?}");
    assert_eq!(found.len(), 3, "the three mentions of `Map`: {found:#?}");
}
