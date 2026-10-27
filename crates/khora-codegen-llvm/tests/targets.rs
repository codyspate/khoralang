#![cfg(feature = "llvm")]

//! Code generation for a machine that is not this one.
//!
//! `docs/design/targets.md` opens by saying Khora compiles for the machine it
//! is running on and nothing else, because `target_machine` initialized only
//! the native target and there was nowhere for a `--target` to point. These
//! check that there is now: an object emitted for a named triple, and the
//! bytes of that object saying which machine it is for.
//!
//! **Linking is not checked and cannot be**, which is the honest limit and the
//! same one `portability.rs` states. An object for `aarch64-unknown-linux-gnu`
//! needs a linker and a sysroot for that target, and this build fetches
//! neither yet — `targets.md` steps three and four. What is proven here is
//! step one, which is the part that had nowhere to point.

mod harness;

use std::path::PathBuf;

use khora_db::{KhoraDatabase, SourceFile, SourceRoot};

/// **One lock for the whole file, and it has to be.** `KHORA_TARGET` is
/// process-wide and cargo runs a binary's tests on several threads. Two locks
/// were worse than none: each test took its own and none excluded the others,
/// so `--test-threads=1` passed and the baseline did not.
static ONE_AT_A_TIME: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// What the first bytes of an object file say it is for.
fn machine_of(bytes: &[u8]) -> String {
    if bytes.starts_with(b"\x7fELF") {
        let machine = bytes[18];
        let name = match machine {
            0x3e => "x86-64".to_string(),
            0xb7 => "aarch64".to_string(),
            other => format!("machine {other:#x}"),
        };
        format!("ELF {name}")
    } else if bytes.starts_with(b"\0asm") {
        format!("WebAssembly v{}", bytes[4])
    } else if bytes.len() > 2 && bytes[0] == 0x64 && bytes[1] == 0x86 {
        "COFF x86-64".to_string()
    } else {
        format!("unrecognized {:02x?}", &bytes[..4.min(bytes.len())])
    }
}

/// Generates for `triple` and reports what the object turned out to be.
///
/// `KHORA_TARGET` is process-wide, so these run one at a time behind a lock —
/// the same reasoning `portability.rs` gives for the same variable.
///
/// **A directory per triple, and the lock is not what makes that safe.** These
/// wrote one `program.exe.o` between them and relied on the mutex above to
/// keep them apart, which holds only while they are threads in one process.
/// `cargo nextest` gives each test a process of its own, so the lock guarded
/// nothing and the aarch64 test read the object the x86_64 test had written —
/// a failure that says "ELF x86-64" where it wanted "ELF aarch64" and looks
/// like a code generation bug. An in-process lock cannot guard a path.
fn emitted_for(triple: &str) -> String {
    let _held = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());

    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("targets").join(triple);
    harness::ensure_runtime();
    std::fs::create_dir_all(&dir).expect("a workspace");
    let exe = dir.join("program.exe");
    let object = dir.join("program.exe.o");
    let _ = std::fs::remove_file(&object);

    let source = "module demo::main;\nfn main() -> () {}\n";
    let db = KhoraDatabase::new();
    let files = vec![SourceFile::new(&db, dir.join("main.kh"), source.to_string())];
    let root = SourceRoot::new(&db, files);

    // SAFETY-of-a-sort: the lock above is what makes this single-threaded.
    unsafe { std::env::set_var("KHORA_TARGET", triple) };
    // Linking is expected to fail for a foreign target and is not the claim;
    // the object beside it is.
    let _ = khora_codegen_llvm::compile(&db, root, &exe);
    unsafe { std::env::remove_var("KHORA_TARGET") };

    let bytes = std::fs::read(&object)
        .unwrap_or_else(|e| panic!("no object was emitted for {triple}: {e}"));
    machine_of(&bytes)
}

/// **The one this was all for.** A Worker takes a wasm module, and here is one,
/// emitted from a Windows host.
#[test]
fn webassembly_is_emitted() {
    assert_eq!(emitted_for("wasm32-unknown-unknown"), "WebAssembly v1");
}

/// arm64 containers and Graviton, from whatever the developer is sitting at.
#[test]
fn aarch64_linux_is_emitted() {
    assert_eq!(emitted_for("aarch64-unknown-linux-gnu"), "ELF aarch64");
}

/// The ordinary server target, cross-generated rather than native.
#[test]
fn x86_64_linux_is_emitted() {
    assert_eq!(emitted_for("x86_64-unknown-linux-gnu"), "ELF x86-64");
}

/// A triple no backend was built for is refused by name, rather than
/// producing something that cannot be run.
///
/// **The triple has to be an unknown *architecture*, not a real one this LLVM
/// happens to lack.** This asked for `sparc64-unknown-linux-gnu`, on the
/// reasoning that nothing here enables SPARC. That is true of the pinned
/// Windows LLVM and false of Ubuntu's, where SPARC is a registered target: code
/// generation succeeded, emitted a SPARC object, and the failure arrived from
/// the host linker instead --
///
///     Relocations in generic ELF (EM: 43)
///
/// which is a different refusal with a different message, so the test failed
/// on every Linux runner. Which targets a build has is a property of how LLVM
/// was configured; `nonesuch` is not an architecture anywhere.
#[test]
fn an_unknown_triple_is_refused() {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("targets_bad");
    harness::ensure_runtime();
    std::fs::create_dir_all(&dir).expect("a workspace");

    let _held = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());

    let db = KhoraDatabase::new();
    let source = "module demo::main;\nfn main() -> () {}\n";
    let files = vec![SourceFile::new(&db, dir.join("main.kh"), source.to_string())];
    let root = SourceRoot::new(&db, files);

    // SAFETY-of-a-sort: as above.
    unsafe { std::env::set_var("KHORA_TARGET", "nonesuch-unknown-none") };
    let outcome = khora_codegen_llvm::compile(&db, root, &dir.join("program.exe"));
    unsafe { std::env::remove_var("KHORA_TARGET") };

    let errors = outcome.expect_err("a target with no backend should be refused");
    let said = errors.iter().map(|e| e.message.clone()).collect::<Vec<_>>().join("\n");
    assert!(
        said.contains("nonesuch") && said.contains("inkwell"),
        "the refusal should name the triple and where the target list comes from: {said}"
    );
}

// --- and a module that actually links -------------------------------------

/// Whether a missing tool or archive must fail the test rather than skip it.
///
/// **What this prevents: a check that has never run reporting that it passed.**
/// Every wasm test below skipped when the wasm runtime archive was absent, and
/// nothing in CI built one, so the archive went 36 compile errors and a
/// corrupted `Array` header without a single red run. A developer without the
/// wasm target still gets a skip; CI, which sets `CI`, gets a failure.
///
/// **Linux only**, because that is the one runner `ci.yml` builds the wasm
/// archive on. The archive is the same bytes whichever host builds it, so the
/// other two runners would be checking nothing new, and requiring it there
/// without building it would make them fail for a missing artifact.
fn must_not_skip() -> bool {
    std::env::var_os("CI").is_some() && cfg!(target_os = "linux")
}

/// Skips with `why`, or fails under CI. See [`must_not_skip`].
fn skip(why: &str) {
    assert!(!must_not_skip(), "{why}; CI must run this, not skip it");
    eprintln!("{why}; skipping");
}

/// The wasm runtime archive, if somebody has built one.
///
/// Not built here: `cargo build -p khora-rt --target wasm32-unknown-unknown`
/// downloads a target's standard library the first time and takes minutes, and
/// a test that does that on a cold machine is a test nobody runs. Skipped with
/// a message instead, except under CI -- [`must_not_skip`].
///
/// Looked for in the target directory this test was built into, which is the
/// one `cargo build -p khora-rt --target ...` beside it wrote to, wherever
/// `CARGO_TARGET_DIR` put it.
fn wasm_runtime() -> Option<PathBuf> {
    let target = PathBuf::from(env!("CARGO_TARGET_TMPDIR"));
    let target = target.parent()?;
    let path = target.join("wasm32-unknown-unknown").join("debug").join("libkhora_rt.a");
    path.is_file().then_some(path)
}

/// Links `source` as a wasm library into `dir/lib.wasm` against `archive`.
///
/// `KHORA_RT_LIB` names the archive rather than leaving the toolchain to find
/// it, so the module is linked against the archive [`wasm_runtime`] found and
/// not some other one with the same name.
fn wasm_library(dir: &std::path::Path, archive: &std::path::Path, source: &str) -> PathBuf {
    let _ = std::fs::remove_dir_all(dir);
    std::fs::create_dir_all(dir).expect("a workspace");
    let out = dir.join("lib.wasm");

    let db = KhoraDatabase::new();
    let files = vec![SourceFile::new(&db, dir.join("lib.kh"), source.to_string())];
    let root = SourceRoot::new(&db, files);

    // SAFETY-of-a-sort: process-wide, and held by the caller's lock.
    unsafe {
        std::env::set_var("KHORA_TARGET", "wasm32-unknown-unknown");
        std::env::set_var("KHORA_RT_LIB", archive);
    }
    let outcome = khora_codegen_llvm::compile_library(&db, root, &out);
    unsafe {
        std::env::remove_var("KHORA_TARGET");
        std::env::remove_var("KHORA_RT_LIB");
    }

    if let Err(errors) = outcome {
        let said = errors.iter().map(|e| e.message.clone()).collect::<Vec<_>>().join("\n");
        panic!("a wasm library should link:\n{said}");
    }
    out
}

/// Instantiates `module` with no imports under Node's WebAssembly API and
/// calls `export` with `args`, returning what it printed: the answer, or
/// `trap: ...`. Every export these tests call returns an `Int`, so an answer
/// that is not an `i64` -- a JS `Number` rather than a `BigInt` -- comes back
/// as `not an i64: ...` and fails whatever it is compared with.
///
/// **Run, not just inspected**, because the inspection passed while the
/// module was wrong: a module that links, validates and exports the right
/// names returned garbage from its first `Array<U8>`. `None` when there is no
/// `node`, which [`skip`] turns into a failure under CI.
fn run_wasm(module: &std::path::Path, export: &str, args: &[i64]) -> Option<String> {
    // A file rather than `node -e`: the script is part of what a failure is
    // reported against, and a reader can run it by hand.
    const RUNNER: &str = "\
const fs = require('fs');
const [file, name, ...args] = process.argv.slice(2);
const compiled = new WebAssembly.Module(fs.readFileSync(file));
const instance = new WebAssembly.Instance(compiled, {});
try {
  const answer = instance.exports[name](...args.map(BigInt));
  console.log(typeof answer === 'bigint' ? String(answer) : 'not an i64: ' + String(answer));
} catch (e) {
  console.log('trap: ' + e.message);
}
";
    // Not `module` for the compiled module: CommonJS already declares that
    // name, and Node answers the clash by rereading the file as an ES module,
    // where `require` does not exist -- an error about something else.
    let runner = module.with_file_name("run.cjs");
    std::fs::write(&runner, RUNNER).expect("writing the runner");
    let ran = std::process::Command::new("node")
        .arg(&runner)
        .arg(module)
        .arg(export)
        .args(args.iter().map(i64::to_string))
        .output()
        .ok()?;
    assert!(
        ran.status.success(),
        "node could not run the module: {}",
        String::from_utf8_lossy(&ran.stderr)
    );
    Some(String::from_utf8_lossy(&ran.stdout).trim().to_string())
}

/// **Bytes made into an array, then a string, inside a wasm module.**
///
/// The runtime wrote an array's header -- length, glue, element kind, stride
/// -- as `usize` words, which are four bytes on wasm32, while generated code
/// reads eight-byte fields on every target. So every `Array` built in a wasm
/// module had its length where generated code did not look and its elements
/// eight bytes early, and nothing that linked or validated the module could
/// tell: `String::from_bytes` saw bytes that were not the ones written and
/// trapped as not UTF-8. The export below answers `24 + 65`, the length and
/// the last byte.
#[test]
fn a_wasm_module_reads_back_the_bytes_it_built() {
    let Some(archive) = wasm_runtime() else {
        skip("no wasm khora-rt; build it with `cargo build -p khora-rt --target wasm32-unknown-unknown`");
        return;
    };
    let _held = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());

    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("wasm_bytes");
    let source = "module w;
pub type Array<A>;
impl<A> Array<A> {
  fn new(length: Int, fill: A) -> Array<A>;
  fn length(self) -> Int;
}
impl String {
  fn from_bytes(bytes: Array<U8>) -> String;
  fn byte_length(self) -> Int;
  fn byte(self, index: Int) -> U8;
}
impl U8 {
  fn of(value: Int) -> U8;
  fn to_int(self) -> Int;
}

pub extern fn counted() -> Int {
  let bytes: Array<U8> = Array::new(24, U8::of(65));
  let text = String::from_bytes(bytes);
  String::byte_length(text) + U8::to_int(String::byte(text, 23))
}
";
    let module = wasm_library(&dir, &archive, source);
    let Some(answer) = run_wasm(&module, "counted", &[]) else {
        skip("no `node` to run the module with");
        return;
    };
    assert_eq!(answer, "89", "24 bytes of `A` should read back as 24 bytes of `A`");
}

/// **A loop, a `!` and a region, inside a wasm module.**
///
/// Generated code is the same on every target: a back-edge and a `!` load
/// `khora_poll` and may call `khora_canceled` or `khora_back_edge`, and a
/// region is `khora_region_open`, `_defer` and `_release`. None of them
/// existed on wasm, where there are no fibers to cancel, so any module with a
/// loop failed in `wasm-ld`. This one also checks the finalizers ran, and in
/// which order: each writes its digit after what is there, and the last one
/// deferred runs first, as on every target -- a rollback before the
/// connection it ran on is closed. So the answer is the loop's sum,
/// 0 + 1 + ... + 9 = 45, plus `21`; first-deferred-first would give `12`.
#[test]
fn a_wasm_module_runs_a_loop_a_raise_and_a_region() {
    let Some(archive) = wasm_runtime() else {
        skip("no wasm khora-rt; build it with `cargo build -p khora-rt --target wasm32-unknown-unknown`");
        return;
    };
    let _held = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());

    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("wasm_control");
    let source = "module w;
pub type Array<A>;
impl<A> Array<A> {
  fn new(length: Int, fill: A) -> Array<A>;
  fn get(self, index: Int) -> A;
  fn set(self, index: Int, value: A) -> ();
}
pub type Region;
impl Region {
  fn open() -> Region;
  fn defer(self, finalizer: () -> ()) -> ();
}

type Bad = | Bad(Int);

fn checked(n: Int) -> Int raises Bad {
  if n < 0 { raise Bad::Bad(n) } else { n }
}

fn summed(n: Int, seen: Array<Int>) -> Int {
  let region = Region::open();
  Region::defer(region, fn () => Array::set(seen, 0, Array::get(seen, 0) * 10 + 1));
  Region::defer(region, fn () => Array::set(seen, 0, Array::get(seen, 0) * 10 + 2));
  let mut i = 0;
  let mut total = 0;
  while i < n {
    total = total + (checked(i)! catch { Bad::Bad(_) => 0 });
    i = i + 1;
  };
  total
}

pub extern fn looped(n: Int) -> Int {
  let seen: Array<Int> = Array::new(1, 0);
  let total = summed(n, seen);
  total + Array::get(seen, 0)
}
";
    let module = wasm_library(&dir, &archive, source);
    let Some(answer) = run_wasm(&module, "looped", &[10]) else {
        skip("no `node` to run the module with");
        return;
    };
    assert_eq!(answer, "66", "the loop's 45, and the finalizers' 21: last deferred, first run");
}

/// **A `print` inside a wasm module returns.**
///
/// Generated code passes a string's length as an `i64` on every target, and
/// the runtime took a `usize`, an `i32` on wasm32. ELF links the two without
/// looking; `wasm-ld` checks, warns, and links the call to a stub whose body is
/// `unreachable`. So every `print` in a wasm module was a trap. Where the text
/// goes is the host's business -- this checks only that the call comes back.
#[test]
fn a_wasm_module_can_print_and_carry_on() {
    let Some(archive) = wasm_runtime() else {
        skip("no wasm khora-rt; build it with `cargo build -p khora-rt --target wasm32-unknown-unknown`");
        return;
    };
    let _held = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());

    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("wasm_print");
    let source = "module w;
fn print(value: String);
extern fn khora_eprint(text: Ptr, length: Int) -> ();
pub type Ptr;
impl String {
  fn with_data<B, 'c, 'e>(self, body: (Ptr, Int) -> B with 'c raises 'e) -> B;
}

pub extern fn spoke(n: Int) -> Int {
  print(\"to stdout\");
  String::with_data(\"to stderr\", fn (p, len) => khora_eprint(p, len));
  n + 1
}
";
    let module = wasm_library(&dir, &archive, source);
    let Some(answer) = run_wasm(&module, "spoke", &[41]) else {
        skip("no `node` to run the module with");
        return;
    };
    assert_eq!(answer, "42", "both prints should return");
}

/// **The leak check runs inside a wasm module.**
///
/// `extern fn khora_live_count() -> Int;` is how a Khora program asks what is
/// still allocated, and an `Int` is an `i64`. The runtime returned a `usize`,
/// an `i32` on wasm32, and `wasm-ld` links such a call to a stub that traps,
/// so the check ended the call it was checking. A library has no `main` to
/// switch counting on, so both counters answer `-1`, "not counting": the
/// export answers `-1 * 1000 + -1`. The same counter is also exported to the
/// host, where the generated header promises a `long long`; the runner
/// refuses an answer that is not one.
#[test]
fn a_wasm_module_can_ask_what_is_live() {
    let Some(archive) = wasm_runtime() else {
        skip("no wasm khora-rt; build it with `cargo build -p khora-rt --target wasm32-unknown-unknown`");
        return;
    };
    let _held = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());

    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("wasm_counters");
    let source = "module w;
extern fn khora_live_count() -> Int;
extern fn khora_alloc_count() -> Int;

pub extern fn counted() -> Int {
  let live = khora_live_count();
  let made = khora_alloc_count();
  live * 1000 + made
}
";
    let module = wasm_library(&dir, &archive, source);
    let Some(answer) = run_wasm(&module, "counted", &[]) else {
        skip("no `node` to run the module with");
        return;
    };
    assert_eq!(answer, "-1001", "both counters, not counting");
    let Some(exported) = run_wasm(&module, "khora_live_count", &[]) else {
        skip("no `node` to run the module with");
        return;
    };
    assert_eq!(exported, "-1", "the exported counter, as the host calls it");
}

/// **A `.wasm` module, linked, with the exports a host would call.**
///
/// `targets.md` step three for the one target that needs no sysroot: a wasm
/// module has no libc to find and no CRT to link, so the runtime archive and
/// the emitted object are the whole input and `wasm-ld` is the whole linker.
///
/// What this asserts is the export section, because that is where the two
/// mistakes were. `--export-dynamic` published 1,255 symbols from a
/// three-function library and defeated dead-code elimination — 2.5 MB where
/// naming them gives 1.9 MB and would give far less without the trap
/// machinery. And `--allow-undefined` silently turned `khora_overflow`,
/// `khora_contain_enabled` and `khora_export_call` into `env.` imports the
/// embedder would have had to supply, *while they were defined in the archive
/// on the same command line*: the module linked, validated, exported exactly
/// the right names, and could not have been instantiated.
#[test]
fn a_wasm_library_links_and_exports_what_it_should() {
    let Some(archive) = wasm_runtime() else {
        skip("no wasm khora-rt; build it with `cargo build -p khora-rt --target wasm32-unknown-unknown`");
        return;
    };
    let _held = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());

    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("wasm_library");
    let source = "module w;\n\
                  pub extern fn price(units: Int, scale: Int) -> Int { units * scale }\n";
    let out = wasm_library(&dir, &archive, source);

    let bytes = std::fs::read(&out).expect("the module was written");
    assert_eq!(&bytes[..4], b"\0asm", "it is a wasm module");

    let (exports, imports) = sections_of(&bytes);
    assert!(exports.contains(&"price".to_string()), "the export is there: {exports:?}");
    assert!(
        exports.contains(&"khora_set_trap_policy".to_string()),
        "and the control surface: {exports:?}"
    );
    // Nothing else. A Worker is measured in megabytes and this is a
    // three-function library.
    assert!(exports.len() < 20, "only what was asked for, got {}: {exports:?}", exports.len());
    assert!(
        !exports.iter().any(|e| e.starts_with("kh$")),
        "no mangled internals: {exports:?}"
    );

    // **No imports at all**, which is the whole of the `--allow-undefined`
    // lesson: everything this module needs, it carries.
    assert!(imports.is_empty(), "a self-contained module imports nothing, got {imports:?}");
}

// --- a static executable, for musl ---------------------------------------

/// The musl triple for this machine's architecture.
#[cfg(target_os = "linux")]
fn host_musl() -> String {
    format!("{}-unknown-linux-musl", std::env::consts::ARCH)
}

/// A musl runtime archive, if somebody has built one for `triple`.
///
/// Only asks whether it exists: the build itself must find it without being
/// told, which is the claim. Built by `scripts/build-musl-runtime.sh`, which
/// is what `ci.yml` runs before these on both Linux runners.
#[cfg(target_os = "linux")]
fn musl_runtime_built(triple: &str) -> bool {
    let target = PathBuf::from(env!("CARGO_TARGET_TMPDIR"));
    let Some(target) = target.parent() else { return false };
    target.join(triple).join("debug").join("libkhora_rt.a").is_file()
}

/// Builds `source` as an executable for `triple` into `dir`, and returns it.
///
/// No `KHORA_RT_LIB` and no sysroot named: finding the target's runtime
/// archive and C library is part of what is being tested.
#[cfg(target_os = "linux")]
fn static_program(dir: &std::path::Path, triple: &str, source: &str) -> PathBuf {
    let _ = std::fs::remove_dir_all(dir);
    std::fs::create_dir_all(dir).expect("a workspace");
    let exe = dir.join("program");

    let db = KhoraDatabase::new();
    let files = vec![SourceFile::new(&db, dir.join("main.kh"), source.to_string())];
    let root = SourceRoot::new(&db, files);

    // SAFETY-of-a-sort: process-wide, and held by the caller's lock.
    unsafe { std::env::set_var("KHORA_TARGET", triple) };
    let outcome = khora_codegen_llvm::compile(&db, root, &exe);
    unsafe { std::env::remove_var("KHORA_TARGET") };

    if let Err(errors) = outcome {
        let said = errors.iter().map(|e| e.message.clone()).collect::<Vec<_>>().join("\n");
        panic!("a {triple} program should build and link:\n{said}");
    }
    exe
}

/// What an ELF executable's program headers say about how it starts.
///
/// `(machine, interpreter, dynamic)`: the `e_machine` field, whether there is a
/// `PT_INTERP` naming a dynamic loader, and whether there is a `PT_DYNAMIC`
/// segment for one to process. A static executable has neither, and that is
/// the whole of what lets it run in an image with no loader and no libc.
///
/// Read from the file rather than from `file(1)`, which is not on every
/// runner and whose wording varies between versions.
#[cfg(target_os = "linux")]
fn elf_linkage(bytes: &[u8]) -> (u16, bool, bool) {
    assert!(bytes.starts_with(b"\x7fELF") && bytes[4] == 2, "a 64-bit ELF");
    let u16_at = |at: usize| u16::from_le_bytes([bytes[at], bytes[at + 1]]);
    let u32_at = |at: usize| u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap());
    let u64_at = |at: usize| u64::from_le_bytes(bytes[at..at + 8].try_into().unwrap());
    let machine = u16_at(18);
    let (offset, size, count) = (u64_at(32) as usize, u16_at(54) as usize, u16_at(56) as usize);
    let kinds: Vec<u32> = (0..count).map(|i| u32_at(offset + i * size)).collect();
    // PT_INTERP is 3, PT_DYNAMIC is 2.
    (machine, kinds.contains(&3), kinds.contains(&2))
}

/// **A musl program is one file that needs nothing from the machine.**
///
/// What this prevents: a `--target ...-musl` build that links, runs on the
/// machine that built it, and fails in a `scratch` image. Running it here
/// proves only the first two, because this machine has a loader and a libc;
/// a dynamically linked executable runs here too. So the program headers are
/// read as well: no `PT_INTERP` (no loader is asked for) and no `PT_DYNAMIC`
/// (nothing is left for one to resolve). Linux only: musl is a Linux C
/// library, and the program is run.
#[cfg(target_os = "linux")]
#[test]
fn a_musl_program_is_static_and_runs() {
    let triple = host_musl();
    if !musl_runtime_built(&triple) {
        skip(&format!("no {triple} khora-rt; build it with `sh scripts/build-musl-runtime.sh {triple}`"));
        return;
    }
    let _held = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());

    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("musl_static");
    let exe = static_program(
        &dir,
        &triple,
        "module demo::main;\nfn print(value: String);\nfn main() -> Int { print(\"static\"); 0 }\n",
    );

    let (_, interpreter, dynamic) = elf_linkage(&std::fs::read(&exe).expect("the executable"));
    assert!(!interpreter, "a static executable names no dynamic loader");
    assert!(!dynamic, "a static executable has nothing left to resolve at load time");

    // With an empty environment, so nothing from this process -- a
    // `LD_LIBRARY_PATH`, a preload -- can be what made it work.
    let ran = std::process::Command::new(&exe).env_clear().output().expect("running it");
    assert_eq!(String::from_utf8_lossy(&ran.stdout), "static\n", "{}", String::from_utf8_lossy(&ran.stderr));
    assert!(ran.status.success());
}

/// **A fiber, on both backends, in a static musl program.**
///
/// The context switch and the thread-local current fiber are where a static
/// libc could differ -- `static` TLS is laid out by the linker rather than by
/// the loader -- and a program whose fibers fail there passes the test above.
/// Linux only, for the reason that one gives.
#[cfg(target_os = "linux")]
#[test]
fn a_musl_program_runs_fibers_on_both_backends() {
    let triple = host_musl();
    if !musl_runtime_built(&triple) {
        skip(&format!("no {triple} khora-rt; build it with `sh scripts/build-musl-runtime.sh {triple}`"));
        return;
    }
    let _held = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());

    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("musl_fibers");
    let source = "module demo::main;\n\
                  import std::core::{Fiber, print};\n\
                  fn main() -> Int {\n  \
                    let a = Fiber::spawn(fn () => 20);\n  \
                    let b = Fiber::spawn(fn () => 22);\n  \
                    print((Fiber::join(a)! + Fiber::join(b)!).show());\n  \
                    0\n\
                  }\n";
    let db = KhoraDatabase::new();
    let root = with_std(&db, &dir, source);
    let exe = dir.join("program");
    // SAFETY-of-a-sort: process-wide, and held by the lock above.
    unsafe { std::env::set_var("KHORA_TARGET", &triple) };
    let outcome = khora_codegen_llvm::compile(&db, root, &exe);
    unsafe { std::env::remove_var("KHORA_TARGET") };
    if let Err(errors) = outcome {
        let said = errors.iter().map(|e| e.message.clone()).collect::<Vec<_>>().join("\n");
        panic!("a {triple} program with fibers should build:\n{said}");
    }
    // Static, or this is a test of the glibc build that every other suite
    // already runs.
    let (_, interpreter, dynamic) = elf_linkage(&std::fs::read(&exe).expect("the executable"));
    assert!(!interpreter && !dynamic, "a static executable, or this tests nothing new");

    for backend in ["threads", "scheduler"] {
        let ran = std::process::Command::new(&exe)
            .env_clear()
            .env("KHORA_FIBERS", backend)
            .output()
            .expect("running it");
        assert_eq!(
            String::from_utf8_lossy(&ran.stdout),
            "42\n",
            "{backend}: {}",
            String::from_utf8_lossy(&ran.stderr)
        );
    }
}

/// **An arm64 static executable, linked on an x86-64 machine.**
///
/// What this prevents: the musl link working only for the architecture the
/// compiler runs on -- a host linker, or a host `crt1.o`, would do that and
/// still pass every other test here. It cannot be run on this machine, so it
/// is checked as far as reading it goes: an aarch64 ELF, with no loader asked
/// for. `ci.yml`'s arm64 runner runs the native build of the same thing.
/// x86-64 Linux only, because that is the machine this is a *cross* build
/// from.
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
#[test]
fn an_arm64_musl_program_links_from_x86_64() {
    let triple = "aarch64-unknown-linux-musl";
    if !musl_runtime_built(triple) {
        skip(&format!("no {triple} khora-rt; build it with `sh scripts/build-musl-runtime.sh {triple}`"));
        return;
    }
    let _held = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());

    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("musl_arm64");
    let exe = static_program(
        &dir,
        triple,
        "module demo::main;\nfn print(value: String);\nfn main() -> Int { print(\"arm\"); 0 }\n",
    );
    let (machine, interpreter, dynamic) = elf_linkage(&std::fs::read(&exe).expect("the executable"));
    assert_eq!(machine, 0xb7, "an aarch64 executable");
    assert!(!interpreter && !dynamic, "and a static one");
}

/// **A shared library for musl is refused, by name.**
///
/// What this prevents: `--lib` with a musl triple producing something that
/// looks like a library and is not one a musl host can load. The target exists
/// for a static executable, and a static `libc.a` cannot be put inside a shared
/// object, so this says so rather than handing the linker a contradiction.
#[cfg(target_os = "linux")]
#[test]
fn a_musl_shared_library_is_refused() {
    let triple = host_musl();
    if !musl_runtime_built(&triple) {
        skip(&format!("no {triple} khora-rt; build it with `sh scripts/build-musl-runtime.sh {triple}`"));
        return;
    }
    let _held = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());

    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("musl_library");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("a workspace");
    let db = KhoraDatabase::new();
    let source = "module w;\npub extern fn price(units: Int) -> Int { units * 2 }\n";
    let files = vec![SourceFile::new(&db, dir.join("lib.kh"), source.to_string())];
    let root = SourceRoot::new(&db, files);
    // SAFETY-of-a-sort: process-wide, and held by the lock above.
    unsafe { std::env::set_var("KHORA_TARGET", &triple) };
    let outcome = khora_codegen_llvm::compile_library(&db, root, &dir.join("lib.so"));
    unsafe { std::env::remove_var("KHORA_TARGET") };
    let errors = outcome.expect_err("a musl shared library should be refused");
    let said = errors.iter().map(|e| e.message.clone()).collect::<Vec<_>>().join("\n");
    assert!(said.contains("musl") && said.contains("static"), "the refusal should say why: {said}");
}

/// **A TLS round trip inside one static musl program.**
///
/// What this prevents: TLS that works dynamically and not statically.
/// `rustls` and `ring` carry their own assembly and C, compiled for this
/// target by `build-musl-runtime.sh`, and the client reads the machine's
/// trust store through `rustls-native-certs`, which on Linux opens files a
/// `scratch` image may not have. The server presents the test chain in
/// `tests/fixtures`; the client trusts the test CA explicitly and checks the
/// name, so this is the same verification a real client does. Both ends are
/// Khora, so it shows the static build is coherent; that `rustls` agrees with
/// OpenSSL is `tls.rs`'s claim, made against the ordinary build. Linux only,
/// for the reason [`a_musl_program_is_static_and_runs`] gives.
#[cfg(target_os = "linux")]
#[test]
fn a_musl_program_speaks_tls_to_itself() {
    let triple = host_musl();
    if !musl_runtime_built(&triple) {
        skip(&format!("no {triple} khora-rt; build it with `sh scripts/build-musl-runtime.sh {triple}`"));
        return;
    }
    let _held = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());

    let fixture = |name: &str| {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests").join("fixtures").join(name);
        format!("{:?}", std::fs::read_to_string(path).expect("the test chain is in tests/fixtures"))
    };
    let source = r#"module main;

import std::core::{Array, Fiber, Result, Scope, attempt, print};
import std::net::socket::{accept_on, invalid_handle, listen_on, start};
import std::net::tls::{TlsClient, TlsError, TlsServer, connect, receive, secure, server, shut, transmit, trusting};

const certificate = CERT_PEM;
const key = KEY_PEM;
const authority = CA_PEM;

fn call(caller: TlsClient) -> () {
  match attempt(fn () => connect(caller, "localhost", 18991)!) {
    Result::Err(_) => print("the client could not connect"),
    Result::Ok(connection) => {
      transmit(connection, "static");
      let buffer: Array<U8> = Array::new(4096, 0);
      let read = receive(connection, buffer);
      if read > 0 { print(String::from_bytes(Array::prefix(buffer, read))); } else { print("nothing"); };
      shut(connection)
    },
  }
}

fn answer(settings: TlsServer, socket: Int) -> () raises TlsError {
  let connection = secure(settings, socket)!;
  let buffer: Array<U8> = Array::new(4096, 0);
  let read = receive(connection, buffer);
  if read > 0 { transmit(connection, "you said: " + String::from_bytes(Array::prefix(buffer, read))); } else { };
  shut(connection)
}

pub fn main() -> () raises TlsError {
  with { scope: Scope::root() } {
    start();
    let settings = server(certificate, key)!;
    let listener = listen_on(18991);
    let caller = trusting(authority)!;
    let calling = Fiber::spawn(fn () => call(caller));
    let taken = accept_on(listener);
    if taken == invalid_handle() { } else { answer(settings, taken)! };
    Fiber::join(calling)!
  }
}
"#
    .replace("CERT_PEM", &fixture("localhost.cert.pem"))
    .replace("KEY_PEM", &fixture("localhost.key.pem"))
    .replace("CA_PEM", &fixture("ca.cert.pem"));

    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("musl_tls");
    let db = KhoraDatabase::new();
    let root = with_std(&db, &dir, &source);
    let exe = dir.join("program");
    // SAFETY-of-a-sort: process-wide, and held by the lock above.
    unsafe { std::env::set_var("KHORA_TARGET", &triple) };
    let outcome = khora_codegen_llvm::compile(&db, root, &exe);
    unsafe { std::env::remove_var("KHORA_TARGET") };
    if let Err(errors) = outcome {
        let said = errors.iter().map(|e| e.message.clone()).collect::<Vec<_>>().join("\n");
        panic!("a {triple} TLS program should build:\n{said}");
    }
    let (_, interpreter, dynamic) = elf_linkage(&std::fs::read(&exe).expect("the executable"));
    assert!(!interpreter && !dynamic, "a static executable, or this tests nothing new");

    let ran = std::process::Command::new(&exe).env_clear().output().expect("running it");
    assert_eq!(
        String::from_utf8_lossy(&ran.stdout),
        "you said: static\n",
        "{}",
        String::from_utf8_lossy(&ran.stderr)
    );
}

/// A workspace holding `source` as its `main` module, with the `std` files
/// the target in `KHORA_TARGET` (or the host) selects, as `emission.rs` does.
#[cfg(target_os = "linux")]
fn with_std(db: &KhoraDatabase, dir: &std::path::Path, source: &str) -> SourceRoot {
    let _ = std::fs::remove_dir_all(dir);
    std::fs::create_dir_all(dir).expect("a workspace");
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..").join("..").join("std");
    let mut files = Vec::new();
    let mut stack = vec![root];
    while let Some(here) = stack.pop() {
        for entry in std::fs::read_dir(&here).expect("a readable std") {
            let path = entry.expect("an entry").path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|e| e == "kh")
                && khora_db::selected_for_target(&path, khora_db::host_target())
            {
                let text = std::fs::read_to_string(&path).expect("readable");
                files.push(SourceFile::new(db, path, text));
            }
        }
    }
    files.push(SourceFile::new(db, dir.join("main.kh"), source.to_string()));
    SourceRoot::new(db, files)
}

/// The export and import names in a wasm module.
fn sections_of(bytes: &[u8]) -> (Vec<String>, Vec<String>) {
    fn uleb(b: &[u8], i: &mut usize) -> u64 {
        let (mut r, mut s) = (0u64, 0u32);
        loop {
            let x = b[*i];
            *i += 1;
            r |= u64::from(x & 0x7f) << s;
            s += 7;
            if x & 0x80 == 0 {
                return r;
            }
        }
    }
    fn name(b: &[u8], i: &mut usize) -> String {
        let len = uleb(b, i) as usize;
        let s = String::from_utf8_lossy(&b[*i..*i + len]).into_owned();
        *i += len;
        s
    }

    let (mut exports, mut imports) = (Vec::new(), Vec::new());
    let mut i = 8;
    while i < bytes.len() {
        let id = bytes[i];
        i += 1;
        let size = uleb(bytes, &mut i) as usize;
        let end = i + size;
        let mut j = i;
        if id == 7 {
            let n = uleb(bytes, &mut j);
            for _ in 0..n {
                exports.push(name(bytes, &mut j));
                j += 1;
                uleb(bytes, &mut j);
            }
        } else if id == 2 {
            let n = uleb(bytes, &mut j);
            for _ in 0..n {
                let module = name(bytes, &mut j);
                let what = name(bytes, &mut j);
                imports.push(format!("{module}.{what}"));
                let kind = bytes[j];
                j += 1;
                match kind {
                    0 => {
                        uleb(bytes, &mut j);
                    }
                    1 => {
                        j += 1;
                        let limits = uleb(bytes, &mut j);
                        uleb(bytes, &mut j);
                        if limits == 1 {
                            uleb(bytes, &mut j);
                        }
                    }
                    2 => {
                        let limits = uleb(bytes, &mut j);
                        uleb(bytes, &mut j);
                        if limits == 1 {
                            uleb(bytes, &mut j);
                        }
                    }
                    _ => j += 2,
                }
            }
        }
        i = end;
    }
    (exports, imports)
}
