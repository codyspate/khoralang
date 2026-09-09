//! The second pair: the key an impl's body is filed under, built twice.
//!
//! `khora_hir::body::impl_key` names the body. `khora_types::traits::method_key`
//! names the signature. They are two crates deriving one string from one piece
//! of syntax, and nothing but reading them side by side has ever said they
//! still match -- so when trait arguments were added to the key, both halves
//! had to be changed by hand and in step.
//!
//! They also do not derive it the same way, which is what makes this worth a
//! gate rather than a comment. The trait half is shared: `khora-types` calls
//! `khora_hir::body::trait_key`. The *type* half is not. Lowering reads the
//! head straight off the syntax; the checker builds a `Type` first and takes
//! the head of that. Two routes to one half of one key, and an alias, a
//! generic head or a const-generic argument is where routes like that part
//! company.
//!
//! # What is asserted
//!
//! For each program in [`CASES`], that the set of keys carrying a `#` is the
//! same on both sides -- every body has a signature filed under exactly its
//! own key, and no signature is filed under a key no body answers to. A body
//! with no signature is a method the checker cannot see; a signature with no
//! body is a call that type-checks and links to nothing.
//!
//! `expected` pins the keys as well, because a rule that only says "the two
//! sides agree" also passes when both sides produce nothing.
//!
//! # Adding another pair
//!
//! One test, one shared list of cases, and a line in
//! `scripts/check-agreement.sh`. See the header of
//! `crates/khora-codegen-llvm/tests/agreement.rs`, which is the first pair and
//! the one that needed the cases shared across two crates.

use std::collections::BTreeSet;

use khora_db::{KhoraDatabase, SourceFile};

/// One program, and the impl-method keys it must produce on both sides.
struct Case {
    source: &'static str,
    expected: &'static [&'static str],
    why: &'static str,
}

const CASES: &[Case] = &[
    Case {
        source: "module m;\n\
                 pub trait Show { fn show(self) -> String; }\n\
                 impl Show for Int { fn show(self) -> String { \"i\" } }\n",
        expected: &["Show#Int::show"],
        why: "the ordinary shape: a trait, a builtin head",
    },
    Case {
        source: "module m;\n\
                 pub type User = { n: Int };\n\
                 impl User { fn f(self) -> Int { 1 } }\n",
        expected: &["#User::f"],
        why: "an inherent impl has an empty trait half and still keys with a `#`",
    },
    Case {
        source: "module m;\n\
                 pub trait Conv<A> { fn to(self) -> A; }\n\
                 impl Conv<String> for Int { fn to(self) -> String { \"x\" } }\n\
                 impl Conv<Bool> for Int { fn to(self) -> Bool { true } }\n",
        expected: &["Conv<String>#Int::to", "Conv<Bool>#Int::to"],
        why: "**the reason the key grew a trait-argument half**: keyed as `Conv#Int` \
              these two impls recorded two bodies under one name and whichever was \
              lowered first ran for both",
    },
    Case {
        source: "module m;\n\
                 pub trait Conv<A> { fn to(self) -> A; }\n\
                 impl Conv< String > for Int { fn to(self) -> String { \"x\" } }\n",
        expected: &["Conv<String>#Int::to"],
        why: "the argument half is source text with the whitespace taken out, so \
              how the author spaced it must not reach the key",
    },
    Case {
        source: "module m;\n\
                 pub trait Conv<A> { fn to(self) -> A; }\n\
                 impl Conv<Option<Int>> for Int { fn to(self) -> Option<Int> { Option::None } }\n",
        expected: &["Conv<Option<Int>>#Int::to"],
        why: "a nested argument, where a matcher counting angle brackets would stop early",
    },
    Case {
        source: "module m;\n\
                 pub type Id = Int;\n\
                 pub trait Show { fn show(self) -> String; }\n\
                 impl Show for Id { fn show(self) -> String { \"i\" } }\n",
        expected: &["Show#Id::show"],
        why: "**an alias is where the two routes could part company.** Lowering reads \
              `Id` off the syntax; the checker builds a type first, and a type that \
              resolved the alias would file the signature under `Show#Int` while the \
              body sat under `Show#Id`",
    },
    Case {
        source: "module m;\n\
                 pub trait Show { fn show(self) -> String; }\n\
                 pub type Box<A> = { v: A };\n\
                 impl<A> Show for Box<A> { fn show(self) -> String { \"b\" } }\n",
        expected: &["Show#Box::show"],
        why: "a generic head keys by its head alone -- instance selection is nominal",
    },
    Case {
        source: "module m;\n\
                 pub trait Show { fn show(self) -> String; }\n\
                 impl Show for Array<Int, 3> { fn show(self) -> String { \"a\" } }\n",
        expected: &["Show#Array::show"],
        why: "a const-generic argument is part of the type and not of the key",
    },
];

/// The impl-method keys lowering files bodies under.
///
/// `#` is the punctuation `impl_key` uses and cannot occur in a Khora
/// identifier, so it is what separates an impl method from a plain function
/// without this test having to know how either is spelled.
fn body_keys(db: &KhoraDatabase, file: SourceFile) -> BTreeSet<String> {
    khora_hir::body::bodies(db, file)
        .iter()
        .map(|(key, _)| key.clone())
        .filter(|key| key.contains('#'))
        .collect()
}

/// The impl-method keys the checker files signatures under.
fn signature_keys(db: &KhoraDatabase, file: SourceFile) -> BTreeSet<String> {
    khora_types::type_map(db, file)
        .signatures
        .keys()
        .filter(|key| key.contains('#'))
        .cloned()
        .collect()
}

/// **The body's key and the signature's key are the same string.**
#[test]
fn lowering_and_the_checker_key_an_impl_method_the_same_way() {
    let mut wrong = Vec::new();
    for case in CASES {
        let db = KhoraDatabase::new();
        let file = SourceFile::new(&db, "a.kh".into(), case.source.to_string());
        let bodies = body_keys(&db, file);
        let signatures = signature_keys(&db, file);
        let expected: BTreeSet<String> = case.expected.iter().map(|k| (*k).to_string()).collect();

        if bodies != signatures {
            wrong.push(format!(
                "  bodies and signatures are filed under different keys:\n    \
                 khora_hir::body::impl_key gave {bodies:?}\n    \
                 khora_types::traits::method_key gave {signatures:?}\n    {}\n{}",
                case.why, case.source
            ));
        } else if bodies != expected {
            wrong.push(format!(
                "  both sides agree and neither says what this case is for:\n    \
                 expected {expected:?}, both gave {bodies:?}\n    {}\n{}",
                case.why, case.source
            ));
        }
    }
    assert!(wrong.is_empty(), "the two key builders have diverged:\n{}", wrong.join("\n"));
}
