//! `[fmt]` settings, once anything reads them.
//!
//! The table was parsed, validated and documented for a long time before the
//! formatter took an argument at all, so every one of these is checking that
//! the setting has an effect rather than that the effect is pretty.
//! Roadmap 14.20a.

use khora_fmt::{format, format_with, Options};

const SOURCE: &str = "module t;\n\npub fn f() -> Int {\n      1\n}\n";

/// The indentation of the one indented line.
fn indent_of(text: &str) -> String {
    let line = text.lines().find(|line| line.trim() == "1").expect("the body");
    line.chars().take_while(|c| c.is_whitespace()).collect()
}

#[test]
fn the_default_is_two_spaces() {
    let out = format(SOURCE).expect("it parses");
    assert_eq!(indent_of(&out), "  ");
    assert_eq!(out, format_with(SOURCE, &Options::default()).expect("it parses"));
}

#[test]
fn a_width_is_honoured() {
    let out = format_with(SOURCE, &Options::spaces(4)).expect("it parses");
    assert_eq!(indent_of(&out), "    ");
}

#[test]
fn tabs_are_one_tab_per_level_whatever_the_width_says() {
    // The width of a tab is the reader's setting, which is the entire reason
    // somebody picks tabs.
    let out = format_with(SOURCE, &Options::tabs()).expect("it parses");
    assert_eq!(indent_of(&out), "\t");
}

#[test]
fn a_width_of_zero_falls_back_rather_than_producing_nothing() {
    // This number comes out of a manifest. A formatter that refuses to run,
    // or that flattens a file, because somebody typed 0 is worse than one
    // that formats.
    let out = format_with(SOURCE, &Options::spaces(0)).expect("it parses");
    assert_eq!(indent_of(&out), "  ");
}

#[test]
fn formatting_is_still_idempotent_under_any_setting() {
    for options in [Options::default(), Options::spaces(4), Options::tabs()] {
        let once = format_with(SOURCE, &options).expect("it parses");
        let twice = format_with(&once, &options).expect("it parses");
        assert_eq!(once, twice, "{options:?}");
    }
}

#[test]
fn nesting_multiplies_the_indent() {
    let source = "module t;\n\npub fn f() -> Int {\nlet g = fn () => {\n1\n};\n2\n}\n";
    let out = format_with(source, &Options::spaces(4)).expect("it parses");
    let deepest = out
        .lines()
        .filter(|line| line.trim() == "1")
        .map(|line| line.chars().take_while(|c| c.is_whitespace()).count())
        .max()
        .expect("a nested line");
    assert!(deepest >= 8, "two levels of four:\n{out}");
}
