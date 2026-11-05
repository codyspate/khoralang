//! Programs with their own `Vector`, `Map`, `Array`, `Dict` or `Iterable`,
//! which owe nothing to the `for` loop over `std::core`'s.
//!
//! **What these guard: a std impl that reaches a file by bare name.** `std`'s
//! `Iterable` impls are written for `std::core`'s four collections, and a file
//! is entitled to declare a type or a trait of the same name. When those
//! impls were handed to every file by name, a program's own
//! `type Map = { n: Int }` was refused as "`Map` takes 0 type argument(s), but
//! 2 were given", at the end of the file, with no loop anywhere; a program's
//! own `trait Iterable` got eight errors about `std`'s cursors. Each shape
//! below compiled before `Iterable` existed and has to compile now, in every
//! file of the program.

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

/// Every diagnostic in every one of the program's own files, compiled with
/// `std`, as `file: message`.
fn diagnostics(program: &[(&str, &str)]) -> Vec<String> {
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
    let mine: Vec<SourceFile> = program
        .iter()
        .map(|(name, text)| SourceFile::new(&db, PathBuf::from(name), text.to_string()))
        .collect();
    files.extend(mine.iter().copied());
    SourceRoot::new(&db, files);
    let mut out = Vec::new();
    for (file, (name, _)) in mine.iter().zip(program) {
        for e in khora_types::diagnostics(&db, *file).iter() {
            out.push(format!("{name}: {}", e.message));
        }
    }
    out
}

fn assert_clean(program: &[(&str, &str)]) {
    let found = diagnostics(program);
    assert!(found.is_empty(), "{found:#?}");
}

const I: &str = "import std::core::{Iterator, Step, List, print};\n";

/// A `Vector` of the program's own that is its own `Iterator`, walked.
#[test]
fn an_own_vector_that_is_an_iterator_is_walked() {
    let main = format!(
        "module main;\n{I}type Vector = {{ left: Int }};\n\
         impl Iterator for Vector {{\n  type Item = Int;\n  type Effects = {{}};\n  \
         fn next(self) -> Step<Vector, Int> {{ if self.left == 0 {{ Step::Done }} else \
         {{ Step::Yield({{ left: self.left - 1 }}, self.left) }} }}\n}}\n\
         pub fn main() -> Int {{ let v: Vector = {{ left: 3 }}; let mut s = \"\"; \
         for x in v {{ s = s + \"${{x}}\"; }}; print(s); 0 }}\n"
    );
    assert_clean(&[("main.kh", &main)]);
}

/// An own `Vector` record beside a `List` loop.
#[test]
fn an_own_vector_beside_a_list_loop() {
    let main = format!(
        "module main;\n{I}type Vector = {{ n: Int }};\n\
         pub fn main() -> Int {{ let v: Vector = {{ n: 3 }}; let mut t = 0; \
         for x in [1, 2] {{ t = t + x; }}; print(\"${{t + v.n}}\"); 0 }}\n"
    );
    assert_clean(&[("main.kh", &main)]);
}

/// An own `Map` record beside a `List` loop.
#[test]
fn an_own_map_beside_a_list_loop() {
    let main = format!(
        "module main;\n{I}type Map = {{ n: Int }};\n\
         pub fn main() -> Int {{ let m: Map = {{ n: 3 }}; let mut t = 0; \
         for x in [1, 2] {{ t = t + x; }}; print(\"${{t + m.n}}\"); 0 }}\n"
    );
    assert_clean(&[("main.kh", &main)]);
}

/// An own `Map` in a file with no loop that imports only `print`.
#[test]
fn an_own_map_with_no_loop() {
    assert_clean(&[(
        "main.kh",
        "module main;\nimport std::core::{print};\ntype Map = { n: Int };\n\
         pub fn main() -> Int { let m: Map = { n: 3 }; print(\"${m.n}\"); 0 }\n",
    )]);
}

/// An own `Map` in a file that imports nothing at all.
#[test]
fn an_own_map_with_no_imports() {
    assert_clean(&[(
        "main.kh",
        "module main;\n\ntype Map = { n: Int };\n\n\
         pub fn main() -> Int { let m: Map = { n: 3 }; m.n }\n",
    )]);
}

/// An own `Dict` of `std`'s arity, matched, beside a `List` loop.
#[test]
fn an_own_dict_of_the_same_arity() {
    let main = format!(
        "module main;\n{I}type Dict<K, V> = | Leaf | One(k: K, v: V);\n\
         pub fn main() -> Int {{ let d: Dict<Int, Int> = Dict::One(1, 2); let mut t = 0; \
         for x in [1] {{ t = t + x; }}; match d {{ Dict::One(k, v) => print(\"${{k + v + t}}\"), \
         Dict::Leaf => () }}; 0 }}\n"
    );
    assert_clean(&[("main.kh", &main)]);
}

/// An own `Array` is refused on its own terms -- the name is the compiler's
/// -- and with nothing besides: no error about `std`'s `Iterable for Array`
/// being judged against it.
#[test]
fn an_own_array_gets_one_error_about_its_name() {
    let main = format!(
        "module main;\n{I}type Array = | Empty;\n\
         pub fn main() -> Int {{ let a = Array::Empty; let mut t = 0; \
         for x in [1, 2] {{ t = t + x; }}; match a {{ Array::Empty => print(\"${{t}}\") }}; 0 }}\n"
    );
    let found = diagnostics(&[("main.kh", &main)]);
    assert_eq!(found.len(), 1, "{found:#?}");
    assert!(found[0].contains("`Array` is a name the compiler already means"), "{found:#?}");
}

/// An own generic `Vector` built on `std`'s `Array`.
#[test]
fn an_own_generic_vector() {
    assert_clean(&[(
        "main.kh",
        "module main;\nimport std::core::{print, Array};\n\n\
         type Vector<A> = { items: Array<A>, n: Int };\n\n\
         pub fn main() -> Int { let v: Vector<Int> = { items: Array::from_fn(2, fn i => i), n: 2 }; \
         print(\"${v.n}\"); 0 }\n",
    )]);
}

/// An own trait named `Iterable`, beside a `List` loop.
#[test]
fn an_own_trait_named_iterable_beside_a_loop() {
    let main = format!(
        "module main;\n{I}trait Iterable {{ fn iter(self) -> Int; }}\ntype Bag = {{ n: Int }};\n\
         impl Iterable for Bag {{ fn iter(self) -> Int {{ self.n }} }}\n\
         pub fn main() -> Int {{ let b: Bag = {{ n: 4 }}; let mut t = 0; \
         for x in [1, 2] {{ t = t + x; }}; print(\"${{t}} ${{b.iter()}}\"); 0 }}\n"
    );
    assert_clean(&[("main.kh", &main)]);
}

/// An own trait named `Iterable` in a file with no loop.
#[test]
fn an_own_trait_named_iterable_with_no_loop() {
    assert_clean(&[(
        "main.kh",
        "module main;\nimport std::core::{print};\n\
         trait Iterable { fn iter(self) -> Int; }\ntype Bag = { n: Int };\n\
         impl Iterable for Bag { fn iter(self) -> Int { self.n } }\n\
         pub fn main() -> Int { let b: Bag = { n: 4 }; print(\"${b.iter()}\"); 0 }\n",
    )]);
}

/// **`v.iter()` means the program's own trait** when the program never
/// imported `std`'s `Iterable`.
#[test]
fn an_own_iter_method_on_std_vector_is_not_ambiguous() {
    assert_clean(&[(
        "main.kh",
        "module main;\nimport std::core::{Vector, List, print};\n\
         trait Mine { fn iter(self) -> Int; }\n\
         impl<A> Mine for Vector<A> { fn iter(self) -> Int { Vector::length(self) } }\n\
         pub fn main() -> Int { let v: Vector<Int> = Vector::from_list([1, 2, 3]); \
         print(\"${v.iter()}\"); 0 }\n",
    )]);
}

const GEO: &str = "module geo;\n\npub type Map = { pub w: Int, pub h: Int };\n\n\
                   pub fn area(m: Map) -> Int { m.w * m.h }\n";
const GEO_CORE: &str = "module geo;\nimport std::core::{print};\n\n\
                        pub type Map = { pub w: Int, pub h: Int };\n\n\
                        pub fn area(m: Map) -> Int { print(\"area\"); m.w * m.h }\n";
const GEO_PRIVATE: &str = "module geo;\nimport std::core::{print};\n\n\
                           pub type Map = { w: Int, h: Int };\n\n\
                           pub fn area(m: Map) -> Int { print(\"area\"); m.w * m.h }\n";
const NAMED: &str = "module main;\nimport std::core::{print};\nimport geo::{Map, area};\n\n\
                     pub fn main() -> Int { let m: Map = { w: 2, h: 3 }; print(\"${area(m)}\"); 0 }\n";
const UNNAMED: &str = "module main;\nimport std::core::{print};\nimport geo::{area};\n\n\
                       pub fn main() -> Int { print(\"${area({ w: 2, h: 3 })}\"); 0 }\n";

/// A `Map` declared in another module of the program, named by the importer.
#[test]
fn a_map_from_another_module_named_by_the_importer() {
    for geo in [GEO, GEO_CORE] {
        assert_clean(&[("main.kh", NAMED), ("geo.kh", geo)]);
    }
}

/// The same `Map`, reached only through a function the importer calls.
#[test]
fn a_map_from_another_module_never_named() {
    for geo in [GEO, GEO_CORE] {
        assert_clean(&[("main.kh", UNNAMED), ("geo.kh", geo)]);
    }
}

/// With private fields the importer is refused for building one, which is
/// the privacy rule -- and `geo`, which declares it, is told nothing.
#[test]
fn a_map_with_private_fields_is_refused_for_privacy_alone() {
    for main in [NAMED, UNNAMED] {
        let found = diagnostics(&[("main.kh", main), ("geo.kh", GEO_PRIVATE)]);
        assert_eq!(found.len(), 1, "{found:#?}");
        assert!(found[0].starts_with("main.kh: cannot build `Map` here"), "{found:#?}");
    }
}

/// **An own `Vector` made walkable with an own `impl Iterable`.** The
/// feature's own route for a type of the program's, under a name `std` also
/// uses: the impl is the program's and nothing of `std`'s may stand in for it.
#[test]
fn an_own_vector_with_its_own_iterable_impl_is_walked() {
    assert_clean(&[(
        "main.kh",
        "module main;\nimport std::core::{Iterator, Iterable, Step, List, print};\n\n\
         type Vector<A> = { xs: List<A> };\n\
         impl<A> Iterable for Vector<A> { type Cursor = List<A>; fn iter(self) -> List<A> { self.xs } }\n\n\
         pub fn main() -> Int { let v: Vector<Int> = { xs: [4, 5] }; let mut t = 0; \
         for x in v { t = t + x; }; print(\"${t}\"); 0 }\n",
    )]);
}

/// And a loop over an own `Dict` that is neither, refused as such -- never
/// walked with `std`'s cursor.
#[test]
fn a_loop_over_an_own_dict_is_refused_not_walked_as_stds() {
    let found = diagnostics(&[(
        "main.kh",
        "module main;\nimport std::core::{Iterator, Step, print};\n\n\
         type Dict<K, V> = | Leaf | One(k: K, v: V);\n\n\
         pub fn main() -> Int { let d: Dict<Int, Int> = Dict::One(1, 2); let mut t = 0; \
         for _x in d { t = t + 1; }; print(\"${t}\"); 0 }\n",
    )]);
    assert_eq!(found.len(), 1, "{found:#?}");
    assert!(found[0].contains("`Dict<Int, Int>` is neither"), "{found:#?}");
}

/// **A file with its own `Iterable` cannot walk `std`'s collections**, and is
/// told why in one sentence rather than "`Vector<Int>` does not implement
/// `Iterable`": the loop calls `Iterable::iter`, which in this file is the
/// program's own trait. Not a regression -- such a loop did not compile
/// before `std` had an `Iterable` -- and the trait itself still compiles.
#[test]
fn a_loop_over_a_std_vector_beside_an_own_iterable_says_why_it_is_refused() {
    let found = diagnostics(&[(
        "main.kh",
        "module main;\nimport std::core::{Iterator, Step, Vector, List, print};\n\
         trait Iterable { fn iter(self) -> Int; }\ntype Bag = { n: Int };\n\
         impl Iterable for Bag { fn iter(self) -> Int { self.n } }\n\
         pub fn main() -> Int { let b: Bag = { n: 4 }; let v: Vector<Int> = Vector::from_list([1, 2, 3]); \
         let mut t = 0; for x in v { t = t + x; }; print(\"${t} ${b.iter()}\"); 0 }\n",
    )]);
    assert_eq!(found.len(), 1, "{found:#?}");
    assert!(found[0].contains("declares an `Iterable` of its own"), "{found:#?}");
}

const LIB: &str = "module lib;\nimport std::core::{Vector, List};\n\
                   pub fn make() -> Vector<Int> { Vector::from_list([1, 2, 3, 4]) }\n";

/// **An own `Vector` that is an `Iterator` does not make `std`'s one.** The
/// two have one name and two homes; asked whether `std`'s `Vector` is an
/// `Iterator`, an impl search that fell back on the name answered yes with
/// the program's impl, and the loop called `next` on a `std::core::Vector`.
#[test]
fn an_own_vector_iterator_does_not_answer_for_stds_vector() {
    let main = format!(
        "module main;\n{I}import lib::{{make}};\ntype Vector = {{ left: Int }};\n\
         impl Iterator for Vector {{\n  type Item = Int;\n  type Effects = {{}};\n  \
         fn next(self) -> Step<Vector, Int> {{ if self.left == 0 {{ Step::Done }} else \
         {{ Step::Yield({{ left: self.left - 1 }}, self.left) }} }}\n}}\n\
         pub fn main() -> Int {{ let mut t = 0; for x in make() {{ t = t + x; }}; \
         let v: Vector = {{ left: 3 }}; for y in v {{ t = t + y; }}; print(\"${{t}}\"); 0 }}\n"
    );
    assert_clean(&[("main.kh", &main), ("lib.kh", LIB)]);
}

const CARDS: &str = "module cards;\nimport std::core::{Iterable, Vector, VectorItems, List};\n\
                     pub type Deck = { pub cards: Vector<Int> };\n\
                     impl Iterable for Deck {\n  type Cursor = VectorItems<Int>;\n  \
                     fn iter(self) -> VectorItems<Int> { Vector::iter(self.cards) }\n}\n\
                     pub fn deck() -> Deck { { cards: Vector::from_list([7, 8]) } }\n";

/// **An impl of `std`'s `Iterable` that rides in with its type is not an
/// impl of a file's own `Iterable`.** `cards` makes `Deck` walkable with
/// `std`'s trait; a file declaring a trait of the same name and importing
/// `Deck` had that impl judged against its own trait.
#[test]
fn an_imported_impl_of_stds_iterable_is_not_judged_against_an_own_one() {
    assert_clean(&[
        (
            "main.kh",
            "module main;\nimport std::core::{print, Vector};\nimport cards::{Deck, deck};\n\
             trait Iterable { fn iter(self) -> Int; }\ntype Bag = { n: Int };\n\
             impl Iterable for Bag { fn iter(self) -> Int { self.n } }\n\
             pub fn main() -> Int { let d: Deck = deck(); let b: Bag = { n: 4 }; \
             print(\"${b.iter()} ${Vector::length(d.cards)}\"); 0 }\n",
        ),
        ("cards.kh", CARDS),
    ]);
}

/// And `d.iter()` there is a call to this file's `Iterable`, which `Deck`
/// does not implement: refused, rather than checked against this file's
/// signature and emitted as a call to `cards`'s body for the other trait.
#[test]
fn a_call_through_an_own_iterable_does_not_reach_an_impl_of_stds() {
    let found = diagnostics(&[
        (
            "main.kh",
            "module main;\nimport std::core::{print, Vector};\nimport cards::{Deck, deck};\n\
             trait Iterable { fn iter(self) -> Int; }\n\
             pub fn main() -> Int { let d: Deck = deck(); print(\"${d.iter()}\"); 0 }\n",
        ),
        ("cards.kh", CARDS),
    ]);
    assert_eq!(found.len(), 1, "{found:#?}");
    assert!(found[0].starts_with("main.kh: `Deck` does not implement `Iterable`"), "{found:#?}");
}

/// **The rule is about any trait, not `std`'s.** `shapes` implements its own
/// `Area` for `Square`; a file that imports `Square` and declares an `Area`
/// of its own got that impl with the type and judged it against the wrong
/// trait: "`area` returns `Int` here, but `Area` declares `String`", about a
/// declaration in another module, for a program that never mixed the two.
#[test]
fn an_impl_riding_in_with_its_type_is_not_judged_against_a_namesake_trait() {
    assert_clean(&[
        (
            "main.kh",
            "module main;\nimport std::core::{print};\nimport shapes::{Square, square};\n\n\
             trait Area { fn area(self) -> String; }\ntype Dot = { n: Int };\n\
             impl Area for Dot { fn area(self) -> String { \"dot\" } }\n\n\
             pub fn main() -> Int { let s: Square = square(); let d: Dot = { n: 1 }; \
             print(\"${d.area()} ${s.side}\"); 0 }\n",
        ),
        (
            "shapes.kh",
            "module shapes;\n\npub trait Area { fn area(self) -> Int; }\n\
             pub type Square = { pub side: Int };\n\
             impl Area for Square { fn area(self) -> Int { self.side * self.side } }\n\
             pub fn square() -> Square { { side: 3 } }\n",
        ),
    ]);
}

/// **Both `Vector`s walked in one file.** The program's own, through its
/// own impl of `std`'s `Iterable`, and `std`'s, handed back by another
/// module. `std`'s impl does not join this file's impls, because here
/// `Vector` names the program's type; the loop over `std`'s one has to find
/// it by module anyway, for its cursor and for the bound its `iter` carries.
#[test]
fn an_own_vector_and_stds_are_both_walked_in_one_file() {
    assert_clean(&[
        (
            "main.kh",
            "module main;\nimport std::core::{Iterator, Iterable, Step, List, print};\n\
             import lib::{make};\n\n\
             type Vector<A> = { xs: List<A> };\n\
             impl<A> Iterable for Vector<A> { type Cursor = List<A>; fn iter(self) -> List<A> { self.xs } }\n\n\
             pub fn main() -> Int { let mine: Vector<Int> = { xs: [100, 200] }; let mut a = 0; \
             for x in mine { a = a + x; }; let mut b = 0; for y in make() { b = b + y; }; \
             print(\"${a} ${b}\"); 0 }\n",
        ),
        ("lib.kh", LIB),
    ]);
}

/// The `traits.md` `Deck`, in a module of its own: a type that lends its
/// collection's cursor, as a library would ship it.
const DECK: &str = "module cards;\nimport std::core::{Iterable, Vector, VectorItems, List};\n\
                    pub type Deck = { pub cards: Vector<Int> };\n\
                    impl Iterable for Deck {\n  type Cursor = VectorItems<Int>;\n  \
                    fn iter(self) -> VectorItems<Int> { Vector::iter(self.cards) }\n}\n\
                    pub fn deck() -> Deck { { cards: Vector::from_list([7, 8]) } }\n";

/// **A program's own `impl Iterable` travels with its type**, as
/// `impl Show for Deck` beside it does. Only `std`'s four impls are kept
/// apart for the loop; treating every impl of `std`'s trait as one of them
/// left `Deck` walkable in `cards` and nowhere else: "`Deck` is neither".
#[test]
fn an_iterable_type_from_another_module_is_walked() {
    assert_clean(&[
        (
            "main.kh",
            "module main;\nimport std::core::{Iterator, Step, print};\nimport cards::{Deck, deck};\n\n\
             pub fn main() -> Int { let d: Deck = deck(); let mut t = 0; \
             for x in d { t = t + x; }; print(\"${t}\"); 0 }\n",
        ),
        ("cards.kh", DECK),
    ]);
}

/// The same `Deck` reached only through the function that makes it is under
/// the import rule every impl is: a type not in scope has none here, as a
/// `${..}` hole says of `Show`. One sentence, naming the import to add.
#[test]
fn an_iterable_type_never_named_asks_for_its_import() {
    let found = diagnostics(&[
        (
            "main.kh",
            "module main;\nimport std::core::{Iterator, Step, print};\nimport cards::{deck};\n\n\
             pub fn main() -> Int { let mut t = 0; for x in deck() { t = t + x; }; print(\"${t}\"); 0 }\n",
        ),
        ("cards.kh", DECK),
    ]);
    assert_eq!(
        found,
        vec!["main.kh: `Deck` is not in scope here, so nothing is known about whether a `for` \
              can walk it -- add it to an `import`"
            .to_string()]
    );
}

/// `d.iter()` and `Iterable::iter(d)` on it, with `Iterable` imported.
#[test]
fn an_iterable_type_from_another_module_answers_iter() {
    assert_clean(&[
        (
            "main.kh",
            "module main;\nimport std::core::{Iterator, Iterable, Step, print};\nimport cards::{Deck, deck};\n\n\
             pub fn main() -> Int { let d: Deck = deck(); let mut t = 0; \
             for x in d.iter() { t = t + x; }; for y in Iterable::iter(d) { t = t + y; }; \
             print(\"${t}\"); 0 }\n",
        ),
        ("cards.kh", DECK),
    ]);
}

/// **The refusal says where the other `Iterable` comes from.** Here the file
/// declares nothing: it imports another module's trait, and the remedy is
/// the import. The message used to tell it to rename "this file's
/// `Iterable`", which does not exist.
#[test]
fn a_loop_beside_an_imported_namesake_iterable_names_the_import() {
    let found = diagnostics(&[
        (
            "main.kh",
            "module main;\nimport std::core::{Iterator, Step, Vector, List, print};\n\
             import t::{Iterable, bag};\n\n\
             pub fn main() -> Int { let b = bag(); let v: Vector<Int> = Vector::from_list([1, 2, 3]); \
             let mut t = 0; for x in v { t = t + x; }; print(\"${t} ${b.iter()}\"); 0 }\n",
        ),
        (
            "t.kh",
            "module t;\n\npub trait Iterable { fn iter(self) -> Int; }\npub type Bag = { pub n: Int };\n\
             impl Iterable for Bag { fn iter(self) -> Int { self.n } }\n\
             pub fn bag() -> Bag { { n: 4 } }\n",
        ),
    ]);
    assert_eq!(found.len(), 1, "{found:#?}");
    assert!(found[0].contains("this file imports another `Iterable`, from `t`"), "{found:#?}");
    assert!(found[0].contains("Leave `Iterable` out of that import"), "{found:#?}");
    assert!(!found[0].contains("declares"), "{found:#?}");
}
