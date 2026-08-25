//! The C symbols a library publishes.
//!
//! `docs/design/c-export.md`. An `export extern fn` with a body gets a second
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
use inkwell::values::BasicMetadataValueEnum;
use inkwell::DLLStorageClass;
use khora_types::{can_raise, Type};
use text_size::TextRange;

use super::Backend;

impl<'ctx> Backend<'ctx> {
    /// Emits the C entry point for every `export extern fn` with a body.
    ///
    /// `exports` maps the bare Khora name to the mangled symbol. Built by the
    /// driver, which is what knows the instances.
    pub(crate) fn emit_c_exports(&mut self, exports: &[(String, String)]) {
        // **Two functions cannot publish one symbol.** The C namespace is flat
        // and has no modules in it, so two `export extern fn price` in
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
    }
}
