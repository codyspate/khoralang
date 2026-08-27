//! Rows: what a function requires, what it raises, and where both go.
//!
//! A requirement raised inside a `with` block is discharged by it and a
//! `catch` subtracts from the failure row, so this is row subtraction with
//! diagnostics attached. `docs/design/effects.md`.
//!
//! The direction that matters: a demand travels *up* until something discharges
//! it, and reaching the top of a function that did not declare it is the error.
//! Nothing here asks what a function provides; it asks what is still owed.

use super::*;

impl<'a> Checker<'a> {
    /// Closes every lambda error row nothing ever asked to be wider.
    ///
    /// Run once the body is checked. A tail still unsolved here was never
    /// compared against anything, so the honest reading is "this raises exactly
    /// what its body raises" — which is the closed empty row on the end.
    pub(crate) fn close_open_raises(&mut self) {
        for tail in std::mem::take(&mut self.open_raises) {
            if matches!(self.unifier.shallow(&tail), Type::Var(_)) {
                let _ = self.unifier.unify(&tail, &Type::empty_row());
            }
        }
    }

    /// Requires two types to be equal, reporting `context` if they are not.
    ///
    /// `context` is a noun phrase: the mismatch supplies the detail after it,
    /// so the two read as one sentence.
    pub(super) fn require(&mut self, expected: &Type, found: &Type, context: &str, range: TextRange) -> bool {
        // A projection whose owner is not known yet defers instead of failing,
        // and is retried in `settle_projections`. Where it was written is not
        // recoverable from the unifier, so it is noted here, in the one place
        // that has both a range and a reason.
        let before = self.unifier.deferred_len();
        let outcome = self.unifier.unify(expected, found);
        for _ in before..self.unifier.deferred_len() {
            self.projections.push((range, context.to_string()));
        }
        match outcome {
            Ok(()) => true,
            Err(why) => {
                // Zonk first: a message naming `?3` instead of `Int` is useless,
                // and unification may have solved the variable on the way to
                // discovering the conflict.
                let message = match why {
                    Mismatch::Types { expected: inner, found: got } => {
                        let inner = self.unifier.zonk(&inner);
                        let got = self.unifier.zonk(&got);
                        let outer = self.unifier.zonk(expected);
                        let whole = self.unifier.zonk(found);
                        let head =
                            Mismatch::Types {
                            expected: Box::new(outer.clone()),
                            found: Box::new(whole.clone()),
                        };
                        let detail = disagreement((&outer, &whole), (&inner, &got));
                        format!("{context}: {head}{detail}")
                    }
                    other => format!("{context}: {other}"),
                };
                self.error(message, range);
                false
            }
        }
    }

    /// Records what a call requires of the enclosing function.
    ///
    /// The rows are instantiated with the same arguments the signature was, so
    /// a row variable in the callee becomes a fresh variable here and is
    /// solved by whatever the caller turns out to provide.
    pub(super) fn demand(
        &mut self,
        signature: &Signature,
        type_args: &[Type],
        key: &str,
        callee_site: ExprId,
        range: TextRange,
    ) {
        let mapping: HashMap<&str, Type> = signature
            .generics
            .iter()
            .map(String::as_str)
            .zip(type_args.iter().cloned())
            .collect();
        let requires = unify::substitute(&signature.requires, &mapping);
        let raises = unify::substitute(&signature.raises, &mapping);
        self.demand_rows(&requires, &raises, key, Some(callee_site), range);
    }

    /// Records what a call requires, given rows that are already instantiated.
    ///
    /// This is the form a call *through a value* uses: the rows are part of
    /// the callee's type rather than looked up from a signature, which is what
    /// lets an effectful function be passed around and called somewhere else.
    pub(super) fn demand_rows(
        &mut self,
        requires: &Type,
        raises: &Type,
        key: &str,
        callee_site: Option<ExprId>,
        range: TextRange,
    ) {
        // Before anything is subtracted: what an enclosing `with` block
        // supplies is exactly what a lambda inside it has to capture, so the
        // labels have to be read here rather than after they are discharged.
        if let (Some(site), Type::Row { fields, .. }) = (callee_site, &self.unifier.zonk(requires))
        {
            for (label, _) in fields {
                self.note_implicit_capture(site, label);
            }
        }

        for (clause, row) in [(Clause::Requires, requires), (Clause::Raises, raises)] {
            // Zonked here, not later: the `installed` subtraction below needs
            // the labels, and `installed` is scoped to the `with` block this
            // call sits in — by the time `check_effects` runs, the block is
            // long gone. A row that a call's arguments have already solved is
            // known by now, which is the ordinary case; one that is still a
            // variable simply has nothing to subtract.
            let mut row = self.unifier.zonk(row);
            // Whatever an enclosing `with` block supplies is already answered.
            if clause == Clause::Requires {
                if let Type::Row { fields, tail } = &row {
                    let left: Vec<(String, Type)> = fields
                        .iter()
                        .filter(|(l, _)| !self.installed.contains(l))
                        .cloned()
                        .collect();
                    row = Type::row(left, tail.as_deref().cloned());
                }
            }
            if matches!(&row, Type::Row { fields, tail } if fields.is_empty() && tail.is_none()) {
                continue;
            }
            // A row with something *in* it. A variable is not one yet — a
            // closure calling itself asks for the row it is in the middle of
            // inferring — and neither is an open tail, which says "possibly
            // more" rather than "at least one". Every lambda's row is open
            // now, because what a body raises is a lower bound; counting a
            // tail here would make every self-call demand a `!` for nothing.
            //
            // Nothing is lost by ignoring it: if the tail is later solved to
            // something with labels in it, the row itself says so, and
            // `check_effects` re-reads the row.
            let known_fallible = matches!(&row, Type::Row { fields, .. } if !fields.is_empty());

            // Published for whoever wants to *show* this rather than check it.
            // Here rather than afterwards: by the time the demand list is
            // reconciled with the signature, a `with` block or a `catch` has
            // subtracted from the row and it no longer says what the call
            // asked for. `crate::CallRows`.
            if let Some(at) = callee_site {
                let entry = self.call_rows.entry(at).or_default();
                match clause {
                    Clause::Requires => entry.requires = Some(row.clone()),
                    Clause::Raises => entry.raises = Some(row.clone()),
                }
            }

            self.demanded.push(Demand {
                fallible: clause == Clause::Raises && known_fallible,
                clause,
                row,
                range,
                callee: key.to_string(),
                site: callee_site,
            });
        }
    }

    /// Takes the failures demanded since `before` as a closure's own row.
    ///
    /// A closure cannot charge its failures to whoever wrote it: it may be
    /// called anywhere, and by then that function has returned. So they become
    /// part of *its* type, and the enclosing function answers only what it was
    /// asked directly.
    ///
    /// The demands stay in the list with their rows emptied rather than being
    /// removed, because they are also what checks that a fallible call wore its
    /// `!`. A closure no more excuses the mark than a `catch` does.
    pub(super) fn absorb_raises(&mut self, before: usize) -> Type {
        let window: Vec<Demand> = self.demanded.split_off(before);
        let mut fields: Vec<(String, Type)> = Vec::new();
        let mut tail = None;

        let kept: Vec<Demand> = window
            .into_iter()
            .map(|mut demand| {
                if demand.clause == Clause::Raises {
                    if let Type::Row { fields: raised, tail: rest } = self.unifier.zonk(&demand.row)
                    {
                        fields.extend(raised);
                        tail = tail.take().or(rest.map(|t| *t));
                        demand.row = Type::empty_row();
                    }
                }
                demand
            })
            .collect();
        self.demanded.extend(kept);
        Type::row(fields, tail)
    }

    /// Takes the capabilities a closure could not resolve lexically as its own.
    ///
    /// **Resolve if you can, require if you cannot.** A lambda written where
    /// `ledger` is in scope captures it and its requirement row stays empty, so
    /// `List::map` does not have to be row-polymorphic to accept a callback
    /// that logs.
    ///
    /// The other case is every library that installs a capability for its
    /// callback:
    ///
    /// ```khora
    /// nursery(fn () => serve()!)
    /// ```
    ///
    /// `serve` needs a nursery, `nursery` will supply one, and the thunk was
    /// written before the binding existed. Charging that to the enclosing
    /// function is wrong twice over: it has no nursery, and it is not the one
    /// being asked. So the demand becomes the *closure's* requirement — what a
    /// named function does with its `with` clause, and why `nursery(serve)`
    /// works where its own eta-expansion would not.
    ///
    /// Closed, not open: these are exactly what this body needs, the same
    /// promise a written `with` clause makes.
    /// `docs/design/capability-passing.md`.
    pub(super) fn absorb_requires(&mut self, before: usize) -> Type {
        let window: Vec<Demand> = self.demanded.split_off(before);
        let mut mine: Vec<(String, Type)> = Vec::new();

        let kept: Vec<Demand> = window
            .into_iter()
            .map(|mut demand| {
                if demand.clause != Clause::Requires {
                    return demand;
                }
                let Type::Row { fields, tail } = self.unifier.zonk(&demand.row) else {
                    return demand;
                };
                // Label by label, because one call can need two capabilities
                // and have a binding for only one of them.
                let mut left = Vec::new();
                for (label, ty) in fields {
                    let lexical = demand
                        .site
                        .and_then(|site| self.body.capability_at(site, &label))
                        .is_some();
                    if lexical || self.installed.contains(&label) {
                        left.push((label, ty));
                    } else if !mine.iter().any(|(l, _)| l == &label) {
                        mine.push((label, ty));
                    }
                }
                demand.row = Type::row(left, tail.map(|t| *t));
                demand
            })
            .collect();
        self.demanded.extend(kept);
        // In label order, which is the order code generation appends the
        // parameters in. Two places agreeing on an order is how errata 33
        // happened; one of them sorting is how it does not happen again.
        mine.sort_by(|(a, _), (b, _)| a.cmp(b));
        Type::row(mine, None)
    }

    /// Records that a lambda uses a capability without naming it.
    ///
    /// Against every enclosing lambda, not just the innermost: an inner
    /// lambda reads the binding out of the outer one's frame, so the outer one
    /// has to have captured it too. The mark is what says whether a binding is
    /// outside a given lambda — below it means declared before the lambda
    /// began, which is what captured means everywhere else.
    pub(super) fn note_implicit_capture(&mut self, site: ExprId, label: &str) {
        let Some(local) = self.body.capability_at(site, label) else { return };
        for (lambda, found) in &mut self.enclosing_lambdas {
            let mark = self.body.lambda_marks.get(lambda).copied().unwrap_or(0);
            if local.index() < mark && !found.contains(&local) {
                found.push(local);
            }
        }
    }

    /// Checks everything the body demanded against what the signature promised.
    ///
    /// Run once, after the body: a requirement is satisfied by the declaration
    /// or it is an error, and reporting it at the call that raised it is what
    /// makes the message actionable.
    pub(crate) fn check_effects(&mut self) {
        for Demand { fallible, clause, row, range, callee, site } in
            std::mem::take(&mut self.demanded)
        {
            let callee = as_written(&callee);

            // Zonked before anything is decided: a row recorded as a variable
            // is only now known to be anything.
            let row = self.unifier.zonk(&row);
            let empty =
                matches!(&row, Type::Row { fields, tail } if fields.is_empty() && tail.is_none());
            // Nothing left to satisfy, but possibly still something to mark:
            // a `catch` discharges the row and does not excuse the `!`.
            if empty && !fallible {
                continue;
            }
            // Satisfied means *subsumed*, not equal: a caller providing
            // `{ ledger, ai }` can call something needing only `{ ledger }`.
            // Opening the demand is that check — its labels must all be
            // present, and its fresh tail absorbs whatever the promise has
            // that this call did not ask for.
            let row = match &row {
                Type::Row { fields, tail: None } => {
                    let rest = self.unifier.fresh();
                    Type::row(fields.clone(), Some(rest))
                }
                // A bare row *variable* is a row already: `'r` means
                // `{ | 'r }`. Written out so the two shapes are one shape from
                // here on.
                Type::Param(_) => Type::row(Vec::new(), Some(row.clone())),
                other => other.clone(),
            };
            let promise = match clause {
                Clause::Requires => self.signature.requires.clone(),
                Clause::Raises => self.signature.raises.clone(),
            };

            // A call that can leave the function says so at the call site.
            // Reported before the row is compared: "mark it" is the actionable
            // half, and a marked call whose row is also wrong reports both.
            if clause == Clause::Raises {
                if let Some(site) = site {
                    if !self.marked.contains(&site) {
                        self.error(
                            format!(
                                "`{callee}` can leave this function, so the call needs `!`: \
                                 write `{callee}(..)!`"
                            ),
                            range,
                        );
                    }
                }
            }

            if empty {
                continue;
            }

            // A demand whose tail is a *rigid* variable cannot be opened:
            // there is no fresh tail to absorb the promise's extra labels,
            // since the demand already stands for "whatever `'r` is". It is
            // satisfied when the promise carries the same tail and at least the
            // same labels — subsumption, where neither side knows the tail.
            //
            // Every row-polymorphic library function needs this the moment it
            // adds a capability of its own: `listen` promising
            // `{ 'r | scope: Scope }` and calling something that needs `'r`
            // reads to unification alone as `'r` being asked to equal
            // `{ scope: Scope | 'r }`.
            if self.demand_is_carried(&row, &promise) {
                continue;
            }

            if let Err(why) = self.unifier.unify(&promise, &row) {
                self.error(
                    match why {
                        unify::Mismatch::Missing { label, ty } => format!(
                            "`{callee}` needs `{}`, which this function does not {}",
                            clause.describe(&label, &ty),
                            clause.verb()
                        ),
                        other => format!("`{callee}` cannot be called here: {other}"),
                    },
                    range,
                );
            }
        }
    }

    /// Whether `promise` covers `demand` outright, tails and all.
    ///
    /// Only asked of a demand with a rigid tail, where opening it is not
    /// possible — see the caller. `false` means "not obviously", and the
    /// ordinary comparison runs and reports whatever it finds; nothing is
    /// accepted here that unification would have rejected for a reason.
    pub(super) fn demand_is_carried(&mut self, demand: &Type, promise: &Type) -> bool {
        let (Type::Row { fields: wanted, tail: Some(wanted_tail) }, Type::Row { fields: held, tail: Some(held_tail) }) =
            (self.unifier.zonk(demand), self.unifier.zonk(promise))
        else {
            return false;
        };
        // Both rigid, and the same one. Two different rigid tails are two
        // different unknowns and neither covers the other.
        let (Type::Param(wanted_tail), Type::Param(held_tail)) = (*wanted_tail, *held_tail) else {
            return false;
        };
        if wanted_tail != held_tail {
            return false;
        }
        wanted.iter().all(|(label, ty)| {
            held.iter().any(|(held_label, held_ty)| {
                held_label == label && self.unifier.unify(held_ty, ty).is_ok()
            })
        })
    }
}
