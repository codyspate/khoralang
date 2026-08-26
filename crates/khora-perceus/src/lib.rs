//! Reference counting: where `dup` and `drop` go.
//!
//! Two passes over one body. The **conservative** scheme owns a value for the
//! whole of a binding's scope — a read `dup`s, a block releases what it
//! declared — and is correct alone. The **last-use** pass then takes that
//! apart with a backward liveness walk, so a read the binding does not outlive
//! takes its reference instead of copying, and a matched cell that is dead can
//! be handed to the arm's own constructor.
//!
//! Both are needed: under the conservative scheme alone, `xs` at the
//! constructor in `match xs { Cons(h, t) => Cons(f(h), map(t)) }` is held by
//! its binding *and* by the read's dup, so a uniqueness test correctly
//! declines. Deciding the last use on every path is the analysis, and getting
//! it wrong is a double free rather than a slow program.
//!
//! `docs/design/reuse.md` has the design, the measurements, and the three rules
//! that were each found by a crash.
//!
//! # The scheme
//!
//! Only *boxed* values are counted; `Int` and `Bool` are machine words.
//!
//! - A local holding a boxed value owns one reference.
//! - Reading one `dup`s, unless the binding is not needed afterwards, in which
//!   case the read takes the binding's reference.
//! - A block `drop`s every boxed local it declared and nothing took.
//! - Parameters are owned by the callee and dropped like locals.
//!
//! It balances because a read `dup`s and the callee drops, so a call is
//! neutral. A boxed value in statement position and never bound has no binding
//! to hang a release on; code generation drops it at `Stmt::Expr`.
//!
//! The output is a side table keyed by [`ExprId`] and [`LocalId`] rather than a
//! new IR, so code generation walks the same HIR the checker did.

#![deny(missing_docs)]

use std::collections::{BTreeSet, HashMap, HashSet};

use khora_db::{Db, SourceFile};
use khora_hir::body::{BinOp, Body, Expr, ExprId, LocalId, Pat, PatId, Stmt};
use khora_types::Type;

/// Where reference-counting operations belong in one function body.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RcPlan {
    /// Local reads whose result must be `dup`ed.
    pub dups: HashSet<ExprId>,
    /// Locals to `drop` when a block exits, keyed by the block.
    pub drops: HashMap<ExprId, Vec<LocalId>>,
    /// Locals holding a boxed value.
    pub boxed: HashSet<LocalId>,
    /// Arguments passed as a *borrow*: no reference was made, and the callee
    /// must not release one.
    pub borrowed: HashSet<ExprId>,
    /// Locals to release at the *head* of a branch arm, keyed by the arm.
    ///
    /// Where one arm takes a binding's reference and another never mentions
    /// it, the second has to release it, and the head is the only place on
    /// exactly those paths. An arm that merely *reads* the binding gets none —
    /// releasing there would free what it is about to use.
    pub arm_drops: HashMap<ExprId, Vec<LocalId>>,
    /// Locals a `match` arm's pattern binds and its body reads, to `dup` on
    /// entering the arm.
    ///
    /// Without this a binding is a borrowed view into the scrutinee's payload,
    /// valid only while the `match` holds it. Copying once on the way in makes
    /// the arm an owner, which is what lets the scrutinee be released at the
    /// arm's head — the prerequisite for reuse. `docs/design/reuse.md` §2.
    pub arm_binds: HashMap<ExprId, Vec<LocalId>>,
    /// `match` arms that may build their result in the cell they matched,
    /// naming the constructor that may take it.
    ///
    /// `khora_drop_reuse` at the arm's head, `khora_alloc_reuse` at the
    /// constructor. **A token has no owner, so it must be spent on every
    /// path** — which is why the rule is so narrow: the arm's body has to *be*
    /// the constructor and nothing in it may leave the frame early, leaving
    /// exactly one path from release to allocation. `docs/design/reuse.md` §2.
    pub reuse: HashMap<ExprId, ExprId>,
    /// Reads that took the binding's reference rather than copying it.
    ///
    /// Not the complement of `dups`: a borrow copies nothing either, and so
    /// does a read no copy was ever planned for. Deriving takes from "no copy
    /// planned" put a slot-clearing store on reads that were neither.
    pub takes: HashSet<ExprId>,
    /// Whether this body can leave a frame without reaching its end.
    ///
    /// Decides how a *take* is recorded. Where nothing unwinds, whether a
    /// binding was handed on is settled at compile time and the block does not
    /// release it; where something can, the block releases everything and a
    /// take clears the slot, so the question is answered at run time.
    /// `docs/design/reuse.md` §1.
    pub unwinds: bool,
    /// Locals whose reference went to their last read rather than to a block.
    ///
    /// Recorded so the invariant stays checkable: "released exactly once" is
    /// now "released exactly once, or moved exactly once", and without this a
    /// moved local is indistinguishable from a forgotten one.
    pub moved: HashSet<LocalId>,
}

impl RcPlan {
    /// Whether this expression's value has to be `dup`ed where it is used.
    pub fn needs_dup(&self, expr: ExprId) -> bool {
        self.dups.contains(&expr)
    }

    /// Locals to release at the end of this block.
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

    /// Whether this local holds a reference-counted object rather than a
    /// machine word, and so has to be released at all.
    pub fn is_boxed(&self, local: LocalId) -> bool {
        self.boxed.contains(&local)
    }
}

/// Which of a call's arguments it only looks at. Indices into the argument
/// list, receiver first.
///
/// Saying so saves a cancelling `dup`/`drop` pair, and does something that
/// matters more: a borrowed argument is not a *use*, so a binding passed to
/// `Region::defer` keeps its reference and its finalizers run when the scope
/// ends rather than inside `defer`.
///
/// **Only bodyless declarations may appear here.** A function written in Khora
/// owns its parameters and releases them, so promising a caller a borrow of
/// one is a use after free. The key is a bare type name, and anybody may write
/// their own `Shared` with its own `get` — so the planner is given [`Defined`]
/// and consults this table only for a `(type, method)` pair nothing
/// implements. Bodylessness is the property that matters, not whether the
/// declaration is `std`'s.
///
/// `docs/design/reuse.md` §1 wants ownership written on the intrinsic itself
/// rather than a table keyed by name.
pub fn borrowed_arguments(owner: &str, method: &str) -> &'static [usize] {
    const RECEIVER: &[usize] = &[0];
    const NONE: &[usize] = &[];
    match (owner, method) {
        // The runtime keeps the finalizer and only looks at the region.
        ("Region", "defer") => RECEIVER,
        ("Shared", "get" | "set" | "update" | "modify") => RECEIVER,
        // `send` hands over the *value* — the queue owns it — while the handle
        // stays the caller's. A serving fiber sends per reply.
        ("Channel", "send" | "receive" | "close" | "depth") => RECEIVER,
        // *Releasing* a handle is what joins; that is the binding's business.
        ("Fiber", "join" | "cancel") => RECEIVER,
        ("Fibers", "adopt" | "wait") => RECEIVER,

        // The ones that pay: a borrow applies inside a loop where a last use
        // does not, and `lowered_between` calls `String::byte` per character.
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
/// Gates [`borrowed_arguments`] — see there for why a table keyed by a bare
/// type name stopped being safe when packages arrived. Empty means "nothing is
/// known to have a body", which makes the table apply as it did before this
/// existed: wrong for safety, right for a caller holding one file.
#[derive(Debug, Clone, Default)]
pub struct Defined(HashSet<String>);

impl Defined {
    /// From the names bodies are keyed by: `#Type::method` for an inherent
    /// method, `Trait#Type::method` for a trait impl.
    pub fn from_body_names<'a>(names: impl IntoIterator<Item = &'a str>) -> Self {
        Defined(names.into_iter().map(str::to_string).collect())
    }

    /// Whether the program writes this method in Khora.
    ///
    /// Both spellings, because no caller here knows the trait name.
    fn writes(&self, owner: &str, method: &str) -> bool {
        let inherent = format!("#{owner}::{method}");
        let suffix = format!("#{owner}::{method}");
        self.0.iter().any(|name| *name == inherent || name.ends_with(&suffix))
    }
}

/// Whether values of this type carry a reference count.
///
/// `Unknown` counts as unboxed: it only appears downstream of an error, and a
/// spurious `drop` on a machine word is a wild free.
pub fn is_boxed(ty: &Type) -> bool {
    // A closure is an ordinary heap object — a function pointer and its
    // captures under the usual header — and so is a tuple.
    matches!(ty, Type::Str | Type::Adt { .. } | Type::Fn { .. } | Type::Tuple(_))
}

/// Plans reference counting for one body at one set of types.
///
/// **Takes the types rather than deriving them.** Whether a value is boxed
/// depends on the instantiation: `A` in `fn id<A>` is never boxed, and the
/// same body at `A = List<Int>` holds a pointer that must be counted. Errata
/// 24.
pub fn plan(body: &Body, types: &khora_types::BodyTypes, defined: &Defined) -> RcPlan {
    let mut planner = Planner {
        body,
        plan: RcPlan::default(),
        types,
        defined,
        reads: Vec::new(),
        unowned: Live::new(),
        unwinds: false,
    };
    planner.plan_function();
    planner.settle_last_uses();
    planner.plan_reuse();
    planner.plan
}

/// Plans every function body in a file, at the types it was *written* at.
///
/// Good enough for a non-generic function, and what the tests read. Code
/// generation calls [`plan`] once per specialization instead.
#[salsa::tracked(returns(ref))]
pub fn rc_plans(db: &dyn Db, file: SourceFile) -> Vec<(String, RcPlan)> {
    let checked = khora_types::checked(db, file);
    let empty = khora_types::BodyTypes::default();

    // Every body in the *program*: a package may implement `Shared::get`
    // elsewhere, and noticing that is the point of the set.
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
            // The checker's types, not types re-derived from the shape of the
            // expressions: that had no idea what a lambda's type was, so a
            // closure was never counted and a boxed value passed to one was
            // freed twice.
            let body_types =
                checked.bodies.iter().find(|(n, _)| n == name).map(|(_, t)| t).unwrap_or(&empty);
            (name.clone(), plan(body, body_types, &defined))
        })
        .collect()
}

/// Bindings still needed at a point in the backward pass.
///
/// **A `BTreeSet`, and that is a correctness requirement.** These sets are
/// iterated to decide the order releases are emitted in, and every `HashSet`
/// is seeded with a per-process random value — so two builds of one program
/// produced different object files. `docs/project.md` §6.1 asks for bit-for-bit
/// reproducible builds and this was the whole of why they were not.
type Live = BTreeSet<LocalId>;

/// One read of a counted binding, as the walk saw it.
struct Read {
    local: LocalId,
    at: ExprId,
    /// The callee only looks at it, so no reference was made.
    ///
    /// **Still a use.** Leaving borrowed reads out of this list was a use after
    /// free: `f(s)` then `String::byte(s, 0)` moved the binding into `f` and
    /// freed it there, because the borrow was invisible to "which read is
    /// last". A borrow cannot take ownership, but it can come after the read
    /// that would have.
    borrowed: bool,
}

struct Planner<'a> {
    body: &'a Body,
    plan: RcPlan,
    types: &'a khora_types::BodyTypes,
    /// What the program implements in Khora. [`Defined`].
    defined: &'a Defined,
    /// Every read of a boxed local, in program order.
    reads: Vec<Read>,
    /// Bindings holding a reference belonging to somebody else.
    ///
    /// A `match` arm's bindings are projections of the scrutinee's payload: no
    /// reference was made for them, so reading one has to copy, always.
    unowned: Live,
    /// Whether this body can leave a frame early.
    ///
    /// Moving a reference out of a binding makes the set a frame releases
    /// depend on how far execution got, which `docs/design/reuse.md` §1 does
    /// not attempt — so a body that can unwind keeps the conservative plan.
    unwinds: bool,
}

// One module per pass. An inherent impl may be split across modules of one
// crate, so each opens `impl<'a> Planner<'a>` again. Roadmap 9.6.
mod conservative;
mod lastuse;
mod reuse;
mod walk;
