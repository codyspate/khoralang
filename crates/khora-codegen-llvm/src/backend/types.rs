//! What a Khora type is, once LLVM has to hold one.
//!
//! Machine words for the scalars, a pointer for everything counted, and the
//! `{ i32, i64 }` pair a fallible function returns. Also the error identity a
//! `catch` switches on: there is no static type at a release site, so the id
//! does the work at run time.

use super::*;

impl<'ctx> Backend<'ctx> {
    /// The machine representation of a Khora type.
    ///
    /// `Unit` is a word rather than nothing at all. Making it void would mean
    /// every expression's lowering returns an optional value and every consumer
    /// handles the absent case, to represent something no program can observe;
    /// an ignored `i64` costs one register the optimizer deletes. Functions
    /// returning `Unit` still return `void`, because that is a real ABI
    /// difference rather than an internal convenience.
    pub fn llvm_type(&self, ty: &Type) -> Option<BasicTypeEnum<'ctx>> {
        match ty {
            Type::Int | Type::Unit => Some(self.ctx.i64_type().into()),
            Type::Float => Some(self.ctx.f64_type().into()),
            // A `U8` is an `i8`, so an array of them is packed rather than one
            // byte per word. Signedness is not in the LLVM type — it is in the
            // instruction — so `U8` and `I8` share this and differ at every
            // `div`, `shr` and ordering comparison.
            Type::Fixed(kind) => Some(self.int_width(kind.bits.into()).into()),
            Type::Bool => Some(self.ctx.bool_type().into()),
            // **Thirty-two bits, like every other language's `char`.** A
            // Unicode scalar value needs twenty-one, and the two widths that
            // fit are 32 and a packed 24 nothing has instructions for. It is
            // an integer to LLVM, so equality and ordering come free and the
            // FFI boundary sees a `uint32_t`.
            Type::Char => Some(self.ctx.i32_type().into()),
            // A closure is a heap object holding its function pointer and its
            // captures, so a value of function type is a pointer to one. `Ptr`
            // is a pointer that is only a pointer: no header, no count.
            // A tuple is an anonymous record: one heap object with its
            // elements as positional fields, so a value of tuple type is a
            // pointer to one exactly as a record is. Nothing else about it is
            // special — the same header, the same counting, the same generated
            // `drop_fields`.
            // An unboxed ADT is the aggregate itself rather than a pointer to
            // one. Asked first, because everything below assumes a pointer.
            Type::Adt { .. } if self.unboxed.holds(ty) => {
                self.unboxed_type(ty).map(Into::into)
            }
            Type::Ptr | Type::Str | Type::Adt { .. } | Type::Fn { .. } | Type::Tuple(_) => {
                Some(self.ctx.ptr_type(AddressSpace::default()).into())
            }
            // A variable or a rigid parameter reaching code generation means
            // inference left something unsolved, or a generic function was not
            // monomorphized. Both are compiler bugs rather than user errors, so
            // there is no representation to pick here.
            Type::Var(_) | Type::Param(_) => None,
            // A projection reaching here never normalized, which means the
            // owner was never pinned down. That is a type error reported
            // elsewhere, not a shape the backend could pick.
            // A row is a compile-time description of what a function needs,
            // not a value: nothing is ever emitted holding one.
            Type::Const(_)
            | Type::Applied { .. }
            | Type::Assoc { .. }
            | Type::Row { .. } => None,
            Type::Never | Type::Unknown => None,
        }
    }

    /// The aggregate an unboxed value *is*.
    ///
    /// `{ [i32 tag,] field, .. }` — the tag only where there is something to
    /// discriminate. A record has one case, so one shape, so nothing to store:
    /// `Range` is `{ i64, i64 }` and not `{ i32, i64, i64 }`. That is a word
    /// off every record in the language.
    ///
    /// No header. That is the whole point: sixteen bytes of refcount, tag and
    /// width exist so that a *shared* object can be counted and taken apart,
    /// and an inline value is neither shared nor counted.
    pub fn unboxed_type(&self, ty: &Type) -> Option<inkwell::types::StructType<'ctx>> {
        let payload = self.unboxed.payload(ty)?;
        let mut parts: Vec<BasicTypeEnum<'ctx>> = Vec::with_capacity(payload.len() + 1);
        if self.cases_of(ty) > 1 {
            parts.push(self.ctx.i32_type().into());
        }
        for field in &payload {
            parts.push(self.llvm_type(field)?);
        }
        Some(self.ctx.struct_type(&parts, false))
    }

    /// How many cases a type declares, which decides whether it needs a tag.
    pub fn cases_of(&self, ty: &Type) -> usize {
        match ty {
            Type::Adt { name, home, .. } => self.variants_in(home.as_ref(), name).len(),
            _ => 0,
        }
    }

    /// Where a field sits in an unboxed value: after the tag, if there is one.
    pub fn unboxed_field_at(&self, ty: &Type, index: usize) -> u32 {
        (index + usize::from(self.cases_of(ty) > 1)) as u32
    }

    /// `{ i32 which, i64 payload }` — what a fallible function returns.
    ///
    /// `which` is 0 for an ordinary return and otherwise the error's type id,
    /// so one field carries both "did this raise" and "raise of what". A bare
    /// bit would answer the first question and leave `catch` unable to handle
    /// part of a row.
    pub fn tagged_type(&self) -> inkwell::types::StructType<'ctx> {
        self.ctx.struct_type(&[self.ctx.i32_type().into(), self.ctx.i64_type().into()], false)
    }

    /// A tag and a word put back together into what a fallible call returns.
    ///
    /// The inverse of `read_tagged`, and it exists because some fallible things
    /// are not calls. A `join` gets its two halves from the runtime through a
    /// return value and a stack slot -- an aggregate is the thing that cannot
    /// cross that boundary -- and then wants everything a fallible call gets:
    /// the branch, the unwinding, the release of this frame's bindings. Making
    /// the aggregate here is cheaper than a second copy of all of that.
    pub fn tagged_of(
        &self,
        which: inkwell::values::IntValue<'ctx>,
        word: inkwell::values::IntValue<'ctx>,
    ) -> BasicValueEnum<'ctx> {
        let empty = self.tagged_type().get_undef();
        let with_tag = self
            .builder
            .build_insert_value(empty, which, 0, "tagged.which")
            .expect("putting the tag in");
        self.builder
            .build_insert_value(with_tag, word, 1, "tagged")
            .expect("putting the payload in")
            .into_struct_value()
            .into()
    }

    /// Releases an error whose type is not known where it is caught.
    ///
    /// `catch { _ => .. }` handles the whole row, tail and all, so the arm has
    /// no static type to select drop glue from — and the row may be `'e`, which
    /// nothing at this point in the pipeline can enumerate either. Dropping the
    /// object with a null callback would free the object and leak every boxed
    /// field inside it, once per caught error, which on a server's failure path
    /// is a leak per request rather than a bounded one.
    ///
    /// So the dispatch is deferred to a function emitted once, at the end,
    /// when every error type in the program has an id: a `switch` on `which`
    /// whose cases each release the word as the type that id belongs to. The
    /// caller only has to know the id, which is the one thing it does know.
    ///
    /// [`Backend::emit_error_releaser`] is the definition.
    pub fn release_error(&mut self) -> FunctionValue<'ctx> {
        if let Some(existing) = self.error_releaser {
            return existing;
        }
        let signature = self.ctx.void_type().fn_type(
            &[self.ctx.i32_type().into(), self.ctx.i64_type().into()],
            false,
        );
        let function = self.module.add_function("khora.release_error", signature, None);
        self.error_releaser = Some(function);
        function
    }

    /// Defines the releaser, if anything asked for it.
    ///
    /// Emitted after every function and every lifted closure, because lowering
    /// is what assigns error ids and one more may be assigned by the last body
    /// compiled.
    pub fn emit_error_releaser(&mut self) {
        let Some(function) = self.error_releaser else { return };
        let entry = self.ctx.append_basic_block(function, "entry");
        let done = self.ctx.append_basic_block(function, "done");

        let which = function.get_nth_param(0).expect("which").into_int_value();
        let word = function.get_nth_param(1).expect("word").into_int_value();

        // By id, so the switch reads in the order the ids were handed out and
        // two compilations of the same program emit the same function.
        let mut known: Vec<(String, u32)> =
            self.error_ids.iter().map(|(n, i)| (n.clone(), *i)).collect();
        known.sort_by_key(|(_, id)| *id);

        let mut cases = Vec::with_capacity(known.len());
        for (name, id) in &known {
            let block = self.ctx.append_basic_block(function, &format!("release.{name}"));
            self.builder.position_at_end(block);
            let ty = Type::adt(name);
            if is_boxed(&ty, &self.unboxed) {
                let value = self.word_to_value(word, &ty);
                let glue = self.drop_glue(&ty);
                let drop = self.rt.drop;
                self.builder
                    .build_call(drop, &[value.into(), glue.into()], "")
                    .expect("releasing a caught error");
            }
            self.builder.build_unconditional_branch(done).expect("leaving a release case");
            cases.push((self.ctx.i32_type().const_int(u64::from(*id), false), block));
        }

        self.builder.position_at_end(entry);
        self.builder.build_switch(which, done, &cases).expect("dispatching on the error type");

        // Anything with no id owns nothing this function knows how to release.
        // A cancellation reaches here only if a caller passed one on purpose;
        // it carries no payload, so doing nothing is right.
        self.builder.position_at_end(done);
        self.builder.build_return(None).expect("returning from the releaser");
    }

    /// The id of an error type, assigning one if this is the first sight of it.
    ///
    /// Encounter order within a single whole-program module, which is
    /// deterministic for a given program and never crosses a module boundary —
    /// there is no separate compilation yet, and when there is, this becomes a
    /// link-time numbering rather than a lazy one.
    pub fn error_id(&mut self, name: &str) -> u32 {
        if let Some(id) = self.error_ids.get(name) {
            return *id;
        }
        let id = self.error_ids.len() as u32 + 1;
        self.error_ids.insert(name.to_string(), id);
        id
    }

    /// A value as the one word a tagged return carries it in.
    ///
    /// Every Khora value fits: an `Int` is already one, a `Bool` widens, a
    /// `Float` preserves its IEEE-754 bits, and everything boxed is a pointer.
    pub fn to_word(&self, value: BasicValueEnum<'ctx>) -> inkwell::values::IntValue<'ctx> {
        match value {
            BasicValueEnum::PointerValue(p) => self
                .builder
                .build_ptr_to_int(p, self.ctx.i64_type(), "word")
                .expect("a pointer as a word"),
            BasicValueEnum::IntValue(i) if i.get_type().get_bit_width() < 64 => self
                .builder
                .build_int_z_extend(i, self.ctx.i64_type(), "word")
                .expect("widening to a word"),
            BasicValueEnum::IntValue(i) => i,
            BasicValueEnum::FloatValue(f) => self
                .builder
                .build_bit_cast(f, self.ctx.i64_type(), "float.word")
                .expect("a float as a word")
                .into_int_value(),
            // **An inline value that will not fit in a word is boxed to
            // cross.** A tagged return, a handler's answer and the C boundary
            // all carry exactly one machine word, and an aggregate of two or
            // more is not one. `docs/roadmap.md` § Unboxed records anticipated
            // this for FFI and it is the same answer here: laid out flat where
            // it is used, put back in a box where it has to travel as a word.
            //
            // No worse than before, because these boundaries carried a pointer
            // to a heap object already. The value is spilled here and reloaded
            // by `word_to_value`, which frees the box.
            BasicValueEnum::StructValue(v) => self.spill_to_word(v),
            other => other.into_int_value(),
        }
    }

    /// Puts an inline value in a box so it can cross as one word.
    fn spill_to_word(&self, value: inkwell::values::StructValue<'ctx>) -> inkwell::values::IntValue<'ctx> {
        let fields = value.get_type().count_fields();
        let bytes = self.ctx.i64_type().const_int(u64::from(fields) * 8, false);
        let object = self
            .builder
            .build_call(self.rt.alloc, &[bytes.into(), self.ctx.i32_type().const_zero().into()], "spill")
            .expect("boxing an inline value to cross a word")
            .try_as_basic_value()
            .basic()
            .expect("khora_alloc returns a pointer")
            .into_pointer_value();
        for index in 0..fields {
            let field = self
                .builder
                .build_extract_value(value, index, "spill.field")
                .expect("reading an inline field");
            let slot = crate::runtime::field_pointer(self.ctx, &self.builder, object, u64::from(index));
            self.builder.build_store(slot, field).expect("spilling an inline field");
        }
        self.builder
            .build_ptr_to_int(object, self.ctx.i64_type(), "spill.word")
            .expect("a spilled value as a word")
    }

    /// Reads a spilled inline value back and frees the box it crossed in.
    fn reload_from_word(
        &self,
        word: inkwell::values::IntValue<'ctx>,
        shape: inkwell::types::StructType<'ctx>,
    ) -> BasicValueEnum<'ctx> {
        let ptr = self.ctx.ptr_type(AddressSpace::default());
        let object = self
            .builder
            .build_int_to_ptr(word, ptr, "spilled")
            .expect("a word as a spilled value");
        let mut value: inkwell::values::AggregateValueEnum<'ctx> = shape.get_undef().into();
        for index in 0..shape.count_fields() {
            let field_ty = shape.get_field_type_at_index(index).expect("a field");
            let slot = crate::runtime::field_pointer(self.ctx, &self.builder, object, u64::from(index));
            let read = self
                .builder
                .build_load(field_ty, slot, "reload.field")
                .expect("reading a spilled field");
            value = self
                .builder
                .build_insert_value(value, read, index, "reload")
                .expect("rebuilding an inline value");
        }
        // The box existed only to cross. Nothing else refers to it, and under
        // `Fields::Scalars` it holds nothing that needs releasing first.
        self.builder
            .build_call(
                self.rt.drop,
                &[object.into(), ptr.const_null().into()],
                "",
            )
            .expect("freeing the box a value crossed in");
        value.into_struct_value().into()
    }

    /// The inverse: a word read back as a value of `ty`.
    pub fn word_to_value(
        &self,
        word: inkwell::values::IntValue<'ctx>,
        ty: &Type,
    ) -> BasicValueEnum<'ctx> {
        match self.llvm_type(ty) {
            Some(BasicTypeEnum::PointerType(p)) => self
                .builder
                .build_int_to_ptr(word, p, "unword")
                .expect("a word as a pointer")
                .into(),
            Some(BasicTypeEnum::IntType(i)) if i.get_bit_width() < 64 => self
                .builder
                .build_int_truncate(word, i, "unword")
                .expect("narrowing from a word")
                .into(),
            Some(BasicTypeEnum::FloatType(f)) => self
                .builder
                .build_bit_cast(word, f, "word.float")
                .expect("a word as a float"),
            Some(BasicTypeEnum::StructType(shape)) => self.reload_from_word(word, shape),
            _ => word.into(),
        }
    }

    /// The zero value of a type: `null` for a pointer, `0` otherwise.
    ///
    /// Every local slot starts here. A boxed slot holding null is what makes an
    /// unconditional `drop` safe on a path where the binding was never reached
    /// — the runtime documents null tolerance for exactly this.
    pub fn zero_value(&self, ty: &Type) -> BasicValueEnum<'ctx> {
        match self.llvm_type(ty) {
            Some(BasicTypeEnum::PointerType(p)) => p.const_null().into(),
            Some(BasicTypeEnum::IntType(i)) => i.const_zero().into(),
            Some(BasicTypeEnum::FloatType(f)) => f.const_zero().into(),
            // An inline value's empty state is zero in every field. Nothing
            // reads it -- a slot is written before it is used -- but a slot
            // has to start somewhere and a struct cannot start as an `i64`.
            Some(BasicTypeEnum::StructType(s)) => s.const_zero().into(),
            _ => self.ctx.i64_type().const_zero().into(),
        }
    }

    /// The value standing for `()`.
    pub fn unit_value(&self) -> BasicValueEnum<'ctx> {
        self.ctx.i64_type().const_zero().into()
    }

    /// A null pointer, for a drop with no field routine.
    pub fn null_pointer(&self) -> PointerValue<'ctx> {
        self.ctx.ptr_type(AddressSpace::default()).const_null()
    }

    pub(super) fn function_type(&self, signature: &Signature) -> Option<FunctionType<'ctx>> {
        self.shaped(signature, false)
    }

    /// The machine type of a function, as a Khora definition or as a foreign
    /// declaration.
    ///
    /// The two differ in exactly one way, and it is the whole of decision 3 in
    /// `docs/design/ffi.md`: **a `with` clause on a foreign function is a
    /// permission, and nothing is appended to the call.** A C function has no
    /// use for a Khora record of closures, so passing one would be meaningless;
    /// but requiring it is how the boundary is governed, since nothing can open
    /// a file without holding `Fs` and `Fs` is not something a function can
    /// conjure.
    pub(super) fn shaped(&self, signature: &Signature, foreign: bool) -> Option<FunctionType<'ctx>> {
        let mut params: Vec<BasicMetadataTypeEnum<'ctx>> = Vec::new();
        for param in &signature.params {
            params.push(self.llvm_type(param)?.into());
        }
        // Capabilities are ordinary parameters, appended after the written
        // ones in label order. The row is sorted, so both sides agree without
        // anything being written down twice.
        if !foreign {
            for (_, capability) in evidence_of(signature) {
                params.push(self.llvm_type(&capability)?.into());
            }
        }
        // A function that can raise returns a tagged word instead of its
        // value: `{ i1 raised, i64 payload }`. One word suffices because every
        // Khora value is word-sized — the same fact `store_field` relies on —
        // and two fields come back in registers rather than through memory.
        //
        // No unwinder, no landing pads, no personality routine: a raise is a
        // return with a tag. `docs/design/effect-runtime.md` §2.
        if can_raise(signature) {
            return Some(self.tagged_type().fn_type(&params, false));
        }
        Some(match &signature.ret {
            // `Never` shapes like `()` and means something stronger: not "it
            // returns nothing" but "it does not return". LLVM has no type for
            // that -- divergence is a property of the call, marked `noreturn`
            // at the site -- so `void` is the honest shape and the type system
            // is what knows the difference. `khora_bounds_fail` is the first,
            // and every trap the runtime exports after it is the rest.
            Type::Unit | Type::Never => self.ctx.void_type().fn_type(&params, false),
            other => self.llvm_type(other)?.fn_type(&params, false),
        })
    }

    // -----------------------------------------------------------------------
    // ADTs
    // -----------------------------------------------------------------------

    /// The variants of an ADT, in declaration order, by name.
    ///
    /// A `home` of `None` asks by name alone, which is all a caller holding a
    /// bare spelling can do. Anything holding a [`Type`] should use
    /// [`Backend::variants_for`] instead: two modules may each declare a
    /// `Point`, and by name they are one. Errata 46.
    pub fn variants_in(
        &self,
        home: Option<&khora_hir::ModulePath>,
        type_name: &str,
    ) -> Vec<VariantInfo> {
        self.variants_named(home, type_name, None)
    }

    /// The variants of one type, never two.
    ///
    /// **A lookup with no home used to answer with every type of that name at
    /// once.** Two modules declaring a `Result` produced one list of
    /// `Ok`, `Err`, `Result` -- and since a variant's index in the list *is*
    /// its tag, the second module's record was built and matched under tag 2
    /// of a type that has one case. The failure was a field loaded at the
    /// wrong offset, so it surfaced as the backend asking a pointer of an
    /// integer, a long way from the declaration that caused it.
    ///
    /// Naming a `Pair` or a `Result` is an ordinary thing to do and it
    /// crashed the compiler; `docs/errata.md` 46 is the same bug where the
    /// home was recorded but not consulted.
    ///
    /// Where the home is known this is the filter it always was. Where it is
    /// not -- the compiler's own references to `Ordering` and friends, which
    /// name a type without saying whose -- the groups are kept apart and the
    /// one holding `case` wins, because a case name is the evidence available
    /// about which type was meant. Failing that, the first group, so the
    /// answer is one type's list either way.
    fn variants_named(
        &self,
        home: Option<&khora_hir::ModulePath>,
        type_name: &str,
        case: Option<&str>,
    ) -> Vec<VariantInfo> {
        let matching = self.types.variants.iter().filter(|v| v.type_name == type_name);
        if let Some(wanted) = home {
            return matching.filter(|v| v.home.as_ref() == Some(wanted)).cloned().collect();
        }

        let mut groups: Vec<(Option<&khora_hir::ModulePath>, Vec<VariantInfo>)> = Vec::new();
        for v in matching {
            let key = v.home.as_ref();
            match groups.iter_mut().find(|(h, _)| *h == key) {
                Some((_, group)) => group.push(v.clone()),
                None => groups.push((key, vec![v.clone()])),
            }
        }
        if let Some(case) = case {
            if let Some((_, group)) = groups.iter().find(|(_, g)| g.iter().any(|v| v.name == case))
            {
                return group.clone();
            }
        }
        groups.into_iter().next().map(|(_, g)| g).unwrap_or_default()
    }

    /// The variants of the declaration this type *is*, in declaration order.
    ///
    /// Order is the whole point: a variant's index in this list *is* its tag,
    /// which is what `match` switches on and what a constructor stores. It is
    /// declaration order because `khora_types::type_map` pushes variants as it
    /// reads them, and nothing between here and there sorts them.
    pub fn variants_for(&self, ty: &Type) -> Vec<VariantInfo> {
        match ty {
            Type::Adt { name, home, .. } => self.variants_in(home.as_ref(), name),
            _ => Vec::new(),
        }
    }

    /// A constructor's tag and fields, found by its type *and* its own name.
    ///
    /// The type is not optional. Case names repeat across a program, and a tag
    /// is an index within one type's variant list, so a lookup by bare name
    /// silently returns another type's tag — which is a `match` taking the
    /// wrong arm rather than a diagnostic.
    pub fn variant_of(&self, type_name: &str, case: &str) -> Option<(u32, VariantInfo)> {
        self.variant_in(None, type_name, case)
    }

    /// The same, told which declaration it means.
    ///
    /// A tag is an index into *one* type's variant list, so asking by name
    /// where two modules declare the same one returns the other type's tag —
    /// a `match` taking the wrong arm, or a record built to the wrong layout.
    pub fn variant_in(
        &self,
        home: Option<&khora_hir::ModulePath>,
        type_name: &str,
        case: &str,
    ) -> Option<(u32, VariantInfo)> {
        let variants = self.variants_named(home, type_name, Some(case));
        let tag = variants.iter().position(|v| v.name == case)?;
        Some((tag as u32, variants[tag].clone()))
    }

    // -----------------------------------------------------------------------
    // Functions
    // -----------------------------------------------------------------------
}
