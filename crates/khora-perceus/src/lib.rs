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
//! `docs/design/reuse.md` has the design.
//!
//! # The scheme
//!
//! Only *boxed* values are counted: `Int` and `Bool` are machine words with
//! nothing to own. Strings and ADTs live behind the header in `khora-rt`.
//!
//! - A local holding a boxed value **owns** one reference.
//! - Reading such a local yields a value that outlives the read, so the read
//!   `dup`s.
//! - A block `drop`s every boxed local it declared, on the way out.
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
use khora_hir::body::{Body, Expr, ExprId, LocalId, Pat, PatId, Stmt};
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

    pub fn is_boxed(&self, local: LocalId) -> bool {
        self.boxed.contains(&local)
    }
}

/// Whether releasing a value of this type can be *observed*.
///
/// **Freeing earlier is only invisible when freeing does nothing but free.**
/// `docs/design/compatibility.md` says when memory is released is not something
/// a program may depend on, and that is true of memory. It is not true of a
/// `Region`, whose release runs the finalizers deferred into it — user code,
/// with output and effects of its own.
///
/// Found by a test rather than by reasoning:
///
/// ```khora
/// let region = Region::open();
/// Region::defer(region, fn () => print(2));
/// print(1);
/// ```
///
/// The last read of `region` is the `defer`, so moving its reference there
/// released the region on that line and printed `2` before `1`. Not a leak, not
/// a double free — a different program.
///
/// So this is deliberately narrow: a `String` and nothing else. A `String`
/// holds bytes, its release frees them, and there is nothing it can reach. An
/// ADT could hold a `Region` in a field, a closure could capture one, and
/// deciding *which* of those are quiet needs the field types this pass is not
/// given. Widening it is worth doing and wants the question answered first:
/// whether a region's end is its scope or its last reference is a language
/// decision, not an optimizer's.
fn quietly_dropped(ty: &Type) -> bool {
    matches!(ty, Type::Str)
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
        branching: 0,
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

struct Planner<'a> {
    body: &'a Body,
    plan: RcPlan,
    types: &'a khora_types::BodyTypes,
    /// Every read of a boxed local, in program order, with whether it happened
    /// somewhere that might not run.
    reads: Vec<(LocalId, ExprId, bool)>,
    /// How many conditional or repeated constructs enclose the walk.
    ///
    /// A read inside an `if` arm, a `match` arm, a loop or a lambda may run
    /// zero times or many, so it cannot be *the* last use of anything.
    branching: usize,
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

    /// Hands a binding's reference to its last read instead of copying it.
    ///
    /// The conservative scheme gives every read its own reference and releases
    /// the binding's where the block ends, so a value read once costs a `dup`,
    /// the consumer's `drop`, and the block's `drop`. Three operations to move
    /// one value. Counted across a workload the ratio is stark: parsing one
    /// HTTP request performs 677 reference-count operations against 55
    /// allocations, and a hundred failed `Map::get` calls perform 5,704
    /// against 5. `docs/design/reuse.md`.
    ///
    /// So: where a binding's *last* read is unambiguous, that read takes the
    /// binding's reference rather than making one, and the block no longer
    /// releases it. Two operations become none.
    ///
    /// **The conditions are what make this safe, and they are deliberately
    /// crude.** This is the first step of `reuse.md` §1, not the whole of it.
    ///
    /// - **The body cannot unwind.** A `!`, a `raise`, a `catch` or a `return`
    ///   leaves a frame from the middle, and what is still owned then depends
    ///   on how far execution got. Making the release set path-dependent is the
    ///   hard half of §1; a body that can unwind keeps the conservative plan
    ///   entirely.
    /// - **No read may be inside a branch, a loop, or a lambda.** Those run
    ///   zero times, or many, so no read within them is *the* last one.
    /// - **The binding must be read at least once**, or there is nothing to
    ///   move the reference to and the block still has to release it.
    ///
    /// A parameter counts as a binding here, released by the outermost block
    /// like any other.
    fn settle_last_uses(&mut self) {
        if self.unwinds {
            return;
        }

        let mut moved: Vec<LocalId> = Vec::new();
        for local in self.plan.boxed.clone() {
            if !quietly_dropped(self.types.local(local)) {
                continue;
            }
            let mut reads = self.reads.iter().filter(|(who, _, _)| *who == local).peekable();
            if reads.peek().is_none() {
                continue;
            }
            if self.reads.iter().any(|(who, _, risky)| *who == local && *risky) {
                continue;
            }
            // In program order, because `walk` visits in it.
            let last = self.reads.iter().rfind(|(who, _, _)| *who == local).map(|(_, r, _)| *r);
            if let Some(last) = last {
                self.plan.dups.remove(&last);
                self.plan.moved.insert(local);
                moved.push(local);
            }
        }

        for releases in self.plan.drops.values_mut() {
            releases.retain(|local| !moved.contains(local));
        }
        self.plan.drops.retain(|_, releases| !releases.is_empty());
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
                    self.reads.push((local, id, self.branching > 0));
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
                self.branching += 1;
                self.walk(body);
                self.branching -= 1;
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
                self.branching += 1;
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
                self.branching -= 1;
            }
            // Same shape as `match`, over the error rather than a scrutinee.
            // The error object itself is owned by the catching frame — the
            // raising one moved it into the return — so code generation drops
            // it after the arm; see `lower_catch`.
            Expr::Catch { inner, arms } => {
                self.unwinds = true;
                self.walk(inner);
                self.branching += 1;
                for arm in &arms {
                    let mut ignored = Vec::new();
                    self.bind(arm.pat, &mut ignored);
                    if let Some(guard) = arm.guard {
                        self.walk(guard);
                    }
                    self.walk(arm.body);
                }
                self.branching -= 1;
            }
            Expr::Call { callee, args } => {
                self.walk(callee);
                for arg in args {
                    self.walk(arg);
                }
            }
            Expr::Binary { op, lhs, rhs } => {
                self.walk(lhs);
                // **`&&` and `||` short-circuit**, so the right side is a
                // branch even though it does not look like one. Missing this
                // leaked exactly one object in a derived `Bool` method whose
                // body is three comparisons joined by `||`: the value read on
                // the right was never released when the left answered first.
                let skippable = matches!(op, khora_hir::body::BinOp::And | khora_hir::body::BinOp::Or);
                if skippable {
                    self.branching += 1;
                }
                self.walk(rhs);
                if skippable {
                    self.branching -= 1;
                }
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
                self.branching += 1;
                self.walk(then_branch);
                if let Some(e) = else_branch {
                    self.walk(e);
                }
                self.branching -= 1;
            }
            Expr::While { condition, body } => {
                // The condition runs at least once and the body may run many
                // times; both are inside the repetition as far as a last use is
                // concerned.
                self.branching += 1;
                self.walk(condition);
                self.walk(body);
                self.branching -= 1;
            }
            Expr::Loop { body } => {
                self.branching += 1;
                self.walk(body);
                self.branching -= 1;
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
