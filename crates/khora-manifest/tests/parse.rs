//! End-to-end tests for `khora.toml` parsing.

use khora_manifest::{IndentStyle, LintLevel, Manifest, ManifestError, Parsed, WarningKind};

/// The manifest from `docs/project.md` §4.1, as an actual project ships it.
///
/// Included from the example rather than copied, so that a change to the format
/// that forgets this crate fails here instead of silently diverging.
const EXAMPLE: &str = include_str!("../../../examples/risk_analyzer/khora.toml");

fn parse(text: &str) -> Parsed {
    match Manifest::parse(text) {
        Ok(parsed) => parsed,
        Err(error) => panic!("expected this manifest to parse, got `{error}`:\n{text}"),
    }
}

fn parse_error(text: &str) -> ManifestError {
    match Manifest::parse(text) {
        Ok(parsed) => panic!("expected this manifest to fail, got {:?}:\n{text}", parsed.manifest),
        Err(error) => error,
    }
}

fn warning_keys(parsed: &Parsed) -> Vec<&str> {
    parsed.warnings.iter().map(|warning| warning.key()).collect()
}

#[test]
fn the_reference_manifest_parses_with_no_warnings() {
    let parsed = parse(EXAMPLE);

    assert_eq!(
        warning_keys(&parsed),
        Vec::<&str>::new(),
        "every key in the reference manifest should be recognized"
    );

    let manifest = parsed.manifest;
    assert_eq!(manifest.package.name, "risk_analyzer");
    assert_eq!(manifest.package.version, "0.1.0");
    assert_eq!(manifest.package.authors, ["Engineering Team <dev@khora.internal>"]);
    assert_eq!(manifest.package.edition.as_deref(), Some("2026"));

    assert_eq!(
        manifest.permissions.network.as_deref(),
        Some(["0.0.0.0:8080".to_string(), "*.internal:5432".to_string()].as_slice()),
        "grants survive verbatim: the checker needs the text the author wrote in order \
         to point at the offending one"
    );
    let fs = manifest.permissions.fs.clone().expect("the reference manifest grants file access");
    assert_eq!(fs.read, ["/etc/config", "./data/**"]);
    assert_eq!(fs.write, ["./tmp/**"]);
    assert_eq!(manifest.permissions.env.as_deref(), Some(["DB_*".to_string()].as_slice()));

    let fmt = manifest.fmt.expect("the reference manifest configures `[fmt]`");
    assert_eq!(fmt.indent_style, Some(IndentStyle::Space));
    assert_eq!(fmt.indent_width, Some(2));
    assert_eq!(fmt.explicit_semicolons, Some(true));

    assert_eq!(
        manifest.lints.keys().collect::<Vec<_>>(),
        ["cyclomatic-complexity", "unused-capabilities"],
        "both lint spellings should land in the same map"
    );
    assert_eq!(manifest.lints["unused-capabilities"].level, LintLevel::Deny);
    assert_eq!(manifest.lints["cyclomatic-complexity"].level, LintLevel::Warn);
    assert_eq!(
        manifest.lints["cyclomatic-complexity"].option("max").and_then(toml::Value::as_integer),
        Some(15),
        "lint-defined options should be kept as written"
    );

    assert_eq!(
        manifest.dependencies.keys().collect::<Vec<_>>(),
        ["std.ai", "std.core", "std.net.http"],
        "a quoted dotted name is one dependency, not a tree of tables"
    );
    assert_eq!(manifest.dependencies["std.core"].version, "1.0.0");

    let build = manifest.build.expect("the reference manifest configures `[build]`");
    assert_eq!(build.target.as_deref(), Some("x86_64-unknown-linux-musl"));
    assert_eq!(build.plugin.as_deref(), Some("protobuf-compiler@2.1"));

    let ci = manifest.tasks.get("ci").expect("`[tasks.ci]` should be present");
    assert_eq!(ci.description.as_deref(), Some("Run the full CI pipeline"));
    assert_eq!(ci.depends_on, ["lint", "test", "build"]);
}

#[test]
fn every_key_the_schema_documents_is_recognized() {
    // Guards the duplication between the typed model and the audit's key list:
    // a key added to one but not the other shows up here as a warning.
    let parsed = parse(
        r#"
        [package]
        name = "p"
        version = "0.1.0"
        authors = ["A <a@example.com>"]
        edition = "2026"

        [permissions]
        network = []
        env = []

        [fmt]
        indent-style = "tab"
        indent-width = 4
        explicit-semicolons = false

        [lints]
        bare = "allow"
        tabled = { level = "deny" }

        [dependencies]
        "std.effect" = { version = "1.0.0" }

        [build]
        target = "wasm32-wasip1"
        plugin = "protobuf-compiler@2.1"

        [tasks.test]
        description = "Run the tests"
        depends_on = ["build"]
        "#,
    );

    assert_eq!(
        warning_keys(&parsed),
        Vec::<&str>::new(),
        "the documented schema should produce no warnings"
    );
}

#[test]
fn a_package_table_is_enough() {
    let parsed = parse("[package]\nname = \"p\"\nversion = \"0.1.0\"\n");

    assert!(parsed.warnings.is_empty(), "a minimal manifest should be warning-free");
    let manifest = parsed.manifest;
    assert_eq!(manifest.package.name, "p");
    assert!(manifest.package.authors.is_empty(), "`authors` should default to empty");
    assert_eq!(manifest.package.edition, None);
    assert_eq!(manifest.permissions, Default::default(), "no grants means no capabilities");
    assert_eq!(manifest.fmt, None, "an absent `[fmt]` must stay distinguishable from an empty one");
    assert_eq!(manifest.build, None);
    assert!(manifest.lints.is_empty());
    assert!(manifest.dependencies.is_empty());
    assert!(manifest.tasks.is_empty());
}

#[test]
fn lints_accept_a_bare_level_or_a_table() {
    let parsed = parse(
        r#"
        [package]
        name = "p"
        version = "0.1.0"

        [lints]
        bare = "warn"
        levelled = { level = "warn" }
        with-options = { level = "warn", max = 15, note = "why" }
        "#,
    );

    let lints = parsed.manifest.lints;
    assert_eq!(lints["bare"].level, LintLevel::Warn);
    assert!(lints["bare"].options.is_empty(), "the bare form carries no options");
    assert_eq!(
        lints["levelled"], lints["bare"],
        "both spellings of the same level should produce the same lint"
    );

    let options = &lints["with-options"].options;
    assert_eq!(options.len(), 2, "everything except `level` is an option: {options:?}");
    assert_eq!(options["max"].as_integer(), Some(15));
    assert_eq!(options["note"].as_str(), Some("why"));
}

#[test]
fn lint_options_are_not_unknown_keys() {
    // A lint's knobs are declared by the lint, not by the manifest format, so
    // this crate has no list to check them against and must stay quiet.
    let parsed = parse(
        r#"
        [package]
        name = "p"
        version = "0.1.0"

        [lints]
        cyclomatic-complexity = { level = "warn", max = 15, threshold = 3 }
        "#,
    );

    assert_eq!(
        warning_keys(&parsed),
        Vec::<&str>::new(),
        "lint-defined options must not be reported as unknown keys"
    );
}

#[test]
fn every_lint_level_round_trips() {
    for level in [LintLevel::Allow, LintLevel::Warn, LintLevel::Deny] {
        assert_eq!(
            LintLevel::from_name(level.as_str()),
            Some(level),
            "`{level}` should parse back to itself"
        );
    }
    assert_eq!(LintLevel::from_name("forbid"), None, "only three levels are specified");
}

#[test]
fn a_lint_table_must_say_which_level() {
    let error = parse_error(
        "[package]\nname = \"p\"\nversion = \"0.1.0\"\n\n[lints]\ncomplexity = { max = 15 }\n",
    );

    assert!(
        error.message().contains("level"),
        "the error should name the missing field, got `{}`",
        error.message()
    );
}

#[test]
fn an_unknown_lint_level_is_rejected_and_lists_the_valid_ones() {
    let error = parse_error(
        "[package]\nname = \"p\"\nversion = \"0.1.0\"\n\n[lints]\ncomplexity = \"shout\"\n",
    );

    let message = error.message();
    assert!(message.contains("shout"), "the error should quote the bad value, got `{message}`");
    assert!(message.contains("deny"), "the error should list what is accepted, got `{message}`");
    assert_eq!(
        error.location().map(|at| at.line),
        Some(6),
        "the error should point at the lint, not the top of the file"
    );
}

#[test]
fn unknown_keys_warn_but_the_manifest_still_parses() {
    // The whole point: a manifest written for a newer toolchain has to keep
    // building on an older one.
    let text = "\
[package]
name = \"p\"
version = \"0.1.0\"
license = \"MIT\"

[permissions]
gpu = [\"allow-gpu\"]

[dependencies]
\"std.effect\" = { version = \"1.0.0\", registry = \"internal\" }

[tasks.ci]
description = \"CI\"
retries = 3
";
    let parsed = parse(text);

    assert_eq!(
        warning_keys(&parsed),
        [
            "package.license",
            "permissions.gpu",
            "dependencies.\"std.effect\".registry",
            "tasks.ci.retries",
        ],
        "each unrecognized key should be reported at its own path"
    );
    assert!(
        parsed.warnings.iter().all(|w| w.kind() == WarningKind::UnknownKey),
        "these are all unknown-key warnings"
    );
    assert_eq!(
        parsed.manifest.package.name, "p",
        "the recognized part of the manifest must survive intact"
    );
    assert_eq!(parsed.manifest.dependencies["std.effect"].version, "1.0.0");
    assert_eq!(parsed.manifest.tasks["ci"].description.as_deref(), Some("CI"));

    let license = &parsed.warnings[0];
    assert_eq!(
        license.location().map(|at| (at.line, at.column)),
        Some((4, 1)),
        "a warning should point at the key that caused it"
    );
    assert_eq!(
        &text[license.span().expect("a span for a key read from the document")],
        "license",
        "the span should cover exactly the key"
    );
    assert_eq!(license.to_string(), "4:1: unrecognized key `package.license`");
}

#[test]
fn an_unknown_table_is_reported_once_not_per_key() {
    let parsed = parse(
        r#"
        [package]
        name = "p"
        version = "0.1.0"

        [registry]
        url = "https://example.invalid"
        token-file = "~/.khora/token"
        "#,
    );

    assert_eq!(
        warning_keys(&parsed),
        ["registry"],
        "reporting every key under an unknown table would bury the one useful line"
    );
}

#[test]
fn an_unknown_key_holding_a_date_still_only_warns() {
    // Dates are the one value `toml` models as a synthetic table, which costs the
    // audit its spans. Degrading to a position-free warning is fine; failing the
    // parse would not be.
    let parsed = parse("[package]\nname = \"p\"\nversion = \"0.1.0\"\nreleased = 2026-01-01\n");

    assert_eq!(warning_keys(&parsed), ["package.released"]);
    assert_eq!(parsed.manifest.package.name, "p");
}

#[test]
fn malformed_toml_reports_a_line_and_column() {
    let error = parse_error("[package\nname = \"p\"\n");

    assert_eq!(
        error.location().map(|at| (at.line, at.column)),
        Some((1, 9)),
        "the caret should sit where the table header stops being valid"
    );
    assert!(!error.message().is_empty(), "a bare error message is not actionable");
    assert_eq!(
        error.file(),
        None,
        "`parse` is given text, so it cannot know the file on its own"
    );
    assert_eq!(
        error.clone().with_file("khora.toml").to_string(),
        format!("khora.toml:1:9: {}", error.message()),
        "the caller supplies the file name and gets a full diagnostic"
    );
}

#[test]
fn a_missing_required_field_is_fatal_and_located() {
    let error = parse_error("[package]\nname = \"p\"\n");

    assert!(
        error.message().contains("version"),
        "the error should name the missing field, got `{}`",
        error.message()
    );
    assert_eq!(
        error.location().map(|at| at.line),
        Some(1),
        "the error should point at the table that is short a field"
    );
}

#[test]
fn a_known_key_with_the_wrong_type_is_fatal() {
    let error = parse_error("[package]\nname = \"p\"\nversion = \"0.1.0\"\nauthors = \"solo\"\n");

    assert!(
        error.message().contains("string"),
        "the error should say what it found, got `{}`",
        error.message()
    );
    assert_eq!(error.location().map(|at| (at.line, at.column)), Some((4, 11)));
}

#[test]
fn an_unknown_indent_style_is_fatal() {
    let error = parse_error(
        "[package]\nname = \"p\"\nversion = \"0.1.0\"\n\n[fmt]\nindent-style = \"em\"\n",
    );

    assert!(
        error.message().contains("space"),
        "the error should list the accepted styles, got `{}`",
        error.message()
    );
}

#[test]
fn fmt_settings_are_individually_optional() {
    let parsed = parse("[package]\nname = \"p\"\nversion = \"0.1.0\"\n\n[fmt]\nindent-width = 8\n");

    let fmt = parsed.manifest.fmt.expect("`[fmt]` was written, so it should be present");
    assert_eq!(fmt.indent_width, Some(8));
    assert_eq!(fmt.indent_style, None, "an unset knob keeps the formatter's own default");
    assert_eq!(fmt.explicit_semicolons, None);
}

#[test]
fn a_duplicate_key_is_fatal() {
    let error = parse_error("[package]\nname = \"p\"\nname = \"q\"\nversion = \"0.1.0\"\n");

    assert_eq!(
        error.location().map(|at| at.line),
        Some(3),
        "the second definition is the one to point at"
    );
}

#[test]
fn tasks_may_depend_on_names_the_manifest_does_not_declare() {
    // §4.1's own example depends on `lint`, `test` and `build` without declaring
    // them, so dependency names cannot be resolved at parse time.
    let parsed = parse(
        r#"
        [package]
        name = "p"
        version = "0.1.0"

        [tasks.ci]
        depends_on = ["lint", "test", "build"]
        "#,
    );

    assert_eq!(parsed.manifest.tasks["ci"].depends_on, ["lint", "test", "build"]);
    assert_eq!(parsed.manifest.tasks["ci"].description, None);
    assert!(parsed.warnings.is_empty());
}

#[test]
fn task_names_may_be_anything_a_toml_key_may_be() {
    let parsed = parse(
        r#"
        [package]
        name = "p"
        version = "0.1.0"

        [tasks."build:release"]
        description = "Release build"

        [tasks.lint]
        depends_on = []
        "#,
    );

    assert_eq!(
        parsed.manifest.tasks.keys().collect::<Vec<_>>(),
        ["build:release", "lint"],
        "task names are user-chosen keys, not a fixed set"
    );
    assert!(parsed.warnings.is_empty(), "a task name is never an unknown key");
}
