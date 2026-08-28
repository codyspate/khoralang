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
            // A closure is a heap object holding its function pointer and its
            // captures, so a value of function type is a pointer to one. `Ptr`
            // is a pointer that is only a pointer: no header, no count.
            // A tuple is an anonymous record: one heap object with its
            // elements as positional fields, so a value of tuple type is a
            // pointer to one exactly as a record is. Nothing else about it is
            // special — the same header, the same counting, the same generated
            // `drop_fields`.
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
            if is_boxed(&ty) {
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
            other => other.into_int_value(),
        }
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
            Type::Unit => self.ctx.void_type().fn_type(&params, false),
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
        self.types
            .variants
            .iter()
            .filter(|v| {
                v.type_name == type_name
                    && home.is_none_or(|wanted| v.home.as_ref() == Some(wanted))
            })
            .cloned()
            .collect()
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
        let variants = self.variants_in(home, type_name);
        let tag = variants.iter().position(|v| v.name == case)?;
        Some((tag as u32, variants[tag].clone()))
    }

    // -----------------------------------------------------------------------
    // Functions
    // -----------------------------------------------------------------------
}
