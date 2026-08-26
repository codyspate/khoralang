//! The C symbols a library publishes.
//!
//! `docs/design/c-export.md`. An `pub extern fn` with a body gets a second
//! symbol under its bare Khora name, forwarding to the mangled one.
//!
//! # A wrapper rather than an alias
//!
//! The mangled function could simply be given the C name instead, and that
//! would be one symbol fewer. It is a wrapper because the two names are not
//! the same promise: `kh$…` is Khora's, may change with the mangling, and is
//! called by generated code that already knows the calling convention;
//! the bare name is the library's published ABI. A forwarding call costs a jump
//! the linker usually removes, and keeps the place where anything an export has
//! to do — should it ever need to — belongs.
//!
//! # What a wrapper does *not* do
//!
//! It does not start the runtime, because there is nothing to start: the heap
//! is lazy and `SINGLE_THREADED` defaults to the atomic answer. That default is
//! load-bearing here — see [`super::driver`] on why a `--lib` build must never
//! claim to be single-threaded.
//!
//! It also does not close a root region, which `main` does on the way out.
//! A region opened during an exported call and not closed by it outlives the
//! call, and there is no `main` to sweep up after. Regions are not reachable
//! from an exported signature today — a `Region` cannot cross the C ABI — so
//! nothing can currently do this; it is written down because the first thing
//! that could would do it silently.

use std::collections::HashMap;

use inkwell::module::Linkage;
use inkwell::types::BasicTypeEnum;
use inkwell::values::{BasicMetadataValueEnum, FunctionValue};
use inkwell::{AddressSpace, DLLStorageClass};
use khora_types::{can_raise, Signature, Type};
use text_size::TextRange;

use super::Backend;

impl<'ctx> Backend<'ctx> {
    /// Emits the C entry point for every `pub extern fn` with a body.
    ///
    /// `exports` maps the bare Khora name to the mangled symbol. Built by the
    /// driver, which is what knows the instances.
    pub(crate) fn emit_c_exports(&mut self, exports: &[(String, String)]) {
        // **Two functions cannot publish one symbol.** The C namespace is flat
        // and has no modules in it, so two `pub extern fn price` in
        // different modules are a collision the linker would resolve by
        // picking one — silently, and not necessarily the same one twice.
        let mut seen: HashMap<&str, &str> = HashMap::new();
        for (name, symbol) in exports {
            if let Some(first) = seen.insert(name, symbol) {
                self.error(
                    format!(
                        "two functions are exported to C as `{name}` — `{first}` and \
                         `{symbol}`. A C symbol has no module to be qualified by, so one \
                         of them has to be renamed"
                    ),
                    TextRange::empty(0.into()),
                );
                continue;
            }
            self.emit_one_export(name, symbol);
            self.c_exports.push(name.clone());
        }
    }

    fn emit_one_export(&mut self, name: &str, symbol: &str) {
        let Some(target) = self.definition(symbol) else { return };
        let Some(signature) = self.signature_of(symbol) else { return };

        // The same shape, because the wrapper *is* the same function seen from
        // the other side. `shaped(.., true)` is the foreign view: no evidence
        // parameters, which an exported signature cannot have anyway.
        let Some(ty) = self.shaped(&signature, true) else { return };
        if self.module.get_function(name).is_some() {
            self.error(
                format!(
                    "`{name}` is exported to C, but the program already has a symbol by \
                     that name — a runtime function or a C declaration. Rename the export"
                ),
                TextRange::empty(0.into()),
            );
            return;
        }
        let wrapper = self.module.add_function(name, ty, Some(Linkage::External));
        // **Windows exports nothing it was not told to.** External linkage is
        // enough on ELF and Mach-O, where a shared object's symbols are
        // visible by default; a DLL publishes only what carries `dllexport`.
        // Without this the library built, the header generated, the import
        // library was written, and `lld-link` said `undefined symbol: price`
        // to the first C program that tried to call it — the whole feature
        // present and none of it reachable. A no-op on the other two.
        wrapper.as_global_value().set_dll_storage_class(DLLStorageClass::Export);

        let entry = self.ctx.append_basic_block(wrapper, "entry");
        self.builder.position_at_end(entry);
        // No debug location: this function has no Khora source. `Debug::leave`
        // has already run for the last body, and setting one here would point
        // a backtrace at a line belonging to something else.
        self.builder.unset_current_debug_location();

        // **Two paths, chosen at run time.** Containment is opt-in and rare,
        // and this sits on the hot path of every exported call — so a host
        // that asked for nothing gets a load, a predictable branch and a
        // direct call, and only one that opted in pays for a struct on the
        // stack and an indirect call through `khora_export_call`.
        let asked = self
            .builder
            .build_call(self.rt.contain_enabled, &[], "contain")
            .expect("asking whether containment is on")
            .try_as_basic_value()
            .basic()
            .expect("a flag")
            .into_int_value();
        let wants = self
            .builder
            .build_int_compare(
                inkwell::IntPredicate::NE,
                asked,
                self.ctx.i32_type().const_zero(),
                "wants",
            )
            .expect("testing the flag");
        let guarded = self.ctx.append_basic_block(wrapper, "guarded");
        let direct = self.ctx.append_basic_block(wrapper, "direct");
        self.builder.build_conditional_branch(wants, guarded, direct).expect("branching");

        self.builder.position_at_end(direct);
        let args: Vec<BasicMetadataValueEnum<'ctx>> =
            wrapper.get_param_iter().map(|v| v.into()).collect();
        let call = self.builder.build_call(target, &args, "forward").expect("forwarding an export");
        let returns_value = can_raise(&signature) || signature.ret != Type::Unit;
        match call.try_as_basic_value().basic().filter(|_| returns_value) {
            Some(value) => {
                self.builder.build_return(Some(&value)).expect("returning from an export");
            }
            None => {
                self.builder.build_return(None).expect("returning from an export");
            }
        }

        self.builder.position_at_end(guarded);
        self.emit_guarded_path(wrapper, target, name, &signature);
    }

    /// The path an export takes when the host asked for containment.
    ///
    /// The arguments go into a struct on this frame, a generated thunk reads
    /// them back and calls the real function, and `khora_export_call` runs the
    /// thunk under a landing point. The indirection exists because a `jmp_buf`
    /// belongs to the frame that owns it — `csrc/guard.c` — so the frame that
    /// calls the body has to be the C one, and one C function cannot be
    /// written per Khora signature.
    fn emit_guarded_path(
        &mut self,
        wrapper: FunctionValue<'ctx>,
        target: FunctionValue<'ctx>,
        name: &str,
        signature: &Signature,
    ) {
        let field_types: Vec<BasicTypeEnum<'ctx>> =
            wrapper.get_param_iter().map(|v| v.get_type()).collect();
        let ctx_ty = self.ctx.struct_type(&field_types, false);

        let slot = self.builder.build_alloca(ctx_ty, "args").expect("a frame for the arguments");
        for (i, value) in wrapper.get_param_iter().enumerate() {
            let field = self
                .builder
                .build_struct_gep(ctx_ty, slot, i as u32, "arg")
                .expect("addressing an argument");
            self.builder.build_store(field, value).expect("storing an argument");
        }

        let thunk = self.emit_export_thunk(target, name, ctx_ty, signature);
        let raw = self
            .builder
            .build_call(
                self.rt.export_call,
                &[thunk.as_global_value().as_pointer_value().into(), slot.into()],
                "guarded",
            )
            .expect("calling through the guard")
            .try_as_basic_value()
            .basic()
            .expect("a word")
            .into_int_value();

        // **Zero on the discarded path**, which the word already is, so the
        // conversion back is the same either way. A host that ignores
        // `khora_trapped()` gets a zero rather than a plausible answer, which
        // is the least bad of the choices C leaves: there is no third outcome
        // for a function that returns an integer.
        if matches!(signature.ret, Type::Unit) && !can_raise(signature) {
            self.builder.build_return(None).expect("returning from an export");
        } else {
            let value = self.word_to_value(raw, &signature.ret);
            self.builder.build_return(Some(&value)).expect("returning from an export");
        }
    }

    /// `uint64_t kh$export$<name>(void *ctx)` — unpacks the struct and calls.
    fn emit_export_thunk(
        &mut self,
        target: FunctionValue<'ctx>,
        name: &str,
        ctx_ty: inkwell::types::StructType<'ctx>,
        signature: &Signature,
    ) -> FunctionValue<'ctx> {
        let ptr = self.ctx.ptr_type(AddressSpace::default());
        let i64t = self.ctx.i64_type();
        let thunk = self.module.add_function(
            &format!("kh$export${name}"),
            i64t.fn_type(&[ptr.into()], false),
            Some(Linkage::Internal),
        );

        let here = self.builder.get_insert_block().expect("a block to come back to");
        let entry = self.ctx.append_basic_block(thunk, "entry");
        self.builder.position_at_end(entry);

        let slot = thunk.get_nth_param(0).expect("the context").into_pointer_value();
        let mut args: Vec<BasicMetadataValueEnum<'ctx>> = Vec::new();
        for i in 0..ctx_ty.count_fields() {
            let field = self
                .builder
                .build_struct_gep(ctx_ty, slot, i, "arg")
                .expect("addressing an argument");
            let ty = ctx_ty.get_field_type_at_index(i).expect("a field type");
            args.push(self.builder.build_load(ty, field, "a").expect("loading an argument").into());
        }
        let call = self.builder.build_call(target, &args, "inner").expect("calling the export");
        let returns_value = can_raise(signature) || signature.ret != Type::Unit;
        let word = match call.try_as_basic_value().basic().filter(|_| returns_value) {
            Some(value) => self.to_word(value),
            None => i64t.const_zero(),
        };
        self.builder.build_return(Some(&word)).expect("returning a word");

        self.builder.position_at_end(here);
        thunk
    }

}
