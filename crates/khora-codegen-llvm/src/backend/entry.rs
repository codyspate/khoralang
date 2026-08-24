//! `main`, and the two shapes it comes in.
//!
//! An ordinary program calls the Khora `main` and returns its exit status. A
//! test build registers every `test` block instead and hands them to the
//! runner. Everything between — the same monomorphization, the same lowering —
//! is shared, because a test body is a function body.

use super::*;

impl<'ctx> Backend<'ctx> {
    /// Ends the region that lasts as long as the program does.
    ///
    /// On the failing path too: a finalizer that runs only when nothing went
    /// wrong is not a finalizer, and an uncaught raise is exactly when closing
    /// a file matters.
    pub(super) fn close_root_region(&mut self) {
        let close = self.rt.region_close_root;
        self.builder.build_call(close, &[], "").expect("closing the root region");
    }

    /// Emits a `main` that hands every test to the runner.
    ///
    /// `int main(int argc, char **argv)`, and the call that keeps them.
    ///
    /// The arguments a process was started with arrive exactly once, here, and
    /// are gone. Everything else about a program's environment has a function
    /// to ask — `getenv`, `time` — so this is the one thing the runtime has to
    /// hold on to, and the first thing generated code does is hand it over.
    pub(super) fn entry_point(&mut self) -> FunctionValue<'ctx> {
        let i32_type = self.ctx.i32_type();
        let ptr = self.ctx.ptr_type(AddressSpace::default());
        self.module.add_function(
            "main",
            i32_type.fn_type(&[i32_type.into(), ptr.into()], false),
            None,
        )
    }

    /// Hands `argc` and `argv` to the runtime before anything else runs.
    pub(super) fn remember_arguments(&mut self, main: FunctionValue<'ctx>) {
        let (Some(argc), Some(argv)) = (main.get_nth_param(0), main.get_nth_param(1)) else {
            return;
        };
        self.builder
            .build_call(self.rt.args_set, &[argc.into(), argv.into()], "")
            .expect("recording the command line");
    }

    /// Registration is a loop of calls rather than a table, because a table
    /// would need a layout agreed with the runtime and this needs nothing: the
    /// name is a pointer and a length, and the body is a function pointer.
    pub(super) fn emit_test_main(&mut self, tests: &[(String, String)]) {
        let main = self.entry_point();
        let entry = self.ctx.append_basic_block(main, "entry");
        self.builder.position_at_end(entry);
        self.remember_arguments(main);

        for (symbol, name) in tests {
            let Some(function) = self.functions.get(symbol).copied() else { continue };
            let text = self
                .builder
                .build_global_string_ptr(name, "test.name")
                .expect("a test's name")
                .as_pointer_value();
            let len = self.ctx.i64_type().const_int(name.len() as u64, false);
            let code = function.as_global_value().as_pointer_value();
            // The trampoline, not the test itself: a tagged return does not
            // cross into the runtime. See `tagged_trampoline`.
            let call = self.tagged_trampoline(0).as_global_value().as_pointer_value();
            self.builder
                .build_call(
                    self.rt.test_register,
                    &[text.into(), len.into(), code.into(), call.into()],
                    "",
                )
                .expect("registering a test");
        }

        let status = self
            .builder
            .build_call(self.rt.test_run, &[], "status")
            .expect("running the tests")
            .try_as_basic_value()
            .basic()
            .expect("the runner returns a status")
            .into_int_value();
        // The root region ends here too: a test that deferred something to it
        // is as entitled to have it run as `main` is.
        self.close_root_region();
        self.builder.build_return(Some(&status)).expect("returning from main");
    }

    /// Emits the C `main` the operating system actually starts.
    ///
    /// Khora's `main` is an ordinary Khora function; this is the shim that
    /// gives it the signature a C runtime expects. An `Int` result becomes the
    /// exit code, truncated to `i32` because that is all a process status
    /// carries — a Khora `main` returning 2^32 exits 0, which is the same thing
    /// C does and worth knowing about.
    pub(super) fn emit_c_main(&mut self, entry: Option<&str>) {
        let Some(entry) = entry else {
            self.error(
                "this program has no `main` function, so there is nothing to run",
                TextRange::empty(0.into()),
            );
            return;
        };
        let Some(signature) = self.signature_of(entry) else {
            self.error(
                "this program has no `main` function, so there is nothing to run",
                TextRange::empty(0.into()),
            );
            return;
        };
        if !self.is_defined(entry) {
            self.error(
                "`main` is declared without a body, so there is nothing to run",
                TextRange::empty(0.into()),
            );
            return;
        }
        if !signature.params.is_empty() {
            self.error(
                "`main` cannot take parameters yet; command-line arguments arrive with the \
                 standard library",
                TextRange::empty(0.into()),
            );
            return;
        }

        let khora_main = self.functions[entry];
        let raises = self.signature_of(entry).is_some_and(|s| can_raise(&s));
        let i32_type = self.ctx.i32_type();
        let main = self.entry_point();
        let entry = self.ctx.append_basic_block(main, "entry");
        self.builder.position_at_end(entry);
        self.remember_arguments(main);

        // Before anything else, and only when it is true. Generated code has
        // been counting references without atomics on the strength of this, and
        // the runtime turns a spawn that happens anyway into a message rather
        // than a race. See `Backend::single_threaded`.
        if self.single_threaded {
            self.builder
                .build_call(self.rt.single_threaded, &[], "")
                .expect("declaring the program single-threaded");
        }

        let call = self.builder.build_call(khora_main, &[], "result").expect("calling main");

        // An entry point that can raise has nowhere to hand the error, so an
        // uncaught raise is a failing exit. This is what makes a program that
        // raises runnable at all before `catch` lands, and it is the behaviour
        // a shell expects either way.
        let result = if raises {
            let tagged = call
                .try_as_basic_value()
                .basic()
                .expect("a fallible main returns a tagged value")
                .into_struct_value();
            let which = self
                .builder
                .build_extract_value(tagged, 0, "which")
                .expect("reading the tag")
                .into_int_value();
            let flag = self
                .builder
                .build_int_compare(
                    inkwell::IntPredicate::NE,
                    which,
                    self.ctx.i32_type().const_zero(),
                    "raised",
                )
                .expect("testing the tag");
            let word = self
                .builder
                .build_extract_value(tagged, 1, "payload")
                .expect("reading the payload")
                .into_int_value();

            let failed = self.ctx.append_basic_block(main, "raised");
            let ok = self.ctx.append_basic_block(main, "ok");
            self.builder.build_conditional_branch(flag, failed, ok).expect("branching on the tag");

            self.builder.position_at_end(failed);
            self.close_root_region();
            // A cancellation that reached the entry point and an error that
            // did are different outcomes, and worth telling apart from
            // outside: 130 is 128 + SIGINT, which is what a shell already
            // means by "interrupted".
            let cancelled_which =
                self.ctx.i32_type().const_int(runtime::CANCELLED_WHICH, false);
            let was_cancelled = self
                .builder
                .build_int_compare(
                    inkwell::IntPredicate::EQ,
                    which,
                    cancelled_which,
                    "cancelled",
                )
                .expect("testing for a cancellation");
            let status = self
                .builder
                .build_select(
                    was_cancelled,
                    i32_type.const_int(runtime::CANCELLED_EXIT, false),
                    i32_type.const_int(1, false),
                    "status",
                )
                .expect("choosing an exit status");
            self.builder.build_return(Some(&status)).expect("exiting on an uncaught raise");

            self.builder.position_at_end(ok);
            Some(word.into())
        } else {
            call.try_as_basic_value().basic()
        };

        let code = match signature.ret {
            Type::Int => {
                let value = result
                    .expect("an `Int` main returns a value")
                    .into_int_value();
                self.builder
                    .build_int_truncate(value, i32_type, "exit")
                    .expect("truncating the exit code")
            }
            Type::Unit => i32_type.const_zero(),
            other => {
                self.error(
                    format!(
                        "`main` returns `{other}`, but an entry point must return `Int` or `()`"
                    ),
                    TextRange::empty(0.into()),
                );
                i32_type.const_zero()
            }
        };
        self.close_root_region();
        self.builder.build_return(Some(&code)).expect("returning from main");
    }

    // -----------------------------------------------------------------------
    // Emission
    // -----------------------------------------------------------------------
}
