//! `if`, `for` and `..` inside `[..]`: the element forms.
//!
//! **What these guard: a literal that silently changes length.** `[if c { a }]`
//! and `[for x in xs { () }]` were one-element lists before the element forms
//! existed, and a parser that reads either as an element form gives a working
//! program a different answer without a diagnostic. The forms are told apart by
//! `=>`, so every test here that asserts a form also has a sibling asserting the
//! block-bodied spelling is still an ordinary expression.

use khora_syntax::{parse, SyntaxKind, SyntaxNode};

/// Parses `body` inside a function, requiring no errors and a lossless tree.
fn clean(body: &str) -> SyntaxNode {
    let src = format!("module m;\nfn f() {{ {body} }}\n");
    let parsed = parse(&src);
    assert_eq!(parsed.syntax().text().to_string(), src, "lost source text");
    assert!(parsed.errors().is_empty(), "{body}: {:?}\n{}", parsed.errors(), parsed.debug_tree());
    parsed.syntax()
}

/// The error messages for `body` inside a function.
fn errors(body: &str) -> Vec<String> {
    let src = format!("module m;\nfn f() {{ {body} }}\n");
    let parsed = parse(&src);
    assert_eq!(parsed.syntax().text().to_string(), src, "lost source text");
    parsed.errors().iter().map(|e| e.message.clone()).collect()
}

/// The first `LIST_EXPR`, and the kinds of its direct children in order.
fn elements(tree: &SyntaxNode) -> Vec<SyntaxKind> {
    let list = tree
        .descendants()
        .find(|n| n.kind() == SyntaxKind::LIST_EXPR)
        .expect("a list literal");
    list.children().map(|c| c.kind()).collect()
}

fn count(tree: &SyntaxNode, kind: SyntaxKind) -> usize {
    tree.descendants().filter(|n| n.kind() == kind).count()
}

#[test]
fn each_form_is_its_own_element() {
    use SyntaxKind::*;
    let tree = clean("[a, if c => b, for r in rows => f(r), ..tail]");
    let kinds = elements(&tree);
    assert_eq!(kinds.len(), 4, "{kinds:?}");
    assert_eq!(kinds[1], LIST_IF, "{kinds:?}");
    assert_eq!(kinds[2], LIST_FOR, "{kinds:?}");
    assert_eq!(kinds[3], LIST_SPREAD, "{kinds:?}");
}

#[test]
fn forms_nest() {
    let tree = clean("[for x in xs => if x != 2 => x]");
    assert_eq!(count(&tree, SyntaxKind::LIST_FOR), 1);
    assert_eq!(count(&tree, SyntaxKind::LIST_IF), 1);
    let tree = clean("[for x in xs => for y in ys => (x, y)]");
    assert_eq!(count(&tree, SyntaxKind::LIST_FOR), 2);
    let tree = clean("[if c => ..xs else ..ys]");
    assert_eq!(count(&tree, SyntaxKind::LIST_SPREAD), 2);
}

#[test]
fn else_if_chains_are_element_forms_all_the_way_down() {
    let tree = clean("[if a => 1 else if b => 2 else 3]");
    assert_eq!(count(&tree, SyntaxKind::LIST_IF), 2);
    assert_eq!(count(&tree, SyntaxKind::IF_EXPR), 0);
}

/// `[if a => if b => x else y]`: the `else` is the inner `if`'s.
#[test]
fn a_dangling_else_belongs_to_the_nearest_if() {
    let tree = clean("[if a => if b => x else y]");
    let outer = tree.descendants().find(|n| n.kind() == SyntaxKind::LIST_IF).expect("an outer if");
    let has_else = |n: &SyntaxNode| {
        n.children_with_tokens().any(|t| t.kind() == SyntaxKind::ELSE_KW)
    };
    assert!(!has_else(&outer), "the outer `if` took the `else`:\n{outer:#?}");
    let inner = outer.children().find(|n| n.kind() == SyntaxKind::LIST_IF).expect("an inner if");
    assert!(has_else(&inner), "{inner:#?}");
}

#[test]
fn a_trailing_comma_is_allowed_after_a_form() {
    use SyntaxKind::*;
    let tree = clean("[if c => 1, for x in xs => x, ..ys,]");
    assert_eq!(elements(&tree), vec![LIST_IF, LIST_FOR, LIST_SPREAD]);
}

/// An element form is complete: `+ 1` belongs to the element's value.
#[test]
fn an_operator_after_the_arrow_belongs_to_the_value() {
    let tree = clean("[if c => a + 1]");
    assert_eq!(elements(&tree), vec![SyntaxKind::LIST_IF]);
    let arm = tree.descendants().find(|n| n.kind() == SyntaxKind::LIST_IF).unwrap();
    assert!(arm.children().any(|c| c.kind() == SyntaxKind::BIN_EXPR), "{arm:#?}");
}

/// **The additive guarantee.** A block-bodied `if` or `for` in a literal is
/// the expression it always was, so each of these is one element.
#[test]
fn block_bodies_are_still_expressions() {
    use SyntaxKind::*;
    assert_eq!(elements(&clean("[if c { a } else { b }]")), vec![IF_EXPR]);
    assert_eq!(elements(&clean("[if c { () }]")), vec![IF_EXPR]);
    assert_eq!(elements(&clean("[for x in xs { () }]")), vec![FOR_EXPR]);
    assert_eq!(elements(&clean("[1, 2, 3]")), vec![LITERAL_EXPR, LITERAL_EXPR, LITERAL_EXPR]);
}

/// A lambda body and a match arm keep their `=>`: only an `if`/`for` head
/// directly in a literal takes one.
#[test]
fn an_arrow_elsewhere_in_a_literal_is_not_an_element_form() {
    let tree = clean("[fn x => x, match o { _ => 1 }]");
    assert_eq!(count(&tree, SyntaxKind::LIST_IF), 0);
    assert_eq!(count(&tree, SyntaxKind::LIST_FOR), 0);
}

/// An `if` inside a parenthesized element is an expression again.
#[test]
fn a_form_inside_parentheses_is_refused() {
    let found = errors("[(if c => 1)]");
    assert_eq!(found.len(), 1, "{found:?}");
    assert!(found[0].contains("only inside `[..]`"), "{found:?}");
}

/// Only the head that *begins* an element takes `=>`: an `if` nested inside
/// an element's value -- here a call argument -- is an expression, even
/// though it is inside a literal and inside an element form.
#[test]
fn a_form_nested_in_an_elements_value_is_refused() {
    let found = errors("[if c => g(if d => 1)]");
    assert_eq!(found.len(), 1, "{found:?}");
    assert!(found[0].contains("only inside `[..]`"), "{found:?}");
    let found = errors("[for x in xs => x + (for y in ys => y)]");
    assert_eq!(found.len(), 1, "{found:?}");
    assert!(found[0].contains("only inside `[..]`"), "{found:?}");
}

/// Outside a literal the arrow is one mistake, and gets one error that
/// names the statement spelling.
#[test]
fn a_for_arrow_outside_a_literal_is_one_error() {
    let found = errors("let e = for r in rows => r;");
    assert_eq!(found.len(), 1, "one error, not a cascade: {found:?}");
    assert!(found[0].contains("for x in xs { .. }"), "{found:?}");
}

#[test]
fn an_if_arrow_outside_a_literal_is_one_error() {
    let found = errors("g(if c => 1)");
    assert_eq!(found.len(), 1, "one error, not a cascade: {found:?}");
    assert!(found[0].contains("if c { x }"), "{found:?}");
}

#[test]
fn three_dots_is_one_error_naming_two() {
    let found = errors("[...xs]");
    assert_eq!(found.len(), 1, "one error, not a cascade: {found:?}");
    assert!(found[0].contains("write `..xs`"), "{found:?}");
}

/// The typed accessors read by role, not by position: with the condition
/// missing, the value is not mistaken for it.
#[test]
fn the_accessors_find_each_part_by_its_role() {
    use khora_syntax::ast::{AstNode, ListElement, ListExpr};
    let list_of = |body: &str| {
        let parsed = parse(&format!("module m;\nfn f() {{ {body} }}\n"));
        let node = parsed.syntax().descendants().find(|n| n.kind() == SyntaxKind::LIST_EXPR).unwrap();
        ListExpr::cast(node).unwrap()
    };
    fn text(n: &impl AstNode) -> String {
        n.syntax().text().to_string()
    }

    let list = list_of("[if c => a else b, for (x, y) in xs => x, ..ys, z]");
    assert!(list.has_element_forms());
    let parts: Vec<ListElement> = list.elements().collect();
    let ListElement::If(i) = &parts[0] else { panic!("{parts:?}") };
    assert_eq!(text(&i.condition().unwrap()), "c");
    assert_eq!(text(&i.then_element().unwrap()), "a");
    assert_eq!(text(&i.else_element().unwrap()), "b");
    let ListElement::For(f) = &parts[1] else { panic!("{parts:?}") };
    assert_eq!(text(&f.pattern().unwrap()), "(x, y)");
    assert_eq!(text(&f.iterable().unwrap()), "xs");
    assert_eq!(text(&f.body().unwrap()), "x");
    let ListElement::Spread(s) = &parts[2] else { panic!("{parts:?}") };
    assert_eq!(text(&s.list().unwrap()), "ys");
    assert!(matches!(&parts[3], ListElement::Expr(_)));
    assert!(!list_of("[a, if c { b }]").has_element_forms());

    // Broken: no condition, and no iterable.
    let list = list_of("[if => a, for x in => b]");
    let parts: Vec<ListElement> = list.elements().collect();
    let ListElement::If(i) = &parts[0] else { panic!("{parts:?}") };
    assert!(i.condition().is_none(), "the value was read as the condition");
    assert_eq!(text(&i.then_element().unwrap()), "a");
    let ListElement::For(f) = &parts[1] else { panic!("{parts:?}") };
    assert!(f.iterable().is_none(), "the value was read as the iterable");
    assert_eq!(text(&f.body().unwrap()), "b");
}

#[test]
fn a_form_with_no_value_says_so() {
    assert_eq!(errors("[if c =>]"), vec!["expected a list element".to_string()]);
    assert_eq!(errors("[..]"), vec!["expected the list to spread after `..`".to_string()]);
}

/// `..` does not begin an expression, so a spread outside `[..]` is refused.
#[test]
fn a_spread_outside_a_literal_is_refused() {
    assert!(!errors("g((..xs))").is_empty());
}
