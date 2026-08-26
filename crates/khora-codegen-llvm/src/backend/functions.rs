//! Declaring and finding the functions a program actually needs.
//!
//! One per *specialization* rather than per source function: a generic body has
//! no machine representation until its type arguments are known, and a generic
//! function nobody calls is never emitted. Everything is declared before
//! anything is lowered, because mutual recursion means no ordering exists in
//! which every callee is already defined.

use super::*;

impl<'ctx> Backend<'ctx> {
    /// The signature to compile `name` against.
    ///
    /// A specialization is registered under its mangled symbol with its type
    /// arguments already substituted, so the backend never sees a rigid
    /// parameter. Anything not registered is a plain function under its own
    /// name.
    pub fn signature_of(&self, name: &str) -> Option<khora_types::Signature> {
        self.instance_signatures
            .get(name)
            .cloned()
            .or_else(|| self.types.signatures.get(name).cloned())
    }

    /// The signature to call `name` through, given that nothing in Khora
    /// defines it.
    ///
    /// Such a name is a C symbol, so an `extern` declaration of it is the right
    /// answer even when a Khora function of the same name exists somewhere in
    /// the program — see [`Backend::foreign_signatures`].
    pub fn foreign_signature_of(&self, name: &str) -> Option<khora_types::Signature> {
        self.foreign_signatures.get(name).cloned().or_else(|| self.signature_of(name))
    }

    pub(super) fn register_foreign(&mut self, name: &str, signature: khora_types::Signature) {
        self.foreign_signatures.insert(name.to_string(), signature);
    }

    pub fn register_instance(&mut self, symbol: &str, signature: khora_types::Signature) {
        self.instance_signatures.insert(symbol.to_string(), signature);
    }

    /// Declares a function the file defines, under its mangled name.
    pub(super) fn declare_definition(&mut self, name: &str) {
        let Some(signature) = self.signature_of(name) else { return };
        let Some(ty) = self.function_type(&signature) else {
            self.error(
                format!(
                    "`{name}` cannot be compiled: every parameter and the return type need a \
                     type the backend knows (`Int`, `Bool`, `String`, `()` or an ADT)"
                ),
                TextRange::empty(0.into()),
            );
            return;
        };
        let f = self.module.add_function(&mangle(name), ty, Some(Linkage::External));
        self.functions.insert(name.to_string(), f);
        self.defined.insert(name.to_string());
    }

    /// The LLVM function for a call to `name`, declaring an extern on demand.
    ///
    /// A name the file declares but does not define is a C symbol: the
    /// generated code calls it unmangled, which is what lets Khora source reach
    /// the runtime. Nothing checks that the symbol exists — the linker does
    /// that, and its message names the symbol.
    ///
    /// # The trap
    ///
    /// `add_function` with a name the module already has does **not** fail. It
    /// silently appends a suffix, so declaring `khora_print_int` from Khora
    /// source on top of the runtime's own declaration produces a call to
    /// `khora_print_int.3` — which nothing defines, and which surfaces as an
    /// undefined symbol from the linker with no hint of where the `.3` came
    /// from. Always look the name up first.
    pub fn callee(&mut self, name: &str) -> Result<FunctionValue<'ctx>, String> {
        if let Some(f) = self.functions.get(name) {
            return Ok(*f);
        }
        // Which map to ask depends on what kind of thing this is, and
        // `is_defined` is that question: a name no Khora body defines is a C
        // symbol, whatever else in the program happens to share its spelling.
        let signature = if self.is_defined(name) {
            self.signature_of(name)
        } else {
            self.foreign_signature_of(name)
        }
        .ok_or_else(|| format!("`{name}` has no signature to call through"))?;
        // Three kinds of function reach this point, and only one of them is a
        // call to somewhere else.
        //
        // A Khora body is defined here and needs nothing said about it. An
        // `extern fn` is a C symbol, and what may cross to one is a much
        // shorter list than what the backend can represent. And anything else
        // with no body is a **declaration nobody has kept** — a signature
        // written ahead of its implementation, which is a useful thing to have
        // and not a thing to call. That last kind used to be treated as a C
        // symbol silently, so a misspelled name became `undefined symbol` from
        // the linker rather than a sentence about the program.
        //
        // Checked where the call is generated rather than at the declaration,
        // so a signature written ahead of its implementation is only an error
        // once something tries to run it.
        let foreign = !self.is_defined(name);
        if foreign {
            if !signature.is_extern {
                return Err(format!(
                    "`{name}` has no body, so there is nothing to call. Give it one, or \
                     write `extern fn` if it is a C symbol to be found at link time"
                ));
            }
            if let Some(why) = foreign_signature_obstacle(&signature) {
                return Err(format!(
                    "`{name}` is `extern`, so it crosses the C ABI, and {why}. \
                     Only scalars and pointers cross — `docs/design/ffi.md`"
                ));
            }
        }
        let ty = self.shaped(&signature, foreign).ok_or_else(|| {
            format!(
                "`{name}` has a parameter or return type the backend cannot represent yet; \
                 phase 2 handles `Int`, `Bool`, `String`, `()` and ADTs"
            )
        })?;

        let f = match self.module.get_function(name) {
            Some(existing) if existing.get_type() == ty => existing,
            Some(_) => {
                return Err(format!(
                    "`{name}` is already provided by the runtime with a different signature; \
                     declaring it here would silently call something else"
                ))
            }
            None => self.module.add_function(name, ty, Some(Linkage::External)),
        };
        self.functions.insert(name.to_string(), f);
        Ok(f)
    }

    /// Whether the file defines this name, as opposed to only declaring it.
    pub fn is_defined(&self, name: &str) -> bool {
        self.defined.contains(name)
    }

    /// The function a definition was declared as, for giving it a body.
    pub fn definition(&self, name: &str) -> Option<FunctionValue<'ctx>> {
        self.defined.contains(name).then(|| self.functions[name])
    }

    // -----------------------------------------------------------------------
    // Drop glue
    // -----------------------------------------------------------------------

    // -----------------------------------------------------------------------
    // Closures
    // -----------------------------------------------------------------------
}
