//! The phase 2 demo program must pass every semantic check.

use khora_db::{KhoraDatabase, SourceFile};

#[test]
fn the_core_demo_type_checks() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../examples/core_demo/src/main.kh");
    let text = std::fs::read_to_string(&path).expect("reading the demo");

    let db = KhoraDatabase::new();
    let file = SourceFile::new(&db, path.clone(), text);
    let found: Vec<String> = khora_types::diagnostics(&db, file)
        .iter()
        .map(|e| e.message.clone())
        .collect();

    assert!(found.is_empty(), "the demo should be clean, got {found:?}");
}
