//! Forms that are written one way and mean another.
//!
//! `for` becomes a `loop` over `Step`; `[a, b, c]` becomes a `List::Cons`
//! chain; `"a ${e} b"` becomes `"a " + e + " b"`. Each is here rather than in
//! the checker or the backend because a desugaring that happens once, early,
//! is a construct nothing downstream has to know about — which is why neither
//! inference nor code generation has a case for a list literal.
//!
//! All three resolve the names they expand into through the ordinary scope, so
//! `for` needs `Step` imported and `[..]` needs `List`. The alternative is a
//! name the compiler knows and the program cannot see, which is what errata 46
//! is about.
//!
//! **`for` needs `Iterator` too**, and used to say only `Step`. The expansion
//! calls `it.next()`, which is a trait method and so needs its trait in scope
//! like any other; importing `Step` alone left the loop's type unsolved and
//! produced the compiler's own "this is a gap in the compiler" message in
//! front of somebody writing their first loop. Both names are in the message
//! now, and `unused-import` knows a `for` uses them -- it reported `Iterator`
//! as unused, which told the reader to delete what made the program work.
//! Errata 58.

use super::*;

/// Said by the desugaring when it reports, and carried by the `Resolution` for
/// the backend to say if it ever gets one. Named once so the two cannot drift.
const STEP_IS_MISSING: &str = "`for` needs `Step` and `Iterator` in scope; import them from \
                               `std::core`";

impl<'a> Ctx<'a> {
    /// `for pat in iter { body }`, desugared here rather than carried further.
    ///
    /// ```text
    /// {
    ///   let mut it = <iter>;
    ///   loop {
    ///     match it.next() {
    ///       Step::Yield(rest, pat) => { it = rest; <body> }
    ///       Step::Done => break,
    ///     }
    ///   }
    /// }
    /// ```
    ///
    /// Desugaring in the front end means the checker, the reference-counting
    /// plan and the backend need no notion of `for` at all — it is `loop`,
    /// `match` and assignment, each of which already works and is already
    /// tested. The cost is that the loop depends on `Step` being in scope, the
    /// same way Rust's `for` depends on `IntoIterator`.
    pub(super) fn lower_for(&mut self, e: &ast::ForExpr, range: TextRange) -> ExprId {
        let iter = match e.iterable() {
            Some(i) => self.lower_expr(&i),
            None => self.add_expr(Expr::Missing, range),
        };

        // The whole loop lives in a scope of its own so the state variable
        // cannot collide with anything the body declares.
        self.scopes.push(Vec::new());
        let state = self.declare("$iter".to_string(), true, range);
        let state_pat = self.add_pat(Pat::Bind(state), range);

        let rest = self.declare("$rest".to_string(), false, range);
        let rest_pat = self.add_pat(Pat::Bind(rest), range);

        // The arm binds its own scope: the item pattern belongs to the body.
        self.scopes.push(Vec::new());
        let item_pat = match e.pattern() {
            Some(p) => self.lower_pat(&p, false),
            None => self.add_pat(Pat::Wildcard, range),
        };

        let advance = {
            let target = self.add_expr(Expr::Local(state), range);
            let value = self.add_expr(Expr::Local(rest), range);
            self.add_expr(Expr::Assign { target, value }, range)
        };
        // Inside the loop before the body is lowered, or a `break` in it would
        // be reported as outside a loop.
        self.loop_depth += 1;
        let body = match e.body() {
            Some(b) => self.lower_block(&b),
            None => self.add_expr(Expr::Missing, range),
        };
        self.loop_depth -= 1;
        self.scopes.pop();

        let arm_body = self.add_expr(
            Expr::Block { stmts: vec![Stmt::Expr(advance)], tail: Some(body) },
            range,
        );

        // Both at once, and reported once: `Yield` and `Done` missing is one
        // absence, and hoisting the calls out of `add_pat` is what lets
        // `step_cases` take `&mut self` and say so.
        let (yield_case, done_case) = self.step_cases(range);
        let yield_pat = self.add_pat(
            Pat::TupleStruct { resolution: yield_case, fields: vec![rest_pat, item_pat] },
            range,
        );
        let done_pat = self.add_pat(Pat::Path(done_case), range);
        let stop = self.add_expr(Expr::Break(None), range);

        let scrutinee = {
            let receiver = self.add_expr(Expr::Local(state), range);
            let next = self.add_expr(
                Expr::Field { base: receiver, name: "next".to_string() },
                range,
            );
            self.add_call(Expr::Call { callee: next, args: Vec::new() }, range)
        };
        let dispatch = self.add_expr(
            Expr::Match {
                scrutinee,
                arms: vec![
                    MatchArm { pat: yield_pat, guard: None, body: arm_body },
                    MatchArm { pat: done_pat, guard: None, body: stop },
                ],
            },
            range,
        );

        let repeat = self.add_expr(Expr::Loop { body: dispatch }, range);
        self.scopes.pop();

        self.add_expr(
            Expr::Block {
                stmts: vec![Stmt::Let { pat: state_pat, ty: None, init: Some(iter) }],
                tail: Some(repeat),
            },
            range,
        )
    }

    /// `"a ${e} b"` is `"a " + e + " b"`.
    ///
    /// **The parts are joined with the operator somebody would have written.**
    /// That is the whole of the feature: `+` on strings already exists, already
    /// checks that both sides are `String`, and already says
    /// *"string concatenation: expected `String`, found `Int`"* when they are
    /// not — which is the right diagnostic for `"${count}"`, pointing at the
    /// expression rather than at the string.
    ///
    /// Nothing was added to the lexer or the grammar. A string literal is still
    /// one token; the interpolations are found in its text here, and each is
    /// parsed as a source file of its own. That keeps the change to one pass at
    /// the cost of `range_shift`, which is what puts a diagnostic about an
    /// interpolated expression back where the text is.
    ///
    /// `\$` is a literal dollar, so a template for some other tool still fits
    /// in a Khora string.
    pub(super) fn lower_interpolation(&mut self, text: &str, range: TextRange) -> ExprId {
        // Offsets are into the file, so the opening quote is where the literal
        // starts and the body is one byte past it.
        let body_at = u32::from(range.start()) + 1;
        let body = strip_quotes(text);
        let parts = split_interpolation(body);

        // **A backtick literal's indentation is the source's, not the
        // string's**, and it comes off here rather than in `strip_quotes`
        // because these spans are offsets into the file: dedenting the body
        // first would move every hole's position and point each diagnostic at
        // the wrong column. Measured over the whole body so relative
        // indentation survives, then taken off each text piece as it is
        // lowered. The span stays as written, which covers slightly more than
        // the value -- harmless, and better than a span that is wrong.
        let common = if text.starts_with('`') { Some(common_indent(body)) } else { None };

        // The opening and closing blank lines belong to the *body*, not to any
        // one piece, so they are trimmed off whichever text piece happens to
        // carry them -- the first and the last. A literal that opens with a
        // hole has no first text piece and nothing to trim, which is correct:
        // there was no blank line to remove.
        let first_text = parts.iter().position(|p| matches!(p, Part::Text(_)));
        let last_text = parts.iter().rposition(|p| matches!(p, Part::Text(_)));

        let mut joined: Option<ExprId> = None;
        for (index, part) in parts.into_iter().enumerate() {
            let piece = match part {
                Part::Text(raw) => {
                    let at = body_at + raw.at;
                    let width = raw.text.len() as u32;
                    let span = TextRange::at(TextSize::from(at), TextSize::from(width));
                    let text = match common {
                        Some(indent) => {
                            let mut body = raw.text.as_str();
                            if first_text == Some(index) {
                                body = trim_open(body);
                            }
                            if last_text == Some(index) {
                                body = trim_close(body);
                            }
                            strip_indent(body, indent)
                        }
                        None => raw.text.clone(),
                    };
                    self.add_expr(Expr::Literal(Literal::Str(unescape_body(&text))), span)
                }
                Part::Hole(raw) => {
                    // Shown rather than concatenated raw: a hole holds a value
                    // and a message wants its text. See `Expr::Shown`.
                    let value = self.lower_fragment(&raw.text, body_at + raw.at);
                    let at = body_at + raw.at;
                    let span =
                        TextRange::at(TextSize::from(at), TextSize::from(raw.text.len() as u32));
                    self.add_expr(Expr::Shown(value), span)
                }
            };
            joined = Some(match joined {
                None => piece,
                Some(left) => self.add_expr(
                    Expr::Binary { op: BinOp::Add, lhs: left, rhs: piece },
                    range,
                ),
            });
        }

        // `"${}"` has no parts at all, and is the empty string.
        joined.unwrap_or_else(|| {
            self.add_expr(Expr::Literal(Literal::Str(String::new())), range)
        })
    }

    /// Parses one `${..}` and lowers it into this body.
    ///
    /// The wrapper is a whole source file because that is what the parser
    /// takes, and a `const` because its initializer is one accessor away —
    /// digging a tail expression out of a function body would be more code for
    /// the same result.
    pub(super) fn lower_fragment(&mut self, source: &str, at: u32) -> ExprId {
        const PREFIX: &str = "module i;\nconst i = ";
        let wrapped = format!("{PREFIX}{source};\n");
        let parsed = khora_syntax::parse(&wrapped);

        let found = parsed
            .source_file()
            .decls()
            .find_map(|item| match item {
                ast::Decl::Const(c) => c.initializer(),
                _ => None,
            })
            .filter(|_| parsed.errors().is_empty());

        let Some(expr) = found else {
            let width = source.len().max(1) as u32;
            let span = TextRange::at(TextSize::from(at), TextSize::from(width));
            self.error("this `${..}` does not contain an expression", span);
            return self.add_expr(Expr::Missing, span);
        };

        // The fragment's ranges are measured from the start of `wrapped`, and
        // the expression begins right after the prefix — so moving them by
        // `at - PREFIX.len()` puts them exactly where the source text is.
        let outer = self.range_shift;
        self.range_shift = at.wrapping_sub(PREFIX.len() as u32);
        let lowered = self.lower_expr(&expr);
        self.range_shift = outer;
        lowered
    }

    /// `[a, b, c]` is `List::Cons(a, List::Cons(b, List::Cons(c, List::Nil)))`.
    ///
    /// **D13.** The literal parsed and meant nothing: the checker gave it no
    /// type and the backend refused it, so every use was either an inscrutable
    /// "type was never worked out" or a hard error at the end of the pipeline.
    /// It denotes a `List` — the one sequence type that is immutable, that a
    /// `match` can take apart, and that may cross into a fiber. `Array` and
    /// `Vector` are buffers you write into, and a literal quietly producing one
    /// would be three surprises from one pair of brackets.
    ///
    /// **Desugared here rather than typed as itself**, which is why neither the
    /// checker nor the code generator has a case for a list literal: by the
    /// time either sees one it is constructor calls, and everything that
    /// already works for those — inference, monomorphization, reference
    /// counting, reuse — works for this without being told. It is also
    /// literally what `derive(ToJson)` was already emitting by hand.
    ///
    /// Every node carries the literal's own range, so a diagnostic points at
    /// the brackets somebody wrote rather than at a `Cons` nobody did.
    ///
    /// `List` has to be in scope, exactly as `Step` does for a `for` loop, and
    /// for the same reason: the alternative is a name the compiler knows and
    /// the program cannot see.
    pub(super) fn lower_list(&mut self, items: Vec<ExprId>, range: TextRange) -> ExprId {
        // **Said here, rather than carried onward as an unsupported
        // resolution.** The checker turns one of those into `Type::Unknown` and
        // then reports "the type of this expression was never worked out",
        // which is the very diagnostic D13 existed to be rid of. A `Missing` is
        // compatible with anything, so this reports once and nothing cascades.
        let (Some(nil), Some(cons)) = (self.list_case("Nil"), self.list_case("Cons")) else {
            self.error("`[a, b, c]` builds a `List`; import it from `std::core`", range);
            return self.add_expr(Expr::Missing, range);
        };

        let mut chain = self.add_expr(Expr::Path(nil), range);
        for item in items.into_iter().rev() {
            let callee = self.add_expr(Expr::Path(cons.clone()), range);
            chain = self.add_expr(Expr::Call { callee, args: vec![item, chain] }, range);
        }
        chain
    }

    /// `0.01d` is `Decimal::scaled(1, 2)`.
    ///
    /// **The language's only literal suffix**, and `docs/design/numbers.md`
    /// says why it earns the exception: without it there is no way to write an
    /// exact decimal *constant*. `Decimal::of("0.01")` parses at run time,
    /// costs something at every evaluation, and returns a `Result` because a
    /// string might not be a number; going through a `Float` throws away the
    /// exactness the type exists for.
    ///
    /// Desugared here for the reason the list literal is: by the time anything
    /// downstream sees one it is an ordinary call, so inference,
    /// monomorphization, reference counting and reuse all work on it without
    /// being told a literal existed.
    ///
    /// **The scale is the number of digits written**, not the value's
    /// magnitude, so `1.50d` is `scaled(150, 2)` and keeps its trailing zero —
    /// a price to two places stays a price to two places, which is the same
    /// reasoning `Show for Decimal` gives. An exponent shifts the point:
    /// `1.5e3d` is `1500` at scale zero rather than `15` at scale minus two,
    /// because a negative scale is a large number spelled confusingly and
    /// `Decimal::scaled` refuses one.
    ///
    /// `Decimal` has to be in scope, exactly as `List` does for `[a, b]`.
    pub(super) fn lower_decimal(&mut self, text: &str, range: TextRange) -> ExprId {
        let Some((units, scale)) = decimal_parts(text) else {
            self.error(
                format!("`{text}` is not a decimal this compiler can read"),
                range,
            );
            return self.add_expr(Expr::Missing, range);
        };

        let Some(scaled) = self.decimal_scaled() else {
            self.error(
                "a `0.01d` literal builds a `Decimal`; import it from `std::decimal`"
                    .to_string(),
                range,
            );
            return self.add_expr(Expr::Missing, range);
        };

        let callee = self.add_expr(Expr::Path(scaled), range);
        let units = self.add_expr(Expr::Literal(Literal::Int(units.to_string())), range);
        let scale = self.add_expr(Expr::Literal(Literal::Int(scale.to_string())), range);
        self.add_expr(Expr::Call { callee, args: vec![units, scale] }, range)
    }

    /// `Decimal::scaled`, if a `Decimal` is in scope.
    ///
    /// Looked for as a type rather than as a function: the constructor is an
    /// inherent method, and what a program imports is the type it hangs off.
    fn decimal_scaled(&self) -> Option<crate::Resolution> {
        // By *name in scope*, not through `variants_of`: a `Decimal` is a
        // record, and an `ItemMap` records constructors only for variant
        // types — so the check that works for `List` finds nothing here.
        let declared = self.map.item("Decimal").is_some();
        let imported = self.scope.origins.iter().any(|o| o.local == "Decimal");
        if !declared && !imported {
            return None;
        }
        Some(crate::Resolution::TraitItem {
            owner: "Decimal".to_string(),
            name: "scaled".to_string(),
        })
    }

    /// `List::Cons` or `List::Nil`, if a `List` declaring them is in scope.
    pub(super) fn list_case(&self, case: &str) -> Option<crate::Resolution> {
        let found = self
            .map
            .variants_of("List")
            .chain(self.scope.variants_of("List"))
            .find(|v| v.name == case)?;
        Some(crate::Resolution::Variant {
            module: self.home_of_type("List"),
            type_name: found.type_name.clone(),
            name: found.name.clone(),
        })
    }

    /// `Step::Yield` and `Step::Done`, as the desugaring needs them, reporting
    /// once if `Step` is not in scope.
    ///
    /// **The message existed and nobody printed it.** `Resolution::Unsupported`
    /// carries the text, and the only thing that reads it is the backend — so
    /// `khora check` on a `for` loop with no `Step` imported said "`Int` has no
    /// method `next`", pointing at a method call the desugaring wrote and the
    /// programmer did not. That is exactly the "unresolved-name error pointing
    /// at code nobody wrote" the message was written to replace, and it was the
    /// error anybody actually saw.
    ///
    /// Reported here, beside the knowledge, the way `resolve_constructor` in
    /// `patterns.rs` already does. Once for the pair, because one missing
    /// `Step` is one mistake.
    pub(super) fn step_cases(
        &mut self,
        range: TextRange,
    ) -> (crate::Resolution, crate::Resolution) {
        let yield_case = self.step_case("Yield");
        let done_case = self.step_case("Done");
        // **`Iterator` is as necessary as `Step` and was never checked for.**
        // The expansion calls `it.next()`, a trait method, so the trait has to
        // be in scope; without it the loop's type simply never solved and what
        // the reader saw was the checker's own "this is a gap in the compiler"
        // message. Reported here, where the requirement is, rather than left
        // to be inferred from a failure three layers away. Errata 58.
        if matches!(yield_case, crate::Resolution::Unsupported(_))
            || matches!(done_case, crate::Resolution::Unsupported(_))
            || !self.has_iterator()
        {
            self.error(STEP_IS_MISSING, range);
        }
        (yield_case, done_case)
    }

    /// Whether `Iterator` is reachable by name.
    ///
    /// Declared here or imported, the same two places `Step` is looked for.
    /// Only the name matters: the trait's *methods* are found by the checker
    /// once the trait is in scope, and a file that has shadowed `Iterator`
    /// with something else has a different problem than this message.
    fn has_iterator(&self) -> bool {
        self.map.item("Iterator").is_some() || self.scope.get("Iterator").is_some()
    }

    /// One case of `Step`, resolved and not reported. See [`Self::step_cases`].
    fn step_case(&self, case: &str) -> crate::Resolution {
        // Through the scope as well as the file: `Step` almost always arrives
        // by `import std::core::{Step}` rather than being declared next to the
        // loop that uses it.
        match self
            .map
            .variants_of("Step")
            .chain(self.scope.variants_of("Step"))
            .find(|v| v.name == case)
        {
            Some(v) => crate::Resolution::Variant {
                module: self.home_of_type("Step"),
                type_name: v.type_name.clone(),
                name: v.name.clone(),
            },
            // `for` needs `Step` the way Rust's needs `IntoIterator`.
            None => crate::Resolution::Unsupported(STEP_IS_MISSING),
        }
    }
}

/// The significand and the scale a decimal literal denotes.
///
/// `1d` is `(1, 0)`, `0.01d` is `(1, 2)`, `1.50d` is `(150, 2)` — the trailing
/// zero is written down, so it is part of what was meant. `1.5e3d` is
/// `(1500, 0)`: the exponent moves the point rather than becoming a negative
/// scale, because `Decimal::scaled` has none and a negative one is a large
/// number spelled confusingly.
///
/// Underscores are separators, as they are in every other numeral here.
fn decimal_parts(text: &str) -> Option<(i128, u32)> {
    let body = text.strip_suffix('d')?.replace('_', "");

    let (mantissa, exponent) = match body.find(['e', 'E']) {
        Some(at) => {
            let power: i32 = body[at + 1..].parse().ok()?;
            (body[..at].to_string(), power)
        }
        None => (body, 0),
    };

    let (digits, written) = match mantissa.find('.') {
        Some(at) => {
            let whole = &mantissa[..at];
            let fraction = &mantissa[at + 1..];
            (format!("{whole}{fraction}"), fraction.len() as i32)
        }
        None => (mantissa, 0),
    };

    let units: i128 = digits.parse().ok()?;
    let scale = written - exponent;

    // A negative scale is a whole number with zeros on the end, which is what
    // multiplying by ten to the power says. `1.5e3d` is `1500`, not `15` at a
    // scale nothing can represent.
    if scale < 0 {
        let mut value = units;
        for _ in 0..(-scale) {
            value = value.checked_mul(10)?;
        }
        return Some((value, 0));
    }
    Some((units, scale as u32))
}
