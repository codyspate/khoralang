//! Pending raises merged into one closure's row: the same tail met twice,
//! tails met in both orders, and a row already closed with the raised type.
//!
//! **Each of these either hung `khora check` or refused a program 0.3.0 ran
//! right.** Merging hung each pending raise's tail off the end of the last,
//! so a closure called twice from another linked a tail to itself -- two
//! calls were refused as an "infinite type", three never returned -- and two
//! closures merged in both orders made a two-tail cycle. And a closure passed
//! where a signature fixes its error type (`attempt`, a parameter written
//! `() -> Int raises Nf`) had its row closed before the raise was charged,
//! so a raise of the very type the row carried was refused.
//!
//! Every check here runs on a thread with a deadline, so a regression to the
//! loop fails the test rather than hanging the suite.

use std::sync::mpsc;
use std::time::Duration;

use khora_db::{KhoraDatabase, SourceFile};
use khora_types::diagnostics;

/// The diagnostics for `text`, or a failure if checking takes over 20 s.
fn errors(text: &str) -> Vec<String> {
    errors_within(text, Duration::from_secs(20))
}

/// The diagnostics for `text`, or a failure if checking takes over `limit`.
///
/// The test fails at the deadline and its process ends, so a regression to
/// work that grows without bound costs the deadline, not the machine.
fn errors_within(text: &str, limit: Duration) -> Vec<String> {
    let owned = text.to_string();
    let (send, receive) = mpsc::channel();
    std::thread::spawn(move || {
        let db = KhoraDatabase::new();
        let file = SourceFile::new(&db, "a.kh".into(), owned);
        let found: Vec<String> =
            diagnostics(&db, file).iter().map(|e| e.message.clone()).collect();
        let _ = send.send(found);
    });
    match receive.recv_timeout(limit) {
        Ok(found) => found,
        Err(_) => panic!("checking did not return within {limit:?}:\n{text}"),
    }
}

fn assert_clean(text: &str) {
    let found = errors(text);
    assert!(found.is_empty(), "expected no errors, got {found:?}\n{text}");
}

fn assert_reports(text: &str, needle: &str) {
    let found = errors(text);
    assert!(
        found.iter().any(|e| e.contains(needle)),
        "expected an error containing {needle:?}, got {found:?}\n{text}"
    );
}

const TYPES: &str = "module m;\n\
    pub type Result<A, E> = | Ok(v: A) | Err(e: E);\n\
    pub type Nf = { p: String };\n\
    pub type Dn = { q: Int };\n\
    fn nf() -> Int raises Nf { raise { p: \"x\" } }\n\
    fn app<'er>(g: () -> Int raises 'er) -> Int raises 'er { g()! }\n\
    fn call_nf0(k: () -> Int raises Nf) -> Int raises Nf { k()! }\n\
    fn attempt<A, E>(k: () -> A raises E) -> Result<A, E>;\n";

// --- F1: the same tail met twice, and tails met in both orders ----------

/// s_twice: refused "infinite type" when the tail was linked to itself.
#[test]
fn a_closure_with_a_pending_raise_called_twice_checks() {
    assert_clean(&format!(
        "{TYPES}fn work() -> Int raises Nf {{ let b = true; \
         let k = fn (e) => if b {{ raise e }} else {{ nf()! }}; \
         let twice = fn (x) => k(x)! + k(x)!; twice({{ p: \"abc\" }})! }}\n"
    ));
}

/// s_thrice: `khora check` never returned.
#[test]
fn a_closure_with_a_pending_raise_called_three_times_checks() {
    assert_clean(&format!(
        "{TYPES}fn work() -> Int raises Nf {{ let b = true; \
         let k = fn (e) => if b {{ raise e }} else {{ nf()! }}; \
         let thrice = fn (x) => k(x)! + k(x)! + k(x)!; thrice({{ p: \"abc\" }})! }}\n"
    ));
}

/// c_thrice: the closure raises only the pending value.
#[test]
fn a_closure_raising_only_a_pending_value_called_three_times_checks() {
    assert_clean(&format!(
        "{TYPES}fn work() -> Int raises Dn {{ let k = fn (e) => {{ raise e }}; \
         let thrice = fn (x) => k(x)! + k(x)! + k(x)!; let d: Dn = {{ q: 4 }}; thrice(d)! }}\n"
    ));
}

/// cyc_swap: `k1` then `k2` in one closure, `k2` then `k1` in another.
#[test]
fn two_pending_raises_merged_in_both_orders_check() {
    assert_clean(&format!(
        "{TYPES}fn work() -> Int raises Dn + Nf {{ let c = true; \
         let k1 = fn (a) => {{ raise a }}; let k2 = fn (z) => {{ raise z }}; \
         let d: Dn = {{ q: 4 }}; let n: Nf = {{ p: \"y\" }}; \
         let one = fn () => k1(d)! + k2(n)!; let two = fn () => k2(n)! + k1(d)!; \
         if c {{ one()! }} else {{ two()! }} }}\n"
    ));
}

/// cyc_swap3: the same with a third merge, which hung.
#[test]
fn two_pending_raises_merged_in_both_orders_three_times_check() {
    assert_clean(&format!(
        "{TYPES}fn work() -> Int raises Dn + Nf {{ let c = true; \
         let k1 = fn (a) => {{ raise a }}; let k2 = fn (z) => {{ raise z }}; \
         let d: Dn = {{ q: 4 }}; let n: Nf = {{ p: \"y\" }}; \
         let one = fn () => k1(d)! + k2(n)!; let two = fn () => k2(n)! + k1(d)!; \
         let three = fn () => k1(d)! + k2(n)!; \
         if c {{ one()! }} else {{ two()! + three()! }} }}\n"
    ));
}

/// Merging must still carry the raise: with only `Nf` declared, the `Dn`
/// from the called-twice closure is still owed.
#[test]
fn a_pending_raise_merged_twice_is_still_charged() {
    assert_reports(
        &format!(
            "{TYPES}fn work() -> Int raises Nf {{ let k = fn (e) => {{ raise e }}; \
             let twice = fn (x) => k(x)! + k(x)!; let d: Dn = {{ q: 4 }}; twice(d)! }}\n"
        ),
        "`twice` needs `Dn`, which this function does not raise",
    );
}

// --- F2: a row already closed with the raised type ----------------------

/// r_attempt_same: `attempt` closes the row at `Nf` before `e` is known.
#[test]
fn a_pending_raise_of_the_type_attempt_already_carries_checks() {
    assert_clean(&format!(
        "{TYPES}fn work() -> Int {{ let b = true; \
         let k = fn (e) => attempt(fn () => if b {{ raise e }} else {{ nf()! }}); \
         let n: Nf = {{ p: \"abcd\" }}; \
         match k(n) {{ Result::Ok(v) => v, Result::Err(x) => 1 }} }}\n"
    ));
}

/// r_named_param0_same: a parameter written `() -> Int raises Nf`.
#[test]
fn a_pending_raise_of_the_type_a_parameter_declares_checks() {
    assert_clean(&format!(
        "{TYPES}fn work() -> Int raises Nf {{ let mut e = Option::None; \
         let r = call_nf0(fn () => match e {{ Option::Some(x) => raise x, Option::None => nf()! }})!; \
         e = Option::Some({{ p: \"zz\" }}); r }}\n\
         pub type Option<A> = | Some(v: A) | None;\n"
    ));
}

/// o_catch_call_same: a `catch` in the same body as the closure, around a
/// call to it.
#[test]
fn a_catch_around_a_call_to_a_closure_with_a_pending_raise_checks() {
    assert_clean(&format!(
        "{TYPES}fn work() -> Int {{ let b = true; \
         let k = fn (e) => if b {{ raise e }} else {{ nf()! }}; \
         let n: Nf = {{ p: \"abc\" }}; k(n)! catch {{ Nf {{ p }} => 100 }} }}\n"
    ));
}

/// b3_attempt_one: nothing but the pending raise fixes `attempt`'s `E`, so
/// the closed row holds a place for one error type, and the raise fills it.
/// The first version of this fix refused it with a row printed as `{ _: _ }`.
/// (Reading `x.q` in the `Err` arm is still refused, "never worked out", as
/// on 0.3.0: the field read is checked before `E` is known.)
#[test]
fn a_pending_raise_fills_the_error_type_attempt_left_open() {
    assert_clean(&format!(
        "{TYPES}fn work() -> Int {{ let k = fn (e) => attempt(fn () => {{ raise e }}); \
         let d: Dn = {{ q: 4 }}; \
         match k(d) {{ Result::Ok(v) => v, Result::Err(_) => 1 }} }}\n"
    ));
}

/// b3_attempt_other: a `Dn` raised into a row `attempt` fixed at `Nf` is
/// still refused, and the message names the row.
#[test]
fn a_pending_raise_of_another_type_into_a_closed_row_is_refused() {
    assert_reports(
        &format!(
            "{TYPES}fn work() -> Int {{ let b = true; \
             let k = fn (e) => attempt(fn () => if b {{ raise e }} else {{ nf()! }}); \
             let d: Dn = {{ q: 4 }}; \
             match k(d) {{ Result::Ok(v) => v, Result::Err(x) => 1 }} }}\n"
        ),
        "does not carry it",
    );
}

/// A `catch` around a call that names a different type leaves the raise
/// owed by the enclosing function.
#[test]
fn a_catch_that_does_not_name_the_pending_raise_leaves_it_owed() {
    assert_reports(
        &format!(
            "{TYPES}fn work() -> Int {{ let b = true; \
             let k = fn (e) => if b {{ raise e }} else {{ nf()! }}; \
             let d: Dn = {{ q: 4 }}; k(d)! catch {{ Nf {{ p }} => 100 }} }}\n"
        ),
        "`k` needs `Dn`, which this function does not raise",
    );
}

// --- G1: one pending raise reached by two paths -------------------------

/// `outer` reaches `k`'s raise twice: through the `catch`, which takes `Nf`,
/// and directly, which does not. Reading a definition once per walk dropped
/// the second path, so `outer`'s row closed to `{}` and the program built
/// and exited 130 with no output; 0.3.0 refused it.
const DIAMOND: &str = "fn nf0(c: Bool) -> Int raises Nf { if c { raise { p: \"z\" } } else { 0 } }\n";

/// e_diamond_nomark: the `catch` path first.
#[test]
fn a_pending_raise_reached_through_a_catch_and_directly_is_still_owed() {
    assert_reports(
        &format!(
            "{TYPES}{DIAMOND}fn work() -> Int {{ let k = fn (e) => {{ raise e }}; \
             let n: Nf = {{ p: \"abc\" }}; \
             let outer = fn (x) => ((k(x)! + nf0(false)!) catch {{ Nf {{ p }} => 1 }}) + k(x)!; \
             outer(n) }}\n"
        ),
        "needs `Nf`",
    );
}

/// e_diamond_declared, with a row that does not carry `Nf`: the declared
/// form is right and runs (see the LLVM suite), so the checker's half of it
/// is that `outer` is charged `Nf` at all. Before, it built and segfaulted.
#[test]
fn a_pending_raise_reached_by_two_paths_is_charged_to_a_declared_row() {
    assert_reports(
        &format!(
            "{TYPES}{DIAMOND}fn work() -> Int raises Dn {{ let k = fn (e) => {{ raise e }}; \
             let n: Nf = {{ p: \"abc\" }}; \
             let outer = fn (x) => ((k(x)! + nf0(false)!) catch {{ Nf {{ p }} => 1 }}) + k(x)!; \
             outer(n)! }}\n"
        ),
        "`outer` needs `Nf`, which this function does not raise",
    );
}

/// e_diamond_rev_nomark: the direct path first. The dropped-path bug gave
/// the right answer in this order, so this guards the order, not the bug.
#[test]
fn a_pending_raise_reached_directly_and_through_a_catch_is_still_owed() {
    assert_reports(
        &format!(
            "{TYPES}{DIAMOND}fn work() -> Int {{ let k = fn (e) => {{ raise e }}; \
             let n: Nf = {{ p: \"abc\" }}; \
             let outer = fn (x) => k(x)! + ((k(x)! + nf0(false)!) catch {{ Nf {{ p }} => 1 }}); \
             outer(n) }}\n"
        ),
        "needs `Nf`",
    );
}

/// A chain of 22 diamonds: level `i` calls level `i - 1` inside a named
/// `catch` and again outside it. Walking every path without remembering
/// what a shared tail stood for is 2^22 walks; the check must finish in 3 s,
/// and still charge the `Nf` to a function that does not raise it.
#[test]
fn a_chain_of_diamonds_checks_quickly_and_charges_the_raise() {
    let mut body = String::from("let l0 = fn (e) => { raise e }; ");
    for i in 1..=22 {
        body.push_str(&format!(
            "let l{i} = fn (x) => ((l{p}(x)! + nf0(false)!) catch {{ Nf {{ p }} => 1 }}) + l{p}(x)!; ",
            p = i - 1
        ));
    }
    let text = format!(
        "{TYPES}{DIAMOND}fn work() -> Int raises Dn {{ {body}let n: Nf = {{ p: \"abc\" }}; l22(n)! }}\n"
    );
    let started = std::time::Instant::now();
    let found = errors(&text);
    let took = started.elapsed();
    assert!(
        found.iter().any(|e| e.contains("`l22` needs `Nf`, which this function does not raise")),
        "expected `l22` to be charged `Nf`, got {found:?}"
    );
    assert!(took < Duration::from_secs(3), "checking took {took:?}, over 3 s");
}

// --- G2: a tail several closure rows end in -----------------------------

/// g_beside_dn_both: `outer` carries `Nf` beside `k`'s tail, and `call_dn`
/// closes that tail at `raises Dn`. Crediting `k` with `outer`'s `Nf` let
/// `k` raise `Nf` through a type saying `raises Dn`; the program built and
/// ended in the total-`catch` trap. 0.3.0 printed the right answer by luck.
#[test]
fn a_raise_is_not_credited_with_another_closures_entries() {
    assert_reports(
        &format!(
            "{TYPES}{DIAMOND}fn call_dn(k: (Nf) -> Int raises Dn, n: Nf) -> Int raises Dn {{ k(n)! }}\n\
             fn work() -> Int raises Dn + Nf {{ let k = fn (e) => {{ raise e }}; \
             let n: Nf = {{ p: \"abc\" }}; let outer = fn (x) => nf0(false)! + k(x)!; \
             let a = call_dn(k, n)! catch {{ Dn {{ q }} => q }}; a + outer(n)! }}\n"
        ),
        "this `raise` sends `Nf`, and the closure it is in had its error row closed to `{ Dn: Dn }`",
    );
}

/// u_beside_retry_like: the same through a no-argument wrapper.
#[test]
fn a_raise_through_a_wrapper_is_not_credited_with_another_closures_entries() {
    assert_reports(
        &format!(
            "{TYPES}{DIAMOND}fn call_dn0(k: () -> Int raises Dn) -> Int raises Dn {{ k()! }}\n\
             fn work() -> Int raises Dn + Nf {{ let n: Nf = {{ p: \"abc\" }}; \
             let k = fn (e) => {{ raise e }}; let outer = fn () => nf0(false)! + k(n)!; \
             let a = call_dn0(fn () => k(n)!)! catch {{ Dn {{ q }} => q }}; a + outer()! }}\n"
        ),
        "this `raise` sends `Nf`, and the closure it is in had its error row closed to `{ Dn: Dn }`",
    );
}

// --- F3: the merge is what carries the raise ----------------------------

/// j_pend_g_Nfonly: the closure's row meets `app`'s `'er` and the pending
/// raise; keeping only the first dropped the `Dn`, and the program built and
/// ended in the total-`catch` trap.
#[test]
fn a_pending_raise_beside_a_row_variable_is_charged() {
    assert_reports(
        &format!(
            "{TYPES}fn work() -> Int raises Nf {{ let b = false; \
             let k = fn (g, e) => if b {{ app(g)! }} else {{ raise e }}; \
             let d: Dn = {{ q: 4 }}; k(fn () => nf()!, d)! }}\n"
        ),
        "`k` needs `Dn`, which this function does not raise",
    );
}

// --- G1, continued: entries kept once -----------------------------------

/// Seven levels, each calling the one below eight times: seven of them each
/// inside its own `catch` naming `Dn`, so the `Nf` passes through all eight
/// paths. Kept once per label and type, every level owes one `Nf`. Kept
/// once per path, the list is multiplied by eight at each level -- `8^7`,
/// two million copies at the top -- and checking ran out of a 1.5 GB
/// allowance in five seconds instead of finishing in a fraction of one.
#[test]
fn a_raise_reached_by_millions_of_paths_is_one_entry() {
    let mut body = String::from("let l0 = fn (e) => { raise e }; ");
    for i in 1..=7 {
        let p = i - 1;
        let caught: Vec<String> = (0..7)
            .map(|j| format!("((l{p}(x)! + dn0(false)!) catch {{ Dn {{ q }} => {j} }})"))
            .collect();
        body.push_str(&format!("let l{i} = fn (x) => {} + l{p}(x)!; ", caught.join(" + ")));
    }
    let text = format!(
        "{TYPES}fn dn0(c: Bool) -> Int raises Dn {{ if c {{ raise {{ q: 1 }} }} else {{ 0 }} }}\n\
         fn work() -> Int raises Dn {{ {body}let n: Nf = {{ p: \"abc\" }}; l7(n)! }}\n"
    );
    let started = std::time::Instant::now();
    let found = errors_within(&text, Duration::from_secs(3));
    let took = started.elapsed();
    assert!(
        found.iter().any(|e| e.contains("`l7` needs `Nf`, which this function does not raise")),
        "expected `l7` to be charged `Nf`, got {:?}",
        &found[..found.len().min(3)]
    );
    assert!(took < Duration::from_secs(3), "checking took {took:?}, over 3 s");
}

// --- H1: a cycle through two closures' types ----------------------------

/// `k` and `w` made one type -- by a list holding both, or a generic
/// `same<T>(a: T, b: T)` -- which makes `w`'s `catch` tail and `k`'s pending
/// tail one variable, so the definitions form a cycle. `w` raises `Dn`
/// (from `k2`, which its `Nf` arm does not take).
const TIE_BODY: &str = "let n: Nf = { p: \"abc\" }; let d: Dn = { q: 4 }; \
     let k = fn (e) => { raise e }; let k2 = fn (z) => { raise z }; \
     let outer = fn (x) => k2(d)! + k(x)!; \
     let w = fn (x) => (outer(x)! + nf0(false)!) catch { Nf { p } => 1 }; ";

const TIE_DECLS: &str = "fn nf0(c: Bool) -> Int raises Nf { if c { raise { p: \"z\" } } else { 0 } }\n\
     pub type List<A> = | Cons(h: A, t: List<A>) | Nil;\n\
     fn same<T>(a: T, b: T) -> Int { 0 }\n";

/// cycr_list_nf_only: remembering what a definition stood for while the
/// cycle through it was cut short kept `w`'s `Dn` out of its row, `work`
/// was accepted as raising only `Nf`, and the program ended in the
/// total-`catch` trap. 0.3.0 refused it.
#[test]
fn a_raise_through_closures_tied_by_a_list_is_still_owed() {
    assert_reports(
        &format!(
            "{TYPES}{TIE_DECLS}fn work() -> Int raises Nf {{ {TIE_BODY}\
             let ks = List::Cons(k, List::Cons(w, List::Nil)); w(n)! }}\n"
        ),
        "`w` needs `Dn`, which this function does not raise",
    );
}

/// cycr_same_nf_only: the same tie through a generic function's two
/// parameters of one type.
#[test]
fn a_raise_through_closures_tied_by_a_generic_parameter_is_still_owed() {
    assert_reports(
        &format!(
            "{TYPES}{TIE_DECLS}fn work() -> Int raises Nf {{ {TIE_BODY}\
             let s = same(k, w); w(n)! }}\n"
        ),
        "`w` needs `Dn`, which this function does not raise",
    );
}

/// cycr_list_both: declared right, the tied program checks.
#[test]
fn closures_tied_by_a_list_in_a_row_that_carries_both_check() {
    assert_clean(&format!(
        "{TYPES}{TIE_DECLS}fn work() -> Int raises Nf + Dn {{ {TIE_BODY}\
         let ks = List::Cons(k, List::Cons(w, List::Nil)); w(n)! }}\n"
    ));
}

/// cycr_list_pure: caught with `_`, the tied program checks.
#[test]
fn closures_tied_by_a_list_caught_with_a_wildcard_check() {
    assert_clean(&format!(
        "{TYPES}{TIE_DECLS}fn work() -> Int {{ {TIE_BODY}\
         let ks = List::Cons(k, List::Cons(w, List::Nil)); w(n)! catch {{ _ => 9 }} }}\n"
    ));
}

// --- G2-n2: the message for a row closed by where a caller is passed ----

/// g2_close_via_outer_nf: nothing the user wrote gave `k` a row -- `k`'s
/// tail was closed to `{}` because `o`, which calls it, was passed where a
/// signature says `raises Nf`. The message said `k` "was already given the
/// error row `{}`"; it has to say where the row came from, and what to write.
#[test]
fn a_row_closed_by_where_a_caller_is_passed_says_so() {
    let found = errors(&format!(
        "{TYPES}{DIAMOND}fn call_nf(k: (Nf) -> Int raises Nf, n: Nf) -> Int raises Nf {{ k(n)! }}\n\
         fn work() -> Int raises Nf {{ let n: Nf = {{ p: \"abc\" }}; \
         let k = fn (e) => {{ raise e }}; let o = fn (x) => nf0(false)! + k(x)!; \
         call_nf(o, n)! }}\n"
    ));
    assert!(
        found.iter().any(|e| e.contains("had its error row closed to `{}`")
            && e.contains("or a closure that calls it")
            && e.contains("Annotate the raised value's type")),
        "{found:?}"
    );
}

// --- C1: many closures on one cycle -------------------------------------

/// Middleware: layer `i` wraps layer `i - 1` in a `catch`, and every layer
/// is kept in one list, so all of them are one type and every layer's tail
/// is one variable. Reading the definitions path by path, with a cycle's
/// contents never remembered, walked every simple path through that cycle:
/// ten layers took 20 s and twelve never finished -- a checker, and a
/// language server, that does not return. Sixteen layers must check in 3 s.
#[test]
fn many_closures_in_one_list_check_quickly() {
    let mut body = String::from("let l0 = fn (e) => { raise e }; ");
    for i in 1..=16 {
        body.push_str(&format!(
            "let l{i} = fn (x) => (l{p}(x)! + nf0(false)!) catch {{ Nf {{ p }} => {i} }}; ",
            p = i - 1
        ));
    }
    let mut list = String::from("List::Nil");
    for i in (0..=16).rev() {
        list = format!("List::Cons(l{i}, {list})");
    }
    let text = format!(
        "{TYPES}{TIE_DECLS}fn work() -> Int raises Nf {{ {body}let hs = {list}; \
         l16({{ p: \"abc\" }})! }}\n"
    );
    let started = std::time::Instant::now();
    let found = errors_within(&text, Duration::from_secs(3));
    let took = started.elapsed();
    assert!(found.is_empty(), "expected no errors, got {found:?}");
    assert!(took < Duration::from_secs(3), "checking took {took:?}, over 3 s");
}

/// A chain of 22 diamonds with only its top tied to its bottom
/// (`same(l0, l22)`): one cycle through every level, and `2^22` simple
/// paths around it. The path walk took 25 s at 20 levels and did not finish
/// 22. It must check in 3 s, and still charge nothing it should not.
#[test]
fn a_chain_of_diamonds_tied_top_to_bottom_checks_quickly() {
    let mut body = String::from("let l0 = fn (e) => { raise e }; ");
    for i in 1..=22 {
        body.push_str(&format!(
            "let l{i} = fn (x) => ((l{p}(x)! + nf0(false)!) catch {{ Nf {{ p }} => 1 }}) + l{p}(x)!; ",
            p = i - 1
        ));
    }
    let text = format!(
        "{TYPES}{TIE_DECLS}fn work() -> Int raises Nf {{ {body}let s = same(l0, l22); \
         l22({{ p: \"abc\" }})! }}\n"
    );
    let started = std::time::Instant::now();
    let found = errors_within(&text, Duration::from_secs(3));
    let took = started.elapsed();
    assert!(found.is_empty(), "expected no errors, got {found:?}");
    assert!(took < Duration::from_secs(3), "checking took {took:?}, over 3 s");
}

// --- T1: what a cycle brings reaches every definition on it --------------

/// c3_chain_nf_only: three raising closures, two `catch`es in a chain, one
/// tie (`k` and `w` in a list). `k2`'s `Dn` reaches `w` only around the
/// cycle through `b` and `c`; a reading that let a definition in the middle
/// of the cycle keep what it had before the cycle closed dropped it, and a
/// function declared `raises Nf` was accepted.
#[test]
fn a_raise_around_a_cycle_through_two_catches_is_still_owed() {
    assert_reports(
        &format!(
            "{TYPES}{TIE_DECLS}fn work() -> Int raises Nf {{ let n: Nf = {{ p: \"abc\" }}; \
             let d: Dn = {{ q: 4 }}; let k = fn (e) => {{ raise e }}; \
             let k2 = fn (z) => {{ raise z }}; let k3 = fn (y) => {{ raise y }}; \
             let a = fn (x) => k(x)! + k2(d)!; \
             let b = fn (x) => (a(x)! + nf0(false)!) catch {{ Nf {{ p }} => 1 }}; \
             let c = fn (x) => (b(x)! + nf0(false)!) catch {{ Nf {{ p }} => 2 }}; \
             let w = fn (x) => c(x)! + k3(x)!; \
             let ks = List::Cons(k, List::Cons(w, List::Nil)); w(n)! }}\n"
        ),
        "`w` needs `Dn`, which this function does not raise",
    );
}

/// cd_arms_nf_only: the tie plus a diamond whose two paths go through a
/// `catch` naming `Nf` and one naming `Dn`. Only the `Nf` path lets `Dn`
/// through, and it does so around the cycle.
#[test]
fn a_raise_around_a_cycle_through_a_diamond_of_catches_is_still_owed() {
    assert_reports(
        &format!(
            "{TYPES}{TIE_DECLS}fn dn0(c: Bool) -> Int raises Dn {{ if c {{ raise {{ q: 1 }} }} else {{ 0 }} }}\n\
             fn work() -> Int raises Nf {{ let n: Nf = {{ p: \"abc\" }}; \
             let d: Dn = {{ q: 4 }}; let k = fn (e) => {{ raise e }}; \
             let k2 = fn (z) => {{ raise z }}; let outer = fn (x) => k2(d)! + k(x)!; \
             let w = fn (x) => ((outer(x)! + nf0(false)!) catch {{ Nf {{ p }} => 1 }}) \
             + ((outer(x)! + dn0(false)!) catch {{ Dn {{ q }} => 2 }}); \
             let s = same(k, w); w(n)! }}\n"
        ),
        "`w` needs `Dn`, which this function does not raise",
    );
}

// --- M1: a row fixed by a typed `let` -------------------------------------

/// A typed `let` fixes `k`'s row; nothing was "passed". The message named
/// only passing, which sends the reader looking for a call that is not
/// there.
#[test]
fn a_row_closed_by_a_typed_let_says_so() {
    let found = errors(&format!(
        "{TYPES}fn work() -> Int raises Nf {{ let d: Dn = {{ q: 4 }}; \
         let k = fn (e) => {{ raise e }}; let g: (Dn) -> Int raises Nf = k; g(d)! }}\n"
    ));
    assert!(
        found.iter().any(|e| e.contains("had its error row closed to")
            && e.contains("a typed `let`")),
        "{found:?}"
    );
}

/// What `w` raises reaches `outer` only around the cycle: `outer` calls `k`,
/// `k` is tied to `w`, and `w` raises `Bd` (from `k3`) that nothing else
/// `outer` calls raises. Read in the order the definitions were made,
/// `outer`'s is read before `w`'s, so one pass leaves the `Bd` out; it
/// takes a second. A function declared `raises Nf + Dn` calls `outer`.
#[test]
fn a_raise_that_comes_back_around_a_cycle_is_owed_by_an_earlier_closure() {
    assert_reports(
        &format!(
            "{TYPES}{TIE_DECLS}pub type Bd = {{ r: Int }};\n\
             fn work() -> Int raises Nf + Dn {{ let n: Nf = {{ p: \"abc\" }}; \
             let d: Dn = {{ q: 4 }}; let b: Bd = {{ r: 1 }}; \
             let k = fn (e) => {{ raise e }}; let k2 = fn (z) => {{ raise z }}; \
             let k3 = fn (y) => {{ raise y }}; \
             let outer = fn (x) => k2(d)! + k(x)!; \
             let w = fn (x) => (outer(x)! + k3(b)! + nf0(false)!) catch {{ Nf {{ p }} => 1 }}; \
             let ks = List::Cons(k, List::Cons(w, List::Nil)); outer(n)! }}\n"
        ),
        "`outer` needs `Bd`, which this function does not raise",
    );
}

// --- P1: a long chain against definition order beside a dense cycle ------

/// The reviewer's `combo_<n>_<m>`: `n` middleware layers in one list, each
/// adding its own late-typed error (so every layer's content holds up to `n`
/// entries), and in the same function an `m`-long chain of tied closures
/// built so that each link's error reaches the one defined before it.
fn combo(n: usize, m: usize) -> String {
    let mut decls = String::from("pub type Bd = { r: Int };\n");
    for i in 1..=n {
        decls.push_str(&format!("pub type E{i} = {{ v{i}: Int }};\n"));
    }
    let mut body = String::from("let n: Nf = { p: \"abc\" }; ");
    for i in 1..=n {
        body.push_str(&format!("let x{i}: E{i} = {{ v{i}: {i} }}; let z{i} = fn (e) => {{ raise e }}; "));
    }
    body.push_str("let l0 = fn (e) => { raise e }; ");
    for i in 1..=n {
        body.push_str(&format!(
            "let l{i} = fn (x) => (l{p}(x)! + z{i}(x{i})! + nf0(false)!) catch {{ Nf {{ p }} => {i} }}; ",
            p = i - 1
        ));
    }
    let mut list = String::from("List::Nil");
    for i in (0..=n).rev() {
        list = format!("List::Cons(l{i}, {list})");
    }
    body.push_str(&format!("let hs = {list}; "));
    body.push_str("let b: Bd = { r: 1 }; let kb = fn (y) => { raise y }; ");
    for i in 1..=m {
        body.push_str(&format!("let k{i} = fn (e) => {{ raise e }}; let y{i} = fn (e) => {{ raise e }}; "));
    }
    for i in 1..=m {
        body.push_str(&format!("let o{i} = fn (x) => k{i}(x)! + y{i}(x)!; "));
    }
    for i in 1..m {
        body.push_str(&format!(
            "let w{i} = fn (x) => (o{j}(x)! + nf0(false)!) catch {{ Nf {{ p }} => 1 }}; ",
            j = i + 1
        ));
    }
    body.push_str(&format!("let w{m} = fn (x) => (kb(b)! + nf0(false)!) catch {{ Nf {{ p }} => 1 }}; "));
    for i in 1..=m {
        body.push_str(&format!("let s{i} = same(k{i}, w{i}); "));
    }
    format!(
        "{TYPES}{TIE_DECLS}{decls}fn work() -> Int {{ {body}\
         let r = (l{n}(n)! + o1(n)!) catch {{ _ => 9 }}; r }}\n"
    )
}

/// combo_060_060. Re-reading every definition once per round made the chain
/// cost one round per link, and every round re-read the 60-layer clique:
/// 62 rounds, 253,000 reads of contents up to 60 entries, 26.6 s. It must
/// check in 3 s.
#[test]
fn a_long_chain_beside_a_dense_cycle_checks_quickly() {
    let text = combo(60, 60);
    let started = std::time::Instant::now();
    let found = errors_within(&text, Duration::from_secs(3));
    let took = started.elapsed();
    assert!(found.is_empty(), "expected no errors, got {:?}", &found[..found.len().min(3)]);
    assert!(took < Duration::from_secs(3), "checking took {took:?}, over 3 s");
}

// --- T2: a `catch` arm is checked against what finally reaches it ---------

/// late_arm2: `Gx<Int>` reaches `c0`'s arm, bound at `Gx<String>`, only
/// once a second tie has carried it -- `kg`'s raise into `w2`, `w2` tied to
/// `k2`, `k2` called by `o2`, and `o2` into `w1`, which is tied to `k1`,
/// which `c0` calls through `o1`. Checking the arms against what had arrived
/// after one pass accepted the program; it built and ran, and read an `Int`
/// as a `String`'s length.
#[test]
fn a_catch_arm_is_checked_against_an_error_that_arrives_late() {
    assert_reports(
        &format!(
            "{TYPES}{TIE_DECLS}pub type Gx<A> = {{ v: A }};\n\
             fn gs0(c: Bool) -> Int raises Gx<String> {{ if c {{ raise {{ v: \"s\" }} }} else {{ 0 }} }}\n\
             fn work() -> Int {{ let n: Nf = {{ p: \"abc\" }}; let gi: Gx<Int> = {{ v: 4 }}; \
             let k1 = fn (e) => {{ raise e }}; let k2 = fn (e) => {{ raise e }}; \
             let kg = fn (y) => {{ raise y }}; \
             let o1 = fn (x) => k1(x)! + 1; let o2 = fn (x) => k2(x)! + 1; \
             let c0 = fn (x) => (o1(x)! + gs0(false)! + nf0(false)!) \
             catch {{ Gx {{ v }} => 3, Nf {{ p }} => 2 }}; \
             let w1 = fn (x) => (o2(x)! + nf0(false)!) catch {{ Nf {{ p }} => 1 }}; \
             let w2 = fn (x) => (kg(gi)! + nf0(false)!) catch {{ Nf {{ p }} => 1 }}; \
             let t1 = List::Cons(k1, List::Cons(w1, List::Nil)); \
             let t2 = List::Cons(k2, List::Cons(w2, List::Nil)); c0(n) }}\n"
        ),
        "a `catch` arm handles one instantiation of a type",
    );
}

/// combo_080_080: the same at 80 and 80, 84 s on the rounds (53 s on a quiet
/// machine).
///
/// **A limit of 20 s, not 3.** The worklist checks this in 1.4-2.8 s on the
/// Linux and macOS runners, and took just over 3 s on the Windows one, whose
/// debug build is slower. The limit is there to catch the rounds coming
/// back, which take 53-84 s here, so 20 s still fails them by more than
/// twice over while leaving a slow runner several times the room it needs.
#[test]
fn a_longer_chain_beside_a_denser_cycle_checks_quickly() {
    let text = combo(80, 80);
    let started = std::time::Instant::now();
    let found = errors_within(&text, Duration::from_secs(20));
    let took = started.elapsed();
    assert!(found.is_empty(), "expected no errors, got {:?}", &found[..found.len().min(3)]);
    assert!(took < Duration::from_secs(20), "checking took {took:?}, over 20 s");
}
