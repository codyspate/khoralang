//! Reference counting: where `dup` and `drop` go.
//!
//! Roadmap phase 2.3. This inserts *correct* reference counting, not yet
//! *minimal* reference counting. Perceus proper earns its name by removing
//! pairs that provably cancel and by fusing a `drop` with a following
//! allocation into an in-place `reuse` — that is phase 9 (FBIP), and it is an
//! optimization over exactly this output. Getting the conservative version
//! right first means phase 9 has something to prove itself against.
//!
//! # What phase 9 has to change here, and why it is a rewrite
//!
//! The scheme below owns a value for the whole of a binding's scope: a read
//! `dup`s, and the block releases what it declared on the way out. Reuse needs
//! the opposite — the *last* use of a value, so that the object is uniquely
//! held at the point an arm allocates a new one and its memory can be handed
//! straight over.
//!
//! Concretely, `match xs { List::Cons(h, t) => List::Cons(f(h), map(t)) }`
//! cannot reuse anything today, and not because the fusion is missing: at the
//! constructor, `xs` is still held by its binding *and* by the dup the read
//! made, so a uniqueness test sees two references and correctly declines. The
//! fusion is the easy half. Moving the release to the last use, on every path,
//! is the analysis, and it is the part that turns a wrong answer into a double
//! free rather than a slow program.
//!
//! `settle_last_uses` is that analysis as far as it goes: a backward liveness
//! pass over a body that cannot unwind, which turns a read the binding does not
//! outlive into a take, and balances a branch that takes on one path with a
//! release at the head of the arms that do not. What it does not do is the
//! paths that leave a frame early — `raise`, `!`, `catch`, `return` — because
//! the code generator's cleanup stack is positional and cannot describe a set
//! of live values that depends on how far execution got.
//!
//! `docs/design/reuse.md` has the design.
//!
//! # The scheme
//!
//! Only *boxed* values are counted: `Int` and `Bool` are machine words with
//! nothing to own. Strings and ADTs live behind the header in `khora-rt`.
//!
//! - A local holding a boxed value **owns** one reference.
//! - Reading such a local yields a value that outlives the read, so the read
//!   `dup`s — unless the binding is not needed afterwards, in which case the
//!   read takes the binding's own reference and nothing is copied.
//! - A block `drop`s every boxed local it declared and nothing took, on the way
//!   out.
//! - Parameters are owned by the callee, so they are dropped like locals.
//!
//! # Why the scheme balances
//!
//! Worth spelling out, because it is not obvious: a read `dup`s, and the callee
//! that receives the value drops it as an owned parameter, so a call is
//! neutral. `let t = s; t` allocates once, dups twice and drops twice, leaving
//! the single reference the caller receives. Construction yields one reference,
//! and the block that binds it releases it.
//!
//! The one thing outside that is a boxed value produced in statement position
//! and never bound — `Shape::Circle(4);` — because this plan records releases
//! for *bindings* and there is no binding. It does not leak: code generation
//! drops the value of a discarded statement expression itself, at `Stmt::Expr`
//! in `lower.rs`. A note here used to call it an open leak, which it has not
//! been for some time.
//!
//! # The interface
//!
//! The output is a side table keyed by [`ExprId`] and [`LocalId`] rather than a
//! new IR. Code generation walks the same HIR the type checker did and consults
//! this as it goes, so there is no third representation to keep in step — which
//! matters most while the three passes are all still moving.

use std::collections::{HashMap, HashSet};

use khora_db::{Db, SourceFile};
use khora_hir::body::{BinOp, Body, Expr, ExprId, LocalId, Pat, PatId, Stmt};
use khora_types::Type;

/// Where reference-counting operations belong in one function body.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RcPlan {
    /// Local reads whose result must be `dup`ed, by the id of the reading
    /// expression.
    pub dups: HashSet<ExprId>,
    /// Locals to `drop` when a block exits, keyed by the block's id.
    pub drops: HashMap<ExprId, Vec<LocalId>>,
    /// Locals holding a boxed value. Everything else is a machine word.
    pub boxed: HashSet<LocalId>,
    /// Argument expressions passed as a *borrow*: no reference was made for
    /// them, and the callee must not release one.
    ///
    /// The backend reads this. A borrowing intrinsic used to be handed an owned
    /// reference and immediately drop it — a `dup` and a `drop` that cancel,
    /// two atomic operations to pass a value the callee only looks at.
    pub borrowed: HashSet<ExprId>,
    /// Locals to release at the *head* of a branch arm, keyed by the arm's body.
    ///
    /// A branch consumes a binding when every path through it does. Where one
    /// arm takes the reference and another never mentions the binding at all,
    /// the second arm has to release it, and the head of that arm is the only
    /// place that is on exactly the paths that need it.
    ///
    /// An arm that merely *reads* the binding is not given one — releasing at
    /// the head would free something the arm is about to use. Such a branch
    /// does not consume the binding at all, and its block releases it as
    /// before.
    pub arm_drops: HashMap<ExprId, Vec<LocalId>>,
    /// Locals whose reference was handed to their last read rather than
    /// released by their block.
    ///
    /// Recorded so the invariant stays checkable. It used to be "every counted
    /// local is released exactly once"; it is now "released exactly once, or
    /// moved exactly once", and without this there would be no way to tell a
    /// moved local from one somebody forgot.
    pub moved: HashSet<LocalId>,
}

impl RcPlan {
    pub fn needs_dup(&self, expr: ExprId) -> bool {
        self.dups.contains(&expr)
    }

    pub fn drops_for(&self, block: ExprId) -> &[LocalId] {
        self.drops.get(&block).map(|v| v.as_slice()).unwrap_or(&[])
    }

    /// Locals to release on entering this branch arm.
    pub fn arm_drops_for(&self, arm: ExprId) -> &[LocalId] {
        self.arm_drops.get(&arm).map(|v| v.as_slice()).unwrap_or(&[])
    }

    pub fn is_boxed(&self, local: LocalId) -> bool {
        self.boxed.contains(&local)
    }
}

/// Which of a call's arguments it only looks at.
///
/// **These are the calls that were already borrowing and saying otherwise.**
/// The runtime does not keep the region a finalizer is deferred into, the cell
/// a `Shared` operation reads, or the handle a fiber is joined through — but
/// the reference-counting plan read each as the ordinary owning call it is
/// written as, so the caller made a reference and the callee released it. Two
/// atomic operations, cancelling.
///
/// Saying so has a second effect that matters more than the two operations. A
/// borrowed argument is not a *use* that could be somebody's last one, so a
/// binding passed to `Region::defer` keeps its reference — which is what makes
/// its finalizers run when the region's scope ends rather than inside `defer`.
/// Without this the last-use analysis had to be restricted to `String` to avoid
/// reordering a program's output. `docs/design/reuse.md`.
///
/// Indices are into the argument list, receiver first.
///
/// **Only bodyless declarations may appear here**, and the distinction is not
/// cosmetic: a function written in Khora owns its parameters and releases them,
/// so promising a caller a borrow of one is a use after free. `Array::prefix`
/// and `String::matches_at` are written in Khora and were briefly on this list.
/// Deciding it for an ordinary function needs an escape analysis rather than a
/// table.
pub fn borrowed_arguments(owner: &str, method: &str) -> &'static [usize] {
    const RECEIVER: &[usize] = &[0];
    const NONE: &[usize] = &[];
    match (owner, method) {
        // The runtime keeps the *finalizer* and only looks at the region.
        ("Region", "defer") => RECEIVER,
        // A cell is read or written through; the handle stays the caller's.
        ("Shared", "get" | "set" | "update" | "modify") => RECEIVER,
        // Joining or cancelling looks at a handle. *Releasing* one is what
        // joins, and that is the binding's business rather than the call's.
        ("Fiber", "join" | "cancel") => RECEIVER,
        // A nursery adopts the *fiber*; the nursery itself is borrowed.
        ("Fibers", "adopt" | "wait") => RECEIVER,

        // **The ones that pay.** A `String` or an `Array` intrinsic reads
        // through its receiver and hands back a number, a byte or a new object;
        // none of them keeps it. Unlike a last use, a borrow applies inside a
        // loop, and that is where these live: `lowered_between` calls
        // `String::byte` once per character, and every call was making a
        // reference to the whole string and releasing it again.
        ("String", "byte" | "byte_length" | "bytes" | "slice" | "find") => RECEIVER,
        ("Array", "get" | "set" | "length" | "is_utf8") => RECEIVER,
        _ => NONE,
    }
}

/// The name a method on this type is looked up under.
fn owner_of(ty: &Type) -> Option<&str> {
    match ty {
        Type::Str => Some("String"),
        Type::Adt { name, .. } => Some(name),
        _ => None,
    }
}

/// Whether values of this type carry a reference count.
///
/// `Unknown` counts as unboxed: it only appears downstream of an error, and a
/// spurious `drop` on a machine word would be a wild free.
pub fn is_boxed(ty: &Type) -> bool {
    // A closure is an ordinary heap object: a function pointer and whatever it
    // captured, under the same header as everything else.
    matches!(ty, Type::Str | Type::Adt { .. } | Type::Fn { .. })
}

/// Plans reference counting for one body at one set of types.
///
/// **Takes the types rather than deriving them**, because whether a value is
/// boxed depends on the *instantiation*: `A` in `fn id<A>` is a rigid parameter
/// and never boxed, while the same body compiled at `A = List<Int>` holds a
/// pointer that has to be counted. A plan made once from the generic body is
/// wrong for every instantiation that fills a parameter with something boxed —
/// see `docs/errata.md`, entry 24.
pub fn plan(body: &Body, types: &khora_types::BodyTypes) -> RcPlan {
    let mut planner = Planner {
        body,
        plan: RcPlan::default(),
        types,
        reads: Vec::new(),
        unowned: HashSet::new(),
        unwinds: false,
    };
    planner.plan_function();
    planner.settle_last_uses();
    planner.plan
}

/// Plans reference counting for every function body in a file, at the types
/// the body was *written* at.
///
/// Good enough for a non-generic function, and what the tests read. Code
/// generation calls [`plan`] once per specialization instead.
#[salsa::tracked(returns(ref))]
pub fn rc_plans(db: &dyn Db, file: SourceFile) -> Vec<(String, RcPlan)> {
    let checked = khora_types::checked(db, file);
    let empty = khora_types::BodyTypes::default();
    khora_hir::body::bodies(db, file)
        .iter()
        .map(|(name, body)| {
            // The checker already worked out every type in this body and zonked
            // them. Re-deriving them here from the shape of the expressions was
            // wrong in exactly the cases that matter: it had no idea what a
            // lambda's type was, so a closure was never counted, and a boxed
            // value passed to one was freed twice.
            let body_types =
                checked.bodies.iter().find(|(n, _)| n == name).map(|(_, t)| t).unwrap_or(&empty);
            (name.clone(), plan(body, body_types))
        })
        .collect()
}

/// Bindings still needed at a point in the backward pass.
type Live = HashSet<LocalId>;

/// One read of a counted binding, as the walk saw it.
struct Read {
    local: LocalId,
    at: ExprId,
    /// The callee only looks at it, so no reference was made for this read.
    ///
    /// **Still a use.** Leaving borrowed reads out of this list entirely was a
    /// use-after-free: `f(s)` followed by `String::byte(s, 0)` moved the
    /// binding into `f`, freed it there, and then read the bytes of the freed
    /// object — because the borrow was invisible to the question "which read is
    /// last". A borrow cannot *take* ownership, but it can certainly come after
    /// the read that would have.
    borrowed: bool,
}

struct Planner<'a> {
    body: &'a Body,
    plan: RcPlan,
    types: &'a khora_types::BodyTypes,
    /// Every read of a boxed local, in program order.
    reads: Vec<Read>,
    /// Bindings that hold a reference belonging to somebody else.
    ///
    /// A `match` arm's bindings are projections of the scrutinee's payload: the
    /// arm never made a reference for them, which is why no block releases one
    /// — see `match_arm_bindings_are_not_released_by_the_arm`. Reading one has
    /// to copy, always, because there is no reference there to hand over.
    unowned: HashSet<LocalId>,
    /// Whether this body can leave a frame early.
    ///
    /// A `!` or a `raise` unwinds, and unwinding releases what the frame's
    /// blocks declared. Moving a reference out of a binding makes that set
    /// depend on how far execution got, which is the hard half of
    /// `docs/design/reuse.md` §1 and is not attempted here — so a body that
    /// can unwind keeps the conservative plan entirely.
    unwinds: bool,
}

impl<'a> Planner<'a> {
    fn plan_function(&mut self) {
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
    fn settle_last_uses(&mut self) {
        if self.unwinds {
            return;
        }
        let Some(root) = self.body.root else { return };
        self.unowned = self.projected_bindings(root);
        self.live_before(root, &Live::new());

        // Whoever took the reference releases it. This has to sweep every list
        // rather than only the declaring block's: a `match` arm's bindings are
        // registered against the arm, and a parameter against the outermost
        // block, so a per-block sweep leaves those to be released twice.
        let taken = self.plan.moved.clone();
        for releases in self.plan.drops.values_mut() {
            releases.retain(|local| !taken.contains(local));
        }
        self.plan.drops.retain(|_, releases| !releases.is_empty());
    }

    /// What is live *before* `id`, given what is live after it.
    ///
    /// Every read encountered is decided on the way past: kept as a copy if the
    /// binding is needed later, turned into a take if it is not.
    fn live_before(&mut self, id: ExprId, after: &Live) -> Live {
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

            // A loop's body may run many times, so a read in it is never a last
            // use — the next turn may want the value again.
            Expr::While { condition, body } => {
                let mut live = after.clone();
                live.extend(self.reads_in(condition));
                live.extend(self.reads_in(body));
                live
            }
            Expr::Loop { body } => {
                let mut live = after.clone();
                live.extend(self.reads_in(body));
                live
            }

            // A closure's body runs when it is called, which is not here.
            Expr::Lambda { captures, body, .. } => {
                let mut live = after.clone();
                live.extend(captures.iter().filter(|c| self.plan.boxed.contains(c)));
                live.extend(self.reads_in(body));
                live
            }

            // A write is not a read, and the value written is evaluated first.
            Expr::Assign { target, value } => {
                let mut live = after.clone();
                live.extend(self.reads_in(target));
                self.live_before(value, &live)
            }

            Expr::Call { callee, args } => {
                let mut live = after.clone();
                for arg in args.iter().rev() {
                    live = self.live_before(*arg, &live);
                }
                self.live_before(callee, &live)
            }
            Expr::Record { fields, .. } => {
                let mut live = after.clone();
                for (_, value) in fields.iter().rev() {
                    live = self.live_before(*value, &live);
                }
                live
            }
            Expr::List(items) | Expr::Tuple(items) => {
                let mut live = after.clone();
                for item in items.iter().rev() {
                    live = self.live_before(*item, &live);
                }
                live
            }
            Expr::Field { base, .. } => self.live_before(base, after),
            Expr::Unary { operand, .. } => self.live_before(operand, after),
            Expr::Break(Some(v)) => self.live_before(v, after),

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

    /// The arms of a branch, and the releases the ones that do not consume owe.
    ///
    /// A branch consumes a binding only when *every* path through it does. Where
    /// one arm takes a binding and another never mentions it, the second arm
    /// releases it at its head. Where another arm merely reads it, the branch
    /// consumes nothing — releasing at the head would free a value that arm is
    /// about to use, and releasing at the end is what the block already does.
    fn across_arms(&mut self, arms: &[ExprId], arm_bound: &[LocalId], after: &Live) -> Live {
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
            // Every arm either takes it, or does not touch it at all.
            let settled = takes
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

        let mut live = Live::new();
        for arm in before {
            live.extend(arm);
        }
        for local in consumed {
            live.insert(local);
        }
        live.extend(after.iter().copied());
        live
    }

    /// Every binding a `match` or `catch` arm projects out of its scrutinee.
    fn projected_bindings(&self, id: ExprId) -> Live {
        let mut found = Live::new();
        self.collect_projected(id, &mut found);
        found
    }

    fn collect_projected(&self, id: ExprId, found: &mut Live) {
        if let Expr::Match { arms, .. } | Expr::Catch { arms, .. } = self.body.expr(id) {
            for arm in arms {
                found.extend(self.bound_by(arm.pat));
            }
        }
        self.each_child(id, &mut |child| self.collect_projected(child, found));
    }

    /// Every binding introduced anywhere inside `id`.
    fn bindings_in(&self, id: ExprId) -> Live {
        let mut found = Live::new();
        self.collect_bindings(id, &mut found);
        found
    }

    fn collect_bindings(&self, id: ExprId, found: &mut Live) {
        match self.body.expr(id) {
            Expr::Block { stmts, .. } => {
                for stmt in stmts {
                    if let Stmt::Let { pat, .. } = stmt {
                        found.extend(self.bound_by(*pat));
                    }
                }
            }
            Expr::Match { arms, .. } | Expr::Catch { arms, .. } => {
                for arm in arms {
                    found.extend(self.bound_by(arm.pat));
                }
            }
            Expr::Lambda { params, .. } => {
                for param in params.clone() {
                    found.extend(self.bound_by(param));
                }
            }
            _ => {}
        }
        self.each_child(id, &mut |child| self.collect_bindings(child, found));
    }

    /// Every boxed binding read anywhere inside `id`.
    fn reads_in(&self, id: ExprId) -> Live {
        let mut found = Live::new();
        self.collect_reads(id, &mut found);
        found
    }

    /// Every boxed binding whose reference is *taken* inside `id`.
    fn takes_in(&self, id: ExprId) -> Live {
        let mut found = Live::new();
        for read in &self.reads {
            if !self.plan.dups.contains(&read.at)
                && !read.borrowed
                && self.within(id, read.at)
            {
                found.insert(read.local);
            }
        }
        found
    }

    /// Whether `needle` is `haystack` or somewhere inside it.
    fn within(&self, haystack: ExprId, needle: ExprId) -> bool {
        if haystack == needle {
            return true;
        }
        let mut found = false;
        self.each_child(haystack, &mut |child| {
            found = found || self.within(child, needle);
        });
        found
    }

    /// Puts back the copies a take removed, for a binding a branch cannot
    /// consume after all.
    fn restore_dups(&mut self, arm: ExprId, local: LocalId) {
        let places: Vec<ExprId> = self
            .reads
            .iter()
            .filter(|read| read.local == local && !read.borrowed && self.within(arm, read.at))
            .map(|read| read.at)
            .collect();
        for at in places {
            self.plan.dups.insert(at);
        }
        // A branch nested inside may have granted an arm release for this
        // binding on the strength of a take that is now a copy again. Left in
        // place it would release something the block still releases.
        let inner: Vec<ExprId> = self
            .plan
            .arm_drops
            .keys()
            .copied()
            .filter(|at| self.within(arm, *at))
            .collect();
        for at in inner {
            if let Some(releases) = self.plan.arm_drops.get_mut(&at) {
                releases.retain(|held| *held != local);
            }
        }
        self.plan.arm_drops.retain(|_, releases| !releases.is_empty());
    }

    fn collect_reads(&self, id: ExprId, found: &mut Live) {
        if let Expr::Local(local) = self.body.expr(id) {
            if self.plan.boxed.contains(local) {
                found.insert(*local);
            }
        }
        self.each_child(id, &mut |child| self.collect_reads(child, found));
    }

    /// Every expression `id` immediately contains.
    fn each_child(&self, id: ExprId, visit: &mut dyn FnMut(ExprId)) {
        match self.body.expr(id) {
            Expr::Block { stmts, tail } => {
                for stmt in stmts {
                    match stmt {
                        Stmt::Expr(e) => visit(*e),
                        Stmt::Let { init, .. } => {
                            if let Some(init) = init {
                                visit(*init);
                            }
                        }
                    }
                }
                if let Some(tail) = tail {
                    visit(*tail);
                }
            }
            Expr::Call { callee, args } => {
                visit(*callee);
                for arg in args {
                    visit(*arg);
                }
            }
            Expr::Binary { lhs, rhs, .. } => {
                visit(*lhs);
                visit(*rhs);
            }
            Expr::Assign { target, value } => {
                visit(*target);
                visit(*value);
            }
            Expr::Unary { operand, .. } => visit(*operand),
            Expr::Field { base, .. } => visit(*base),
            Expr::If { condition, then_branch, else_branch } => {
                visit(*condition);
                visit(*then_branch);
                if let Some(otherwise) = else_branch {
                    visit(*otherwise);
                }
            }
            Expr::While { condition, body } => {
                visit(*condition);
                visit(*body);
            }
            Expr::Loop { body } => visit(*body),
            Expr::Match { scrutinee, arms } => {
                visit(*scrutinee);
                for arm in arms {
                    if let Some(guard) = arm.guard {
                        visit(guard);
                    }
                    visit(arm.body);
                }
            }
            Expr::Catch { inner, arms } => {
                visit(*inner);
                for arm in arms {
                    if let Some(guard) = arm.guard {
                        visit(guard);
                    }
                    visit(arm.body);
                }
            }
            Expr::Lambda { body, .. } => visit(*body),
            Expr::Record { fields, .. } => {
                for (_, value) in fields {
                    visit(*value);
                }
            }
            Expr::List(items) | Expr::Tuple(items) => {
                for item in items {
                    visit(*item);
                }
            }
            Expr::Raise(inner) | Expr::Try(inner) => visit(*inner),
            Expr::Break(Some(v)) | Expr::Return(Some(v)) => visit(*v),
            _ => {}
        }
    }

    /// The locals a pattern binds.
    fn bound_by(&self, pat: PatId) -> Vec<LocalId> {
        let mut found = Vec::new();
        self.gather_bound(pat, &mut found);
        found
    }

    fn gather_bound(&self, pat: PatId, found: &mut Vec<LocalId>) {
        match self.body.pat(pat) {
            Pat::Bind(local) => found.push(*local),
            Pat::TupleStruct { fields, .. } | Pat::Tuple(fields) => {
                for field in fields.clone() {
                    self.gather_bound(field, found);
                }
            }
            _ => {}
        }
    }

    /// Records a read that takes no reference of its own.
    fn borrow(&mut self, at: ExprId) {
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

    /// The argument positions this callee only looks at.
    fn lent_by(&self, callee: ExprId) -> Vec<usize> {
        match self.body.expr(callee) {
            Expr::Path(khora_hir::Resolution::TraitItem { owner, name }) => {
                borrowed_arguments(owner, name).to_vec()
            }
            _ => Vec::new(),
        }
    }

    /// Records a pattern's bindings and collects the boxed ones.
    fn bind(&mut self, pat: PatId, owned: &mut Vec<LocalId>) {
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

    fn walk(&mut self, id: ExprId) {
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
                    // Arm bindings borrow out of the scrutinee, which the arm
                    // does not own, so they are recorded but not dropped.
                    let mut ignored = Vec::new();
                    self.bind(arm.pat, &mut ignored);
                    if let Some(guard) = arm.guard {
                        self.walk(guard);
                    }
                    self.walk(arm.body);
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
                    .is_some_and(|owner| borrowed_arguments(owner, &name).contains(&0));
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
            Expr::List(items) | Expr::Tuple(items) => {
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
