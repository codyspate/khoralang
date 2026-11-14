//! Bodiless methods on the types whose operations code generation lowers by
//! name: which `khora check` refuses and which it leaves alone.
//!
//! **What these guard: `check` and `build` disagreeing about one program.**
//! `check` left every bodiless method on `String`, `Ptr`, `Char` and `Float`
//! to the build, so a program's own `impl String { fn data(self) -> Ptr; }`
//! checked clean and failed to build. The other half matters as much: the
//! methods code generation does lower (`Ptr::null`, `String::byte_length`)
//! may be declared bodiless by a program compiled without std -- the code
//! generator's own tests are written that way -- and refusing those would be
//! a disagreement in the other direction.

use std::path::PathBuf;

use khora_db::{KhoraDatabase, SourceFile, SourceRoot};

/// Every diagnostic `khora check` gives this one file, compiled without std.
fn diagnostics(text: &str) -> Vec<String> {
    let db = KhoraDatabase::new();
    let file = SourceFile::new(&db, PathBuf::from("main.kh"), text.to_string());
    SourceRoot::new(&db, vec![file]);
    khora_types::diagnostics(&db, file).iter().map(|e| e.message.clone()).collect()
}

#[test]
fn a_bodiless_method_code_generation_does_not_lower_is_refused() {
    for (decl, call, name) in [
        ("impl String {\n  fn data(self) -> Ptr;\n}", "String::data(\"hi\")", "String::data"),
        ("impl String {\n  fn data(self) -> Ptr;\n}", "\"hi\".data()", "String::data"),
        (
            "impl Ptr {\n  fn null() -> Ptr;\n  fn twice(self) -> Ptr;\n}",
            "Ptr::twice(Ptr::null())",
            "Ptr::twice",
        ),
        ("impl Float {\n  fn half(self) -> Float;\n}", "Float::half(1.0)", "Float::half"),
        ("impl Char {\n  fn upper(self) -> Char;\n}", "Char::upper('a')", "Char::upper"),
    ] {
        let found = diagnostics(&format!("module m;\n{decl}\npub fn main() -> Int {{ let x = {call}; 0 }}\n"));
        let wanted = format!("`{name}` has no body, so there is nothing to call");
        assert!(found.iter().any(|e| e.contains(&wanted)), "{call}: {found:?}");
    }
}

#[test]
fn every_method_code_generation_lowers_by_name_may_be_bodiless() {
    let found = diagnostics(
        "module m;\n\
         impl Ptr {\n  fn null() -> Ptr;\n  fn is_null(self) -> Bool;\n}\n\
         impl String {\n  fn byte_length(self) -> Int;\n  fn byte(self, at: Int) -> Int;\n}\n\
         impl Char {\n  fn code(self) -> Int;\n}\n\
         pub fn main() -> Int {\n  \
           if Ptr::is_null(Ptr::null()) { String::byte_length(\"hi\") + \"hi\".byte(0) + Char::code('a') } else { 0 }\n\
         }\n",
    );
    assert!(found.iter().all(|e| !e.contains("has no body")), "{found:?}");
}

/// **A bodiless method in a *trait* impl is refused the same way, at the
/// call.** `impl Size for String { fn size(self) -> Int; }` checked clean and
/// failed to build with "`Size#String::size` has no body", the internal key,
/// located at a doc comment in an unrelated std file. On `String`, `Float`
/// and a record, by method call and by trait path.
#[test]
fn a_bodiless_method_in_a_trait_impl_is_refused_at_the_call() {
    let trait_ = "trait Size {\n  fn size(self) -> Int;\n}";
    for (owner, value) in [("String", "\"ab\""), ("Float", "1.5"), ("W", "w")] {
        for call in [format!("{value}.size()"), format!("Size::size({value})")] {
            let found = diagnostics(&format!(
                "module m;\n{trait_}\ntype W = {{ n: Int }};\nimpl Size for {owner} {{\n  fn size(self) -> Int;\n}}\n\
                 pub fn main() -> Int {{ let w: W = {{ n: 1 }}; {call} }}\n"
            ));
            let wanted = format!("`{owner}::size` has no body, so there is nothing to call");
            assert!(found.iter().any(|e| e.contains(&wanted)), "{owner}, {call}: {found:?}");
        }
    }
}

/// Not refused: a trait impl's method with a body, and one the impl leaves
/// out so the trait's default body runs.
#[test]
fn a_trait_method_with_a_body_or_a_default_is_not_refused() {
    let found = diagnostics(
        "module m;\n\
         trait Size {\n  fn size(self) -> Int;\n  fn twice(self) -> Int { 2 }\n}\n\
         impl Size for String {\n  fn size(self) -> Int { 1 }\n}\n\
         pub fn main() -> Int { \"ab\".size() + Size::size(\"ab\") + \"ab\".twice() }\n",
    );
    assert!(found.iter().all(|e| !e.contains("has no body")), "{found:?}");
}
