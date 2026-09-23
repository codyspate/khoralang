//! Which functions can reach a cancellation point. **Report-only for now.**
//!
//! **What this prevents: paying for a cancellation tag in functions that can
//! never be cancelled part-way.** Strategy T puts a tag on a function's return
//! so a cancellation has a way out of it, and branches on the tag after every
//! call. A function that cannot reach a cancellation point has nothing to
//! carry, so its calls can keep their plain return and pay nothing. This works
//! out which functions those are, once, for the whole program.
//!
//! **Nothing reads the answer yet.** `KHORA_CANCEL_T_REPORT` prints a summary
//! and codegen does not change. The tagged calling convention that would
//! consume it is a later change, and it has to land in one commit, because a
//! caller and a callee that disagree about a return type is a miscompile. This
//! module can be tested on its own before that.
//!
//! # The rule
//!
//! A function **can stop** when its expression arena has
//!
//! - a back-edge: `loop` or `while`, and so `for`;
//! - a `!`, which is a call to a fallible function that may return a tag;
//! - a call through a function value, where the callee is unknown and so has
//!   to be assumed tagged -- including `SharedFn::call`, which is one;
//! - a `Fiber`, `Fibers` or `Channel` operation, the blocking ones;
//! - a call to something that is not a Khora body and is handed a function
//!   value (`attempt`, `Shared::update`, `Region::defer`);
//! - a call to a **runtime export** (`khora_*`), see below;
//! - a call to a **backend intrinsic not known to be stop-free**, see below;
//! - a mention of a Khora function that can stop;
//! - or it is part of a **call cycle**. Recursion is a loop with no back-edge,
//!   and "there is always a cancellation point soon" means it needs one
//!   somewhere. A function in a cycle polls when it is entered.
//!
//! Everything a function mentions counts as something it calls, including a
//! function passed as a value and anything inside a lambda it builds. That
//! over-approximates, which is the safe direction. The unsafe direction is a
//! caller that does not branch on a tag its callee returns, and that reads a
//! pair as a value -- a miscompile. Tagging a function that did not need it
//! costs a branch. **So every default here is toward tagging**: a callee
//! written in a shape this module does not recognise is a call through a
//! function value, and a bodiless operation it does not recognise is an
//! intrinsic that may stop, until somebody lists it as one that cannot.
//!
//! # Foreign calls
//!
//! The blocking `std` operations are foreign calls: `clock.sleep` is
//! `khora_sleep`, `connect_to` is `khora_net_connect`, `accept_on` is
//! `khora_net_accept`. The runtime makes each of them give up when the fiber
//! is cancelled, so a function whose only cancellation point is one of them
//! would, untagged, carry on after a sleep a cancellation cut short.
//!
//! **A call to a runtime export counts; a call to any other C does not.**
//! Only the runtime can observe a cancellation, and third-party C already
//! runs to its end -- a stated limit of cancellation, not something a tag
//! could change. The runtime's exports are a closed set, named by their
//! `khora_` prefix, so the rule needs no list of which ones block: it tags a
//! few that do not (`khora_decimal_*`, say), which costs a branch each.
//! [`CanStop::only_foreign`] counts the functions tagged only because of it.
//!
//! # Intrinsics
//!
//! A bodiless operation the backend implements is counted
//! ([`Local::Intrinsic`]) unless [`stop_free`] or [`stop_free_item`] names it.
//! Those lists are the operations with no callback and no wait: the integer
//! types, `Array` (but `with_data`), `String`'s byte operations, `Char`,
//! `Float::to_int`, `Ptr`, `Region::open`/`root`, `Shared::of`/`get`/`set`,
//! and `print`/`assert`/`assert_that`. An operation that takes a function
//! value is counted by [`Local::HandsAClosure`] as well. A bodiless call on a
//! type declared in Khora can only be a field holding a function value, and is
//! a [`Local::ClosureCall`].
//!
//! **`Shared::get` and `set` can wait, and are still not cancellation
//! points.** Both take the cell's lock, and so wait for as long as another
//! fiber's `update` change function holds it. That wait cannot be made a
//! cancellation point without giving up the lock's guarantee, and it is
//! bounded by the other fiber's change function rather than by anything
//! outside the program -- the same kind of limit as a foreign call already in
//! progress.

use std::collections::{HashMap, HashSet};

use khora_hir::body::{Body, Expr};
use khora_hir::Resolution;
use khora_types::{BodyTypes, Type};

/// Why a function can stop on its own account, before its callees are counted.
///
/// An enum rather than a flag so a test can check *which* rule fired, and so a
/// rule that stops applying shows up as a test failure, not a silently larger
/// or smaller answer.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub(crate) enum Local {
    /// A `loop` or `while`.
    BackEdge,
    /// A `!`.
    Try,
    /// A call through a function value.
    ClosureCall,
    /// A `Fiber`, `Fibers` or `Channel` operation.
    Blocking,
    /// A call to something that is not a Khora body, handed a function value.
    HandsAClosure,
    /// A call to a runtime export (`khora_*`). See the module docs.
    Foreign,
    /// A call to a backend intrinsic [`stop_free`] does not name.
    Intrinsic,
}

impl Local {
    /// Every reason, for the report.
    ///
    /// **A new variant fails to compile in [`Local::COUNT`]'s match** until an
    /// arm names it. That is where whoever adds it is made to look here; the
    /// match cannot also check the number or the list, which is what
    /// `every_reason_is_in_the_report` does.
    const ALL: [Local; Local::COUNT] = [
        Local::BackEdge,
        Local::Try,
        Local::ClosureCall,
        Local::Blocking,
        Local::HandsAClosure,
        Local::Foreign,
        Local::Intrinsic,
    ];
    const COUNT: usize = match Local::Foreign {
        Local::BackEdge
        | Local::Try
        | Local::ClosureCall
        | Local::Blocking
        | Local::HandsAClosure
        | Local::Foreign
        | Local::Intrinsic => 7,
    };
}

/// The whole program's answer, by symbol.
#[derive(Default, Debug)]
pub(crate) struct CanStop {
    /// Functions whose calls need the tag.
    pub stops: HashSet<String>,
    /// Functions in a call cycle, which poll when entered.
    pub cyclic: HashSet<String>,
    /// Functions in [`Self::stops`] only because the [`Local::Foreign`] rule
    /// is counted.
    pub only_foreign: HashSet<String>,
    /// Each function's own reasons, before its callees are counted.
    pub local: HashMap<String, HashSet<Local>>,
    /// How many functions were considered.
    pub considered: usize,
}

impl CanStop {
    /// What `KHORA_CANCEL_T_REPORT` prints: the totals, then how many functions
    /// each rule fires in, before callees are counted.
    pub(crate) fn summary(&self) -> String {
        let reasons = Local::ALL
            .map(|reason| {
                let count = self.local.values().filter(|r| r.contains(&reason)).count();
                format!("{reason:?} {count}")
            })
            .join(", ");
        format!(
            "cancel-T: {} functions, {} can stop ({} in a call cycle, {} only through a \
             foreign call), {} keep a plain return\ncancel-T: by own reason: {reasons}",
            self.considered,
            self.stops.len(),
            self.cyclic.len(),
            self.only_foreign.len(),
            self.considered - self.stops.len()
        )
    }
}

/// Whether the function this expression names is one of the runtime's
/// blocking types' operations.
fn is_blocking_owner(name: &str) -> bool {
    name == crate::runtime::FIBER_TYPE
        || name == crate::runtime::FIBERS_TYPE
        || name == crate::runtime::CHANNEL_TYPE
}

/// Whether `owner` is a type the backend implements operations for, as
/// opposed to a type declared in Khora, whose bodiless "method" can only be a
/// field holding a function value.
fn is_intrinsic_owner(owner: &str) -> bool {
    is_blocking_owner(owner)
        || owner == crate::runtime::REGION_TYPE
        || owner == crate::runtime::SHARED_TYPE
        || owner == crate::runtime::SHARED_FN_TYPE
        || owner == crate::runtime::ARRAY_TYPE
        || is_int_owner(owner)
        || matches!(owner, "Char" | "String" | "Float" | "Ptr")
}

fn is_int_owner(owner: &str) -> bool {
    matches!(owner, "Int" | "I64") || khora_types::IntKind::parse(owner).is_some()
}

/// The backend intrinsics known to have no cancellation point: no callback,
/// and no wait but the bounded one `Shared::get`/`set` take on the cell's lock
/// (the module docs say why that one is not a point).
///
/// **The list is what is allowed, not what is refused.** An operation missing
/// from it is counted, so a new intrinsic costs a branch until somebody adds it
/// here -- rather than an untagged caller of something that stops, which is a
/// miscompile. One that takes a function value is counted by
/// [`Local::HandsAClosure`] whether it is here or not.
fn stop_free(owner: &str, name: &str) -> bool {
    if is_int_owner(owner) || owner == crate::runtime::ARRAY_TYPE && name != "with_data" {
        return true;
    }
    if owner == crate::runtime::REGION_TYPE {
        return matches!(name, "open" | "root");
    }
    if owner == crate::runtime::SHARED_TYPE {
        return matches!(name, "of" | "get" | "set");
    }
    match owner {
        "Char" => matches!(name, "code" | "of" | "from_code"),
        "String" => matches!(name, "bytes" | "byte" | "byte_length" | "slice" | "find" | "from_bytes"),
        "Float" => name == "to_int",
        "Ptr" => matches!(name, "null" | "is_null"),
        _ => false,
    }
}

/// The bodiless free functions `std` declares for the backend to implement,
/// known to have no cancellation point. The same rule as [`stop_free`]:
/// anything not here is counted.
fn stop_free_item(name: &str) -> bool {
    matches!(name, "print" | "assert" | "assert_that")
}

/// Why a call to the bodiless type operation `owner::name` can stop, if it can.
fn operation(owner: &str, name: &str) -> Option<Local> {
    if is_blocking_owner(owner) {
        Some(Local::Blocking)
    } else if owner == crate::runtime::SHARED_FN_TYPE && name == "call" {
        // Calls the wrapped closure: a call through a function value.
        Some(Local::ClosureCall)
    } else if !is_intrinsic_owner(owner) {
        // A Khora type has no intrinsics, so a bodiless call on one is a
        // field holding a function value.
        Some(Local::ClosureCall)
    } else if stop_free(owner, name) {
        None
    } else {
        Some(Local::Intrinsic)
    }
}

/// The name [`operation`] is keyed on for a receiver of type `ty`, the way
/// the backend's own dispatch names it. `None` for a receiver with no
/// operations of its own: a tuple, a function value.
fn receiver_owner(ty: &Type) -> Option<String> {
    match ty {
        Type::Str => Some("String".to_string()),
        Type::Adt { name, .. } => Some(name.clone()),
        Type::Int => Some("Int".to_string()),
        Type::Fixed(kind) => Some(kind.name()),
        Type::Float => Some("Float".to_string()),
        Type::Ptr => Some("Ptr".to_string()),
        _ => None,
    }
}

/// Why a call to a named function with no Khora body can stop, if it can:
/// an `extern` declaration, or a bodiless function the backend implements.
fn named(name: &str, is_extern: &impl Fn(&str) -> bool) -> Option<Local> {
    if is_extern(name) {
        // Only the runtime observes a cancellation; other C runs to its end.
        name.starts_with("khora_").then_some(Local::Foreign)
    } else if stop_free_item(name) {
        None
    } else {
        Some(Local::Intrinsic)
    }
}

/// Why a call whose callee is `target`, with no Khora body behind it, can
/// stop.
///
/// **Exhaustive over the callee's shape, and every shape not recognised as
/// something stop-free is a call through a function value**: an expression
/// that produces a function is the one kind of callee this cannot see into.
fn callee_reason(
    target: &Expr,
    resolved: Option<&str>,
    types: &BodyTypes,
    is_extern: &impl Fn(&str) -> bool,
) -> Option<Local> {
    use khora_hir::ItemKind;
    match target {
        Expr::Path(Resolution::Item { name, kind, .. }) => match kind {
            ItemKind::Function => named(resolved.unwrap_or(name), is_extern),
            // A constructor.
            ItemKind::Type => None,
            // A value that is called, so a function value.
            ItemKind::Const | ItemKind::Context | ItemKind::Effect | ItemKind::Trait | ItemKind::Row => {
                Some(Local::ClosureCall)
            }
        },
        // A constructor.
        Expr::Path(Resolution::Variant { .. }) => None,
        Expr::Path(Resolution::TraitItem { owner, name }) => operation(owner, name),
        // A compile error elsewhere; counted rather than guessed at.
        Expr::Path(Resolution::Unsupported(_)) => Some(Local::Intrinsic),
        Expr::Field { base, name } => match receiver_owner(types.of(*base)) {
            Some(owner) => operation(&owner, name),
            None => Some(Local::ClosureCall),
        },
        Expr::Missing
        | Expr::Literal(_)
        | Expr::Local(_)
        | Expr::Unresolved(_)
        | Expr::Call { .. }
        | Expr::Binary { .. }
        | Expr::Unary { .. }
        | Expr::Assign { .. }
        | Expr::If { .. }
        | Expr::Match { .. }
        | Expr::Block { .. }
        | Expr::While { .. }
        | Expr::Loop { .. }
        | Expr::Break(_)
        | Expr::Continue
        | Expr::Return(_)
        | Expr::Tuple(_)
        | Expr::Shown(_)
        | Expr::Raise(_)
        | Expr::Try(_)
        | Expr::Catch { .. }
        | Expr::LambdaSelf
        | Expr::Record { .. }
        | Expr::Lambda { .. }
        | Expr::Unit => Some(Local::ClosureCall),
    }
}

/// One function's own reasons to stop, and the Khora functions it mentions.
fn scan(
    symbol: &str,
    body: &Body,
    types: &BodyTypes,
    mono: &khora_types::mono::Instances,
    is_defined: &impl Fn(&str) -> bool,
    is_extern: &impl Fn(&str) -> bool,
) -> (HashSet<Local>, Vec<String>) {
    let mut local = HashSet::new();
    let mut out = Vec::new();
    for (id, expr) in body.exprs() {
        // A mention is an edge, called or not: a function passed as a value is
        // called by whoever receives it. This is also how an operator or a
        // `${..}` on a user type reaches its `eq` or `show`.
        if let Some(callee) = mono.callee(symbol, id) {
            if is_defined(&callee) {
                out.push(callee);
            }
        }
        // **Exhaustive, so a new kind of expression has to say whether it is
        // a cancellation point.** A new looping construct that fell into a
        // catch-all here would add no reason, which is the miscompiling
        // direction.
        match expr {
            Expr::Loop { .. } | Expr::While { .. } => {
                local.insert(Local::BackEdge);
            }
            Expr::Try(_) => {
                local.insert(Local::Try);
            }
            Expr::Call { callee, args } => {
                let resolved = mono.callee(symbol, *callee);
                if resolved.as_deref().is_some_and(is_defined) {
                    // A Khora body: counted through the edge above.
                    continue;
                }
                let target = body.expr(*callee);
                if let Some(reason) = callee_reason(target, resolved.as_deref(), types, is_extern) {
                    local.insert(reason);
                }
                if args.iter().any(|a| matches!(types.of(*a), Type::Fn { .. })) {
                    local.insert(Local::HandsAClosure);
                }
            }
            // No cancellation point of their own. Their operands are
            // expressions in the same arena and are scanned there; an
            // operator or interpolation that dispatches to Khora is an edge.
            Expr::Missing
            | Expr::Literal(_)
            | Expr::Local(_)
            | Expr::Path(_)
            | Expr::Unresolved(_)
            | Expr::Field { .. }
            | Expr::Binary { .. }
            | Expr::Unary { .. }
            | Expr::Assign { .. }
            | Expr::If { .. }
            | Expr::Match { .. }
            | Expr::Block { .. }
            | Expr::Break(_)
            | Expr::Continue
            | Expr::Return(_)
            | Expr::Tuple(_)
            | Expr::Shown(_)
            | Expr::Raise(_)
            | Expr::Catch { .. }
            | Expr::LambdaSelf
            | Expr::Record { .. }
            | Expr::Lambda { .. }
            | Expr::Unit => {}
        }
    }
    (local, out)
}

/// Decides, for every instance, whether it can stop.
///
/// `instances` is each emitted symbol with its body and its types at that
/// specialization. `is_defined` answers whether a symbol is one of them, as
/// opposed to C or an intrinsic; `is_extern` whether a bodiless name is an
/// `extern` declaration, as opposed to an operation the backend implements.
pub(crate) fn decide<'a>(
    instances: impl Iterator<Item = (String, &'a Body, &'a BodyTypes)>,
    mono: &khora_types::mono::Instances,
    is_defined: impl Fn(&str) -> bool,
    is_extern: impl Fn(&str) -> bool,
) -> CanStop {
    let mut edges: HashMap<String, Vec<String>> = HashMap::new();
    let mut local: HashMap<String, HashSet<Local>> = HashMap::new();
    let mut considered = 0;
    for (symbol, body, types) in instances {
        considered += 1;
        let (reasons, out) = scan(&symbol, body, types, mono, &is_defined, &is_extern);
        local.insert(symbol.clone(), reasons);
        edges.insert(symbol, out);
    }

    let cyclic = in_a_cycle(&edges);
    let closure = |counts: &dyn Fn(&HashSet<Local>) -> bool| {
        let mut stops: HashSet<String> = local
            .iter()
            .filter(|(_, reasons)| counts(reasons))
            .map(|(symbol, _)| symbol.clone())
            .collect();
        stops.extend(cyclic.iter().cloned());
        // Least fixed point: a caller of something that stops, stops.
        loop {
            let before = stops.len();
            for (symbol, callees) in &edges {
                if !stops.contains(symbol) && callees.iter().any(|c| stops.contains(c)) {
                    stops.insert(symbol.clone());
                }
            }
            if stops.len() == before {
                return stops;
            }
        }
    };
    let stops = closure(&|reasons| !reasons.is_empty());
    let without_foreign = closure(&|reasons| reasons.iter().any(|r| *r != Local::Foreign));
    let only_foreign = stops.difference(&without_foreign).cloned().collect();

    CanStop { stops, cyclic, only_foreign, local, considered }
}

/// Every symbol that can reach itself: a member of a strongly connected
/// component of more than one, or a function that calls itself.
///
/// Tarjan's algorithm, iterative. **Iterative because recursion here would be
/// bounded by the depth of the program's call graph**, and the compiler's own
/// stack is not somewhere a deep program should be able to reach.
fn in_a_cycle(edges: &HashMap<String, Vec<String>>) -> HashSet<String> {
    // Sorted so the walk, and so any bug in it, is the same every run.
    let mut names: Vec<&str> = edges.keys().map(String::as_str).collect();
    names.sort_unstable();
    let index_of: HashMap<&str, usize> = names.iter().enumerate().map(|(i, n)| (*n, i)).collect();
    let succ: Vec<Vec<usize>> = names
        .iter()
        .map(|n| edges[*n].iter().filter_map(|c| index_of.get(c.as_str()).copied()).collect())
        .collect();

    let unvisited = usize::MAX;
    let mut index = vec![unvisited; names.len()];
    let mut low = vec![0; names.len()];
    let mut on_stack = vec![false; names.len()];
    let mut stack: Vec<usize> = Vec::new();
    let mut next = 0;
    let mut cyclic = HashSet::new();

    for root in 0..names.len() {
        if index[root] != unvisited {
            continue;
        }
        // (node, how many of its successors have been looked at)
        let mut work: Vec<(usize, usize)> = vec![(root, 0)];
        index[root] = next;
        low[root] = next;
        next += 1;
        stack.push(root);
        on_stack[root] = true;
        while let Some(&mut (node, ref mut seen)) = work.last_mut() {
            if let Some(&to) = succ[node].get(*seen) {
                *seen += 1;
                if index[to] == unvisited {
                    index[to] = next;
                    low[to] = next;
                    next += 1;
                    stack.push(to);
                    on_stack[to] = true;
                    work.push((to, 0));
                } else if on_stack[to] {
                    low[node] = low[node].min(index[to]);
                }
                continue;
            }
            work.pop();
            if let Some(&(parent, _)) = work.last() {
                low[parent] = low[parent].min(low[node]);
            }
            if low[node] == index[node] {
                let mut component = Vec::new();
                while let Some(member) = stack.pop() {
                    on_stack[member] = false;
                    component.push(member);
                    if member == node {
                        break;
                    }
                }
                let recursive = component.len() > 1 || succ[node].contains(&node);
                if recursive {
                    cyclic.extend(component.into_iter().map(|m| names[m].to_string()));
                }
            }
        }
    }
    cyclic
}

/// [`decide`] over a whole program's instances, finding each one's body.
///
/// `files` is every file in the program, for the `extern` declarations: a
/// name declared `extern` in any of them is foreign C, which is the symbol a
/// call to it reaches whichever module wrote it.
pub(crate) fn analyse(
    db: &dyn khora_db::Db,
    files: &[khora_db::SourceFile],
    mono: &khora_types::mono::Instances,
) -> CanStop {
    let bodies: Vec<(String, &Body, &BodyTypes)> = mono
        .instances
        .iter()
        .filter_map(|(instance, types)| {
            let home = mono.home(&instance.symbol())?;
            let body = khora_hir::body::bodies(db, home)
                .iter()
                .find(|(n, _)| n == &instance.function)
                .map(|(_, b)| b)?;
            Some((instance.symbol(), body, types))
        })
        .collect();
    let defined: HashSet<String> = bodies.iter().map(|(s, _, _)| s.clone()).collect();
    let externs: HashSet<String> = files
        .iter()
        .flat_map(|file| {
            khora_types::type_map(db, *file)
                .signatures
                .iter()
                .filter(|(_, signature)| signature.is_extern)
                .map(|(name, _)| name.to_string())
                .collect::<Vec<_>>()
        })
        .collect();
    decide(
        bodies.iter().map(|(s, b, t)| (s.clone(), *b, *t)),
        mono,
        |n| defined.contains(n),
        |n| externs.contains(n),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use khora_db::{KhoraDatabase, SourceFile, SourceRoot};

    /// The analysis for one single-file program.
    fn analysed(source: &str) -> CanStop {
        let db = KhoraDatabase::new();
        let file = SourceFile::new(&db, "main.kh".into(), source.to_string());
        let root = SourceRoot::new(&db, vec![file]);
        let mono = khora_types::mono::program_instances(&db, root);
        assert!(mono.errors.is_empty(), "the fixture should compile: {:?}", mono.errors);
        analyse(&db, &[file], mono)
    }

    /// The symbol whose last name segment is `name`: `main$add`, or
    /// `main$#Counter::peek` for a method. Panics on none, or on two.
    fn symbol(answer: &CanStop, name: &str) -> String {
        let last = |s: &str| s.rsplit(['$', ':']).next().unwrap_or(s).to_string();
        let found: Vec<&String> = answer.local.keys().filter(|s| last(s) == name).collect();
        assert_eq!(found.len(), 1, "exactly one symbol ending `{name}` in {:?}", answer.local.keys());
        found[0].clone()
    }

    fn stops(answer: &CanStop, name: &str) -> bool {
        answer.stops.contains(&symbol(answer, name))
    }

    fn cyclic(answer: &CanStop, name: &str) -> bool {
        answer.cyclic.contains(&symbol(answer, name))
    }

    #[test]
    fn a_leaf_with_no_loop_and_no_call_keeps_its_plain_return() {
        let answer = analysed(
            "module main;
fn add(a: Int, b: Int) -> Int { a + b }
fn main() -> Int { add(1, 2) }
",
        );
        assert!(!stops(&answer, "add"));
        assert!(!stops(&answer, "main"), "calling only a leaf is not a reason");
        assert!(!cyclic(&answer, "add"));
    }

    #[test]
    fn a_loop_stops_and_so_does_every_caller_of_it() {
        let answer = analysed(
            "module main;
fn spin() -> Int { let mut n = 0; while n < 10 { n = n + 1; } n }
fn middle() -> Int { spin() + 1 }
fn main() -> Int { middle() }
",
        );
        assert!(answer.local[&symbol(&answer, "spin")].contains(&Local::BackEdge));
        assert!(stops(&answer, "spin"));
        assert!(stops(&answer, "middle"), "a caller of something that stops, stops");
        assert!(stops(&answer, "main"));
        assert!(!cyclic(&answer, "spin"), "a back-edge is not a call cycle");
    }

    /// Recursion has no back-edge, so a cycle is itself the reason.
    #[test]
    fn a_call_cycle_stops_and_polls_at_entry() {
        let answer = analysed(
            "module main;
fn fib(n: Int) -> Int { if n < 2 { n } else { fib(n - 1) + fib(n - 2) } }
fn even(n: Int) -> Bool { if n == 0 { true } else { odd(n - 1) } }
fn odd(n: Int) -> Bool { if n == 0 { false } else { even(n - 1) } }
fn leaf() -> Int { 1 }
fn main() -> Int { if even(4) { fib(10) } else { leaf() } }
",
        );
        assert!(cyclic(&answer, "fib"), "self-recursion");
        assert!(cyclic(&answer, "even") && cyclic(&answer, "odd"), "mutual recursion");
        assert!(stops(&answer, "fib") && stops(&answer, "even"));
        assert!(!cyclic(&answer, "main"), "calling into a cycle is not being in one");
        assert!(stops(&answer, "main"));
        assert!(!stops(&answer, "leaf"));
    }

    /// A function value's callee is unknown, so a call through one is assumed
    /// to be a cancellation point.
    #[test]
    fn a_call_through_a_function_value_stops() {
        let answer = analysed(
            "module main;
fn apply(f: (Int) -> Int, x: Int) -> Int { f(x) }
fn double(x: Int) -> Int { x * 2 }
fn main() -> Int { apply(double, 3) }
",
        );
        assert!(answer.local[&symbol(&answer, "apply")].contains(&Local::ClosureCall));
        assert!(stops(&answer, "apply"));
        assert!(!stops(&answer, "double"), "the function passed is a leaf");
        assert!(stops(&answer, "main"));
    }

    /// One answer per specialization: a generic that loops stops at every type
    /// it is used at, and a generic leaf at none.
    #[test]
    fn a_generic_is_decided_per_specialization() {
        let answer = analysed(
            "module main;
fn same<A>(x: A) -> A { x }
fn wait<A>(x: A) -> A { let mut n = 0; while n < 3 { n = n + 1; } x }
fn main() -> Int { let b = same(true); let s = wait(true); same(1) + wait(2) }
",
        );
        let of = |name: &str| -> Vec<&String> {
            answer.local.keys().filter(|s| s.contains(&format!("${name}"))).collect()
        };
        assert_eq!(of("same").len(), 2, "two specializations: {:?}", answer.local.keys());
        assert!(of("same").iter().all(|s| !answer.stops.contains(*s)));
        assert_eq!(of("wait").len(), 2);
        assert!(of("wait").iter().all(|s| answer.stops.contains(*s)));
    }

    /// **The hole the prototype had.** A blocking `std` call is a foreign call,
    /// so a function whose only cancellation point is one must still carry the
    /// tag. It is counted as foreign, and reported as tagged only for that
    /// reason.
    #[test]
    fn a_call_to_a_foreign_function_stops() {
        let answer = analysed(
            "module main;
extern fn khora_sleep(millis: Int) -> ();
fn nap() -> () { khora_sleep(10) }
fn main() -> Int { nap(); 0 }
",
        );
        assert!(answer.local[&symbol(&answer, "nap")].contains(&Local::Foreign));
        assert!(stops(&answer, "nap"));
        assert!(answer.only_foreign.contains(&symbol(&answer, "nap")));
        assert!(stops(&answer, "main"));
        assert!(
            !answer.local.keys().any(|s| s.ends_with("khora_sleep")),
            "a declaration has no body and is not an instance"
        );
    }

    /// Operations the backend implements, declared without a body: a blocking
    /// type's stop, one handed a closure stops, and a listed stop-free one
    /// does not. A bodiless "method" on a type declared in Khora is a field
    /// holding a function value, and counts.
    #[test]
    fn a_bodiless_operation_stops_only_if_it_blocks_or_takes_a_function() {
        let answer = analysed(
            "module main;
pub type Fiber<A, 'r>;
impl<A, 'r> Fiber<A, 'r> { fn spawn(body: () -> A raises 'r) -> Fiber<A, 'r>; fn join(self) -> A raises 'r; }
pub type Shared<A>;
impl<A> Shared<A> { fn get(self) -> A; fn update(self, change: (A) -> A) -> A; }
pub type Cell;
impl Cell { fn width(self) -> Int; }
fn work() -> Int { 1 }
fn joins() -> Int raises {} { Fiber::join(Fiber::spawn(fn () => work()))! }
fn measures(c: Shared<Int>) -> Int { c.get() }
fn changes(c: Shared<Int>) -> Int { c.update(fn n => n + 1) }
fn fielded(c: Cell) -> Int { c.width() }
fn main() -> Int { 0 }
",
        );
        assert!(answer.local[&symbol(&answer, "joins")].contains(&Local::Blocking));
        assert!(answer.local[&symbol(&answer, "joins")].contains(&Local::Try), "the `!` on join");
        assert!(!stops(&answer, "measures"), "a listed stop-free intrinsic is no cancellation point");
        assert!(answer.local[&symbol(&answer, "changes")].contains(&Local::HandsAClosure));
        assert!(answer.local[&symbol(&answer, "fielded")].contains(&Local::ClosureCall));
    }

    /// A method is an ordinary instance, and a method call an ordinary edge.
    #[test]
    fn a_method_is_decided_like_any_function() {
        let answer = analysed(
            "module main;
type Counter = { n: Int }
impl Counter {
  fn peek(self) -> Int { self.n }
  fn drain(self) -> Int { let mut n = self.n; while n > 0 { n = n - 1; } n }
}
fn look(c: Counter) -> Int { c.peek() }
fn empty(c: Counter) -> Int { c.drain() }
fn main() -> Int { let c = { n: 3 }; look(c) + empty(c) }
",
        );
        assert!(!stops(&answer, "peek"));
        assert!(stops(&answer, "drain"));
        assert!(!stops(&answer, "look"), "a call to a method that cannot stop");
        assert!(stops(&answer, "empty"), "a call to a method that can");
    }

    /// Tarjan on its own: a cycle is found wherever the walk enters it, and a
    /// path into or out of one is not part of it.
    #[test]
    fn cycles_are_exactly_the_strongly_connected_components() {
        let edges: HashMap<String, Vec<String>> = [
            ("a", vec!["b"]),
            ("b", vec!["c"]),
            ("c", vec!["b", "d"]),
            ("d", vec![]),
            ("e", vec!["e"]),
            ("f", vec!["a", "e"]),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v.into_iter().map(String::from).collect()))
        .collect();
        let mut found: Vec<String> = in_a_cycle(&edges).into_iter().collect();
        found.sort();
        assert_eq!(found, ["b", "c", "e"]);
    }

    /// The report's list has every reason once, so none is left out of the
    /// counts it prints.
    #[test]
    fn every_reason_is_in_the_report() {
        let listed: HashSet<Local> = Local::ALL.into_iter().collect();
        assert_eq!(listed.len(), Local::ALL.len(), "a reason is listed twice");
        let summary = CanStop::default().summary();
        for reason in Local::ALL {
            assert!(summary.contains(&format!("{reason:?} 0")), "{reason:?} missing: {summary}");
        }
    }

    const SHARED_FN: &str = "pub type SharedFn<A, B, 'er>;
impl<A, B, 'er> SharedFn<A, B, 'er> { fn of(f: (A) -> B raises 'er) -> SharedFn<A, B, 'er>; fn call(self, argument: A) -> B raises 'er; }
fn spin(x: Int) -> Int { let mut n = x; while n > 0 { n = n - 1; } n }
";

    /// **`SharedFn::call` runs whatever closure it wraps**, so it is a call
    /// through a function value. Its argument is the `SharedFn`, not a
    /// `Type::Fn`, so nothing else here would count it.
    #[test]
    fn a_shared_fn_call_is_a_call_through_a_function_value() {
        let answer = analysed(&format!(
            "module main;
{SHARED_FN}fn route(h: SharedFn<Int, Int, {{}}>) -> Int {{ SharedFn::call(h, 5) }}
fn main() -> Int {{ route(SharedFn::of(fn x => spin(x))) }}
"
        ));
        assert!(answer.local[&symbol(&answer, "route")].contains(&Local::ClosureCall));
        assert!(stops(&answer, "route"));
    }

    /// The same, written as a method.
    #[test]
    fn a_shared_fn_method_call_is_a_call_through_a_function_value() {
        let answer = analysed(&format!(
            "module main;
{SHARED_FN}fn route(h: SharedFn<Int, Int, {{}}>) -> Int {{ h.call(5) }}
fn main() -> Int {{ route(SharedFn::of(fn x => spin(x))) }}
"
        ));
        assert!(answer.local[&symbol(&answer, "route")].contains(&Local::ClosureCall));
        assert!(stops(&answer, "route"));
    }

    /// **The default is to count.** A bodiless operation on a backend type
    /// that nobody has listed as stop-free is a cancellation point, until
    /// somebody lists it -- here a made-up `Array::fetch_remote`.
    #[test]
    fn an_intrinsic_nobody_listed_is_counted() {
        let answer = analysed(
            "module main;
pub type Array<A>;
impl<A> Array<A> { fn length(self) -> Int; fn with_data(self, f: (Ptr, Int) -> Int) -> Int; }
pub type Region;
impl Region { fn open() -> Region; fn warm_up(self) -> (); }
fn listed(a: Array<Int>) -> Int { a.length() }
fn unlisted(r: Region) -> () { r.warm_up() }
fn made_up() -> () { Region::warm_up(Region::open()) }
fn main() -> Int { 0 }
",
        );
        assert!(!stops(&answer, "listed"), "a listed, stop-free intrinsic is not counted");
        assert!(answer.local[&symbol(&answer, "unlisted")].contains(&Local::Intrinsic));
        assert!(answer.local[&symbol(&answer, "made_up")].contains(&Local::Intrinsic));
    }

    /// `Shared::get` and `set` wait on the cell's lock, and are still not
    /// cancellation points: the module docs say why.
    #[test]
    fn shared_get_and_set_are_not_cancellation_points() {
        let answer = analysed(
            "module main;
pub type Shared<A>;
impl<A> Shared<A> { fn of(value: A) -> Shared<A>; fn get(self) -> A; fn set(self, value: A) -> (); fn update(self, change: (A) -> A) -> A; }
fn read(c: Shared<Int>) -> Int { c.get() }
fn write(c: Shared<Int>) -> () { Shared::set(c, 1) }
fn bump(c: Shared<Int>) -> Int { c.update(fn n => n + 1) }
fn main() -> Int { 0 }
",
        );
        assert!(!stops(&answer, "read"));
        assert!(!stops(&answer, "write"));
        assert!(answer.local[&symbol(&answer, "bump")].contains(&Local::HandsAClosure));
    }

    /// **Only the runtime observes a cancellation**, so a call to a runtime
    /// export counts and a call to third-party C does not.
    #[test]
    fn a_runtime_export_counts_and_third_party_c_does_not() {
        let answer = analysed(
            "module main;
extern fn khora_sleep(millis: Int) -> ();
extern fn getpid() -> Int;
fn nap() -> () { khora_sleep(10) }
fn who() -> Int { getpid() }
fn main() -> Int { nap(); who() }
",
        );
        assert!(answer.local[&symbol(&answer, "nap")].contains(&Local::Foreign));
        assert!(!stops(&answer, "who"), "third-party C runs to its end whatever the tag says");
    }

    /// **The blocking `std` calls the prototype's rule missed are still
    /// counted under the narrower one**, read from `std`'s own source: each
    /// of these calls a runtime export directly, from a function or from a
    /// lambda it builds (`Clock::real`'s `sleep`), which marks the builder.
    /// Unreached from a program, so this scans the bodies rather than the
    /// instances: `accept_on`, `connect_to` and `real` are the functions a
    /// server's accept loop, a client and every `clock.sleep` go through.
    #[test]
    fn the_blocking_std_calls_are_counted() {
        let db = KhoraDatabase::new();
        let std = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../std");
        let mut seen = Vec::new();
        for file in ["clock_native.kh", "net/socket_linux.kh", "net/socket_macos.kh", "net/socket_windows.kh"] {
            let path = std.join(file);
            let text = std::fs::read_to_string(&path).expect("a std file");
            let source = SourceFile::new(&db, path, text);
            let types = khora_types::type_map(&db, source);
            let is_extern = |n: &str| types.signatures.get(n).is_some_and(|s| s.is_extern);
            let checked = khora_types::checked(&db, source);
            for (name, body) in khora_hir::body::bodies(&db, source).iter() {
                if !(matches!(name.as_str(), "accept_on" | "connect_to") || name.ends_with("Clock::real")) {
                    continue;
                }
                let body_types = checked
                    .bodies
                    .iter()
                    .find(|(n, _)| n == name)
                    .map(|(_, t)| t)
                    .expect("a checked body");
                let mono = khora_types::mono::Instances::default();
                let (reasons, _) = scan(name, body, body_types, &mono, &|_| false, &is_extern);
                assert!(reasons.contains(&Local::Foreign), "{file} {name}: {reasons:?}");
                seen.push(format!("{file} {name}"));
            }
        }
        assert_eq!(seen.len(), 7, "accept_on and connect_to on three platforms, and real: {seen:?}");
    }
}
