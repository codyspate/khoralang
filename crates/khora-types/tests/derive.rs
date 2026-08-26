//! `derive(Eq, Ord, Show, Hash)`.
//!
//! What is being pinned here is that a derived impl is an *ordinary* impl:
//! resolvable, callable, checked, and refused for the same reasons a written
//! one would be. The expansion is source-to-source, so if these pass then
//! nothing between the parser and the backend has had to learn a new concept.
//!
//! The traits are declared in each test rather than imported from `std`, in
//! the style of `traits.rs`: a checker test that reaches for the real standard
//! library is testing the standard library.

use khora_db::{KhoraDatabase, SourceFile};
use khora_types::diagnostics;

fn errors(text: &str) -> Vec<String> {
    let db = KhoraDatabase::new();
    let file = SourceFile::new(&db, "a.kh".into(), text.to_string());
    diagnostics(&db, file).iter().map(|e| e.message.clone()).collect()
}

fn assert_clean(text: &str) {
    let found = errors(text);
    assert!(found.is_empty(), "expected no errors, got {found:?}\n{text}");
}

fn assert_reports(text: &str, needle: &str) {
    let found = errors(text);
    assert!(
        found.iter().any(|e| e.contains(needle)),
        "expected an error containing {needle:?}, got {found:?}\n{text}"
    );
}

/// The four traits, spelled as `std/core.kh` spells them, plus the impls for
/// the scalars a derived field is most likely to be.
const CORE: &str = "\
module m;
pub type Ordering = | Less | Equal | Greater;
pub trait Eq { fn eq(self, other: Self) -> Bool; }
pub trait Ord: Eq { fn cmp(self, other: Self) -> Ordering; }
pub trait Hash: Eq { fn hash(self) -> Int; }
pub trait Show { fn show(self) -> String; }
impl Eq for Int { fn eq(self, other: Int) -> Bool { self == other } }
impl Ord for Int {
  fn cmp(self, other: Int) -> Ordering {
    if self < other { Ordering::Less }
    else if self == other { Ordering::Equal }
    else { Ordering::Greater }
  }
}
impl Hash for Int { fn hash(self) -> Int { self } }
impl Show for Int { fn show(self) -> String { Int::to_string(self) } }
impl Int { fn to_string(self) -> String { \"n\" } }
impl Eq for Bool { fn eq(self, other: Bool) -> Bool { self == other } }
impl Hash for Bool { fn hash(self) -> Int { if self { 1 } else { 0 } } }
impl Show for Bool { fn show(self) -> String { if self { \"true\" } else { \"false\" } } }
impl Eq for Ordering {
  fn eq(self, other: Ordering) -> Bool {
    match self {
      Ordering::Less => match other { Ordering::Less => true, _ => false },
      Ordering::Equal => match other { Ordering::Equal => true, _ => false },
      Ordering::Greater => match other { Ordering::Greater => true, _ => false },
    }
  }
}
";

// --- the four traits, derived and used ------------------------------------

#[test]
fn a_derived_eq_is_callable() {
    assert_clean(&format!(
        "{CORE}\
         derive(Eq)\n\
         pub type Point = {{ x: Int, y: Int }};\n\
         fn same(a: Point, b: Point) -> Bool {{ a.eq(b) }}\n"
    ));
}

#[test]
fn a_derived_ord_is_callable() {
    assert_clean(&format!(
        "{CORE}\
         derive(Eq, Ord)\n\
         pub type Point = {{ x: Int, y: Int }};\n\
         fn order(a: Point, b: Point) -> Ordering {{ a.cmp(b) }}\n"
    ));
}

#[test]
fn a_derived_hash_is_callable() {
    assert_clean(&format!(
        "{CORE}\
         derive(Eq, Hash)\n\
         pub type Point = {{ x: Int, y: Int }};\n\
         fn key(p: Point) -> Int {{ p.hash() }}\n"
    ));
}

#[test]
fn a_derived_show_is_callable() {
    assert_clean(&format!(
        "{CORE}\
         derive(Show)\n\
         pub type Point = {{ x: Int, y: Int }};\n\
         fn text(p: Point) -> String {{ p.show() }}\n"
    ));
}

#[test]
fn all_four_can_be_derived_at_once_for_a_variant() {
    assert_clean(&format!(
        "{CORE}\
         derive(Eq, Ord, Show, Hash)\n\
         pub type Shape = | Dot | Circle(r: Int) | Rect(w: Int, h: Int);\n\
         fn use_them(a: Shape, b: Shape) -> String {{\n\
         \x20 if a.eq(b) && a.cmp(b).eq(Ordering::Equal) && a.hash() == b.hash() {{ a.show() }}\n\
         \x20 else {{ b.show() }}\n\
         }}\n"
    ));
}

/// A derived impl satisfies a bound like any other, which is the whole reason
/// to want one: `sort` asks for `Ord` and does not care who wrote it.
#[test]
fn a_derived_impl_satisfies_a_bound() {
    assert_clean(&format!(
        "{CORE}\
         derive(Eq, Ord)\n\
         pub type Point = {{ x: Int, y: Int }};\n\
         fn least<T: Ord>(a: T, b: T) -> T {{ if a.cmp(b).eq(Ordering::Greater) {{ b }} else {{ a }} }}\n\
         fn go(a: Point, b: Point) -> Point {{ least(a, b) }}\n"
    ));
}

// --- refusing rather than guessing ----------------------------------------

/// The error has to name the field, or the reader is left comparing the
/// declaration against the trait by eye.
#[test]
fn a_field_without_the_trait_is_refused_by_name() {
    assert_reports(
        &format!(
            "{CORE}\
             pub type Opaque = | Nothing;\n\
             derive(Eq)\n\
             pub type Point = {{ x: Int, y: Opaque }};\n"
        ),
        "the field `y` has type `Opaque`, which does not",
    );
}

/// The variant half of the same rule, where the field belongs to a case.
#[test]
fn a_case_payload_without_the_trait_is_refused_by_name() {
    assert_reports(
        &format!(
            "{CORE}\
             pub type Opaque = | Nothing;\n\
             derive(Show)\n\
             pub type Wrapper = | Plain(Int) | Odd(inner: Opaque);\n"
        ),
        "the field `inner` of `Odd` has type `Opaque`, which does not",
    );
}

/// A positional payload has no name, so its position is what the message uses.
#[test]
fn a_positional_payload_is_named_by_its_position() {
    assert_reports(
        &format!(
            "{CORE}\
             pub type Opaque = | Nothing;\n\
             derive(Eq)\n\
             pub type Wrapper = | Both(Int, Opaque);\n"
        ),
        "field 1 of `Both` has type `Opaque`",
    );
}

/// One accurate sentence, not that sentence plus a body's worth of noise about
/// the impl the compiler wrote in response to it.
#[test]
fn a_refused_derive_reports_once() {
    let found = errors(&format!(
        "{CORE}\
         pub type Opaque = | Nothing;\n\
         derive(Eq)\n\
         pub type Point = {{ x: Opaque }};\n"
    ));
    assert_eq!(found.len(), 1, "expected exactly one diagnostic, got {found:?}");
}

#[test]
fn a_trait_that_cannot_be_derived_says_which_can() {
    assert_reports(
        &format!(
            "{CORE}\
             derive(Frobnicate)\n\
             pub type Point = {{ x: Int }};\n"
        ),
        "`Frobnicate` cannot be derived",
    );
}

#[test]
fn a_type_with_no_body_has_nothing_to_derive_from() {
    assert_reports(
        &format!("{CORE}derive(Eq)\npub type Handle;\n"),
        "it is declared with no body",
    );
}

/// `Ord` requires `Eq`, and `derive(Ord)` alone does not quietly supply it:
/// what a type implements should be readable from its declaration.
#[test]
fn deriving_ord_without_eq_says_so() {
    assert_reports(
        &format!(
            "{CORE}\
             derive(Ord)\n\
             pub type Point = {{ x: Int }};\n"
        ),
        "`Ord` requires `Eq`, and `Point` does not implement it",
    );
}

#[test]
fn deriving_hash_without_eq_says_so() {
    assert_reports(
        &format!(
            "{CORE}\
             derive(Hash)\n\
             pub type Point = {{ x: Int }};\n"
        ),
        "`Hash` requires `Eq`, and `Point` does not implement it",
    );
}

/// A hand-written `Eq` satisfies the requirement as well as a derived one.
#[test]
fn a_hand_written_eq_satisfies_ord() {
    assert_clean(&format!(
        "{CORE}\
         derive(Ord)\n\
         pub type Point = {{ x: Int }};\n\
         impl Eq for Point {{ fn eq(self, other: Point) -> Bool {{ self.x.eq(other.x) }} }}\n"
    ));
}

/// Deriving what the file also writes is the one impl too many, and the
/// `derive` is the half to point at.
#[test]
fn deriving_what_is_already_written_is_a_duplicate() {
    assert_reports(
        &format!(
            "{CORE}\
             derive(Eq)\n\
             pub type Point = {{ x: Int }};\n\
             impl Eq for Point {{ fn eq(self, other: Point) -> Bool {{ true }} }}\n"
        ),
        "`Eq` is already implemented for `Point`",
    );
}

/// A syntax error, so it comes out of the parser rather than out of
/// `diagnostics` — the two are deliberately separate answers in this compiler.
#[test]
fn a_derive_in_front_of_something_else_is_reported() {
    let parse = khora_syntax::parse("module m;\nderive(Eq)\nfn f() -> Int { 1 }\n");
    let found: Vec<String> = parse.errors().iter().map(|e| e.message.clone()).collect();
    assert!(
        found.iter().any(|e| e.contains("`derive(..)` introduces a `type` declaration")),
        "got {found:?}"
    );
}

/// `derive` stays an ordinary word everywhere it is not a declaration.
#[test]
fn derive_is_still_usable_as_a_name() {
    assert_clean(&format!(
        "{CORE}fn derive(x: Int) -> Int {{ x }}\nfn f() -> Int {{ derive(1) }}\n"
    ));
}

// --- the semantics, run --------------------------------------------------
//
// These check the *shape* of what was generated by reading it back, because a
// checker test cannot run a program. The execution test in
// `khora-codegen-llvm/tests/derive.rs` is what pins the values.

/// Field order is comparison order. Read off the generated source rather than
/// asserted about at run time here — but it is the same text the backend
/// compiles, so the two cannot disagree.
#[test]
fn a_records_fields_compare_in_declaration_order() {
    let source = "module m;\nderive(Ord)\npub type Point = { x: Int, y: Int };\n";
    let parse = khora_syntax::parse(source);
    let generated = khora_hir::derive::expand(&parse.source_file());
    let text = generated.source();
    let x = text.find("self.x.cmp(other.x)").expect("`x` is compared");
    let y = text.find("self.y.cmp(other.y)").expect("`y` is compared");
    assert!(x < y, "`x` is declared first and must be compared first:\n{text}");
}

/// Declaration order decides which case is `Less`, so the position a case is
/// written at is the number it compares by.
#[test]
fn a_variants_cases_order_by_declaration() {
    let source = "module m;\nderive(Ord)\npub type Shape = | Dot | Circle(r: Int);\n";
    let parse = khora_syntax::parse(source);
    let generated = khora_hir::derive::expand(&parse.source_file());
    let text = generated.source();
    assert!(text.contains("Shape::Dot => 0"), "{text}");
    assert!(text.contains("Shape::Circle(_) => 1"), "{text}");
}

/// Equal values must hash equal, and the way to be sure of that by
/// construction is for `hash` to visit exactly the fields `eq` compares, in
/// the same order. A derived pair that disagreed about which fields matter is
/// the failure mode the `Hash` doc comment in `std/core.kh` is about.
#[test]
fn a_derived_hash_reads_the_same_fields_as_the_derived_eq() {
    let source = "module m;\nderive(Eq, Hash)\npub type Point = { x: Int, y: Int };\n";
    let parse = khora_syntax::parse(source);
    let generated = khora_hir::derive::expand(&parse.source_file());
    let text = generated.source();
    for field in ["x", "y"] {
        assert!(text.contains(&format!("self.{field}.eq(other.{field})")), "{text}");
        assert!(text.contains(&format!("self.{field}.hash()")), "{text}");
    }
}

// --- generics -------------------------------------------------------------

#[test]
fn a_generic_type_derives_with_a_bound_on_every_parameter() {
    assert_clean(&format!(
        "{CORE}\
         derive(Eq)\n\
         pub type Box<A> = {{ value: A }};\n\
         fn same(a: Box<Int>, b: Box<Int>) -> Bool {{ a.eq(b) }}\n"
    ));
}

/// The generated impl is `impl<A: Eq> Eq for Box<A>`, bound and all — the same
/// text somebody writing it out by hand would produce.
///
/// What this does *not* assert is that `Box<Opaque>.eq(..)` is refused, and it
/// deliberately does not: today it is accepted. That is not a derive bug —
/// writing the same impl by hand and calling it on a `Box<Opaque>` is accepted
/// too — but a gap in how an impl block's own bounds are discharged at a call
/// site. Recorded here because this is the test somebody will come looking at
/// when they notice.
#[test]
fn a_generic_derive_bounds_every_parameter_by_the_trait() {
    let source = "module m;\nderive(Eq, Show)\npub type Pair<A, B> = { left: A, right: B };\n";
    let parse = khora_syntax::parse(source);
    let generated = khora_hir::derive::expand(&parse.source_file());
    let text = generated.source();
    assert!(text.contains("impl<A: Eq, B: Eq> Eq for Pair<A, B>"), "{text}");
    assert!(text.contains("impl<A: Show, B: Show> Show for Pair<A, B>"), "{text}");
}

#[test]
fn a_generic_variant_derives() {
    assert_clean(&format!(
        "{CORE}\
         derive(Eq, Ord)\n\
         pub type Maybe<A> = | Nothing | Just(value: A);\n\
         fn order(a: Maybe<Int>, b: Maybe<Int>) -> Ordering {{ a.cmp(b) }}\n"
    ));
}

/// A const parameter is refused rather than half-supported: an impl header
/// that restates it wrongly implements a different type than the one derived.
#[test]
fn a_const_parameter_is_refused_with_a_reason() {
    assert_reports(
        &format!(
            "{CORE}\
             derive(Eq)\n\
             pub type Vector<const N: Int> = {{ length: Int }};\n"
        ),
        "const or row parameter",
    );
}

// --- recursion ------------------------------------------------------------

/// A type that contains itself derives: the impl being generated is in scope
/// by the time the fields are checked, which is what makes a list comparable.
#[test]
fn a_recursive_type_derives() {
    assert_clean(&format!(
        "{CORE}\
         derive(Eq)\n\
         pub type Chain = | End | Link(head: Int, rest: Chain);\n\
         fn same(a: Chain, b: Chain) -> Bool {{ a.eq(b) }}\n"
    ));
}
