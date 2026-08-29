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

        let object = self.allocate_at(id, info.fields.len(), 0, "tuple");
        for (index, (value, field_ty)) in values.into_iter().zip(&info.fields).enumerate() {
            self.store_field(object, index, value, field_ty);
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

        let object = self.allocate_at(id, info.fields.len(), tag, &name);

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
                let carried = self.load_field(from, index, &field_ty);
                if is_boxed(&field_ty) {
                    self.dup(carried);
                }
                self.store_field(object, index, carried, &field_ty);
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
            self.store_field(object, index, value, &field_ty);
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
            return Some(self.be.static_variant(owner, case, tag).into());
        }

        let mut values = Vec::with_capacity(args.len());
        for arg in args {
            values.push(self.expr(*arg)?);
        }

        let object = self.allocate_at(site, info.fields.len(), tag, case);

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
        fields: usize,
        tag: u32,
        name: &str,
    ) -> PointerValue<'ctx> {
        let Some(token) = self.take_reuse_token(site) else {
            return self.allocate(fields, tag, name);
        };
        let alloc_reuse = self.be.rt.alloc_reuse;
        self.be
            .builder
            .build_call(
                alloc_reuse,
                &[
                    token.into(),
                    self.be.ctx.i64_type().const_int(FIELD_WORD * fields as u64, false).into(),
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

    /// Hands over the reuse token if it was promised to this expression.
    pub(super) fn take_reuse_token(&mut self, site: ExprId) -> Option<PointerValue<'ctx>> {
        match self.reuse {
            Some((promised, token)) if promised == site => {
                self.reuse = None;
                Some(token)
            }
            _ => None,
        }
    }

    /// A fresh heap object with room for `fields` words, under `tag`.
    pub(super) fn allocate(&mut self, fields: usize, tag: u32, name: &str) -> PointerValue<'ctx> {
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
    pub(super) fn store_field(
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

    pub(super) fn load_field(
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
}
