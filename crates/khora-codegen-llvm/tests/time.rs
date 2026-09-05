#![cfg(feature = "llvm")]

//! Civil dates, compiled and run.
//!
//! `Clock` gave milliseconds since 1970 and nothing that could say what day
//! that was. These check the calendar arithmetic against dates whose answers
//! are known independently — a leap day, a century that is not a leap year, a
//! date before 1970, and the weekday of a day everyone can look up.

use crate::harness;

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

fn run(name: &str, body: &str) -> String {
    let main = format!(
        r#"module demo::main;
import std::core::{{Eq, Option, Ord, Show, print}};
import std::time::{{Date, DateTime, Offset, Time, days_in_month, is_leap}};

fn shown_time(value: Option<Time>) -> String {{
  match value {{
    Option::Some(t) => t.show(),
    Option::None => "None",
  }}
}}

fn shown_when(value: Option<DateTime>) -> String {{
  match value {{
    Option::Some(w) => w.show(),
    Option::None => "None",
  }}
}}

fn shown_offset(value: Option<Offset>) -> String {{
  match value {{
    Option::Some(o) => o.show(),
    Option::None => "None",
  }}
}}

fn shown_int(value: Option<Int>) -> String {{
  match value {{
    Option::Some(n) => Int::to_string(n),
    Option::None => "None",
  }}
}}

fn shown_date(value: Option<Date>) -> String {{
  match value {{
    Option::Some(date) => date.show(),
    Option::None => "None",
  }}
}}

fn main() -> () {{
{body}
}}
"#
    );

    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(name);
    harness::ensure_runtime();
    std::fs::create_dir_all(&dir).expect("a workspace");
    let exe = dir.join(if cfg!(windows) { "program.exe" } else { "program" });
    let _ = std::fs::remove_file(&exe);

    let db = KhoraDatabase::new();
    let files = vec![
        SourceFile::new(&db, dir.join("core.kh"), std_source("core.kh")),
        SourceFile::new(&db, dir.join("time.kh"), std_source("time.kh")),
        SourceFile::new(&db, dir.join("main.kh"), main),
    ];
    let root = SourceRoot::new(&db, files);
    if let Err(errors) = khora_codegen_llvm::compile(&db, root, &exe) {
        let messages: Vec<String> = errors
            .into_iter()
            .map(|e| format!("{:?}: {}", e.range, e.message))
            .collect();
        panic!("compiling `{name}` failed:\n  {}", messages.join("\n  "));
    }

    let out = std::process::Command::new(&exe).output().expect("the program should run");
    assert_eq!(out.status.code(), Some(0), "`{name}` did not exit cleanly");
    String::from_utf8_lossy(&out.stdout).replace("\r\n", "\n")
}

/// Day zero is the first of January 1970, and it round-trips.
#[test]
fn the_epoch_is_where_it_should_be() {
    let out = run(
        "time_epoch",
        r#"  print((Date::of_days(0).show() == "1970-01-01").show());
  print(Date::of_days(0).show());
  print(Int::to_string(Date::to_days(Date::of_days(0))));"#,
    );
    assert_eq!(out, "true\n1970-01-01\n0\n");
}

/// **The century rule, which is where naive leap-year code is wrong.** 1900
/// was not a leap year and 2000 was.
#[test]
fn the_century_rule_is_the_one_people_get_wrong() {
    let out = run(
        "time_leap",
        r#"  print(is_leap(1900).show());
  print(is_leap(2000).show());
  print(is_leap(2024).show());
  print(is_leap(2026).show());
  print(Int::to_string(days_in_month(1900, 2)));
  print(Int::to_string(days_in_month(2000, 2)));"#,
    );
    assert_eq!(out, "false\ntrue\ntrue\nfalse\n28\n29\n");
}

/// The thirty-first of February is refused rather than rolled into March.
#[test]
fn a_day_that_does_not_exist_is_refused() {
    let out = run(
        "time_invalid",
        r#"  print(shown_date(Date::of(2026, 2, 31)));
  print(shown_date(Date::of(2024, 2, 29)));
  print(shown_date(Date::of(2026, 2, 29)));
  print(shown_date(Date::of(2026, 13, 1)));
  print(shown_date(Date::of(2026, 0, 1)));"#,
    );
    assert_eq!(out, "None\n2024-02-29\nNone\nNone\nNone\n");
}

/// **Dates before 1970**, where a truncating division puts the answer a whole
/// day out. The last millisecond of 1969 is in 1969.
#[test]
fn the_moment_before_the_epoch_is_the_day_before() {
    let out = run(
        "time_negative",
        r#"  print(DateTime::of_unix_millis(0 - 1).show());
  print(DateTime::of_unix_millis(0).show());
  print(Date::of_days(0 - 1).show());
  print(Int::to_string(DateTime::to_unix_millis(DateTime::of_unix_millis(0 - 1))));"#,
    );
    assert_eq!(
        out,
        "1969-12-31T23:59:59.999\n1970-01-01T00:00:00.000\n1969-12-31\n-1\n",
        "a millisecond before the epoch belongs to 1969"
    );
}

/// Weekdays, ISO numbering, against days anyone can check.
#[test]
fn weekdays_are_iso_numbered() {
    let out = run(
        "time_weekday",
        r#"  match Date::of(1970, 1, 1) { Option::Some(d) => print(Int::to_string(d.weekday())), Option::None => print("?") }
  match Date::of(2000, 1, 1) { Option::Some(d) => print(Int::to_string(d.weekday())), Option::None => print("?") }
  match Date::of(2026, 8, 25) { Option::Some(d) => print(Int::to_string(d.weekday())), Option::None => print("?") }
  match Date::of(1969, 7, 20) { Option::Some(d) => print(Int::to_string(d.weekday())), Option::None => print("?") }"#,
    );
    // 1970-01-01 Thursday, 2000-01-01 Saturday, 2026-08-25 Tuesday,
    // 1969-07-20 Sunday.
    assert_eq!(out, "4\n6\n2\n7\n");
}

/// Adding days crosses months, years and a leap day without a special case.
///
/// `2024-02-28` plus 366 is `2025-02-28`, not the first of March: the span
/// contains the leap day, so a year later is 366 days away rather than 365.
///
/// I expected March and the calendar was right.
#[test]
fn adding_days_crosses_every_boundary() {
    let out = run(
        "time_add",
        r#"  match Date::of(2024, 2, 28) {
    Option::Some(d) => {
      print(Date::add_days(d, 1).show());
      print(Date::add_days(d, 2).show());
      print(Date::add_days(d, 366).show());
      print(Date::add_days(d, 0 - 59).show());
    },
    Option::None => print("?"),
  }"#,
    );
    assert_eq!(out, "2024-02-29\n2024-03-01\n2025-02-28\n2023-12-31\n");
}

/// An offset is applied and taken away again, and the round trip is exact.
#[test]
fn an_offset_is_a_number_somebody_else_worked_out() {
    let out = run(
        "time_offset",
        r#"  let india = Offset::of_minutes(330);
  let millis = 1_600_000_000_000;
  print(DateTime::of_unix_millis(millis).show());
  print(DateTime::at_offset(millis, india).show());
  print(india.show());
  print(Int::to_string(DateTime::from_offset(DateTime::at_offset(millis, india), india)));"#,
    );
    assert_eq!(
        out,
        "2020-09-13T12:26:40.000\n2020-09-13T17:56:40.000\n+05:30\n1600000000000\n"
    );
}

/// Dates compare and sort by when they are, not by their fields.
#[test]
fn dates_order_by_the_calendar() {
    let out = run(
        "time_order",
        r#"  match Date::of(2026, 1, 31) {
    Option::Some(a) => match Date::of(2026, 2, 1) {
      Option::Some(b) => {
        print(if a < b { "before" } else { "not before" });
        print(if a == a { "equal" } else { "different" });
        print(Int::to_string(Date::days_until(a, b)));
      },
      Option::None => print("?"),
    },
    Option::None => print("?"),
  }"#,
    );
    assert_eq!(out, "before\nequal\n1\n");
}

// --- reading a date back in ------------------------------------------------

/// **What `Show` prints, read back.** A type that can be written and not read
/// is one-way in the direction that matters least: a timestamp comes *from*
/// somewhere else far more often than it goes to one.
#[test]
fn what_show_prints_parses() {
    let out = run(
        "time_roundtrip",
        r#"  print(shown_date(Date::of_string("2026-08-25")));
  print(shown_time(Time::of_string("09:05:00.123")));
  print(shown_when(DateTime::of_string("2026-08-25T09:05:00.123")));
  print(shown_offset(Offset::of_string("+05:30")));
  print(shown_offset(Offset::of_string("-05:00")));"#,
    );
    assert_eq!(
        out,
        "2026-08-25\n09:05:00.123\n2026-08-25T09:05:00.123\n+05:30\n-05:00\n"
    );
}

/// **Strict about the format, and the day still has to exist.**
///
/// The line is ISO 8601's extended form: fixed widths, hyphens and colons.
/// Everything else — a space where the `T` goes, unpadded fields, a US-style
/// date — is `None`, and a package can be lenient later. Drawn tight on
/// purpose: accepting more is a compatible change and accepting less never is.
#[test]
fn anything_that_is_not_iso_is_refused() {
    let out = run(
        "time_strict",
        r#"  print(shown_date(Date::of_string("2026-02-30")));
  print(shown_date(Date::of_string("2026-8-25")));
  print(shown_date(Date::of_string("08/25/2026")));
  print(shown_date(Date::of_string("2026-08-25 ")));
  print(shown_when(DateTime::of_string("2026-08-25 09:05:00")));
  print(shown_time(Time::of_string("9:05:00")));
  print(shown_time(Time::of_string("25:00:00")));
  print(shown_offset(Offset::of_string("+0530")));"#,
    );
    assert_eq!(out, "None\nNone\nNone\nNone\nNone\nNone\nNone\nNone\n");
}

/// **The fraction is optional, and it is a fraction.**
///
/// `.1` is a hundred milliseconds, not one — which is what a decimal point
/// means and the mistake a right-to-left reader makes. Four digits is refused
/// rather than rounded, because this type holds milliseconds and silently
/// dropping the microseconds off somebody's timestamp is a loss nothing later
/// could notice.
#[test]
fn a_fractional_second_is_read_as_a_fraction() {
    let out = run(
        "time_fraction",
        r#"  print(shown_time(Time::of_string("09:05:00")));
  print(shown_time(Time::of_string("09:05:00.1")));
  print(shown_time(Time::of_string("09:05:00.12")));
  print(shown_time(Time::of_string("09:05:00.123")));
  print(shown_time(Time::of_string("09:05:00.1234")));"#,
    );
    assert_eq!(
        out,
        "09:05:00.000\n09:05:00.100\n09:05:00.120\n09:05:00.123\nNone\n"
    );
}

/// **A zone on a zoneless type is refused, not ignored.**
///
/// `DateTime` has no zone and says so. Dropping a trailing `Z` would turn a
/// moment into a different moment, which is the one mistake a date library
/// must not make quietly — so the zoned form goes to `instant_of_string`,
/// which answers with the instant rather than a wall clock.
#[test]
fn a_zone_goes_to_the_function_that_has_somewhere_to_put_it() {
    let out = run(
        "time_zoned",
        r#"  print(shown_when(DateTime::of_string("2026-08-25T09:05:00Z")));
  print(shown_int(DateTime::instant_of_string("1970-01-01T00:00:00Z")));
  print(shown_int(DateTime::instant_of_string("2026-08-25T09:05:00.123Z")));
  // Five and a half hours east, so the same wall clock is an earlier instant.
  print(shown_int(DateTime::instant_of_string("2026-08-25T09:05:00+05:30")));
  print(shown_int(DateTime::instant_of_string("2026-08-25T09:05:00-05:00")));
  // No zone names no instant.
  print(shown_int(DateTime::instant_of_string("2026-08-25T09:05:00")));"#,
    );
    assert_eq!(
        out,
        "None\n0\n1787648700123\n1787628900000\n1787666700000\nNone\n"
    );
}

/// **Seconds beside milliseconds, because a Unix timestamp is usually
/// seconds** — `extract(epoch)`, a JSON field, a log line.
///
/// And floored, so a moment before 1970 rounds towards the past: `-1`
/// millisecond is second `-1`, the second that contains it. Truncating
/// division answers `0`, which is a whole second on the wrong side of the
/// epoch — the same bug `of_unix_millis` already exists to not have.
#[test]
fn epoch_seconds_floor_the_way_epoch_millis_do() {
    let out = run(
        "time_epoch_seconds",
        r#"  print(Int::to_string(DateTime::to_unix_seconds(DateTime::of_unix_seconds(1756112700))));
  print(DateTime::of_unix_seconds(0).show());
  print(Int::to_string(DateTime::to_unix_seconds(DateTime::of_unix_millis(0 - 1))));
  print(Int::to_string(DateTime::to_unix_seconds(DateTime::of_unix_millis(0 - 1000))));
  print(Int::to_string(DateTime::to_unix_seconds(DateTime::of_unix_millis(999))));"#,
    );
    assert_eq!(
        out,
        "1756112700\n1970-01-01T00:00:00.000\n-1\n-1\n0\n"
    );
}
