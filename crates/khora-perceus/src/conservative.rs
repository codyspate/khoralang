//! The scheme that is correct before anything is optimised.
//!
//! A local holding a boxed value owns one reference, reading it copies, and a
//! block releases what it declared on the way out. Parameters are owned by the
//! callee, so they are released like locals.
//!
//! It balances because a read copies and the callee that receives the value
//! releases it: a call is neutral, and `let t = s; t` allocates once, copies
//! twice and releases twice, leaving the one reference the caller gets.
//!
//! Everything in the other modules here is subtraction from this.

use super::*;

impl<'a> Planner<'a> {
    pub(super) fn plan_function(&mut self) {
        let mut owned = Vec::new();
        // Capabilities are parameters like any other: owned by the callee,
        // read with a dup, released where the body ends. Treating them as
        // borrowed instead would be cheaper and wrong — `ledger.balance(id)`
        // releases the record it read the field out of, and a borrowed
        // capability would be freed under its caller.
        let params: Vec<PatId> = self
            .body
            .params
            .iter()
            .copied()
            .chain(self.body.evidence.iter().map(|(_, pat)| *pat))
            .collect();
        for pat in params {
            self.bind(pat, &mut owned);
        }

        let Some(root) = self.body.root else { return };
        self.walk(root);

        // Parameters are owned by the callee, so the outermost block releases
        // them along with whatever it declared itself.
        if !owned.is_empty() {
            self.plan.drops.entry(root).or_default().extend(owned);
        }
    }

    /// Records a read that takes no reference of its own.
    pub(super) fn borrow(&mut self, at: ExprId) {
        let Expr::Local(local) = *self.body.expr(at) else { return };
        self.plan.borrowed.insert(at);
        if self.plan.boxed.contains(&local) {
            self.reads.push(Read {
                local,
                at,
                borrowed: true,
            });
        }
    }

    /// What `borrowed_arguments` says, unless the program implements the method
    /// itself.
    ///
    /// A Khora body owns its parameters and releases them, so telling its
    /// caller to lend one is a use after free. Any package may declare a type
    /// called `Shared` with a `get`, and this table is keyed by a bare name.
    /// `khora_perceus::Defined`.
    fn lends(&self, owner: &str, method: &str) -> Vec<usize> {
        if self.defined.writes(owner, method) {
            return Vec::new();
        }
        borrowed_arguments(owner, method).to_vec()
    }

    /// The argument positions this callee only looks at.
    pub(super) fn lent_by(&self, callee: ExprId) -> Vec<usize> {
        match self.body.expr(callee) {
            Expr::Path(khora_hir::Resolution::TraitItem { owner, name }) => {
                self.lends(owner, name)
            }
            _ => Vec::new(),
        }
    }

    /// Records a pattern's bindings and collects the boxed ones.
    pub(super) fn bind(&mut self, pat: PatId, owned: &mut Vec<LocalId>) {
        match self.body.pat(pat).clone() {
            Pat::Bind(local) => {
                if is_boxed(self.types.local(local)) {
                    self.plan.boxed.insert(local);
                    owned.push(local);
                }
            }
            Pat::TupleStruct { fields, .. } | Pat::Tuple(fields) => {
                for field in fields {
                    self.bind(field, owned);
                }
            }
            Pat::Wildcard | Pat::Literal(_) | Pat::Path(_) | Pat::Missing => {}
        }
    }

    pub(super) fn walk(&mut self, id: ExprId) {
        match self.body.expr(id).clone() {
            Expr::Local(local) => {
                // The value outlives the read, so it needs its own reference —
                // unless this read turns out to be the last one, which
                // `settle_last_uses` decides once the whole body has been seen.
                if self.plan.boxed.contains(&local) {
                    self.plan.dups.insert(id);
                    self.reads.push(Read {
                        local,
                        at: id,
                        borrowed: false,
                    });
                }
            }
            // A record's fields are moved into it, exactly as a
            // constructor's arguments are.
            // The error is moved into the return, and `!` is the identity on
            // ownership: the value it unwraps is the value the call produced.
            Expr::Raise(error) => {
                self.unwinds = true;
                self.walk(error);
            }
            Expr::Try(inner) => {
                self.unwinds = true;
                self.walk(inner);
            }
            Expr::Record { fields, .. } => {
                for (_, value) in &fields {
                    self.walk(*value);
                }
            }
            Expr::Lambda { params, body, .. } => {
                // The lambda's parameters are owned by the lambda, exactly as a
                // function's are, and released where its body ends. Captures
                // are *not* released here: the closure object owns those, and
                // its drop glue is what lets them go.
                let mut owned = Vec::new();
                for pat in &params {
                    self.bind(*pat, &mut owned);
                }
                // Deliberately not recorded as drops here. A lambda body is
                // not always a block — `(x) => x + 1` is an expression — and
                // the plan's releases are keyed by block. The lifted function
                // releases its own parameters instead, on every path out.
                let _ = owned;
                // A lambda's body runs when the closure is called, which is not
                // here and may be never.
                self.walk(body);
            }
            Expr::Block { stmts, tail } => {
                let mut declared = Vec::new();
                for stmt in &stmts {
                    match stmt {
                        Stmt::Let { pat, init, .. } => {
                            if let Some(init) = init {
                                self.walk(*init);
                            }
                            self.bind(*pat, &mut declared);
                        }
                        Stmt::Expr(e) => self.walk(*e),
                    }
                }
                if let Some(tail) = tail {
                    self.walk(tail);
                }
                if !declared.is_empty() {
                    self.plan.drops.entry(id).or_default().extend(declared);
                }
            }
            Expr::Match { scrutinee, arms } => {
                self.walk(scrutinee);
                for arm in &arms {
                    let mut bound = Vec::new();
                    self.bind(arm.pat, &mut bound);
                    if let Some(guard) = arm.guard {
                        self.walk(guard);
                    }
                    self.walk(arm.body);
                    // The arm owns what its body actually reads: one copy on
                    // the way in, and the arm releases it. A binding the body
                    // never touches is left borrowed, because owning it would
                    // be a copy and a release for nothing.
                    //
                    // A read in the *guard* does not count. A guard runs before
                    // the arm is committed to, so the copy has not happened
                    // yet; those reads copy out of the payload the scrutinee
                    // still holds, exactly as they always did.
                    let read = self.reads_in(arm.body);
                    bound.retain(|local| read.contains(local));
                    if !bound.is_empty() {
                        self.plan.arm_binds.insert(arm.body, bound);
                    }
                }
            }
            // Same shape as `match`, over the error rather than a scrutinee.
            // The error object itself is owned by the catching frame — the
            // raising one moved it into the return — so code generation drops
            // it after the arm; see `lower_catch`.
            Expr::Catch { inner, arms } => {
                self.unwinds = true;
                self.walk(inner);
                for arm in &arms {
                    let mut ignored = Vec::new();
                    self.bind(arm.pat, &mut ignored);
                    if let Some(guard) = arm.guard {
                        self.walk(guard);
                    }
                    self.walk(arm.body);
                }
            }
            // `xs.length()` — the receiver is the callee's base rather than an
            // argument, and it is how most of these are written. Reading only
            // the `Array::length(xs)` spelling meant the table quietly did
            // nothing for the calls that matter.
            Expr::Call { callee, args }
                if matches!(self.body.expr(callee), Expr::Field { .. }) =>
            {
                let Expr::Field { base, name } = self.body.expr(callee).clone() else {
                    unreachable!("just matched")
                };
                let lends = owner_of(self.types.of(base))
                    .is_some_and(|owner| self.lends(owner, &name).contains(&0));
                if lends && matches!(self.body.expr(base), Expr::Local(_)) {
                    self.borrow(base);
                } else {
                    self.walk(base);
                }
                for arg in args {
                    self.walk(arg);
                }
            }
            Expr::Call { callee, args } => {
                self.walk(callee);
                let lent = self.lent_by(callee);
                for (index, arg) in args.iter().enumerate() {
                    // A borrowed argument that is a plain read of a binding
                    // needs no reference of its own: the binding holds one and
                    // outlives the call. Anything else — a temporary, an
                    // expression — is owned by nobody else, so it is passed and
                    // released as before.
                    if lent.contains(&index) && matches!(self.body.expr(*arg), Expr::Local(_)) {
                        self.borrow(*arg);
                        continue;
                    }
                    self.walk(*arg);
                }
            }
            Expr::Binary { lhs, rhs, .. } => {
                self.walk(lhs);
                self.walk(rhs);
            }
            Expr::Assign { target, value } => {
                // Walking the target records a *read* of the local, and a write
                // is not a read: taking it for one would hand the binding's
                // reference to the assignment and leave the new value with
                // nothing to release it.
                let before = self.reads.len();
                self.walk(target);
                self.reads.truncate(before);
                self.walk(value);
            }
            Expr::Unary { operand, .. } => self.walk(operand),
            Expr::Field { base, .. } => self.walk(base),
            Expr::If { condition, then_branch, else_branch } => {
                self.walk(condition);
                self.walk(then_branch);
                if let Some(e) = else_branch {
                    self.walk(e);
                }
            }
            Expr::While { condition, body } => {
                // The condition runs at least once and the body may run many
                // times; both are inside the repetition as far as a last use is
                // concerned.
                self.walk(condition);
                self.walk(body);
            }
            Expr::Loop { body } => {
                self.walk(body);
            }
            Expr::Break(Some(v)) => self.walk(v),
            // A `return` leaves the frame from the middle, which makes what is
            // still owned depend on where it left from.
            Expr::Return(Some(v)) => {
                self.unwinds = true;
                self.walk(v);
            }
            Expr::Tuple(items) => {
                for item in items {
                    self.walk(item);
                }
            }
            Expr::Break(None)
            | Expr::Return(None)
            | Expr::Continue
            | Expr::Literal(_)
            | Expr::Path(_)
            | Expr::Unit
            // A closure's own name inside its body is the argument it was
            // called through, borrowed for the call. Counting it would be the
            // self-reference this design exists to avoid.
            | Expr::LambdaSelf
            | Expr::Missing
            | Expr::Unresolved(_) => {}
        }
    }
}
