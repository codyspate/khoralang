#![cfg(feature = "llvm")]

//! JSON, written in Khora.
//!
//! The encoder walks bytes with `String::byte` and the parser is recursive
//! descent over the same. Nothing in `std/json.kh` is an intrinsic and nothing
//! is foreign, which is the claim these tests are really checking: a format
//! this ordinary should need no help from the compiler.

mod harness;

use std::path::PathBuf;

use khora_db::{KhoraDatabase, SourceFile, SourceRoot};

fn std_source(name: &str) -> String {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("std")
        .join(name);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("reading {}: {e}", path.display()))
}

fn run(name: &str, main: &str) -> String {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(name);
    harness::ensure_runtime();
    std::fs::create_dir_all(&dir).expect("a workspace");
    let exe = dir.join(if cfg!(windows) {
        "program.exe"
    } else {
        "program"
    });
    let _ = std::fs::remove_file(&exe);

    let db = KhoraDatabase::new();
    let main_file = SourceFile::new(&db, dir.join("main.kh"), main.to_string());
    let files = vec![
        SourceFile::new(&db, dir.join("core.kh"), std_source("core.kh")),
        SourceFile::new(&db, dir.join("json.kh"), std_source("json.kh")),
        main_file,
    ];
    let root = SourceRoot::new(&db, files);
    if let Err(errors) = khora_codegen_llvm::compile(&db, root, &exe) {
        let messages: Vec<String> = errors
            .into_iter()
            .map(|e| format!("{:?}: {}", e.range, e.message))
            .collect();
        let mut unknowns = Vec::new();
        let bodies = khora_hir::body::bodies(&db, main_file);
        for ((name, body), (_, types)) in bodies.iter().zip(khora_types::body_types(&db, main_file)) {
            for (id, expr) in body.exprs() {
                if matches!(types.of(id), khora_types::Type::Unknown) {
                    unknowns.push(format!("{name}: {expr:?}"));
                }
            }
        }
        panic!(
            "compiling `{name}` failed:\n  {}\n\nunknowns:\n  {}\n\ngenerated:\n{}",
            messages.join("\n  "),
            unknowns.join("\n  "),
            khora_hir::derive::derived(&db, main_file).source()
        );
    }

    let out = std::process::Command::new(&exe)
        .output()
        .expect("the program should run");
    assert_eq!(out.status.code(), Some(0), "`{name}` did not exit cleanly");
    String::from_utf8_lossy(&out.stdout).replace("\r\n", "\n")
}

const HEAD: &str = "module demo::main;
import std::core::{List, Map, Option, Result};
import std::json::{DecodeError, FromJson, Json, JsonError, ToJson, decode, encode, parse};

fn print(value: String);
extern fn khora_print_int(value: Int);
extern fn khora_live_count() -> Int;

/// The text of a value, or the error's offset and what it wanted there — so a
/// failure reads as plainly as a success in the expected output.
fn shown(text: String) -> String {
  match parse(text) {
    Result::Ok(value) => encode(value),
    Result::Err(why) => \"at \" + Int::to_string(why.at) + \": expected \" + why.expected,
  }
}
";

/// The scalars, out and back.
#[test]
fn the_simple_values_round_trip() {
    let out = run(
        "json_scalars",
        &format!(
            "{HEAD}
fn main() -> Int {{
  print(encode(Json::Null));
  print(encode(Json::Bool(true)));
  print(encode(Json::Bool(false)));
  print(encode(Json::Number(1.5)));
  print(encode(Json::Text(\"khora\")));
  print(shown(\"null\"));
  print(shown(\"true\"));
  print(shown(\"false\"));
  print(shown(\"1.5\"));
  print(shown(\"\\\"khora\\\"\"));
  khora_print_int(khora_live_count());
  0
}}
"
        ),
    );
    assert_eq!(
        out,
        "null\ntrue\nfalse\n1.5\n\"khora\"\nnull\ntrue\nfalse\n1.5\n\"khora\"\n0\n"
    );
}

/// Numbers are the part of JSON with the most corners: a sign, a fraction, an
/// exponent, and the interaction of all three.
#[test]
fn numbers_parse_in_every_shape() {
    let out = run(
        "json_numbers",
        &format!(
            "{HEAD}
fn main() -> Int {{
  print(shown(\"0\"));
  print(shown(\"42\"));
  print(shown(\"-42\"));
  print(shown(\"3.25\"));
  print(shown(\"-0.5\"));
  print(shown(\"1e3\"));
  print(shown(\"1.5e2\"));
  print(shown(\"1E+2\"));
  print(shown(\"1500e-3\"));
  khora_print_int(khora_live_count());
  0
}}
"
        ),
    );
    assert_eq!(out, "0\n42\n-42\n3.25\n-0.5\n1000\n150\n100\n1.5\n0\n");
}

/// Every escape the format has, in both directions. The `\u00XX` form is what
/// a control character becomes going out, and the short forms are what come
/// back.
#[test]
fn strings_escape_and_unescape() {
    let out = run(
        "json_escapes",
        &format!(
            "{HEAD}
fn main() -> Int {{
  print(encode(Json::Text(\"a\\\"b\")));
  print(encode(Json::Text(\"a\\\\b\")));
  print(encode(Json::Text(\"a\\nb\")));
  print(encode(Json::Text(\"a\\tb\")));
  // A URL keeps its slashes: `\\/` is legal and nothing requires it.
  print(encode(Json::Text(\"http://x/y\")));
  print(shown(\"\\\"a\\\\\\\"b\\\"\"));
  print(shown(\"\\\"a\\\\nb\\\"\"));
  print(shown(\"\\\"a\\\\u0041b\\\"\"));
  khora_print_int(khora_live_count());
  0
}}
"
        ),
    );
    assert_eq!(
        out,
        "\"a\\\"b\"\n\"a\\\\b\"\n\"a\\nb\"\n\"a\\tb\"\n\"http://x/y\"\n\"a\\\"b\"\n\"a\\nb\"\n\"aAb\"\n0\n",
        "the last three are parsed and re-encoded, so an escape survives a round trip"
    );
}

/// A control character has no literal spelling in JSON, so it goes out as
/// `\u00XX` — and a document that arrives with one written that way comes back
/// the same.
#[test]
fn control_characters_become_unicode_escapes() {
    let out = run(
        "json_control",
        &format!(
            "{HEAD}
fn main() -> Int {{
  print(shown(\"\\\"a\\\\u0001b\\\"\"));
  print(shown(\"\\\"\\\\u001f\\\"\"));
  khora_print_int(khora_live_count());
  0
}}
"
        ),
    );
    assert_eq!(out, "\"a\\u0001b\"\n\"\\u001f\"\n0\n");
}

/// Text outside the basic plane arrives as a surrogate pair and has to be
/// joined; a parser that stops at the first half cannot read an emoji.
#[test]
fn a_surrogate_pair_becomes_one_character() {
    let out = run(
        "json_surrogates",
        &format!(
            "{HEAD}
fn main() -> Int {{
  // U+00E9, then U+4E2D, then U+1F600 as a pair.
  print(shown(\"\\\"caf\\\\u00e9\\\"\"));
  print(shown(\"\\\"\\\\u4e2d\\\"\"));
  print(shown(\"\\\"\\\\ud83d\\\\ude00\\\"\"));
  print(shown(\"\\\"\\\\ud83d\\\"\"));
  khora_print_int(khora_live_count());
  0
}}
"
        ),
    );
    assert_eq!(
        out,
        "\"café\"\n\"中\"\n\"😀\"\nat 7: expected the low half of a surrogate pair\n0\n"
    );
}

/// Nesting, and the empty cases either side of it.
#[test]
fn arrays_and_objects_nest() {
    let out = run(
        "json_nesting",
        &format!(
            "{HEAD}
fn main() -> Int {{
  print(shown(\"[]\"));
  print(shown(\"{{}}\"));
  print(shown(\"[1,2,3]\"));
  print(shown(\"[ 1 , [ 2 , [ 3 ] ] ]\"));
  print(shown(\"{{\\\"a\\\":1}}\"));
  print(shown(\"[{{\\\"a\\\":[true,null]}}]\"));
  khora_print_int(khora_live_count());
  0
}}
"
        ),
    );
    assert_eq!(
        out, "[]\n{}\n[1,2,3]\n[1,[2,[3]]]\n{\"a\":1}\n[{\"a\":[true,null]}]\n0\n",
        "whitespace is skipped and the order of an array is kept"
    );
}

/// Reaching into a document is what a consumer actually does with one.
#[test]
fn a_document_can_be_read_field_by_field() {
    let out = run(
        "json_access",
        &format!(
            "{HEAD}
fn main() -> Int {{
  match parse(\"{{\\\"name\\\":\\\"ada\\\",\\\"age\\\":36,\\\"tags\\\":[\\\"x\\\"],\\\"ok\\\":true}}\") {{
    Result::Err(why) => print(\"failed\"),
    Result::Ok(doc) => {{
      print(Json::field(doc, \"name\").unwrap_or(Json::Null).text().unwrap_or(\"?\"));
      khora_print_int(Float::to_int(
        Json::field(doc, \"age\").unwrap_or(Json::Null).number().unwrap_or(0.0)));
      khora_print_int(
        if Json::field(doc, \"ok\").unwrap_or(Json::Null).boolean().unwrap_or(false) {{ 1 }}
        else {{ 0 }});
      khora_print_int(List::length(
        Json::field(doc, \"tags\").unwrap_or(Json::Null).items().unwrap_or(List::Nil)));
      khora_print_int(if Json::field(doc, \"absent\").is_none() {{ 1 }} else {{ 0 }});
      khora_print_int(List::length(Json::entries(doc)));
    }},
  }};
  khora_print_int(khora_live_count());
  0
}}
"
        ),
    );
    // The trailing zero was a `1` when this was written, and pinning it is how
    // errata 44 was found: matching an `Option<Bool>` appeared to leak, and
    // underneath was a variant payload narrower than a word being read as an
    // `i64` and stored into its own smaller slot — over the frame, over the
    // scrutinee, so the release at the end of the `match` dropped a pointer
    // that had been overwritten.
    //
    // Left as a zero with the story attached, because this is the shape of
    // program that found it.
    assert_eq!(out, "ada\n36\n1\n1\n1\n4\n0\n");
}

/// A malformed document is reported at the byte where it went wrong, which is
/// the only part of an error message a caller can act on.
#[test]
fn malformed_input_is_reported_at_its_offset() {
    let out = run(
        "json_malformed",
        &format!(
            "{HEAD}
fn main() -> Int {{
  print(shown(\"\"));
  print(shown(\"[1,]\"));
  print(shown(\"[1 2]\"));
  print(shown(\"{{\\\"a\\\" 1}}\"));
  print(shown(\"{{\\\"a\\\":}}\"));
  print(shown(\"nul\"));
  print(shown(\"1.\"));
  print(shown(\"1e\"));
  print(shown(\"\\\"unterminated\"));
  print(shown(\"{{\\\"a\\\":1}} oops\"));
  khora_print_int(khora_live_count());
  0
}}
"
        ),
    );
    assert_eq!(
        out,
        "at 0: expected a value\n\
         at 3: expected a value\n\
         at 3: expected a comma or a closing bracket\n\
         at 5: expected a colon\n\
         at 5: expected a value\n\
         at 0: expected null\n\
         at 2: expected a digit after the decimal point\n\
         at 2: expected a digit in the exponent\n\
         at 13: expected a closing quote\n\
         at 8: expected the end of the document\n\
         0\n"
    );
}

/// The encoder and the parser agree, which is the property that matters more
/// than either of them alone.
#[test]
fn a_document_survives_a_round_trip() {
    let out = run(
        "json_round_trip",
        &format!(
            "{HEAD}
/// Encoded, parsed, encoded again. If the two agree the second time, nothing
/// was lost in between.
fn twice(text: String) -> String {{
  match parse(text) {{
    Result::Err(why) => \"failed\",
    Result::Ok(once) => shown(encode(once)),
  }}
}}

fn main() -> Int {{
  print(twice(\"[1,\\\"two\\\",true,null,[],{{}}]\"));
  print(twice(\"{{\\\"only\\\":[{{\\\"deep\\\":-2.5}}]}}\"));
  print(twice(\"\\\"tab\\\\there\\\"\"));
  khora_print_int(khora_live_count());
  0
}}
"
        ),
    );
    assert_eq!(
        out,
        "[1,\"two\",true,null,[],{}]\n{\"only\":[{\"deep\":-2.5}]}\n\"tab\\there\"\n0\n"
    );
}

/// A derived record is an ordinary JSON object, including nested generic
/// containers, and the value that comes back is the one that went in.
#[test]
fn a_record_derives_json_in_both_directions() {
    let out = run(
        "json_derive_record",
        &format!(
            "{HEAD}
derive(ToJson, FromJson)
type Account = {{ name: String, active: Bool, scores: List<Int>, note: Option<String> }};

/// The round trip in its own scope, so the count below is about what the
/// derived code *left behind* rather than what is still in scope. Counting
/// beside live locals reports them as leaks, which is a test bug and not a
/// finding.
fn round_trip() -> () raises DecodeError {{
  let original: Account = {{
    name: \"ada\", active: true,
    scores: List::Cons(7, List::Cons(9, List::Nil)),
    note: Option::Some(\"kept\"),
  }};
  let document = original.to_json();
  let restored: Account = decode(document)!;
  print(restored.name);
  print(if restored.active {{ \"active\" }} else {{ \"inactive\" }});
  khora_print_int(List::length(restored.scores));
  print(restored.note.unwrap_or(\"missing\"))
}}

fn main() -> Int raises DecodeError {{
  round_trip()!;
  khora_print_int(khora_live_count());
  0
}}
"
        ),
    );
    assert_eq!(out, "ada\nactive\n2\nkept\n0\n", "the trailing 0 is the live-object count");
}

/// Cases are adjacent-tagged uniformly, then recovered by tag and positional
/// payload. A nullary case uses the same shape as one carrying values.
#[test]
fn a_variant_derives_json_in_both_directions() {
    let out = run(
        "json_derive_variant",
        &format!(
            "{HEAD}
derive(ToJson, FromJson)
type Shape = | Dot | Circle(radius: Int) | Label(String, Bool);

fn describe(shape: Shape) -> String {{
  match shape {{
    Shape::Dot => \"dot\",
    Shape::Circle(radius) => \"circle \" + Int::to_string(radius),
    Shape::Label(text, flag) => text + if flag {{ \" yes\" }} else {{ \" no\" }},
  }}
}}

/// Scoped for the reason the record's round trip is: the count belongs after
/// the values, not beside them.
fn round_trip() -> () raises DecodeError {{
  let a: Shape = decode(Shape::Dot.to_json())!;
  let b: Shape = decode(Shape::Circle(3).to_json())!;
  let c: Shape = decode(Shape::Label(\"tag\", true).to_json())!;
  print(describe(a));
  print(describe(b));
  print(describe(c))
}}

fn main() -> Int raises DecodeError {{
  round_trip()!;
  khora_print_int(khora_live_count());
  0
}}
"
        ),
    );
    assert_eq!(out, "dot\ncircle 3\ntag yes\n0\n", "the trailing 0 is the live-object count");
}

/// A decoder error retains the complete route to the value that disagreed.
/// That is the difference between a useful API error and "invalid JSON".
#[test]
fn a_derived_decode_names_the_nested_field() {
    let out = run(
        "json_derive_path",
        &format!(
            "{HEAD}
derive(FromJson)
type Inner = {{ amount: Int }};
derive(FromJson)
type Outer = {{ entry: Inner }};

fn path_text(path: List<String>) -> String {{
  match path {{
    List::Nil => \"\",
    List::Cons(head, List::Nil) => head,
    List::Cons(head, tail) => head + \".\" + path_text(tail),
  }}
}}

fn main() -> Int {{
  match parse(\"{{\\\"entry\\\":{{\\\"amount\\\":\\\"many\\\"}}}}\") {{
    Result::Err(_) => print(\"parse failed\"),
    Result::Ok(document) => {{
      let _: Outer = decode(document)! catch {{
        DecodeError::At(path, expected, found) => {{
          print(path_text(path));
          print(expected);
          print(found);
          {{ entry: {{ amount: 0 }} }}
        }},
      }};
    }},
  }};
  khora_print_int(khora_live_count());
  0
}}
"
        ),
    );
    assert_eq!(
        out,
        "entry.amount\nwhole number in the range of Int\nstring\n0\n"
    );
}
