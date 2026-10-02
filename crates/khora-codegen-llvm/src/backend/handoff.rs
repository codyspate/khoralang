//! Hand-off glue: what `std::core::Handoff<A>` needs to know about `A` to
//! give a value away.
//!
//! **What this prevents: a `Share` part of a value refused, or a writable one
//! passed, because the runtime cannot tell them apart.** A hand-off's send
//! walks the value and requires every writable object in it to be held only
//! from inside it, while a `Share` object may also be held outside and is
//! then marked (`khora-rt`'s `handoff.rs`). Which is which is a fact about
//! types. Drop glue knows only \"these are the children\", and the prototype
//! that walked with it had to assume, behind a switch, that any aliased
//! descendant was `Share`.
//!
//! So each type reached from a hand-off's `A` gets a constant, a
//! `HandoffType`: how it is walked, its hand-off glue or drop glue, and its
//! name for the trap. A writable type's **hand-off glue** is drop glue's
//! twin: one routine per type, switching on the tag, visiting each field that
//! holds a reference with that field's own `HandoffType`. Read at *this*
//! instantiation, for the reason drop glue is: a generic field is a parameter,
//! and the declaration cannot say what it holds.
//!
//! A field held inline has no header and so no count; the glue walks into it
//! in place, as `inline.rs` does for counting. An array's elements are walked
//! by the runtime's loop, calling a routine generated per element type.

use super::*;
use inkwell::values::GlobalValue;

/// What the runtime calls a type's walk, as `khora-rt`'s `Walked`.
#[derive(Clone, Copy)]
enum Walked {
    Writable = 0,
    Share = 1,
    Unseen = 2,
}

impl<'ctx> Backend<'ctx> {
    /// The `HandoffType` constant a hand-off of `held` is opened with.
    ///
    /// **The box, when `held` is held inline.** Such a value crosses a queue
    /// in a box of its own (`counted_across`), and the box is what the send
    /// is handed: a header, a count and the value at field zero. Its
    /// description walks into the value and releases it with the spill glue.
    /// `Lent = { c: Connection }` is one: a record with no `mut` field and a
    /// single pointer is held inline.
    pub fn handoff_root(&mut self, held: &Type) -> PointerValue<'ctx> {
        if self.unboxed.holds(held) {
            return self.spilled_handoff_type(held).as_pointer_value();
        }
        self.handoff_type(held).as_pointer_value()
    }

    /// The constant describing `ty`, made on first use.
    ///
    /// Declared before its glue is built, so a type that reaches itself -- a
    /// list's tail -- finds the constant rather than starting another.
    fn handoff_type(&mut self, ty: &Type) -> GlobalValue<'ctx> {
        let key = format!("kh$handoff${}", super::glue::type_key(ty));
        if let Some(found) = self.module.get_global(&key) {
            return found;
        }
        let shape = self.handoff_type_shape();
        let global = self.module.add_global(shape, None, &key);
        global.set_linkage(Linkage::Private);
        global.set_constant(true);
        global.set_alignment(8);

        let (walked, glue) = match ty {
            // What a closure captured is not in its type, so its parts are
            // found through its drop glue and each is held to the writable
            // rule. Sound, and it can refuse a closure holding an aliased
            // `Share` value.
            Type::Fn { .. } => (Walked::Unseen, self.null_pointer()),
            _ if self.types.is_shareable(ty, &[]) => (Walked::Share, self.null_pointer()),
            _ => (Walked::Writable, self.handoff_glue(ty)),
        };
        let drop = self.drop_glue(ty);
        let initial = self.handoff_type_value(walked, glue, drop, &ty.to_string());
        global.set_initializer(&initial);
        global
    }

    /// The description of the box a value held inline crosses in.
    fn spilled_handoff_type(&mut self, held: &Type) -> GlobalValue<'ctx> {
        let key = format!("kh$handoff$spilled${}", super::glue::type_key(held));
        if let Some(found) = self.module.get_global(&key) {
            return found;
        }
        let shape = self.handoff_type_shape();
        let global = self.module.add_global(shape, None, &key);
        global.set_linkage(Linkage::Private);
        global.set_constant(true);
        global.set_alignment(8);

        let glue = self.spilled_handoff_glue(held);
        let drop = self.spill_glue(held);
        let initial = self.handoff_type_value(Walked::Writable, glue, drop, &held.to_string());
        global.set_initializer(&initial);
        global
    }

    /// `{ u64 walked, ptr glue, ptr drop, ptr name, u64 name_len }`.
    fn handoff_type_shape(&self) -> inkwell::types::StructType<'ctx> {
        let i64t = self.ctx.i64_type();
        let ptr = self.ctx.ptr_type(AddressSpace::default());
        self.ctx.struct_type(&[i64t.into(), ptr.into(), ptr.into(), ptr.into(), i64t.into()], false)
    }

    fn handoff_type_value(
        &mut self,
        walked: Walked,
        glue: PointerValue<'ctx>,
        drop: PointerValue<'ctx>,
        name: &str,
    ) -> inkwell::values::StructValue<'ctx> {
        let i64t = self.ctx.i64_type();
        let text = self.ctx.const_string(name.as_bytes(), false);
        let bytes = self.module.add_global(text.get_type(), None, "kh$handoff$name");
        bytes.set_initializer(&text);
        bytes.set_linkage(Linkage::Private);
        bytes.set_constant(true);
        self.ctx.const_struct(
            &[
                i64t.const_int(walked as u64, false).into(),
                glue.into(),
                drop.into(),
                bytes.as_pointer_value().into(),
                i64t.const_int(name.len() as u64, false).into(),
            ],
            false,
        )
    }

    /// `void glue(void *object, void *walk)`, the shape every hand-off glue
    /// has.
    fn handoff_glue_type(&self) -> inkwell::types::FunctionType<'ctx> {
        let ptr = self.ctx.ptr_type(AddressSpace::default());
        self.ctx.void_type().fn_type(&[ptr.into(), ptr.into()], false)
    }

    /// A writable type's hand-off glue, or null where it holds nothing.
    ///
    /// Emitted at once rather than queued like drop glue: it builds with a
    /// builder of its own and puts the shared one back, so it can be asked
    /// for from the middle of a function body.
    fn handoff_glue(&mut self, ty: &Type) -> PointerValue<'ctx> {
        let name = format!("kh$handoff_glue${}", mangle_type(ty));
        if let Some(f) = self.module.get_function(&name) {
            return f.as_global_value().as_pointer_value();
        }
        // An array's elements are the runtime's to loop over: its length is
        // a run-time value. **Asked by shape, not by name alone**, for the
        // reason `drop_glue` gives: a program's own `Array` is a record.
        if let Type::Adt { name: type_name, args, .. } = ty {
            if type_name == runtime::ARRAY_TYPE && self.variants_for(ty).is_empty() {
                let element = args.first().cloned().unwrap_or(Type::Unknown);
                if !self.owns_a_reference(&element) {
                    return self.null_pointer();
                }
                return self.array_handoff_glue(&name, &element);
            }
        }
        let variants = self.instantiated_variants(ty);
        if !variants.iter().any(|v| v.fields.iter().any(|t| self.owns_a_reference(t))) {
            return self.null_pointer();
        }

        let f = self.module.add_function(&name, self.handoff_glue_type(), Some(Linkage::Internal));
        let saved = self.builder.get_insert_block();
        let object = f.get_nth_param(0).expect("the object").into_pointer_value();
        let walk = f.get_nth_param(1).expect("the walk").into_pointer_value();
        let entry = self.ctx.append_basic_block(f, "entry");
        let done = self.ctx.append_basic_block(f, "done");

        // **One case per variant, switching on the tag**, never a field list
        // assumed for all of them: the same rule, and the same reason, as
        // `emit_drop_glue`.
        let mut cases = Vec::new();
        for (tag, variant) in variants.into_iter().enumerate() {
            let (at, _) = self.field_layout(&variant.fields);
            let owned: Vec<(u64, Type)> = variant
                .fields
                .iter()
                .enumerate()
                .filter(|(_, t)| self.owns_a_reference(t))
                .map(|(i, t)| (at[i], t.clone()))
                .collect();
            if owned.is_empty() {
                continue;
            }
            let block = self.ctx.append_basic_block(f, &format!("visit.{}", variant.name));
            cases.push((self.ctx.i32_type().const_int(tag as u64, false), block));
            self.builder.position_at_end(block);
            for (index, field_ty) in owned {
                let slot = runtime::field_pointer(self.ctx, &self.builder, object, index);
                let held = self
                    .llvm_type(&field_ty)
                    .unwrap_or_else(|| self.ctx.ptr_type(AddressSpace::default()).into());
                let value =
                    self.builder.build_load(held, slot, "field").expect("loading a field to visit");
                self.visit_held(walk, value, &field_ty);
            }
            self.builder.build_unconditional_branch(done).expect("leaving a case");
        }
        self.builder.position_at_end(entry);
        let tag = runtime::load_tag(self.ctx, &self.builder, object);
        self.builder.build_switch(tag, done, &cases).expect("switching on a tag");
        self.builder.position_at_end(done);
        self.builder.build_return(None).expect("returning from hand-off glue");
        if let Some(block) = saved {
            self.builder.position_at_end(block);
        }
        f.as_global_value().as_pointer_value()
    }

    /// An array's hand-off glue: the runtime's loop over a routine that
    /// visits one element, given its slot.
    fn array_handoff_glue(&mut self, name: &str, element: &Type) -> PointerValue<'ctx> {
        let saved = self.builder.get_insert_block();
        let each_name = format!("kh$handoff_element${}", mangle_type(element));
        let each = match self.module.get_function(&each_name) {
            Some(f) => f,
            None => {
                let f = self.module.add_function(
                    &each_name,
                    self.handoff_glue_type(),
                    Some(Linkage::Internal),
                );
                let slot = f.get_nth_param(0).expect("the slot").into_pointer_value();
                let walk = f.get_nth_param(1).expect("the walk").into_pointer_value();
                let entry = self.ctx.append_basic_block(f, "entry");
                self.builder.position_at_end(entry);
                let held = self
                    .llvm_type(element)
                    .unwrap_or_else(|| self.ctx.ptr_type(AddressSpace::default()).into());
                let value =
                    self.builder.build_load(held, slot, "element").expect("loading an element");
                self.visit_held(walk, value, element);
                self.builder.build_return(None).expect("returning from an element's visit");
                f
            }
        };

        let f = self.module.add_function(name, self.handoff_glue_type(), Some(Linkage::Internal));
        let array = f.get_nth_param(0).expect("the array").into_pointer_value();
        let walk = f.get_nth_param(1).expect("the walk").into_pointer_value();
        let entry = self.ctx.append_basic_block(f, "entry");
        self.builder.position_at_end(entry);
        let elements = self.rt.handoff_elements;
        self.builder
            .build_call(
                elements,
                &[walk.into(), array.into(), each.as_global_value().as_pointer_value().into()],
                "",
            )
            .expect("walking an array's elements");
        self.builder.build_return(None).expect("returning from an array's hand-off glue");
        if let Some(block) = saved {
            self.builder.position_at_end(block);
        }
        f.as_global_value().as_pointer_value()
    }

    /// The glue for the box an inline value crosses in: visit the value at
    /// field zero.
    fn spilled_handoff_glue(&mut self, held: &Type) -> PointerValue<'ctx> {
        if !self.owns_a_reference(held) {
            return self.null_pointer();
        }
        let name = format!("kh$handoff_glue$spilled${}", mangle_type(held));
        if let Some(f) = self.module.get_function(&name) {
            return f.as_global_value().as_pointer_value();
        }
        let Some(shape) = self.unboxed_type(held) else { return self.null_pointer() };
        let saved = self.builder.get_insert_block();
        let f = self.module.add_function(&name, self.handoff_glue_type(), Some(Linkage::Internal));
        let object = f.get_nth_param(0).expect("the box").into_pointer_value();
        let walk = f.get_nth_param(1).expect("the walk").into_pointer_value();
        let entry = self.ctx.append_basic_block(f, "entry");
        self.builder.position_at_end(entry);
        let slot = runtime::field_pointer(self.ctx, &self.builder, object, 0);
        let value = self.builder.build_load(shape, slot, "spilled").expect("reading a spilled value");
        self.visit_held(walk, value, held);
        self.builder.build_return(None).expect("returning from a box's hand-off glue");
        if let Some(block) = saved {
            self.builder.position_at_end(block);
        }
        f.as_global_value().as_pointer_value()
    }

    /// Visits what one value holds: a counted pointer is handed to the
    /// runtime with its type's description, and a value held inline is
    /// walked into in place, since it has no header to count.
    ///
    /// Emits at the builder's position.
    fn visit_held(&mut self, walk: PointerValue<'ctx>, value: BasicValueEnum<'ctx>, ty: &Type) {
        if is_boxed(ty, &self.unboxed) {
            let described = self.handoff_type(ty).as_pointer_value();
            let visit = self.rt.handoff_visit;
            self.builder
                .build_call(visit, &[walk.into(), value.into(), described.into()], "")
                .expect("visiting a field");
            return;
        }
        if !self.unboxed.holds(ty) || !self.owns_a_reference(ty) {
            return;
        }
        let whole = value.into_struct_value();
        let payloads = self.unboxed.payloads(ty).unwrap_or_default();
        // Only the fields this value's own tag names, for the reason
        // `inline.rs` gives: the slots are shared between cases.
        if self.cases_of(ty) > 1 {
            let function = self
                .builder
                .get_insert_block()
                .and_then(|b| b.get_parent())
                .expect("a function to build in");
            let done = self.ctx.append_basic_block(function, "inline.visited");
            let tag = self
                .builder
                .build_extract_value(whole, 0, "case")
                .expect("reading an inline tag")
                .into_int_value();
            let mut arms = Vec::new();
            let start = self.builder.get_insert_block().expect("a block");
            for (case, fields) in &payloads {
                if !fields.iter().any(|t| self.owns_a_reference(t)) {
                    continue;
                }
                let block = self.ctx.append_basic_block(function, &format!("inline.case.{case}"));
                arms.push((self.ctx.i32_type().const_int(u64::from(*case), false), block));
                self.builder.position_at_end(block);
                for (index, field) in fields.iter().enumerate() {
                    if self.owns_a_reference(field) {
                        let part = self.read_inline(whole, ty, index, field);
                        self.visit_held(walk, part, field);
                    }
                }
                self.builder.build_unconditional_branch(done).expect("leaving a case");
            }
            self.builder.position_at_end(start);
            self.builder.build_switch(tag, done, &arms).expect("switching on an inline tag");
            self.builder.position_at_end(done);
        } else {
            let fields = payloads.first().map(|(_, f)| f.clone()).unwrap_or_default();
            for (index, field) in fields.iter().enumerate() {
                if self.owns_a_reference(field) {
                    let part = self.read_inline(whole, ty, index, field);
                    self.visit_held(walk, part, field);
                }
            }
        }
    }
}
