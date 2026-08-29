//! Inferring an expression's type, one form at a time.
//!
//! `infer` memoises and `infer_uncached` does the work, so every arm below can
//! recurse freely without the cost showing up quadratically. What is *not* here
//! is calls — they are big enough and different enough to be `calls`, and rows
//! travel with them.

use super::*;

impl<'a> Checker<'a> {
    pub(super) fn infer(&mut self, id: ExprId) -> Type {
        let ty = self.infer_uncached(id);
        self.exprs.insert(id, ty.clone());
        ty
    }

    pub(super) fn infer_uncached(&mut self, id: ExprId) -> Type {
        let range = self.body.range(id);
        let hint = self.hint.take();
        match self.body.expr(id).clone() {
            Expr::Missing | Expr::Unresolved(_) => Type::Unknown,
            Expr::Unit => Type::Unit,
            Expr::Literal(lit) => match lit {
                // An integer literal is an `Int` unless something is asking
                // for a narrower one, in which case it is that — and has to
                // fit it. There is no widening anywhere else in the language,
                // so this is the only way to write a `U8` that is not a
                // conversion, and without it every byte in a table would be
                // `Int::to_u8(65)`.
                Literal::Int(text) => match hint {
                    Some(Type::Fixed(kind)) => {
                        self.check_literal_fits(&text, kind, range);
                        Type::Fixed(kind)
                    }
                    _ => Type::Int,
                },
                Literal::Float(_) => Type::Float,
                Literal::Str(_) => Type::Str,
                Literal::Bool(_) => Type::Bool,
            },
            Expr::Local(local) => self.locals.get(&local).cloned().unwrap_or(Type::Unknown),
            Expr::Path(resolution) => self.type_of_resolution(id, &resolution),
            Expr::Field { base, name } => {
                let owner = self.infer(base);
                let owner = self.unifier.shallow(&owner);
                let Some((_, field)) = self.record_field(&owner, &name) else {
                    // Silent for a type that is not known yet: `Unknown` is
                    // downstream of an error already reported.
                    if !matches!(owner, Type::Unknown | Type::Var(_) | Type::Never) {
                        self.error(self.why_no_field(&owner, &name), range);
                    }
                    return Type::Unknown;
                };
                field
            }
            Expr::Unary { op, operand } => match op {
                UnOp::Neg => self.infer_negation(operand, hint, range),
                UnOp::Not => self.expect(operand, &Type::Bool, "`!`"),
            },
            Expr::Binary { op, lhs, rhs } => self.infer_binary(id, op, lhs, rhs, hint),
            Expr::Assign { target, value } => {
                let target_ty = self.infer(target);
                self.check_writable(target, range);
                self.expect(value, &target_ty, "this assignment");
                Type::Unit
            }
            Expr::Call { callee, args } => self.infer_call(callee, &args, hint, range),
            Expr::Block { stmts, tail } => {
                // A `with` block lowered to an ordinary one, and its labels
                // are supplied to everything inside it.
                let supplied = self.body.installs.get(&id).cloned().unwrap_or_default();
                let depth = self.installed.len();
                self.installed.extend(supplied);
                self.hint = hint;
                let ty = self.infer_block(&stmts, tail);
                self.installed.truncate(depth);
                ty
            }
            Expr::If { condition, then_branch, else_branch } => {
                self.expect(condition, &Type::Bool, "an `if` condition");
                self.hint = hint.clone();
                let then_ty = self.infer(then_branch);
                match else_branch {
                    Some(else_id) => {
                        self.hint = hint;
                        let else_ty = self.infer(else_id);
                        if !self.require(&then_ty, &else_ty, "`if` branches disagree", range) {
                            return Type::Unknown;
                        }
                        if matches!(then_ty, Type::Never) { else_ty } else { then_ty }
                    }
                    // Without an `else`, the branch is only well typed if it
                    // produces nothing — the same rule `match` follows.
                    None => {
                        self.require(
                            &Type::Unit,
                            &then_ty,
                            "an `if` without `else` must produce `()`",
                            range,
                        );
                        Type::Unit
                    }
                }
            }
            Expr::While { condition, body } => {
                self.expect(condition, &Type::Bool, "a `while` condition");
                self.infer(body);
                Type::Unit
            }
            Expr::Loop { body } => {
                // A `loop` yields whatever its `break`s carry. Left as
                // `Unknown` through phase 2 rather than guessed — which was
                // fine until `Unknown` stopped being allowed to mean "not
                // worked out", because `Unknown` unifies with everything and so
                // `let n: Bool = loop { break 1 };` was accepted.
                let answer = self.unifier.fresh();
                self.loops.push((answer.clone(), false));
                self.infer(body);
                let (answer, carried) = self.loops.pop().expect("just pushed");
                // Nothing broke with a value, so there is no value: an infinite
                // loop and a loop that just stops both produce `()`.
                if carried { answer } else { Type::Unit }
            }
            Expr::Break(value) => {
                if let Some(v) = value {
                    let carried = self.infer(v);
                    // A `break` outside a loop is reported by HIR lowering,
                    // which knows the nesting; nothing to add here.
                    if let Some((answer, _)) = self.loops.last().cloned() {
                        self.require(&answer, &carried, "`break` values disagree", range);
                        if let Some(last) = self.loops.last_mut() {
                            last.1 = true;
                        }
                    }
                }
                Type::Never
            }
            Expr::Continue => Type::Never,
            // **A `${..}` hole, shown.** The desugaring wrote this; nobody
            // spells it. `Show` is resolved against the value's type rather
            // than through the names in scope, because interpolating something
            // should not require importing the trait that prints it -- see
            // `khora_hir::body::Expr::Shown`.
            Expr::Shown(value) => self.shown(id, value, range),
            Expr::Return(value) => {
                let expected = self.signature.ret.clone();
                match value {
                    Some(v) => {
                        self.expect(v, &expected, "this `return`");
                    }
                    None => {
                        if self.unifier.unify(&expected, &Type::Unit).is_err() {
                            self.error(
                                format!("this function returns `{expected}`, so `return` needs a value"),
                                range,
                            );
                        }
                    }
                }
                Type::Never
            }
            Expr::Tuple(items) => {
                let types: Vec<Type> = items.iter().map(|i| self.infer(*i)).collect();
                if types.is_empty() { Type::Unit } else { Type::Tuple(types) }
            }
            Expr::Match { scrutinee, arms } => {
                self.hint = hint;
                self.infer_match(scrutinee, &arms, range)
            }
            Expr::Record { owner, fields, base } => {
                self.infer_record(owner, &fields, base, range)
            }

            // `raise e` leaves the function, so it stands wherever an
            // expression can and its type constrains nothing.
            Expr::Raise(error) => {
                let ty = self.infer(error);
                let ty = self.unifier.zonk(&ty);
                if !matches!(ty, Type::Unknown | Type::Var(_)) {
                    self.demanded.push(Demand {
                        // A `raise` is the mark: there is no call to write `!`
                        // on, and `site: None` already says the check does not
                        // apply here.
                        fallible: false,
                        clause: Clause::Raises,
                        row: Type::row(vec![(label_of(&ty), ty)], None),
                        range,
                        callee: "raise".to_string(),
                        site: None,
                    });
                }
                Type::Never
            }

            // `f()!` is the identity on types. What it does is mark, and the
            // mark is what excuses the call from needing one.
            Expr::Try(inner) => {
                // A demand is recorded against the *callee*, since that is
                // what carries the signature, so the mark has to reach it too:
                // `f()!` marks the call and the `f` inside it.
                self.marked.push(inner);
                if let Expr::Call { callee, .. } = self.body.expr(inner) {
                    self.marked.push(*callee);
                }
                self.infer(inner)
            }
            Expr::Catch { inner, arms } => self.infer_catch(inner, &arms, range),
            Expr::Lambda { evidence, params, param_types, body, .. } => {
                // A parameter with no annotation gets a variable, so its type
                // is settled by how the lambda is used: `map(xs, (x) => x + 1)`
                // learns `x: Int` from `map`'s signature.
                //
                // **And one with an annotation gets the annotation**, which
                // reads as too obvious to write down and is not: a lambda in a
                // `let`, with no call yet to hint from, has its annotation as
                // the only source of truth, and dropping it in lowering means
                // `fn (s: String) => s + "b"` is checked as though `String` had
                // never been said.
                let types: Vec<Type> = params
                    .iter()
                    .enumerate()
                    .map(|(i, _)| match param_types.get(i).and_then(|t| t.as_ref()) {
                        Some(written) => {
                            type_of_ref(written, &self.signature.generics, &self.types.homes)
                        }
                        None => self.unifier.fresh(),
                    })
                    .collect();
                let result = self.unifier.fresh();

                // **Solved from the expected type before the patterns are
                // bound, and long before the body is inferred.** `expect`
                // already knows what this argument has to be; using it only in
                // the `require` at the end is too late for anything inside.
                //
                // Without it, a `match` in the body destructures a parameter
                // whose type is still a variable, `bind_pattern` cannot take
                // that apart, and the binding and every field read off it get
                // `Unknown` — silently, because an unsolved owner is exactly
                // the case a field read declines to complain about. The failure
                // surfaces in the `Unknown` audit at the end, blaming a line
                // with nothing wrong with it.
                //
                // Silent unification, for the reason `hint_at` is: a hint that
                // does not fit is not itself the error, and the `require` below
                // reports it against the right range.
                if let Some(Type::Fn { params: wanted, ret: wanted_ret, .. }) =
                    hint.as_ref().map(|h| self.unifier.shallow(h))
                {
                    for (mine, theirs) in types.iter().zip(&wanted) {
                        let _ = self.unifier.unify(mine, theirs);
                    }
                    let _ = self.unifier.unify(&result, &wanted_ret);
                }

                for (pat, ty) in params.iter().zip(&types) {
                    self.bind_pattern(*pat, ty);
                }

                // **A capability the body named and could not resolve.**
                //
                // Lowering bound it rather than reporting it, because only
                // this point knows what the lambda is *expected* to require:
                // `bounded_nursery`'s parameter says `with { 'ef | nursery:
                // Nursery }`, so a body that writes `nursery.adopt(f)` is
                // naming that label and nothing else could have been meant.
                //
                // A label the expected row does not name is the typo lowering
                // deferred, and gets the message lowering would have given.
                // Nothing is lost by having moved it; `capability-passing.md`
                // §"The limit" declined this on the grounds that the message
                // would disappear, and it does not.
                let expected_row = hint
                    .as_ref()
                    .map(|h| self.unifier.shallow(h))
                    .and_then(|h| match h {
                        Type::Fn { requires, .. } => Some(self.unifier.zonk(&requires)),
                        _ => None,
                    });
                let offered: Vec<(String, Type)> = match &expected_row {
                    Some(Type::Row { fields, .. }) => fields.clone(),
                    _ => Vec::new(),
                };
                let mut required: Vec<(String, Type)> = Vec::new();
                for (label, pat) in &evidence {
                    match offered.iter().find(|(l, _)| l == label) {
                        Some((_, ty)) => {
                            self.bind_pattern(*pat, ty);
                            if !required.iter().any(|(l, _)| l == label) {
                                required.push((label.clone(), ty.clone()));
                            }
                        }
                        None => {
                            self.error(
                                format!("cannot find `{label}` in this scope"),
                                self.body.pat_range(*pat),
                            );
                            self.bind_pattern(*pat, &Type::Unknown);
                        }
                    }
                }

                // The whole type exists before the body is checked, because a
                // recursive closure mentions itself inside it. The result is a
                // variable the body then solves, and so is the error row.
                let raises = self.unifier.fresh();
                let requires = self.unifier.fresh();
                let whole = Type::Fn {
                    params: types,
                    ret: Box::new(result.clone()),
                    // A variable, solved below by what the body turned out to
                    // need and could not reach. Usually that is nothing: a
                    // capability in scope is *captured*, because a `with` block
                    // is a block of `let`s and a closure captures the bindings
                    // it reads. What is left over is a capability that does not
                    // exist yet where the lambda is written, which is the whole
                    // of `docs/design/capability-passing.md`.
                    requires: Box::new(requires.clone()),
                    raises: Box::new(raises.clone()),
                };
                self.lambdas.push(whole.clone());
                self.enclosing_lambdas.push((id, Vec::new()));

                let before = self.demanded.len();
                let ret = self.infer(body);
                let needs = self.absorb_requires(before);
                // The labels the body *named* go in alongside the ones its
                // calls asked for. Sorted with them, because code generation
                // appends a parameter per label in this order and only one
                // side should be deciding it.
                let needs = match needs {
                    Type::Row { mut fields, tail } => {
                        for (label, ty) in required {
                            if !fields.iter().any(|(l, _)| *l == label) {
                                fields.push((label, ty));
                            }
                        }
                        Type::row(fields, tail.map(|t| *t))
                    }
                    other => other,
                };
                let _ = self.unifier.unify(&requires, &needs);
                let mine = self.absorb_raises(before);
                // Left open, because what the body raises is a lower bound
                // rather than the answer — see `open_raises`. A closed row here
                // is what made a mock that cannot fail unusable as an operation
                // declared to fail.
                let mine = match mine {
                    Type::Row { fields, tail: None } => {
                        let rest = self.unifier.fresh();
                        self.open_raises.push(rest.clone());
                        Type::row(fields, Some(rest))
                    }
                    already_open => already_open,
                };
                let _ = self.unifier.unify(&raises, &mine);

                if let Some((_, found)) = self.enclosing_lambdas.pop() {
                    self.lambda_captures.insert(id, found);
                }
                self.lambdas.pop();

                self.require(&result, &ret, "this closure's body", range);
                whole
            }
            // Inside its own body, a closure's name is the closure.
            Expr::LambdaSelf => {
                self.lambdas.last().cloned().unwrap_or(Type::Unknown)
            }
        }
    }

    /// `-x`, over any number.
    ///
    /// The type is whatever is being negated rather than always `Int`, so
    /// `-1.5` is a `Float` and `-b` on a `U8` is refused for the better reason.
    ///
    /// And a negated *literal* is one number rather than a negation applied to
    /// another: `-128` is an `I8` even though `128` is not, and there is no
    /// other way to write that type's smallest value.
    pub(super) fn infer_negation(&mut self, operand: ExprId, hint: Option<Type>, range: TextRange) -> Type {
        if let (Some(Type::Fixed(kind)), Expr::Literal(Literal::Int(text))) =
            (&hint, self.body.expr(operand).clone())
        {
            let kind = *kind;
            self.check_literal_fits(&format!("-{text}"), kind, range);
            let ty = Type::Fixed(kind);
            self.exprs.insert(operand, ty.clone());
            return ty;
        }

        self.hint = hint;
        let inner = self.infer(operand);
        let inner = self.unifier.zonk(&inner);
        // An unsolved variable becomes `Int`, which is what a bare `-x` in a
        // generic position has always meant.
        let expected = match inner {
            Type::Float => Type::Float,
            Type::Fixed(kind) => Type::Fixed(kind),
            _ => Type::Int,
        };
        self.require(&expected, &inner, "negation", self.body.range(operand));
        expected
    }

    pub(super) fn infer_binary(
        &mut self,
        site: ExprId,
        op: BinOp,
        lhs: ExprId,
        rhs: ExprId,
        hint: Option<Type>,
    ) -> Type {
        match op {
            BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Rem => {
                // The left operand decides which arithmetic this is, so the
                // hint goes to it and to nothing else: `let b: U8 = 1 + 2`
                // needs the `1` to know, and once it does the `2` is told by
                // the `expect` below rather than by guessing again.
                if matches!(hint, Some(Type::Fixed(_))) {
                    self.hint = hint;
                }
                // **Zonk before asking what the left operand is.** `infer`
                // hands back whatever variable the operand was given, and
                // inside a closure a `String` parameter *is* a variable bound
                // to `Str` rather than `Str` itself — so the raw type answers
                // "not a string" for every concatenation that is not two
                // literals, and the arithmetic branch below then reports
                // `expected Int, found String`.
                let lhs_range = self.body.range(lhs);
                let left = self.infer(lhs);
                let left = self.unifier.zonk(&left);
                // `+` is also string concatenation, which the reference
                // program relies on.
                if op == BinOp::Add && matches!(left, Type::Str) {
                    self.expect(rhs, &Type::Str, "string concatenation");
                    return Type::Str;
                }
                // **The left operand decides only when it knows something.**
                // Where it is still a variable — an unannotated closure
                // parameter whose call site comes later — the right operand is
                // the only evidence there is, and a `String` there can only be
                // concatenation, since no `Int + String` exists to be ambiguous
                // with. Defaulting to `Int` instead blames the string literal
                // for a line where nothing is wrong.
                if op == BinOp::Add && matches!(left, Type::Var(_)) {
                    let right = self.infer(rhs);
                    let right = self.unifier.zonk(&right);
                    if matches!(right, Type::Str) {
                        self.require(&Type::Str, &left, "string concatenation", lhs_range);
                        return Type::Str;
                    }
                    // Inferred once and reused: the arithmetic below requires
                    // rather than expects, so `rhs` is not visited twice.
                    let expected = match self.unifier.zonk(&left) {
                        Type::Float => Type::Float,
                        Type::Fixed(kind) => Type::Fixed(kind),
                        _ => Type::Int,
                    };
                    self.require(&expected, &left, "arithmetic", lhs_range);
                    self.require(&expected, &right, "arithmetic", self.body.range(rhs));
                    return expected;
                }
                // Arithmetic is over `Int` or over `Float`, and the left
                // operand says which. No mixing and no promotion: `1 + 2.0` is
                // an error rather than a silent conversion, which is what Go
                // and Rust both do and what stops a rounding surprise from
                // being invisible.
                let expected = match left {
                    Type::Float => Type::Float,
                    Type::Fixed(kind) => Type::Fixed(kind),
                    _ => Type::Int,
                };
                self.require(&expected, &left, "arithmetic", lhs_range);
                self.expect(rhs, &expected, "arithmetic");
                expected
            }
            BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Gt | BinOp::Le | BinOp::Ge => {
                let left = self.infer(lhs);
                self.expect(rhs, &left, "this comparison");
                let zonked = self.unifier.zonk(&left);
                let asks = match op {
                    BinOp::Eq | BinOp::Ne => needs_an_eq_impl(&zonked),
                    _ => needs_an_ord_impl(&zonked),
                };
                if asks {
                    let range = self.body.range(site);
                    match op {
                        BinOp::Eq | BinOp::Ne => {
                            self.require_comparison(site, "Eq", "eq", &zonked, range)
                        }
                        // Ordering is `Ord::cmp`, for the same reason equality
                        // is `Eq::eq`: what "less than" means for a type is the
                        // type's answer, and `Ord: Eq` is the trait saying the
                        // two have to agree.
                        _ => self.require_comparison(site, "Ord", "cmp", &zonked, range),
                    }
                }
                Type::Bool
            }
            BinOp::And | BinOp::Or => {
                self.expect(lhs, &Type::Bool, "a logical operator");
                self.expect(rhs, &Type::Bool, "a logical operator");
                Type::Bool
            }
        }
    }

    /// Resolves the impl that a comparison operator on this type will call.
    ///
    /// **`==` on a scalar is a machine instruction; on anything else it is
    /// `Eq::eq`, and `<` is `Ord::cmp`.** So a type decides what equality and
    /// order mean for it, in Khora, in a function a reader can go and look at.
    /// `impl Eq for Int` is written *in terms of* `==` rather than the other way
    /// round, which is what stops the rule being circular — and why `Float` can
    /// have the operators without the traits.
    ///
    /// Recorded as an instantiation, so monomorphization emits the impl and the
    /// code generator can find it, exactly as a written `a.eq(b)` would.
    pub(super) fn require_comparison(
        &mut self,
        site: ExprId,
        trait_name: &str,
        method: &str,
        ty: &Type,
        range: TextRange,
    ) {
        let key = format!("{trait_name}::{method}");

        // Inside a generic function the operand is *rigid*, and the only
        // comparison it has is the one its bounds promise. Which impl runs is
        // decided when the function is specialized, exactly as it is for a
        // written `a.cmp(b)` on a bounded parameter.
        let available = match ty {
            Type::Param(param) => self.bounds_on(param).iter().any(|b| b == trait_name),
            other => self.types.traits.find(trait_name, other).is_some(),
        };
        if !available {
            let advice = match ty {
                Type::Param(param) => format!("Add the bound, as `{param}: {trait_name}`"),
                other => format!("Write `impl {trait_name} for {other}`"),
            };
            let operators = if trait_name == "Eq" {
                "`==` and `!=` have"
            } else {
                "`<`, `>`, `<=` and `>=` have"
            };
            self.error(
                format!("`{ty}` has no `{trait_name}` impl, so {operators} nothing to call. {advice}"),
                range,
            );
            return;
        }
        // Reported whether or not the trait is in this file's scope. "There is
        // no impl" is true either way, and staying quiet here is what let the
        // reference application reach the code generator before anybody
        // mentioned that `RiskLevel` could not be compared.
        let Some(signature) = self.types.signatures.get(key.as_str()).cloned() else { return };

        // `Self` is the method's first type argument — the same fact
        // `call_signature` relies on — and binding it is what tells
        // monomorphization which impl to emit.
        let (_, type_args) =
            self.unifier.instantiate_with(&signature.generics, &signature.as_fn());
        if let Some(self_arg) = type_args.first() {
            let _ = self.unifier.unify(self_arg, ty);
        }
        self.instantiations.insert(site, (key, type_args));
    }

    /// The type of a block: its tail, or `Never` if anything in it diverges.
    ///
    /// A statement that diverges makes the whole block diverge — `{ return 0; }`
    /// has type `Never`, not `()`, or an `if` whose branch returns would
    /// wrongly disagree with the other branch.
    pub(super) fn infer_block(&mut self, stmts: &[Stmt], tail: Option<ExprId>) -> Type {
        // **The hint describes the block's value, which is its tail.** Held
        // here so the statements cannot consume it: `self.hint` is taken by
        // the first `infer` that runs, and without this that is the first
        // statement rather than the tail.
        //
        // Usually harmless, because unifying a concrete hint with an unrelated
        // statement type simply fails and `hint_at` discards the failure. It
        // is not harmless when the hint is an unsolved variable, because then
        // the unification *succeeds* and solves the caller's variable to
        // whatever that statement happened to be:
        //
        //     Result::Ok(body() with { db: over(leased) })
        //
        // A `with` lowers to a block whose first statement binds the
        // capability, so `Ok`'s payload variable was solved to `Db` before the
        // tail was ever looked at -- and the real type then "disagreed" with
        // it. Errata 53.
        let hint = self.hint.take();
        let mut diverged = false;
        for stmt in stmts {
            match stmt {
                Stmt::Let { pat, ty: declared, init } => {
                    // An annotation is checked against the initializer and
                    // then *is* the binding's type. Until errata 36 it was
                    // parsed and dropped, so `let x: Bool = 5` compiled clean
                    // — an annotation that is only a comment is worse than no
                    // annotation, because it is believed.
                    let declared = declared
                        .as_ref()
                        .map(|t| type_of_ref(t, &self.signature.generics, &self.types.homes));
                    let ty = match (declared, init) {
                        (Some(declared), Some(e)) => {
                            self.expect(*e, &declared, "this binding");
                            declared
                        }
                        (Some(declared), None) => declared,
                        (None, Some(e)) => self.infer(*e),
                        (None, None) => Type::Unknown,
                    };
                    diverged |= matches!(ty, Type::Never);
                    self.bind_pattern(*pat, &ty);
                }
                Stmt::Expr(e) => {
                    diverged |= matches!(self.infer(*e), Type::Never);
                }
            }
        }
        self.hint = hint;
        let tail_ty = tail.map(|t| self.infer(t)).unwrap_or(Type::Unit);
        if diverged {
            Type::Never
        } else {
            tail_ty
        }
    }

    /// `{ x: 1, y: 2 }`, or the operations of a handler.
    ///
    /// Nominal, like everything else: the literal is not a type of its own, it
    /// is *some declared record*. `handler for Ledger` says which; a bare
    /// literal is found by its labels, and having to say so when that is
    /// ambiguous is better than inventing a structural type nobody declared.
    pub(super) fn infer_record(
        &mut self,
        owner: Option<String>,
        fields: &[(String, ExprId)],
        base: Option<ExprId>,
        range: TextRange,
    ) -> Type {
        let written: Vec<&str> = fields.iter().map(|(l, _)| l.as_str()).collect();

        // **A field named twice is a mistake, not a last-one-wins.** Worth
        // saying with or without a base; with one it is the difference between
        // overriding a field and overriding it twice.
        for (index, (label, _)) in fields.iter().enumerate() {
            if fields[..index].iter().any(|(earlier, _)| earlier == label) {
                self.error(format!("`{label}` is given twice in this record"), range);
            }
        }

        // **With a base, the base decides the type**, so there is no search by
        // label set and no ambiguity to report. That also means a record
        // update works where a bare literal would not: the fields need not
        // name every one, which is the entire point.
        if let Some(base) = base {
            return self.infer_record_update(base, fields, &written, range);
        }

        let candidates: Vec<VariantInfo> = match &owner {
            Some(name) => self
                .types
                .variants
                .iter()
                .filter(|v| &v.type_name == name && v.name == *name)
                .cloned()
                .collect(),
            None => {
                let record = |exact: bool| -> Vec<VariantInfo> {
                    self.types
                        .variants
                        .iter()
                        .filter(|v| v.name == v.type_name)
                        .filter(|v| {
                            if exact {
                                covers(&v.labels, &written)
                            } else {
                                // A literal short of a field still names its
                                // record. Saying which field is missing beats
                                // saying no type has these fields.
                                written.iter().all(|w| v.labels.iter().any(|l| l == w))
                            }
                        })
                        .cloned()
                        .collect()
                };
                match record(true) {
                    found if !found.is_empty() => found,
                    _ => record(false),
                }
            }
        };

        let record = match candidates.as_slice() {
            [only] => only.clone(),
            [] => {
                for (_, value) in fields {
                    self.infer(*value);
                }
                self.error(
                    match &owner {
                        Some(name) => format!("`{name}` is not a record type"),
                        None => format!(
                            "no record type has exactly the fields {}",
                            written
                                .iter()
                                .map(|l| format!("`{l}`"))
                                .collect::<Vec<_>>()
                                .join(", ")
                        ),
                    },
                    range,
                );
                return Type::Unknown;
            }
            several => {
                for (_, value) in fields {
                    self.infer(*value);
                }
                let names: Vec<String> =
                    several.iter().map(|v| format!("`{}`", v.type_name)).collect();
                self.error(
                    format!(
                        "these fields fit {} — say which with `handler for ..`, or annotate it",
                        names.join(" and ")
                    ),
                    range,
                );
                return Type::Unknown;
            }
        };

        // Field types are declared against the record's own parameters, so the
        // literal decides them: `{ value: 1 }` for `Wrapper<A>` is `Wrapper<Int>`.
        let (whole, mapping) = self.instantiate_adt(&record.type_name);
        let borrowed: HashMap<&str, Type> =
            mapping.iter().map(|(k, v)| (k.as_str(), v.clone())).collect();

        for (label, value) in fields {
            match record.field(label) {
                Some((_, declared)) => {
                    let declared = unify::substitute(declared, &borrowed);
                    self.expect(*value, &declared, &format!("field `{label}`"));
                }
                None => {
                    self.infer(*value);
                    let range = self.body.range(*value);
                    self.error(
                        format!("`{}` has no field `{label}`", record.type_name),
                        range,
                    );
                }
            }
        }
        for label in &record.labels {
            if !written.iter().any(|w| w == label) {
                self.error(
                    format!("this `{}` is missing `{label}`", record.type_name),
                    range,
                );
            }
        }
        // A handler is the one place a capability's closures are visible, and
        // an effect's shareability is paid for by asking here.
        if self.types.effects.contains(&record.type_name) {
            let owner = record.type_name.clone();
            self.check_handler_is_shareable(&owner, fields);
        }
        whole
    }

    /// `{ ..old, field: value }` — `old` with some fields replaced.
    ///
    /// **The result is a new record**; `old` is untouched and still whatever
    /// it was. What the syntax saves is writing out the fields that do not
    /// change, which for one accumulator in one program was thirty-five lines
    /// of five near-identical literals.
    ///
    /// Every named field must belong to the base's type and match its declared
    /// type. Nothing else is required — a record update naming no fields is
    /// `old`, which is legal and pointless.
    fn infer_record_update(
        &mut self,
        base: ExprId,
        fields: &[(String, ExprId)],
        written: &[&str],
        range: TextRange,
    ) -> Type {
        let whole = self.infer(base);
        let settled = self.unifier.zonk(&whole);

        // Not settled yet, or already reported. Check the values so they are
        // not left uninferred, and let the `Unknown` audit have the last word.
        let Some(name) = traits::head_of(&settled) else {
            for (_, value) in fields {
                self.infer(*value);
            }
            return settled;
        };

        let Some(record) = self
            .types
            .variants
            .iter()
            .find(|v| v.type_name == name && v.name == name)
            .cloned()
        else {
            for (_, value) in fields {
                self.infer(*value);
            }
            if !matches!(settled, Type::Unknown | Type::Var(_) | Type::Never) {
                self.error(
                    format!(
                        "`{settled}` is not a record, so there is nothing to take fields \
                         from with `..`"
                    ),
                    self.body.range(base),
                );
            }
            return Type::Unknown;
        };

        // The base's own type arguments decide the field types, which is what
        // makes `{ ..pair, key: 2 }` keep the value's type rather than solving
        // it again.
        let arguments = match &settled {
            Type::Adt { args, .. } => args.clone(),
            _ => Vec::new(),
        };
        let parameters = self.types.adts.get(&name).cloned().unwrap_or_default();
        let borrowed: HashMap<&str, Type> = parameters
            .iter()
            .zip(&arguments)
            .map(|(p, a)| (p.as_str(), a.clone()))
            .collect();

        for (label, value) in fields {
            match record.field(label) {
                Some((_, declared)) => {
                    let declared = unify::substitute(declared, &borrowed);
                    self.expect(*value, &declared, &format!("field `{label}`"));
                }
                None => {
                    self.infer(*value);
                    let at = self.body.range(*value);
                    self.error(format!("`{name}` has no field `{label}`"), at);
                }
            }
        }
        let _ = (written, range);

        settled
    }

    /// The position and type of `label` on a record, at this instantiation.
    ///
    /// A record's fields are declared against the type's own parameters, so
    /// they have to be read at the value's arguments: `Pair<Int>.first` is
    /// `Int`, not `A`.
    pub(super) fn record_field(&mut self, owner: &Type, label: &str) -> Option<(usize, Type)> {
        let Type::Adt { name, .. } = owner else { return None };
        // A *record* — `type Point = { x: Int }` — whose one variant carries
        // the type's own name. `type User = | Of(age: Int)` is a sum that
        // happens to have one case, and its payload is reached by matching:
        // `Of` is a constructor, not a field, and the two must not blur.
        // By identity, not by spelling. Finding it by name alone is how a file
        // that declares a `Point` and imports another module's `Point` was told
        // its own type had no field `label`. Errata 46.
        let record = self.types.record_of(owner)?;
        let (index, declared) = record.field(label).map(|(i, t)| (i, t.clone()))?;

        let mapping = self.substitution_for(name, owner);
        let borrowed: HashMap<&str, Type> =
            mapping.iter().map(|(k, v)| (k.as_str(), v.clone())).collect();
        let field = unify::substitute(&declared, &borrowed);

        // **An operation may quantify over a row the effect does not declare.**
        //
        // `adopt: (Fiber<(), 'er>) -> ()` names a row that `Nursery` has no
        // parameter for, and the substitution above has already replaced every
        // row the effect *does* declare -- so a `'x` still standing here is the
        // operation's own, and belongs to each call rather than to the handler.
        // Instantiating it is what `instantiate` does for a generic function,
        // and for the same reason: two call sites must not constrain each
        // other.
        //
        // **A row costs nothing to quantify**, which is why this is a checker
        // change and not a monomorphization one. A capability crosses as
        // evidence and an error as a tag, so the handler's closure is the same
        // code for every `'er` -- unlike a *type* parameter, which decides a
        // layout and has to be monomorphized. That asymmetry is the whole
        // reason an operation can be row-generic when it cannot be generic.
        //
        // **An effect and not every record**, which is a real distinction and
        // not caution. A record's field holds a closure somebody built, and
        // that closure has one row, decided where it was written -- reading the
        // field twice must not pretend otherwise. An effect's operation is a
        // *declaration* the handler has to satisfy, and it is satisfied
        // rigidly: `rows.rs` pins that a handler using `'er` is refused with
        // "a type the caller chooses". So the quantifier is rank-1 -- the call
        // instantiates, the handler does not -- and an operation is row-generic
        // exactly when its handler never looks at the row.
        if self.types.effects.contains(name) {
            let rows = unify::free_row_params(&field);
            if !rows.is_empty() {
                return Some((index, self.unifier.instantiate(&rows, &field)));
            }
        }
        Some((index, field))
    }

    pub(super) fn infer_match(
        &mut self,
        scrutinee: ExprId,
        arms: &[khora_hir::body::MatchArm],
        range: TextRange,
    ) -> Type {
        let scrutinee_ty = self.infer(scrutinee);

        let mut result: Option<Type> = None;
        for arm in arms {
            self.bind_pattern(arm.pat, &scrutinee_ty);
            if let Some(guard) = arm.guard {
                self.expect(guard, &Type::Bool, "a match guard");
            }
            let arm_ty = self.infer(arm.body);
            match result.clone() {
                None => result = Some(arm_ty),
                Some(expected) => {
                    let range = self.body.range(arm.body);
                    if self.require(&expected, &arm_ty, "match arms disagree", range) {
                        if matches!(expected, Type::Never) {
                            result = Some(arm_ty);
                        }
                    } else {
                        result = Some(Type::Unknown);
                    }
                }
            }
        }

        self.check_match_coverage(&scrutinee_ty, arms, range);
        result.unwrap_or(Type::Unknown)
    }

    /// `f()! catch { .. }` — handles part of the error row.
    ///
    /// The subtraction is by error *type*, named by the arms' constructors, so
    /// this is not a `match` on a result: the arms see the error the operand
    /// left with rather than a value it produced, and the ones they name stop
    /// being the enclosing function's problem.
    ///
    /// **A `_` arm subtracts the whole row, including its tail** — the one
    /// thing naming constructors cannot express, and what a supervisor needs: a
    /// server answering a request, or a queue running a job, has to recover
    /// from work whose failures are the *caller's* choice. It costs what it
    /// should, since an arm with no name to learn under learns nothing about
    /// what went wrong. Name the constructors where they are known.
    pub(super) fn infer_catch(
        &mut self,
        inner: ExprId,
        arms: &[khora_hir::body::MatchArm],
        range: TextRange,
    ) -> Type {
        // Demands raised *inside* the operand are the ones this `catch` is in
        // a position to handle. Remembering where the list stood draws that
        // window: a demand from an enclosing expression is not in it, and a
        // nested `catch` has already narrowed its own.
        let before = self.demanded.len();
        let value = self.infer(inner);

        // Each arm is matched against its own error type rather than against
        // one scrutinee, which is the other way this differs from `match`.
        let mut caught: Vec<String> = Vec::new();
        let mut result: Option<Type> = None;
        // Whether an arm handles what the named ones did not.
        let mut everything = false;
        for arm in arms {
            let owner = match self.body.pat(arm.pat) {
                Pat::Path(r) | Pat::TupleStruct { resolution: r, .. } => {
                    variant_case(r).map(|(_, t, _)| t)
                }
                _ => None,
            };
            if owner.is_none() && matches!(self.body.pat(arm.pat), Pat::Wildcard) {
                everything = true;
                if let Some(guard) = arm.guard {
                    self.expect(guard, &Type::Bool, "a match guard");
                }
                let arm_ty = self.infer(arm.body);
                match result.clone() {
                    None => result = Some(arm_ty),
                    Some(expected) => {
                        let at = self.body.range(arm.body);
                        if self.require(&expected, &arm_ty, "catch arms disagree", at) {
                            if matches!(expected, Type::Never) {
                                result = Some(arm_ty);
                            }
                        } else {
                            result = Some(Type::Unknown);
                        }
                    }
                }
                continue;
            }
            let Some(owner) = owner else {
                // Silent when the pattern named a constructor that did not
                // resolve: that is already reported, and saying it twice buries
                // the message that can actually be acted on.
                if !matches!(
                    self.body.pat(arm.pat),
                    Pat::Path(_) | Pat::TupleStruct { .. } | Pat::Missing
                ) {
                    self.error(
                        "a `catch` arm has to name an error constructor, since it is the \
                         constructor's type that says which errors are handled here"
                            .to_string(),
                        self.body.range(arm.body),
                    );
                }
                continue;
            };
            if !caught.contains(&owner) {
                caught.push(owner.clone());
            }
            self.bind_pattern(arm.pat, &Type::adt(&owner));
            if let Some(guard) = arm.guard {
                self.expect(guard, &Type::Bool, "a match guard");
            }
            let arm_ty = self.infer(arm.body);
            match result.clone() {
                None => result = Some(arm_ty),
                Some(expected) => {
                    let range = self.body.range(arm.body);
                    if self.require(&expected, &arm_ty, "catch arms disagree", range) {
                        if matches!(expected, Type::Never) {
                            result = Some(arm_ty);
                        }
                    } else {
                        result = Some(Type::Unknown);
                    }
                }
            }
        }

        // Naming a type commits to all of it. A partially handled type would
        // have to stay in the row *and* divert some of its variants, so the
        // signature would say it can still leave while the reader sees it
        // handled — the subtraction is only honest if it is total.
        //
        // Unless a `_` arm is there to take the rest, which is what makes
        // `catch { NotFound => .., _ => .. }` the ordinary shape it looks like.
        for owner in caught.iter().filter(|_| !everything) {
            let mine: Vec<khora_hir::body::MatchArm> = arms
                .iter()
                .filter(|a| {
                    matches!(self.body.pat(a.pat),
                        Pat::Path(r) | Pat::TupleStruct { resolution: r, .. }
                            if variant_case(r).is_some_and(|(_, t, _)| &t == owner))
                })
                .cloned()
                .collect();
            self.check_match_coverage(&Type::adt(owner), &mine, range);
        }

        // The bodies stand in for the operand's value, so the whole expression
        // has one type whichever way it went.
        if let Some(handled) = result.clone() {
            self.require(&value, &handled, "a `catch` arm", range);
        }

        // Subtract. The demand stays even when nothing is left of its row: it
        // is also what checks that the call wore its `!`, and a `catch` does
        // not excuse the mark — control still leaves the operand.
        let window: Vec<Demand> = self.demanded.split_off(before);
        let mut names = Vec::new();
        let kept: Vec<Demand> = window
            .into_iter()
            .map(|mut demand| {
                if demand.clause == Clause::Raises {
                    if let Type::Row { fields, tail } = &demand.row {
                        names.extend(fields.iter().map(|(l, _)| l.clone()));
                        if everything {
                            // Tail and all: that is the difference between this
                            // and any number of named arms.
                            demand.row = Type::row(Vec::new(), None);
                        } else {
                            let left: Vec<(String, Type)> = fields
                                .iter()
                                .filter(|(l, _)| !caught.contains(l))
                                .cloned()
                                .collect();
                            demand.row = Type::row(left, tail.as_deref().cloned());
                        }
                    }
                }
                demand
            })
            .collect();
        self.demanded.extend(kept);

        for owner in &caught {
            if !names.contains(owner) {
                self.error(
                    format!("nothing in this expression raises `{owner}`"),
                    range,
                );
            }
        }

        value
    }
}

impl<'a> Checker<'a> {
    /// The type of a `${..}` hole: `String`, if the value can be shown.
    ///
    /// **Resolved against the value's type, not the module's imports.** The
    /// hole is the use; `Show` is never written, so requiring it in scope would
    /// mean a lint calling the import unused and removing it breaking the
    /// build — the trap trait bounds were just taken out of.
    ///
    /// Recorded as an instantiation of `Show::show` with the value's type as
    /// its `Self`, which is exactly the shape an ordinary trait call takes, so
    /// monomorphization picks the impl by the route it already has and code
    /// generation finds the symbol where it finds every other one.
    fn shown(&mut self, site: ExprId, value: ExprId, range: TextRange) -> Type {
        let ty = self.infer(value);
        let settled = self.unifier.zonk(&ty);

        // Nothing to say about a type inference has not worked out. The
        // `Unknown` audit reports the ones that never do, and complaining here
        // as well would blame the message for the expression inside it.
        if matches!(settled, Type::Unknown | Type::Var(_) | Type::Never) {
            return Type::Str;
        }

        // **A hole that already holds a `String` needs nothing.** Not the
        // call, which `impl Show for String` would answer with the string
        // itself, and not the requirement either: a message made of text
        // should work in a file that has never heard of `Show`, which is most
        // files. Recording no instantiation is what tells code generation to
        // use the value where it stands.
        if matches!(settled, Type::Str) {
            return Type::Str;
        }

        if !self.satisfies(SHOW, &settled) {
            self.error(
                format!(
                    "`{settled}` has no `Show`, so it cannot go in a `${{..}}` hole. \
                     Write `derive(Show)` on it, or `impl Show for {settled}`"
                ),
                range,
            );
            return Type::Str;
        }

        let key = format!("{SHOW}::show");
        if let Some(signature) = self.types.signatures.get(key.as_str()).cloned() {
            let (_, type_args) =
                self.unifier.instantiate_with(&signature.generics, &signature.as_fn());
            // `Self` first, as every trait call carries it.
            if let Some(first) = type_args.first() {
                let _ = self.unifier.unify(first, &settled);
            }
            self.instantiations.insert(site, (key, type_args));
        }

        Type::Str
    }
}

