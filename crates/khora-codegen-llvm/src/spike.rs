//! End-to-end smoke test of the backend toolchain.
//!
//! This is not a code generator. It builds the smallest possible module by
//! hand so the pipeline — inkwell, target machine, object emission, linking,
//! execution — is proven independently of the rest of the compiler. When Phase 2
//! codegen starts failing, this tells us whether the toolchain or our lowering
//! is at fault.

use std::path::Path;

use inkwell::context::Context;
use inkwell::targets::{
    CodeModel, FileType, InitializationConfig, RelocMode, Target, TargetMachine,
};
use inkwell::OptimizationLevel;

/// CPU and feature set to generate for.
///
/// Deliberately generic rather than the host's. §6.1 requires bit-for-bit
/// reproducible builds, which host-specific instruction selection would break —
/// CPU tuning belongs behind an explicit target flag, never a silent default.
const CPU: &str = "generic";
const FEATURES: &str = "";

/// Emits an object file defining `int main(void) { return value; }`.
pub fn emit_constant_main(out: &Path, value: i32) -> Result<(), String> {
    Target::initialize_native(&InitializationConfig::default())
        .map_err(|e| format!("initializing native target: {e}"))?;

    let context = Context::create();
    let module = context.create_module("khora_spike");
    let builder = context.create_builder();

    let i32_type = context.i32_type();
    let main = module.add_function("main", i32_type.fn_type(&[], false), None);
    let entry = context.append_basic_block(main, "entry");
    builder.position_at_end(entry);
    builder
        .build_return(Some(&i32_type.const_int(value as u64, false)))
        .map_err(|e| format!("building return: {e}"))?;

    let triple = TargetMachine::get_default_triple();
    module.set_triple(&triple);

    let target = Target::from_triple(&triple)
        .map_err(|e| format!("resolving target {}: {e}", triple.as_str().to_string_lossy()))?;
    let machine = target
        .create_target_machine(
            &triple,
            CPU,
            FEATURES,
            OptimizationLevel::Default,
            RelocMode::Default,
            CodeModel::Default,
        )
        .ok_or_else(|| "creating target machine".to_string())?;

    // Bind the target data rather than chaining: the `DataLayout` borrows it.
    let target_data = machine.get_target_data();
    module.set_data_layout(&target_data.get_data_layout());

    module.verify().map_err(|e| format!("module verification failed: {e}"))?;
    machine
        .write_to_file(&module, FileType::Object, out)
        .map_err(|e| format!("writing object: {e}"))?;
    Ok(())
}
