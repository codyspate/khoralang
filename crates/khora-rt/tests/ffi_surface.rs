//! Keeps the C boundary honest about its preconditions.
//!
//! A hundred functions cross from generated code into this runtime. Most take
//! a pointer, and every one of those has a precondition the caller must meet —
//! that the object is live, that a buffer is as long as the length beside it,
//! that a `glue` routine belongs to the type whose values it will release.
//!
//! **`unsafe fn` is where a precondition is recorded**, and it is the only
//! place a compiler will act on one. A safe `extern "C" fn` says there is
//! nothing to get wrong. Generated code cannot tell the difference; a Rust
//! caller can, and the runtime's own tests are Rust callers.
//!
//! The phase 13 soundness audit found five that said the wrong thing — all
//! four of `std::fs`'s shims, each already carrying a `SAFETY` comment
//! discharging an obligation nobody had been given, and `khora_array_new`,
//! which keeps a drop routine and calls it once per element. This test is what
//! stops the sixth. `docs/design/soundness.md`.

use std::path::PathBuf;

/// One exported function, as this test needs to see it.
struct Export {
    file: String,
    name: String,
    params: String,
    is_unsafe: bool,
}

/// Every `#[unsafe(no_mangle)]` function in the runtime's sources.
fn exports() -> Vec<Export> {
    let src = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    collect(&src, &mut files);
    files.sort();

    let mut found = Vec::new();
    for path in files {
        let text = std::fs::read_to_string(&path).expect("a readable source file");
        let name = path.file_name().unwrap_or_default().to_string_lossy().to_string();
        let lines: Vec<&str> = text.lines().collect();
        for (i, line) in lines.iter().enumerate() {
            if !line.contains("no_mangle") {
                continue;
            }
            // The signature runs from the `fn` line to the opening brace.
            let Some(start) = (i + 1..lines.len()).find(|j| lines[*j].contains("fn ")) else {
                continue;
            };
            let mut signature = String::new();
            for line in &lines[start..] {
                signature.push_str(line);
                signature.push(' ');
                if line.contains('{') {
                    break;
                }
            }
            found.push(Export {
                file: name.clone(),
                name: between(&signature, "fn ", "("),
                params: parameters(&signature),
                is_unsafe: signature.contains("pub unsafe extern"),
            });
        }
    }
    found
}

fn collect(dir: &std::path::Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

fn between(text: &str, from: &str, to: &str) -> String {
    let Some(start) = text.find(from) else { return String::new() };
    let rest = &text[start + from.len()..];
    match rest.find(to) {
        Some(end) => rest[..end].trim().to_string(),
        None => rest.trim().to_string(),
    }
}

/// The argument list, balanced so a `fn(*mut u8)` parameter type stays inside
/// it rather than ending it.
fn parameters(signature: &str) -> String {
    let Some(open) = signature.find('(') else { return String::new() };
    let mut depth = 0usize;
    for (at, ch) in signature[open..].char_indices() {
        match ch {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    return signature[open + 1..open + at].to_string();
                }
            }
            _ => {}
        }
    }
    String::new()
}

/// Whether a parameter list takes a raw pointer to *data*.
///
/// A `fn(*mut u8)` parameter is a function pointer, not a data pointer, and
/// taking one is safe on its own — what is unsafe is *calling* it, which is
/// the `glue` obligation and is checked separately below.
fn takes_a_data_pointer(params: &str) -> bool {
    let without_fn_types = params.replace("extern \"C\" fn(*mut u8)", "");
    without_fn_types.contains("*const ") || without_fn_types.contains("*mut ")
}

/// **A safe export must have nothing for a caller to get wrong.**
#[test]
fn no_safe_export_takes_a_pointer() {
    let offenders: Vec<String> = exports()
        .iter()
        .filter(|e| !e.is_unsafe && takes_a_data_pointer(&e.params))
        .map(|e| format!("{}: {}({})", e.file, e.name, e.params.trim()))
        .collect();
    assert!(
        offenders.is_empty(),
        "these take a pointer and declare no precondition — make them \
         `pub unsafe extern \"C\" fn` and give them a `# Safety` section:\n  {}",
        offenders.join("\n  ")
    );
}

/// **A `glue` routine is called once per value it is given.** Handing over one
/// that belongs to another type releases those values through the wrong field
/// list, which is the mistake the code generator actually made — see
/// `type_key` in `khora-codegen-llvm`.
#[test]
fn no_safe_export_takes_a_drop_routine() {
    let offenders: Vec<String> = exports()
        .iter()
        .filter(|e| !e.is_unsafe && e.params.contains("glue"))
        .map(|e| format!("{}: {}", e.file, e.name))
        .collect();
    assert!(
        offenders.is_empty(),
        "these keep a drop routine and call it later, which the caller has to \
         get right:\n  {}",
        offenders.join("\n  ")
    );
}

/// The scan itself has to be finding things, or the two tests above pass by
/// looking at nothing. Both numbers are lower bounds rather than exact, so
/// adding a function does not fail this.
#[test]
fn the_scan_sees_the_boundary() {
    let all = exports();
    assert!(all.len() > 80, "expected the whole C surface, found {}", all.len());
    assert!(
        all.iter().filter(|e| e.is_unsafe).count() > 50,
        "most of the boundary takes a pointer and should be unsafe"
    );
    assert!(
        all.iter().any(|e| e.name == "khora_drop"),
        "the scan should find the most-called function in the runtime"
    );
    assert!(
        all.iter().any(|e| e.name == "khora_array_new" && e.is_unsafe),
        "the audit made this one unsafe; it should have stayed that way"
    );
}
