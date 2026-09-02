//! `struct({ host: string(), port: int() })`, rewritten before it is typed.
//!
//! The spelling a reader reaches for cannot be a library function: its
//! argument is a record of *schemas* and its result a schema of the record
//! they decode, and there is no type-level map from one to the other. It can
//! be a call the compiler rewrites, because everything it rewrites into
//! already types and compiles -- `Schema::record` over `Fields::zip` and
//! `Fields::map`, with a lambda that takes the tuple apart and builds the
//! record literal. That literal then resolves against the declared record the
//! way every record literal does: from the type the expression is asked for,
//! or by its labels when nothing asked.
//!
//! # Static calls, and the order of evaluation
//!
//! The rewrite emits `Fields::map(chain, lambda)` and never a method chain,
//! because only a static call pushes the expected type into its return before
//! its arguments are checked; a method call infers its receiver first and the
//! hint is spent by then. Each field's schema is bound to a hidden local
//! before any of it, in source order, so a schema is evaluated once and the
//! problems it reports come out in declaration order.
//!
//! # Whose range a node carries
//!
//! Every synthesized node is blamed on the `struct(..)` call, because that is
//! the line the author wrote. The one exception is the mention of each field
//! inside the built literal, which carries the range of the author's schema
//! expression, so `port: string()` against a `port: Int` is reported at
//! `string()`.

use khora_syntax::ast::{self, AstNode};
use text_size::TextRange;

use super::{Ctx, Expr, ExprId, Pat, Stmt};

/// What to write instead, for every refusal that is about the argument.
const HOW: &str = "`struct` takes a record literal with one schema per field, such as \
                   `struct({ host: string(), port: int() })`";

impl<'a> Ctx<'a> {
    /// Whether a callee, as written, is `std::schema::struct`.
    ///
    /// Judged by where the name was imported from rather than by its
    /// spelling, so an alias still counts and a program's own function called
    /// `struct` does not; a local or an item of this file shadows the import,
    /// as it does everywhere else.
    pub(super) fn is_schema_struct(&self, callee: Option<&ast::Expr>) -> bool {
        let Some(ast::Expr::Path(path)) = callee else { return false };
        let segments: Vec<String> = path
            .syntax()
            .children()
            .find_map(ast::Path::cast)
            .map(|p| p.segments().filter_map(|s| s.ident()).collect())
            .unwrap_or_default();
        let [only] = segments.as_slice() else { return false };
        if self.lookup(only).is_some() || self.map.item(only).is_some() {
            return false;
        }
        self.scope.origin(only).is_some_and(|origin| {
            origin.name == "struct"
                && origin.kind == crate::ItemKind::Function
                && origin.module.segments() == ["std", "schema"]
        })
    }

    /// Refuses a use of `struct` that is not a call with a record literal.
    pub(super) fn refuse_struct(&mut self, message: &str, at: TextRange) -> ExprId {
        self.error(message.to_string(), at);
        self.add_expr(Expr::Missing, at)
    }

    /// `struct({ l0: e0, .. })` as the expression it stands for.
    pub(super) fn lower_struct_call(&mut self, call: &ast::CallExpr, range: TextRange) -> ExprId {
        let written: Vec<ast::Expr> =
            call.args().map(|list| list.args().collect()).unwrap_or_default();
        let [argument] = written.as_slice() else {
            return self.refuse_struct(HOW, range);
        };
        let ast::Expr::Record(record) = argument else {
            return self.refuse_struct(HOW, argument.syntax().text_range());
        };
        if record.base().is_some() {
            return self.refuse_struct(
                "`struct` cannot take fields from another record; write every field",
                argument.syntax().text_range(),
            );
        }
        let mut fields: Vec<(String, ast::Expr, TextRange)> = Vec::new();
        for field in record.fields() {
            let at = field.syntax().text_range();
            let (Some(label), Some(value)) = (field.name().and_then(|n| n.ident()), field.value())
            else {
                return self.refuse_struct(HOW, at);
            };
            if fields.iter().any(|(seen, _, _)| *seen == label) {
                return self.refuse_struct(&format!("`{label}` is given twice in this `struct`"), at);
            }
            fields.push((label, value, at));
        }
        if fields.is_empty() {
            return self.refuse_struct(
                "`struct({})` describes no fields",
                argument.syntax().text_range(),
            );
        }

        // The schemas first, each in a hidden local, in the order written.
        self.scopes.push(Vec::new());
        let mut stmts = Vec::with_capacity(fields.len());
        let mut value_ranges = Vec::with_capacity(fields.len());
        for (i, (_, value, _)) in fields.iter().enumerate() {
            let init = self.lower_expr(value);
            value_ranges.push(value.syntax().text_range());
            let local = self.declare(hidden(i), false, range);
            let pat = self.add_pat(Pat::Bind(local), range);
            stmts.push(Stmt::Let { pat, ty: None, init: Some(init) });
        }

        // Then the schema, as text: the same shape a derived schema has.
        let labels: Vec<&str> = fields.iter().map(|(label, _, _)| label.as_str()).collect();
        let text = record_text(&labels);
        let expr_mark = self.body.exprs.len();
        let pat_mark = self.body.pats.len();
        let local_mark = self.body.locals.len();
        let tail = self.lower_fragment(&text, range.start().into());
        debug_assert!(
            !matches!(self.body.exprs[tail.0 as usize], Expr::Missing),
            "the `struct` rewrite wrote Khora that does not parse: {text}"
        );

        // Blame the call for everything synthesized ...
        for at in &mut self.body.expr_ranges[expr_mark..] {
            *at = range;
        }
        for at in &mut self.body.pat_ranges[pat_mark..] {
            *at = range;
        }
        for local in &mut self.body.locals[local_mark..] {
            local.range = range;
        }
        // ... except the built literal's values, which are the author's
        // schemas as far as a message about them is concerned.
        let built: Vec<ExprId> = self.body.exprs[expr_mark..]
            .iter()
            .find_map(|expr| match expr {
                Expr::Record { owner: None, fields: written, base: None }
                    if written.len() == fields.len() =>
                {
                    Some(written.iter().map(|(_, value)| *value).collect())
                }
                _ => None,
            })
            .unwrap_or_default();
        for (value, at) in built.iter().zip(&value_ranges) {
            self.body.expr_ranges[value.0 as usize] = self.shifted(*at);
        }

        self.scopes.pop();
        self.add_expr(Expr::Block { stmts, tail: Some(tail) }, range)
    }
}

/// The local the `i`th schema is bound to. Two leading underscores are legal
/// and unwritten: the lint that refuses them in source does not run over
/// what the compiler wrote.
fn hidden(i: usize) -> String {
    format!("__struct_{i}")
}

/// `Schema::record(Fields::map(chain, fn t => { let (a0, a1) = t; { l0: a0, l1: a1 } }))`.
fn record_text(labels: &[&str]) -> String {
    let chain = zip_chain(
        labels
            .iter()
            .enumerate()
            .map(|(i, label)| format!("Fields::of(\"{label}\", {})", hidden(i))),
    );
    let literal = labels
        .iter()
        .enumerate()
        .map(|(i, label)| format!("{label}: a{i}"))
        .collect::<Vec<_>>()
        .join(", ");
    let assemble = match labels.len() {
        1 => format!("fn a0 => {{ {literal} }}"),
        n => format!("fn t => {{ let {} = t; {{ {literal} }} }}", tuple_pattern(n)),
    };
    format!("Schema::record(Fields::map({chain}, {assemble}))")
}

/// `Fields::zip(a, Fields::zip(b, c))`, nested to the right so that
/// `tuple_pattern` takes it apart. One field is itself.
fn zip_chain(items: impl IntoIterator<Item = String>) -> String {
    let items: Vec<String> = items.into_iter().collect();
    let Some((last, front)) = items.split_last() else {
        return "Fields::none()".to_string();
    };
    front
        .iter()
        .rev()
        .fold(last.clone(), |rest, item| format!("Fields::zip({item}, {rest})"))
}

/// `(a0, (a1, a2))` — the pattern that takes a `zip_chain` of `n` apart.
fn tuple_pattern(n: usize) -> String {
    (0..n.saturating_sub(1))
        .rev()
        .fold(format!("a{}", n.saturating_sub(1)), |rest, i| format!("(a{i}, {rest})"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_field_needs_no_tuple() {
        assert_eq!(
            record_text(&["host"]),
            "Schema::record(Fields::map(Fields::of(\"host\", __struct_0), fn a0 => { host: a0 }))"
        );
    }

    #[test]
    fn three_fields_nest_to_the_right() {
        assert_eq!(
            record_text(&["a", "b", "c"]),
            "Schema::record(Fields::map(Fields::zip(Fields::of(\"a\", __struct_0), \
             Fields::zip(Fields::of(\"b\", __struct_1), Fields::of(\"c\", __struct_2))), \
             fn t => { let (a0, (a1, a2)) = t; { a: a0, b: a1, c: a2 } }))"
        );
    }

    #[test]
    fn the_text_parses() {
        for n in 1..=6 {
            let labels: Vec<String> = (0..n).map(|i| format!("f{i}")).collect();
            let labels: Vec<&str> = labels.iter().map(String::as_str).collect();
            let wrapped = format!("module i;\nconst i = {};\n", record_text(&labels));
            let parsed = khora_syntax::parse(&wrapped);
            assert!(parsed.errors().is_empty(), "{n} fields: {:?}\n{wrapped}", parsed.errors());
        }
    }
}
