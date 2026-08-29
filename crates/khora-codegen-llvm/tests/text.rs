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
import std::core::{Array, Dict, Eq, Halves, List, Map, Option, Ord, Ordering, Pair, Parts, Result, Shared, Show, Split};

/// The two helpers every list test below wants: an empty `List<Int>` that
/// inference can name, and an `Option<Int>` written out.
fn empty() -> List<Int> { List::Nil }

fn shown(value: Option<Int>) -> String {
  match value { Option::Some(n) => Int::to_string(n), Option::None => \"none\" }
}

/// An `Option<Int>` and two `Result<Int, String>`s that inference can name,
/// for the same reason `empty` is here.
fn empty_option() -> Option<Int> { Option::None }
fn good() -> Result<Int, String> { Result::Ok(2) }
fn bad() -> Result<Int, String> { Result::Err(\"no\") }
fn no_rows() -> List<Pair<Int, String>> { List::Nil }

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

// --- the list toolkit, which was three functions deep ----------------------

/// **`any` and `all` stop early**, which is what makes them worth having over
/// the `fold` everybody was writing instead.
///
/// The counter says so: a fold visits five, and these visit as many as they
/// need to decide.
#[test]
fn any_and_all_stop_as_soon_as_they_know() {
    let out = run(
        "list_any_all",
        &program(r#"  let seen = Shared::of(0);
  let count = fn (n: Int) => { Shared::update(seen, fn c => c + 1); n > 2 };

  print(([1, 2, 3, 4, 5].any(count)).show());
  print(Int::to_string(Shared::get(seen)));

  Shared::set(seen, 0);
  print(([1, 2, 3, 4, 5].all(count)).show());
  print(Int::to_string(Shared::get(seen)));

  // `all` of nothing is true, which is what makes it compose: asking each half
  // of a split list gives the same answer as asking the whole.
  print((empty().all(fn n => n > 100)).show());
  print((empty().any(fn n => n > 100)).show());"#),
    );

    // `any` looks at three before 3 > 2; `all` stops at the first, 1.
    assert_eq!(out, "true\n3\nfalse\n1\ntrue\nfalse\n");
}

/// `find`, `nth`, `last` and `contains` — the four ways to get one element out.
#[test]
fn the_four_ways_to_reach_one_element() {
    let out = run(
        "list_reach",
        &program(r#"  let xs = [10, 20, 30];
  print(shown(xs.find(fn n => n > 15)));
  print(shown(xs.find(fn n => n > 99)));
  print(shown(xs.nth(0)));
  print(shown(xs.nth(2)));
  print(shown(xs.nth(3)));
  print(shown(xs.nth(0 - 1)));
  print(shown(xs.last()));
  print(shown(empty().last()));
  print((xs.contains(20)).show());
  print((xs.contains(21)).show());"#),
    );

    assert_eq!(
        out,
        "20\nnone\n10\n30\nnone\nnone\n30\nnone\ntrue\nfalse\n"
    );
}

/// `take` and `drop` are total: a count past the end is the whole list or
/// nothing, and a negative one is not an error.
///
/// Both matter because the count usually came from somewhere else — a page
/// size, a header, an argument — and a version that refused would make every
/// caller check first.
#[test]
fn taking_and_dropping_never_refuse() {
    let out = run(
        "list_take_drop",
        &program(r#"  let xs = [1, 2, 3];
  print(xs.take(2).show());
  print(xs.take(0).show());
  print(xs.take(0 - 5).show());
  print(xs.take(99).show());
  print(xs.drop(2).show());
  print(xs.drop(0).show());
  print(xs.drop(99).show());
  // Together they partition, for any count.
  print(xs.take(2).concat(xs.drop(2)).show());"#),
    );

    assert_eq!(
        out,
        "[1, 2]\n[]\n[]\n[1, 2, 3]\n[3]\n[1, 2, 3]\n[]\n[1, 2, 3]\n"
    );
}

/// `zip` stops at the shorter, because there is nothing to pad a `List<A>`
/// with — `A` is any type and this library has no default.
#[test]
fn zipping_stops_at_the_shorter() {
    let out = run(
        "list_zip",
        &program(r#"  print(Int::to_string(List::length([1, 2, 3].zip(["a", "b"]))));
  print(Int::to_string(List::length([1].zip(["a", "b", "c"]))));
  print(Int::to_string(List::length(empty().zip(["a"]))));
  let pairs = [1, 2].zip(["x", "y"]);
  print(String::join(List::map(pairs, fn p => p.value + Int::to_string(p.key)), ","));"#),
    );

    assert_eq!(out, "2\n1\n0\nx1,y2\n");
}

/// `flat_map` and `sum`, and the empty cases both have to get right.
#[test]
fn flat_map_and_sum() {
    let out = run(
        "list_flat_sum",
        &program(r#"  print([1, 2, 3].flat_map(fn n => [n, n]).show());
  print([1, 2, 3].flat_map(fn _n => empty()).show());
  print(empty().flat_map(fn n => [n]).show());
  print(Int::to_string([1, 2, 3].sum()));
  print(Int::to_string(empty().sum()));"#),
    );

    assert_eq!(out, "[1, 1, 2, 2, 3, 3]\n[]\n[]\n6\n0\n");
}

/// **`Dict` answers `keys` and `values`**, which `Map` always did.
///
/// Both in key order, because `entries` is and these are it read one way.
#[test]
fn a_dict_gives_up_its_keys_and_values() {
    let out = run(
        "dict_keys_values",
        &program(r#"  let d = Dict::new()
    |> Dict::insert(2, "two")
    |> Dict::insert(1, "one")
    |> Dict::insert(3, "three");
  print(d.keys().show());
  print(String::join(d.values(), ","));
  print(Int::to_string(List::length(Dict::entries(d))));"#),
    );

    assert_eq!(out, "[1, 2, 3]\none,two,three\n3\n");
}

/// **The most negative `Int` prints.**
///
/// It did not. `to_string` took the magnitude with `0 - self`, which is the
/// one subtraction that number does not survive, so printing it stopped the
/// program and reported an overflow the caller never wrote. Found while
/// testing `Decimal::show`, which could build that significand, compare it and
/// add to it, and not show it.
///
/// There is no literal for it either -- `9223372036854775808` does not fit,
/// and the minus arrives after the number is read -- so the test walks to it,
/// which is what `std::core` does too.
#[test]
fn the_most_negative_int_prints() {
    let out = run(
        "int_show_smallest",
        &program(
            r#"  let smallest = 0 - 9223372036854775807 - 1;
  print(Int::to_string(smallest));
  print(Int::to_string(smallest + 1));
  print(Int::to_string(9223372036854775807));
  // And the ordinary ones are unchanged.
  print(Int::to_string(0));
  print(Int::to_string(-7));
  print(Int::to_string(10));
  print(Int::to_string(-10));
  print(Int::to_string(-100));"#,
        ),
    );

    assert_eq!(
        out,
        "-9223372036854775808\n-9223372036854775807\n9223372036854775807\n0\n-7\n10\n-10\n-100\n"
    );
}

/// **A numeral one past the largest `Int` is refused, not run into.**
///
/// `of_string`'s own doc comment says it refuses rather than overflows,
/// because a long run of digits off a socket that stops the program is a
/// denial of service with extra steps. The guard was a digit short: it let a
/// total equal to a tenth of the maximum through, so `9223372036854775808`
/// grew past the end and did the thing the comment promises it does not.
///
/// And the total is counted downward now, so the most negative number is one
/// this can reach -- it used to build the positive twin on the way, and that
/// number does not exist.
#[test]
fn a_numeral_past_the_end_is_refused_rather_than_overflowing() {
    let out = run(
        "int_of_string_edges",
        &program(
            r#"  // The two ends, which must read.
  print(shown(Int::of_string("9223372036854775807")));
  print(shown(Int::of_string("-9223372036854775808")));
  // One past each, which must not -- and must not stop the program.
  print(shown(Int::of_string("9223372036854775808")));
  print(shown(Int::of_string("-9223372036854775809")));
  // Far past, the shape somebody pastes in by accident.
  print(shown(Int::of_string("1234567890123456789012345")));
  print(shown(Int::of_string("-1234567890123456789012345")));
  // Ordinary numbers are unaffected.
  print(shown(Int::of_string("0")));
  print(shown(Int::of_string("-1")));
  print(shown(Int::of_string("1250")));
  print(shown(Int::of_string("")));
  print(shown(Int::of_string("-")));
  print(shown(Int::of_string("12a")));"#,
        ),
    );

    assert_eq!(
        out,
        "9223372036854775807\n-9223372036854775808\nnone\nnone\nnone\nnone\n0\n-1\n1250\nnone\nnone\nnone\n"
    );
}

/// Text in and the same text out, at both ends of the range.
///
/// The round trip is the property the two fixes above are really about: a
/// number `to_string` can write is one `of_string` can read.
#[test]
fn a_number_survives_the_round_trip_at_both_ends() {
    let out = run(
        "int_text_round_trip",
        &program(
            r#"  let smallest = 0 - 9223372036854775807 - 1;
  print(shown(Int::of_string(Int::to_string(smallest))));
  print(shown(Int::of_string(Int::to_string(9223372036854775807))));
  print(shown(Int::of_string(Int::to_string(0))));
  print(shown(Int::of_string(Int::to_string(-1))));"#,
        ),
    );

    assert_eq!(
        out,
        "-9223372036854775808\n9223372036854775807\n0\n-1\n"
    );
}

/// **A list of a hundred thousand elements can be walked.**
///
/// It could not. Every traversal in `List` recursed once per element, so a log
/// analyser that read a hundred and twenty-two thousand lines died while
/// *reporting* on them -- exit 253, nothing on stdout, nothing on stderr, and
/// stack exhaustion in neither `docs/reference/traps.md` nor the output.
/// Thresholds measured at the time: about eight thousand in debug, twelve in
/// release, with `List::sort` going first because it stacked the deepest.
///
/// The algorithms were never the problem. `sort` was already a merge sort;
/// what killed it was that its `merge` built `Cons(x, merge(..))`, which is
/// not a tail call, so merging the two halves at the top took a frame per
/// element. All of these are loops now.
///
/// A hundred thousand rather than a million because this runs in the ordinary
/// test suite and the point is the shape, not the ceiling.
#[test]
fn a_large_list_can_be_walked_without_running_out_of_stack() {
    let out = run(
        "list_large_walk",
        &program(
            r#"  let n = 100000;
  let mut xs = empty();
  let mut i = 0;
  while i < n { xs = List::Cons(i, xs); i = i + 1; };
  print(Int::to_string(xs.length()));
  print(Int::to_string(xs.sum()));
  print(Int::to_string(xs.fold(0, fn (a, b) => a + b)));
  print(Int::to_string(xs.filter(fn v => v % 2 == 0).length()));
  print(Int::to_string(xs.reverse().length()));
  print(Int::to_string(xs.take(60000).length()));
  print(Int::to_string(xs.drop(60000).length()));
  print(Int::to_string(xs.zip(xs).length()));
  print(Int::to_string(xs.flat_map(fn v => [v]).length()));
  print(if xs.any(fn v => v == 99999) { "yes" } else { "no" });
  print(if xs.all(fn v => v >= 0) { "yes" } else { "no" });
  print(if xs.contains(50000) { "yes" } else { "no" });
  print(shown(xs.find(fn v => v == 7)));
  print(shown(xs.nth(1)));
  print(shown(xs.last()));"#,
        ),
    );

    // 0 + 1 + .. + 99999
    let total = (99_999i64 * 100_000 / 2).to_string();
    assert_eq!(
        out,
        format!(
            "100000\n{total}\n{total}\n50000\n100000\n60000\n40000\n100000\n100000\n\
             yes\nyes\nyes\n7\n99998\n0\n"
        )
    );
}

/// **And sorted**, which is the one that a real workload hit first.
///
/// Twelve thousand rather than a hundred: `sort` releases its intermediate
/// lists rather than walking them, and freeing a long list is still
/// depth-proportional -- reference counting frees a value's children through a
/// callback that recurses. That is the remaining limit and it is written down
/// in `docs/limitations`.
///
/// Twelve thousand is chosen against the *debug* build, which this suite is,
/// and where the old ceiling for `sort` was about eight. In release the same
/// code sorts twenty-five thousand where it used to manage twelve.
#[test]
fn a_large_list_can_be_sorted() {
    let out = run(
        "list_large_sort",
        &program(
            r#"  let n = 12000;
  let mut xs = empty();
  let mut i = 0;
  // Multiplied by a coprime so the input is shuffled rather than already in
  // order, which is the case a merge sort could otherwise walk straight past.
  while i < n { xs = List::Cons((i * 7919) % n, xs); i = i + 1; };
  let sorted = xs.sort();
  print(Int::to_string(sorted.length()));
  print(shown(sorted.head()));
  print(shown(sorted.last()));
  print(shown(sorted.nth(6000)));
  // Each element against the next, with `zip` and `drop` -- both loops, so
  // the check does not reintroduce the recursion the test is about.
  let pairs = sorted.zip(sorted.drop(1));
  print(if pairs.all(fn p => p.key <= p.value) { "ordered" } else { "not ordered" });"#,
        ),
    );

    assert_eq!(out, "12000\n0\n11999\n6000\nordered\n");
}

/// **Releasing a long list costs no stack.**
///
/// Walking one stopped being deep when every `List` traversal became a loop.
/// Freeing one did not: `drop_fields` releases an object's children by calling
/// back into the runtime, so letting go of a hundred thousand cons cells was a
/// hundred thousand nested frames. It was the last thing in the language whose
/// cost was proportional to the *depth* of a value, and it is why `List::sort`
/// still gave out in the tens of thousands after everything else stopped --
/// a sort releases its intermediate lists rather than walking them.
///
/// The distinguishing case is a list nothing walks. A traversal frees as it
/// goes, one node at a time, and hid this for every operation except the ones
/// that simply drop.
#[test]
fn releasing_a_large_list_costs_no_stack() {
    let out = run(
        "list_large_release",
        &program(
            r#"  let n = 100000;
  let mut xs = empty();
  let mut i = 0;
  while i < n { xs = List::Cons(i, xs); i = i + 1; };
  // Split it and let both halves go without walking either. Before this, the
  // release at the end of the block ran out of stack.
  let halves = xs.split();
  print(Int::to_string(halves.left.length()));
  print("released");"#,
        ),
    );

    assert_eq!(out, "50000\nreleased\n");
}

/// And the sort that was capped by it is not capped by anything now.
///
/// Twelve thousand was as far as the debug profile went while freeing was
/// recursive. This is ten times that and finishes; the ceiling that remains is
/// memory.
#[test]
fn a_much_larger_list_can_be_sorted() {
    let out = run(
        "list_huge_sort",
        &program(
            r#"  let n = 120000;
  let mut xs = empty();
  let mut i = 0;
  while i < n { xs = List::Cons((i * 7919) % n, xs); i = i + 1; };
  let sorted = xs.sort();
  print(Int::to_string(sorted.length()));
  print(shown(sorted.head()));
  print(shown(sorted.last()));
  let pairs = sorted.zip(sorted.drop(1));
  print(if pairs.all(fn p => p.key <= p.value) { "ordered" } else { "not ordered" });"#,
        ),
    );

    assert_eq!(out, "120000\n0\n119999\nordered\n");
}

/// **A `${..}` hole shows whatever it holds**, against the real `std::core`.
///
/// The tiny prelude in `tests/interpolation.rs` pins the rule; this pins that
/// the rule reaches every `Show` the standard library actually has, including
/// a derived one and a `List`. It is the table the log analyser was trying to
/// print:
///
///     print("    ${pad_right(row.name, 26)}${Int::to_string(row.count)}...");
///
/// which is why "the single biggest tax" was the phrase used for it.
///
/// **No `Show` in the import list.** That is deliberate and is half the point:
/// the hole is the use, the trait is never named in the source, and requiring
/// it would put a lint on the import and a build failure on removing it.
#[test]
fn a_hole_shows_whatever_it_holds() {
    let out = run(
        "interp_shows_std",
        &program(
            r#"  let n = 42;
  let name = "khora";
  let flag = true;
  print("n = ${n}");
  print("name = ${name}");
  print("flag = ${flag}");
  print("list = ${[1, 2, 3]}");
  // Several holes and some arithmetic in one, which is the shape a table row
  // takes and the reason the explicit conversions were unreadable.
  print("${n} and ${name} and ${n * 2}");"#,
        ),
    );

    assert_eq!(
        out,
        "n = 42\nname = khora\nflag = true\nlist = [1, 2, 3]\n42 and khora and 84\n"
    );
}


/// **`pad_left` and `pad_right`, which four separate people wrote first.**
///
/// Every one of the three programs written against this library grew its own,
/// and `std::time` had a third copy under the name `padded`. A function four
/// people write is one the library should have.
///
/// Bytes rather than characters, because `byte_length` is what everything else
/// here counts in — and lining text up by *display* width is a font question a
/// `String` cannot answer. Already at or past the width is returned unchanged:
/// a pad that truncates is a different function, and one that silently drops a
/// digit from a total is not one this library offers by accident.
#[test]
fn text_can_be_padded_into_a_column() {
    let out = run(
        "text_pad",
        &program(
            r#"  print("[" + String::pad_left("7", 3, "0") + "]");
  print("[" + String::pad_right("ab", 5, ".") + "]");
  print("[" + String::pad_left("", 3, " ") + "]");
  // Already wide enough, and wider than asked for: both unchanged.
  print("[" + String::pad_left("abc", 3, "0") + "]");
  print("[" + String::pad_right("toolong", 3, "0") + "]");
  // A fill that is not one byte could not land on the width exactly.
  print("[" + String::pad_left("7", 4, "ab") + "]");
  print("[" + String::pad_left("7", 4, "") + "]");"#,
        ),
    );

    assert_eq!(out, "[007]\n[ab...]\n[   ]\n[abc]\n[toolong]\n[7]\n[7]\n");
}

/// **`split_whitespace` keeps no empty pieces, and `split` keeps them all.**
///
/// That difference is the whole reason for the second function. `split` is
/// right for a CSV, where two commas in a row mean a field that is there and
/// blank; it is wrong for anything column-aligned, where two spaces in a row
/// mean the writer was lining something up. A log line split the first way has
/// empty strings in it that nobody put there.
#[test]
fn whitespace_splits_into_words_and_not_into_gaps() {
    let out = run(
        "text_words",
        &program(
            r#"  print(String::join(String::split_whitespace("  a  bb  c "), "|"));
  print(String::join(String::split("  a  bb  c ", " "), "|"));
  // A string of nothing but spaces, and one with none at all.
  print("[" + String::join(String::split_whitespace("   "), "|") + "]");
  print("[" + String::join(String::split_whitespace(""), "|") + "]");
  print(String::join(String::split_whitespace("one"), "|"));"#,
        ),
    );

    assert_eq!(out, "a|bb|c\n||a||bb||c|\n[]\n[]\none\n");
}

/// **`and_then` is the one whose absence changes the shape of the code.**
///
/// `map` with a function that returns an `Option` gives an `Option<Option<B>>`.
/// Without the flattening version, reading a record out of positional fields
/// is a `match` inside a `match` inside a `match` — `examples/link_shortener`
/// nested six deep before this existed.
#[test]
fn an_option_chains_without_nesting() {
    let out = run(
        "option_chain",
        &program(
            r#"  let halve = fn (n: Int) => if n % 2 == 0 { Option::Some(n / 2) } else { Option::None };
  print(shown(Option::and_then(Option::Some(8), halve)));
  print(shown(Option::and_then(Option::Some(7), halve)));
  print(shown(Option::and_then(empty_option(), halve)));
  // Three deep, which is the shape that used to be a pyramid.
  print(shown(Option::and_then(Option::and_then(Option::and_then(
    Option::Some(8), halve), halve), halve)));
  print(shown(Option::filter(Option::Some(4), fn n => n > 3)));
  print(shown(Option::filter(Option::Some(2), fn n => n > 3)));
  print(shown(Option::filter(empty_option(), fn n => n > 3)));"#,
        ),
    );

    assert_eq!(out, "4\nnone\nnone\n1\n4\nnone\nnone\n");
}

/// **`map` beside `map_err`**, which is the asymmetry that made its absence
/// odd: a reader who has met one goes looking for the other and finds a
/// `match`.
///
/// Not a `Functor` impl — that trait takes one type parameter and `Result` has
/// two, so it would need partial application of a type constructor for the
/// sake of one method.
#[test]
fn a_result_maps_and_chains() {
    let out = run(
        "result_chain",
        &program(
            r#"  print(shown(Result::ok(Result::map(good(), fn n => n + 1))));
  print(shown(Result::ok(Result::map(bad(), fn n => n + 1))));
  print(shown(Result::ok(Result::and_then(good(), fn n => Result::Ok(n * 5)))));
  print(shown(Result::ok(Result::and_then(good(), fn _n => bad()))));
  print(shown(Result::ok(Result::and_then(bad(), fn n => Result::Ok(n * 5)))));
  // The error survives a `map`, which is the half `map_err` does not do.
  match Result::map(bad(), fn n => n + 1) {
    Result::Ok(_n) => print("ok"),
    Result::Err(e) => print(e),
  };"#,
        ),
    );

    assert_eq!(out, "3\nnone\n10\nnone\nnone\nno\n");
}

/// **`Bool::to_string` under the name the other scalars use.**
///
/// `Int::to_string` and `Float::to_string` are both here, so a reader who has
/// met those goes looking for this one. `Show for Bool` is written in terms of
/// it rather than the other way round, so the two words are spelled once.
///
/// `Show for Ordering` is the same kind of gap: a comparison in a log had
/// nothing to say for itself.
#[test]
fn a_bool_and_an_ordering_can_say_what_they_are() {
    let out = run(
        "show_bool_ordering",
        &program(
            r#"  print(Bool::to_string(true) + " " + Bool::to_string(false));
  print(true.show() + " " + false.show());
  print(Ordering::Less.show() + " " + Ordering::Equal.show() + " " + Ordering::Greater.show());
  print("a bool in a hole: ${false}");"#,
        ),
    );

    assert_eq!(out, "true false\ntrue false\nLess Equal Greater\na bool in a hole: false\n");
}

/// **The smallest of nothing is not a number**, so both answer `Option`.
///
/// Ties go to the first element, which is what makes `min` and
/// `head(sort(xs))` name the same one.
#[test]
fn a_list_has_a_smallest_and_a_largest() {
    let out = run(
        "list_extremes",
        &program(
            r#"  print(shown(List::min([3, 1, 2])));
  print(shown(List::max([3, 1, 2])));
  print(shown(List::min(empty())));
  print(shown(List::max(empty())));
  print(shown(List::min([5])));
  print(shown(List::min([0 - 4, 0 - 9, 2])));"#,
        ),
    );

    assert_eq!(out, "1\n3\nnone\nnone\n5\n-9\n");
}

/// **One walk and one question per element**, which two `filter`s with
/// opposite predicates is not — and which matters twice over when the question
/// is expensive or is not pure.
#[test]
fn a_list_partitions_in_one_walk() {
    let out = run(
        "list_partition",
        &program(
            r#"  let split = List::partition([1, 2, 3, 4, 5], fn n => n % 2 == 1);
  print(String::join(List::map(split.kept, fn n => Int::to_string(n)), ""));
  print(String::join(List::map(split.rest, fn n => Int::to_string(n)), ""));
  // Everything on one side, and nothing at all.
  let all = List::partition([2, 4], fn n => n % 2 == 0);
  print("[" + String::join(List::map(all.rest, fn n => Int::to_string(n)), "") + "]");
  let none = List::partition(empty(), fn n => n > 0);
  print("[" + String::join(List::map(none.kept, fn n => Int::to_string(n)), "") + "]");"#,
        ),
    );

    assert_eq!(out, "135\n24\n[]\n[]\n");
}

/// **`sort_by` is what `sort` deliberately cannot do**, and `sort`'s own doc
/// comment admitted the gap before this closed it.
///
/// `sort` takes `A: Ord` on purpose, so that `<` on the elements and `sort` on
/// the list cannot disagree; sorting a record by one of its fields is a
/// different question and this is where it is asked.
///
/// Stable, like `sort`: the two pairs keyed `2` come back in the order they
/// went in, which is what makes sorting by one key and then another do what
/// everybody expects. Descending is the same comparison `reverse`d rather than
/// a second one written out.
#[test]
fn sorting_by_a_key_is_stable() {
    let out = run(
        "list_sort_by",
        &program(
            r#"  let rows: List<Pair<Int, String>> = [
    { key: 2, value: "b" },
    { key: 2, value: "a" },
    { key: 1, value: "c" },
  ];
  let named = fn (xs: List<Pair<Int, String>>) =>
    String::join(List::map(xs, fn r => r.value), "");
  print(named(List::sort_by(rows, fn (l, r) => l.key.cmp(r.key))));
  print(named(List::sort_by(rows, fn (l, r) => l.key.cmp(r.key).reverse())));
  // Nothing to sort, and one thing, which are the two the split must not
  // recurse forever on.
  print("[" + named(List::sort_by(no_rows(), fn (l, r) => l.key.cmp(r.key))) + "]");
  print(named(List::sort_by([{ key: 9, value: "z" }], fn (l, r) => l.key.cmp(r.key))));"#,
        ),
    );

    assert_eq!(out, "cba\nbac\n[]\nz\n");
}

/// **Lexicographic, with length as the tie-break rather than the first
/// question.**
///
/// What every other language means by comparing two sequences, and what a
/// sorted list of paths or version segments needs. Consistent with the derived
/// `Eq`: two lists compare `Equal` exactly when `eq` says they are equal,
/// which is what lets `sort` on a `List<List<A>>` mean anything.
#[test]
fn lists_compare_element_by_element() {
    let out = run(
        "list_ord",
        &program(
            r#"  print(List::cmp([1, 2], [1, 2, 0]).show());
  print(List::cmp([1, 2, 0], [1, 2]).show());
  print(List::cmp([2], [1, 9, 9]).show());
  print(List::cmp([1, 2], [1, 2]).show());
  print(List::cmp(empty(), empty()).show());
  print(List::cmp(empty(), [0]).show());
  // And a sort that uses it, which is the reason it exists.
  print(String::join(List::map(List::sort([[2], [1, 9], [1]]),
    fn xs => "(" + String::join(List::map(xs, fn n => Int::to_string(n)), ",") + ")"), ""));"#,
        ),
    );

    assert_eq!(out, "Less\nGreater\nGreater\nEqual\nEqual\nLess\n(1)(1,9)(2)\n");
}


/// **Counting things into a map is the most common thing a log analyser
/// does**, and it used to be
/// `insert(t, k, unwrap_or(get(t, k), 0) + 1)` — which names the map three
/// times, the key twice, and walks the tree twice to answer one question.
///
/// `step` is handed `None` when the key is new, so the initial value and the
/// update are one expression, and it is called exactly once.
#[test]
fn a_dict_updates_in_one_walk() {
    let out = run(
        "dict_update",
        &program(
            r#"  let words = ["a", "b", "a", "c", "a"];
  let counts = List::fold(words, Dict::new(),
    fn (t, w) => Dict::update(t, w, fn seen => seen.unwrap_or(0) + 1));
  print(counts.show());
  // A key that is new: `step` sees `None` and says what to start from.
  print(Dict::update(Dict::new(), "x", fn s => s.unwrap_or(10) + 1).show());
  // And one that is not, which replaces rather than growing the map.
  print(Int::to_string(Dict::size(Dict::update(counts, "a", fn _s => 99))));
  print(shown(Dict::get(Dict::update(counts, "a", fn _s => 99), "a")));"#,
        ),
    );

    assert_eq!(out, "{a: 3, b: 1, c: 1}\n{x: 11}\n3\n99\n");
}

/// **A record holding a `Dict` or a `Map` could not derive `Show`**, which is
/// what made this a hole rather than a nicety.
///
/// A `Dict` prints in key order, because it is a search tree and `entries`
/// walks it in order — so two dictionaries with the same entries print the
/// same way and a golden test over one is worth writing. A `Map` prints in
/// bucket order, which is arbitrary and says so in its own doc comment; this
/// test only has one entry in it for that reason.
#[test]
fn a_map_and_a_dict_can_print_themselves() {
    let out = run(
        "show_maps",
        &format!(
            "{HEAD}
derive(Show)
type Report = {{ counts: Dict<String, Int>, seen: Map<String, Int> }};

fn main() -> Int {{
  let counts = Dict::insert(Dict::insert(Dict::new(), \"b\", 2), \"a\", 1);
  print(counts.show());
  print(no_entries().show());

  let one: Pair<String, Int> = {{ key: \"k\", value: 3 }};
  print(one.show());

  let seen: Map<String, Int> = Map::new();
  Map::insert(seen, \"only\", 1);
  print(seen.show());

  // The whole reason: a record holding both, derived.
  let r: Report = {{ counts: counts, seen: seen }};
  print(r.show());
  0
}}

fn no_entries() -> Dict<String, Int> {{ Dict::new() }}
"
        ),
    );

    assert_eq!(
        out,
        "{a: 1, b: 2}\n{}\nk: 3\n{only: 1}\nReport { counts: {a: 1, b: 2}, seen: {only: 1} }\n"
    );
}


/// **For a column and for a percentage.**
///
/// `to_string` gives the shortest form that reads back, so `0.5` and `0.125`
/// are three characters apart and a table of them does not line up. Two of the
/// review programs wrote the same `percent(part, whole)` helper and
/// `examples/risk_analyzer` had a third copy — the rounding was the half none
/// of them wanted to write.
///
/// **Not written in Khora.** Multiplying by `10^places`, rounding and dividing
/// back does the rounding in binary floating point, so a percentage renders as
/// `33.33` on one machine and `33.34` on another — the bug `std::decimal`
/// exists to prevent, reintroduced by the formatter. The runtime rounds the
/// decimal expansion of the double instead, the way C's `printf` and Go's
/// `strconv` do.
///
/// The two odd-looking answers are the right ones and are why this is bound:
/// `0.125` at two places is `0.12` because the tie goes to even, and `-2.675`
/// is `-2.67` because the nearest double to `-2.675` is a little above it.
/// Every other language agrees, and a Khora version doing its own arithmetic
/// would not.
#[test]
fn a_float_can_be_written_to_a_fixed_width() {
    let out = run(
        "text_to_fixed",
        &program(
            r#"  print(Float::to_fixed(0.5, 2));
  print(Float::to_fixed(2.0, 0));
  print(Float::to_fixed(1.0 / 3.0, 4));
  print(Float::to_fixed(0.125, 2));
  print(Float::to_fixed(-2.675, 2));
  // Still a rounding of an approximation, and it says so: `0.30` here is
  // `0.30` because the sum was near enough, not because it was right.
  print(Float::to_fixed(0.1 + 0.2, 2));
  // A column that lines up, which is the point.
  print("[" + String::pad_left(Float::to_fixed(5.0, 2), 8, " ") + "]");
  print("[" + String::pad_left(Float::to_fixed(1234.5, 2), 8, " ") + "]");
  // Negative and zero place counts clamp rather than misbehaving.
  print(Float::to_fixed(1.5, 0 - 3));"#,
        ),
    );

    assert_eq!(
        out,
        "0.50\n2\n0.3333\n0.12\n-2.67\n0.30\n[    5.00]\n[ 1234.50]\n2\n"
    );
}
