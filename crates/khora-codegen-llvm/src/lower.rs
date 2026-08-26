//! Lowering one function body to LLVM IR.
//!
//! # The two conventions everything else follows from
//!
//! **Divergence is `None`.** Lowering an expression returns
//! `Option<BasicValueEnum>`, and `None` means control does not continue past
//! it — a `return`, a `break`, a block that ended in one. The current basic
//! block already has a terminator at that point, and appending to a terminated
//! block is invalid IR that LLVM only notices at verification, a long way from
//! the mistake. Threading it through the return type makes the compiler refuse
//! to let a caller ignore it.
//!
//! **A boxed value is owned by whoever holds it.** `khora-perceus` says a read
//! of a boxed local `dup`s and a block `drop`s what it declared; the gaps it
//! leaves — a discarded temporary, a `match` scrutinee, an overwritten `mut`
//! binding, an early exit past a scope — are closed here, because they are
//! properties of the control flow graph rather than of the HIR. Every one of
//! them is marked in the code below.
//!
//! # Reading the `build_*` calls
//!
//! inkwell returns `Result<_, BuilderError>` from every builder method and this
//! module unwraps all of them. A `BuilderError` means an unset insertion point
//! or mismatched operand types: both are bugs in this file, never in the
//! program being compiled. Anything a Khora program can cause goes through
//! [`Backend::error`] and comes back as a `HirError`.

use std::collections::HashMap;

use inkwell::basic_block::BasicBlock;
use inkwell::types::{BasicType, BasicTypeEnum};
use inkwell::values::{BasicValueEnum, FunctionValue, IntValue, PointerValue};
use inkwell::{AddressSpace, AtomicOrdering, AtomicRMWBinOp, IntPredicate};

use khora_hir::body::{
    BinOp, Body, Expr, ExprId, Literal, LocalId, MatchArm, Pat, PatId, Stmt, UnOp,
};
use khora_perceus::{is_boxed, RcPlan};
use khora_types::{BodyTypes, Type, VariantInfo};
use text_size::TextRange;

use crate::backend::{
    can_raise, evidence_of, Backend, CLOSURE_ADAPTER_TAG, CLOSURE_CAPTURE_BASE,
};
use crate::runtime::{self, FIELD_WORD, STRING_BYTES_OFFSET, STRING_LEN_FIELD, STRING_TAG};

/// The result of lowering an expression: `None` when control diverged.
type Flow<'ctx> = Option<BasicValueEnum<'ctx>>;

/// Emits the body of one Khora function.
pub(crate) fn emit_function<'ctx>(
    be: &mut Backend<'ctx>,
    name: &str,
    body: &Body,
    plan: Option<&RcPlan>,
    types: &BodyTypes,
    mono: &khora_types::mono::Instances,
) {
    let Some(function) = be.definition(name) else { return };
    // `name` is a specialization's symbol, so the signature has to come from
    // the instance table rather than from the source signatures.
    let Some(signature) = be.signature_of(name) else { return };
    let empty = RcPlan::default();

    let entry = be.ctx.append_basic_block(function, "entry");
    be.builder.position_at_end(entry);

    let mut lower = Lower {
        be,
        body,
        plan: plan.unwrap_or(&empty),
        types,
        mono,
        function,
        owner: name.to_string(),
        ret: signature.ret.clone(),
        raises: can_raise(&signature),
        slots: HashMap::new(),
        scopes: Vec::new(),
        reuse: None,
        loops: Vec::new(),
        catches: Vec::new(),
        incoming: HashMap::new(),
        aborted: false,
    };

    lower.allocate_slots();
    lower.take_evidence(&signature);
    lower.bind_parameters();

    let value = match body.root {
        Some(root) => lower.expr(root),
        None => Some(lower.be.unit_value()),
    };
    // The scope `take_evidence` opened for the capabilities this body cannot
    // name. Left on the ordinary path here; `unwind_to` covers a `return` and
    // a `raise`, which is why they are a scope rather than a list.
    if value.is_some() {
        lower.leave_scope();
    }
    lower.finish(value);
}

/// Emits the function one lambda was lifted to.
///
/// The lambda's body is an expression in the *enclosing* function's arena, so
/// this is the same `Body`, the same `BodyTypes` and the same reference-counting
/// plan — only the entry point differs. Captures are read out of the closure
/// object into the slots the body already expects them in, which is what makes
/// the body's own lowering need no notion of a capture at all.
pub(crate) fn emit_closure<'ctx>(
    be: &mut Backend<'ctx>,
    site: &crate::backend::ClosureSite,
    body: &Body,
    plan: Option<&RcPlan>,
    types: &BodyTypes,
    mono: &khora_types::mono::Instances,
) {
    let Some(function) = be.definition(&site.symbol) else { return };
    let Expr::Lambda { params, body: root, .. } = body.expr(site.expr).clone() else { return };
    let empty = RcPlan::default();

    let entry = be.ctx.append_basic_block(function, "entry");
    be.builder.position_at_end(entry);

    let mut lower = Lower {
        be,
        body,
        plan: plan.unwrap_or(&empty),
        types,
        mono,
        function,
        owner: site.owner.clone(),
        ret: site.ret.clone(),
        raises: !matches!(
            &site.raises,
            Type::Row { fields, tail } if fields.is_empty() && tail.is_none()
        ),
        slots: HashMap::new(),
        scopes: Vec::new(),
        reuse: None,
        loops: Vec::new(),
        catches: Vec::new(),
        incoming: HashMap::new(),
        aborted: false,
    };

    lower.allocate_slots();

    // Captures first: the closure lends them for the duration of the call, so
    // they are stored without a `dup` and released by the closure's own drop
    // glue rather than here.
    let closure = function.get_nth_param(0).expect("a lifted lambda takes its closure");
    for (index, (local, ty)) in site.captures.iter().enumerate() {
        let Some(slot) = lower.slots.get(local).copied() else { continue };
        let value = lower.load_field(
            closure.into_pointer_value(),
            index + CLOSURE_CAPTURE_BASE,
            ty,
        );
        lower.be.builder.build_store(slot, value).expect("storing a capture");
    }

    // The lambda's own parameters are owned by it, exactly as a function's are.
    // They sit in a scope of their own so that every path out — falling off the
    // end, or an early `return` — releases them.
    let mut owned = Vec::new();
    for (index, pat) in params.iter().enumerate() {
        let Pat::Bind(local) = body.pat(*pat).clone() else { continue };
        let Some(slot) = lower.slots.get(&local).copied() else { continue };
        let Some(value) = function.get_nth_param(index as u32 + 1) else { continue };
        lower.be.builder.build_store(slot, value).expect("storing a lambda parameter");
        if is_boxed(types.local(local)) {
            owned.push(Cleanup::Local(local));
        }
    }

    // Then whatever the closure is *handed* rather than captured: a capability
    // that did not exist where the lambda was written, supplied by whoever
    // calls it. `docs/design/capability-passing.md`.
    //
    // No binding names these — the source never wrote them down, which is the
    // whole point — so they go straight into `incoming`, where
    // `evidence_from_row` already looks for a capability a `with 'r` clause
    // forwarded. Owned like every other argument, and released here because
    // there is no local for the reference-counting plan to hang them on.
    let handed = crate::backend::evidence_of(&site.requires_signature());
    let base = params.len() + 1;
    for (offset, (label, ty)) in handed.into_iter().enumerate() {
        let Some(value) = function.get_nth_param((base + offset) as u32) else { continue };
        if is_boxed(&ty) {
            owned.push(Cleanup::Temp(value, ty));
        }
        lower.incoming.insert(label, value);
    }
    lower.scopes.push(owned);

    let value = lower.expr(root);
    if value.is_some() {
        lower.leave_scope();
    }
    lower.finish(value);
}

/// One scope's worth of pending releases.
///
/// A scope is left on more than one path — normally, by `return`, by `break` —
/// and the releases have to happen on all of them, so they are held here rather
/// than emitted at the point the scope is created.
#[derive(Clone)]
enum Cleanup<'ctx> {
    /// A local whose slot owns a reference.
    Local(LocalId),
    /// An owned temporary: a `match` scrutinee, held while the guards run.
    Temp(BasicValueEnum<'ctx>, Type),
}

/// The value a set of arms is dispatching on, and who releases it.
///
/// A `match` releases it at the head of each arm, so that the count can reach
/// zero before the arm's own constructor — `docs/design/reuse.md` §2. A `catch`
/// does not: it has no static type for the error and releases by the runtime
/// tag once the arms are done, in `lower_catch`.
#[derive(Clone)]
struct Scrutinee<'ctx> {
    value: BasicValueEnum<'ctx>,
    ty: Type,
    released_by_arms: bool,
}

/// A closure call that has happened, before anything is decided about its tag.
///
/// The scope holding the closure's reference is still open — the caller closes
/// it — because what happens next differs: propagating an error leaves through
/// a branch that must see the scope, and capturing one does not.
struct Invoked<'ctx> {
    raw: Option<BasicValueEnum<'ctx>>,
    fallible: bool,
}

/// What a `Type::Fn` says about how to call it.
///
/// The same four facts a `Signature` gives a direct call, which is the point:
/// a value's calling convention follows its type the way a named function's
/// follows its signature, so neither has a shape the other cannot express.
struct FnShape {
    params: Vec<Type>,
    ret: Type,
    requires: Type,
    raises: Type,
}

impl FnShape {
    fn of(ty: &Type) -> Option<FnShape> {
        match ty {
            Type::Fn { params, ret, requires, raises } => Some(FnShape {
                params: params.clone(),
                ret: (**ret).clone(),
                requires: (**requires).clone(),
                raises: (**raises).clone(),
            }),
            _ => None,
        }
    }
}

/// The labelled entries of a row, or none for anything that is not one.
fn row_fields(ty: &Type) -> Vec<(String, Type)> {
    match ty {
        Type::Row { fields, .. } => fields.clone(),
        _ => Vec::new(),
    }
}

/// Whether a row asks for nothing at all.
fn row_is_empty(ty: &Type) -> bool {
    match ty {
        Type::Row { fields, tail } => fields.is_empty() && tail.is_none(),
        _ => true,
    }
}

/// A `catch` whose operand is being lowered.
///
/// While one of these is on the stack, an error leaving a `!` inside the
/// operand goes to `handler` instead of out of the function. The two phis
/// collect it, one incoming edge per `!` — the operand may contain several,
/// and they are all the same branch as far as the arms are concerned.
struct CatchFrame<'ctx> {
    handler: BasicBlock<'ctx>,
    which: inkwell::values::PhiValue<'ctx>,
    word: inkwell::values::PhiValue<'ctx>,
    /// Scopes above this index were opened inside the operand, so they are
    /// released on the way to the handler. Scopes below it belong to the
    /// enclosing function, which the error is no longer leaving.
    scope_depth: usize,
}

/// A loop that `break` and `continue` can target.
struct LoopFrame<'ctx> {
    continue_to: BasicBlock<'ctx>,
    break_to: BasicBlock<'ctx>,
    /// Scopes above this index belong to the loop and are left by a `break`.
    scope_depth: usize,
    /// Whether anything actually branches to `break_to`, which decides whether
    /// the block after the loop is reachable at all.
    breaks: usize,
}

struct Lower<'a, 'ctx> {
    be: &'a mut Backend<'ctx>,
    body: &'a Body,
    plan: &'a RcPlan,
    types: &'a BodyTypes,
    mono: &'a khora_types::mono::Instances,
    function: FunctionValue<'ctx>,
    /// The symbol being emitted. A lambda site is keyed by this plus the
    /// expression, because one lambda in a generic body becomes a different
    /// closure in every specialization.
    owner: String,
    ret: Type,
    /// Whether *this* function returns a tagged pair.
    ///
    /// Not `can_raise(signature_of(owner))`: a lifted lambda's `owner` is the
    /// function it was written inside, whose `raises` row is not the lambda's.
    /// A closure type carries no row, so a lifted lambda cannot raise at all
    /// — and reading the enclosing signature made it emit its enclosing
    /// function's calling convention over its own.
    raises: bool,
    slots: HashMap<LocalId, PointerValue<'ctx>>,
    scopes: Vec<Vec<Cleanup<'ctx>>>,
    /// A cell a `match` arm was handed back, and the constructor it is for.
    ///
    /// Set at the head of an arm that reaches a constructor unconditionally,
    /// spent by that exact expression, and empty everywhere else. There is
    /// never more than one outstanding: an arm nested inside another arm's
    /// constructor arguments would be a second, and `reuse_site` declines to
    /// promise one while a promise is already open.
    reuse: Option<(ExprId, PointerValue<'ctx>)>,
    loops: Vec<LoopFrame<'ctx>>,
    catches: Vec<CatchFrame<'ctx>>,
    /// The evidence this function was handed, by label.
    ///
    /// Needed because a `with 'r` clause names nothing: the body has no
    /// binding for `ledger`, cannot mention it, and only has to forward it.
    /// Which labels `'r` turned out to be is a fact about the *specialization*,
    /// so it is read from the instantiated signature rather than from the
    /// source, and it is what makes a row-polymorphic function able to pass
    /// its caller's capabilities to a function value it was given.
    incoming: HashMap<String, BasicValueEnum<'ctx>>,
    /// Set once something could not be lowered. Everything after is skipped:
    /// the module is discarded anyway, and continuing would build IR against
    /// values that were never produced.
    aborted: bool,
}

// One module per lowering responsibility. This was 5,872 lines in one file and
// one `impl Lower` of 5,500, whose own section banners had grown to the point
// where "Calls" covered three thousand lines of string, array, effect and
// closure work. Roadmap 9.6.2.
//
// Rust lets an inherent impl be split across modules of one crate, so each file
// below opens `impl<'ctx> Lower<'_, 'ctx>` again and adds its own methods. The
// struct and everything shared stay here, where a child module can see them.
mod array;
mod calls;
mod closure;
mod control;
mod effects;
mod expr;
mod failure;
mod num;
mod objects;
mod operators;
mod pattern;
mod rc;
mod text;

impl<'ctx> Lower<'_, 'ctx> {
    /// Gives every local a stack slot, zeroed.
    ///
    /// All of them in the entry block, including the ones a `match` arm binds
    /// deep inside a loop: an `alloca` reached repeatedly grows the stack every
    /// time, so an allocation inside a loop body is a leak with a different
    /// name. Zeroing matters for the boxed ones — a null slot is what makes a
    /// `drop` on a path that never reached the binding a no-op instead of a
    /// wild free.
    fn allocate_slots(&mut self) {
        let locals: Vec<(LocalId, String)> =
            self.body.locals().map(|(id, l)| (id, l.name.clone())).collect();

        for (id, name) in locals {
            let ty = self.types.local(id).clone();
            let Some(llvm_ty) = self.be.llvm_type(&ty) else {
                let range = self.body.local(id).range;
                // **An unknown type and an unsupported one are different
                // problems**, and saying the second when it is the first sends
                // the reader to the backend's capabilities instead of to their
                // own missing annotation. It happened twice while writing
                // `std/db.kh`: a `transaction` whose body only ever returned
                // `Err` left its success type undetermined, and the message
                // talked about what phase 2 can represent — which was not the
                // matter at all. `vision.md` non-negotiable 4.
                let message = if matches!(ty, Type::Unknown | Type::Var(_)) {
                    format!(
                        "the type of `{name}` is not determined — nothing in this function \
                         says what it is. An annotation on the binding, or on the call that \
                         produces it, is what settles it; a generic whose result is never used \
                         is the usual reason"
                    )
                } else {
                    format!(
                        "`{name}` has type `{ty}`, which the backend cannot represent yet; \
                         phase 2 handles `Int`, `Bool`, `String`, `()` and ADTs"
                    )
                };
                self.fail(message, range);
                continue;
            };
            let slot = self.be.builder.build_alloca(llvm_ty, &name).expect("a local slot");
            let zero = self.be.zero_value(&ty);
            self.be.builder.build_store(slot, zero).expect("zeroing a local slot");
            // Named for the debugger while the slot and its type are both in
            // hand. `dbg.declare` rather than `dbg.value` because an `alloca`
            // has one address for the whole frame — see `crate::debug`.
            if self.be.debug.is_some() {
                let range = self.body.local(id).range;
                let block = self.be.builder.get_insert_block().expect("a block");
                let ctx = self.be.ctx;
                if let Some(debug) = self.be.debug.as_mut() {
                    debug.declare_local(&name, slot, &ty, range, ctx, block);
                }
            }
            self.slots.insert(id, slot);
        }
    }

    /// Records the evidence parameters this specialization receives.
    ///
    /// Capabilities follow the written parameters, in label order, which is
    /// the order `function_type` declared them in. Which labels those are
    /// comes from the *instantiated* signature: a body written `with 'r` has
    /// no labels of its own, and its caller's are only known once `'r` is.
    fn take_evidence(&mut self, signature: &khora_types::Signature) {
        let base = self.body.params.len();
        let named: Vec<String> =
            self.body.evidence.iter().map(|(label, _)| label.clone()).collect();

        // Evidence is passed owned, so this frame has to release it. The ones
        // the source named are locals, and the reference-counting plan already
        // covers them; the ones a `with 'r` clause brought have no binding to
        // hang a plan on, so they are held here as temporaries of the
        // outermost scope and released on every path out.
        let mut owned = Vec::new();
        for (offset, (label, ty)) in evidence_of(signature).into_iter().enumerate() {
            let Some(value) = self.function.get_nth_param((base + offset) as u32) else {
                continue;
            };
            if !named.contains(&label) && is_boxed(&ty) {
                owned.push(Cleanup::Temp(value, ty));
            }
            self.incoming.insert(label, value);
        }
        self.scopes.push(owned);
    }

    /// Moves the incoming arguments into their slots.
    ///
    /// Parameters are owned by the callee, so nothing is `dup`ed here; the
    /// matching `drop` is in the plan's releases for the outermost block.
    fn bind_parameters(&mut self) {
        for (index, pat) in self.body.params.clone().into_iter().enumerate() {
            let Pat::Bind(local) = self.body.pat(pat).clone() else { continue };
            let Some(slot) = self.slots.get(&local).copied() else { continue };
            let Some(value) = self.function.get_nth_param(index as u32) else { continue };
            self.be.builder.build_store(slot, value).expect("storing a parameter");
        }

        // A capability the source *named* also gets a slot, so `ledger.balance`
        // reads it like any other binding. Matched by label rather than by
        // position: the signature's order is the contract, and a body that
        // named only some of them would otherwise bind the wrong ones.
        for (label, pat) in self.body.evidence.clone() {
            let Pat::Bind(local) = self.body.pat(pat).clone() else { continue };
            let Some(slot) = self.slots.get(&local).copied() else { continue };
            let Some(value) = self.incoming.get(&label).copied() else { continue };
            self.be.builder.build_store(slot, value).expect("storing a capability");
        }
    }

    /// Emits the function's `ret`, and repairs the IR if lowering gave up.
    fn finish(&mut self, value: Flow<'ctx>) {
        // A fallible function always returns the tagged pair, so falling off
        // the end of the body is the *ok* case rather than a bare return.
        if self.raises {
            if let Some(value) = value {
                self.return_ok(value);
            } else if self.here().get_terminator().is_none() {
                let zero = self.be.ctx.i64_type().const_zero();
                self.return_ok(zero.into());
            }
            return;
        }
        if let Some(value) = value {
            let expected = self.function.get_type().get_return_type();
            match (&self.ret, expected) {
                (Type::Unit, _) => {
                    self.be.builder.build_return(None).expect("returning unit");
                }
                (_, Some(expected)) if expected == value.get_type() => {
                    self.be.builder.build_return(Some(&value)).expect("returning a value");
                }
                _ => {
                    // Reachable when a body's type is `Unknown` — a `loop` used
                    // as a value, say. The checker accepts `Unknown` anywhere,
                    // so it cannot have caught this.
                    let ret = self.ret.clone();
                    let range = self.body.root.map(|r| self.body.range(r)).unwrap_or_default();
                    self.fail(
                        format!(
                            "this body does not produce the `{ret}` its signature promises in a \
                             form the backend can return; annotate it or restructure the \
                             expression it ends with"
                        ),
                        range,
                    );
                }
            }
        }

        // A function whose lowering aborted can be left with blocks that were
        // created but never terminated. The module is about to be discarded,
        // but it still passes through inkwell, so leave it structurally sound.
        if self.aborted {
            for block in self.function.get_basic_blocks() {
                if block.get_terminator().is_none() {
                    self.be.builder.position_at_end(block);
                    self.be.builder.build_unreachable().expect("sealing a block");
                }
            }
        }
    }

    // -----------------------------------------------------------------------
    // Small helpers
    // -----------------------------------------------------------------------

    fn fail(&mut self, message: impl Into<String>, range: TextRange) -> Flow<'ctx> {
        self.be.error(message, range);
        self.aborted = true;
        None
    }

    fn block(&mut self, name: &str) -> BasicBlock<'ctx> {
        self.be.ctx.append_basic_block(self.function, name)
    }

    fn here(&self) -> BasicBlock<'ctx> {
        self.be.builder.get_insert_block().expect("the builder is always positioned")
    }

    fn at(&self, block: BasicBlock<'ctx>) {
        self.be.builder.position_at_end(block);
    }

    fn br(&self, target: BasicBlock<'ctx>) {
        self.be.builder.build_unconditional_branch(target).expect("an unconditional branch");
    }

    /// Gives the scheduler a chance to take the worker back.
    ///
    /// Emitted at loop back-edges, which is the only place a Khora program can
    /// run forever without doing anything the runtime already sees. A
    /// cancellation is observed at `!` in something that can raise, so a
    /// function with no error row has no cancellation point — correct as a
    /// language rule, and on M:N it means an infallible loop would own a
    /// worker until the process ended. `docs/design/scheduler.md` §1.
    ///
    /// **Nothing is emitted for a program that cannot spawn.** The compiler
    /// already proves that to decide whether reference counting is atomic, and
    /// the same proof says there is nobody to be fair to. So the usual program
    /// pays exactly nothing for this.
    fn safepoint(&mut self) {
        if self.be.single_threaded {
            return;
        }
        self.be
            .builder
            .build_call(self.be.rt.safepoint, &[], "")
            .expect("a safepoint");
    }

    /// An `alloca` in the entry block, whatever block we are currently in.
    fn entry_slot(&self, ty: BasicTypeEnum<'ctx>, name: &str) -> PointerValue<'ctx> {
        let current = self.be.builder.get_insert_block();
        let entry = self.function.get_first_basic_block().expect("an entry block");
        match entry.get_first_instruction() {
            Some(first) => self.be.builder.position_before(&first),
            None => self.be.builder.position_at_end(entry),
        }
        let slot = self.be.builder.build_alloca(ty, name).expect("a temporary slot");
        if let Some(block) = current {
            self.be.builder.position_at_end(block);
        }
        slot
    }

    /// A slot for the value of a branching expression, or `None` when there is
    /// no value worth keeping.
    fn result_slot(&self, ty: &Type) -> Option<PointerValue<'ctx>> {
        match ty {
            Type::Unit | Type::Never | Type::Unknown | Type::Fn { .. } => None,
            other => self.be.llvm_type(other).map(|t| self.entry_slot(t, "result")),
        }
    }

    fn store_result(&self, slot: Option<PointerValue<'ctx>>, value: BasicValueEnum<'ctx>) {
        if let Some(slot) = slot {
            self.be.builder.build_store(slot, value).expect("storing a branch result");
        }
    }

    fn load_result(&self, slot: Option<PointerValue<'ctx>>, ty: &Type) -> BasicValueEnum<'ctx> {
        match (slot, self.be.llvm_type(ty)) {
            (Some(slot), Some(llvm_ty)) => self
                .be
                .builder
                .build_load(llvm_ty, slot, "joined")
                .expect("loading a branch result"),
            _ => self.be.unit_value(),
        }
    }
}


/// Parses an integer literal.
///
/// Underscores are stripped and the radix prefixes the lexer admits are
/// honoured. The value is parsed as `i64` because that is what `Int` is; a
/// literal that does not fit is a diagnostic rather than a wrap.
fn parse_int(text: &str) -> Option<i64> {
    let cleaned: String = text.chars().filter(|c| *c != '_').collect();
    let (radix, digits) = match cleaned.get(..2) {
        Some("0x") | Some("0X") => (16, &cleaned[2..]),
        Some("0b") | Some("0B") => (2, &cleaned[2..]),
        Some("0o") | Some("0O") => (8, &cleaned[2..]),
        _ => (10, cleaned.as_str()),
    };
    i64::from_str_radix(digits, radix).ok()
}

/// The width and signedness of an integer type named as a path owner.
///
/// `Int` and `I64` are the same 64-bit signed integer, so `I64::to_u8` has to
/// resolve exactly as `Int::to_u8` does — see `IntKind`.
fn int_owner(owner: &str) -> Option<(u32, bool)> {
    match owner {
        "Int" | "I64" => Some((64, true)),
        other => khora_types::IntKind::parse(other).map(|k| (k.bits.into(), k.signed)),
    }
}
