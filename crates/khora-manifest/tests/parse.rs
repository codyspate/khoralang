//! End-to-end tests for `khora.toml` parsing.

use khora_manifest::{IndentStyle, LintLevel, Manifest, ManifestError, Parsed, WarningKind};

/// The manifest from `docs/project.md` §4.1, as an actual project ships it.
///
/// Read from the example rather than copied, so that a change to the format
/// that forgets this crate fails here instead of silently diverging.
///
/// **By path rather than `include_str!`**, because the example is a workspace
/// member: it takes its version and its `[fmt]` table from the
/// root, and text alone cannot find a root. That is the whole of what
/// `Manifest::load` is for. Roadmap 14.14.
const EXAMPLE: &str =
    concat!(env!("CARGO_MANIFEST_DIR"), "/../../examples/risk_analyzer/khora.toml");

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
    let parsed = Manifest::load(std::path::Path::new(EXAMPLE))
        .expect("the reference manifest should load");

    assert_eq!(
        warning_keys(&parsed),
        Vec::<&str>::new(),
        "every key in the reference manifest should be recognized"
    );

    let manifest = parsed.manifest;
    assert_eq!(manifest.package().expect("a package").name, "risk_analyzer");
    assert_eq!(manifest.package().expect("a package").version, "0.1.0");
    assert_eq!(manifest.package().expect("a package").authors, ["Engineering Team <dev@khora.internal>"]);

    assert_eq!(
        manifest.permissions.network.as_deref(),
        Some(["0.0.0.0:47821".to_string(), "*.internal:5432".to_string()].as_slice()),
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

    assert!(
        manifest.dependencies.is_empty(),
        "`std` is found beside the compiler rather than declared, so the reference \
         application depends on nothing yet"
    );

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
        publish = true

        [permissions]
        network = []
        env = []

        [fmt]
        indent-style = "tab"
        indent-width = 4

        [lints]
        bare = "allow"
        tabled = { level = "deny" }

        [dependencies]
        "std.effect" = { version = "1.0.0" }
        inner = { git = "https://example.com/z.git", rev = "abc", subdir = "packages/inner" }

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
    assert_eq!(manifest.package().expect("a package").name, "p");
    assert!(manifest.package().expect("a package").authors.is_empty(), "`authors` should default to empty");
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
        parsed.manifest.package().expect("a package").name, "p",
        "the recognized part of the manifest must survive intact"
    );
    assert_eq!(parsed.manifest.dependencies["std.effect"].version.as_deref(), Some("1.0.0"));
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
    assert_eq!(parsed.manifest.package().expect("a package").name, "p");
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
}

#[test]
fn a_key_this_toolchain_removed_says_so_and_says_what_to_do() {
    // Not "unrecognized key". That reads as "your toolchain is too old", which
    // is the opposite of the truth, and it is what somebody would be told
    // about a line `docs/project.md` §4.1 asked them to write. Roadmap 14.20b.
    let parsed = parse(
        "[package]\nname = \"p\"\nversion = \"0.1.0\"\n\n[fmt]\nexplicit-semicolons = true\n",
    );

    let warning = match parsed.warnings.as_slice() {
        [only] => only,
        other => panic!("expected exactly one warning, got {other:?}"),
    };
    assert_eq!(warning.kind(), WarningKind::RemovedKey);
    assert_eq!(warning.key(), "fmt.explicit-semicolons");
    assert!(warning.note().is_some_and(|note| note.contains("grammar")), "{warning}");
    assert!(warning.to_string().contains("Delete the line"), "{warning}");
}

#[test]
fn a_removed_key_is_not_fatal() {
    // An older manifest still builds. The whole reason the audit warns rather
    // than erroring is that a manifest outliving a toolchain version is normal.
    let parsed = parse(
        "[package]\nname = \"p\"\nversion = \"0.1.0\"\n\n\
         [fmt]\nindent-width = 4\nexplicit-semicolons = true\n",
    );
    let fmt = parsed.manifest.fmt.expect("the rest of the table still reads");
    assert_eq!(fmt.indent_width, Some(4));
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

/// A dotted name in quotes is one key, not a tree of tables. TOML will happily
/// read `[dependencies.std.net.http]` as three nested tables, and a dependency
/// called `std.net.http` is one thing.
#[test]
fn a_quoted_dotted_dependency_is_one_key() {
    let parsed = Manifest::parse(
        "[package]
name = \"p\"
version = \"0.1.0\"

[dependencies]
\"acme.json\" = { version = \"1.0.0\" }
\"acme.http\" = { path = \"../http\" }
",
    )
    .expect("a manifest");
    let deps = parsed.manifest.dependencies;
    assert_eq!(deps.keys().collect::<Vec<_>>(), ["acme.http", "acme.json"]);
    assert_eq!(deps["acme.json"].version.as_deref(), Some("1.0.0"));
    assert_eq!(deps["acme.json"].path, None);
    assert_eq!(deps["acme.http"].path.as_deref(), Some("../http"));
    assert!(deps.values().all(|d| d.is_located()));
}

/// A dependency that says neither where nor which version parses, and resolves
/// to nothing. Worth naming rather than discovering at link time.
#[test]
fn a_dependency_with_no_source_is_not_located() {
    let parsed = Manifest::parse(
        "[package]
name = \"p\"
version = \"0.1.0\"

[dependencies]
\"acme.json\" = {}
",
    )
    .expect("a manifest");
    assert!(!parsed.manifest.dependencies["acme.json"].is_located());
}

/// Every key the model reads is a key the audit knows.
///
/// These two lists are maintained by hand in different files, and the failure
/// when they drift is quiet: a manifest using a real feature is told the key is
/// unrecognized, which reads as "this does nothing". Three keys had already
/// drifted — `permissions.extern`, and a dependency's `git` and `rev` — before
/// anything checked.
#[test]
fn every_supported_key_is_recognized_by_the_audit() {
    let text = r#"
[package]
name = "a"
version = "0.1.0"
authors = ["Someone <s@example.com>"]

[toolchain]
version = "0.1.0"

[permissions]
default = "deny"
network = ["*"]
env = ["HOME"]
extern = ["std"]

[permissions.fs]
read = ["/tmp"]
write = ["/tmp"]

[fmt]
indent-style = "space"
indent-width = 2

[lints]
dangling-expression = "warn"

[dependencies]
by_path = { path = "../other" }
by_git = { git = "https://example.com/x.git", rev = "abc" }
by_subdir = { git = "https://example.com/z.git", rev = "abc", subdir = "packages/inner" }
by_tag = { git = "https://example.com/y.git", tag = "v1" }

[build]
target = "x86_64-unknown-linux-musl"
plugin = "protobuf-compiler@2.1"

[tasks.ci]
description = "everything"
depends_on = ["test"]
"#;
    let parsed = Manifest::parse(text).expect("a well-formed manifest");
    assert!(
        parsed.warnings.is_empty(),
        "these are all real keys: {:?}",
        parsed.warnings.iter().map(ToString::to_string).collect::<Vec<_>>()
    );
}

/// The audit still has to catch a genuine typo, or the test above could be
/// satisfied by an audit that warns about nothing.
///
/// **The typo used to be `toolchain.verison`, and that is now an error rather
/// than a warning** -- a misspelled `version` leaves `[toolchain]` without the
/// field it requires, so the manifest does not parse at all and never reaches
/// the audit. Which is the better outcome for that key, and no use as a test of
/// the audit. `package.authros` is a key nothing requires, so it still gets
/// there.
#[test]
fn a_misspelled_key_is_still_caught() {
    let text = "[package]\nname = \"a\"\nversion = \"0.1.0\"\nauthros = []\n";
    let parsed = Manifest::parse(text).expect("it parses");
    assert_eq!(parsed.warnings.len(), 1, "{:?}", parsed.warnings);
    assert!(parsed.warnings[0].to_string().contains("package.authros"), "{:?}", parsed.warnings);
}

/// **A misspelled `version` under `[toolchain]` is an error**, and worth its
/// own test now that it is. The table has one required field; misspelling it
/// means the table does not have it.
#[test]
fn a_misspelled_pin_is_an_error_rather_than_a_warning() {
    let text = "[package]\nname = \"a\"\nversion = \"0.1.0\"\n\n\
                [toolchain]\nverison = \"0.1.0\"\n";
    let why = parse_error(text).to_string();
    assert!(why.contains("version"), "{why}");
}

#[test]
fn the_toolchain_version_is_read() {
    let text = "[package]\nname = \"a\"\nversion = \"0.1.0\"\n\n\
                [toolchain]\nversion = \"0.2.0\"\n";
    let parsed = Manifest::parse(text).expect("a well-formed manifest");
    assert_eq!(parsed.manifest.toolchain.expect("a pin").version, "0.2.0");
}

/// `publish` has three states and only one of them is yes, which is the point:
/// absent is a no, because publishing here is passive -- pushing a repository
/// makes it fetchable, so the active choice is the one written down.
#[test]
fn publish_is_absent_true_or_false() {
    let absent = parse("[package]\nname = \"p\"\nversion = \"0.1.0\"\n");
    assert_eq!(absent.manifest.package().expect("a package").publish, None);

    let yes = parse("[package]\nname = \"p\"\nversion = \"0.1.0\"\npublish = true\n");
    assert_eq!(yes.manifest.package().expect("a package").publish, Some(true));

    let no = parse("[package]\nname = \"p\"\nversion = \"0.1.0\"\npublish = false\n");
    assert_eq!(no.manifest.package().expect("a package").publish, Some(false));
}

/// A git URL names a repository, and a repository is not a package.
#[test]
fn a_git_dependency_may_name_a_subdirectory() {
    let parsed = parse(
        "[package]\nname = \"p\"\nversion = \"0.1.0\"\n\n\
         [dependencies]\n\
         inner = { git = \"https://example.com/z.git\", rev = \"abc\", subdir = \"packages/inner\" }\n\
         outer = { git = \"https://example.com/z.git\", rev = \"abc\" }\n",
    );
    assert_eq!(warning_keys(&parsed), Vec::<&str>::new());
    let deps = &parsed.manifest.dependencies;
    assert_eq!(deps["inner"].subdir.as_deref(), Some("packages/inner"));
    assert_eq!(deps["outer"].subdir, None);
}

/// **`[permission.fs]` is one letter short and turns the sandbox off.**
///
/// A manifest with no `[permissions]` table grants everything, so a misspelled
/// one reads exactly like a program that was never sandboxed — while its author
/// believes they wrote a sandbox. What they got was `unrecognized key
/// permission` and an exit status of 0.
///
/// The warning still does not fail the build, which is deliberate and is why
/// the sentence has to carry the weight: the audit warns rather than errors so
/// that a manifest written against a newer toolchain stays buildable by an
/// older one, and that reason does not stop applying because this particular
/// key is important.
#[test]
fn a_misspelled_permissions_table_says_what_it_costs() {
    let parsed = parse(
        r#"
[package]
name = "app"
version = "0.1.0"

[permission.fs]
read = ["./data/**"]
"#,
    );

    let said = parsed.warnings[0].to_string();
    assert!(said.contains("did you mean `permissions`?"), "{said}");
    assert!(said.contains("running unsandboxed"), "{said}");
    assert_eq!(parsed.warnings[0].suggestion(), Some("permissions"));
}

/// Every other key gets the suggestion and not the sermon.
///
/// `[permissions]` is the one place where being ignored is not the same as
/// being left at a default, so it is the one place with a second sentence.
#[test]
fn another_misspelled_table_gets_only_the_suggestion() {
    let parsed = parse(
        r#"
[package]
name = "app"
version = "0.1.0"

[fmtt]
indent-width = 2
"#,
    );

    let said = parsed.warnings[0].to_string();
    assert!(said.contains("did you mean `fmt`?"), "{said}");
    assert!(!said.contains("unsandboxed"), "{said}");
}

/// And a key that is not a misspelling of anything says so and stops.
///
/// One edit away is the whole of what the suggestion is for — a dropped or
/// doubled letter, a transposition, a typed-over one. Anything further is a
/// different key rather than a misspelling of this one, and a guess at it would
/// be worse than nothing: it would send somebody to rename a line that was
/// never meant to be that key.
#[test]
fn a_key_that_resembles_nothing_is_reported_without_a_guess() {
    let parsed = parse(
        r#"
[package]
name = "app"
version = "0.1.0"

[bananas]
count = 2
"#,
    );

    let said = parsed.warnings[0].to_string();
    assert!(said.contains("unrecognized key `bananas`"), "{said}");
    assert!(!said.contains("did you mean"), "{said}");
    assert_eq!(parsed.warnings[0].suggestion(), None);
}

/// **The package-name rule was `khora new`'s and nobody else's.**
///
/// A package name is a module path segment — `import app::main` — so it is an
/// identifier, and a name that is not one cannot be written in the language
/// that is supposed to import it. The scaffold checked the directory name and
/// said so clearly; a manifest written or edited by hand went through, and the
/// first sign was whatever the name broke downstream.
///
/// `new` is one of the ways a manifest comes to exist. Hand-editing is the
/// other, and it was the unchecked one.
#[test]
fn a_hyphenated_package_name_is_refused() {
    let why = parse_error("[package]\nname = \"has-a-hyphen\"\nversion = \"0.1.0\"\n").to_string();
    assert!(why.contains("package.name"), "{why}");
    assert!(why.contains("identifiers"), "{why}");
    // And it says what the name would have to be.
    assert!(why.contains("has_a_hyphen"), "the message should suggest a spelling: {why}");
}

/// A digit cannot start an identifier either, and the message says why in the
/// terms the reader will meet it: `import 1st::main` does not read.
#[test]
fn a_package_name_starting_with_a_digit_is_refused() {
    let why = parse_error("[package]\nname = \"1st\"\nversion = \"0.1.0\"\n").to_string();
    assert!(why.contains("starts with a digit"), "{why}");
}

#[test]
fn an_ordinary_package_name_is_accepted() {
    let parsed = parse("[package]\nname = \"has_an_underscore\"\nversion = \"0.1.0\"\n");
    assert_eq!(parsed.manifest.package().map(|p| p.name.as_str()), Some("has_an_underscore"));
}
