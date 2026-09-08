//! Which instantiated types may be held without a header.
//!
//! Read against `std` itself rather than a fixture, because the claim is about
//! the shapes the standard library actually has -- and because the type this
//! exists for, `Step`, is declared there.
use khora_types::Type;

fn merged() -> (khora_db::KhoraDatabase, khora_types::TypeMap) {
    let db = khora_db::KhoraDatabase::new();
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..").join("std");
    let mut files = Vec::new();
    let mut stack = vec![root];
    while let Some(here) = stack.pop() {
        for entry in std::fs::read_dir(&here).expect("std") {
            let path = entry.expect("entry").path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|e| e == "kh")
                && khora_db::selected_for_target(&path, khora_db::host_target())
            {
                let text = std::fs::read_to_string(&path).expect("readable");
                files.push(khora_db::SourceFile::new(&db, path, text));
            }
        }
    }
    let mut map = khora_types::TypeMap::default();
    for f in &files {
        let m = khora_types::type_map(&db, *f);
        map.variants.extend(m.variants.iter().cloned());
        for (k, v) in &m.adts_in {
            map.adts_in.entry(k.clone()).or_insert_with(|| v.clone());
        }
        for (k, v) in &m.adts {
            map.adts.entry(k.clone()).or_insert_with(|| v.clone());
        }
    }
    (db, map)
}

fn core(name: &str, args: Vec<Type>) -> Type {
    Type::Adt {
        name: name.to_string(),
        home: Some(khora_hir::ModulePath::new(vec!["std".into(), "core".into()])),
        args,
    }
}

/// **The answer depends on the arguments, not the declaration.**
///
/// `Step<S, A>` has two fields and both are type parameters, which have no
/// width. Every generic type would be refused if the question were asked of
/// the declaration -- which is every type worth asking about.
///
/// `Step<List<Int>, Int>` is the case the whole thing is for: a `List` cannot
/// be inline, so that field is a pointer, so the `Step` around it is three
/// words and needs no header. The roadmap's original criterion was all-scalar
/// fields and would have refused it.
#[test]
fn the_shapes_iteration_needs() {
    let (_db, map) = merged();
    let u = khora_types::unboxed::decide(&map, khora_types::unboxed::Fields::Any);

    let list_int = core("List", vec![Type::Int]);
    let range = core("Range", vec![]);
    let cases: Vec<(&str, Type)> = vec![
        ("Range", range.clone()),
        ("Option<Int>", core("Option", vec![Type::Int])),
        ("Step<Range, Int>", core("Step", vec![range.clone(), Type::Int])),
        ("Step<List<Int>, Int>", core("Step", vec![list_int.clone(), Type::Int])),
        ("List<Int>  (recursive)", list_int.clone()),
        ("Pair<String, Int>", core("Pair", vec![Type::Str, Type::Int])),
    ];
    for (label, ty) in &cases {
        eprintln!("{:24} {}", label, if u.holds(ty) { "UNBOXED" } else { "boxed" });
    }
    assert!(u.holds(&range), "Range is two Ints in one case");
    assert!(u.holds(&core("Step", vec![range, Type::Int])), "Step over a Range");
    assert!(u.holds(&core("Step", vec![list_int.clone(), Type::Int])), "Step over a List");
    assert!(!u.holds(&list_int), "a List holds a List, so it cannot be inline");
    assert!(u.holds(&core("Option", vec![Type::Int])), "one case carries, one does not");
}

/// A record has one shape, so there is nothing to discriminate.
///
/// Worth a test of its own because it is the difference between
/// `Step<Range, Int>` fitting in four words and not: `Range` is two integers
/// rather than a tag and two integers.
#[test]
fn a_single_case_type_carries_no_tag() {
    let (_db, map) = merged();
    let u = khora_types::unboxed::decide(&map, khora_types::unboxed::Fields::Any);
    let range = core("Range", vec![]);
    assert_eq!(u.payload(&range).map(|f| f.len()), Some(2), "`from` and `to`");
    assert!(u.holds(&core("Step", vec![range, Type::Int])), "a tag, a Range and an item");
}

/// A `mut` field is observable identity, so the value stays behind a pointer.
///
/// The one criterion here that is about meaning rather than layout: a write
/// through one holder has to be seen by another, and passing a copy loses it.
/// `Fields::Scalars` is the staged half: laid out flat, nothing inside to
/// count. `Step<List<Int>, Int>` holds a pointer, so it waits for `Any`.
#[test]
fn the_scalar_only_stage_takes_the_flat_ones() {
    let (_db, map) = merged();
    let u = khora_types::unboxed::decide(&map, khora_types::unboxed::Fields::Scalars);
    let range = core("Range", vec![]);
    assert!(u.holds(&range), "two integers and nothing counted");
    assert!(u.holds(&core("Step", vec![range, Type::Int])), "a tag and three words");
    assert!(
        !u.holds(&core("Step", vec![core("List", vec![Type::Int]), Type::Int])),
        "holds a `List` pointer, so it waits for the half that counts fields"
    );
    assert!(!u.holds(&core("Pair", vec![Type::Str, Type::Int])), "a `String` is counted");
}

#[test]
fn a_mutable_field_keeps_its_pointer() {
    let (_db, map) = merged();
    let u = khora_types::unboxed::decide(&map, khora_types::unboxed::Fields::Any);
    let mutable: Vec<&khora_types::VariantInfo> =
        map.variants.iter().filter(|v| v.mutable.iter().any(|m| *m)).collect();
    assert!(!mutable.is_empty(), "`std` has at least one, or this proves nothing");
    for v in mutable {
        let ty = Type::Adt { name: v.type_name.clone(), home: v.home.clone(), args: Vec::new() };
        assert!(!u.holds(&ty), "`{}` has a `mut` field and must stay boxed", v.type_name);
    }
}

/// A slot two variants disagree about has to agree about being a pointer.
///
/// **The rule above it admits scalars and pointers into one word, which is
/// true of how a slot is read and false of how it is released.** The
/// reference-counting plan works from a slot's static type and cannot consult
/// the tag, so a slot holding an `Int` under one variant and a counted pointer
/// under the other gets one plan for both -- and `Result<Int, E>` with a boxed
/// `E` is exactly that: `khora check` clean, `khora build` clean, and the
/// program decrementing a refcount through the integer 5. It died with SIGILL
/// on the *success* path, which is the tell: `Ok` is the variant whose word is
/// not a pointer.
///
/// A boxed ADT reaches that slot precisely because it is boxed -- one word,
/// and it fails the "is it laid out flat" test *because* it is a pointer --
/// so nothing before this asked the question. Roadmap 16.6.
#[test]
fn a_shared_slot_may_not_mix_a_pointer_with_a_scalar() {
    let (_db, map) = merged();
    let u = khora_types::unboxed::decide(&map, khora_types::unboxed::Fields::Any);

    let list_int = core("List", vec![Type::Int]);
    assert!(!u.holds(&list_int), "the premise: a List is behind a pointer");

    assert!(
        !u.holds(&core("Result", vec![Type::Int, list_int.clone()])),
        "`Ok` carries an Int and `Err` carries a counted pointer; one slot cannot \
         be released both ways"
    );
    assert!(
        !u.holds(&core("Result", vec![list_int.clone(), Type::Int])),
        "and the same the other way round"
    );

    // The two agreeing cases still hold, because the reason to refuse is the
    // disagreement rather than the pointer.
    assert!(
        u.holds(&core("Result", vec![Type::Int, Type::Bool])),
        "two scalars share a word"
    );
    assert!(
        u.holds(&core("Result", vec![list_int.clone(), list_int])),
        "and two counted pointers share one plan"
    );
}
