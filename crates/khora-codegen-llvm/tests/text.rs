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
import std::core::{Array, Dict, Eq, List, Option, Ord, Ordering, Pair, Shared, Show, Split};

/// The two helpers every list test below wants: an empty `List<Int>` that
/// inference can name, and an `Option<Int>` written out.
fn empty() -> List<Int> { List::Nil }

fn shown(value: Option<Int>) -> String {
  match value { Option::Some(n) => Int::to_string(n), Option::None => \"none\" }
}

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
