//! Handing a binding's reference to its last use instead of copying it.
//!
//! A backward walk: `live` is what is still needed *after* the point being
//! looked at, and a read of a binding that is not in it takes the reference
//! rather than copying it. A branch that consumes on every path is balanced by
//! a release at the head of the arms that do not.
//!
//! Three rules earn their keep and each was found by a crash rather than by
//! thinking about it — a `match` arm's bindings own nothing, only an arm that
//! never mentions a binding may release at its head, and a binding an arm
//! introduces itself is not the branch's to settle. `docs/design/reuse.md` §1.

use super::*;

/// How many turns of a loop to walk before giving up on a fixed point.
///
/// Liveness settles in two turns for a loop that carries nothing and three for
/// most that do, so this is slack rather than a budget. It is a bound rather
/// than a `while` because the cost compounds through nesting -- an inner loop
/// is re-walked once per turn of the outer one -- and a body that would take
/// many turns is one where the conservative answer is close to the true one
/// anyway.
const TURNS: usize = 8;

impl<'a> Planner<'a> {
    /// Hands a binding's reference to its last use instead of copying it.
    ///
    /// The conservative scheme gives every read its own reference and releases
    /// the binding where its block ends, so moving one value costs a `dup`, the
    /// consumer's `drop`, and the block's `drop`. Counted across a workload the
    /// ratio is stark: parsing one HTTP request performed 677 reference-count
    /// operations against 55 allocations. `docs/design/reuse.md`.
    ///
    /// This is a backward pass. Walking from the end, `live` is the set of
    /// bindings still needed *after* the point being looked at; a read of a
    /// binding that is not in it is that binding's last use, and takes the
    /// reference rather than copying it.
    ///
    /// **A body that can unwind keeps the conservative plan entirely.** A `!`,
    /// a `raise`, a `catch` or a `return` leaves a frame from the middle, and
    /// what is still owned there depends on how far execution got — the code
    /// generator's cleanup stack is positional, so it can only be right if
    /// nothing between two points changes what is owned. Making that set
    /// path-dependent is the rest of `reuse.md` §1 and is not attempted here.
    pub(super) fn settle_last_uses(&mut self) {
        self.plan.unwinds = self.unwinds;
        let Some(root) = self.body.root else { return };
        self.unowned = self.projected_bindings(root);
        self.unowned.extend(self.forwarded_capabilities());
        self.live_before(root, &Live::new());

        // Whoever took the reference releases it, so a binding that was taken
        // is struck from the release lists. This has to sweep every list rather
        // than only the declaring block's: a `match` arm's bindings are
        // registered against the arm, and a parameter against the outermost
        // block, so a per-block sweep leaves those to be released twice.
        //
        // **Not in a body that can unwind.** There the take is a fact about a
        // point in the program rather than about the binding: before it the
        // block still owns the reference and a `raise` passing through has to
        // release it, after it the block does not. Striking the binding here
        // would leak on every early path, and leaving it would double-release
        // on the ordinary one. So the block keeps its release and the code
        // generator clears the slot at the take, which makes "has this been
        // handed on" a question the slot answers rather than one the lowering
        // position has to.
        if !self.unwinds {
            let taken = self.plan.moved.clone();
            for releases in self.plan.drops.values_mut() {
                releases.retain(|local| !taken.contains(local));
            }
        }
        self.plan.drops.retain(|_, releases| !releases.is_empty());
    }

    /// What is live *before* `id`, given what is live after it.
    ///
    /// Every read encountered is decided on the way past: kept as a copy if the
    /// binding is needed later, turned into a take if it is not.
    pub(super) fn live_before(&mut self, id: ExprId, after: &Live) -> Live {
        match self.body.expr(id).clone() {
            Expr::Local(local) => {
                let mut live = after.clone();
                if self.plan.boxed.contains(&local) {
                    if self.plan.borrowed.contains(&id) {
                        // A borrow takes no reference and ends nothing.
                    } else if self.unowned.contains(&local) {
                        // Nothing here to hand over; the copy is the reference.
                    } else if after.contains(&local) {
                        // Needed later, so this read needs one of its own.
                    } else {
                        self.plan.dups.remove(&id);
                        self.plan.takes.insert(id);
                        self.plan.moved.insert(local);
                    }
                    live.insert(local);
                }
                live
            }

            Expr::Block { stmts, tail } => {
                let mut live = match tail {
                    Some(tail) => self.live_before(tail, after),
                    None => after.clone(),
                };
                for stmt in stmts.iter().rev() {
                    match stmt {
                        Stmt::Expr(e) => live = self.live_before(*e, &live),
                        Stmt::Let { pat, init, .. } => {
                            // Backwards, a binding goes out of scope here.
                            for local in self.bound_by(*pat) {
                                live.remove(&local);
                            }
                            if let Some(init) = init {
                                live = self.live_before(*init, &live);
                            }
                        }
                    }
                }
                live
            }

            // **The right-hand side of `&&` and `||` may not run**, so nothing
            // in it can be anybody's last use. Same shape as a branch with an
            // arm that does nothing, and not worth the machinery.
            Expr::Binary { op: BinOp::And | BinOp::Or, lhs, rhs } => {
                let mut live = after.clone();
                live.extend(self.reads_in(rhs));
                self.live_before(lhs, &live)
            }
            Expr::Binary { lhs, rhs, .. } => {
                let live = self.live_before(rhs, after);
                self.live_before(lhs, &live)
            }

            Expr::If { condition, then_branch, else_branch } => {
                let Some(otherwise) = else_branch else {
                    // No `else` is an arm with nothing in it to hold a release.
                    let mut live = after.clone();
                    live.extend(self.reads_in(then_branch));
                    return self.live_before(condition, &live);
                };
                let live = self.across_arms(&[then_branch, otherwise], &[], after);
                self.live_before(condition, &live)
            }

            Expr::Match { scrutinee, arms } => {
                let bodies: Vec<ExprId> = arms.iter().map(|arm| arm.body).collect();
                let bound: Vec<LocalId> =
                    arms.iter().flat_map(|arm| self.bound_by(arm.pat)).collect();
                let mut live = self.across_arms(&bodies, &bound, after);
                // **A guard is a read even though this pass does not walk into
                // one.** A guard runs before its arm and may not run at all, so
                // nothing in it can be a last use and its copies stand — but
                // something earlier must not hand the binding away underneath
                // it. Leaving them out let `let t = s + ""` take `s` and the
                // guard then read the freed object.
                for arm in &arms {
                    if let Some(guard) = arm.guard {
                        live.extend(self.reads_in(guard));
                    }
                }
                self.live_before(scrutinee, &live)
            }

            // **A loop's liveness is a fixed point, not a set of reads.**
            //
            // What is live at the end of one turn is what the next turn reads,
            // which is what is live at the start of a turn — so the answer
            // defines itself and has to be found by iterating. Every read in
            // the body used to stand in for it, which is sound and says that
            // nothing in any loop is ever a last use. That is the whole of why
            // `for x in xs` allocates: the `Step` is built from a cell whose
            // reference was copied on the way in, so `khora_drop_reuse` finds
            // it shared, hands back no token, and the constructor allocates.
            // The same walk written as recursion allocates nothing.
            //
            // Started from what is live after the loop and grown, so it is
            // reached from below: every intermediate answer is *smaller* than
            // the truth, and the loop stops the first time a turn adds
            // nothing. Bounded by the body's bindings, so it terminates.
            //
            // Over-approximating here costs a copy nobody needed.
            // Under-approximating frees something still in use, so where this
            // does not settle it falls back to the reads, which is the old
            // answer and the safe one.
            //
            // **The trial turns must not record.** `live_before` decides each
            // read as it passes and writes that into the plan, so a turn run
            // against a set that later grows leaves behind a take the grown set
            // forbids. The plan is put back between turns, and the walk that
            // records is the last one, over the settled set.
            Expr::Loop { body } => {
                if self.holds_a_continue(body) {
                    let mut live = after.clone();
                    live.extend(self.reads_in(body));
                    return live;
                }
                let saved = self.plan.clone();
                let mut live = after.clone();
                let mut settled = false;
                for _ in 0..TURNS {
                    self.loop_exits.push(after.clone());
                    let mut turn = self.live_before(body, &live);
                    self.loop_exits.pop();
                    self.plan = saved.clone();
                    turn.extend(after.iter().copied());
                    if turn.is_subset(&live) {
                        settled = true;
                        break;
                    }
                    live.extend(turn);
                }
                if !settled {
                    live.extend(self.reads_in(body));
                    return live;
                }
                self.loop_exits.push(after.clone());
                let settled = self.live_before(body, &live);
                self.loop_exits.pop();
                settled
            }
            // As `Loop`, with the condition read on the way into every turn and
            // once more on the way out.
            Expr::While { condition, body } => {
                if self.holds_a_continue(body) {
                    let mut live = after.clone();
                    live.extend(self.reads_in(condition));
                    live.extend(self.reads_in(body));
                    return live;
                }
                let saved = self.plan.clone();
                let mut live = after.clone();
                let mut settled = false;
                for _ in 0..TURNS {
                    self.loop_exits.push(after.clone());
                    let inside = self.leaving_or_turning_again(body, &live, after);
                    let mut turn = self.live_before(condition, &inside);
                    self.loop_exits.pop();
                    self.plan = saved.clone();
                    turn.extend(after.iter().copied());
                    if turn.is_subset(&live) {
                        settled = true;
                        break;
                    }
                    live.extend(turn);
                }
                if !settled {
                    live.extend(self.reads_in(condition));
                    live.extend(self.reads_in(body));
                    return live;
                }
                self.loop_exits.push(after.clone());
                let inside = self.leaving_or_turning_again(body, &live, after);
                let settled = self.live_before(condition, &inside);
                self.loop_exits.pop();
                settled
            }

            // A closure's body runs when it is called, which is not here.
            Expr::Lambda { captures, body, .. } => {
                let mut live = after.clone();
                live.extend(captures.iter().filter(|c| self.plan.boxed.contains(c)));
                live.extend(self.reads_in(body));
                live
            }

            // A write is not a read, and the value written is evaluated first.
            //
            // **Assigning a binding ends what it was holding.** The assignment
            // drops the old value itself, so nothing after this point can want
            // it and the read that handed it on may be a last use. Leaving it
            // live is what kept `for x in xs` allocating: the cell reaches
            // `khora_drop_reuse` with the loop's copy still outstanding, no
            // token comes back, and the `Step` is allocated instead of built
            // where the `Cons` was.
            //
            // Only a whole binding is ended. `s.f = v` reads `s` to find the
            // field and leaves it holding everything else.
            Expr::Assign { target, value } => {
                let mut live = after.clone();
                match self.body.expr(target).clone() {
                    Expr::Local(local) => {
                        live.remove(&local);
                    }
                    _ => live.extend(self.reads_in(target)),
                }
                self.live_before(value, &live)
            }

            Expr::Call { callee, args } => {
                let mut live = after.clone();
                for arg in args.iter().rev() {
                    live = self.live_before(*arg, &live);
                }
                self.live_before(callee, &live)
            }
            Expr::Record { fields, base, .. } => {
                let mut live = after.clone();
                for (_, value) in fields.iter().rev() {
                    live = self.live_before(*value, &live);
                }
                // In reverse of evaluation order, and the base is evaluated
                // first, so it is considered last.
                match base {
                    Some(base) => self.live_before(base, &live),
                    None => live,
                }
            }
            Expr::Tuple(items) => {
                let mut live = after.clone();
                for item in items.iter().rev() {
                    live = self.live_before(*item, &live);
                }
                live
            }
            Expr::Field { base, .. } => self.live_before(base, after),
            Expr::Unary { operand, .. } => self.live_before(operand, after),
            // **A `${..}` hole reads its value, and this pass has to see it.**
            //
            // It lowers to a one-argument call to `Show::show`, so it is the
            // same shape as any other single child. Falling into the catch-all
            // below made the read *invisible to liveness*: a binding whose only
            // later use was inside a hole looked dead at the use before it, so
            // that earlier use took the reference, and the hole then read freed
            // memory.
            //
            // Eleven lines were enough — a two-field record, `let first = p.x;`
            // and then `print("second ${p.y}")` — for a program that type-checks
            // to exit with `STATUS_HEAP_CORRUPTION`. It also showed up as a
            // segfault, as a misaligned pointer inside the allocator, and worst
            // of all as `List::length` of a two-element list answering `1` with
            // exit 0. Three of four people writing their first Khora program hit
            // it, and two adopted "never call a function inside a hole" as a
            // house rule for the rest of the day.
            //
            // The shape of the bug is why it survived: with *no* earlier use
            // both reads happen before the block's release and everything works,
            // and with a *later* use the binding stays live and everything
            // works. Only the exact middle case is wrong.
            Expr::Shown(inner) => self.live_before(inner, after),
            // **A `break` leaves for after the loop, not for the next turn.**
            // What is live where it lands is the enclosing loop's own `after`,
            // and reading the ambient set instead would claim everything the
            // body reads is still wanted -- which makes a binding the body
            // reassigns every turn live across its own read, and so never
            // handed over. That is the difference between a loop that reuses
            // the cell it matched and one that allocates.
            Expr::Break(value) => {
                let exit = self.loop_exits.last().cloned().unwrap_or_else(|| after.clone());
                match value {
                    Some(v) => self.live_before(v, &exit),
                    None => exit,
                }
            }

            // Unreachable while `unwinds` guards this pass, and conservative if
            // that ever changes.
            Expr::Raise(_) | Expr::Try(_) | Expr::Return(_) | Expr::Catch { .. } => {
                let mut live = after.clone();
                live.extend(self.reads_in(id));
                live
            }

            _ => after.clone(),
        }
    }

    /// What a `while`'s condition is standing in front of.
    ///
    /// **A `while` leaves through its condition**, so what is live after the
    /// loop is live at the test as surely as what the next turn reads is.
    /// Walking the body and handing the condition only *that* says the exit
    /// path wants nothing, and a binding the body reassigns each turn is then
    /// dead at the test -- handed over there, and read again by whatever the
    /// loop was computing it for.
    ///
    /// `fresh_code` in the link shortener is the shape:
    ///
    /// ```khora
    /// let mut code = draw_code();
    /// while Dict::contains(taken, code) && tries < 10 { code = draw_code(); .. };
    /// code
    /// ```
    ///
    /// The condition took `code`, the loop then ended, and the function
    /// returned the slot it had just cleared. A `loop` does not need this: it
    /// leaves through `break`, which reads `loop_exits` for the same set.
    fn leaving_or_turning_again(&mut self, body: ExprId, back: &Live, after: &Live) -> Live {
        let mut live = self.live_before(body, back);
        live.extend(after.iter().copied());
        live
    }

    /// Whether anything inside `id` jumps back to a loop head.
    ///
    /// A `continue` goes to the back edge, and what is live there is the whole
    /// of what the next turn reads -- not what follows the `continue` in its
    /// block, which is nothing. Threading the back edge's set to it needs the
    /// same stack `loop_exits` is, and until something wants it a loop holding
    /// one keeps the conservative answer instead: sound, and `std` has no
    /// `continue` in it to lose anything by.
    ///
    /// A `continue` belonging to a *nested* loop disqualifies the outer one
    /// too. Also conservative, and it saves asking which loop each one means.
    pub(super) fn holds_a_continue(&self, id: ExprId) -> bool {
        if matches!(self.body.expr(id), Expr::Continue) {
            return true;
        }
        let mut found = false;
        self.each_child(id, &mut |child| found = found || self.holds_a_continue(child));
        found
    }

    /// The arms of a branch, and the releases the ones that do not consume owe.
    ///
    /// A branch consumes a binding only when *every* path through it does. Where
    /// one arm takes a binding and another never mentions it, the second arm
    /// releases it at its head. Where another arm merely reads it, the branch
    /// consumes nothing — releasing at the head would free a value that arm is
    /// about to use, and releasing at the end is what the block already does.
    pub(super) fn across_arms(&mut self, arms: &[ExprId], arm_bound: &[LocalId], after: &Live) -> Live {
        if self.unwinds {
            let mut live = Live::new();
            for arm in arms {
                live.extend(self.reads_in(*arm));
            }
            live.extend(after.iter().copied());
            return live;
        }
        let before: Vec<Live> = arms
            .iter()
            .map(|arm| {
                let taken_before = self.plan.moved.clone();
                let live = self.live_before(*arm, after);
                let _ = taken_before;
                live
            })
            .collect();

        // What each arm did with each binding, worked out from the reads it
        // holds rather than from the pass above — the pass shares one `moved`
        // set across arms and cannot say which arm did the taking.
        let mut consumed: Vec<LocalId> = Vec::new();
        let uses: Vec<Live> = arms.iter().map(|arm| self.reads_in(*arm)).collect();
        let takes: Vec<Live> = arms.iter().map(|arm| self.takes_in(*arm)).collect();

        // Only a binding that outlives the branch can be settled by it. One an
        // arm introduces itself — through its pattern, or a `let` inside it —
        // does not exist in the other arms, and a release at their head would
        // be reading a slot that was never written on that path. Such a
        // binding is taken and released entirely within its own arm, which
        // needs nothing from here.
        let mut inside: Live = arm_bound.iter().copied().collect();
        for arm in arms {
            inside.extend(self.bindings_in(*arm));
        }
        let mut candidates: Live = Live::new();
        for take in &takes {
            candidates.extend(take.iter().copied());
        }
        candidates.retain(|local| !inside.contains(local));
        for local in candidates {
            // **A branch can only settle a binding that is dead after it.**
            // Releasing at the head of the arms that did not take it is right
            // when control leaves the branch afterwards, and wrong the moment
            // it can come back. A loop's `match` with one arm that breaks out
            // holding the binding and one that falls through to the back edge
            // would release it in the arm that is about to go round and read it
            // again.
            //
            // `Filtered::next` is that shape exactly -- `break Step::Yield({
            // inner: rest, keep: self.keep }, item)` against `cur = rest` --
            // and it took `std`'s own pipeline off the stack. Nothing had
            // reached it before because no take was ever recorded inside a
            // loop body, so no branch inside one ever had anything to settle.
            //
            // Every arm either takes it, or does not touch it at all.
            let settled = !after.contains(&local)
                && takes
                    .iter()
                    .zip(&uses)
                    .all(|(take, use_)| take.contains(&local) || !use_.contains(&local));
            if !settled {
                // Some arm reads it without taking it. Put the copies back and
                // leave the binding to its block.
                for arm in arms {
                    self.restore_dups(*arm, local);
                }
                self.plan.moved.remove(&local);
                continue;
            }
            for (arm, take) in arms.iter().zip(&takes) {
                if !take.contains(&local) {
                    self.plan.arm_drops.entry(*arm).or_default().push(local);
                }
            }
            consumed.push(local);
        }

        // **What is live before a branch is what its arms want, and nothing
        // else.** Each arm was walked against `after`, so a binding the branch
        // does not settle is already carried out by whichever arm still wants
        // it; one that every arm consumes is put back by `consumed`, because it
        // has to be live on the way in to be consumed at all.
        //
        // Adding `after` wholesale on top of that was the conservative answer
        // and it costs the case this pass exists for: a binding every arm
        // *ends* -- by assigning it, the way a loop reassigns the cursor it
        // walks -- came back live, so the read that fed the branch could never
        // be its last use. That is `for x in xs` allocating a `Step` per
        // element where the same walk written as recursion allocates none.
        let mut live = Live::new();
        for arm in before {
            live.extend(arm);
        }
        for local in consumed {
            live.insert(local);
        }
        live
    }
}
