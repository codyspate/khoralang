//! The `[toolchain]` table: which Khora builds this project.
//!
//! **The pin is the one manifest field a project cannot do without**, so it is
//! the one worth a file of its own. `edition` used to sit beside it answering
//! the same question with a year that named no compiler and that nothing read;
//! these tests are what that field never had.

use khora_manifest::Manifest;

/// A package manifest with `[toolchain]` appended, since every case here needs
/// one and only the table differs.
fn with(table: &str) -> String {
    format!("[package]\nname = \"p\"\nversion = \"0.1.0\"\n\n{table}")
}

#[test]
fn a_version_is_read() {
    let parsed = Manifest::parse(&with("[toolchain]\nversion = \"0.2.0\"\n")).expect("it parses");
    assert_eq!(parsed.manifest.toolchain.expect("a pin").version, "0.2.0");
}

/// **The table exists to answer one question**, so a `[toolchain]` that does
/// not answer it is a mistake rather than a default. `version` is not an
/// `Option`, which is what turns this into serde's own missing-field error.
#[test]
fn an_empty_table_is_a_missing_version() {
    let why = Manifest::parse(&with("[toolchain]\n")).expect_err("an empty table is an error");
    let text = why.to_string();
    assert!(text.contains("version"), "{text}");
}

/// Both channels are accepted, and they are the only two words that are.
#[test]
fn the_channels_are_accepted() {
    for channel in ["latest", "latest.rc"] {
        let text = with(&format!("[toolchain]\nversion = \"{channel}\"\n"));
        let parsed = Manifest::parse(&text).unwrap_or_else(|why| panic!("{channel}: {why}"));
        assert_eq!(parsed.manifest.toolchain.expect("a pin").version, channel);
    }
}

/// **A pin that cannot be resolved has to fail at the manifest.** It decides
/// which binary runs, and that decision is made before any argument is parsed
/// -- so a word that is not a version and not a channel would otherwise come
/// back as "`newest` is not installed", listing the versions that are, which
/// reads as a missing toolchain rather than as the wrong word.
#[test]
fn a_word_that_is_neither_a_version_nor_a_channel_is_refused() {
    for pin in ["newest", "stable", "nightly", "^0.2", "v0.2.0"] {
        let text = with(&format!("[toolchain]\nversion = \"{pin}\"\n"));
        let why = Manifest::parse(&text)
            .err()
            .unwrap_or_else(|| panic!("`{pin}` should be refused"))
            .to_string();
        assert!(why.contains("toolchain.version"), "{pin}: {why}");
        assert!(why.contains("latest"), "the message should name the channels: {why}");
    }
}

/// Anything starting with a digit is left to `khora-toolchain` to resolve
/// against what is installed. This crate does not order versions, and a rule
/// strict enough to reject `0.2.0-rc.1+build.4` here would reject a real
/// release the day the numbering grows a part.
#[test]
fn versions_of_every_shape_are_accepted() {
    for pin in ["0.2.0", "0.2.0-rc.1", "1.0.0-alpha.2+build.4", "10.20.30"] {
        let text = with(&format!("[toolchain]\nversion = \"{pin}\"\n"));
        Manifest::parse(&text).unwrap_or_else(|why| panic!("{pin}: {why}"));
    }
}

/// **`edition` is gone, and says so.** It named a year rather than a compiler,
/// nothing read it, and `[toolchain]` answers the question it was pretending
/// to. A manifest that still carries the line gets a sentence about what
/// replaced it -- not "unrecognized key", which reads as "this toolchain is
/// too old" and is the opposite of the truth.
#[test]
fn edition_is_told_what_replaced_it() {
    let text = "[package]\nname = \"p\"\nversion = \"0.1.0\"\nedition = \"2026\"\n\n\
                [toolchain]\nversion = \"0.2.0\"\n";
    let parsed = Manifest::parse(text).expect("the line is a warning, not an error");
    assert_eq!(parsed.warnings.len(), 1, "{:?}", parsed.warnings);
    let warning = parsed.warnings[0].to_string();
    assert!(warning.contains("edition"), "{warning}");
    assert!(warning.contains("[toolchain]"), "it should name the replacement: {warning}");
}
