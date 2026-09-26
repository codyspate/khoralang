#![cfg(feature = "llvm")]

//! Pending raises merged into one closure's row, end to end.
//!
//! **Programs 0.3.0 ran right, which the first version of the pending-raise
//! fix refused or could not check.** A closure with a pending raise called
//! twice from another was refused as an "infinite type"; three times, and
//! `khora check` never returned. A closure passed where a signature fixes
//! its error type, and a `catch` around a call to such a closure in the same
//! body, were refused though the row already carried the raised type. And
//! the one program that showed the merge carried a raise at all -- a pending
//! raise beside a row variable -- builds and reaches the total-`catch` trap
//! if the merge drops it.
//!
//! Each compile runs with a deadline, so a regression to the loop fails.

use std::sync::mpsc;
use std::time::Duration;

use crate::matching::{refused, run_both, Ran};

const PRELUDE: &str = "module t;
fn print(value: Int);
extern fn khora_live_count() -> Int;

impl String {
  fn byte_length(self) -> Int;
}

pub type Nf = { p: String };
pub type Dn = { q: Int };
pub type Option<A> = | Some(v: A) | None;

fn nf() -> Int raises Nf { raise { p: \"x\" + \"1\" } }
fn app<'er>(g: () -> Int raises 'er) -> Int raises 'er { g()! }
fn call_nf0(k: () -> Int raises Nf) -> Int raises Nf { k()! }
";

/// `run_both`, on a thread with a 120 s deadline.
fn run_within(name: &'static str, source: String) -> Ran {
    let (send, receive) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = send.send(run_both(name, &source));
    });
    receive
        .recv_timeout(Duration::from_secs(120))
        .unwrap_or_else(|_| panic!("`{name}` did not build and run within 120 s"))
}

/// s_twice: refused "infinite type"; 0.3.0 printed 3.
#[test]
fn a_closure_with_a_pending_raise_called_twice_runs() {
    let ran = run_within(
        "merge_twice",
        format!(
            "{PRELUDE}fn work() -> Int raises Nf {{ let b = true; \
             let k = fn (e) => if b {{ raise e }} else {{ nf()! }}; \
             let twice = fn (x) => k(x)! + k(x)!; twice({{ p: \"a\" + \"bc\" }})! }}\n\
             fn main() -> Int {{ print(work()! catch {{ Nf {{ p }} => String::byte_length(p) }}); \
             print(khora_live_count()); 0 }}\n"
        ),
    );
    assert_eq!(ran.stdout, "3\n0\n", "stderr: {}", ran.stderr);
    assert_eq!(ran.code, Some(0));
}

/// s_thrice: `khora check` never returned.
#[test]
fn a_closure_with_a_pending_raise_called_three_times_runs() {
    let ran = run_within(
        "merge_thrice",
        format!(
            "{PRELUDE}fn work() -> Int raises Nf {{ let b = true; \
             let k = fn (e) => if b {{ raise e }} else {{ nf()! }}; \
             let thrice = fn (x) => k(x)! + k(x)! + k(x)!; thrice({{ p: \"a\" + \"bc\" }})! }}\n\
             fn main() -> Int {{ print(work()! catch {{ Nf {{ p }} => String::byte_length(p) }}); \
             print(khora_live_count()); 0 }}\n"
        ),
    );
    assert_eq!(ran.stdout, "3\n0\n", "stderr: {}", ran.stderr);
    assert_eq!(ran.code, Some(0));
}

/// cyc_swap: two closures merged in both orders.
#[test]
fn two_pending_raises_merged_in_both_orders_run() {
    let ran = run_within(
        "merge_both_orders",
        format!(
            "{PRELUDE}fn work() -> Int raises Dn + Nf {{ let c = true; \
             let k1 = fn (a) => {{ raise a }}; let k2 = fn (z) => {{ raise z }}; \
             let d: Dn = {{ q: 4 }}; let n: Nf = {{ p: \"y\" }}; \
             let one = fn () => k1(d)! + k2(n)!; let two = fn () => k2(n)! + k1(d)!; \
             if c {{ one()! }} else {{ two()! }} }}\n\
             fn main() -> Int {{ print(work()! catch {{ Dn {{ q }} => q * 10, Nf {{ p }} => 1 }}); 0 }}\n"
        ),
    );
    assert_eq!(ran.stdout, "40\n", "stderr: {}", ran.stderr);
    assert_eq!(ran.code, Some(0));
}

/// r_named_param0_same: refused "`Nf` is not accounted for here"; 0.3.0
/// printed 5.
#[test]
fn a_pending_raise_of_the_type_a_parameter_declares_runs() {
    let ran = run_within(
        "merge_closed_row",
        format!(
            "{PRELUDE}fn work() -> Int raises Nf {{ let mut e = Option::None; \
             let r = call_nf0(fn () => match e {{ Option::Some(x) => raise x, Option::None => nf()! }})!; \
             e = Option::Some({{ p: \"zz\" }}); r }}\n\
             fn main() -> Int {{ print(work()! catch {{ Nf {{ p }} => String::byte_length(p) }}); \
             print(khora_live_count()); 0 }}\n"
        ),
    );
    assert_eq!(ran.stdout, "2\n0\n", "stderr: {}", ran.stderr);
    assert_eq!(ran.code, Some(0));
}

/// o_catch_call_same: refused "`k` needs `Nf`"; 0.3.0 printed 103.
#[test]
fn a_catch_around_a_call_to_a_closure_with_a_pending_raise_runs() {
    let ran = run_within(
        "merge_catch_same_body",
        format!(
            "{PRELUDE}fn work() -> Int {{ let b = true; \
             let k = fn (e) => if b {{ raise e }} else {{ nf()! }}; \
             let n: Nf = {{ p: \"a\" + \"bc\" }}; \
             k(n)! catch {{ Nf {{ p }} => 100 + String::byte_length(p) }} }}\n\
             fn main() -> Int {{ print(work()); print(khora_live_count()); 0 }}\n"
        ),
    );
    assert_eq!(ran.stdout, "103\n0\n", "stderr: {}", ran.stderr);
    assert_eq!(ran.code, Some(0));
}

/// j_pend_g_Nfonly: with the merge keeping only the first tail it built, and
/// ended in the total-`catch` trap (status 134); 0.3.0 exited 139.
#[test]
fn a_pending_raise_beside_a_row_variable_is_refused() {
    let found = refused(
        "merge_beside_row_variable",
        &format!(
            "{PRELUDE}fn work() -> Int raises Nf {{ let b = false; \
             let k = fn (g, e) => if b {{ app(g)! }} else {{ raise e }}; \
             let d: Dn = {{ q: 4 }}; k(fn () => nf()!, d)! }}\n\
             fn main() -> Int {{ print(work()! catch {{ Nf {{ p }} => 1 }}); 0 }}\n"
        ),
    );
    assert!(
        found.iter().any(|e| e.contains("`k` needs `Dn`, which this function does not raise")),
        "{found:?}"
    );
}

// --- G1: one pending raise reached by two paths -------------------------

/// `outer` below reaches `k`'s raise through the `catch`, which takes `Nf`,
/// and directly, which does not.
const DIAMOND: &str = "fn nf0(c: Bool) -> Int raises Nf { if c { raise { p: \"z\" } } else { 0 } }\n";

/// e_diamond_nomark: reading each definition once per walk dropped the
/// direct path, so `outer`'s row closed to `{}` and `work`, declared to
/// raise nothing, built and exited 130 with no output. 0.3.0 refused it.
#[test]
fn a_pending_raise_reached_through_a_catch_and_directly_is_refused() {
    let found = refused(
        "diamond_nomark",
        &format!(
            "{PRELUDE}{DIAMOND}fn work() -> Int {{ let k = fn (e) => {{ raise e }}; \
             let n: Nf = {{ p: \"abc\" }}; \
             let outer = fn (x) => ((k(x)! + nf0(false)!) catch {{ Nf {{ p }} => 1 }}) + k(x)!; \
             outer(n) }}\n\
             fn main() -> Int {{ print(work()); 0 }}\n"
        ),
    );
    assert!(found.iter().any(|e| e.contains("needs `Nf`")), "{found:?}");
}

/// e_diamond_rev_nomark: the direct path first, which the dropped-path bug
/// happened to get right; guards the order.
#[test]
fn a_pending_raise_reached_directly_and_through_a_catch_is_refused() {
    let found = refused(
        "diamond_rev_nomark",
        &format!(
            "{PRELUDE}{DIAMOND}fn work() -> Int {{ let k = fn (e) => {{ raise e }}; \
             let n: Nf = {{ p: \"abc\" }}; \
             let outer = fn (x) => k(x)! + ((k(x)! + nf0(false)!) catch {{ Nf {{ p }} => 1 }}); \
             outer(n) }}\n\
             fn main() -> Int {{ print(work()); 0 }}\n"
        ),
    );
    assert!(found.iter().any(|e| e.contains("needs `Nf`")), "{found:?}");
}

/// e_diamond_declared: the same shape in a function that does declare `Nf`
/// is right, and must run: the `Nf` from the direct call reaches `main`'s
/// `catch`. With the direct path dropped, `outer` was compiled as raising
/// nothing and the program ended "khora: the stack ran out", status 139.
#[test]
fn a_pending_raise_reached_by_two_paths_in_a_declared_row_runs() {
    let ran = run_within(
        "diamond_declared",
        format!(
            "{PRELUDE}{DIAMOND}fn work() -> Int raises Nf {{ let k = fn (e) => {{ raise e }}; \
             let n: Nf = {{ p: \"abc\" }}; \
             let outer = fn (x) => ((k(x)! + nf0(false)!) catch {{ Nf {{ p }} => 1 }}) + k(x)!; \
             outer(n)! }}\n\
             fn main() -> Int {{ print(work()! catch {{ Nf {{ p }} => 100 + String::byte_length(p) }}); \
             print(khora_live_count()); 0 }}\n"
        ),
    );
    assert_eq!(ran.stdout, "103\n0\n", "stderr: {}", ran.stderr);
    assert_eq!(ran.code, Some(0));
}

// --- G2: a tail several closure rows end in -----------------------------

/// g_beside_dn_both: `outer` carried `Nf` beside `k`'s tail, `call_dn` closed
/// the tail at `raises Dn`, and `k` was credited with `outer`'s `Nf`. The
/// program built, `k` raised `Nf` through a type saying `raises Dn`, and the
/// run ended in the total-`catch` trap (status 134).
#[test]
fn a_raise_credited_with_another_closures_entries_is_refused() {
    let found = refused(
        "beside_dn_both",
        &format!(
            "{PRELUDE}{DIAMOND}fn call_dn(k: (Nf) -> Int raises Dn, n: Nf) -> Int raises Dn {{ k(n)! }}\n\
             fn work() -> Int raises Dn + Nf {{ let k = fn (e) => {{ raise e }}; \
             let n: Nf = {{ p: \"abc\" }}; let outer = fn (x) => nf0(false)! + k(x)!; \
             let a = call_dn(k, n)! catch {{ Dn {{ q }} => q }}; a + outer(n)! }}\n\
             fn main() -> Int {{ print(work()! catch {{ Nf {{ p }} => 1, Dn {{ q }} => q }}); 0 }}\n"
        ),
    );
    assert!(
        found.iter().any(|e| e.contains("had its error row closed to `{ Dn: Dn }`")),
        "{found:?}"
    );
}

/// u_beside_retry_like: the same through a no-argument wrapper; also the
/// trap. 0.3.0 printed the right answer by luck.
#[test]
fn a_raise_through_a_wrapper_credited_with_another_closures_entries_is_refused() {
    let found = refused(
        "beside_retry_like",
        &format!(
            "{PRELUDE}{DIAMOND}fn call_dn0(k: () -> Int raises Dn) -> Int raises Dn {{ k()! }}\n\
             fn work() -> Int raises Dn + Nf {{ let n: Nf = {{ p: \"abc\" }}; \
             let k = fn (e) => {{ raise e }}; let outer = fn () => nf0(false)! + k(n)!; \
             let a = call_dn0(fn () => k(n)!)! catch {{ Dn {{ q }} => q }}; a + outer()! }}\n\
             fn main() -> Int {{ print(work()! catch {{ Nf {{ p }} => 1, Dn {{ q }} => q }}); 0 }}\n"
        ),
    );
    assert!(
        found.iter().any(|e| e.contains("had its error row closed to `{ Dn: Dn }`")),
        "{found:?}"
    );
}

// --- H1: a cycle through two closures' types ----------------------------

/// `k` and `w` made one type ties `w`'s `catch` tail to `k`'s pending tail:
/// a cycle. `w` raises `Dn` from `k2`, which its `Nf` arm does not take.
const TIE_BODY: &str = "let n: Nf = { p: \"abc\" }; let d: Dn = { q: 4 }; \
     let k = fn (e) => { raise e }; let k2 = fn (z) => { raise z }; \
     let outer = fn (x) => k2(d)! + k(x)!; \
     let w = fn (x) => (outer(x)! + nf0(false)!) catch { Nf { p } => 1 }; ";

const TIE_DECLS: &str = "fn nf0(c: Bool) -> Int raises Nf { if c { raise { p: \"z\" } } else { 0 } }\n\
     pub type List<A> = | Cons(h: A, t: List<A>) | Nil;\n\
     fn same<T>(a: T, b: T) -> Int { 0 }\n";

/// cycr_list_nf_only: with the cut-short content remembered, `work` was
/// accepted as raising only `Nf`; `k2`'s `Dn` reached `main`'s `catch`,
/// sealed as total, and the run ended in the trap (status 134).
#[test]
fn a_raise_through_closures_tied_by_a_list_is_refused() {
    let found = refused(
        "tie_list_nf_only",
        &format!(
            "{PRELUDE}{TIE_DECLS}fn work() -> Int raises Nf {{ {TIE_BODY}\
             let ks = List::Cons(k, List::Cons(w, List::Nil)); w(n)! }}\n\
             fn main() -> Int {{ print(work()! catch {{ Nf {{ p }} => 1 }}); 0 }}\n"
        ),
    );
    assert!(
        found.iter().any(|e| e.contains("`w` needs `Dn`, which this function does not raise")),
        "{found:?}"
    );
}

/// cycr_same_nf_only: the same tie through `same<T>(a: T, b: T)`.
#[test]
fn a_raise_through_closures_tied_by_a_generic_parameter_is_refused() {
    let found = refused(
        "tie_same_nf_only",
        &format!(
            "{PRELUDE}{TIE_DECLS}fn work() -> Int raises Nf {{ {TIE_BODY}\
             let s = same(k, w); w(n)! }}\n\
             fn main() -> Int {{ print(work()! catch {{ Nf {{ p }} => 1 }}); 0 }}\n"
        ),
    );
    assert!(
        found.iter().any(|e| e.contains("`w` needs `Dn`, which this function does not raise")),
        "{found:?}"
    );
}

/// cycr_list_both: declared right, it runs, and `Dn` reaches its arm.
#[test]
fn closures_tied_by_a_list_in_a_row_that_carries_both_run() {
    let ran = run_within(
        "tie_list_both",
        format!(
            "{PRELUDE}{TIE_DECLS}fn work() -> Int raises Nf + Dn {{ {TIE_BODY}\
             let ks = List::Cons(k, List::Cons(w, List::Nil)); w(n)! }}\n\
             fn main() -> Int {{ print(work()! catch {{ Nf {{ p }} => 100, Dn {{ q }} => q * 10 }}); 0 }}\n"
        ),
    );
    assert_eq!(ran.stdout, "40\n", "stderr: {}", ran.stderr);
    assert_eq!(ran.code, Some(0));
}

/// cycr_list_pure: caught with `_`, it runs.
#[test]
fn closures_tied_by_a_list_caught_with_a_wildcard_run() {
    let ran = run_within(
        "tie_list_pure",
        format!(
            "{PRELUDE}{TIE_DECLS}fn work() -> Int {{ {TIE_BODY}\
             let ks = List::Cons(k, List::Cons(w, List::Nil)); w(n)! catch {{ _ => 9 }} }}\n\
             fn main() -> Int {{ print(work()); 0 }}\n"
        ),
    );
    assert_eq!(ran.stdout, "9\n", "stderr: {}", ran.stderr);
    assert_eq!(ran.code, Some(0));
}
