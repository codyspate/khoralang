//! Labeled arguments: `f(a, verbose: true)` checks that the second
//! parameter of `f` is called `verbose`, and changes nothing else.
//!
//! **A label is a claim about the position it is written in.** It never
//! moves an argument, so a label that names some other parameter is refused
//! rather than obeyed: obeying it would make `f(b: x(), a: y())` evaluate in
//! an order nobody wrote, and nothing downstream of the checker would know a
//! label had been there. Every fixture here is one rule, fired and not fired.
//!
//! No `std`: each program declares what it calls.

use khora_db::{KhoraDatabase, SourceFile, SourceRoot};
use khora_types::diagnostics;

fn errors(text: &str) -> Vec<String> {
    let db = KhoraDatabase::new();
    let file = SourceFile::new(&db, "a.kh".into(), text.to_string());
    diagnostics(&db, file).iter().map(|e| e.message.clone()).collect()
}

fn assert_clean(text: &str) {
    let found = errors(text);
    assert!(found.is_empty(), "expected no errors, got {found:?}\n{text}");
}

/// At least one error says `needle`. For a reordering whose types also
/// disagree, where the type errors are true too.
fn assert_reports(text: &str, needle: &str) {
    let found = errors(text);
    assert!(
        found.iter().any(|e| e.contains(needle)),
        "expected an error containing {needle:?}, got {found:?}\n{text}"
    );
}

/// Exactly one error, and it says `needle`.
fn assert_refused(text: &str, needle: &str) {
    let found = errors(text);
    assert!(
        found.len() == 1 && found[0].contains(needle),
        "expected one error containing {needle:?}, got {found:?}\n{text}"
    );
}

const REPLY: &str = "module m;\n\
    pub type Conn = { id: Int };\n\
    pub fn reply(connection: Conn, response: String, keep_alive: Bool) -> String { response }\n";

fn with_reply(body: &str) -> String {
    format!("{REPLY}fn go(c: Conn) -> String {{ {body} }}\n")
}

#[test]
fn a_label_at_its_own_position_is_accepted() {
    assert_clean(&with_reply("reply(c, \"ok\", keep_alive: false)"));
    assert_clean(&with_reply("reply(connection: c, response: \"ok\", keep_alive: true)"));
    // Mixed freely, in any combination, because nothing moves.
    assert_clean(&with_reply("reply(connection: c, \"ok\", true)"));
    assert_clean(&with_reply("reply(c, response: \"ok\", true)"));
}

/// The headline refusal: both names exist, in the other order.
#[test]
fn a_label_does_not_reorder() {
    assert_reports(
        &with_reply("reply(c, keep_alive: true, response: \"ok\")"),
        "`keep_alive:` does not name this argument: parameter 2 of `reply` is `response`; \
         `keep_alive` is parameter 3, and a label does not move an argument",
    );
}

#[test]
fn an_unknown_label_is_refused() {
    assert_refused(
        &with_reply("reply(c, \"ok\", keep_alive_: true)"),
        "`keep_alive_:` does not name this argument: parameter 3 of `reply` is `keep_alive`",
    );
}

/// A label written twice cannot match both positions, so one of them is
/// wrong, and that one is what is reported.
#[test]
fn a_duplicate_label_is_refused() {
    let found = errors(&with_reply("reply(c, keep_alive: \"ok\", keep_alive: true)"));
    assert!(
        found.iter().any(|e| e.contains("`keep_alive:` does not name this argument: parameter 2")),
        "{found:?}"
    );
}

#[test]
fn a_label_on_an_underscore_parameter_is_refused() {
    assert_clean(
        "module m;\nfn ignore(_: Int, flag: Bool) -> Bool { flag }\nfn go() -> Bool { ignore(1, flag: true) }\n",
    );
    assert_refused(
        "module m;\nfn ignore(_: Int, flag: Bool) -> Bool { flag }\nfn go() -> Bool { ignore(x: 1, true) }\n",
        "parameter 1 of `ignore` is `_`, which has no name to label it with",
    );
}

#[test]
fn a_label_through_a_function_value_is_refused() {
    assert_refused(
        &with_reply("let f = reply; f(c, \"ok\", keep_alive: true)"),
        "`keep_alive:` labels an argument, but `f` is a value, and a function type has no \
         parameter names to check a label against; drop the label",
    );
}

#[test]
fn a_label_on_a_parameter_of_function_type_is_refused() {
    assert_refused(
        "module m;\nfn apply(f: (Int) -> Int, x: Int) -> Int { f(n: x) }\n",
        "`n:` labels an argument, but `f` is a value",
    );
}

#[test]
fn a_label_at_a_closure_call_is_refused() {
    assert_refused(
        "module m;\nfn go() -> Int { let g = fn (x: Int) => x; g(x: 1) }\n",
        "`x:` labels an argument, but `g` is a value",
    );
}

/// An effect's operation is a field of function type, so it has no names.
#[test]
fn a_label_at_an_effect_operation_is_refused() {
    assert_refused(
        "module m;\neffect Log { say: String -> () }\n\
         fn go() -> () with { log: Log } { log.say(line: \"hi\") }\n",
        "`line:` labels an argument, but `say` is a value",
    );
}

#[test]
fn a_named_payload_takes_labels() {
    let ng = "module m;\npub type Ng = | A(v: Int, w: Bool) | B(Int);\n";
    assert_clean(&format!("{ng}fn go() -> Ng {{ Ng::A(v: 1, w: true) }}\n"));
    assert_clean(&format!("{ng}fn go() -> Ng {{ Ng::A(1, w: true) }}\n"));
    assert_reports(
        &format!("{ng}fn go() -> Ng {{ Ng::A(w: true, v: 1) }}\n"),
        "`w:` does not name this argument: parameter 1 of `Ng::A` is `v`",
    );
}

#[test]
fn a_positional_payload_takes_none() {
    assert_refused(
        "module m;\npub type Ng = | A(v: Int, w: Bool) | B(Int);\nfn go() -> Ng { Ng::B(value: 1) }\n",
        "`value:` labels an argument, but `Ng::B`'s payload is positional and its fields have \
         no names; drop the label",
    );
}

const CONN: &str = "module m;\n\
    pub type Conn = { id: Int };\n\
    impl Conn {\n  pub fn reply(self, body: String, keep_alive: Bool) -> String { body }\n}\n";

/// In `x.f(a)` the receiver is parameter 1, so `a` is parameter 2.
#[test]
fn a_method_call_counts_from_the_second_parameter() {
    assert_clean(&format!("{CONN}fn go(c: Conn) -> String {{ c.reply(\"ok\", keep_alive: true) }}\n"));
    assert_clean(&format!(
        "{CONN}fn go(c: Conn) -> String {{ Conn::reply(c, body: \"ok\", keep_alive: true) }}\n"
    ));
    assert_refused(
        &format!("{CONN}fn go(c: Conn) -> String {{ c.reply(keep_alive: \"ok\", true) }}\n"),
        "`keep_alive:` does not name this argument: parameter 2 of `Conn::reply` is `body`",
    );
}

/// The inherent method is named as a reader would write it, not by the
/// checker's internal key.
#[test]
fn an_inherent_method_is_named_readably() {
    let found = errors(&format!(
        "{CONN}fn go(c: Conn) -> String {{ c.reply(\"ok\", keep: true) }}\n"
    ));
    assert!(found.iter().all(|e| !e.contains('#')), "{found:?}");
    assert!(found.iter().any(|e| e.contains("of `Conn::reply` is `keep_alive`")), "{found:?}");
}

/// The receiver has one spelling. `self` is parameter 1's name, and
/// accepting `self:` would give `Type::f(self: x)` as a second way to write
/// what `x.f()` already says.
#[test]
fn self_is_not_a_label() {
    assert_refused(
        &format!("{CONN}fn go(c: Conn) -> String {{ Conn::reply(self: c, \"ok\", true) }}\n"),
        "`self:` cannot label the receiver",
    );
}

const GREETER: &str = "module m;\n\
    pub type Conn = { id: Int };\n\
    pub trait Greeter {\n  fn greet(self, loud: Bool) -> String;\n}\n\
    impl Greeter for Conn {\n  fn greet(self, shout: Bool) -> String { if shout { \"HI\" } else { \"hi\" } }\n}\n";

/// A trait method's labels are the trait's names however it is reached. An
/// impl may rename a parameter; that name is local to its body.
#[test]
fn a_trait_method_is_labeled_by_the_trait() {
    for call in ["c.greet(loud: true)", "Greeter::greet(c, loud: true)", "Conn::greet(c, loud: true)"] {
        assert_clean(&format!("{GREETER}fn go(c: Conn) -> String {{ {call} }}\n"));
    }
    for call in ["c.greet(shout: true)", "Greeter::greet(c, shout: true)", "Conn::greet(c, shout: true)"] {
        // Named as the call reached it (`Conn::greet` or `Greeter::greet`);
        // what matters is that the name checked against is the trait's.
        let found = errors(&format!("{GREETER}fn go(c: Conn) -> String {{ {call} }}\n"));
        assert!(
            found.len() == 1
                && found[0].starts_with("`shout:` does not name this argument: parameter 2 of `")
                && found[0].ends_with("::greet` is `loud`"),
            "{call}: {found:?}"
        );
    }
}

/// Inside a generic function the call goes through the bound, and the
/// trait's names are the labels there too.
#[test]
fn a_call_through_a_bound_is_labeled_by_the_trait() {
    assert_clean(&format!("{GREETER}fn go<T: Greeter>(t: T) -> String {{ t.greet(loud: true) }}\n"));
    assert_refused(
        &format!("{GREETER}fn go<T: Greeter>(t: T) -> String {{ t.greet(shout: true) }}\n"),
        "parameter 2 of `Greeter::greet` is `loud`",
    );
}

/// The piped value takes parameter 1, or the `_` slot, and labels are checked
/// against where each written argument lands.
#[test]
fn a_pipe_fills_its_slot_before_labels_are_checked() {
    assert_clean(&with_reply("c |> reply(\"ok\", keep_alive: true)"));
    assert_clean(&with_reply("c |> reply(response: \"ok\", keep_alive: true)"));
    assert_refused(
        &with_reply("c |> reply(connection: \"ok\", true)"),
        "`connection:` does not name this argument: parameter 2 of `reply` is `response`",
    );
    let three = "module m;\nfn three(a: Int, b: Int, flag: Bool) -> Int { a + b }\n";
    assert_clean(&format!("{three}fn go() -> Int {{ 5 |> three(1, _, flag: true) }}\n"));
    assert_clean(&format!("{three}fn go() -> Int {{ 5 |> three(a: 1, _, flag: true) }}\n"));
    assert_refused(
        &format!("{three}fn go() -> Int {{ 5 |> three(1, _, b: true) }}\n"),
        "`b:` does not name this argument: parameter 3 of `three` is `flag`",
    );
}

#[test]
fn a_generic_function_takes_labels() {
    let pick = "module m;\nfn pick<A>(first: A, second: A, take_second: Bool) -> A { if take_second { second } else { first } }\n";
    assert_clean(&format!("{pick}fn go() -> Int {{ pick(1, 2, take_second: true) }}\n"));
    assert_refused(
        &format!("{pick}fn go() -> Int {{ pick(1, 2, second: true) }}\n"),
        "parameter 3 of `pick` is `take_second`",
    );
}

#[test]
fn an_extern_function_is_labeled_by_its_declared_names() {
    let abs = "module m;\nextern fn abs(value: Int) -> Int;\n";
    assert_clean(&format!("{abs}fn go() -> Int {{ abs(value: 3) }}\n"));
    assert_refused(
        &format!("{abs}fn go() -> Int {{ abs(n: 3) }}\n"),
        "`n:` does not name this argument: parameter 1 of `abs` is `value`",
    );
}

/// A capability is not a parameter, so a labeled call charges its row the
/// same as an unlabeled one.
#[test]
fn an_effect_row_is_untouched() {
    let text = "module m;\neffect Log { say: String -> () }\n\
        fn noisy(n: Int, verbose: Bool) -> Int with { log: Log } { if verbose { log.say(\"n\"); } n }\n\
        fn go() -> Int with { log: Log } { noisy(3, verbose: true) }\n";
    assert_clean(text);
    // And a labeled call still demands the row: dropping `with` is refused.
    let found = errors(
        "module m;\neffect Log { say: String -> () }\n\
         fn noisy(n: Int, verbose: Bool) -> Int with { log: Log } { if verbose { log.say(\"n\"); } n }\n\
         fn go() -> Int { noisy(3, verbose: true) }\n",
    );
    assert!(!found.is_empty(), "the row is still charged");
}

/// **Names survive an import.** Signatures are copied whole across modules,
/// so a caller in another file is checked against the declaration's names.
#[test]
fn names_cross_a_module_boundary() {
    let db = KhoraDatabase::new();
    let lib = SourceFile::new(
        &db,
        "net.kh".into(),
        "module net;\n\
         pub type Conn = { id: Int };\n\
         impl Conn {\n  pub fn reply(self, body: String, keep_alive: Bool) -> String { body }\n}\n\
         pub fn send(c: Conn, body: String, keep_alive: Bool) -> String { body }\n"
            .to_string(),
    );
    let app = SourceFile::new(
        &db,
        "app.kh".into(),
        "module app;\n\
         import net::{Conn, send};\n\
         fn ok(c: Conn) -> String { send(c, \"x\", keep_alive: true) }\n\
         fn ok2(c: Conn) -> String { c.reply(\"x\", keep_alive: true) }\n\
         fn bad(c: Conn) -> String { send(c, \"x\", keep: true) }\n\
         fn bad2(c: Conn) -> String { c.reply(\"x\", keep: true) }\n"
            .to_string(),
    );
    SourceRoot::new(&db, vec![lib, app]);
    let found: Vec<String> = diagnostics(&db, app).iter().map(|e| e.message.clone()).collect();
    assert_eq!(found.len(), 2, "{found:?}");
    assert!(found.iter().any(|e| e.contains("parameter 3 of `send` is `keep_alive`")), "{found:?}");
    assert!(
        found.iter().any(|e| e.contains("parameter 3 of `Conn::reply` is `keep_alive`")),
        "{found:?}"
    );
}

/// **The trait's names reach a caller in another module**, where the impl is
/// imported with its type and the trait declaration is not in the file.
#[test]
fn trait_names_cross_a_module_boundary() {
    let db = KhoraDatabase::new();
    let lib = SourceFile::new(
        &db,
        "net.kh".into(),
        "module net;\n\
         pub type Conn = { id: Int };\n\
         pub trait Greeter {\n  fn greet(self, loud: Bool) -> String;\n}\n\
         impl Greeter for Conn {\n  fn greet(self, shout: Bool) -> String { if shout { \"HI\" } else { \"hi\" } }\n}\n"
            .to_string(),
    );
    let app = SourceFile::new(
        &db,
        "app.kh".into(),
        "module app;\n\
         import net::{Conn, Greeter};\n\
         fn ok(c: Conn) -> String { c.greet(loud: true) }\n\
         fn ok2(c: Conn) -> String { Greeter::greet(c, loud: true) }\n\
         fn bad(c: Conn) -> String { c.greet(shout: true) }\n"
            .to_string(),
    );
    SourceRoot::new(&db, vec![lib, app]);
    let found: Vec<String> = diagnostics(&db, app).iter().map(|e| e.message.clone()).collect();
    assert_eq!(found.len(), 1, "{found:?}");
    assert!(found[0].contains("`shout:` does not name this argument"), "{found:?}");
    assert!(found[0].ends_with("is `loud`"), "{found:?}");
}

/// **The same when the caller does not import the trait.** `Conn::greet(c)`
/// reaches the impl through the type alone, and the checker then has the
/// impl's signature and not the trait's. The impl's `shout` was being taken
/// as the label, so `loud:` was refused and `shout:` accepted -- the one
/// spelling the rule says is local to the impl's body.
#[test]
fn trait_names_hold_when_only_the_type_is_imported() {
    let db = KhoraDatabase::new();
    let lib = SourceFile::new(
        &db,
        "net.kh".into(),
        "module net;\n\
         pub type Conn = { id: Int };\n\
         pub trait Greeter {\n  fn greet(self, loud: Bool) -> String;\n}\n\
         impl Greeter for Conn {\n  fn greet(self, shout: Bool) -> String { if shout { \"HI\" } else { \"hi\" } }\n}\n"
            .to_string(),
    );
    let app = SourceFile::new(
        &db,
        "app.kh".into(),
        "module app;\n\
         import net::{Conn};\n\
         fn ok(c: Conn) -> String { Conn::greet(c, loud: true) }\n\
         fn bad(c: Conn) -> String { Conn::greet(c, shout: true) }\n"
            .to_string(),
    );
    SourceRoot::new(&db, vec![lib, app]);
    let found: Vec<String> = diagnostics(&db, app).iter().map(|e| e.message.clone()).collect();
    assert_eq!(found.len(), 1, "{found:?}");
    assert!(found[0].starts_with("`shout:` does not name this argument"), "{found:?}");
    assert!(found[0].ends_with("is `loud`"), "{found:?}");
}

/// `struct({ .. })` is rewritten before it is typed, and the rewrite reads
/// only the record literal. A label there would be dropped without a word.
#[test]
fn a_label_on_struct_is_refused() {
    let db = KhoraDatabase::new();
    let schema = SourceFile::new(
        &db,
        "std/schema.kh".into(),
        "module std::schema;\n\
         pub type Schema<A> = { name: String };\n\
         pub fn int() -> Schema<Int> { { name: \"int\" } }\n\
         pub fn struct<A>(fields: A) -> Schema<A> { { name: \"struct\" } }\n"
            .to_string(),
    );
    let app = SourceFile::new(
        &db,
        "app.kh".into(),
        "module app;\nimport std::schema::{struct, int};\nfn go() -> () { let s = struct(fields: { port: int() }); }\n"
            .to_string(),
    );
    SourceRoot::new(&db, vec![schema, app]);
    let found: Vec<String> = diagnostics(&db, app).iter().map(|e| e.message.clone()).collect();
    assert!(found.iter().any(|e| e.contains("`struct` takes no labels")), "{found:?}");
}
