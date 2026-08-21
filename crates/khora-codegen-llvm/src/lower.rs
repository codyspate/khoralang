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
use inkwell::values::{BasicValueEnum, FunctionValue, PointerValue};
use inkwell::{AddressSpace, IntPredicate};

use khora_hir::body::{
    BinOp, Body, Expr, ExprId, Literal, LocalId, MatchArm, Pat, PatId, Stmt, UnOp,
};
use khora_perceus::{is_boxed, RcPlan};
use khora_types::{BodyTypes, Type, VariantInfo};
use text_size::TextRange;

use crate::backend::{Backend, CLOSURE_ADAPTER_TAG, CLOSURE_CAPTURE_BASE};
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
        slots: HashMap::new(),
        scopes: Vec::new(),
        loops: Vec::new(),
        aborted: false,
    };

    lower.allocate_slots();
    lower.bind_parameters();

    let value = match body.root {
        Some(root) => lower.expr(root),
        None => Some(lower.be.unit_value()),
    };
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
        slots: HashMap::new(),
        scopes: Vec::new(),
        loops: Vec::new(),
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
    slots: HashMap<LocalId, PointerValue<'ctx>>,
    scopes: Vec<Vec<Cleanup<'ctx>>>,
    loops: Vec<LoopFrame<'ctx>>,
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
    fn bind_parameters(&mut self) {
        for (index, pat) in self.body.params.iter().enumerate() {
            let Pat::Bind(local) = self.body.pat(*pat).clone() else { continue };
            let Some(slot) = self.slots.get(&local).copied() else { continue };
            let Some(value) = self.function.get_nth_param(index as u32) else { continue };
            self.be.builder.build_store(slot, value).expect("storing a parameter");
        }
    }

    /// Emits the function's `ret`, and repairs the IR if lowering gave up.
    fn finish(&mut self, value: Flow<'ctx>) {
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
            Expr::Literal(lit) => self.literal(lit, range),
            Expr::Local(local) => self.read_local(id, local, range),
            Expr::Path(resolution) => self.path(id, &resolution, range),
            Expr::Call { callee, args } => self.call(callee, &args, range),
            Expr::Binary { op, lhs, rhs } => self.binary(op, lhs, rhs, range),
            Expr::Unary { op, operand } => self.unary(op, operand, range),
            Expr::Assign { target, value } => self.assign(target, value, range),
            Expr::Block { stmts, tail } => self.lower_block(id, &stmts, tail),
            Expr::If { condition, then_branch, else_branch } => {
                self.lower_if(id, condition, then_branch, else_branch)
            }
            Expr::Match { scrutinee, arms } => self.lower_match(id, scrutinee, &arms, range),
            Expr::While { condition, body } => self.lower_while(condition, body),
            Expr::Loop { body } => self.lower_loop(body),
            Expr::Break(value) => self.lower_break(value, range),
            Expr::Continue => self.lower_continue(range),
            Expr::Return(value) => self.lower_return(value),
            Expr::Lambda { captures, .. } => self.make_closure(id, &captures, range),
            Expr::Field { .. } => {
                self.fail("field access needs records, which arrive in phase 3", range)
            }
            Expr::List(_) => self.fail("list literals are not supported yet", range),
            Expr::Tuple(_) => self.fail("tuple literals are not supported yet", range),
            Expr::Unsupported(what) => self.fail(format!("{what} are not supported yet"), range),
            // The checker already rejected these, so reaching one means
            // `compile` ran with diagnostics it should have refused.
            Expr::Missing | Expr::Unresolved(_) => {
                self.fail("this expression did not survive the front end", range)
            }
        }
    }

    fn literal(&mut self, lit: Literal, range: TextRange) -> Flow<'ctx> {
        match lit {
            Literal::Int(text) => match parse_int(&text) {
                Some(value) => {
                    // `sign_extend` is false because `value` is already the
                    // exact bit pattern; LLVM would otherwise re-extend a
                    // negative literal that has none of its bits to spare.
                    Some(self.be.ctx.i64_type().const_int(value as u64, false).into())
                }
                None => self.fail(format!("`{text}` does not fit in an `Int`"), range),
            },
            Literal::Bool(value) => {
                Some(self.be.ctx.bool_type().const_int(value as u64, false).into())
            }
            Literal::Str(text) => self.string_literal(&text),
            Literal::Float(_) => {
                self.fail("floating point is not part of the phase 2 subset", range)
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

    fn call(&mut self, callee: ExprId, args: &[ExprId], range: TextRange) -> Flow<'ctx> {
        match self.body.expr(callee).clone() {
            Expr::Path(khora_hir::Resolution::Variant { type_name, name, .. }) => {
                self.construct(&type_name, &name, args, range)
            }
            Expr::Path(khora_hir::Resolution::TraitItem { name, .. }) => {
                match self.mono.callee(&self.owner.clone(), callee) {
                    Some(symbol) => self.call_named(&symbol, args, range),
                    None => self.fail(
                        format!("`{name}` was not resolved to an impl; that is a compiler bug"),
                        range,
                    ),
                }
            }
            Expr::Path(khora_hir::Resolution::Item { name, .. }) => {
                if name == "print" && args.len() == 1 {
                    self.print(args[0], range)
                } else {
                    // A generic callee resolves to the specialization this call
                    // site asked for; a concrete one keeps its own name.
                    let symbol = self
                        .mono
                        .callee(&self.owner.clone(), callee)
                        .unwrap_or_else(|| name.clone());
                    self.call_named(&symbol, args, range)
                }
            }
            // `a.show()` — the receiver becomes the first argument, and which
            // impl runs was settled by monomorphization.
            Expr::Field { base, .. } => match self.mono.callee(&self.owner.clone(), callee) {
                Some(symbol) => {
                    let mut all = vec![base];
                    all.extend_from_slice(args);
                    self.call_named(&symbol, &all, range)
                }
                None => self.fail(
                    "this method call was not resolved to an impl; that is a compiler bug",
                    range,
                ),
            },
            // A value of function type: a closure, called indirectly.
            _ if matches!(self.types.of(callee), Type::Fn { .. }) => {
                let Type::Fn { params, ret } = self.types.of(callee).clone() else {
                    unreachable!("guarded by the match arm")
                };
                self.call_closure(callee, &params, &ret, args, range)
            }
            _ => self.fail(
                "only a named function or a constructor can be called; there are no function \
                 values until closures land",
                range,
            ),
        }
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

    /// Allocates the closure object for a lambda expression.
    ///
    /// Field 0 holds the lifted function's address and the captures follow, all
    /// under the ordinary object header — so a closure is dup'ed, dropped and
    /// counted by exactly the machinery every other heap value already uses.
    fn make_closure(
        &mut self,
        id: ExprId,
        captures: &[LocalId],
        range: TextRange,
    ) -> Flow<'ctx> {
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

        let fields = CLOSURE_CAPTURE_BASE + captures.len();
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
    fn call_closure(
        &mut self,
        callee: ExprId,
        params: &[Type],
        ret: &Type,
        args: &[ExprId],
        range: TextRange,
    ) -> Flow<'ctx> {
        let closure = self.expr(callee)?.into_pointer_value();

        if args.len() != params.len() {
            return self.fail(
                format!("this call takes {} argument(s), but {} were given", params.len(), args.len()),
                range,
            );
        }
        let mut values: Vec<inkwell::values::BasicMetadataValueEnum<'ctx>> = vec![closure.into()];
        for arg in args {
            values.push(self.expr(*arg)?.into());
        }

        let ptr = self.be.ctx.ptr_type(AddressSpace::default());
        let mut param_types: Vec<inkwell::types::BasicMetadataTypeEnum<'ctx>> = vec![ptr.into()];
        for param in params {
            let Some(ty) = self.be.llvm_type(param) else {
                return self.fail("a closure parameter has no machine type", range);
            };
            param_types.push(ty.into());
        }
        let fn_type = match ret {
            Type::Unit => self.be.ctx.void_type().fn_type(&param_types, false),
            other => match self.be.llvm_type(other) {
                Some(ty) => ty.fn_type(&param_types, false),
                None => return self.fail("a closure's result has no machine type", range),
            },
        };

        let slot = runtime::field_pointer(self.be.ctx, &self.be.builder, closure, 0);
        let code = self
            .be
            .builder
            .build_load(ptr, slot, "closure.code")
            .expect("loading a closure's code pointer")
            .into_pointer_value();

        let call = self
            .be
            .builder
            .build_indirect_call(fn_type, code, &values, "closure.call")
            .expect("calling a closure");

        let result = match ret {
            Type::Unit => self.be.unit_value(),
            _ => call.try_as_basic_value().basic().unwrap_or_else(|| self.be.unit_value()),
        };

        // The call site owns a reference to the closure: reading a local dup'ed
        // it, and a lambda written in place was born owned. The callee only
        // borrows it — a lifted body reads its captures without taking a
        // reference — so the release belongs here, after the call.
        let callee_ty = self.types.of(callee).clone();
        self.drop(closure.into(), &callee_ty);
        Some(result)
    }

    fn call_named(&mut self, name: &str, args: &[ExprId], range: TextRange) -> Flow<'ctx> {
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

        let call = self.be.builder.build_call(function, &values, "call").expect("a call");
        Some(match signature.ret {
            Type::Unit => self.be.unit_value(),
            _ => call.try_as_basic_value().basic().unwrap_or_else(|| self.be.unit_value()),
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

        let alloc = self.be.rt.alloc;
        let field_bytes = FIELD_WORD * info.fields.len() as u64;
        let object = self
            .be
            .builder
            .build_call(
                alloc,
                &[
                    self.be.ctx.i64_type().const_int(field_bytes, false).into(),
                    self.be.ctx.i32_type().const_int(tag as u64, false).into(),
                ],
                &format!("{case}.obj"),
            )
            .expect("allocating an ADT")
            .try_as_basic_value()
            .basic()
            .expect("khora_alloc returns a pointer")
            .into_pointer_value();

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
            Type::Bool => {
                let word = self
                    .be
                    .builder
                    .build_load(self.be.ctx.i64_type(), slot, "field.word")
                    .expect("reading a Bool field")
                    .into_int_value();
                self.be
                    .builder
                    .build_int_truncate(word, self.be.ctx.bool_type(), "field")
                    .expect("narrowing a Bool field")
                    .into()
            }
            Type::Str | Type::Adt { .. } => self
                .be
                .builder
                .build_load(self.be.ctx.ptr_type(AddressSpace::default()), slot, "field")
                .expect("reading a boxed field"),
            _ => self
                .be
                .builder
                .build_load(self.be.ctx.i64_type(), slot, "field")
                .expect("reading a word field"),
        }
    }

    // -----------------------------------------------------------------------
    // Operators
    // -----------------------------------------------------------------------

    fn binary(&mut self, op: BinOp, lhs: ExprId, rhs: ExprId, range: TextRange) -> Flow<'ctx> {
        if matches!(op, BinOp::And | BinOp::Or) {
            return self.logical(op, lhs, rhs);
        }

        let operand_ty = self.types.of(lhs).clone();
        let left = self.expr(lhs)?;
        let right = self.expr(rhs)?;

        match op {
            BinOp::Add if matches!(operand_ty, Type::Str) => {
                self.drop(left, &Type::Str);
                self.drop(right, &Type::Str);
                self.fail(
                    "string concatenation needs a runtime routine that does not exist yet; \
                     `khora-rt` has no allocator-side `concat`",
                    range,
                )
            }
            BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Rem => {
                let (l, r) = (left.into_int_value(), right.into_int_value());
                let value = match op {
                    BinOp::Add => self.be.builder.build_int_add(l, r, "add"),
                    BinOp::Sub => self.be.builder.build_int_sub(l, r, "sub"),
                    BinOp::Mul => self.be.builder.build_int_mul(l, r, "mul"),
                    // Signed division by zero faults rather than trapping
                    // politely. Phase 2 has no error channel to raise into;
                    // `raises` in phase 4 is where a checked `/` belongs.
                    BinOp::Div => self.be.builder.build_int_signed_div(l, r, "div"),
                    _ => self.be.builder.build_int_signed_rem(l, r, "rem"),
                };
                Some(value.expect("integer arithmetic").into())
            }
            _ => self.compare(op, left, right, &operand_ty, range),
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
        op: BinOp,
        left: BasicValueEnum<'ctx>,
        right: BasicValueEnum<'ctx>,
        operand_ty: &Type,
        range: TextRange,
    ) -> Flow<'ctx> {
        // `Bool` is an `i1`, where "less than" means `false < true`, so its
        // ordering is unsigned. Signing an `i1` comparison inverts it.
        let signed = !matches!(operand_ty, Type::Bool);
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

        // Strings compare by their bytes, not by their address: two `"a"`
        // literals are separate allocations and a pointer comparison would call
        // them different.
        if matches!(operand_ty, Type::Str) && matches!(op, BinOp::Eq | BinOp::Ne) {
            return self.compare_strings(op, left, right);
        }

        if !matches!(operand_ty, Type::Int | Type::Bool | Type::Unit) {
            self.drop(left, operand_ty);
            self.drop(right, operand_ty);
            return self.fail(
                format!(
                    "two `{operand_ty}` values can only be compared with `==` and `!=`, and \
                     ordering one needs a `Ord` impl the backend does not call yet"
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
        let value = self.expr(operand)?.into_int_value();
        let result = match op {
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
        let Expr::Local(local) = self.body.expr(target).clone() else {
            return self.fail(
                "only a `let mut` binding can be assigned to; fields need records",
                range,
            );
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
                Stmt::Let { pat, init } => self.lower_let(*pat, *init),
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

    fn lower_return(&mut self, value: Option<ExprId>) -> Flow<'ctx> {
        let value = match value {
            Some(expr) => Some(self.expr(expr)?),
            None => None,
        };
        // Every scope, not just the innermost: a `return` leaves the whole
        // frame, and the parameters are released by the outermost one.
        self.unwind_to(0);

        match (&self.ret, value) {
            (Type::Unit, _) | (_, None) => {
                self.be.builder.build_return(None).expect("returning unit");
            }
            (_, Some(value)) => {
                self.be.builder.build_return(Some(&value)).expect("returning a value");
            }
        }
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

        // One pair of blocks per arm: bindings and guard first, then the body,
        // so a failing guard can jump on without the body ever being entered.
        let mut binds = Vec::with_capacity(arms.len());
        let mut bodies = Vec::with_capacity(arms.len());
        for index in 0..arms.len() {
            binds.push(self.block(&format!("arm{index}.bind")));
            bodies.push(self.block(&format!("arm{index}.body")));
        }

        self.dispatch(arms, value, &scrutinee_ty, &binds, range);

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
                    let field_ty = info.fields.get(index).cloned().unwrap_or(Type::Unknown);
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
