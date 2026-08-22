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
use inkwell::module::Linkage;
use inkwell::types::{BasicType, BasicTypeEnum};
use inkwell::values::{BasicValueEnum, FunctionValue, IntValue, PointerValue};
use inkwell::{AddressSpace, IntPredicate};

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
    /// An owned temporary: a `match` scrutinee, which the arms borrow out of.
    Temp(BasicValueEnum<'ctx>, Type),
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

impl<'ctx> Lower<'_, 'ctx> {
    // -----------------------------------------------------------------------
    // Frame setup
    // -----------------------------------------------------------------------

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
                self.fail(
                    format!(
                        "`{name}` has no type the backend can represent; phase 2 handles `Int`, \
                         `Bool`, `String`, `()` and ADTs"
                    ),
                    range,
                );
                continue;
            };
            let slot = self.be.builder.build_alloca(llvm_ty, &name).expect("a local slot");
            let zero = self.be.zero_value(&ty);
            self.be.builder.build_store(slot, zero).expect("zeroing a local slot");
            self.slots.insert(id, slot);
        }
    }

    /// Moves the incoming arguments into their slots.
    ///
    /// Parameters are owned by the callee, so nothing is `dup`ed here; the
    /// matching `drop` is in the plan's releases for the outermost block.
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

    // -----------------------------------------------------------------------
    // Reference counting
    // -----------------------------------------------------------------------

    /// `khora_dup(value)`, for a value that must outlive the expression that
    /// produced it.
    fn dup(&mut self, value: BasicValueEnum<'ctx>) {
        let dup = self.be.rt.dup;
        self.be.builder.build_call(dup, &[value.into()], "").expect("a dup");
    }

    /// `khora_drop(value, drop_fields_for(ty))`. A no-op for a machine word.
    fn drop(&mut self, value: BasicValueEnum<'ctx>, ty: &Type) {
        if !is_boxed(ty) {
            return;
        }
        let glue = self.be.drop_glue(ty);
        let drop = self.be.rt.drop;
        self.be.builder.build_call(drop, &[value.into(), glue.into()], "").expect("a drop");
    }

    /// Releases everything owned by scopes at or above `depth`, innermost
    /// first, without popping them.
    ///
    /// Not popping is the point: this runs at a `return` or a `break`, which
    /// leaves the scopes on one path while the lowering of the enclosing
    /// expression carries on building the others.
    fn unwind_to(&mut self, depth: usize) {
        for level in (depth..self.scopes.len()).rev() {
            for cleanup in self.scopes[level].clone().into_iter().rev() {
                self.release(cleanup);
            }
        }
    }

    /// Releases and pops the innermost scope, on the path that reaches its end.
    fn leave_scope(&mut self) {
        let scope = self.scopes.pop().unwrap_or_default();
        for cleanup in scope.into_iter().rev() {
            self.release(cleanup);
        }
    }

    fn release(&mut self, cleanup: Cleanup<'ctx>) {
        match cleanup {
            Cleanup::Local(local) => {
                let ty = self.types.local(local).clone();
                let Some(slot) = self.slots.get(&local).copied() else { return };
                let Some(llvm_ty) = self.be.llvm_type(&ty) else { return };
                let value = self
                    .be
                    .builder
                    .build_load(llvm_ty, slot, "released")
                    .expect("loading a local to release");
                self.drop(value, &ty);

                // Null the slot afterwards. A scope inside a loop is left once
                // per iteration, and `break` on a later iteration leaves it
                // again before the binding has been reached — which would drop
                // the previous iteration's freed pointer a second time. The
                // runtime's null tolerance turns that into a no-op, but only if
                // the slot is actually null.
                let zero = self.be.zero_value(&ty);
                self.be.builder.build_store(slot, zero).expect("clearing a released slot");
            }
            Cleanup::Temp(value, ty) => self.drop(value, &ty),
        }
    }

    // -----------------------------------------------------------------------
    // Expressions
    // -----------------------------------------------------------------------

    fn expr(&mut self, id: ExprId) -> Flow<'ctx> {
        if self.aborted {
            return None;
        }
        let range = self.body.range(id);
        match self.body.expr(id).clone() {
            Expr::Unit => Some(self.be.unit_value()),
            Expr::Literal(lit) => {
                let ty = self.types.of(id).clone();
                self.literal(lit, &ty, range)
            }
            Expr::Local(local) => self.read_local(id, local, range),
            Expr::Path(resolution) => self.path(id, &resolution, range),
            Expr::Call { callee, args } => self.call(id, callee, &args, range),
            Expr::Binary { op, lhs, rhs } => self.binary(id, op, lhs, rhs, range),
            Expr::Unary { op, operand } => self.unary(op, operand, range),
            Expr::Assign { target, value } => self.assign(target, value, range),
            Expr::Block { stmts, tail } => self.lower_block(id, &stmts, tail),
            Expr::If { condition, then_branch, else_branch } => {
                self.lower_if(id, condition, then_branch, else_branch)
            }
            Expr::Match { scrutinee, arms } => self.lower_match(id, scrutinee, &arms, range),
            Expr::Catch { inner, arms } => self.lower_catch(id, inner, &arms, range),
            Expr::While { condition, body } => self.lower_while(condition, body),
            Expr::Loop { body } => self.lower_loop(body),
            Expr::Break(value) => self.lower_break(value, range),
            Expr::Continue => self.lower_continue(range),
            Expr::Return(value) => self.lower_return(value),
            Expr::Record { fields, .. } => self.build_record(id, &fields, range),
            Expr::Raise(error) => self.lower_raise(error, range),
            // `!` is the identity on values. The branch it stands for is
            // emitted by the call underneath, which knows it is marked.
            // `!` is a cancellation point as well as an error branch. The
            // check comes *before* the call, so a cancelled computation stops
            // rather than doing work it is about to throw away — and before
            // the arguments are evaluated, so there is nothing half-built to
            // leak on the way out.
            Expr::Try(inner) => {
                self.check_cancellation(range);
                self.expr(inner)
            }
            Expr::Lambda { .. } => self.make_closure(id, range),
            // Parameter 0 of a lifted lambda *is* the closure, and it is live
            // for the duration of the call because the caller holds it. No
            // capture, no reference count, no cycle.
            Expr::LambdaSelf => match self.function.get_nth_param(0) {
                Some(closure) => Some(closure),
                None => self.fail(
                    "a closure referred to itself outside a closure, which is a compiler bug",
                    range,
                ),
            },
            Expr::Field { base, name } => self.read_field(base, &name, range),
            Expr::List(_) => self.fail("list literals are not supported yet", range),
            Expr::Tuple(_) => self.fail("tuple literals are not supported yet", range),
            // The checker already rejected these, so reaching one means
            // `compile` ran with diagnostics it should have refused.
            Expr::Missing | Expr::Unresolved(_) => {
                self.fail("this expression did not survive the front end", range)
            }
        }
    }

    /// A literal, at the width the checker gave it.
    ///
    /// `ty` matters because an integer literal is not always an `Int`: the
    /// checker types it from context, so the `56` in `U8::wrapping_add(b, 56)`
    /// is a `U8` and has to be an `i8` here. Emitting `i64` regardless is a
    /// mismatch LLVM catches and the checker never would.
    fn literal(&mut self, lit: Literal, ty: &Type, range: TextRange) -> Flow<'ctx> {
        match lit {
            Literal::Int(text) => match parse_int(&text) {
                Some(value) => {
                    let bits = Self::int_shape(ty).map_or(64, |(bits, _)| bits);
                    // `sign_extend` is false because `value` is already the
                    // exact bit pattern; LLVM would otherwise re-extend a
                    // negative literal that has none of its bits to spare.
                    Some(self.be.int_width(bits).const_int(value as u64, false).into())
                }
                None => self.fail(format!("`{text}` does not fit in an `Int`"), range),
            },
            Literal::Bool(value) => {
                Some(self.be.ctx.bool_type().const_int(value as u64, false).into())
            }
            Literal::Str(text) => self.string_literal(&text),
            Literal::Float(text) => {
                let Ok(value) = text.parse::<f64>() else {
                    return self.fail(format!("`{text}` is not a number this target can hold"), range);
                };
                Some(self.be.ctx.f64_type().const_float(value).into())
            }
        }
    }

    /// Builds a heap `String` from a literal.
    ///
    /// Layout, since the runtime does not impose one: tag [`STRING_TAG`], field
    /// 0 is the byte length, and the bytes follow it immediately. The bytes are
    /// copied out of a private constant rather than pointed at, so that every
    /// pointer stored in a field is a Khora object and `drop_fields` never has
    /// to distinguish. A string owns nothing, so it is dropped with a null
    /// field routine.
    fn string_literal(&mut self, text: &str) -> Flow<'ctx> {
        let bytes = text.as_bytes();
        let len = bytes.len() as u64;

        let i64_type = self.be.ctx.i64_type();
        let alloc = self.be.rt.alloc;
        let object = self
            .be
            .builder
            .build_call(
                alloc,
                &[
                    i64_type.const_int(FIELD_WORD + len, false).into(),
                    self.be.ctx.i32_type().const_int(STRING_TAG, false).into(),
                ],
                "str",
            )
            .expect("allocating a string")
            .try_as_basic_value()
            .basic()
            .expect("khora_alloc returns a pointer")
            .into_pointer_value();

        let length_slot =
            runtime::field_pointer(self.be.ctx, &self.be.builder, object, STRING_LEN_FIELD);
        self.be
            .builder
            .build_store(length_slot, i64_type.const_int(len, false))
            .expect("storing a string length");

        if len > 0 {
            let array = self.be.ctx.i8_type().array_type(len as u32);
            let global = self.be.module.add_global(array, None, "kh$str");
            global.set_initializer(&self.be.ctx.const_string(bytes, false));
            global.set_constant(true);
            global.set_linkage(Linkage::Private);

            let destination = runtime::byte_offset(
                self.be.ctx,
                &self.be.builder,
                object,
                STRING_BYTES_OFFSET,
                "str.bytes",
            );
            // Alignment 1 on both sides. The destination is in fact 8-aligned,
            // but claiming more than is guaranteed here buys nothing a memcpy
            // of a handful of bytes would notice.
            let count = i64_type.const_int(len, false);
            self.be
                .builder
                .build_memcpy(destination, 1, global.as_pointer_value(), 1, count)
                .expect("copying string bytes");
        }

        Some(object.into())
    }

    fn read_local(&mut self, id: ExprId, local: LocalId, range: TextRange) -> Flow<'ctx> {
        let ty = self.types.local(local).clone();
        let Some(slot) = self.slots.get(&local).copied() else {
            let name = self.body.local(local).name.clone();
            return self.fail(format!("`{name}` has no storage, which is a compiler bug"), range);
        };
        let Some(llvm_ty) = self.be.llvm_type(&ty) else {
            let name = self.body.local(local).name.clone();
            return self.fail(format!("`{name}` has a type the backend cannot represent"), range);
        };

        let value =
            self.be.builder.build_load(llvm_ty, slot, "load").expect("reading a local").to_owned();

        // The plan decides this, not the type: the value outlives the read, so
        // it needs its own reference.
        if self.plan.needs_dup(id) {
            self.dup(value);
        }
        Some(value)
    }

    fn path(
        &mut self,
        id: ExprId,
        resolution: &khora_hir::Resolution,
        range: TextRange,
    ) -> Flow<'ctx> {
        match resolution {
            // A constructor with no payload is still an allocation: it has a
            // tag, and a tag lives in a header. Interning the nullary cases
            // would need the refcount to be saturating, which is a phase 6
            // conversation.
            khora_hir::Resolution::Variant { type_name, name, .. } => {
                let (owner, case) = (type_name.clone(), name.clone());
                self.construct(&owner, &case, &[], range)
            }
            // A named function used as a value becomes a closure that
            // captures nothing and forwards to it.
            khora_hir::Resolution::Item { name, .. }
                if matches!(self.types.of(id), Type::Fn { .. }) =>
            {
                let symbol =
                    self.mono.callee(&self.owner.clone(), id).unwrap_or_else(|| name.clone());
                self.function_value(&symbol, range)
            }
            khora_hir::Resolution::Item { name, .. } => self.fail(
                format!("`{name}` is not a value; only functions and constructors have one"),
                range,
            ),
            // `Applicative::pure(x)` in value position: the same wrapper a
            // named function gets, around whichever impl was selected.
            khora_hir::Resolution::TraitItem { .. } => match self.mono.callee(&self.owner.clone(), id) {
                Some(symbol) => self.function_value(&symbol, range),
                None => self.fail(
                    "this trait function was not resolved to an impl; that is a compiler bug",
                    range,
                ),
            },
            khora_hir::Resolution::Unsupported(what) => self.fail(what.to_string(), range),
        }
    }

    // -----------------------------------------------------------------------
    // Calls
    // -----------------------------------------------------------------------

    fn call(
        &mut self,
        site: ExprId,
        callee: ExprId,
        args: &[ExprId],
        range: TextRange,
    ) -> Flow<'ctx> {
        match self.body.expr(callee).clone() {
            Expr::Path(khora_hir::Resolution::Variant { type_name, name, .. }) => {
                self.construct(&type_name, &name, args, range)
            }
            Expr::Path(khora_hir::Resolution::TraitItem { owner, name }) => {
                // **A method somebody wrote wins over one the backend
                // implements.** An intrinsic is a *declaration the backend
                // fills in*, so the test is that nothing else filled it in
                // first — `Int::to_string` is written in `std::core`, and
                // keying `Int::` on the owner alone sent it to the two-argument
                // integer operations and asked a `String` to be an `i64`.
                //
                // The same rule `attempt` already needed, applied where it
                // belongs: once, before any of them.
                if let Some(symbol) = self.mono.callee(&self.owner.clone(), callee) {
                    if self.be.is_defined(&symbol) {
                        return self.call_named(&symbol, site, args, range);
                    }
                }
                if owner == runtime::REGION_TYPE {
                    return self.region_intrinsic(&name, args, range);
                }
                if owner == runtime::FIBER_TYPE {
                    return self.fiber_intrinsic(&name, args, range);
                }
                if owner == runtime::FIBERS_TYPE {
                    return self.nursery_intrinsic(&name, args, range);
                }
                if owner == runtime::SHARED_FN_TYPE {
                    return self.shared_fn_intrinsic(site, &name, args, range);
                }
                if owner == runtime::ARRAY_TYPE {
                    return self.array_intrinsic(site, &name, args, range);
                }
                if let Some(shape) = int_owner(&owner) {
                    return self.int_intrinsic(shape, &owner, &name, args, range);
                }
                if owner == "String" && name == "with_data" {
                    return self.with_data(site, args, range);
                }
                if owner == "String" && name == "with_c_string" {
                    return self.with_c_string(site, args, range);
                }
                if owner == "String" && name == "from_bytes" {
                    return self.string_from_bytes(args, range);
                }
                if owner == "Float" && name == "to_int" {
                    return self.float_to_int(args, range);
                }
                if owner == "String" && matches!(name.as_str(), "bytes" | "byte" | "byte_length")
                {
                    return self.string_intrinsic(&name, args, range);
                }
                if owner == "Ptr" && matches!(name.as_str(), "null" | "is_null") {
                    return self.ptr_intrinsic(&name, args, range);
                }
                match self.mono.callee(&self.owner.clone(), callee) {
                    Some(symbol) => self.call_named(&symbol, site, args, range),
                    None => self.fail(
                        format!("`{name}` was not resolved to an impl; that is a compiler bug"),
                        range,
                    ),
                }
            }
            Expr::Path(khora_hir::Resolution::Item { name, .. }) => {
                // A generic callee resolves to the specialization this call
                // site asked for; a concrete one keeps its own name.
                let symbol = self
                    .mono
                    .callee(&self.owner.clone(), callee)
                    .unwrap_or_else(|| name.clone());

                // An intrinsic is a *declaration the backend implements*, so
                // the test is that nothing else does. A program with its own
                // `attempt` — the tests in this repository have one — gets its
                // own, and the name means what it was written to mean.
                let is_intrinsic = !self.be.is_defined(&symbol) && args.len() == 1;
                if is_intrinsic && name == "print" {
                    self.print(args[0], range)
                } else if is_intrinsic && name == "assert" {
                    self.assert(args[0], range)
                } else if is_intrinsic && name == "attempt" {
                    self.attempt(site, args[0], range)
                } else {
                    self.call_named(&symbol, site, args, range)
                }
            }
            // `a.show()` — the receiver becomes the first argument, and which
            // impl runs was settled by monomorphization.
            //
            // Unless the callee is a *field* holding a function, which wins
            // over a method of the same name (D2). The checker decided that
            // already, and recorded it by typing the field access as a
            // function; monomorphization has nothing for such a site.
            Expr::Field { base, .. } => match self.mono.callee(&self.owner.clone(), callee) {
                Some(symbol) => {
                    let mut all = vec![base];
                    all.extend_from_slice(args);
                    self.call_named(&symbol, site, &all, range)
                }
                None if matches!(self.types.of(callee), Type::Fn { .. }) => {
                    let shape = FnShape::of(self.types.of(callee))
                        .expect("guarded by the match arm");
                    self.call_closure(site, callee, &shape, args, range)
                }
                None => self.fail(
                    "this method call was not resolved to an impl; that is a compiler bug",
                    range,
                ),
            },
            // A value of function type: a closure, called indirectly.
            _ if matches!(self.types.of(callee), Type::Fn { .. }) => {
                let shape =
                    FnShape::of(self.types.of(callee)).expect("guarded by the match arm");
                self.call_closure(site, callee, &shape, args, range)
            }
            _ => self.fail(
                "only a named function or a constructor can be called; there are no function \
                 values until closures land",
                range,
            ),
        }
    }

    /// `Region::open` and `Region::defer`.
    ///
    /// Intrinsics rather than externs for one reason: `defer` has to hand the
    /// runtime the closure's *drop routine* alongside the closure. A closure's
    /// routine is generated — one shared function switching on the site tag —
    /// so nothing but the code generator knows the pointer, and a Khora
    /// declaration has nowhere to write it. Everything else about a region is
    /// an ordinary reference-counted object.
    fn region_intrinsic(
        &mut self,
        name: &str,
        args: &[ExprId],
        range: TextRange,
    ) -> Flow<'ctx> {
        match (name, args) {
            // The program's own region. A reference, like any other: the
            // binding that takes it releases it, and the entry point releases
            // the one the runtime keeps once `main` has returned.
            ("root", []) => {
                let root = self.be.rt.region_root;
                let region = self
                    .be
                    .builder
                    .build_call(root, &[], "region.root")
                    .expect("taking the root region")
                    .try_as_basic_value()
                    .basic()
                    .expect("a region is a value");
                Some(region)
            }
            ("open", []) => {
                let open = self.be.rt.region_open;
                let region = self
                    .be
                    .builder
                    .build_call(open, &[], "region")
                    .expect("opening a region")
                    .try_as_basic_value()
                    .basic()
                    .expect("a region is a value");
                Some(region)
            }
            ("defer", [region_arg, finalizer]) => {
                let region_ty = self.types.of(*region_arg).clone();
                let region = self.expr(*region_arg)?;
                let closure = self.expr(*finalizer)?;

                // Both arrive owned, because the reference-counting plan reads
                // this as the ordinary call it is written as. The runtime keeps
                // the closure — it releases it after calling it — and only
                // borrows the region, so the region's reference is given back
                // here rather than leaked. Getting this backwards is a region
                // whose count never reaches zero and finalizers that never run.
                let glue = self.be.drop_glue(&Type::func(Vec::new(), Type::Unit));
                let defer = self.be.rt.region_defer;
                self.be
                    .builder
                    .build_call(defer, &[region.into(), closure.into(), glue.into()], "")
                    .expect("deferring a finalizer");
                self.drop(region, &region_ty);
                Some(self.be.unit_value())
            }
            _ => self.fail(
                format!("`Region::{name}` is not a region operation the backend knows"),
                range,
            ),
        }
    }

    /// `SharedFn::of` and `SharedFn::call`.
    ///
    /// **The wrapper is not there at runtime.** A `SharedFn<A, B, 'e>` *is* the
    /// closure — `of` returns its argument untouched and `call` is an ordinary
    /// closure call — because the whole of what the wrapper does happened in
    /// the checker, at the one line where the captures were visible. Paying for
    /// a proof at runtime would be paying twice.
    ///
    /// The shape `call` needs is read off the wrapper's own type arguments,
    /// which monomorphization has already made concrete: `SharedFn<A, B, 'e>`
    /// says the closure takes an `A`, gives back a `B` and fails with `'e`.
    fn shared_fn_intrinsic(
        &mut self,
        site: ExprId,
        name: &str,
        args: &[ExprId],
        range: TextRange,
    ) -> Flow<'ctx> {
        match (name, args) {
            ("of", [closure]) => Some(self.expr(*closure)?),
            ("call", [wrapper, argument]) => {
                let wrapped = self.types.of(*wrapper).clone();
                let Type::Adt { name: owner, args: parameters } = &wrapped else {
                    return self.fail(format!("`{wrapped}` is not a `SharedFn`"), range);
                };
                if owner != runtime::SHARED_FN_TYPE || parameters.len() < 3 {
                    return self.fail(format!("`{wrapped}` is not a `SharedFn`"), range);
                }
                let signature = FnShape {
                    params: vec![parameters[0].clone()],
                    ret: parameters[1].clone(),
                    // Always empty: a closure captures the capabilities it
                    // uses, so there is nothing left for a caller to supply.
                    requires: Type::empty_row(),
                    raises: parameters[2].clone(),
                };
                let closure = self.expr(*wrapper)?.into_pointer_value();
                let given = vec![self.expr(*argument)?];
                let invoked =
                    self.invoke_closure_at(site, *wrapper, closure, &signature, given, range)?;
                let ret = signature.ret.clone();
                self.after_invoke(invoked, &ret, range)
            }
            _ => self.fail(
                format!("`SharedFn::{name}` is not an operation the backend knows"),
                range,
            ),
        }
    }

    /// `Fiber::spawn`, `Fiber::join` and `Fiber::cancel`.
    ///
    /// Intrinsics for the same reason the region ones are: `spawn` hands the
    /// runtime a closure, and the runtime has to be told how to release it
    /// when the fiber finishes. Everything else about a fiber handle is an
    /// ordinary reference-counted object — including that releasing it joins,
    /// which is where structured concurrency comes from.
    fn fiber_intrinsic(
        &mut self,
        name: &str,
        args: &[ExprId],
        range: TextRange,
    ) -> Flow<'ctx> {
        match (name, args) {
            ("spawn", [body]) => {
                // Whether the thunk returns the tagged pair, which is how a
                // fiber says it was cancelled or that it failed. Read from the
                // thunk's own type: a closure carries its error row, so this
                // is a fact about the value rather than a guess about it.
                let fallible = match self.types.of(*body) {
                    Type::Fn { raises, .. } => !matches!(
                        &**raises,
                        Type::Row { fields, tail } if fields.is_empty() && tail.is_none()
                    ),
                    _ => false,
                };
                // Handed over, not lent: the fiber releases the closure when
                // it finishes, so this gives up the reference the plan gave it.
                let closure = self.expr(*body)?;
                let glue = self.be.drop_glue(&Type::func(Vec::new(), Type::Unit));
                // Null for a thunk that cannot fail; otherwise the trampoline
                // that takes its tagged return apart on this side of the
                // boundary. See `Backend::tagged_trampoline`.
                let call = if fallible {
                    self.be.tagged_trampoline(1).as_global_value().as_pointer_value()
                } else {
                    self.be.null_pointer()
                };
                let spawn = self.be.rt.fiber_spawn;
                let fiber = self
                    .be
                    .builder
                    .build_call(spawn, &[closure.into(), glue.into(), call.into()], "fiber")
                    .expect("spawning a fiber")
                    .try_as_basic_value()
                    .basic()
                    .expect("a fiber handle is a value");
                Some(fiber)
            }
            ("join", [fiber]) | ("cancel", [fiber]) => {
                let ty = self.types.of(*fiber).clone();
                let handle = self.expr(*fiber)?;
                let call = if name == "join" {
                    self.be.rt.fiber_join
                } else {
                    self.be.rt.fiber_cancel
                };
                self.be
                    .builder
                    .build_call(call, &[handle.into()], "")
                    .expect("acting on a fiber");
                // Borrowed, not consumed — the handle is still the caller's,
                // and the plan handed this frame an owned reference.
                self.drop(handle, &ty);
                Some(self.be.unit_value())
            }
            _ => self.fail(
                format!("`Fiber::{name}` is not a fiber operation the backend knows"),
                range,
            ),
        }
    }

    /// The bit and wrapping operations on `Int`.
    ///
    /// Methods rather than operators, for now. `^`, `&`, `|`, `<<` and `>>`
    /// are five new tokens and `>>` has to be told apart from the end of two
    /// nested type arguments; none of that is hard and none of it is what a
    /// hash function is waiting for.
    ///
    /// Wrapping arithmetic is here because ordinary arithmetic *traps* — see
    /// `checked_arithmetic`. A hash, a checksum and a PRNG are the places that
    /// genuinely want the other behaviour, and asking for it by name is how
    /// the trap stays the default without being in the way.
    /// The primitive integer operations, at whatever width the owner is.
    ///
    /// Three families, and the reason each exists:
    ///
    /// - **Wrapping arithmetic**, because ordinary arithmetic *traps* — see
    ///   `checked_arithmetic`. A hash, a checksum and a PRNG are the places
    ///   that genuinely want the other behaviour, and asking for it by name is
    ///   how the trap stays the default without being in the way.
    /// - **Bit operations**, which are what a hash is made of and what a wire
    ///   format is written in.
    /// - **Conversions**, which are always explicit, because there is no
    ///   implicit widening anywhere in the language and a narrowing that
    ///   happens on its own is how a length becomes 44.
    ///
    /// Every conversion goes through `Int`: `U8::of` and `U8::to_int` rather
    /// than a method for each of the forty-two ordered pairs. `U8` to `U32` is
    /// two steps, which is more to type and never wrong — and the pairs that
    /// deserve one step can be given one later without changing what these
    /// mean.
    fn int_intrinsic(
        &mut self,
        (bits, signed): (u32, bool),
        owner: &str,
        name: &str,
        args: &[ExprId],
        range: TextRange,
    ) -> Flow<'ctx> {
        // The conversions take one argument; everything else takes two.
        if name == "to_float" {
            let [only] = args else {
                return self.fail(format!("`{owner}::to_float` takes one number"), range);
            };
            let value = self.expr(*only)?.into_int_value();
            let converted = self
                .be
                .builder
                .build_signed_int_to_float(value, self.be.ctx.f64_type(), "to.float")
                .expect("converting an integer to a float");
            return Some(converted.into());
        }
        if matches!(name, "of" | "wrapping" | "to_int" | "wrapping_to_int") {
            let [only] = args else {
                return self.fail(format!("`{owner}::{name}` takes one argument"), range);
            };
            let value = self.expr(*only)?.into_int_value();
            return self.convert(value, (bits, signed), owner, name, range);
        }

        let [left, right] = args else {
            return self.fail(format!("`{owner}::{name}` takes two arguments"), range);
        };
        let l = self.expr(*left)?.into_int_value();
        let r = self.expr(*right)?.into_int_value();
        let b = &self.be.builder;
        let value = match name {
            "wrapping_add" => b.build_int_add(l, r, "wrapping.add"),
            "wrapping_sub" => b.build_int_sub(l, r, "wrapping.sub"),
            "wrapping_mul" => b.build_int_mul(l, r, "wrapping.mul"),
            "xor" => b.build_xor(l, r, "xor"),
            "and" => b.build_and(l, r, "and"),
            "or" => b.build_or(l, r, "or"),
            // Shifting by the width or more is undefined in LLVM, so the count
            // is masked. Silently, and deliberately: every shift would
            // otherwise need a branch, and there is no answer for `x << 8` on
            // a `U8` that is more right than any other.
            "shl" | "shr" => {
                let mask = self.be.int_width(bits).const_int(u64::from(bits - 1), false);
                let count = b.build_and(r, mask, "shift.count").expect("masking a shift");
                if name == "shl" {
                    b.build_left_shift(l, count, "shl")
                } else {
                    // Arithmetic for a signed type, so a negative number stays
                    // negative; logical for an unsigned one, which is what a
                    // hash wants and what `Int` could never express.
                    b.build_right_shift(l, count, signed, "shr")
                }
            }
            _ => {
                return self.fail(
                    format!("`{owner}::{name}` is not an integer operation the backend knows"),
                    range,
                )
            }
        };
        Some(value.expect("an integer operation").into())
    }

    /// One of the four conversions, between `Int` and a fixed-width type.
    ///
    /// `of` and `to_int` stop the program when the value does not fit, for the
    /// same reason `+` does: a number that silently becomes a different number
    /// is found in production rather than in a test. `wrapping` and
    /// `wrapping_to_int` are how to ask for truncation by name.
    fn convert(
        &mut self,
        value: IntValue<'ctx>,
        (bits, signed): (u32, bool),
        owner: &str,
        name: &str,
        range: TextRange,
    ) -> Flow<'ctx> {
        let i64_type = self.be.ctx.i64_type();
        let narrow = self.be.int_width(bits);
        let b = &self.be.builder;
        match name {
            // Into the fixed-width type, truncating.
            "wrapping" => {
                Some(b.build_int_truncate_or_bit_cast(value, narrow, "wrapping").ok()?.into())
            }
            // Out of it, widening — which for everything but `U64` is exact,
            // and needs no check.
            "wrapping_to_int" => {
                let wide = if signed {
                    b.build_int_s_extend_or_bit_cast(value, i64_type, "to.int")
                } else {
                    b.build_int_z_extend_or_bit_cast(value, i64_type, "to.int")
                };
                Some(wide.ok()?.into())
            }
            "to_int" => {
                let wide = if signed {
                    b.build_int_s_extend_or_bit_cast(value, i64_type, "to.int")
                } else {
                    b.build_int_z_extend_or_bit_cast(value, i64_type, "to.int")
                }
                .expect("widening to Int");
                // Only `U64` can hold a number `Int` cannot, and it does so
                // exactly when the same bits read as signed are negative.
                if !signed && bits == 64 {
                    let zero = i64_type.const_zero();
                    let ok = self
                        .be
                        .builder
                        .build_int_compare(IntPredicate::SGE, wide, zero, "fits.int")
                        .expect("range-checking a U64");
                    self.guard(ok, &format!("converting {owner} to Int"));
                }
                Some(wide.into())
            }
            // Into the fixed-width type, checked. The check is a round trip:
            // narrow it, widen it back the way the target's signedness says,
            // and require the same number. That is one rule for all fourteen
            // combinations rather than fourteen bounds written by hand.
            _ => {
                let narrowed = b
                    .build_int_truncate_or_bit_cast(value, narrow, "narrowed")
                    .expect("narrowing to a fixed-width integer");
                let back = if signed {
                    self.be.builder.build_int_s_extend_or_bit_cast(narrowed, i64_type, "back")
                } else {
                    self.be.builder.build_int_z_extend_or_bit_cast(narrowed, i64_type, "back")
                }
                .expect("widening back");
                let ok = self
                    .be
                    .builder
                    .build_int_compare(IntPredicate::EQ, back, value, "fits")
                    .expect("comparing the round trip");
                self.guard(ok, &format!("converting Int to {owner}"));
                let _ = range;
                Some(narrowed.into())
            }
        }
    }

    /// Continues only if `ok`; otherwise stops the program saying `what`.
    fn guard(&mut self, ok: IntValue<'ctx>, what: &str) {
        let good = self.block("in.range");
        let bad = self.block("out.of.range");
        self.be
            .builder
            .build_conditional_branch(ok, good, bad)
            .expect("branching on a range check");
        self.at(bad);
        self.trap(what);
        self.at(good);
    }

    /// `a + b` on two strings.
    ///
    /// Generated rather than a runtime call, for the same reason `String::bytes`
    /// is: the string layout is the code generator's business, and the runtime
    /// stays a function of the data it is handed. `khora_alloc` and two
    /// `memcpy`s are the whole of it.
    ///
    /// Both operands are released afterwards. Neither is reused even when one
    /// is empty — returning the other would be one fewer allocation and a
    /// second rule about when the result shares storage, and a string is
    /// immutable so nothing would notice, but nothing needs it yet either.
    fn concat(&mut self, left: BasicValueEnum<'ctx>, right: BasicValueEnum<'ctx>) -> Flow<'ctx> {
        let i64_type = self.be.ctx.i64_type();
        let (a, b) = (left.into_pointer_value(), right.into_pointer_value());
        let a_len = self.string_length(a);
        let b_len = self.string_length(b);
        let total = self
            .be
            .builder
            .build_int_add(a_len, b_len, "concat.len")
            .expect("adding two string lengths");

        let size = self
            .be
            .builder
            .build_int_add(total, i64_type.const_int(runtime::FIELD_WORD, false), "concat.size")
            .expect("sizing the result");
        let object = self
            .be
            .builder
            .build_call(
                self.be.rt.alloc,
                &[size.into(), self.be.ctx.i32_type().const_int(STRING_TAG, false).into()],
                "concat",
            )
            .expect("allocating a string")
            .try_as_basic_value()
            .basic()
            .expect("khora_alloc returns a pointer")
            .into_pointer_value();

        let length_slot =
            runtime::field_pointer(self.be.ctx, &self.be.builder, object, STRING_LEN_FIELD);
        self.be
            .builder
            .build_store(length_slot, total)
            .expect("storing the result's length");

        let out = runtime::byte_offset(
            self.be.ctx,
            &self.be.builder,
            object,
            STRING_BYTES_OFFSET,
            "concat.bytes",
        );
        for (source, len, offset) in [(a, a_len, None), (b, b_len, Some(a_len))] {
            let from = runtime::byte_offset(
                self.be.ctx,
                &self.be.builder,
                source,
                STRING_BYTES_OFFSET,
                "part.bytes",
            );
            let to = match offset {
                None => out,
                Some(at) => unsafe {
                    self.be
                        .builder
                        .build_in_bounds_gep(self.be.ctx.i8_type(), out, &[at], "concat.second")
                        .expect("addressing the second half")
                },
            };
            // Alignment 1: the bytes follow a length word so they are in fact
            // word-aligned, but the *second* copy starts wherever the first one
            // ended, which is any offset at all.
            self.be
                .builder
                .build_memcpy(to, 1, from, 1, len)
                .expect("copying a string");
        }

        self.drop(left, &Type::Str);
        self.drop(right, &Type::Str);
        Some(object.into())
    }

    /// `Array::with_data` and `String::with_data`: lend the elements to a body
    /// as a pointer and a count.
    ///
    /// **The lifetime is the call, and that is the whole design.** The obvious
    /// alternative — `Array::data(self) -> Ptr`, returning a bare pointer — is
    /// a dangling pointer waiting to happen: Perceus releases the array at its
    /// last *use*, and that use is the `data` call itself, so the array can be
    /// freed before the pointer is read. There is no scope that would fix it
    /// either. The innermost one is wrong for `if c { data(a) } else { data(b) }`,
    /// and the function's own is wrong for a loop, which would accumulate one
    /// live buffer per iteration. A body is the only bound that is right in all
    /// three.
    ///
    /// The array is released by a *scope* rather than by a statement after the
    /// call, so a body that raises does not leak it. That is errata 34, which
    /// has now been the answer three times.
    ///
    /// What this does not do is stop the pointer escaping — a body can write it
    /// into a `mut` field and read it later. That is the same line Rust draws:
    /// obtaining a pointer is safe, and what happens on the far side of the
    /// boundary is the binding author's responsibility. What it removes is the
    /// *accidental* case, the one the compiler creates behind your back.
    fn with_data(&mut self, site: ExprId, args: &[ExprId], range: TextRange) -> Flow<'ctx> {
        let [subject, body] = args else {
            return self.fail("`with_data` takes a body to lend the data to", range);
        };
        let subject_ty = self.types.of(*subject).clone();
        let Some(shape) = FnShape::of(self.types.of(*body)) else {
            return self.fail("`with_data` takes a function to run", range);
        };

        // A `String` lends its bytes; an `Array<A>` lends its elements, and the
        // count is the element count rather than a byte count — the same number
        // `Array::length` gives, so the two never disagree.
        let (elements, count) = match &subject_ty {
            Type::Str => {
                let object = self.expr(*subject)?.into_pointer_value();
                let length = self.string_length(object);
                let bytes = runtime::byte_offset(
                    self.be.ctx,
                    &self.be.builder,
                    object,
                    runtime::STRING_BYTES_OFFSET,
                    "str.bytes",
                );
                self.scopes.push(vec![Cleanup::Temp(object.into(), Type::Str)]);
                (bytes, length)
            }
            _ => {
                let element = self.array_element(&subject_ty, range)?;
                // An array of Khora objects is an array of counted pointers,
                // and handing those to a foreign function is the mistake the
                // whole boundary exists to prevent.
                if is_boxed(&element) {
                    return self.fail(
                        format!(
                            "an `Array<{element}>` holds reference-counted objects, so its \
                             elements cannot be lent across the C ABI — only an array of \
                             numbers can. `docs/design/ffi.md`"
                        ),
                        range,
                    );
                }
                let object = self.expr(*subject)?.into_pointer_value();
                let length_slot = runtime::field_pointer(
                    self.be.ctx,
                    &self.be.builder,
                    object,
                    runtime::ARRAY_LEN_FIELD,
                );
                let length = self
                    .be
                    .builder
                    .build_load(self.be.ctx.i64_type(), length_slot, "array.len")
                    .expect("reading an array's length")
                    .into_int_value();
                let elements = runtime::byte_offset(
                    self.be.ctx,
                    &self.be.builder,
                    object,
                    runtime::FIELD_OFFSET + runtime::ARRAY_HEADER_FIELDS * runtime::FIELD_WORD,
                    "array.elements",
                );
                self.scopes.push(vec![Cleanup::Temp(object.into(), subject_ty.clone())]);
                (elements, length)
            }
        };

        let closure = self.expr(*body)?.into_pointer_value();
        let Invoked { raw, fallible } = self.invoke_closure_at(
            site,
            *body,
            closure,
            &shape,
            vec![elements.into(), count.into()],
            range,
        )?;
        let result = if fallible {
            let tagged = raw.expect("a fallible body returns a tagged value");
            self.split_tagged(tagged, &shape.ret, range)?
        } else {
            match shape.ret {
                Type::Unit => self.be.unit_value(),
                _ => raw.unwrap_or_else(|| self.be.unit_value()),
            }
        };
        // The closure's own scope, then the one holding what was lent.
        self.leave_scope();
        self.leave_scope();
        Some(result)
    }

    /// Which of `<`, `>`, `<=`, `>=` an `Ordering` answers.
    ///
    /// `Ord::cmp` hands back a three-way answer, because one comparison should
    /// decide all four operators rather than four calls deciding them
    /// separately — `docs/design` has the argument at `Ordering`'s
    /// declaration. Reading it here is a tag comparison.
    ///
    /// `<=` is *not* `Less`-or-`Equal` spelled out; it is "not `Greater`". Two
    /// tests rather than one, and the same answer, so the cheaper one wins.
    ///
    /// The `Ordering` is a heap object like any other nullary variant, and it
    /// is released here — one allocation per comparison, which is exactly what
    /// phase 9's reuse analysis exists to remove and is not worth a special
    /// case before then.
    fn read_ordering(
        &mut self,
        op: BinOp,
        answer: BasicValueEnum<'ctx>,
        range: TextRange,
    ) -> Flow<'ctx> {
        let ordering = Type::adt("Ordering");
        let (Some((less, _)), Some((greater, _))) = (
            self.be.variant_of("Ordering", "Less"),
            self.be.variant_of("Ordering", "Greater"),
        ) else {
            self.drop(answer, &ordering);
            return self.fail(
                "`Ord::cmp` produces an `Ordering`, which has `Less`, `Equal` and `Greater`",
                range,
            );
        };

        let tag = runtime::load_tag(self.be.ctx, &self.be.builder, answer.into_pointer_value());
        let against = self.be.ctx.i32_type().const_int(
            u64::from(if matches!(op, BinOp::Lt | BinOp::Ge) { less } else { greater }),
            false,
        );
        // `<` is "is Less"; `>=` is "is not Less"; `>` is "is Greater"; `<=` is
        // "is not Greater". One tag read and one comparison for all four.
        let predicate = if matches!(op, BinOp::Lt | BinOp::Gt) {
            IntPredicate::EQ
        } else {
            IntPredicate::NE
        };
        let decided = self
            .be
            .builder
            .build_int_compare(predicate, tag, against, "ordered")
            .expect("reading an `Ordering`");
        self.drop(answer, &ordering);
        Some(decided.into())
    }

    /// The bytes of an `Array<U8>`, as a pointer and a length.
    ///
    /// Shared by `is_utf8` and `from_bytes`, which want the same three values
    /// out of the same object and would otherwise each work them out.
    fn byte_array(
        &mut self,
        array: ExprId,
        what: &str,
        range: TextRange,
    ) -> Option<(PointerValue<'ctx>, PointerValue<'ctx>, IntValue<'ctx>, Type)> {
        let array_ty = self.types.of(array).clone();
        let element = self.array_element(&array_ty, range)?;
        if element != Type::Fixed(khora_types::IntKind { signed: false, bits: 8 }) {
            self.fail(format!("`{what}` is about bytes, and `{element}` is not one"), range)?;
        }
        let object = self.expr(array)?.into_pointer_value();
        let length_slot = runtime::field_pointer(
            self.be.ctx,
            &self.be.builder,
            object,
            runtime::ARRAY_LEN_FIELD,
        );
        let length = self
            .be
            .builder
            .build_load(self.be.ctx.i64_type(), length_slot, "array.len")
            .expect("reading an array's length")
            .into_int_value();
        let elements = runtime::byte_offset(
            self.be.ctx,
            &self.be.builder,
            object,
            runtime::FIELD_OFFSET + runtime::ARRAY_HEADER_FIELDS * runtime::FIELD_WORD,
            "array.elements",
        );
        Some((object, elements, length, array_ty))
    }

    /// `Array::is_utf8`: whether these bytes are a `String`'s worth.
    ///
    /// Separate from the conversion, and paired with it the way `Array::length`
    /// is paired with `Array::get`: the check is how you avoid the trap, and
    /// having both means the *policy* — raise, substitute, give up — is written
    /// in Khora by whoever knows which is right.
    fn is_utf8(&mut self, array: ExprId, range: TextRange) -> Flow<'ctx> {
        let (object, elements, length, array_ty) =
            self.byte_array(array, "Array::is_utf8", range)?;
        let answer = self
            .be
            .builder
            .build_call(self.be.rt.utf8_valid, &[elements.into(), length.into()], "utf8")
            .expect("checking for UTF-8")
            .try_as_basic_value()
            .basic()
            .expect("khora_utf8_valid returns a _Bool")
            .into_int_value();
        self.drop(object.into(), &array_ty);
        // A C `_Bool` is one byte; Khora's `Bool` is an `i1`.
        let narrowed = self
            .be
            .builder
            .build_int_truncate_or_bit_cast(answer, self.be.ctx.bool_type(), "utf8.bit")
            .expect("narrowing a C bool");
        Some(narrowed.into())
    }

    /// `String::from_bytes`: the same bytes, as a `String`.
    ///
    /// **Stops the program if they are not UTF-8**, which is the same bargain
    /// `Array::get` makes about an index: the check exists — `Array::is_utf8` —
    /// and calling this without it is the mistake. Returning an `Option` was
    /// the alternative and would have put the decision in the wrong place: what
    /// to *do* about bytes that are not text depends entirely on where they
    /// came from, and only the caller knows.
    fn string_from_bytes(&mut self, args: &[ExprId], range: TextRange) -> Flow<'ctx> {
        let [array] = args else {
            return self.fail("`String::from_bytes` takes an `Array<U8>`", range);
        };
        let (object, elements, length, array_ty) =
            self.byte_array(*array, "String::from_bytes", range)?;

        let valid = self
            .be
            .builder
            .build_call(self.be.rt.utf8_valid, &[elements.into(), length.into()], "utf8")
            .expect("checking for UTF-8")
            .try_as_basic_value()
            .basic()
            .expect("khora_utf8_valid returns a _Bool")
            .into_int_value();
        let ok = self
            .be
            .builder
            .build_int_truncate_or_bit_cast(valid, self.be.ctx.bool_type(), "utf8.bit")
            .expect("narrowing a C bool");
        self.guard(ok, "these bytes are not UTF-8, so they are not a String");

        // The check split the block, so the addresses are recomputed on the
        // side of the branch that continues.
        let elements = runtime::byte_offset(
            self.be.ctx,
            &self.be.builder,
            object,
            runtime::FIELD_OFFSET + runtime::ARRAY_HEADER_FIELDS * runtime::FIELD_WORD,
            "array.elements",
        );
        let i64_type = self.be.ctx.i64_type();
        let size = self
            .be
            .builder
            .build_int_add(length, i64_type.const_int(runtime::FIELD_WORD, false), "str.size")
            .expect("sizing a string")
        ;
        let string = self
            .be
            .builder
            .build_call(
                self.be.rt.alloc,
                &[size.into(), self.be.ctx.i32_type().const_int(STRING_TAG, false).into()],
                "str",
            )
            .expect("allocating a string")
            .try_as_basic_value()
            .basic()
            .expect("khora_alloc returns a pointer")
            .into_pointer_value();
        let length_slot =
            runtime::field_pointer(self.be.ctx, &self.be.builder, string, STRING_LEN_FIELD);
        self.be
            .builder
            .build_store(length_slot, length)
            .expect("storing a string length");
        let into = runtime::byte_offset(
            self.be.ctx,
            &self.be.builder,
            string,
            STRING_BYTES_OFFSET,
            "str.bytes",
        );
        self.be
            .builder
            .build_memcpy(into, 1, elements, 1, length)
            .expect("copying the bytes");

        self.drop(object.into(), &array_ty);
        Some(string.into())
    }

    /// `String::with_c_string`: lend the bytes with a zero byte after them.
    ///
    /// Every function in the C library that takes a string takes a
    /// `const char *` and finds the end by looking for a zero. A Khora string
    /// knows its length instead and has no zero to find, so a copy is the only
    /// honest answer — and it is a copy either way, since a borrowed view could
    /// not have the extra byte appended to it.
    ///
    /// The copy is an `Array<U8>` of `len + 1`, which `khora_array_new` has
    /// already zeroed, so the terminator is written by not writing anything.
    /// It is released by the same scope discipline `with_data` uses, so a body
    /// that raises does not leak it.
    ///
    /// A string containing an interior zero is *not* rejected. C will see a
    /// shorter string than Khora has; that is what C strings are, and refusing
    /// it here would be inventing a rule the boundary does not have.
    fn with_c_string(&mut self, site: ExprId, args: &[ExprId], range: TextRange) -> Flow<'ctx> {
        let [subject, body] = args else {
            return self.fail("`with_c_string` takes a body to lend the string to", range);
        };
        let Some(shape) = FnShape::of(self.types.of(*body)) else {
            return self.fail("`with_c_string` takes a function to run", range);
        };

        let object = self.expr(*subject)?.into_pointer_value();
        let length = self.string_length(object);
        let bytes = runtime::byte_offset(
            self.be.ctx,
            &self.be.builder,
            object,
            runtime::STRING_BYTES_OFFSET,
            "str.bytes",
        );

        let i8_type = self.be.ctx.i8_type();
        let with_room = self
            .be
            .builder
            .build_int_add(length, self.be.ctx.i64_type().const_int(1, false), "c.len")
            .expect("room for the terminator");
        let buffer = self
            .be
            .builder
            .build_call(
                self.be.rt.array_new,
                &[
                    with_room.into(),
                    self.be.ctx.i64_type().const_zero().into(),
                    i8_type.const_int(1, false).into(),
                    i8_type.const_zero().into(),
                    self.be.null_pointer().into(),
                ],
                "c.string",
            )
            .expect("allocating a C string")
            .try_as_basic_value()
            .basic()
            .expect("an array is a value")
            .into_pointer_value();
        let elements = runtime::byte_offset(
            self.be.ctx,
            &self.be.builder,
            buffer,
            runtime::FIELD_OFFSET + runtime::ARRAY_HEADER_FIELDS * runtime::FIELD_WORD,
            "c.bytes",
        );
        self.be
            .builder
            .build_memcpy(elements, 1, bytes, 1, length)
            .expect("copying a string's bytes");

        // The string is done with as soon as its bytes are copied; the buffer
        // has to outlive the call, and a scope is what makes that true on the
        // raising path as well.
        self.drop(object.into(), &Type::Str);
        let buffer_ty = Type::Adt {
            name: runtime::ARRAY_TYPE.to_string(),
            args: vec![Type::Fixed(khora_types::IntKind { signed: false, bits: 8 })],
        };
        self.scopes.push(vec![Cleanup::Temp(buffer.into(), buffer_ty)]);

        let closure = self.expr(*body)?.into_pointer_value();
        let Invoked { raw, fallible } =
            self.invoke_closure_at(site, *body, closure, &shape, vec![elements.into()], range)?;
        let result = if fallible {
            let tagged = raw.expect("a fallible body returns a tagged value");
            self.split_tagged(tagged, &shape.ret, range)?
        } else {
            match shape.ret {
                Type::Unit => self.be.unit_value(),
                _ => raw.unwrap_or_else(|| self.be.unit_value()),
            }
        };
        self.leave_scope();
        self.leave_scope();
        Some(result)
    }

    /// `Float::to_int`: the whole part, and nothing rounded.
    ///
    /// **Truncates toward zero**, which is what C, Rust, Go and every machine
    /// instruction called "convert to integer" do — `2.9` is `2` and `-2.9` is
    /// `-2`. Rounding is a different question with four defensible answers, and
    /// a conversion that quietly picked one would be the wrong kind of
    /// surprise.
    ///
    /// A value too large for an `Int`, or a `NaN`, is *undefined* in LLVM. The
    /// saturating form is what makes it defined, and it is what this uses:
    /// out of range clamps to the nearest end, and a `NaN` is zero. Slower by
    /// one instruction and never nonsense.
    ///
    /// The other direction, `Int::to_float`, is exact for every integer up to
    /// 2^53 and rounds beyond it, which is IEEE's business rather than
    /// Khora's.
    fn float_to_int(&mut self, args: &[ExprId], range: TextRange) -> Flow<'ctx> {
        let [only] = args else {
            return self.fail("`Float::to_int` takes one number", range);
        };
        let value = self.expr(*only)?.into_float_value();
        let converted = self
            .be
            .builder
            .build_float_to_signed_int(value, self.be.ctx.i64_type(), "to.int")
            .expect("converting a float to an integer");
        Some(converted.into())
    }

    /// The two things a `Ptr` can do, which is deliberately all of them.
    ///
    /// A `Ptr` is an opaque machine address that came from the other side of
    /// the C ABI. It cannot be dereferenced, offset, or made from a Khora
    /// value — the last is what keeps a dangling one impossible, because the
    /// only pointers that exist are ones a foreign library handed over and
    /// whose lifetimes are that library's business.
    ///
    /// `null` and `is_null` are here because a C function that fails by
    /// returning `NULL` is not a rare case, and because passing `NULL` where a
    /// library allows it is ordinary. `docs/design/ffi.md`.
    fn ptr_intrinsic(&mut self, name: &str, args: &[ExprId], range: TextRange) -> Flow<'ctx> {
        match (name, args) {
            ("null", []) => Some(self.be.null_pointer().into()),
            ("is_null", [subject]) => {
                let value = self.expr(*subject)?.into_pointer_value();
                let answer = self
                    .be
                    .builder
                    .build_is_null(value, "is.null")
                    .expect("comparing a pointer against null");
                Some(answer.into())
            }
            _ => self.fail(format!("`Ptr::{name}` takes no arguments but `self`"), range),
        }
    }

    /// `String::byte_length`, `String::byte` and `String::bytes`.
    ///
    /// **A string's length is in bytes, and its index is a byte index.** Named
    /// so, because a `String` is UTF-8 and a character is one to four of these
    /// — a `length` that quietly meant one of the two would be wrong for half
    /// its callers and silent about which half. Anything that wants characters
    /// wants a decoder, and that is a library on top of this rather than a
    /// different meaning for the same word.
    ///
    /// `bytes` copies. A string is immutable and an array is not, so handing
    /// out a view would let one be edited through the other; and the two have
    /// different headers besides.
    ///
    /// There is deliberately no `from_bytes` yet. Going the other way has to
    /// answer what happens to bytes that are not UTF-8, and the honest answer
    /// is a `Result` rather than a trap — bytes off a socket are data, not a
    /// programmer's mistake. That wants the error channel wired into an
    /// intrinsic, which is phase 7's problem and not a decision to make in
    /// passing.
    fn string_intrinsic(&mut self, name: &str, args: &[ExprId], range: TextRange) -> Flow<'ctx> {
        let [subject, rest @ ..] = args else {
            return self.fail(format!("`String::{name}` takes a string"), range);
        };
        let object = self.expr(*subject)?.into_pointer_value();
        let length = self.string_length(object);
        let bytes = runtime::byte_offset(
            self.be.ctx,
            &self.be.builder,
            object,
            runtime::STRING_BYTES_OFFSET,
            "str.bytes",
        );

        let result = match (name, rest) {
            ("byte_length", []) => length.into(),
            ("byte", [index]) => {
                let at = self.expr(*index)?.into_int_value();
                self.check_index(at, length);
                // Recomputed after the check, because the check split the block
                // and the pointer above belongs to the one before it.
                let bytes = runtime::byte_offset(
                    self.be.ctx,
                    &self.be.builder,
                    object,
                    runtime::STRING_BYTES_OFFSET,
                    "str.bytes",
                );
                let slot = unsafe {
                    self.be
                        .builder
                        .build_in_bounds_gep(self.be.ctx.i8_type(), bytes, &[at], "byte.ptr")
                        .expect("addressing a byte")
                };
                self.be
                    .builder
                    .build_load(self.be.ctx.i8_type(), slot, "byte")
                    .expect("reading a byte")
            }
            ("bytes", []) => {
                let i8_type = self.be.ctx.i8_type();
                let array = self
                    .be
                    .builder
                    .build_call(
                        self.be.rt.array_new,
                        &[
                            length.into(),
                            self.be.ctx.i64_type().const_zero().into(),
                            i8_type.const_int(1, false).into(),
                            i8_type.const_zero().into(),
                            self.be.null_pointer().into(),
                        ],
                        "str.array",
                    )
                    .expect("allocating a byte array")
                    .try_as_basic_value()
                    .basic()
                    .expect("an array is a value")
                    .into_pointer_value();
                let elements = runtime::byte_offset(
                    self.be.ctx,
                    &self.be.builder,
                    array,
                    runtime::FIELD_OFFSET + runtime::ARRAY_HEADER_FIELDS * runtime::FIELD_WORD,
                    "array.elements",
                );
                // Alignment 1 on both sides: the destination is word-aligned and
                // the source usually is, but neither is *guaranteed* to be by
                // anything written down, and claiming an alignment the data does
                // not have is undefined behaviour rather than a slow copy.
                self.be
                    .builder
                    .build_memcpy(elements, 1, bytes, 1, length)
                    .expect("copying a string's bytes");
                array.into()
            }
            _ => {
                return self.fail(
                    format!("`String::{name}` is not a string operation the backend knows"),
                    range,
                )
            }
        };

        self.drop(object.into(), &Type::Str);
        Some(result)
    }

    /// The element type of the `Array<A>` this call is about.
    ///
    /// Read from the expression's own type rather than from the receiver,
    /// because `Array::new` has no receiver and its result is the array.
    fn array_element(&mut self, ty: &Type, range: TextRange) -> Option<Type> {
        match ty {
            Type::Adt { name, args } if name == runtime::ARRAY_TYPE => {
                Some(args.first().cloned().unwrap_or(Type::Unit))
            }
            other => {
                let other = other.clone();
                self.fail(format!("`{other}` is not an array"), range);
                None
            }
        }
    }

    /// The pointer to element `index`, with the bounds check in front of it.
    ///
    /// Checked rather than trusted, and a failure stops the program rather
    /// than reading whatever is next in memory. Same reasoning as trapping on
    /// integer overflow: a program that runs off its own array is wrong, and
    /// the useful thing is to say where.
    /// How many bytes one element of `ty` occupies inside an array.
    ///
    /// Read from the *type* rather than from LLVM's data layout, because it is
    /// also what the runtime is told and the two have to agree exactly. A
    /// pointer and an `Int` are a word; a fixed-width integer is its own
    /// width; a `Bool` is a byte, which it may as well be now that anything
    /// narrower than a word is possible at all.
    fn stride(ty: &Type) -> u64 {
        match ty {
            Type::Fixed(kind) => u64::from(kind.bits) / 8,
            Type::Bool => 1,
            _ => runtime::FIELD_WORD,
        }
    }

    /// Continues only if `index` is below `length`; otherwise stops the
    /// program, saying which index and what length.
    ///
    /// Unsigned, so a negative index is one enormous one and both ends are the
    /// same comparison. A trap rather than a wrapped index or a poisoned read,
    /// for the same reason integer overflow traps.
    fn check_index(&mut self, index: IntValue<'ctx>, length: IntValue<'ctx>) {
        let in_range = self
            .be
            .builder
            .build_int_compare(IntPredicate::ULT, index, length, "in.range")
            .expect("comparing an index against a length");
        let ok = self.block("index.ok");
        let out = self.block("index.out");
        self.be
            .builder
            .build_conditional_branch(in_range, ok, out)
            .expect("branching on the bounds check");

        self.at(out);
        let fail = self.be.rt.bounds_fail;
        self.be
            .builder
            .build_call(fail, &[index.into(), length.into()], "")
            .expect("reporting an index out of range");
        self.be.builder.build_unreachable().expect("sealing after a bounds failure");

        self.at(ok);
    }

    /// The length word of a string object.
    fn string_length(&mut self, object: PointerValue<'ctx>) -> IntValue<'ctx> {
        let slot = runtime::field_pointer(
            self.be.ctx,
            &self.be.builder,
            object,
            runtime::STRING_LEN_FIELD,
        );
        self.be
            .builder
            .build_load(self.be.ctx.i64_type(), slot, "str.len")
            .expect("reading a string length")
            .into_int_value()
    }

    fn array_slot(
        &mut self,
        array: inkwell::values::PointerValue<'ctx>,
        index: inkwell::values::IntValue<'ctx>,
        stride: u64,
    ) -> PointerValue<'ctx> {
        let i64_type = self.be.ctx.i64_type();
        let length_slot =
            runtime::field_pointer(self.be.ctx, &self.be.builder, array, runtime::ARRAY_LEN_FIELD);
        let length = self
            .be
            .builder
            .build_load(i64_type, length_slot, "array.len")
            .expect("reading an array's length")
            .into_int_value();
        self.check_index(index, length);
        // The header is counted in whole words and the elements in strides, so
        // the two are added as bytes rather than as indices.
        runtime::element_pointer(
            self.be.ctx,
            &self.be.builder,
            array,
            index,
            stride,
            runtime::ARRAY_HEADER_FIELDS * runtime::FIELD_WORD,
        )
    }

    /// `Array::new`, `Array::length`, `Array::get` and `Array::set`.
    ///
    /// Allocation and release are runtime calls because both need the length
    /// at run time; reading and writing an element are generated, so an array
    /// access is a bounds check and a load rather than a call.
    fn array_intrinsic(
        &mut self,
        site: ExprId,
        name: &str,
        args: &[ExprId],
        range: TextRange,
    ) -> Flow<'ctx> {
        match (name, args) {
            ("with_data", _) => self.with_data(site, args, range),
            ("is_utf8", [array]) => self.is_utf8(*array, range),
            ("new", [length, fill]) => {
                let array_ty = self.types.of(site).clone();
                let element = self.array_element(&array_ty, range)?;
                let len = self.expr(*length)?.into_int_value();
                let value = self.expr(*fill)?;

                let boxed = is_boxed(&element);
                let glue = if boxed { self.be.drop_glue(&element) } else { self.be.null_pointer() };
                let word = self.be.to_word(value);
                let flag = self.be.ctx.i8_type().const_int(u64::from(boxed), false);
                let stride =
                    self.be.ctx.i8_type().const_int(Self::stride(&element), false);
                let new = self.be.rt.array_new;
                let array = self
                    .be
                    .builder
                    .build_call(
                        new,
                        &[len.into(), word.into(), stride.into(), flag.into(), glue.into()],
                        "array",
                    )
                    .expect("allocating an array")
                    .try_as_basic_value()
                    .basic()
                    .expect("an array is a value");
                // Every slot took its own reference; this one was the caller's.
                self.drop(value, &element);
                Some(array)
            }
            ("length", [array]) => {
                let array_ty = self.types.of(*array).clone();
                let object = self.expr(*array)?.into_pointer_value();
                let slot = runtime::field_pointer(
                    self.be.ctx,
                    &self.be.builder,
                    object,
                    runtime::ARRAY_LEN_FIELD,
                );
                let length = self
                    .be
                    .builder
                    .build_load(self.be.ctx.i64_type(), slot, "array.len")
                    .expect("reading an array's length");
                self.drop(object.into(), &array_ty);
                Some(length)
            }
            ("get", [array, index]) => {
                let array_ty = self.types.of(*array).clone();
                let element = self.array_element(&array_ty, range)?;
                let object = self.expr(*array)?.into_pointer_value();
                let at = self.expr(*index)?.into_int_value();
                let slot = self.array_slot(object, at, Self::stride(&element));

                let Some(llvm_ty) = self.be.llvm_type(&element) else {
                    return self.fail("an array of that element type cannot be read", range);
                };
                let value = self
                    .be
                    .builder
                    .build_load(llvm_ty, slot, "element")
                    .expect("reading an element");
                // The array keeps its own reference to the element, so the
                // caller is handed one of its own.
                if is_boxed(&element) {
                    self.dup(value);
                }
                self.drop(object.into(), &array_ty);
                Some(value)
            }
            ("set", [array, index, value]) => {
                let array_ty = self.types.of(*array).clone();
                let element = self.array_element(&array_ty, range)?;
                let object = self.expr(*array)?.into_pointer_value();
                let at = self.expr(*index)?.into_int_value();
                let new = self.expr(*value)?;
                let slot = self.array_slot(object, at, Self::stride(&element));

                if is_boxed(&element) {
                    let llvm_ty = self.be.llvm_type(&element).expect("a boxed type is a pointer");
                    let old = self
                        .be
                        .builder
                        .build_load(llvm_ty, slot, "overwritten")
                        .expect("reading the overwritten element");
                    self.be.builder.build_store(slot, new).expect("writing an element");
                    // Store first, so `a.set(i, a.get(i))` cannot free what it
                    // has just written.
                    self.drop(old, &element);
                } else {
                    self.be.builder.build_store(slot, new).expect("writing an element");
                }
                self.drop(object.into(), &array_ty);
                Some(self.be.unit_value())
            }
            _ => self.fail(
                format!("`Array::{name}` is not an array operation the backend knows"),
                range,
            ),
        }
    }

    /// `Fibers::open`, `Fibers::adopt` and `Fibers::wait`.
    ///
    /// A nursery holds fiber handles, and adopting one grows the list — which
    /// is why the list lives in the runtime and this is an intrinsic rather
    /// than an extern. `adopt` takes the handle's reference; the nursery
    /// releases it once the fiber has been waited for.
    fn nursery_intrinsic(
        &mut self,
        name: &str,
        args: &[ExprId],
        range: TextRange,
    ) -> Flow<'ctx> {
        match (name, args) {
            ("open", []) => {
                let open = self.be.rt.fibers_open;
                let fibers = self
                    .be
                    .builder
                    .build_call(open, &[], "fibers")
                    .expect("opening a nursery")
                    .try_as_basic_value()
                    .basic()
                    .expect("a nursery is a value");
                Some(fibers)
            }
            ("adopt", [nursery, fiber]) => {
                let nursery_ty = self.types.of(*nursery).clone();
                let handle = self.expr(*nursery)?;
                let child = self.expr(*fiber)?;
                let adopt = self.be.rt.fibers_adopt;
                self.be
                    .builder
                    .build_call(adopt, &[handle.into(), child.into()], "")
                    .expect("adopting a fiber");
                // The nursery keeps the fiber's reference and only borrows its
                // own, so exactly one of the two is given back.
                self.drop(handle, &nursery_ty);
                Some(self.be.unit_value())
            }
            ("wait", [nursery]) => {
                let nursery_ty = self.types.of(*nursery).clone();
                let handle = self.expr(*nursery)?;
                let wait = self.be.rt.fibers_wait;
                self.be
                    .builder
                    .build_call(wait, &[handle.into()], "")
                    .expect("waiting for a nursery");
                self.drop(handle, &nursery_ty);
                Some(self.be.unit_value())
            }
            _ => self.fail(
                format!("`Fibers::{name}` is not a nursery operation the backend knows"),
                range,
            ),
        }
    }

    /// `record.field = value`.
    ///
    /// The same shape as assigning to a binding, one indirection further out:
    /// store, then release what was there. Store *first*, so that
    /// `p.next = p.next` — where reading already duplicated the reference —
    /// cannot free what it has just written.
    ///
    /// This is where the DAG invariant ends. Until now the heap graph could not
    /// contain a cycle, which made Perceus provably complete; a field that can
    /// be written to a value that (transitively) holds the record is a cycle,
    /// and a cycle leaks. `docs/design/memory.md` §2.
    fn assign_field(
        &mut self,
        base: ExprId,
        label: &str,
        value: ExprId,
        range: TextRange,
    ) -> Flow<'ctx> {
        let owner_ty = self.types.of(base).clone();
        let Type::Adt { name: type_name, .. } = owner_ty.clone() else {
            return self.fail("only a record's field can be assigned to", range);
        };
        let Some((_, info)) = self.be.variant_of(&type_name, &type_name).map(|(t, i)| (t, i.clone()))
        else {
            return self.fail(format!("`{type_name}` is not a record"), range);
        };
        let Some((index, field_ty)) = info.field(label).map(|(i, t)| (i, t.clone())) else {
            return self.fail(format!("`{type_name}` has no field `{label}`"), range);
        };

        let object = self.expr(base)?.into_pointer_value();
        let new = self.expr(value)?;

        let slot = runtime::field_pointer(self.be.ctx, &self.be.builder, object, index as u64);
        if is_boxed(&field_ty) {
            let llvm_ty = self.be.llvm_type(&field_ty).expect("a boxed type is a pointer");
            let old = self
                .be
                .builder
                .build_load(llvm_ty, slot, "overwritten")
                .expect("reading the overwritten field");
            self.be.builder.build_store(slot, new).expect("assigning a field");
            self.drop(old, &field_ty);
        } else {
            self.be.builder.build_store(slot, new).expect("assigning a field");
        }

        // The record itself was read to reach the field, and reading it
        // duplicated the reference. Give it back.
        self.drop(object.into(), &owner_ty);
        Some(self.be.unit_value())
    }

    /// Arithmetic that stops the program rather than wrapping.
    ///
    /// LLVM's `with.overflow` intrinsics return the result and a flag in one
    /// go, so the check costs a branch the optimizer can usually see through
    /// and never a second computation.
    ///
    /// Trapping in *every* build is the decision: a program that passes its
    /// tests and then wraps in production is the failure worth this branch, and
    /// two behaviours put the difference where it is most expensive to find.
    /// The width and signedness of an integer type, or `None` if it is not one.
    ///
    /// Every arithmetic instruction needs both: LLVM's types carry the width
    /// but not the sign, so `U8` and `I8` are the same `i8` and differ only in
    /// which `div`, `shr`, overflow intrinsic and ordering predicate is asked
    /// for. Getting that wrong is silent, which is why it is read from one
    /// place.
    fn int_shape(ty: &Type) -> Option<(u32, bool)> {
        match ty {
            Type::Int => Some((64, true)),
            Type::Fixed(kind) => Some((kind.bits.into(), kind.signed)),
            _ => None,
        }
    }

    /// Stops the program, saying what did not fit.
    ///
    /// The tail of an overflow check and of a narrowing conversion, which want
    /// the same three instructions and the same wording.
    fn trap(&mut self, what: &str) {
        let text = self
            .be
            .builder
            .build_global_string_ptr(what, "overflow.what")
            .expect("naming the operation")
            .as_pointer_value();
        let len = self.be.ctx.i64_type().const_int(what.len() as u64, false);
        let report = self.be.rt.overflow;
        self.be
            .builder
            .build_call(report, &[text.into(), len.into()], "")
            .expect("reporting an overflow");
        self.be.builder.build_unreachable().expect("sealing after an overflow");
    }

    fn checked_arithmetic(
        &mut self,
        intrinsic: &str,
        bits: u32,
        what: &str,
        left: IntValue<'ctx>,
        right: IntValue<'ctx>,
    ) -> BasicValueEnum<'ctx> {
        let checked = self.be.overflow_intrinsic(intrinsic, bits);
        let pair = self
            .be
            .builder
            .build_call(checked, &[left.into(), right.into()], "checked")
            .expect("checked arithmetic")
            .try_as_basic_value()
            .basic()
            .expect("the intrinsic returns a pair")
            .into_struct_value();
        let value = self
            .be
            .builder
            .build_extract_value(pair, 0, what)
            .expect("reading the result");
        let overflowed = self
            .be
            .builder
            .build_extract_value(pair, 1, "overflowed")
            .expect("reading the overflow flag")
            .into_int_value();

        let bad = self.block("overflow");
        let good = self.block("in.range");
        self.be
            .builder
            .build_conditional_branch(overflowed, bad, good)
            .expect("branching on the overflow flag");

        self.at(bad);
        self.trap(what);

        self.at(good);
        value
    }

    /// The `attempt` intrinsic: run a computation and make its failure a value.
    ///
    /// The tagged return is already "an error or a value"; this is the same
    /// thing with a name the type system can see. An intrinsic rather than a
    /// library function because catching *whatever* a body raises is not
    /// something `catch` can express — `catch` names constructors, and this
    /// names none.
    ///
    /// It is what makes retrying possible at all: a policy that runs a
    /// computation again cannot know what the computation failed with.
    fn attempt(&mut self, site: ExprId, body: ExprId, range: TextRange) -> Flow<'ctx> {
        let Some(shape) = FnShape::of(self.types.of(body)) else {
            return self.fail("`attempt` takes a function to run", range);
        };
        let result_ty = self.types.of(site).clone();
        let Type::Adt { name: result_name, .. } = result_ty.clone() else {
            return self.fail("`attempt` produces a `Result`", range);
        };
        let (Some((ok_tag, ok_info)), Some((err_tag, err_info))) = (
            self.be.variant_of(&result_name, "Ok").map(|(t, i)| (t, i.clone())),
            self.be.variant_of(&result_name, "Err").map(|(t, i)| (t, i.clone())),
        ) else {
            return self.fail("`attempt` produces a `Result`, which has `Ok` and `Err`", range);
        };

        let Invoked { raw, fallible } = self.invoke_closure(site, body, &shape, &[], range)?;
        let slot = self.result_slot(&result_ty);

        if !fallible {
            // Nothing to catch. Still a `Result`, because the *type* says so:
            // a caller reading one should not have to know whether the body it
            // passed happened to be infallible.
            let value = raw.unwrap_or_else(|| self.be.unit_value());
            let object = self.allocate(ok_info.fields.len(), ok_tag, &result_name);
            let field = ok_info.fields.first().cloned().unwrap_or(Type::Unit);
            self.store_field(object, 0, value, &field);
            self.leave_scope();
            return Some(object.into());
        }

        let tagged = raw.expect("a fallible closure returns a tagged value");
        let (which, word) = self.read_tagged(tagged);
        let raised = self.raised(which);
        let failed = self.block("attempt.err");
        let succeeded = self.block("attempt.ok");
        let merge = self.block("attempt.end");
        self.be
            .builder
            .build_conditional_branch(raised, failed, succeeded)
            .expect("branching on the tag");

        self.at(succeeded);
        let ok_field = ok_info.fields.first().cloned().unwrap_or(Type::Unit);
        let value = self.be.word_to_value(word, &ok_field);
        let object = self.allocate(ok_info.fields.len(), ok_tag, &result_name);
        self.store_field(object, 0, value, &ok_field);
        self.store_result(slot, object.into());
        self.br(merge);

        self.at(failed);
        let err_field = err_info.fields.first().cloned().unwrap_or(Type::Unit);
        let error = self.be.word_to_value(word, &err_field);
        let object = self.allocate(err_info.fields.len(), err_tag, &result_name);
        self.store_field(object, 0, error, &err_field);
        self.store_result(slot, object.into());
        self.br(merge);

        self.at(merge);
        self.leave_scope();
        Some(self.load_result(slot, &result_ty))
    }

    /// The `assert` intrinsic.
    ///
    /// A false assertion leaves the test the way a raise leaves a function:
    /// release what this frame owns, and return with a tag. The tag is
    /// reserved, so no `catch` can name a failed assertion and only the runner
    /// reads it.
    ///
    /// Only inside a test, and inside one it needs no `!`. That is the one
    /// place the mark rule bends, and it is bounded here rather than in the
    /// checker so that the bend is impossible to reach from ordinary code.
    fn assert(&mut self, condition: ExprId, range: TextRange) -> Flow<'ctx> {
        if !khora_hir::is_test(&self.owner) {
            return self.fail(
                "`assert` is only allowed inside a `test` block; elsewhere, `raise` says the \
                 same thing and says where it goes"
                    .to_string(),
                range,
            );
        }

        let held = self.expr(condition)?.into_int_value();
        let failed = self.block("assert.failed");
        let held_ok = self.block("assert.ok");
        self.be
            .builder
            .build_conditional_branch(held, held_ok, failed)
            .expect("branching on an assertion");

        self.at(failed);
        let which = self.be.ctx.i32_type().const_int(runtime::FAILED_WHICH, false);
        let none = self.be.ctx.i64_type().const_zero();
        self.leave_with(which, none);

        self.at(held_ok);
        Some(self.be.unit_value())
    }

    /// The `print` intrinsic.
    ///
    /// Dispatched on the argument's type, because there is no prelude yet in
    /// which three differently-typed printers could be declared — see
    /// `crate::backend`. It consumes its argument the way any Khora function
    /// consumes a parameter, so a `String` handed to it is dropped here; the
    /// `dup` at the read site is what that drop balances.
    fn print(&mut self, arg: ExprId, range: TextRange) -> Flow<'ctx> {
        let ty = self.types.of(arg).clone();
        let value = self.expr(arg)?;

        match ty {
            Type::Int => {
                let print = self.be.rt.print_int;
                self.be.builder.build_call(print, &[value.into()], "").expect("printing an Int");
            }
            Type::Float => {
                let print = self.be.rt.print_float;
                self.be.builder.build_call(print, &[value.into()], "").expect("printing a Float");
            }
            Type::Bool => {
                let byte = self
                    .be
                    .builder
                    .build_int_z_extend(value.into_int_value(), self.be.ctx.i8_type(), "bool.byte")
                    .expect("widening a Bool for the C ABI");
                let print = self.be.rt.print_bool;
                self.be.builder.build_call(print, &[byte.into()], "").expect("printing a Bool");
            }
            Type::Str => {
                let object = value.into_pointer_value();
                let length_slot = runtime::field_pointer(
                    self.be.ctx,
                    &self.be.builder,
                    object,
                    STRING_LEN_FIELD,
                );
                let length = self
                    .be
                    .builder
                    .build_load(self.be.ctx.i64_type(), length_slot, "str.len")
                    .expect("reading a string length");
                let bytes = runtime::byte_offset(
                    self.be.ctx,
                    &self.be.builder,
                    object,
                    STRING_BYTES_OFFSET,
                    "str.bytes",
                );
                let print = self.be.rt.print_str;
                self.be
                    .builder
                    .build_call(print, &[bytes.into(), length.into()], "")
                    .expect("printing a String");
                self.drop(value, &Type::Str);
            }
            other => {
                return self.fail(
                    format!(
                        "`print` shows `Int`, `Bool` and `String`; showing a `{other}` needs a \
                         typeclass, which arrives in phase 3"
                    ),
                    range,
                )
            }
        }
        Some(self.be.unit_value())
    }

    /// `raise e` — leave the function carrying the error.
    ///
    /// Everything the frame owns is released first, exactly as an early
    /// `return` releases it. A raise *is* a return, with a tag.
    fn lower_raise(&mut self, error: ExprId, range: TextRange) -> Flow<'ctx> {
        // An enclosing `catch` is the other place an error can go, so a
        // function with no `raises` clause may still contain a `raise` — as
        // long as something between here and the signature handles it. The
        // checker has already decided that; this only has to agree.
        if !self.raises && self.catches.is_empty() {
            return self.fail(
                "this function has no `raises` clause, so it cannot raise",
                range,
            );
        }
        // Which error type this is comes from the checker's record, not from
        // the expression's shape: `raise e` may raise a bound variable whose
        // type only inference knows.
        let which = match self.types.of(error) {
            Type::Adt { name, .. } => self.be.error_id(&name.clone()),
            other => {
                let other = other.clone();
                return self
                    .fail(format!("`{other}` is not an error type, so it cannot be raised"), range);
            }
        };
        let value = self.expr(error)?;
        let which = self.be.ctx.i32_type().const_int(u64::from(which), false);
        let word = self.be.to_word(value);
        self.leave_with(which, word);
        None
    }

    /// Returns a value from a fallible function without raising.
    fn return_ok(&mut self, payload: BasicValueEnum<'ctx>) {
        let none = self.be.ctx.i32_type().const_zero();
        self.return_tagged(none, payload);
    }

    /// Returns `{ which, payload }` from a fallible function.
    ///
    /// `which` is 0 to return normally and otherwise the error's type id. It
    /// is a value rather than a constant because propagating an error onward
    /// passes through whatever id arrived, which no frame in the middle knows.
    fn return_tagged(&mut self, which: IntValue<'ctx>, payload: BasicValueEnum<'ctx>) {
        let tagged = self.be.tagged_type();
        let word = self.be.to_word(payload);

        let value = self
            .be
            .builder
            .build_insert_value(tagged.get_undef(), which, 0, "tagged.which")
            .expect("setting the tag");
        let value = self
            .be
            .builder
            .build_insert_value(value, word, 1, "tagged")
            .expect("setting the payload");
        self.be
            .builder
            .build_return(Some(&value.into_struct_value()))
            .expect("returning a tagged value");
    }

    /// Takes a tagged return apart into its `which` and its payload word.
    fn read_tagged(
        &mut self,
        result: BasicValueEnum<'ctx>,
    ) -> (IntValue<'ctx>, IntValue<'ctx>) {
        let aggregate = result.into_struct_value();
        let which = self
            .be
            .builder
            .build_extract_value(aggregate, 0, "which")
            .expect("reading the tag")
            .into_int_value();
        let word = self
            .be
            .builder
            .build_extract_value(aggregate, 1, "payload")
            .expect("reading the payload")
            .into_int_value();
        (which, word)
    }

    /// Whether a `which` says the call raised — that is, whether it is not 0.
    fn raised(&mut self, which: IntValue<'ctx>) -> IntValue<'ctx> {
        let none = self.be.ctx.i32_type().const_zero();
        self.be
            .builder
            .build_int_compare(IntPredicate::NE, which, none, "raised")
            .expect("testing the tag")
    }

    /// Leaves at a cancellation point if a cancellation is pending.
    ///
    /// Emitted only where this function can return a tagged value, because
    /// that is the only channel a cancellation can travel on. A function with
    /// no error channel cannot report one and does not need to: the flag is
    /// the state of record, and the caller's next cancellation point sees it.
    /// `docs/design/effect-runtime.md` §6.
    fn check_cancellation(&mut self, range: TextRange) {
        if !self.raises || self.aborted {
            return;
        }
        let _ = range;

        let asked = self
            .be
            .builder
            .build_call(self.be.rt.cancelled, &[], "cancelled")
            .expect("reading the cancellation flag")
            .try_as_basic_value()
            .basic()
            .expect("a flag is a value")
            .into_int_value();
        let zero = self.be.ctx.i8_type().const_zero();
        let pending = self
            .be
            .builder
            .build_int_compare(IntPredicate::NE, asked, zero, "cancel.pending")
            .expect("testing the cancellation flag");

        let stop = self.block("cancel.stop");
        let carry_on = self.block("cancel.no");
        self.be
            .builder
            .build_conditional_branch(pending, stop, carry_on)
            .expect("branching on the cancellation flag");

        // The same way out an error takes: release what this frame owns — the
        // regions among it, so their finalizers run — and hand the tag on.
        self.at(stop);
        let which = self.be.ctx.i32_type().const_int(runtime::CANCELLED_WHICH, false);
        let none = self.be.ctx.i64_type().const_zero();
        self.leave_with(which, none);

        self.at(carry_on);
    }

    /// Sends an error on from the block it was found in.
    ///
    /// Out of the function, releasing the whole frame — or, inside a `catch`,
    /// into that `catch`'s handler, releasing only what the operand opened.
    /// The frame stays alive in the second case, which is the entire
    /// difference between handling an error and propagating one.
    fn leave_with(&mut self, which: IntValue<'ctx>, word: IntValue<'ctx>) {
        match self.catches.last() {
            Some(frame) => {
                let (handler, depth) = (frame.handler, frame.scope_depth);
                let (which_phi, word_phi) = (frame.which, frame.word);
                self.unwind_to(depth);
                let from = self.here();
                which_phi.add_incoming(&[(&which, from)]);
                word_phi.add_incoming(&[(&word, from)]);
                self.br(handler);
            }
            // Nowhere left: no enclosing `catch` and no `raises` clause.
            //
            // For an *error* the checker guarantees this is unreachable — a
            // total `catch` still emits its fall-through, and that is the only
            // way to get here. For a *cancellation* it is reachable, because a
            // cancellation is not in any row and so nothing the checker looked
            // at ruled it out. There is no frame between here and the entry
            // point that could carry it, so the entry point's outcome is
            // produced here instead. `docs/design/effect-runtime.md` §6.
            None if !self.raises => {
                let cancelled = self.be.ctx.i32_type().const_int(runtime::CANCELLED_WHICH, false);
                let is_cancel = self
                    .be
                    .builder
                    .build_int_compare(IntPredicate::EQ, which, cancelled, "cancelled")
                    .expect("testing for a cancellation");
                let stop = self.block("cancel.nowhere");
                let sealed = self.block("error.impossible");
                self.be
                    .builder
                    .build_conditional_branch(is_cancel, stop, sealed)
                    .expect("branching on the tag");

                self.at(stop);
                let cancel_stop = self.be.rt.cancel_stop;
                self.be.builder.build_call(cancel_stop, &[], "").expect("stopping");
                self.be.builder.build_unreachable().expect("sealing after a stop");

                self.at(sealed);
                self.be.builder.build_unreachable().expect("sealing an unhandled error");
            }
            None => {
                self.unwind_to(0);
                let error = self.be.word_to_value(word, &Type::Str);
                self.return_tagged(which, error);
            }
        }
    }

    /// Splits a fallible call's result: propagate the error, or take the value.
    ///
    /// This is the branch `!` marks. On the error path every binding this frame
    /// owns is released and the error is returned onward, which is the whole of
    /// unwinding — no tables, no personality routine.
    fn split_tagged(
        &mut self,
        result: BasicValueEnum<'ctx>,
        ret: &Type,
        range: TextRange,
    ) -> Flow<'ctx> {
        if !self.raises && self.catches.is_empty() {
            return self.fail(
                "this call can leave the function, but the function has no `raises` clause",
                range,
            );
        }

        let (which, word) = self.read_tagged(result);

        let propagate = self.block("raised");
        let continue_to = self.block("ok");
        let raised = self.raised(which);
        self.be
            .builder
            .build_conditional_branch(raised, propagate, continue_to)
            .expect("branching on the tag");

        self.at(propagate);
        self.leave_with(which, word);

        self.at(continue_to);
        Some(self.be.word_to_value(word, ret))
    }

    /// Builds a record: the same object a constructor builds, with the fields
    /// written in whatever order and stored in declaration order.
    fn build_record(
        &mut self,
        id: ExprId,
        fields: &[(String, ExprId)],
        range: TextRange,
    ) -> Flow<'ctx> {
        let Type::Adt { name, .. } = self.types.of(id).clone() else {
            return self.fail("this record has no type, which is a compiler bug", range);
        };
        let Some((tag, info)) = self.be.variant_of(&name, &name) else {
            return self.fail(format!("`{name}` is not a record"), range);
        };

        // Evaluated in written order, so side effects happen where they read,
        // and stored by label, so the order written does not matter.
        let mut values = Vec::with_capacity(fields.len());
        for (label, value) in fields {
            values.push((label.clone(), self.expr(*value)?));
        }

        let object = self.allocate(info.fields.len(), tag, &name);
        for (label, value) in values {
            let Some((index, field_ty)) = info.field(&label).map(|(i, t)| (i, t.clone())) else {
                continue;
            };
            // Moved in, as a constructor's arguments are: the record owns it
            // now and its drop glue is what releases it.
            self.store_field(object, index, value, &field_ty);
        }
        Some(object.into())
    }

    /// `p.x` — a load from the field's slot.
    fn read_field(&mut self, base: ExprId, label: &str, range: TextRange) -> Flow<'ctx> {
        let owner = self.types.of(base).clone();
        let Type::Adt { name, .. } = &owner else {
            return self.fail(format!("`{owner}` has no fields"), range);
        };
        let Some((_, info)) = self.be.variant_of(name, name) else {
            return self.fail(format!("`{name}` is not a record"), range);
        };
        let Some((index, field_ty)) = info.field(label).map(|(i, t)| (i, t.clone())) else {
            return self.fail(format!("`{name}` has no field `{label}`"), range);
        };

        let object = self.expr(base)?.into_pointer_value();
        let value = self.load_field(object, index, &field_ty);
        // The field is borrowed out of the record, and the record was owned by
        // this expression, so reading one keeps the field alive past the
        // release of what held it.
        if is_boxed(&field_ty) {
            self.dup(value);
        }
        self.drop(object.into(), &owner);
        Some(value)
    }

    /// Allocates the closure object for a lambda expression.
    ///
    /// Field 0 holds the lifted function's address and the captures follow, all
    /// under the ordinary object header — so a closure is dup'ed, dropped and
    /// counted by exactly the machinery every other heap value already uses.
    ///
    /// Which captures those are comes from the *site*, not from the lambda
    /// expression: lowering finds the names the body reads, and the checker
    /// adds the capabilities it uses without naming. The site is where the two
    /// were put together, and it is the only list this may read.
    fn make_closure(&mut self, id: ExprId, range: TextRange) -> Flow<'ctx> {
        let owner = self.owner.clone();
        let Some(site) = self.be.closure_at(&owner, id).cloned() else {
            return self.fail("this closure was never declared, which is a compiler bug", range);
        };
        let Some(tag) = self.be.closure_tag(&owner, id) else {
            return self.fail("this closure has no tag, which is a compiler bug", range);
        };
        let Some(function) = self.be.definition(&site.symbol) else {
            return self.fail("this closure has no lifted function, which is a compiler bug", range);
        };

        // Sized from the *site*, which is the list the fields are written
        // from. Sizing it from the lowering's list instead worked for as long
        // as the two agreed, and wrote past the end of the object the moment
        // one of them grew.
        let fields = CLOSURE_CAPTURE_BASE + site.captures.len();
        let alloc = self.be.rt.alloc;
        let object = self
            .be
            .builder
            .build_call(
                alloc,
                &[
                    self.be.ctx.i64_type().const_int(FIELD_WORD * fields as u64, false).into(),
                    self.be.ctx.i32_type().const_int(tag as u64, false).into(),
                ],
                "closure.obj",
            )
            .expect("allocating a closure")
            .try_as_basic_value()
            .basic()
            .expect("khora_alloc returns a pointer")
            .into_pointer_value();

        let code = function.as_global_value().as_pointer_value();
        let slot = runtime::field_pointer(self.be.ctx, &self.be.builder, object, 0);
        self.be.builder.build_store(slot, code).expect("storing a closure's code pointer");

        for (index, (local, ty)) in site.captures.iter().enumerate() {
            let Some(from) = self.slots.get(local).copied() else { continue };
            let Some(llvm_ty) = self.be.llvm_type(ty) else { continue };
            let value = self
                .be
                .builder
                .build_load(llvm_ty, from, "capture")
                .expect("reading a captured local");
            // The closure outlives this expression and now holds its own
            // reference. This is the one place a capture is counted; the
            // closure's drop glue is the matching release.
            if is_boxed(ty) {
                self.dup(value);
            }
            self.store_field(object, index + CLOSURE_CAPTURE_BASE, value, ty);
        }

        Some(object.into())
    }

    /// Wraps a named function in a closure object so it can be passed along.
    fn function_value(&mut self, symbol: &str, range: TextRange) -> Flow<'ctx> {
        let Some(thunk) = self.be.thunk(symbol) else {
            return self.fail(
                format!("`{symbol}` has a signature the backend cannot represent"),
                range,
            );
        };

        let alloc = self.be.rt.alloc;
        let object = self
            .be
            .builder
            .build_call(
                alloc,
                &[
                    self.be.ctx.i64_type().const_int(FIELD_WORD, false).into(),
                    // Any tag: an adapter captures nothing, so the shared
                    // closure `drop_fields` has no case for it and the default
                    // arm — which releases nothing — is the correct one.
                    self.be.ctx.i32_type().const_int(CLOSURE_ADAPTER_TAG, false).into(),
                ],
                "fnval.obj",
            )
            .expect("allocating a function value")
            .try_as_basic_value()
            .basic()
            .expect("khora_alloc returns a pointer")
            .into_pointer_value();

        let code = thunk.as_global_value().as_pointer_value();
        let slot = runtime::field_pointer(self.be.ctx, &self.be.builder, object, 0);
        self.be.builder.build_store(slot, code).expect("storing an adapter pointer");
        Some(object.into())
    }

    /// Calls a closure value: load its code pointer and call through it.
    fn invoke_closure(
        &mut self,
        site: ExprId,
        callee: ExprId,
        signature: &FnShape,
        args: &[ExprId],
        range: TextRange,
    ) -> Option<Invoked<'ctx>> {
        // The callee before the arguments, which is the order the source is
        // written in and the order the reference-counting plan was made for.
        let closure = self.expr(callee)?.into_pointer_value();
        let mut given = Vec::with_capacity(args.len());
        for arg in args {
            given.push(self.expr(*arg)?);
        }
        self.invoke_closure_at(site, callee, closure, signature, given, range)
    }

    /// The same, given the closure and its arguments as values.
    ///
    /// What an intrinsic that calls back into Khora needs: `Array::with_data`
    /// hands its body a pointer and a length that no expression in the source
    /// produced. Split out rather than written twice, because two places
    /// building the same argument list is how the two come to disagree — see
    /// errata 33.
    fn invoke_closure_at(
        &mut self,
        site: ExprId,
        callee: ExprId,
        closure: PointerValue<'ctx>,
        signature: &FnShape,
        given: Vec<BasicValueEnum<'ctx>>,
        range: TextRange,
    ) -> Option<Invoked<'ctx>> {
        let FnShape { params, ret, requires, raises } = signature;

        if given.len() != params.len() {
            self.fail(
                format!(
                    "this call takes {} argument(s), but {} were given",
                    params.len(),
                    given.len()
                ),
                range,
            )?;
        }
        let mut values: Vec<inkwell::values::BasicMetadataValueEnum<'ctx>> = vec![closure.into()];
        for value in given {
            values.push(value.into());
        }
        // Evidence is appended in label order, exactly as a direct call
        // appends it — the closure's shape follows its *type* the way a named
        // function's follows its signature.
        // A name for the diagnostic if the callee has one; a closure often
        // does not, and "this call" is honest about that.
        let label = match self.body.expr(callee) {
            Expr::Local(local) => self.body.local(*local).name.clone(),
            Expr::Field { name, .. } => name.clone(),
            _ => "this call".to_string(),
        };
        for value in self.evidence_from_row(requires, &label, site, range)? {
            values.push(value.into());
        }

        let ptr = self.be.ctx.ptr_type(AddressSpace::default());
        let mut param_types: Vec<inkwell::types::BasicMetadataTypeEnum<'ctx>> = vec![ptr.into()];
        for param in params {
            let Some(ty) = self.be.llvm_type(param) else {
                self.fail("a closure parameter has no machine type", range)?;
                unreachable!("`fail` returns None")
            };
            param_types.push(ty.into());
        }
        for (_, ty) in row_fields(requires) {
            let Some(ty) = self.be.llvm_type(&ty) else {
                self.fail("a capability has no machine type", range)?;
                unreachable!("`fail` returns None")
            };
            param_types.push(ty.into());
        }
        let fallible = !row_is_empty(raises);
        let fn_type = if fallible {
            self.be.tagged_type().fn_type(&param_types, false)
        } else {
            match ret {
                Type::Unit => self.be.ctx.void_type().fn_type(&param_types, false),
                other => match self.be.llvm_type(other) {
                    Some(ty) => ty.fn_type(&param_types, false),
                    None => {
                        self.fail("a closure's result has no machine type", range)?;
                        unreachable!("`fail` returns None")
                    }
                },
            }
        };

        let slot = runtime::field_pointer(self.be.ctx, &self.be.builder, closure, 0);
        let code = self
            .be
            .builder
            .build_load(ptr, slot, "closure.code")
            .expect("loading a closure's code pointer")
            .into_pointer_value();

        // The call site owns a reference to the closure — reading a local
        // dup'ed it, and a lambda written in place was born owned — and the
        // callee only borrows it. So it has to be released here, on *every*
        // way out of this expression, which is what makes it a scope rather
        // than a line after the call: a fallible callee can leave through the
        // branch below, and that path never reaches the line.
        //
        // A closure calling *itself* is the exception. Its own name is the
        // argument it was called through, which it borrows; releasing that
        // would decrement a count this frame never took, and free the closure
        // out from under the caller still running in it.
        let owned = !matches!(self.body.expr(callee), Expr::LambdaSelf);
        let callee_ty = self.types.of(callee).clone();
        self.scopes.push(if owned && is_boxed(&callee_ty) {
            vec![Cleanup::Temp(closure.into(), callee_ty)]
        } else {
            Vec::new()
        });

        let call = self
            .be
            .builder
            .build_indirect_call(fn_type, code, &values, "closure.call")
            .expect("calling a closure");

        Some(Invoked { raw: call.try_as_basic_value().basic(), fallible })
    }

    /// Calls a closure and propagates whatever it raised.
    ///
    /// The ordinary reading of `f(x)`: an error leaves through the branch `!`
    /// marks. [`Lower::attempt`] is the other reading, and the two differ only
    /// in what they do with the tag.
    fn call_closure(
        &mut self,
        site: ExprId,
        callee: ExprId,
        signature: &FnShape,
        args: &[ExprId],
        range: TextRange,
    ) -> Flow<'ctx> {
        let invoked = self.invoke_closure(site, callee, signature, args, range)?;
        self.after_invoke(invoked, &signature.ret, range)
    }

    /// What to do with a closure call that has happened: split its tag if it
    /// had one, then close the scope holding the closure's reference.
    ///
    /// Split out because `SharedFn::call` invokes a closure the source never
    /// wrote as a call, and two places deciding what a tagged return means is
    /// how the two come to disagree.
    fn after_invoke(
        &mut self,
        invoked: Invoked<'ctx>,
        ret: &Type,
        range: TextRange,
    ) -> Flow<'ctx> {
        let Invoked { raw, fallible } = invoked;
        let result = if fallible {
            let tagged = raw.expect("a fallible closure returns a tagged value");
            self.split_tagged(tagged, ret, range)?
        } else {
            match ret {
                Type::Unit => self.be.unit_value(),
                _ => raw.unwrap_or_else(|| self.be.unit_value()),
            }
        };

        self.leave_scope();
        Some(result)
    }

    /// The capabilities a call needs, read out of the caller's own bindings.
    ///
    /// A label is in scope because the caller declared it in its own `with`
    /// clause or bound it in a `with` block — both are locals by the time
    /// lowering runs, which is why installation needs no runtime of its own.
    fn evidence_for(
        &mut self,
        name: &str,
        site: ExprId,
        range: TextRange,
    ) -> Option<Vec<BasicValueEnum<'ctx>>> {
        let signature = self.be.signature_of(name)?;
        self.evidence_from_row(&signature.requires, name, site, range)
    }

    /// The same, given a row rather than a signature.
    ///
    /// This is what a call *through a value* uses: the requirement is part of
    /// the callee's type, so the evidence a closure is handed is worked out
    /// exactly as it is for a direct call — same labels, same order, same
    /// ownership.
    fn evidence_from_row(
        &mut self,
        requires: &Type,
        name: &str,
        site: ExprId,
        range: TextRange,
    ) -> Option<Vec<BasicValueEnum<'ctx>>> {
        let Type::Row { fields: wanted, .. } = requires else { return Some(Vec::new()) };
        if wanted.is_empty() {
            return Some(Vec::new());
        }

        let wanted = wanted.clone();
        let mut out = Vec::with_capacity(wanted.len());
        for (label, ty) in wanted {
            let Some(local) = self.body.capability_at(site, &label) else {
                // Not a binding this body can name, but possibly one it was
                // handed: a `with 'r` clause forwards capabilities it has no
                // name for. Passed on as it arrived, with a `dup` to match the
                // ownership every other argument has.
                if let Some(value) = self.incoming.get(&label).copied() {
                    if is_boxed(&ty) {
                        self.dup(value);
                    }
                    out.push(value);
                    continue;
                }
                self.fail(
                    format!("`{name}` needs the capability `{label}`, which is not in scope"),
                    range,
                );
                return None;
            };
            let (Some(slot), Some(llvm_ty)) =
                (self.slots.get(&local).copied(), self.be.llvm_type(&ty))
            else {
                self.fail(format!("`{label}` has no storage, which is a compiler bug"), range);
                return None;
            };
            let value = self
                .be
                .builder
                .build_load(llvm_ty, slot, &format!("{label}.evidence"))
                .expect("reading a capability");
            // Passed owned, as every other argument is: the callee's plan
            // releases it where its body ends, so the caller hands over a
            // reference of its own rather than lending the one it holds.
            if is_boxed(&ty) {
                self.dup(value);
            }
            out.push(value);
        }
        Some(out)
    }

    fn call_named(
        &mut self,
        name: &str,
        site: ExprId,
        args: &[ExprId],
        range: TextRange,
    ) -> Flow<'ctx> {
        let Some(signature) = self.be.signature_of(name) else {
            return self.fail(format!("`{name}` has no signature to call through"), range);
        };
        let function = match self.be.callee(name) {
            Ok(function) => function,
            Err(message) => return self.fail(message, range),
        };

        let mut values = Vec::with_capacity(args.len());
        for arg in args {
            values.push(self.expr(*arg)?.into());
        }
        // Then the capabilities, which the source never writes: the row said
        // which and in what order.
        //
        // Except across the C ABI, where a `with` clause is a *permission*
        // rather than an argument — a foreign function has no use for a Khora
        // record of closures, and requiring one it never receives is how the
        // boundary is governed. Decision 3 in `docs/design/ffi.md`; the
        // checker has already charged the row to this frame either way.
        if self.be.is_defined(name) {
            for capability in self.evidence_for(name, site, range)? {
                values.push(capability.into());
            }
        }

        let call = self.be.builder.build_call(function, &values, "call").expect("a call");
        let result = call.try_as_basic_value().basic();

        // A fallible callee handed back `{ raised, payload }`. Splitting it is
        // the branch `!` marks, and the error path is where this frame's
        // bindings are released on the way out.
        if can_raise(&signature) {
            let result = result.expect("a fallible call returns a tagged value");
            return self.split_tagged(result, &signature.ret, range);
        }

        Some(match signature.ret {
            Type::Unit => self.be.unit_value(),
            _ => result.unwrap_or_else(|| self.be.unit_value()),
        })
    }

    /// Builds an ADT: `khora_alloc(8 * fields, tag)` and one store per field.
    ///
    /// The arguments are evaluated before the allocation, not after. An
    /// argument can diverge — `Cons(x, return 0)` — and an object allocated
    /// before that happens is unreachable and unfreed.
    fn construct(
        &mut self,
        owner: &str,
        case: &str,
        args: &[ExprId],
        range: TextRange,
    ) -> Flow<'ctx> {
        let Some((tag, info)) = self.be.variant_of(owner, case) else {
            return self.fail(format!("`{owner}::{case}` is not a constructor"), range);
        };
        if args.len() != info.fields.len() {
            return self.fail(
                format!("`{owner}::{case}` takes {} field(s)", info.fields.len()),
                range,
            );
        }

        let mut values = Vec::with_capacity(args.len());
        for arg in args {
            values.push(self.expr(*arg)?);
        }

        let object = self.allocate(info.fields.len(), tag, case);

        for (index, (value, field_ty)) in values.into_iter().zip(&info.fields).enumerate() {
            // A boxed argument is *moved* into the object: no dup here, and no
            // drop either. The object owns it now, and its `drop_fields` is
            // what eventually releases it.
            self.store_field(object, index, value, field_ty);
        }
        Some(object.into())
    }

    // -----------------------------------------------------------------------
    // Fields
    // -----------------------------------------------------------------------

    /// A fresh heap object with room for `fields` words, under `tag`.
    fn allocate(&mut self, fields: usize, tag: u32, name: &str) -> PointerValue<'ctx> {
        let alloc = self.be.rt.alloc;
        self.be
            .builder
            .build_call(
                alloc,
                &[
                    self.be.ctx.i64_type().const_int(FIELD_WORD * fields as u64, false).into(),
                    self.be.ctx.i32_type().const_int(tag as u64, false).into(),
                ],
                &format!("{name}.obj"),
            )
            .expect("allocating an object")
            .try_as_basic_value()
            .basic()
            .expect("khora_alloc returns a pointer")
            .into_pointer_value()
    }

    /// Writes a field, widening a `Bool` to a full word.
    ///
    /// Every field is a machine word, which is what makes
    /// `KHORA_FIELD_OFFSET + 8 * i` a valid address for field `i` regardless of
    /// what the fields before it hold.
    fn store_field(
        &mut self,
        object: PointerValue<'ctx>,
        index: usize,
        value: BasicValueEnum<'ctx>,
        ty: &Type,
    ) {
        let slot = runtime::field_pointer(self.be.ctx, &self.be.builder, object, index as u64);
        let stored = match ty {
            Type::Bool => self
                .be
                .builder
                .build_int_z_extend(value.into_int_value(), self.be.ctx.i64_type(), "field.word")
                .expect("widening a Bool field")
                .into(),
            _ => value,
        };
        self.be.builder.build_store(slot, stored).expect("storing a field");
    }

    fn load_field(
        &mut self,
        object: PointerValue<'ctx>,
        index: usize,
        ty: &Type,
    ) -> BasicValueEnum<'ctx> {
        let slot = runtime::field_pointer(self.be.ctx, &self.be.builder, object, index as u64);
        match ty {
            // A field slot is a whole word and these are narrower, so the
            // word is read and cut down. Reading them at their own width would
            // work on a little-endian machine and quietly not on the other
            // kind; `store_field` widens for the same reason.
            Type::Bool | Type::Fixed(_) => {
                let word = self
                    .be
                    .builder
                    .build_load(self.be.ctx.i64_type(), slot, "field.word")
                    .expect("reading a narrow field")
                    .into_int_value();
                let narrow = match ty {
                    Type::Fixed(kind) => self.be.int_width(kind.bits.into()),
                    _ => self.be.ctx.bool_type(),
                };
                self.be
                    .builder
                    .build_int_truncate_or_bit_cast(word, narrow, "field")
                    .expect("narrowing a field")
                    .into()
            }
            // Everything else is read back at whatever `llvm_type` says it
            // is. Listing the pointer-shaped types here instead meant a
            // closure in a field — added later — came back as an `i64`.
            other => {
                let ty = self
                    .be
                    .llvm_type(other)
                    .unwrap_or_else(|| self.be.ctx.i64_type().into());
                self.be.builder.build_load(ty, slot, "field").expect("reading a field")
            }
        }
    }

    // -----------------------------------------------------------------------
    // Operators
    // -----------------------------------------------------------------------

    fn binary(
        &mut self,
        site: ExprId,
        op: BinOp,
        lhs: ExprId,
        rhs: ExprId,
        range: TextRange,
    ) -> Flow<'ctx> {
        if matches!(op, BinOp::And | BinOp::Or) {
            return self.logical(op, lhs, rhs);
        }

        let operand_ty = self.types.of(lhs).clone();
        let left = self.expr(lhs)?;
        let right = self.expr(rhs)?;

        match op {
            BinOp::Add if matches!(operand_ty, Type::Str) => self.concat(left, right),
            // IEEE arithmetic does not overflow — it reaches infinity — so
            // there is nothing to trap on and nothing to check.
            BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div
                if matches!(operand_ty, Type::Float) =>
            {
                let (l, r) = (left.into_float_value(), right.into_float_value());
                let b = &self.be.builder;
                let value = match op {
                    BinOp::Add => b.build_float_add(l, r, "fadd"),
                    BinOp::Sub => b.build_float_sub(l, r, "fsub"),
                    BinOp::Mul => b.build_float_mul(l, r, "fmul"),
                    _ => b.build_float_div(l, r, "fdiv"),
                };
                Some(value.expect("floating-point arithmetic").into())
            }
            BinOp::Add | BinOp::Sub | BinOp::Mul => {
                let (l, r) = (left.into_int_value(), right.into_int_value());
                let (bits, signed) =
                    Self::int_shape(&operand_ty).expect("arithmetic on an integer");
                // A `U8` addition traps at 255, not at 2^63: the check is only
                // worth anything if it is the *type's* range being checked.
                let sign = if signed { 's' } else { 'u' };
                let (verb, what) = match op {
                    BinOp::Add => ("add", "addition"),
                    BinOp::Sub => ("sub", "subtraction"),
                    _ => ("mul", "multiplication"),
                };
                let intrinsic = format!("llvm.{sign}{verb}.with.overflow.i{bits}");
                let what = format!("{operand_ty} {what}");
                Some(self.checked_arithmetic(&intrinsic, bits, &what, l, r))
            }
            BinOp::Div | BinOp::Rem => {
                let (l, r) = (left.into_int_value(), right.into_int_value());
                let (bits, signed) =
                    Self::int_shape(&operand_ty).expect("arithmetic on an integer");
                let width = self.be.int_width(bits);

                // Both ways an integer division can go wrong are *undefined* in
                // LLVM, and what they do on hardware is a fault with no message
                // attached — a bare 0xC0000094 or a SIGFPE, which says nothing
                // about which line or which value. Checked for the same reason
                // overflow is: the program is wrong either way, and the useful
                // thing to do is say so.
                let nonzero = self
                    .be
                    .builder
                    .build_int_compare(IntPredicate::NE, r, width.const_zero(), "nonzero")
                    .expect("comparing a divisor against zero");
                self.guard(nonzero, &format!("{operand_ty} division by zero"));

                // The other one only exists for a signed type, and only for one
                // pair of values: the minimum over minus one, whose quotient is
                // one past the maximum. Unsigned division cannot overflow.
                if signed {
                    let min = width.const_int(1u64 << (bits - 1), false);
                    let minus_one = width.const_all_ones();
                    let b = &self.be.builder;
                    let is_min = b
                        .build_int_compare(IntPredicate::EQ, l, min, "is.min")
                        .expect("comparing against the minimum");
                    let is_neg_one = b
                        .build_int_compare(IntPredicate::EQ, r, minus_one, "is.neg.one")
                        .expect("comparing against minus one");
                    let both = b.build_and(is_min, is_neg_one, "overflows").expect("both");
                    let ok = b.build_not(both, "in.range").expect("negating");
                    self.guard(ok, &format!("{operand_ty} division"));
                }

                let b = &self.be.builder;
                let value = match (op, signed) {
                    (BinOp::Div, true) => b.build_int_signed_div(l, r, "div"),
                    (BinOp::Div, false) => b.build_int_unsigned_div(l, r, "div"),
                    (_, true) => b.build_int_signed_rem(l, r, "rem"),
                    (_, false) => b.build_int_unsigned_rem(l, r, "rem"),
                };
                Some(value.expect("integer arithmetic").into())
            }
            _ => self.compare(site, op, left, right, &operand_ty, range),
        }
    }

    /// `a == b` on two strings, by content.
    ///
    /// Both operands are owned here — they were evaluated for this comparison —
    /// so both are released once the answer is in hand.
    fn compare_strings(
        &mut self,
        op: BinOp,
        left: BasicValueEnum<'ctx>,
        right: BasicValueEnum<'ctx>,
    ) -> Flow<'ctx> {
        let mut parts = Vec::with_capacity(4);
        for value in [left, right] {
            let object = value.into_pointer_value();
            let length_slot =
                runtime::field_pointer(self.be.ctx, &self.be.builder, object, STRING_LEN_FIELD);
            let length = self
                .be
                .builder
                .build_load(self.be.ctx.i64_type(), length_slot, "str.len")
                .expect("reading a string length");
            let bytes = runtime::byte_offset(
                self.be.ctx,
                &self.be.builder,
                object,
                STRING_BYTES_OFFSET,
                "str.bytes",
            );
            parts.push(bytes.into());
            parts.push(length.into());
        }

        let equal = self
            .be
            .builder
            .build_call(self.be.rt.str_eq, &parts, "str.eq")
            .expect("comparing two strings")
            .try_as_basic_value()
            .basic()
            .expect("khora_str_eq returns a _Bool")
            .into_int_value();

        self.drop(left, &Type::Str);
        self.drop(right, &Type::Str);

        // The runtime answers in a C `_Bool`, one byte; Khora's `Bool` is an
        // `i1`, so the answer is narrowed by asking whether the byte is set.
        let zero = self.be.ctx.i8_type().const_zero();
        let predicate = match op {
            BinOp::Eq => IntPredicate::NE,
            _ => IntPredicate::EQ,
        };
        let value = self
            .be
            .builder
            .build_int_compare(predicate, equal, zero, "str.cmp")
            .expect("narrowing a _Bool");
        Some(value.into())
    }

    fn compare(
        &mut self,
        site: ExprId,
        op: BinOp,
        left: BasicValueEnum<'ctx>,
        right: BasicValueEnum<'ctx>,
        operand_ty: &Type,
        range: TextRange,
    ) -> Flow<'ctx> {
        // `Bool` is an `i1`, where "less than" means `false < true`, so its
        // ordering is unsigned — signing an `i1` comparison inverts it — and an
        // unsigned integer is unsigned for the obvious reason. `255 < 0` being
        // true is exactly the bug this prevents.
        let signed = match Self::int_shape(operand_ty) {
            Some((_, signed)) => signed,
            None => !matches!(operand_ty, Type::Bool),
        };
        let predicate = match op {
            BinOp::Eq => IntPredicate::EQ,
            BinOp::Ne => IntPredicate::NE,
            BinOp::Lt if signed => IntPredicate::SLT,
            BinOp::Lt => IntPredicate::ULT,
            BinOp::Gt if signed => IntPredicate::SGT,
            BinOp::Gt => IntPredicate::UGT,
            BinOp::Le if signed => IntPredicate::SLE,
            BinOp::Le => IntPredicate::ULE,
            BinOp::Ge if signed => IntPredicate::SGE,
            _ => IntPredicate::UGE,
        };

        // IEEE comparison, which is what every reader expects `==` on floats
        // to mean and exactly why `Float` implements neither `Eq` nor `Ord`:
        // `NaN == NaN` is false, and a law-abiding equivalence cannot say so.
        // The *operator* is primitive; the *trait* is for lawful equality.
        // `docs/design/numbers.md`.
        if matches!(operand_ty, Type::Float) {
            use inkwell::FloatPredicate;
            // Ordered, so every comparison involving a NaN is false — `<`, and
            // `==` too. `!=` is the one that is unordered, so that `x != x` is
            // true for a NaN, which is the other half of the same convention.
            let predicate = match op {
                BinOp::Eq => FloatPredicate::OEQ,
                BinOp::Ne => FloatPredicate::UNE,
                BinOp::Lt => FloatPredicate::OLT,
                BinOp::Gt => FloatPredicate::OGT,
                BinOp::Le => FloatPredicate::OLE,
                _ => FloatPredicate::OGE,
            };
            let value = self
                .be
                .builder
                .build_float_compare(
                    predicate,
                    left.into_float_value(),
                    right.into_float_value(),
                    "fcmp",
                )
                .expect("comparing two floats");
            return Some(value.into());
        }

        // Strings compare by their bytes, not by their address: two `"a"`
        // literals are separate allocations and a pointer comparison would call
        // them different.
        if matches!(operand_ty, Type::Str) && matches!(op, BinOp::Eq | BinOp::Ne) {
            return self.compare_strings(op, left, right);
        }

        // Anything with a shape decides for itself what comparison means, in an
        // `Eq` or `Ord` impl the checker already resolved and monomorphization
        // already emitted. The operator is one thing whichever type it is used
        // on: a machine instruction where that is the answer, and a call where
        // the answer is a question only the type can settle.
        if !matches!(operand_ty, Type::Int | Type::Fixed(_) | Type::Bool | Type::Unit) {
            if let Some(symbol) = self.mono.callee(&self.owner.clone(), site) {
                let function = match self.be.callee(&symbol) {
                    Ok(function) => function,
                    Err(message) => return self.fail(message, range),
                };
                let answer = self
                    .be
                    .builder
                    .build_call(function, &[left.into(), right.into()], "compare")
                    .expect("calling a comparison impl")
                    .try_as_basic_value()
                    .basic()
                    .expect("a comparison returns a value");

                return match op {
                    // `!=` is `==` negated. Asking a type for both would be
                    // asking it to be consistent about something it cannot get
                    // wrong here.
                    BinOp::Eq => Some(answer),
                    BinOp::Ne => Some(
                        self.be
                            .builder
                            .build_not(answer.into_int_value(), "ne")
                            .expect("negating an equality")
                            .into(),
                    ),
                    _ => self.read_ordering(op, answer, range),
                };
            }
        }

        if !matches!(operand_ty, Type::Int | Type::Fixed(_) | Type::Bool | Type::Unit) {
            self.drop(left, operand_ty);
            self.drop(right, operand_ty);
            return self.fail(
                format!(
                    "two `{operand_ty}` values cannot be ordered with `<`, `>`, `<=` or `>=`; \
                     that needs an `Ord` impl the operator does not reach yet. `==` and `!=` \
                     do reach an `Eq` impl"
                ),
                range,
            );
        }

        let value = self
            .be
            .builder
            .build_int_compare(predicate, left.into_int_value(), right.into_int_value(), "cmp")
            .expect("a comparison");
        Some(value.into())
    }

    /// `&&` and `||`, short-circuiting.
    ///
    /// Written with a `phi` rather than a slot because both incoming values are
    /// already in registers and the join has no other work to do.
    fn logical(&mut self, op: BinOp, lhs: ExprId, rhs: ExprId) -> Flow<'ctx> {
        let left = self.expr(lhs)?;
        let entry = self.here();
        let rhs_block = self.block("logic.rhs");
        let merge = self.block("logic.end");

        let condition = left.into_int_value();
        match op {
            BinOp::And => self.be.builder.build_conditional_branch(condition, rhs_block, merge),
            _ => self.be.builder.build_conditional_branch(condition, merge, rhs_block),
        }
        .expect("a short-circuit branch");

        self.at(rhs_block);
        let right = self.expr(rhs);
        let rhs_end = self.here();
        if right.is_some() {
            self.br(merge);
        }

        self.at(merge);
        let bool_type = self.be.ctx.bool_type();
        let phi = self.be.builder.build_phi(bool_type, "logic").expect("a phi");
        // The short-circuit edge carries the answer the operator already knows:
        // `false` for a failed `&&`, `true` for a satisfied `||`.
        let shortcut = bool_type.const_int(u64::from(matches!(op, BinOp::Or)), false);
        phi.add_incoming(&[(&shortcut, entry)]);
        if let Some(right) = right {
            phi.add_incoming(&[(&right, rhs_end)]);
        }
        Some(phi.as_basic_value())
    }

    fn unary(&mut self, op: UnOp, operand: ExprId, _range: TextRange) -> Flow<'ctx> {
        let ty = self.types.of(operand).clone();
        let value = self.expr(operand)?;
        if matches!(op, UnOp::Neg) && matches!(ty, Type::Float) {
            let negated = self
                .be
                .builder
                .build_float_neg(value.into_float_value(), "fneg")
                .expect("negating a float");
            return Some(negated.into());
        }
        let value = value.into_int_value();
        let result = match op {
            // Not checked, unlike `-` the binary operator: the one value that
            // cannot be negated is the type's minimum, and the only way to
            // write it is as a negated literal, which the checker folds into
            // the constant before it ever reaches here.
            UnOp::Neg => self.be.builder.build_int_neg(value, "neg"),
            // On an `i1`, `not` is `xor 1`, which is exactly logical negation.
            UnOp::Not => self.be.builder.build_not(value, "not"),
        };
        Some(result.expect("a unary operator").into())
    }

    // -----------------------------------------------------------------------
    // Statements and control flow
    // -----------------------------------------------------------------------

    /// Assignment to a `let mut` binding.
    ///
    /// The target is deliberately **not** lowered as an expression. The plan
    /// records a `dup` for it — its walk sees a local read on the left of an
    /// `=` and cannot tell it apart from a use — and honouring that would take
    /// a reference nobody ever releases. What the assignment owes instead is
    /// the *old* value's release, which the plan has no place to record.
    fn assign(&mut self, target: ExprId, value: ExprId, range: TextRange) -> Flow<'ctx> {
        if let Expr::Field { base, name } = self.body.expr(target).clone() {
            return self.assign_field(base, &name, value, range);
        }
        let Expr::Local(local) = self.body.expr(target).clone() else {
            return self.fail("this expression cannot be assigned to", range);
        };
        let ty = self.types.local(local).clone();
        let Some(slot) = self.slots.get(&local).copied() else {
            return self.fail("this binding has no storage, which is a compiler bug", range);
        };

        let new = self.expr(value)?;

        if is_boxed(&ty) {
            let llvm_ty = self.be.llvm_type(&ty).expect("a boxed type is a pointer");
            let old = self
                .be
                .builder
                .build_load(llvm_ty, slot, "overwritten")
                .expect("reading the overwritten value");
            self.be.builder.build_store(slot, new).expect("assigning");
            // After the store, so that `s = s` — where the read already
            // duplicated the reference — cannot free what it just stored.
            self.drop(old, &ty);
        } else {
            self.be.builder.build_store(slot, new).expect("assigning");
        }
        Some(self.be.unit_value())
    }

    fn lower_block(&mut self, id: ExprId, stmts: &[Stmt], tail: Option<ExprId>) -> Flow<'ctx> {
        let cleanups: Vec<Cleanup<'ctx>> =
            self.plan.drops_for(id).iter().map(|l| Cleanup::Local(*l)).collect();
        self.scopes.push(cleanups);

        for stmt in stmts {
            let reached = match stmt {
                Stmt::Let { pat, init, .. } => self.lower_let(*pat, *init),
                Stmt::Expr(e) => {
                    let ty = self.types.of(*e).clone();
                    match self.expr(*e) {
                        // A statement's value is discarded, and a discarded
                        // boxed value is a leak the plan does not cover: it
                        // records releases for *bindings*, and this was never
                        // bound to anything.
                        Some(value) => {
                            self.drop(value, &ty);
                            true
                        }
                        None => false,
                    }
                }
            };
            if !reached {
                // Control left through a `return` or a `break`, which released
                // this scope on the way past. Nothing more to emit here.
                self.scopes.pop();
                return None;
            }
        }

        let value = match tail {
            Some(tail) => self.expr(tail),
            None => Some(self.be.unit_value()),
        };
        match value {
            Some(value) => {
                // Releases come after the tail is evaluated: a tail that reads
                // one of these locals has already duplicated it.
                self.leave_scope();
                Some(value)
            }
            None => {
                self.scopes.pop();
                None
            }
        }
    }

    /// Returns whether control continues past the statement.
    fn lower_let(&mut self, pat: PatId, init: Option<ExprId>) -> bool {
        let Some(init) = init else {
            // `let x;` leaves the zeroed slot alone. Reading it before an
            // assignment is a front-end question, not a backend one.
            return true;
        };
        let ty = self.types.of(init).clone();
        let range = self.body.range(init);
        let Some(value) = self.expr(init) else { return false };

        match self.body.pat(pat).clone() {
            Pat::Bind(local) => match self.slots.get(&local).copied() {
                Some(slot) => {
                    self.be.builder.build_store(slot, value).expect("binding a let");
                    true
                }
                None => {
                    self.fail("this binding has no storage, which is a compiler bug", range);
                    false
                }
            },
            // `let _ = f()` still owns what `f` returned.
            Pat::Wildcard | Pat::Missing => {
                self.drop(value, &ty);
                true
            }
            _ => {
                self.fail(
                    "destructuring in a `let` is not supported yet; use `match`, which can \
                     handle the case where the pattern does not apply",
                    range,
                );
                false
            }
        }
    }

    fn lower_if(
        &mut self,
        id: ExprId,
        condition: ExprId,
        then_branch: ExprId,
        else_branch: Option<ExprId>,
    ) -> Flow<'ctx> {
        let condition = self.expr(condition)?.into_int_value();

        let then_block = self.block("if.then");
        let else_block = self.block("if.else");
        let merge = self.block("if.end");
        self.be
            .builder
            .build_conditional_branch(condition, then_block, else_block)
            .expect("an if branch");

        let result_ty = self.types.of(id).clone();
        let slot = self.result_slot(&result_ty);
        let mut reached = 0;

        self.at(then_block);
        if let Some(value) = self.expr(then_branch) {
            self.store_result(slot, value);
            self.br(merge);
            reached += 1;
        }

        self.at(else_block);
        let value = match else_branch {
            Some(else_branch) => self.expr(else_branch),
            // An `if` without `else` is `()` on the missing side, which the
            // checker has already required of the other side too.
            None => Some(self.be.unit_value()),
        };
        if let Some(value) = value {
            self.store_result(slot, value);
            self.br(merge);
            reached += 1;
        }

        self.at(merge);
        if reached == 0 {
            self.be.builder.build_unreachable().expect("sealing an unreachable join");
            return None;
        }
        Some(self.load_result(slot, &result_ty))
    }

    fn lower_while(&mut self, condition: ExprId, body: ExprId) -> Flow<'ctx> {
        let head = self.block("while.head");
        let body_block = self.block("while.body");
        let exit = self.block("while.end");
        self.br(head);

        self.at(head);
        let Some(test) = self.expr(condition) else {
            // A condition that never returns makes the loop and everything
            // after it dead. Seal the blocks so the IR stays well formed.
            self.at(body_block);
            self.be.builder.build_unreachable().expect("sealing a dead loop body");
            self.at(exit);
            self.be.builder.build_unreachable().expect("sealing a dead loop exit");
            return None;
        };
        self.be
            .builder
            .build_conditional_branch(test.into_int_value(), body_block, exit)
            .expect("a loop test");

        self.loops.push(LoopFrame {
            continue_to: head,
            break_to: exit,
            scope_depth: self.scopes.len(),
            breaks: 0,
        });
        self.at(body_block);
        let body_ty = self.types.of(body).clone();
        if let Some(value) = self.expr(body) {
            self.drop(value, &body_ty);
            self.br(head);
        }
        self.loops.pop();

        self.at(exit);
        Some(self.be.unit_value())
    }

    fn lower_loop(&mut self, body: ExprId) -> Flow<'ctx> {
        let body_block = self.block("loop.body");
        let exit = self.block("loop.end");
        self.br(body_block);

        self.loops.push(LoopFrame {
            continue_to: body_block,
            break_to: exit,
            scope_depth: self.scopes.len(),
            breaks: 0,
        });
        self.at(body_block);
        let body_ty = self.types.of(body).clone();
        if let Some(value) = self.expr(body) {
            self.drop(value, &body_ty);
            self.br(body_block);
        }
        let frame = self.loops.pop().expect("the frame just pushed");

        self.at(exit);
        if frame.breaks == 0 {
            // Nothing branches out, so the loop never finishes and the code
            // after it cannot run.
            self.be.builder.build_unreachable().expect("sealing an endless loop");
            return None;
        }
        Some(self.be.unit_value())
    }

    fn lower_break(&mut self, value: Option<ExprId>, range: TextRange) -> Flow<'ctx> {
        if value.is_some() {
            return self.fail(
                "`break` with a value is not supported yet: a `loop`'s type is not inferred in \
                 phase 2, so there is nothing for the value to flow into",
                range,
            );
        }
        let Some(frame) = self.loops.last() else {
            return self.fail("`break` outside a loop", range);
        };
        let (target, depth) = (frame.break_to, frame.scope_depth);
        self.unwind_to(depth);
        self.loops.last_mut().expect("checked above").breaks += 1;
        self.br(target);
        None
    }

    fn lower_continue(&mut self, range: TextRange) -> Flow<'ctx> {
        let Some(frame) = self.loops.last() else {
            return self.fail("`continue` outside a loop", range);
        };
        let (target, depth) = (frame.continue_to, frame.scope_depth);
        self.unwind_to(depth);
        self.br(target);
        None
    }

    /// An early `return` from a fallible function is the ok case: it carries a
    /// value, not an error, and still has to wear the tag.
    fn return_value(&mut self, value: BasicValueEnum<'ctx>) {
        if self.raises {
            self.return_ok(value);
            return;
        }
        match self.ret {
            Type::Unit => {
                self.be.builder.build_return(None).expect("returning unit");
            }
            _ => {
                self.be.builder.build_return(Some(&value)).expect("returning a value");
            }
        }
    }

    fn lower_return(&mut self, value: Option<ExprId>) -> Flow<'ctx> {
        let value = match value {
            Some(expr) => Some(self.expr(expr)?),
            None => None,
        };
        // Every scope, not just the innermost: a `return` leaves the whole
        // frame, and the parameters are released by the outermost one.
        self.unwind_to(0);

        let value = value.unwrap_or_else(|| self.be.unit_value());
        self.return_value(value);
        None
    }

    // -----------------------------------------------------------------------
    // Match
    // -----------------------------------------------------------------------

    /// Lowers a `match`.
    ///
    /// # Who owns the scrutinee
    ///
    /// The plan `dup`s a boxed scrutinee at the read and records nothing for
    /// the arm bindings, which is the right call: the bindings *borrow* fields
    /// out of the scrutinee, so releasing them would free something the parent
    /// still points at. What follows is that the `match` itself owns the
    /// scrutinee for the whole of the arm, and releases it afterwards — after
    /// the body, because an arm that returns a binding has to `dup` it out
    /// first, and before the value escapes, because nothing else will.
    ///
    /// It goes on the scope stack rather than being dropped at the join, so a
    /// `return` inside an arm releases it too.
    fn lower_match(
        &mut self,
        id: ExprId,
        scrutinee: ExprId,
        arms: &[MatchArm],
        range: TextRange,
    ) -> Flow<'ctx> {
        let scrutinee_ty = self.types.of(scrutinee).clone();
        let value = self.expr(scrutinee)?;

        self.scopes.push(if is_boxed(&scrutinee_ty) {
            vec![Cleanup::Temp(value, scrutinee_ty.clone())]
        } else {
            Vec::new()
        });

        let result_ty = self.types.of(id).clone();
        let slot = self.result_slot(&result_ty);
        let merge = self.block("match.end");
        let reached = self.emit_arms(arms, value, &scrutinee_ty, slot, merge, range);

        let scope = self.scopes.pop().unwrap_or_default();
        self.at(merge);
        if reached == 0 {
            self.be.builder.build_unreachable().expect("sealing an unreachable join");
            return None;
        }
        for cleanup in scope.into_iter().rev() {
            self.release(cleanup);
        }
        Some(self.load_result(slot, &result_ty))
    }

    /// `f()! catch { .. }` — the same branch `!` already emits, with the
    /// handled error types diverted to arms instead of returned onward.
    ///
    /// The dispatch is two levels. `which` says which error *type* arrived, and
    /// a type nobody named falls through to the ordinary propagate path, so a
    /// partial `catch` costs one extra `switch` and nothing else. Within a
    /// named type the arms dispatch on the object tag exactly as a `match`
    /// does, which is why they share `emit_arms`.
    fn lower_catch(
        &mut self,
        id: ExprId,
        inner: ExprId,
        arms: &[MatchArm],
        range: TextRange,
    ) -> Flow<'ctx> {
        let result_ty = self.types.of(id).clone();
        let slot = self.result_slot(&result_ty);
        let merge = self.block("catch.end");

        // The phis have to exist before the operand is lowered, because each
        // `!` inside it adds an edge as it is emitted.
        let entry = self.here();
        let handler = self.block("catch.raised");
        self.at(handler);
        let which = self
            .be
            .builder
            .build_phi(self.be.ctx.i32_type(), "which")
            .expect("the error type that arrived");
        let word = self
            .be
            .builder
            .build_phi(self.be.ctx.i64_type(), "error")
            .expect("the error that arrived");
        self.at(entry);

        let depth = self.scopes.len();
        self.catches.push(CatchFrame { handler, which, word, scope_depth: depth });
        let value = self.expr(inner);
        self.catches.pop();

        let mut reached = 0;
        if let Some(value) = value {
            self.store_result(slot, value);
            self.br(merge);
            reached += 1;
        }

        // An operand that cannot raise leaves the handler with no way in. The
        // checker reports that as an error of its own, so this only has to
        // emit something a verifier will accept.
        if which.count_incoming() == 0 {
            self.at(handler);
            self.be.builder.build_unreachable().expect("sealing an unreachable handler");
            return self.join(merge, reached, slot, &result_ty);
        }

        // Group the arms by the error type they name, keeping written order so
        // the emitted blocks read in the order the source does.
        let mut caught: Vec<(String, Vec<MatchArm>)> = Vec::new();
        for arm in arms {
            let Some(owner) = self.owner_of(arm.pat) else { continue };
            match caught.iter_mut().find(|(name, _)| name == &owner) {
                Some((_, mine)) => mine.push(arm.clone()),
                None => caught.push((owner, vec![arm.clone()])),
            }
        }

        let onward = self.block("catch.onward");
        let cases: Vec<(inkwell::values::IntValue<'ctx>, BasicBlock<'ctx>)> = caught
            .iter()
            .map(|(owner, _)| {
                let id = self.be.error_id(owner);
                let tag = self.be.ctx.i32_type().const_int(u64::from(id), false);
                (tag, self.block(&format!("catch.{owner}")))
            })
            .collect();

        self.at(handler);
        let which = which.as_basic_value().into_int_value();
        let word = word.as_basic_value().into_int_value();
        self.be
            .builder
            .build_switch(which, onward, &cases)
            .expect("dispatching on the error type");

        // Not ours: release the frame and hand it to whoever is next. Nested
        // `catch`es chain here, since `leave_with` looks at the stack again
        // and this runs with the inner frame already popped.
        self.at(onward);
        self.leave_with(which, word);

        for ((owner, mine), (_, block)) in caught.iter().zip(&cases) {
            self.at(*block);
            let error_ty = Type::adt(owner);
            let error = self.be.word_to_value(word, &error_ty);

            // The raising frame moved the error into its return, so this frame
            // owns it. The arms borrow their bindings out of it, exactly as a
            // `match` borrows out of a temporary scrutinee, and it is released
            // on the way to the join.
            let released = self.block(&format!("catch.{owner}.done"));
            self.scopes.push(vec![Cleanup::Temp(error, error_ty.clone())]);
            let reached_here = self.emit_arms(mine, error, &error_ty, slot, released, range);
            let scope = self.scopes.pop().unwrap_or_default();

            self.at(released);
            if reached_here == 0 {
                self.be.builder.build_unreachable().expect("sealing a diverging handler");
                continue;
            }
            for cleanup in scope.into_iter().rev() {
                self.release(cleanup);
            }
            self.br(merge);
            reached += 1;
        }

        self.join(merge, reached, slot, &result_ty)
    }

    /// Arrives at a join block, or seals it if nothing reaches it.
    fn join(
        &mut self,
        merge: BasicBlock<'ctx>,
        reached: usize,
        slot: Option<PointerValue<'ctx>>,
        ty: &Type,
    ) -> Flow<'ctx> {
        self.at(merge);
        if reached == 0 {
            self.be.builder.build_unreachable().expect("sealing an unreachable join");
            return None;
        }
        Some(self.load_result(slot, ty))
    }

    /// The error type a `catch` arm names, by its constructor.
    fn owner_of(&self, pat: khora_hir::body::PatId) -> Option<String> {
        match self.body.pat(pat) {
            khora_hir::body::Pat::Path(r)
            | khora_hir::body::Pat::TupleStruct { resolution: r, .. } => match r {
                khora_hir::Resolution::Variant { type_name, .. } => Some(type_name.clone()),
                _ => None,
            },
            _ => None,
        }
    }

    /// Emits the arms of a `match` or a `catch` over `value` and returns how
    /// many of them reach `merge`.
    ///
    /// Shared because a `catch` arm is a `match` arm in every respect except
    /// what it is matching on: one error type's variants rather than a
    /// scrutinee's. None is zero if every arm diverges, and the caller has to
    /// seal `merge` rather than join to it.
    fn emit_arms(
        &mut self,
        arms: &[MatchArm],
        value: BasicValueEnum<'ctx>,
        ty: &Type,
        slot: Option<PointerValue<'ctx>>,
        merge: BasicBlock<'ctx>,
        range: TextRange,
    ) -> usize {
        // One pair of blocks per arm: bindings and guard first, then the body,
        // so a failing guard can jump on without the body ever being entered.
        let mut binds = Vec::with_capacity(arms.len());
        let mut bodies = Vec::with_capacity(arms.len());
        for index in 0..arms.len() {
            binds.push(self.block(&format!("arm{index}.bind")));
            bodies.push(self.block(&format!("arm{index}.body")));
        }

        self.dispatch(arms, value, ty, &binds, range);

        let mut reached = 0;
        for (index, arm) in arms.iter().enumerate() {
            self.at(binds[index]);
            self.bind_pattern(arm.pat, value);
            match arm.guard {
                Some(guard) => {
                    // A guard is checked with the bindings in scope and, if it
                    // fails, hands the value to the next arm untouched.
                    let next = binds.get(index + 1).copied().unwrap_or(merge);
                    match self.expr(guard) {
                        Some(test) => {
                            self.be
                                .builder
                                .build_conditional_branch(
                                    test.into_int_value(),
                                    bodies[index],
                                    next,
                                )
                                .expect("a guard branch");
                        }
                        // A guard that never returns — `if (return 0)` — leaves
                        // the body with no way in. Seal it, or the block sits
                        // there unterminated and fails verification a long way
                        // from the guard that caused it.
                        None => {
                            self.at(bodies[index]);
                            self.be
                                .builder
                                .build_unreachable()
                                .expect("sealing an unenterable arm");
                            continue;
                        }
                    }
                }
                None => self.br(bodies[index]),
            }

            self.at(bodies[index]);
            if let Some(value) = self.expr(arm.body) {
                self.store_result(slot, value);
                self.br(merge);
                reached += 1;
            }
        }
        reached
    }

    /// Branches to the first arm whose pattern applies.
    ///
    /// Two shapes, and the difference is worth the second code path. When every
    /// arm is an unguarded constructor pattern with irrefutable fields — the
    /// overwhelmingly common `match` — the tag goes straight into an LLVM
    /// `switch`, which is a jump table. Anything else (guards, literal
    /// patterns, nested constructors, a non-ADT scrutinee) becomes a chain of
    /// tests, tried in written order, which is the only shape that gets
    /// fallthrough right.
    fn dispatch(
        &mut self,
        arms: &[MatchArm],
        value: BasicValueEnum<'ctx>,
        scrutinee_ty: &Type,
        binds: &[BasicBlock<'ctx>],
        range: TextRange,
    ) {
        if arms.is_empty() {
            self.fail("a `match` needs at least one arm", range);
            return;
        }

        if let Some(plan) = self.switch_plan(arms, scrutinee_ty) {
            let tag = runtime::load_tag(self.be.ctx, &self.be.builder, value.into_pointer_value());
            let mut cases = Vec::new();
            let mut default = None;
            for (index, entry) in plan.into_iter().enumerate() {
                match entry {
                    Some(tag_value) => {
                        let case = self.be.ctx.i32_type().const_int(tag_value as u64, false);
                        cases.push((case, binds[index]));
                    }
                    None => default = Some(binds[index]),
                }
            }
            let default = default.unwrap_or_else(|| self.unmatched_block());
            self.be.builder.build_switch(tag, default, &cases).expect("a tag switch");
            return;
        }

        let tests: Vec<BasicBlock<'ctx>> =
            (0..arms.len()).map(|i| self.block(&format!("arm{i}.test"))).collect();
        let unmatched = self.unmatched_block();
        self.br(tests[0]);

        for (index, arm) in arms.iter().enumerate() {
            let next = tests.get(index + 1).copied().unwrap_or(unmatched);
            self.at(tests[index]);
            self.test_pattern(arm.pat, value, binds[index], next);
        }
    }

    /// A tag per arm, or `None` for an arm that matches anything, when the
    /// whole `match` can dispatch through one `switch`.
    fn switch_plan(&self, arms: &[MatchArm], scrutinee_ty: &Type) -> Option<Vec<Option<u32>>> {
        if !matches!(scrutinee_ty, Type::Adt { .. }) {
            return None;
        }

        let mut plan = Vec::with_capacity(arms.len());
        let mut seen: Vec<u32> = Vec::new();
        let mut catch_all = false;

        for arm in arms {
            if arm.guard.is_some() {
                return None;
            }
            // Anything after a catch-all is unreachable, which the checker
            // rejects; refusing here too keeps this from having to model it.
            if catch_all {
                return None;
            }
            match self.body.pat(arm.pat) {
                Pat::Wildcard | Pat::Bind(_) => {
                    catch_all = true;
                    plan.push(None);
                }
                Pat::Path(resolution) => {
                    let tag = self.tag_of(resolution)?;
                    if seen.contains(&tag) {
                        return None;
                    }
                    seen.push(tag);
                    plan.push(Some(tag));
                }
                Pat::TupleStruct { resolution, fields } => {
                    // A refutable field pattern needs a test the switch has
                    // nowhere to put.
                    if !fields.iter().all(|f| self.is_irrefutable(*f)) {
                        return None;
                    }
                    let tag = self.tag_of(resolution)?;
                    if seen.contains(&tag) {
                        return None;
                    }
                    seen.push(tag);
                    plan.push(Some(tag));
                }
                _ => return None,
            }
        }
        Some(plan)
    }

    fn tag_of(&self, resolution: &khora_hir::Resolution) -> Option<u32> {
        match resolution {
            khora_hir::Resolution::Variant { type_name, name, .. } => {
                self.be.variant_of(type_name, name).map(|(tag, _)| tag)
            }
            _ => None,
        }
    }

    fn is_irrefutable(&self, pat: PatId) -> bool {
        matches!(self.body.pat(pat), Pat::Wildcard | Pat::Bind(_) | Pat::Missing)
    }

    /// A block for "no arm applied".
    ///
    /// Exhaustiveness checking says this cannot happen, so `unreachable` alone
    /// would be correct — and would make a bug in that checker into undefined
    /// behavior rather than a crash. A trap first costs one instruction on a
    /// path nothing takes.
    fn unmatched_block(&mut self) -> BasicBlock<'ctx> {
        let current = self.here();
        let block = self.block("match.unmatched");
        self.at(block);
        let trap = self.be.rt.trap;
        self.be.builder.build_call(trap, &[], "").expect("a trap");
        self.be.builder.build_unreachable().expect("sealing the trap");
        self.at(current);
        block
    }

    /// Emits the tests a pattern requires, ending the current block.
    ///
    /// Chained rather than combined into one condition, because the tests are
    /// not independent: reading field 0 of a `Cons` is only safe once the tag
    /// says it *is* a `Cons`, and a `Nil` has no field 0 to read.
    fn test_pattern(
        &mut self,
        pat: PatId,
        value: BasicValueEnum<'ctx>,
        success: BasicBlock<'ctx>,
        failure: BasicBlock<'ctx>,
    ) {
        let range = TextRange::empty(0.into());
        match self.body.pat(pat).clone() {
            Pat::Wildcard | Pat::Bind(_) | Pat::Missing => self.br(success),
            Pat::Literal(Literal::Int(text)) => {
                let Some(literal) = parse_int(&text) else {
                    self.fail(format!("`{text}` does not fit in an `Int`"), range);
                    return;
                };
                let expected = self.be.ctx.i64_type().const_int(literal as u64, false);
                self.branch_on_equal(value.into_int_value(), expected, success, failure);
            }
            Pat::Literal(Literal::Bool(expected)) => {
                let expected = self.be.ctx.bool_type().const_int(expected as u64, false);
                self.branch_on_equal(value.into_int_value(), expected, success, failure);
            }
            Pat::Literal(_) => {
                self.fail(
                    "matching a `String` or a float literal needs a runtime comparison the \
                     backend does not generate yet",
                    range,
                );
            }
            Pat::Path(resolution) => {
                let Some(tag) = self.tag_of(&resolution) else {
                    self.fail("this pattern does not name a constructor", range);
                    return;
                };
                let loaded =
                    runtime::load_tag(self.be.ctx, &self.be.builder, value.into_pointer_value());
                let expected = self.be.ctx.i32_type().const_int(tag as u64, false);
                self.branch_on_equal(loaded, expected, success, failure);
            }
            Pat::TupleStruct { resolution, fields } => {
                let Some((tag, info)) = self.variant_of(&resolution) else {
                    self.fail("this pattern does not name a constructor", range);
                    return;
                };
                let object = value.into_pointer_value();
                let loaded = runtime::load_tag(self.be.ctx, &self.be.builder, object);
                let expected = self.be.ctx.i32_type().const_int(tag as u64, false);
                let matched = self.block("case");
                self.branch_on_equal(loaded, expected, matched, failure);

                self.at(matched);
                self.test_fields(object, &info, &fields, 0, success, failure);
            }
            Pat::Tuple(_) => {
                self.fail("tuple patterns are not supported yet", range);
            }
        }
    }

    fn test_fields(
        &mut self,
        object: PointerValue<'ctx>,
        info: &VariantInfo,
        fields: &[PatId],
        index: usize,
        success: BasicBlock<'ctx>,
        failure: BasicBlock<'ctx>,
    ) {
        if index >= fields.len() {
            self.br(success);
            return;
        }
        if self.is_irrefutable(fields[index]) {
            self.test_fields(object, info, fields, index + 1, success, failure);
            return;
        }

        let field_ty = info.fields.get(index).cloned().unwrap_or(Type::Unknown);
        let value = self.load_field(object, index, &field_ty);
        let next = self.block("field.next");
        self.test_pattern(fields[index], value, next, failure);
        self.at(next);
        self.test_fields(object, info, fields, index + 1, success, failure);
    }

    fn branch_on_equal(
        &mut self,
        value: inkwell::values::IntValue<'ctx>,
        expected: inkwell::values::IntValue<'ctx>,
        success: BasicBlock<'ctx>,
        failure: BasicBlock<'ctx>,
    ) {
        let test = self
            .be
            .builder
            .build_int_compare(IntPredicate::EQ, value, expected, "matches")
            .expect("a pattern test");
        self.be
            .builder
            .build_conditional_branch(test, success, failure)
            .expect("a pattern branch");
    }

    /// Writes a pattern's bindings into their slots.
    ///
    /// No `dup` anywhere: a binding borrows out of the scrutinee, which the
    /// `match` owns for the duration of the arm. A read of the binding is what
    /// duplicates, and the plan records that read.
    fn bind_pattern(&mut self, pat: PatId, value: BasicValueEnum<'ctx>) {
        match self.body.pat(pat).clone() {
            Pat::Bind(local) => {
                if let Some(slot) = self.slots.get(&local).copied() {
                    self.be.builder.build_store(slot, value).expect("binding a pattern");
                }
            }
            Pat::TupleStruct { resolution, fields } => {
                let Some((_, info)) = self.variant_of(&resolution) else { return };
                let object = value.into_pointer_value();
                for (index, field) in fields.iter().enumerate() {
                    // **The binding's own type, not the variant's declared
                    // one.** `Option::Some(value: A)` declares `A`, and at
                    // `Option<Bool>` the declared type is still `A` — which has
                    // no machine type, so the field came back as an `i64`.
                    //
                    // That was right by accident for everything word-sized and
                    // a *stack overflow* for anything narrower: `v` in
                    // `Option::Some(v)` at `Bool` is a one-byte slot, and
                    // storing eight bytes into it wrote over whatever the
                    // frame put next — which was the scrutinee, so the object
                    // was never released. Errata 44.
                    //
                    // The checker recorded the specialized type of every bound
                    // local, so the leaf knows exactly what it is. A nested
                    // pattern keeps the declared type, which is a pointer for
                    // anything a pattern can descend into.
                    let field_ty = match self.body.pat(*field) {
                        Pat::Bind(local) => self.types.local(*local).clone(),
                        _ => info.fields.get(index).cloned().unwrap_or(Type::Unknown),
                    };
                    let loaded = self.load_field(object, index, &field_ty);
                    self.bind_pattern(*field, loaded);
                }
            }
            Pat::Wildcard | Pat::Literal(_) | Pat::Path(_) | Pat::Missing | Pat::Tuple(_) => {}
        }
    }

    fn variant_of(&self, resolution: &khora_hir::Resolution) -> Option<(u32, VariantInfo)> {
        match resolution {
            khora_hir::Resolution::Variant { type_name, name, .. } => {
                self.be.variant_of(type_name, name)
            }
            _ => None,
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
