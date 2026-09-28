//! `unlabeled-flag`: a `true` or `false` passed, unlabeled, to a parameter
//! declared `Bool` that is not the first.
//!
//! **The rule is narrow on purpose.** A literal `Bool` in any position was
//! measured over the repository: 25 sites, 19 of them fine as written --
//! `assert_that(false, "..")` means "fail here", and `list.fold(true, ..)`
//! seeds an accumulator whose parameter is a type variable. The two
//! conditions below are the ones that separate the six real flags from those.

use khora_db::{Db, KhoraDatabase, SourceFile};
use khora_lint::{findings, Finding, UNLABELED_FLAG};

fn flags(db: &dyn Db, text: &str) -> Vec<Finding> {
    let file = SourceFile::new(db, "a.kh".into(), text.to_string());
    findings(db, file).iter().filter(|f| f.lint == UNLABELED_FLAG).cloned().collect()
}

fn fired(text: &str) -> Vec<String> {
    let db = KhoraDatabase::new();
    flags(&db, text).iter().map(|f| f.message.clone()).collect()
}

const REPLY: &str = "module m;\n\
    pub type Conn = { id: Int };\n\
    impl Conn {\n  pub fn send(self, body: String, keep: Bool) -> String { body }\n}\n\
    pub fn reply(connection: Conn, response: String, keep_alive: Bool) -> String { response }\n";

fn with_reply(body: &str) -> String {
    format!("{REPLY}fn go(c: Conn) -> String {{ {body} }}\n")
}

#[test]
fn a_bare_flag_after_the_first_parameter_is_reported() {
    let found = fired(&with_reply("reply(c, \"ok\", false)"));
    assert_eq!(
        found,
        vec![
            "`false` is passed to `keep_alive`, parameter 3 of `reply`, and nothing at the \
             call says what it means. Write `keep_alive: false`"
                .to_string()
        ]
    );
}

/// The range is the literal, so the fix is an insertion in front of it.
#[test]
fn the_finding_points_at_the_literal() {
    let text = with_reply("reply(c, \"ok\", true)");
    let db = KhoraDatabase::new();
    let found = flags(&db, &text);
    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(&text[found[0].range], "true");
}

#[test]
fn a_method_call_counts_the_receiver() {
    let found = fired(&with_reply("c.send(\"ok\", true)"));
    assert_eq!(found.len(), 1, "{found:?}");
    assert!(found[0].contains("parameter 3 of `Conn::send`"), "{found:?}");
    assert!(found[0].ends_with("Write `keep: true`"), "{found:?}");
    let found = fired(&with_reply("Conn::send(c, \"ok\", true)"));
    assert_eq!(found.len(), 1, "{found:?}");
}

#[test]
fn a_pipe_counts_the_piped_value() {
    let found = fired(&with_reply("c |> reply(\"ok\", true)"));
    assert_eq!(found.len(), 1, "{found:?}");
    assert!(found[0].contains("parameter 3 of `reply`"), "{found:?}");
}

#[test]
fn a_labeled_flag_is_quiet() {
    assert!(fired(&with_reply("reply(c, \"ok\", keep_alive: false)")).is_empty());
    assert!(fired(&with_reply("c.send(\"ok\", keep: true)")).is_empty());
}

/// `assert_that(false, "..")`: the first parameter is what the function is
/// about, and its name adds nothing a reader of the call needs.
#[test]
fn the_first_parameter_is_quiet() {
    let text = "module m;\nfn assert_that(condition: Bool, message: String) -> () { }\n\
        fn go() -> () { assert_that(false, \"unreachable\") }\n";
    assert!(fired(text).is_empty(), "{:?}", fired(text));
}

/// `fold(true, ..)`: the parameter is `B`, and `true` is a value of it, not
/// a switch. Only a parameter *declared* `Bool` is a flag.
#[test]
fn a_type_variable_at_bool_is_quiet() {
    let text = "module m;\nfn fold<B>(items: Int, start: B, step: (B, Int) -> B) -> B { start }\n\
        fn go() -> Bool { fold(3, true, fn (acc, n) => acc) }\n";
    assert!(fired(text).is_empty(), "{:?}", fired(text));
}

/// A variable already says what it is.
#[test]
fn a_named_value_is_quiet() {
    assert!(fired(&with_reply("let keep = true; reply(c, \"ok\", keep)")).is_empty());
    assert!(fired(&with_reply("reply(c, \"ok\", 1 == 1)")).is_empty());
}

/// Through a value there is no name to write, so nothing to suggest.
#[test]
fn a_call_through_a_value_is_quiet() {
    assert!(fired(&with_reply("let f = reply; f(c, \"ok\", true)")).is_empty());
}

/// `true |> f(1, _)` puts the literal in the placeholder's slot, and a
/// label cannot go there: the literal is not in the argument list.
#[test]
fn a_piped_literal_is_quiet() {
    let text = "module m;\nfn three(a: Int, flag: Bool) -> Int { a }\n\
        fn go() -> Int { true |> three(1, _) }\n";
    assert!(fired(text).is_empty(), "{:?}", fired(text));
}

#[test]
fn an_underscore_parameter_is_quiet() {
    let text = "module m;\nfn ignore(n: Int, _: Bool) -> Int { n }\nfn go() -> Int { ignore(1, true) }\n";
    assert!(fired(text).is_empty(), "{:?}", fired(text));
}

/// Off unless asked for: it flags code that is correct.
#[test]
fn it_is_off_by_default_and_in_the_idiomatic_group() {
    assert_eq!(khora_lint::default_level(UNLABELED_FLAG), khora_manifest::LintLevel::Allow);
    let group = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../std/lints/idiomatic.toml"),
    )
    .unwrap();
    assert!(group.contains("unlabeled-flag = \"warn\""), "{group}");
}
