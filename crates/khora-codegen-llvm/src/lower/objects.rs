//! Building things that live on the heap: records, tuples, ADTs, and fields.
//!
//! One allocation shape serves all three — a header and positional fields — so
//! a tuple is an anonymous record and a constructor is a record whose tag says
//! which case it is. `allocate_at` is also where a reuse token is spent, which
//! is the whole of FBIP at the call site. `docs/design/reuse.md` §2.

use super::*;

impl<'ctx> Lower<'_, 'ctx> {
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
    pub(super) fn assign_field(
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
        // By identity: a file that declares a `Point` and imports another
        // module's would otherwise write through whichever was recorded first.
        let Some(info) =
            self.be.variants_for(&owner_ty).into_iter().find(|v| v.name == type_name)
        else {
            return self.fail(format!("`{type_name}` is not a record"), range);
        };
        let Some((index, field_ty)) = info.field(label).map(|(i, t)| (i, t.clone())) else {
            return self.fail(format!("`{type_name}` has no field `{label}`"), range);
        };
        // Where the field *is* comes from the instantiation and not from the
        // declaration, for the reason `read_field` gives below: a parameter
        // has no width, so a record whose field turns out to be held inline
        // is written to at the offset it would have if it were a pointer.
        let laid_out = self.field_types(&owner_ty, &type_name).unwrap_or_else(|| info.fields.clone());
        let at = self.be.field_slot(&laid_out, index);

        let object = self.expr(base)?.into_pointer_value();
        let new = self.expr(value)?;

        let slot = runtime::field_pointer(self.be.ctx, &self.be.builder, object, at);
        if is_boxed(&field_ty, &self.be.unboxed) {
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

    /// Builds a tuple: the same object a record is, with positional fields.
    ///
    /// **A tuple is an anonymous record.** One heap object under the same
    /// header, counted and released the same way, with its elements as fields
    /// 0..n. Nothing in the reference-counting plan, the drop glue or the reuse
    /// analysis had to learn what a tuple is — `instantiated_variants` answers
    /// for one out of its type, and everything downstream asks that.
    ///
    /// Boxed rather than passed in registers, which is a real cost and the
    /// consistent choice: every other aggregate in the language is boxed, and
    /// `docs/design/compatibility.md` says when memory is allocated is not
    /// observable, so unboxing small ones later stays legal.
    pub(super) fn build_tuple(&mut self, id: ExprId, items: &[ExprId], range: TextRange) -> Flow<'ctx> {
        let ty = self.types.of(id).clone();
        let Some(info) = self.be.instantiated_variants(&ty).into_iter().next() else {
            return self.fail(format!("`{ty}` is not a tuple"), range);
        };

        // Evaluated before the allocation, as a constructor's arguments are: an
        // element can diverge, and an object allocated before that happens is
        // unreachable and unfreed.
        let mut values = Vec::with_capacity(items.len());
        for item in items {
            values.push(self.expr(*item)?);
        }

        let (at, words) = self.be.field_layout(&info.fields);
        let object = self.allocate_at(id, words, 0, "tuple");
        for (index, (value, field_ty)) in values.into_iter().zip(&info.fields).enumerate() {
            self.store_field(object, at[index], value, field_ty);
        }
        Some(object.into())
    }

    /// Builds a record: the same object a constructor builds, with the fields
    /// written in whatever order and stored in declaration order.
    pub(super) fn build_record(
        &mut self,
        id: ExprId,
        fields: &[(String, ExprId)],
        base: Option<ExprId>,
        range: TextRange,
    ) -> Flow<'ctx> {
        let Type::Adt { name, home, .. } = self.types.of(id).clone() else {
            return self.fail("this record has no type, which is a compiler bug", range);
        };
        let Some((tag, info)) = self.be.variant_in(home.as_ref(), &name, &name) else {
            return self.fail(format!("`{name}` is not a record"), range);
        };

        // An inline record is built in registers, base and all.
        let built = self.types.of(id).clone();
        if self.be.unboxed.holds(&built) {
            return self.build_record_inline(&built, &info, fields, base, range);
        }

        // **The base first, because it is written first and can diverge.**
        // `{ ..old, x: 1 }` evaluates `old` before `1`, which is the order the
        // reader sees.
        let taken_from = match base {
            Some(base) => Some((self.expr(base)?.into_pointer_value(), base)),
            None => None,
        };

        // Evaluated in written order, so side effects happen where they read,
        // and stored by label, so the order written does not matter.
        let mut values = Vec::with_capacity(fields.len());
        for (label, value) in fields {
            values.push((label.clone(), self.expr(*value)?));
        }

        // Sized and indexed from the fields *at this instantiation*: a
        // generic record's declared field is a parameter, which has no width,
        // and `Pair<Decimal, Int>` needs four slots rather than two.
        let laid_out = self.field_types(&built, &name).unwrap_or_else(|| info.fields.clone());
        let (at, words) = self.be.field_layout(&laid_out);
        let object = self.allocate_at(id, words, tag, &name);

        // **Every field the literal did not name comes from the base**, and
        // comes as an owned reference: the new record holds it too, so a
        // boxed one is retained. The base is released afterwards, which for a
        // base whose last use this is means the whole thing costs one
        // allocation and a handful of increments.
        if let Some((from, base)) = taken_from {
            for label in info.labels.clone() {
                if fields.iter().any(|(written, _)| *written == label) {
                    continue;
                }
                let Some((index, field_ty)) = info.field(&label).map(|(i, t)| (i, t.clone()))
                else {
                    continue;
                };
                let carried = self.load_field(from, at[index], &field_ty);
                if is_boxed(&field_ty, &self.be.unboxed) {
                    self.dup(carried);
                }
                self.store_field(object, at[index], carried, &field_ty);
            }
            let owner = self.types.of(base).clone();
            self.drop(from.into(), &owner);
        }

        for (label, value) in values {
            let Some((index, field_ty)) = info.field(&label).map(|(i, t)| (i, t.clone())) else {
                continue;
            };
            // Moved in, as a constructor's arguments are: the record owns it
            // now and its drop glue is what releases it.
            self.store_field(object, at[index], value, &field_ty);
        }
        Some(object.into())
    }

    /// `p.x` — a load from the field's slot.
    pub(super) fn read_field(&mut self, base: ExprId, label: &str, range: TextRange) -> Flow<'ctx> {
        let owner = self.types.of(base).clone();
        let Type::Adt { name, .. } = &owner else {
            return self.fail(format!("`{owner}` has no fields"), range);
        };
        // At *this* instantiation, not as declared. A generic record's field
        // is a parameter, and a parameter is never boxed, so reading the
        // declaration loads a `Pair<Int, String>`'s `value` as an integer and
        // hands a pointer-shaped hole to whatever wanted the string.
        let Some(info) = self
            .be
            .instantiated_variants(&owner)
            .into_iter()
            .find(|v| v.name == *name)
        else {
            return self.fail(format!("`{name}` is not a record"), range);
        };
        let Some((index, field_ty)) = info.field(label).map(|(i, t)| (i, t.clone())) else {
            return self.fail(format!("`{name}` has no field `{label}`"), range);
        };

        // An inline value has no memory to load from: the field is already in
        // a register beside the others.
        if self.be.unboxed.holds(&owner) {
            let whole = self.expr(base)?;
            let at = self.be.unboxed_field_at(&owner, index);
            let read = self
                .be
                .builder
                .build_extract_value(whole.into_struct_value(), at, "inline.read")
                .expect("reading an inline field");
            return Some(read);
        }

        let object = self.expr(base)?.into_pointer_value();
        let at = self.be.field_slot(&info.fields, index);
        let value = self.load_field(object, at, &field_ty);
        // The field is borrowed out of the record, and the record was owned by
        // this expression, so reading one keeps the field alive past the
        // release of what held it.
        if is_boxed(&field_ty, &self.be.unboxed) {
            self.dup(value);
        }
        self.drop(object.into(), &owner);
        Some(value)
    }

    /// Builds an ADT: `khora_alloc(8 * fields, tag)` and one store per field.
    ///
    /// The arguments are evaluated before the allocation, not after. An
    /// argument can diverge — `Cons(x, return 0)` — and an object allocated
    /// before that happens is unreachable and unfreed.
    pub(super) fn construct(
        &mut self,
        site: ExprId,
        home: Option<&khora_hir::ModulePath>,
        owner: &str,
        case: &str,
        args: &[ExprId],
        range: TextRange,
    ) -> Flow<'ctx> {
        let Some((tag, info)) = self.be.variant_in(home, owner, case) else {
            return self.fail(format!("`{owner}::{case}` is not a constructor"), range);
        };
        if args.len() != info.fields.len() {
            return self.fail(
                format!("`{owner}::{case}` takes {} field(s)", info.fields.len()),
                range,
            );
        }

        // **An unboxed value is built, not allocated**, and that includes the
        // cases with no fields: `Step::Done` held inline is a tag in a
        // register, so it cannot be the shared object below.
        let built = self.types.of(site).clone();
        if self.be.unboxed.holds(&built) {
            return self.construct_inline(&built, tag, args, range);
        }

        // **A case with no fields is one object for the whole program.** It
        // carries nothing but its tag, so every `Option::None` in a program is
        // indistinguishable from every other and there is no reason for them to
        // be different addresses. Before this, `Option::None`, `List::Nil` and
        // every case of a C-like enum each cost an allocation, a pair of atomic
        // reference-count operations and a free — twenty-four bytes of heap for
        // a value that is a constant.
        //
        // The same trick, and the same reasoning, as a string literal: the
        // count starts enormous rather than at one so that `khora_dup` and
        // `khora_drop` need not know a static from anything else, and cannot
        // take it to zero.
        if info.fields.is_empty() {
            // **A static case cannot build in the cell it was promised, so it
            // gives the cell back.** A branch is one path to a constructor and
            // the token has to leave the frame on all of them; where the
            // constructor turns out to be a constant there is nothing to build
            // in, and the alternative to freeing here is the merge block
            // freeing memory the *other* branch has already reused and
            // returned.
            self.discard_token_at(site);
            return Some(self.be.static_variant(owner, case, tag).into());
        }

        let mut values = Vec::with_capacity(args.len());
        for arg in args {
            values.push(self.expr(*arg)?);
        }

        let laid_out = self.field_types(&built, case).unwrap_or_else(|| info.fields.clone());
        let (at, words) = self.be.field_layout(&laid_out);
        let object = self.allocate_at(site, words, tag, case);

        for (index, (value, field_ty)) in values.into_iter().zip(&info.fields).enumerate() {
            // A boxed argument is *moved* into the object: no dup here, and no
            // drop either. The object owns it now, and its `drop_fields` is
            // what eventually releases it.
            self.store_field(object, at[index], value, field_ty);
        }
        Some(object.into())
    }

    // -----------------------------------------------------------------------
    // Fields
    // -----------------------------------------------------------------------

    /// The field types of one case of `ty`, with this use's arguments put in.
    ///
    /// `None` where the type declares no such case, which is a caller that
    /// already reported something and should keep its declared list.
    pub(super) fn field_types(&mut self, ty: &Type, case: &str) -> Option<Vec<Type>> {
        self.be
            .instantiated_variants(ty)
            .into_iter()
            .find(|v| v.name == case)
            .map(|v| v.fields)
    }

    /// An object for the expression `site`, in reused memory where there is
    /// some.
    ///
    /// A `match` arm that reaches its constructor unconditionally released the
    /// scrutinee with `khora_drop_reuse` at its head, which handed back the
    /// cell if nobody else held it. Spending that token here is the whole of
    /// reuse: same memory, new tag, no allocator. `docs/design/reuse.md` §2.
    ///
    /// The token is matched by expression id rather than simply taken, because
    /// a constructor's *arguments* may contain constructors of their own and
    /// the arm promised this one in particular.
    pub(super) fn allocate_at(
        &mut self,
        site: ExprId,
        words: u64,
        tag: u32,
        name: &str,
    ) -> PointerValue<'ctx> {
        let Some(token) = self.take_reuse_token(site) else {
            return self.allocate(words, tag, name);
        };
        let alloc_reuse = self.be.rt.alloc_reuse;
        self.be
            .builder
            .build_call(
                alloc_reuse,
                &[
                    token.into(),
                    self.be.ctx.i64_type().const_int(FIELD_WORD * words, false).into(),
                    self.be.ctx.i32_type().const_int(tag as u64, false).into(),
                ],
                &format!("{name}.reused"),
            )
            .expect("reusing an object")
            .try_as_basic_value()
            .basic()
            .expect("khora_alloc_reuse returns a pointer")
            .into_pointer_value()
    }

    /// A record literal, held inline.
    ///
    /// `{ ..old, x: 1 }` reads the fields it does not name straight out of the
    /// base's registers rather than out of a heap object, and there is nothing
    /// to release afterwards -- the base was a value, not a reference to one.
    /// Evaluation order is unchanged: the base first, because it is written
    /// first and can diverge, then the fields as written.
    fn build_record_inline(
        &mut self,
        ty: &Type,
        info: &VariantInfo,
        fields: &[(String, ExprId)],
        base: Option<ExprId>,
        range: TextRange,
    ) -> Flow<'ctx> {
        let Some(shape) = self.be.unboxed_type(ty) else {
            return self.fail(format!("`{ty}` has no inline layout"), range);
        };
        let taken_from = match base {
            Some(base) => Some(self.expr(base)?.into_struct_value()),
            None => None,
        };
        let mut written = Vec::with_capacity(fields.len());
        for (label, value) in fields {
            written.push((label.clone(), self.expr(*value)?));
        }

        let mut value: inkwell::values::AggregateValueEnum<'ctx> = shape.get_undef().into();
        for (index, label) in info.labels.iter().enumerate() {
            let at = self.be.unboxed_field_at(ty, index);
            let field = match written.iter().find(|(w, _)| w == label) {
                Some((_, v)) => *v,
                None => match taken_from {
                    Some(from) => self
                        .be
                        .builder
                        .build_extract_value(from, at, "inline.carried")
                        .expect("carrying a field from the base"),
                    // The checker refuses a literal that names neither every
                    // field nor a base, so there is nothing to read here.
                    None => return self.fail(format!("`{label}` was not given"), range),
                },
            };
            value = self
                .be
                .builder
                .build_insert_value(value, field, at, "inline.field")
                .expect("writing an inline field");
        }
        Some(value.into_struct_value().into())
    }

    /// Builds an unboxed value in registers.
    ///
    /// No allocation, no header, no reference count -- the value *is* the
    /// aggregate. A case that carries nothing leaves the payload undefined,
    /// which nothing reads: the tag says which case it is, and every arm that
    /// looks at a field has tested the tag first.
    fn construct_inline(
        &mut self,
        ty: &Type,
        tag: u32,
        args: &[ExprId],
        range: TextRange,
    ) -> Flow<'ctx> {
        let Some(shape) = self.be.unboxed_type(ty) else {
            return self.fail(format!("`{ty}` has no inline layout"), range);
        };
        let mut value: inkwell::values::AggregateValueEnum<'ctx> = shape.get_undef().into();
        if self.be.cases_of(ty) > 1 {
            let which = self.be.ctx.i32_type().const_int(u64::from(tag), false);
            value = self
                .be
                .builder
                .build_insert_value(value, which, 0, "case")
                .expect("writing an inline tag");
        }
        for (index, arg) in args.iter().enumerate() {
            let field = self.expr(*arg)?;
            let at = self.be.unboxed_field_at(ty, index);
            value = self
                .be
                .builder
                .build_insert_value(value, field, at, "inline.field")
                .expect("writing an inline field");
        }
        Some(value.into_struct_value().into())
    }

    /// Hands over the reuse token if it was promised to this expression.
    ///
    /// Cleared once spent, so a second constructor further along the same path
    /// allocates rather than building where the first one already did. An arm
    /// that branches promises the token to a site in each branch, and the
    /// branch lowering puts it back between them: one path, one spend.
    pub(super) fn take_reuse_token(&mut self, site: ExprId) -> Option<PointerValue<'ctx>> {
        match &self.reuse {
            Some((promised, token)) if promised.contains(&site) => {
                let token = *token;
                self.reuse = None;
                Some(token)
            }
            _ => None,
        }
    }

    /// Gives back a token this site was promised but cannot spend.
    ///
    /// Only when it was promised to *this* site: an ordinary constant case
    /// somewhere inside an arm has nothing to do with the token, and freeing
    /// it there would take the cell out from under the constructor that is
    /// going to build in it.
    pub(super) fn discard_token_at(&mut self, site: ExprId) {
        let promised = matches!(&self.reuse, Some((sites, _)) if sites.contains(&site));
        if promised {
            self.discard_unspent_token();
        }
    }

    /// The token as it stands, to be put back before the next branch.
    pub(super) fn held_reuse_token(&self) -> Option<(Vec<ExprId>, PointerValue<'ctx>)> {
        self.reuse.clone()
    }

    /// Puts back what [`Self::held_reuse_token`] took a copy of.
    ///
    /// **Not a second token.** The same runtime value, offered again to a
    /// branch the lowering has not walked yet -- only one of them runs, so
    /// only one of them spends it.
    pub(super) fn restore_reuse_token(
        &mut self,
        held: Option<(Vec<ExprId>, PointerValue<'ctx>)>,
    ) {
        self.reuse = held;
    }

    /// A fresh heap object with room for `fields` words, under `tag`.
    pub(super) fn allocate(&mut self, words: u64, tag: u32, name: &str) -> PointerValue<'ctx> {
        let alloc = self.be.rt.alloc;
        self.be
            .builder
            .build_call(
                alloc,
                &[
                    self.be.ctx.i64_type().const_int(FIELD_WORD * words, false).into(),
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
    /// `at` is a *word* offset into the field area and not a field index: a
    /// value held inline occupies its whole width there, so the two stop
    /// agreeing the moment anything is unboxed. [`Backend::field_layout`] is
    /// where the offsets come from, and every reader and writer of an object
    /// asks it rather than counting fields.
    pub(super) fn store_field(
        &mut self,
        object: PointerValue<'ctx>,
        at: u64,
        value: BasicValueEnum<'ctx>,
        ty: &Type,
    ) {
        let slot = runtime::field_pointer(self.be.ctx, &self.be.builder, object, at);
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

    /// Reads a field from the word offset [`Self::store_field`] wrote it to.
    pub(super) fn load_field(
        &mut self,
        object: PointerValue<'ctx>,
        at: u64,
        ty: &Type,
    ) -> BasicValueEnum<'ctx> {
        let slot = runtime::field_pointer(self.be.ctx, &self.be.builder, object, at);
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
}
