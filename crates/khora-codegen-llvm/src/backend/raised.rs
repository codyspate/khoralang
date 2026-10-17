//! `Raised<'er>`: a failure, held as a value.
//!
//! **The tagged return, put in a box.** A fallible call already comes back as
//! `{ which, word }`: which error type, and the one word that carries it.
//! A `Raised` is that pair in a three-word object, with a third word for the
//! context a program attached (`String`, or null for none). Nothing about the
//! error changes on the way in or out, so a failure that went through a
//! `Raised` is the same failure, of the same type, to every `catch` after it.
//!
//! **Context travels in the box.** `raise f` of a `Raised` that holds a note
//! leaves under [`crate::runtime::NOTED_WHICH`] with the box as its word, so
//! the note reaches whatever handles the failure next. Every place that takes
//! a failure apart opens the box first. A program that never attaches context
//! never makes a noted failure, and [`Backend::noted`] keeps it from paying
//! for the opening: the flag is decided before any body is lowered.
//!
//! **What it costs.** One allocation per failure that becomes a value:
//! `attempt`, or a binding arm over more than one type. A `catch` that names
//! its cases, or binds one type, allocates nothing.

use super::*;

/// The functions a `Raised` is built on, declared on first use and defined
/// once every error type has its id.
#[derive(Default)]
pub(crate) struct RaisedFns<'ctx> {
    glue: Option<FunctionValue<'ctx>>,
    note_glue: Option<FunctionValue<'ctx>>,
    copy: Option<FunctionValue<'ctx>>,
    describe: Option<FunctionValue<'ctx>>,
    type_name: Option<FunctionValue<'ctx>>,
}

/// The word a `Raised` keeps its error's id in.
pub(crate) const RAISED_WHICH: u64 = 0;
/// The word a `Raised` keeps its error in.
pub(crate) const RAISED_WORD: u64 = 1;
/// The word a `Raised` keeps its context in: a `String`, or null for none.
pub(crate) const RAISED_NOTE: u64 = 2;
/// How many words a `Raised` is.
pub(crate) const RAISED_WORDS: u64 = 3;

impl<'ctx> Backend<'ctx> {
    /// Whether this program attaches context to a failure anywhere: calls
    /// `Raised::with_note`, which `context` is built on.
    ///
    /// **The one thing that decides whether failure paths open a box.** Only
    /// `raise` of a `Raised` with a note makes a [`runtime::NOTED_WHICH`]
    /// failure, and only `with_note` makes a note, so a program without it
    /// has none, and its `catch`es, `attempt`s and entry point are the code
    /// they were before `Raised` existed. Set once, before lowering, by
    /// [`Backend::find_noted`].
    pub(crate) fn noted(&self) -> bool {
        self.attaches_context
    }

    /// Looks through every body that will be emitted for a call that attaches
    /// context, and records the answer for [`Backend::noted`].
    ///
    /// A false positive costs a compare per handled failure; a false negative
    /// would hand a box to a `catch` that reads it as an error. So any call of
    /// a function named `with_note` through `Raised` counts, whether or not
    /// it is reached.
    pub(crate) fn find_noted(&mut self, bodies: &[&khora_hir::body::Body]) {
        self.attaches_context = bodies.iter().any(|body| {
            body.exprs().any(|(_, expr)| match expr {
                khora_hir::body::Expr::Path(khora_hir::Resolution::TraitItem { owner, name }) => {
                    owner == khora_types::RAISED_TYPE && name == "with_note"
                }
                khora_hir::body::Expr::Field { name, .. } => name == "with_note",
                _ => false,
            })
        });
    }

    /// How many of the failure routines have been asked for, and how much
    /// drop glue is waiting: what the driver's last round watches for
    /// growth, so a routine first asked for by another's body is emitted too.
    pub(crate) fn failure_routines_asked(&self) -> usize {
        let raised = [self.raised.glue, self.raised.note_glue, self.raised.copy, self.raised.describe, self.raised.type_name];
        raised.iter().flatten().count()
            + usize::from(self.error_releaser.is_some())
            + usize::from(self.error_taker.is_some())
            + self.pending_glue.len()
    }

    /// The drop glue of a `Raised`: the error it holds, by its id, and the
    /// note.
    pub(crate) fn raised_glue(&mut self) -> PointerValue<'ctx> {
        if let Some(f) = self.raised.glue {
            return f.as_global_value().as_pointer_value();
        }
        let releaser = self.release_error();
        let ptr = self.ctx.ptr_type(AddressSpace::default());
        let f = self.module.add_function(
            "kh$drop_fields$Raised",
            self.ctx.void_type().fn_type(&[ptr.into()], false),
            Some(Linkage::Internal),
        );
        self.raised.glue = Some(f);

        // Emitted now rather than queued: the body names nothing whose id or
        // glue is still to be decided, only the releaser, which is declared.
        let saved = self.builder.get_insert_block();
        let entry = self.ctx.append_basic_block(f, "entry");
        self.builder.position_at_end(entry);
        let object = f.get_nth_param(0).expect("the object").into_pointer_value();
        let (which, word) = self.raised_pair(object);
        self.builder
            .build_call(releaser, &[which.into(), word.into()], "")
            .expect("releasing the held error");
        self.release_note(f, object);
        self.builder.build_return(None).expect("done");
        if let Some(block) = saved {
            self.builder.position_at_end(block);
        }
        f.as_global_value().as_pointer_value()
    }

    /// Drop glue that releases only a `Raised`'s note: for a box whose error
    /// has been taken out, so freeing the box must leave the error alone.
    pub(crate) fn raised_note_glue(&mut self) -> PointerValue<'ctx> {
        if let Some(f) = self.raised.note_glue {
            return f.as_global_value().as_pointer_value();
        }
        let ptr = self.ctx.ptr_type(AddressSpace::default());
        let f = self.module.add_function(
            "kh$drop_note$Raised",
            self.ctx.void_type().fn_type(&[ptr.into()], false),
            Some(Linkage::Internal),
        );
        self.raised.note_glue = Some(f);
        let saved = self.builder.get_insert_block();
        let entry = self.ctx.append_basic_block(f, "entry");
        self.builder.position_at_end(entry);
        let object = f.get_nth_param(0).expect("the object").into_pointer_value();
        self.release_note(f, object);
        self.builder.build_return(None).expect("done");
        if let Some(block) = saved {
            self.builder.position_at_end(block);
        }
        f.as_global_value().as_pointer_value()
    }

    /// Releases a `Raised`'s note, if it has one, from inside glue `f`.
    ///
    /// **A null note is not handed to `khora_drop`.** The runtime would
    /// return at once, but the call is still a call, on every failure that
    /// carries no context -- most of them.
    fn release_note(&mut self, f: FunctionValue<'ctx>, object: PointerValue<'ctx>) {
        let ptr = self.ctx.ptr_type(AddressSpace::default());
        let slot = crate::runtime::field_pointer(self.ctx, &self.builder, object, RAISED_NOTE);
        let note = self.builder.build_load(ptr, slot, "note").expect("reading the note").into_pointer_value();
        let none = self.builder.build_is_null(note, "note.none").expect("testing the note");
        let release = self.ctx.append_basic_block(f, "note.release");
        let done = self.ctx.append_basic_block(f, "note.done");
        self.builder.build_conditional_branch(none, done, release).expect("on the note");
        self.builder.position_at_end(release);
        self.builder
            .build_call(self.rt.drop, &[note.into(), ptr.const_null().into()], "")
            .expect("releasing the note");
        self.builder.build_unconditional_branch(done).expect("on");
        self.builder.position_at_end(done);
    }

    /// A `Raised`'s id and word, borrowed from the box.
    pub(crate) fn raised_pair(
        &self,
        object: PointerValue<'ctx>,
    ) -> (inkwell::values::IntValue<'ctx>, inkwell::values::IntValue<'ctx>) {
        let i64_type = self.ctx.i64_type();
        let slot = crate::runtime::field_pointer(self.ctx, &self.builder, object, RAISED_WHICH);
        let which = self
            .builder
            .build_load(i64_type, slot, "raised.which")
            .expect("reading the id")
            .into_int_value();
        let which = self
            .builder
            .build_int_truncate(which, self.ctx.i32_type(), "raised.which.32")
            .expect("narrowing the id");
        let slot = crate::runtime::field_pointer(self.ctx, &self.builder, object, RAISED_WORD);
        let word = self
            .builder
            .build_load(i64_type, slot, "raised.word")
            .expect("reading the word")
            .into_int_value();
        (which, word)
    }

    /// `khora.copy_error(which, word) -> word`: a reference of the caller's
    /// own to the error `word` carries, which the caller then owns.
    ///
    /// For a `Raised` that somebody else also holds: taking its error out
    /// would take it from them too.
    pub(crate) fn copy_error(&mut self) -> FunctionValue<'ctx> {
        if let Some(f) = self.raised.copy {
            return f;
        }
        let i64_type = self.ctx.i64_type();
        let signature = i64_type.fn_type(&[self.ctx.i32_type().into(), i64_type.into()], false);
        let f = self.module.add_function("khora.copy_error", signature, Some(Linkage::Internal));
        self.raised.copy = Some(f);
        // The taker gives an inline error with counted fields a box of its
        // own; declared now so that it is emitted with the rest.
        let _ = self.take_error();
        f
    }

    /// `khora.describe_error(which, word) -> { which, String }`: the error,
    /// shown by its own type's `Show`, or its type's name for one without.
    ///
    /// **The tag comes back with the text.** A `Show` that can stop answers
    /// `{ which, String }`, and a cancellation inside it has no text: the
    /// caller leaves on the tag before it reads the half that is null.
    pub(crate) fn describe_error(&mut self) -> FunctionValue<'ctx> {
        if let Some(f) = self.raised.describe {
            return f;
        }
        let signature =
            self.described_type().fn_type(&[self.ctx.i32_type().into(), self.ctx.i64_type().into()], false);
        let f = self.module.add_function("khora.describe_error", signature, Some(Linkage::Internal));
        self.raised.describe = Some(f);
        let _ = self.copy_error();
        let _ = self.error_type_name();
        f
    }

    /// `khora.error_type_name(which) -> String`: the declared name, as a
    /// static.
    pub(crate) fn error_type_name(&mut self) -> FunctionValue<'ctx> {
        if let Some(f) = self.raised.type_name {
            return f;
        }
        let ptr = self.ctx.ptr_type(AddressSpace::default());
        let signature = ptr.fn_type(&[self.ctx.i32_type().into()], false);
        let f = self.module.add_function("khora.error_type_name", signature, Some(Linkage::Internal));
        self.raised.type_name = Some(f);
        f
    }

    /// Defines whichever of these were asked for and are still empty. After
    /// every body, so every error type has its id; safe to call again.
    pub(crate) fn emit_raised_fns(&mut self) {
        let known = self.known_errors();
        if let Some(f) = self.raised.copy.filter(|f| f.count_basic_blocks() == 0) {
            self.emit_copy_error(f, &known);
        }
        if let Some(f) = self.raised.describe.filter(|f| f.count_basic_blocks() == 0) {
            self.emit_describe_error(f, &known);
        }
        if let Some(f) = self.raised.type_name.filter(|f| f.count_basic_blocks() == 0) {
            self.emit_error_type_name(f, &known);
        }
    }

    fn emit_error_type_name(&mut self, f: FunctionValue<'ctx>, known: &[(Type, u32)]) {
        let entry = self.ctx.append_basic_block(f, "entry");
        let fallback = self.ctx.append_basic_block(f, "unknown");
        let which = f.get_nth_param(0).expect("which").into_int_value();
        let mut cases = Vec::new();
        for (ty, id) in known {
            let block = self.ctx.append_basic_block(f, &format!("name.{ty}"));
            self.builder.position_at_end(block);
            let name = match ty {
                Type::Adt { name, .. } => name.clone(),
                other => other.to_string(),
            };
            let text = self.static_string(&name);
            self.builder.build_return(Some(&text)).expect("answering a name");
            cases.push((self.ctx.i32_type().const_int(u64::from(*id), false), block));
        }
        self.builder.position_at_end(fallback);
        let text = self.static_string("?");
        self.builder.build_return(Some(&text)).expect("answering a name");
        self.builder.position_at_end(entry);
        self.builder.build_switch(which, fallback, &cases).expect("dispatching on the id");
    }

    fn emit_copy_error(&mut self, f: FunctionValue<'ctx>, known: &[(Type, u32)]) {
        let entry = self.ctx.append_basic_block(f, "entry");
        let done = self.ctx.append_basic_block(f, "done");
        let which = f.get_nth_param(0).expect("which").into_int_value();
        let word = f.get_nth_param(1).expect("word").into_int_value();
        let ptr = self.ctx.ptr_type(AddressSpace::default());
        let mut cases = Vec::new();
        for (ty, id) in known {
            // Only an error that travels as a pointer -- boxed, or inline and
            // crossing in a spill box -- has a count to take.
            let travels_as_pointer = matches!(
                self.llvm_type(ty),
                Some(BasicTypeEnum::PointerType(_) | BasicTypeEnum::StructType(_))
            );
            if !travels_as_pointer {
                continue;
            }
            let block = self.ctx.append_basic_block(f, &format!("copy.{ty}"));
            self.builder.position_at_end(block);
            let object = self.builder.build_int_to_ptr(word, ptr, "held").expect("a word as a box");
            self.builder.build_call(self.rt.dup, &[object.into()], "").expect("counting a copy");
            self.builder.build_unconditional_branch(done).expect("on");
            cases.push((self.ctx.i32_type().const_int(u64::from(*id), false), block));
        }
        self.builder.position_at_end(entry);
        self.builder.build_switch(which, done, &cases).expect("dispatching on the id");
        // An inline error with counted fields is then copied out of the box
        // the reference was just taken to, as a second joiner of a failed
        // fiber copies it: the taker spends that reference.
        self.builder.position_at_end(done);
        let taker = self.take_error();
        let taken = self
            .builder
            .build_call(taker, &[which.into(), word.into()], "taken")
            .expect("taking a copy")
            .try_as_basic_value()
            .basic()
            .expect("a word");
        self.builder.build_return(Some(&taken)).expect("the copy");
    }

    fn emit_describe_error(&mut self, f: FunctionValue<'ctx>, known: &[(Type, u32)]) {
        let entry = self.ctx.append_basic_block(f, "entry");
        let fallback = self.ctx.append_basic_block(f, "unshown");
        let which = f.get_nth_param(0).expect("which").into_int_value();
        let word = f.get_nth_param(1).expect("word").into_int_value();
        let copier = self.copy_error();
        let namer = self.error_type_name();
        let mut cases = Vec::new();
        for (ty, id) in known {
            // **The impl the program already compiled.** A non-generic type's
            // `Show` is one concrete function; a generic type's at this
            // argument may not have been emitted, and is described by name.
            let Type::Adt { name, home: Some(home), args } = ty else { continue };
            if !args.is_empty() {
                continue;
            }
            let symbol = khora_types::mono::Instance {
                module: home.clone(),
                function: khora_types::traits::method_key("Show", name, "show"),
                args: Vec::new(),
            }
            .symbol();
            if !self.is_defined(&symbol) {
                continue;
            }
            let Ok(show) = self.callee(&symbol) else { continue };
            let block = self.ctx.append_basic_block(f, &format!("show.{ty}"));
            self.builder.position_at_end(block);
            // `show` owns its argument, so it is handed a copy.
            let copy = self
                .builder
                .build_call(copier, &[which.into(), word.into()], "copy")
                .expect("copying the error")
                .try_as_basic_value()
                .basic()
                .expect("a word")
                .into_int_value();
            let value = self.word_to_value(copy, ty);
            let answer = self
                .builder
                .build_call(show, &[value.into()], "shown")
                .expect("showing the error")
                .try_as_basic_value()
                .basic()
                .expect("a string");
            let pair = match answer {
                BasicValueEnum::StructValue(pair) => {
                    let tag = self.builder.build_extract_value(pair, 0, "show.which").expect("the tag");
                    let text = self.builder.build_extract_value(pair, 1, "text").expect("the answer half");
                    self.described(tag.into_int_value(), text)
                }
                other => {
                    let zero = self.ctx.i32_type().const_zero();
                    self.described(zero, other)
                }
            };
            self.builder.build_return(Some(&pair)).expect("the text");
            cases.push((self.ctx.i32_type().const_int(u64::from(*id), false), block));
        }
        self.builder.position_at_end(fallback);
        let name = self
            .builder
            .build_call(namer, &[which.into()], "name")
            .expect("naming the type")
            .try_as_basic_value()
            .basic()
            .expect("a string");
        let zero = self.ctx.i32_type().const_zero();
        let pair = self.described(zero, name);
        self.builder.build_return(Some(&pair)).expect("the name");
        self.builder.position_at_end(entry);
        self.builder.build_switch(which, fallback, &cases).expect("dispatching on the id");
    }

    /// The `{ which, String }` `describe_error` answers.
    fn described(
        &self,
        which: inkwell::values::IntValue<'ctx>,
        text: BasicValueEnum<'ctx>,
    ) -> inkwell::values::StructValue<'ctx> {
        let shape = self.described_type();
        let pair = self.builder.build_insert_value(shape.get_undef(), which, 0, "d.which").expect("the tag");
        self.builder.build_insert_value(pair, text, 1, "d.text").expect("the text").into_struct_value()
    }

    /// `{ i32, ptr }`.
    pub(crate) fn described_type(&self) -> inkwell::types::StructType<'ctx> {
        let ptr = self.ctx.ptr_type(AddressSpace::default());
        self.ctx.struct_type(&[self.ctx.i32_type().into(), ptr.into()], false)
    }
}
