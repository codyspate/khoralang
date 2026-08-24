//! Reference counting: where `dup` and `drop` go.
//!
//! Two passes over one body, and the second is where the interesting part is.
//!
//! The **conservative** scheme owns a value for the whole of a binding's scope:
//! a read `dup`s, and the block releases what it declared on the way out. It is
//! correct on its own and it is what everything else is measured against.
//!
//! The **last-use** pass then takes that apart. It is a backward liveness walk:
//! a read the binding does not outlive takes the reference rather than copying
//! it, a branch that consumes on every path is balanced by a release at the
//! head of the arms that do not, and a matched cell that is dead by the time an
//! arm builds its result is handed to that constructor instead of being freed.
//! That last one is what makes `map` over a uniquely-owned list allocate
//! nothing.
//!
//! Both are needed. `match xs { List::Cons(h, t) => List::Cons(f(h), map(t)) }`
//! cannot reuse anything under the conservative scheme alone, and not because
//! the fusion is missing: at the constructor `xs` is still held by its binding
//! *and* by the dup the read made, so a uniqueness test sees two references and
//! correctly declines. The fusion is the easy half; deciding the last use on
//! every path is the analysis, and it is the part that turns a wrong answer
//! into a double free rather than a slow program.
//!
//! `docs/design/reuse.md` has the design, the measurements, and the three rules
//! that were each found by a crash rather than by thinking.
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
    /// Locals a `match` arm's pattern binds and the arm's body reads, to `dup`
    /// on entering the arm, keyed by the arm's body.
    ///
    /// **This is what makes an arm's bindings ordinary.** `bind_pattern` stores
    /// the loaded field straight into the slot, so without this a binding is a
    /// borrowed view into the scrutinee's payload and only stays valid while
    /// the `match` holds the scrutinee. Copying once on the way in makes the
    /// arm an owner, which is what lets the scrutinee be released at the arm's
    /// head — the prerequisite for handing its memory to the arm's own
    /// constructor. `docs/design/reuse.md` §2.
    ///
    /// The count does not move: the copy that used to happen at each read now
    /// happens once at the head, and the reads that remain are settled by the
    /// last-use pass like any other. A binding the body never reads is left
    /// out, because owning it would cost a copy and a release for nothing.
    pub arm_binds: HashMap<ExprId, Vec<LocalId>>,
    /// `match` arms that may build their result in the cell they matched,
    /// keyed by the arm's body and naming the constructor that may take it.
    ///
    /// The pair is `khora_drop_reuse` at the arm's head and
    /// `khora_alloc_reuse` at the constructor: releasing the scrutinee hands
    /// the memory back when nobody else held it, and the constructor writes
    /// the new object into it. A ten-element `map` over a list nothing else
    /// holds then allocates nothing at all, which is phase 9's exit criterion.
    ///
    /// **A token has no owner, so it must be spent on every path.** That is why
    /// the rule is as narrow as it is: the arm's body has to *be* the
    /// constructor, and nothing inside it may leave the frame early. There is
    /// then exactly one path from the release to the allocation and no way to
    /// take it. `docs/design/reuse.md` §2.
    pub reuse: HashMap<ExprId, ExprId>,
    /// Reads that took the binding's reference rather than copying it.
    ///
    /// The same decision `dups` records by omission, said positively, because
    /// the two are not complements: a borrow copies nothing either, and so does
    /// a read the forward walk never planned a copy for. The code generator
    /// used to work out which reads were takes by asking whether a copy was
    /// planned, and that put a slot-clearing store on reads that were neither.
    pub takes: HashSet<ExprId>,
    /// Whether this body can leave a frame without reaching its end.
    ///
    /// A `!`, a `raise`, a `catch` or a `return`. The code generator reads this
    /// to decide how a *take* is recorded: where nothing can unwind, whether a
    /// binding has been handed on is settled at compile time and the block
    /// simply does not release it; where something can, the block releases
    /// every binding it declared and a take clears the slot, so the question
    /// is answered by what is in the slot at run time. `docs/design/reuse.md`
    /// §1.
    pub unwinds: bool,
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

    /// Locals to `dup` on entering this `match` arm, which the arm then owns.
    pub fn arm_binds_for(&self, arm: ExprId) -> &[LocalId] {
        self.arm_binds.get(&arm).map(|v| v.as_slice()).unwrap_or(&[])
    }

    /// The constructor this arm may build in the cell it matched, if any.
    pub fn reuse_site(&self, arm: ExprId) -> Option<ExprId> {
        self.reuse.get(&arm).copied()
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
///
/// # The rule above is now enforced rather than remembered
///
/// The key is a bare type *name*, which was safe while every program was one
/// source root and every `Shared` was `std`'s. Packages ended that. Anyone may
/// now write
///
/// ```khora
/// export type Shared = { .. };
/// impl Shared { export fn get(self) -> Int { .. } }
/// ```
///
/// and under a name-only key their `get` — an ordinary Khora function, which
/// owns its receiver and releases it — would be told its caller was lending.
/// The caller would not make a reference, the callee would release one anyway,
/// and the object would be freed while somebody still held it: a silent use
/// after free in a package whose only mistake was choosing a common noun.
///
/// So the planner is given [`Defined`] — the set of methods the *program*
/// implements in Khora — and this table is consulted only for a `(type,
/// method)` pair nothing implements. That is precisely the rule the paragraph
/// above states, and it used to be enforced by whoever edited this file.
///
/// **Not "declared by `std`", which was tried first and is wrong.** A
/// self-contained program may legitimately declare its own `Region` and let the
/// runtime implement `defer` — most of `khora-codegen-llvm`'s tests do exactly
/// that, and restricting the table to `std` silently stopped lending to them,
/// which reordered a program's finalizers. Bodylessness is the property that
/// actually matters; where the declaration lives is not.
///
/// `docs/design/reuse.md` §1 has the shape this eventually wants — ownership
/// written on the intrinsic itself, rather than a table keyed by name.
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

/// The methods a program implements in Khora, as `#Type::method`.
///
/// Handed to the planner so that [`borrowed_arguments`] can be consulted only
/// for a pair nothing implements — see its documentation for why a table keyed
/// by a bare type name stopped being safe when packages arrived.
///
/// Empty means "nothing is known to have a body", which makes the table apply
/// exactly as it did before this existed. That is the wrong default for safety
/// and the right one for a caller that only has one file: `rc_plans` builds it
/// from the whole source root, and the backend from monomorphization, and those
/// are the two callers that matter.
#[derive(Debug, Clone, Default)]
pub struct Defined(HashSet<String>);

impl Defined {
    /// From the names bodies are keyed by, which for a method is
    /// `#Type::method` and for a trait impl `Trait#Type::method`.
    pub fn from_body_names<'a>(names: impl IntoIterator<Item = &'a str>) -> Self {
        Defined(names.into_iter().map(str::to_string).collect())
    }

    /// Whether the program writes this method in Khora.
    ///
    /// Both spellings are checked: an inherent `impl Shared { fn get }` is
    /// `#Shared::get`, and a trait impl is `Trait#Shared::get`, which no caller
    /// here knows the trait name for.
    fn writes(&self, owner: &str, method: &str) -> bool {
        let inherent = format!("#{owner}::{method}");
        let suffix = format!("#{owner}::{method}");
        self.0.iter().any(|name| *name == inherent || name.ends_with(&suffix))
    }
}

/// Whether values of this type carry a reference count.
///
/// `Unknown` counts as unboxed: it only appears downstream of an error, and a
/// spurious `drop` on a machine word would be a wild free.
pub fn is_boxed(ty: &Type) -> bool {
    // A closure is an ordinary heap object: a function pointer and whatever it
    // captured, under the same header as everything else. So is a tuple — an
    // anonymous record with positional fields, counted and released like the
    // named kind.
    matches!(ty, Type::Str | Type::Adt { .. } | Type::Fn { .. } | Type::Tuple(_))
}

/// Plans reference counting for one body at one set of types.
///
/// **Takes the types rather than deriving them**, because whether a value is
/// boxed depends on the *instantiation*: `A` in `fn id<A>` is a rigid parameter
/// and never boxed, while the same body compiled at `A = List<Int>` holds a
/// pointer that has to be counted. A plan made once from the generic body is
/// wrong for every instantiation that fills a parameter with something boxed —
/// see `docs/errata.md`, entry 24.
pub fn plan(body: &Body, types: &khora_types::BodyTypes, defined: &Defined) -> RcPlan {
    let mut planner = Planner {
        body,
        plan: RcPlan::default(),
        types,
        defined,
        reads: Vec::new(),
        unowned: HashSet::new(),
        unwinds: false,
    };
    planner.plan_function();
    planner.settle_last_uses();
    planner.plan_reuse();
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

    // Every body in the program, not just this file's: a package may implement
    // `Shared::get` somewhere else entirely, and the point of the set is to
    // notice that. `source_root` is absent only in a test that made a file
    // without one, where this file's own bodies are the whole program.
    let names: Vec<String> = match khora_db::source_root(db) {
        Some(root) => root
            .files(db)
            .iter()
            .flat_map(|f| khora_hir::body::bodies(db, *f).iter().map(|(n, _)| n.clone()))
            .collect(),
        None => khora_hir::body::bodies(db, file).iter().map(|(n, _)| n.clone()).collect(),
    };
    let defined = Defined::from_body_names(names.iter().map(String::as_str));

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
            (name.clone(), plan(body, body_types, &defined))
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
    /// What the program implements in Khora, so that the borrow table is not
    /// consulted for a method somebody wrote a body for. [`Defined`].
    defined: &'a Defined,
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

// One module per pass. Rust lets an inherent impl be split across modules of
// one crate, so each opens `impl<'a> Planner<'a>` again. Roadmap 9.6.
mod conservative;
mod lastuse;
mod reuse;
mod walk;
