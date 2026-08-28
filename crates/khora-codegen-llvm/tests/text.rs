#![cfg(feature = "llvm")]

//! Text handling, written in Khora.
//!
//! Slicing, searching, splitting and writing a number out — all of it in
//! `std::core`, over `String::byte` and `Array<U8>`, with no intrinsic behind
//! any of it. That is the point: if slicing a string needed the compiler's
//! help, so would everything above it.

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
    let exe = dir.join(if cfg!(windows) { "program.exe" } else { "program" });
    let _ = std::fs::remove_file(&exe);

    let db = KhoraDatabase::new();
    let files = vec![
        SourceFile::new(&db, dir.join("core.kh"), std_source("core.kh")),
        SourceFile::new(&db, dir.join("main.kh"), main.to_string()),
    ];
    let root = SourceRoot::new(&db, files);
    if let Err(errors) = khora_codegen_llvm::compile(&db, root, &exe) {
        let messages: Vec<String> = errors.into_iter().map(|e| e.message).collect();
        panic!("compiling `{name}` failed:\n  {}", messages.join("\n  "));
    }

    let out = std::process::Command::new(&exe).output().expect("the program should run");
    assert_eq!(out.status.code(), Some(0), "`{name}` did not exit cleanly");
    String::from_utf8_lossy(&out.stdout).replace("\r\n", "\n")
}

const HEAD: &str = "module demo::main;
import std::core::{Array, Eq, List, Option, Ord, Ordering, Split};

fn print(value: String);
extern fn khora_print_int(value: Int);
extern fn khora_live_count() -> Int;
";

#[test]
fn a_number_can_be_written_out() {
    let out = run(
        "text_to_string",
        &format!(
            "{HEAD}
fn main() -> Int {{
  print(Int::to_string(0));
  print(Int::to_string(7));
  print(Int::to_string(1536));
  print(Int::to_string(0 - 42));
  print(Int::to_string(9223372036854775807));
  khora_print_int(khora_live_count());
  0
}}
"
        ),
    );
    assert_eq!(out, "0\n7\n1536\n-42\n9223372036854775807\n0\n");
}

#[test]
fn a_string_can_be_sliced() {
    let out = run(
        "text_slice",
        &format!(
            "{HEAD}
fn main() -> Int {{
  print(String::slice(\"hello, khora\", 0, 5));
  print(String::slice(\"hello, khora\", 7, 12));
  print(String::slice(\"hello\", 2, 2));
  print(String::slice(\"hello\", 3, 999));
  khora_print_int(khora_live_count());
  0
}}
"
        ),
    );
    assert_eq!(out, "hello\nkhora\n\nlo\n0\n", "the third is empty, the fourth clamped");
}

#[test]
fn a_string_can_be_searched() {
    let out = run(
        "text_search",
        &format!(
            "{HEAD}
fn main() -> Int {{
  khora_print_int(String::index_of(\"GET /analyze HTTP/1.1\", \" \").unwrap_or(0 - 1));
  khora_print_int(String::index_of(\"hello\", \"llo\").unwrap_or(0 - 1));
  khora_print_int(String::index_of(\"hello\", \"nope\").unwrap_or(0 - 1));
  khora_print_int(if String::starts_with(\"GET /x\", \"GET \") {{ 1 }} else {{ 0 }});
  khora_print_int(if String::starts_with(\"GET /x\", \"POST\") {{ 1 }} else {{ 0 }});
  khora_print_int(khora_live_count());
  0
}}
"
        ),
    );
    assert_eq!(out, "3\n2\n-1\n1\n0\n0\n");
}

/// The shape an HTTP request line wants: head, rest, repeat.
#[test]
fn a_string_can_be_split_once() {
    let out = run(
        "text_split",
        &format!(
            "{HEAD}
fn main() -> Int {{
  match String::split_once(\"GET /analyze/acc_1 HTTP/1.1\", \" \") {{
    Option::None => print(\"no\"),
    Option::Some(parts) => {{
      print(parts.head);
      print(parts.rest);
    }}
  }};
  match String::split_once(\"nospaces\", \" \") {{
    Option::None => print(\"absent\"),
    Option::Some(parts) => print(parts.head),
  }};
  khora_print_int(khora_live_count());
  0
}}
"
        ),
    );
    assert_eq!(out, "GET\n/analyze/acc_1 HTTP/1.1\nabsent\n0\n");
}

// --- numbers as text --------------------------------------------------------

/// The shortest form that reads back as the same number, which is what a
/// reader means by "the number" — and, until this existed, something a program
/// could `print` but could not put in a message.
#[test]
fn a_float_can_be_written_out() {
    let out = run(
        "text_float",
        &format!(
            "{HEAD}
fn main() -> Int {{
  print(Float::to_string(1.5));
  print(Float::to_string(0.1 + 0.2));
  print(Float::to_string(0.0 - 42.0));
  print(\"total: \" + Float::to_string(98.6));
  khora_print_int(khora_live_count());
  0
}}
"
        ),
    );
    assert_eq!(
        out,
        "1.5\n0.30000000000000004\n-42\ntotal: 98.6\n0\n",
        "the second is the whole point: it round-trips rather than rounding"
    );
}

// --- sorting ----------------------------------------------------------------

const SHOW: &str = "
fn show_ints(items: List<Int>) -> () {
  match items {
    List::Nil => (),
    List::Cons(head, rest) => { khora_print_int(head); show_ints(rest) },
  }
}

fn show_text(items: List<String>) -> () {
  match items {
    List::Nil => (),
    List::Cons(head, rest) => { print(head); show_text(rest) },
  }
}
";

/// A merge sort over a linked list, in Khora, over whatever `Ord` says.
#[test]
fn a_list_can_be_sorted() {
    let out = run(
        "text_sort",
        &format!(
            "{HEAD}{SHOW}
fn main() -> Int {{
  let numbers =
    List::Cons(5, List::Cons(1, List::Cons(4, List::Cons(1, List::Cons(9, List::Nil)))));
  show_ints(numbers.sort());
  print(\"--\");
  show_text(List::Cons(\"pear\", List::Cons(\"apple\", List::Cons(\"fig\", List::Nil))).sort());
  0
}}
"
        ),
    );
    assert_eq!(out, "1\n1\n4\n5\n9\n--\napple\nfig\npear\n");
}

/// Nothing and one thing are already sorted, and are where a split that cannot
/// make progress would loop forever.
#[test]
fn sorting_a_short_list_terminates() {
    let out = run(
        "text_sort_short",
        &format!(
            "{HEAD}{SHOW}
fn main() -> Int {{
  show_ints(List::Nil.sort());
  print(\"--\");
  show_ints(List::Cons(7, List::Nil).sort());
  print(\"--\");
  show_ints(List::Cons(2, List::Cons(1, List::Nil)).sort());
  0
}}
"
        ),
    );
    assert_eq!(out, "--\n7\n--\n1\n2\n");
}

/// **Stable.** Equal elements keep the order they were given, which is the
/// property a caller notices the absence of when sorting by one field and
/// expecting the previous sort to survive. `merge` breaking its tie towards
/// the left half is what makes it true.
#[test]
fn sorting_is_stable() {
    let out = run(
        "text_sort_stable",
        &format!(
            "{HEAD}
pub type Entry = {{ key: Int, tag: String }};

impl Eq for Entry {{
  fn eq(self, other: Entry) -> Bool {{ self.key == other.key }}
}}

impl Ord for Entry {{
  fn cmp(self, other: Entry) -> Ordering {{ self.key.cmp(other.key) }}
}}

fn show_tags(items: List<Entry>) -> () {{
  match items {{
    List::Nil => (),
    List::Cons(head, rest) => {{ print(head.tag); show_tags(rest) }},
  }}
}}

fn main() -> Int {{
  let entries =
    List::Cons({{ key: 2, tag: \"a\" }},
    List::Cons({{ key: 1, tag: \"b\" }},
    List::Cons({{ key: 2, tag: \"c\" }},
    List::Cons({{ key: 1, tag: \"d\" }}, List::Nil))));
  show_tags(entries.sort());
  0
}}
"
        ),
    );
    assert_eq!(out, "b\nd\na\nc\n", "the two 1s keep b-before-d, and the two 2s a-before-c");
}

// --- the everyday half, which was missing ----------------------------------

/// A `main` around a body, so a test reads as the Khora it is checking.
///
/// `format!` inserts `body` at run time, so the braces inside it need no
/// doubling -- which is the difference between a readable test and one that
/// is mostly backslashes.
fn program(body: &str) -> String {
    format!("{HEAD}fn main() -> Int {{
{body}
  0
}}
")
}

/// **`ends_with` beside `starts_with`, `upper` beside `lower`.**
///
/// The asymmetries were the diagnosis rather than a judgement call: a surface
/// with one of each pair is what it looks like when every function was added
/// by the one caller that needed it.
#[test]
fn the_pairs_that_were_missing_a_half() {
    let out = run(
        "text_pairs",
        &program(r#"  print(if String::ends_with("khora.toml", ".toml") { "yes" } else { "no" });
  print(if String::ends_with("khora.toml", ".kh") { "yes" } else { "no" });
  // Shorter than the suffix, which is the index that would go negative.
  print(if String::ends_with("a", "aaa") { "yes" } else { "no" });
  print(String::upper("Khora 1"));
  print(if String::contains("khora", "hor") { "yes" } else { "no" });
  print(if String::contains("khora", "zz") { "yes" } else { "no" });
  print(if String::is_empty("") { "yes" } else { "no" });
  print("[" + String::trim_start("  x  ") + "]");
  print("[" + String::trim_end("  x  ") + "]");
  print(String::repeat("ab", 3));
  print("[" + String::repeat("ab", 0) + "]");"#),
    );

    assert_eq!(
        out,
        "yes\nno\nno\nKHORA 1\nyes\nno\nyes\n[x  ]\n[  x]\nababab\n[]\n"
    );
}

/// **`n` separators give `n + 1` pieces, always.**
///
/// The rule that makes `split` and `join` inverses, and the one every library
/// that quietly drops empty pieces gives up. `"a,,b"` has three fields and the
/// middle one is empty; a caller who wants them dropped writes a filter, which
/// they could not do if the split had already decided.
#[test]
fn splitting_keeps_every_piece() {
    let out = run(
        "text_split_all",
        &program(r#"  print(String::join(String::split("a,b,c", ","), "|"));
  print(String::join(String::split("a,,b", ","), "|"));
  print(String::join(String::split(",a", ","), "|"));
  print(String::join(String::split("a,", ","), "|"));
  print(String::join(String::split("", ","), "|"));
  print(String::join(String::split("nothing", ","), "|"));
  // A multi-byte separator, so the step is the separator's length and not one.
  print(String::join(String::split("a<>b<>c", "<>"), "|"));
  // An empty separator matches everywhere and so splits nothing.
  print(String::join(String::split("abc", ""), "|"));"#),
    );

    assert_eq!(
        out,
        "a|b|c\na||b\n|a\na|\n\nnothing\na|b|c\nabc\n"
    );
}

/// **`join(split(s, x), x)` is `s`**, which is the property both were written
/// to satisfy and the reason `split` keeps its empty pieces.
#[test]
fn splitting_and_joining_are_inverses() {
    let out = run(
        "text_roundtrip",
        &program(r#"  print(String::join(String::split("a,b,,c,", ","), ","));
  print(String::join(String::split("", ","), ","));
  print(String::join(String::split(",,,", ","), ","));"#),
    );

    assert_eq!(out, "a,b,,c,\n\n,,,\n");
}

/// **Replacement does not re-read what it wrote.**
///
/// `"a"` to `"aa"` doubles each `a` once. A loop that searched the output
/// instead would not terminate, which is the bug this shape avoids by
/// construction rather than by a guard.
#[test]
fn replacing_reads_the_original_only() {
    let out = run(
        "text_replace",
        &program(r#"  print(String::replace("banana", "a", "aa"));
  print(String::replace("banana", "na", "-"));
  print(String::replace("aaa", "aa", "b"));
  print(String::replace("khora", "zz", "!"));
  print(String::replace("khora", "", "!"));
  print(String::replace("a,b", ",", ""));"#),
    );

    // Two things the counting has to get right, and the first draft of this
    // test got both wrong. `"banana"` has three `a`s and so *four* pieces --
    // the last one empty -- which is why doubling them gives `baanaanaa` and
    // not `baanaana`. And `"na"` occurs twice with nothing after the second,
    // so `ba--` and not `ba-`.
    //
    // `aaa` with `aa` -> `b` is `ba`: the trailing `a` is what the first match
    // left, and nothing rescans.
    assert_eq!(out, "baanaanaa\nba--\nba\nkhora\nkhora\nab\n");
}

/// Splitting a long line is linear, not quadratic.
///
/// `split` builds its list backwards and reverses once, because prepending is
/// the cheap end and appending walks the list per piece — which turns a
/// thousand fields into a million steps. A timing assertion would be flaky;
/// the count is what says the work happened.
#[test]
fn splitting_a_long_line_stays_linear() {
    let out = run(
        "text_split_long",
        &program(r#"  let mut line = "";
  let mut i = 0;
  while i < 500 {
    line = line + "f,";
    i = i + 1
  };
  let pieces = String::split(line, ",");
  print(Int::to_string(List::length(pieces)));
  print(String::join(pieces, ""));"#),
    );

    // 500 separators, 501 pieces, the last one empty.
    let mut expected = String::from("501\n");
    expected.push_str(&"f".repeat(500));
    expected.push('\n');
    assert_eq!(out, expected);
}
