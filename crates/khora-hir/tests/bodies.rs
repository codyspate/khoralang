//! Body lowering: desugaring, scopes and resolution.

use khora_db::{Db, KhoraDatabase, Setter, SourceFile};
use khora_hir::body::{bodies, BinOp, Body, Expr, Literal, Pat, Stmt};

fn lower(db: &dyn Db, text: &str) -> Vec<(String, Body)> {
    let file = SourceFile::new(db, "a.kh".into(), text.to_string());
    bodies(db, file).clone()
}

fn only_body(db: &dyn Db, text: &str) -> Body {
    let mut all = lower(db, text);
    assert_eq!(all.len(), 1, "expected exactly one function");
    all.pop().unwrap().1
}

/// The tail expression of the function's top-level block.
fn tail(body: &Body) -> &Expr {
    let root = body.root.expect("no body root");
    match body.expr(root) {
        Expr::Block { tail: Some(t), .. } => body.expr(*t),
        other => other,
    }
}

fn errors(body: &Body) -> Vec<String> {
    body.errors.iter().map(|e| e.message.clone()).collect()
}

#[test]
fn a_pipeline_becomes_an_ordinary_call() {
    let db = KhoraDatabase::new();
    let body = only_body(&db, "module m;\nfn f(x: Int) -> Int { x |> g(2) }\n");

    // `x |> g(2)` is `g(x, 2)`: the piped value leads.
    let Expr::Call { args, .. } = tail(&body) else {
        panic!("expected a call, got {:?}", tail(&body));
    };
    assert_eq!(args.len(), 2, "piped value should be prepended");
    assert!(matches!(body.expr(args[0]), Expr::Local(_)), "first arg should be `x`");
    assert!(
        matches!(body.expr(args[1]), Expr::Literal(Literal::Int(n)) if n == "2"),
        "second arg should be the written one"
    );
}

/// The placeholder takes the piped value instead of the leading position.
#[test]
fn a_placeholder_moves_where_the_piped_value_lands() {
    let db = KhoraDatabase::new();
    let body = only_body(&db, "module m;\nfn f(x: Int) -> Int { x |> g(1, _, 3) }\n");

    let Expr::Call { args, .. } = tail(&body) else { panic!("expected a call") };
    assert_eq!(args.len(), 3, "arity should match what was written");
    assert!(matches!(body.expr(args[0]), Expr::Literal(Literal::Int(n)) if n == "1"));
    assert!(matches!(body.expr(args[1]), Expr::Local(_)), "`_` should hold the piped value");
    assert!(matches!(body.expr(args[2]), Expr::Literal(Literal::Int(n)) if n == "3"));
}

#[test]
fn piping_into_a_bare_name_calls_it() {
    let db = KhoraDatabase::new();
    let body = only_body(&db, "module m;\nfn f(x: Int) -> Int { x |> g }\n");

    let Expr::Call { args, .. } = tail(&body) else { panic!("expected a call") };
    assert_eq!(args.len(), 1);
}

#[test]
fn more_than_one_placeholder_in_a_stage_is_an_error() {
    let db = KhoraDatabase::new();
    let body = only_body(&db, "module m;\nfn f(x: Int) -> Int { x |> g(_, _) }\n");
    assert!(
        errors(&body).iter().any(|e| e.contains("at most once")),
        "{:?}",
        errors(&body)
    );
}

/// A `_` anywhere else is meaningless and should say so rather than lower to
/// something arbitrary.
#[test]
fn a_placeholder_outside_a_pipeline_is_an_error() {
    let db = KhoraDatabase::new();
    let body = only_body(&db, "module m;\nfn f() -> Int { g(_) }\n");
    assert!(
        errors(&body).iter().any(|e| e.contains("only meaningful in a pipeline")),
        "{:?}",
        errors(&body)
    );
}

#[test]
fn parameters_and_lets_resolve_to_locals() {
    let db = KhoraDatabase::new();
    let body = only_body(&db, "module m;\nfn f(a: Int) -> Int { let b = a; b }\n");

    assert!(matches!(tail(&body), Expr::Local(_)), "`b` should be a local");
    let names: Vec<_> = body.locals().map(|(_, l)| l.name.clone()).collect();
    assert_eq!(names, vec!["a", "b"]);
}

/// `let x = x;` must read the outer `x`, so the initializer is lowered before
/// the binding exists.
#[test]
fn a_let_initializer_cannot_see_its_own_binding() {
    let db = KhoraDatabase::new();
    let body = only_body(&db, "module m;\nfn f(x: Int) -> Int { let x = x; x }\n");

    assert!(errors(&body).is_empty(), "{:?}", errors(&body));
    let Expr::Block { stmts, .. } = body.expr(body.root.unwrap()) else { panic!() };
    let Stmt::Let { init: Some(init), .. } = &stmts[0] else { panic!("expected a let") };
    let Expr::Local(referenced) = body.expr(*init) else { panic!("initializer is not a local") };
    assert_eq!(body.local(*referenced).name, "x");
    // Two distinct locals share the name; the initializer must use the first.
    assert_eq!(body.locals().count(), 2, "shadowing should create a second local");
    assert_eq!(referenced.index(), 0, "initializer bound to the shadowing local");
}

#[test]
fn assigning_to_an_immutable_binding_is_an_error() {
    let db = KhoraDatabase::new();
    let body = only_body(&db, "module m;\nfn f() { let x = 1; x = 2; }\n");
    assert!(
        errors(&body).iter().any(|e| e.contains("not declared `mut`")),
        "{:?}",
        errors(&body)
    );

    let ok = only_body(&db, "module m;\nfn f() { let mut x = 1; x = 2; }\n");
    assert!(errors(&ok).is_empty(), "{:?}", errors(&ok));
}

#[test]
fn break_and_continue_outside_a_loop_are_errors() {
    let db = KhoraDatabase::new();
    let body = only_body(&db, "module m;\nfn f() { break; }\n");
    assert!(errors(&body).iter().any(|e| e.contains("outside a loop")), "{:?}", errors(&body));

    let inside = only_body(&db, "module m;\nfn f() { loop { break; } }\n");
    assert!(errors(&inside).is_empty(), "{:?}", errors(&inside));
}

/// A binding introduced by one arm must not leak into the next.
#[test]
fn match_arm_bindings_are_scoped_to_their_arm() {
    let db = KhoraDatabase::new();
    let body = only_body(
        &db,
        "module m;\nexport type R = | A(x: Int) | B;\nfn f(r: R) -> Int {\n  match r {\n    R::A(v) => v,\n    R::B => 0,\n  }\n}\n",
    );
    assert!(errors(&body).is_empty(), "{:?}", errors(&body));

    let leaked = only_body(
        &db,
        "module m;\nexport type R = | A(x: Int) | B;\nfn f(r: R) -> Int {\n  match r {\n    R::A(v) => 0,\n    R::B => v,\n  }\n}\n",
    );
    assert!(
        errors(&leaked).iter().any(|e| e.contains("cannot find `v`")),
        "a binding leaked between arms: {:?}",
        errors(&leaked)
    );
}

#[test]
fn constructors_resolve_in_patterns_and_expressions() {
    let db = KhoraDatabase::new();
    let body = only_body(
        &db,
        "module m;\nexport type R = | A | B;\nfn f(r: R) -> R {\n  match r {\n    R::A => R::B,\n    R::B => R::A,\n  }\n}\n",
    );
    assert!(errors(&body).is_empty(), "{:?}", errors(&body));

    let Expr::Match { arms, .. } = tail(&body) else { panic!("expected a match") };
    assert_eq!(arms.len(), 2);
    assert!(matches!(body.pat(arms[0].pat), Pat::Path(_)), "constructor pattern not resolved");
}

#[test]
fn an_unknown_constructor_is_reported() {
    let db = KhoraDatabase::new();
    let body = only_body(
        &db,
        "module m;\nexport type R = | A;\nfn f(r: R) -> Int {\n  match r {\n    R::Nope => 1,\n  }\n}\n",
    );
    assert!(
        errors(&body).iter().any(|e| e.contains("cannot find constructor")),
        "{:?}",
        errors(&body)
    );
}

#[test]
fn operators_and_control_flow_lower() {
    let db = KhoraDatabase::new();
    let body = only_body(
        &db,
        "module m;\nfn f(n: Int) -> Int {\n  let mut total = 0;\n  while n > 0 { total = total + n; }\n  if total > 10 { return total; }\n  total\n}\n",
    );
    assert!(errors(&body).is_empty(), "{:?}", errors(&body));

    let has = |pred: fn(&Expr) -> bool| body.exprs().any(|(_, e)| pred(e));
    assert!(has(|e| matches!(e, Expr::While { .. })), "no while");
    assert!(has(|e| matches!(e, Expr::If { .. })), "no if");
    assert!(has(|e| matches!(e, Expr::Return(_))), "no return");
    assert!(has(|e| matches!(e, Expr::Assign { .. })), "no assignment");
    assert!(has(|e| matches!(e, Expr::Binary { op: BinOp::Gt, .. })), "no comparison");
}

/// Syntax outside the phase 2 subset must be reported and marked, never
/// silently dropped — a later phase finds every site by one variant.
#[test]
fn syntax_outside_the_subset_is_marked_not_dropped() {
    let db = KhoraDatabase::new();
    let body = only_body(&db, "module m;\nfn f() -> Int { g() catch { E::A(_) => 1, } }\n");

    assert!(
        body.exprs().any(|(_, e)| matches!(e, Expr::Unsupported(_))),
        "`catch` was dropped instead of marked"
    );
    assert!(errors(&body).iter().any(|e| e.contains("catch")), "{:?}", errors(&body));
}

/// Closures and record literals used to be in the list above. Both lower for
/// real now.

#[test]
fn a_record_literal_lowers() {
    let db = KhoraDatabase::new();
    let body = only_body(&db, "module m;\nfn f() -> Int { let r = { a: 1 }; 1 }\n");
    assert!(errors(&body).is_empty(), "{:?}", errors(&body));
    assert!(
        body.exprs().any(|(_, e)| matches!(e, Expr::Record { .. })),
        "no record in the body"
    );
}

/// `handler for E { .. }` is a record literal whose type the syntax names.
#[test]
fn a_handler_is_a_record_that_names_its_type() {
    let db = KhoraDatabase::new();
    let body = only_body(
        &db,
        "module m;\nfn f() -> Int { let h = handler for Ledger { get: fn i => 1 }; 1 }\n",
    );
    assert!(errors(&body).is_empty(), "{:?}", errors(&body));
    let owner = body.exprs().find_map(|(_, e)| match e {
        Expr::Record { owner, .. } => Some(owner.clone()),
        _ => None,
    });
    assert_eq!(owner, Some(Some("Ledger".to_string())));
}

#[test]
fn a_closure_lowers_to_a_lambda() {
    let db = KhoraDatabase::new();
    let body = only_body(&db, "module m;\nfn f() -> Int { let g = fn x => x; 1 }\n");

    assert!(errors(&body).is_empty(), "{:?}", errors(&body));
    assert!(
        body.exprs().any(|(_, e)| matches!(e, Expr::Lambda { .. })),
        "no lambda in the body"
    );
}

/// A lambda's body sits in the enclosing function's arena, and a local from
/// outside it is recorded as a capture rather than resolved afresh.
#[test]
fn a_lambda_records_what_it_captures() {
    let db = KhoraDatabase::new();
    let body = only_body(
        &db,
        "module m;\nfn f() -> Int { let n = 1; let g = fn x => x + n; 2 }\n",
    );

    let captures = body
        .exprs()
        .find_map(|(_, e)| match e {
            Expr::Lambda { captures, .. } => Some(captures.clone()),
            _ => None,
        })
        .expect("no lambda");
    assert_eq!(captures.len(), 1, "expected exactly `n` to be captured");
    assert_eq!(body.local(captures[0]).name, "n");
}

/// A lambda's own parameter is not a capture: it is declared inside.
#[test]
fn a_lambda_parameter_is_not_a_capture() {
    let db = KhoraDatabase::new();
    let body = only_body(&db, "module m;\nfn f() -> Int { let g = fn x => x + 1; 2 }\n");

    let captures = body
        .exprs()
        .find_map(|(_, e)| match e {
            Expr::Lambda { captures, .. } => Some(captures.clone()),
            _ => None,
        })
        .expect("no lambda");
    assert!(captures.is_empty(), "{captures:?}");
}

/// An inner lambda's free variable is still free in the outer one, so both
/// have to capture it or the inner would have nothing to read.
#[test]
fn a_nested_lambda_makes_its_free_variables_captures_of_both() {
    let db = KhoraDatabase::new();
    let body = only_body(
        &db,
        "module m;\nfn f() -> Int { let n = 1; let g = fn x => fn y => x + y + n; 2 }\n",
    );

    let all: Vec<Vec<_>> = body
        .exprs()
        .filter_map(|(_, e)| match e {
            Expr::Lambda { captures, .. } => {
                Some(captures.iter().map(|c| body.local(*c).name.clone()).collect())
            }
            _ => None,
        })
        .collect();
    assert_eq!(all.len(), 2, "expected two lambdas");
    // Innermost first: it is completed before the one containing it.
    assert_eq!(all[0], vec!["x".to_string(), "n".to_string()]);
    assert_eq!(all[1], vec!["n".to_string()]);
}

/// A capture is a copy, so writing to it would change nothing outside.
#[test]
fn assigning_to_a_capture_is_rejected() {
    let db = KhoraDatabase::new();
    let body = only_body(
        &db,
        "module m;\nfn f() -> Int { let mut n = 1; let g = fn x => { n = x; n }; 2 }\n",
    );
    assert!(
        errors(&body).iter().any(|e| e.contains("captured by value")),
        "{:?}",
        errors(&body)
    );
}

#[test]
fn every_function_in_a_file_is_lowered() {
    let db = KhoraDatabase::new();
    let all = lower(&db, "module m;\nfn a() -> Int { 1 }\nfn b() -> Int { 2 }\nfn c() -> Int;\n");
    let names: Vec<_> = all.iter().map(|(n, _)| n.clone()).collect();
    assert_eq!(names, vec!["a", "b"], "a signature without a body has nothing to lower");
}

/// Lowering one file must not read another, the same property item collection
/// has, or an edit anywhere would invalidate every body.
#[test]
fn editing_one_file_does_not_relower_another() {
    let (mut db, log) = KhoraDatabase::logged();
    let a = SourceFile::new(&db, "a.kh".into(), "module a;\nfn f() -> Int { 1 }\n".to_string());
    let b = SourceFile::new(&db, "b.kh".into(), "module b;\nfn g() -> Int { 2 }\n".to_string());

    bodies(&db, a);
    bodies(&db, b);
    log.take();

    b.set_text(&mut db).to("module b;\nfn g() -> Int { 99 }\n".to_string());

    bodies(&db, a);
    bodies(&db, b);

    let executed = log.take();
    assert!(
        !executed.iter().any(|e| e.contains("a.kh")),
        "file a was relowered: {executed:?}"
    );
}
