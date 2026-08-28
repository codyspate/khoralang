//! What `[permissions]` grants, and what a wildcard covers.
//!
//! `docs/design/permissions.md` decides D4. Two halves: the manifest says which
//! capabilities a program may *hold*, checked when it is compiled, and the
//! grants say what may be done with one, checked where the access happens.
//! This is the second half's matching rules and the first half's default.
//!
//! There are three matchers rather than one, because a path and a hostname do
//! not have the same structure and one rule for both would be surprising in
//! whichever direction it was bent.

use khora_manifest::{granted_host, granted_name, granted_path, Category, Manifest, Permissions};

fn permissions(table: &str) -> Permissions {
    let text =
        format!("[package]\nname = \"p\"\nversion = \"0.1.0\"\nedition = \"2026\"\n\n{table}");
    Manifest::parse(&text).expect("a manifest").manifest.permissions
}

fn owned(patterns: &[&str]) -> Vec<String> {
    patterns.iter().map(|p| p.to_string()).collect()
}

fn host(patterns: &[&str], value: &str) -> bool {
    granted_host(&owned(patterns), value)
}

fn path(patterns: &[&str], value: &str) -> bool {
    granted_path(&owned(patterns), value)
}

fn name(patterns: &[&str], value: &str) -> bool {
    granted_name(&owned(patterns), value)
}

// --- the default -----------------------------------------------------------

/// **No barrier to entry.** A program that has never heard of permissions
/// compiles: tightening is opt-in, the same bargain Rust makes with `unsafe`.
#[test]
fn a_missing_table_grants_everything() {
    let none = permissions("");
    for category in [Category::Fs, Category::Network, Category::Env] {
        assert!(none.grants(category), "{} should be granted", category.key());
    }
}

/// Naming one category says nothing about the others. A rule that locked
/// everything down the moment you got careful about one thing would punish the
/// first step towards being careful.
#[test]
fn categories_are_independent() {
    let some = permissions("[permissions]\nnetwork = [\"api.example.com:443\"]\n");
    assert!(some.grants(Category::Network));
    assert!(some.grants(Category::Fs), "an unmentioned category is unrestricted");
    assert!(some.grants(Category::Env));
}

/// An empty list is a category named and then refused, which is different from
/// one never mentioned.
#[test]
fn an_empty_list_grants_nothing() {
    let denied = permissions("[permissions]\nnetwork = []\n");
    assert!(!denied.grants(Category::Network));
    assert!(denied.grants(Category::Fs));
}

/// One line for the strict posture: set once, in CI, and forgotten.
#[test]
fn default_deny_flips_the_unmentioned_ones() {
    let strict =
        permissions("[permissions]\ndefault = \"deny\"\nnetwork = [\"api.example.com:443\"]\n");
    assert!(strict.grants(Category::Network), "what was named is still granted");
    assert!(!strict.grants(Category::Fs));
    assert!(!strict.grants(Category::Env));
}

/// Reading and writing are not the same grant, and either one alone means the
/// capability is held.
#[test]
fn reading_and_writing_are_separate_grants() {
    let read_only = permissions("[permissions.fs]\nread = [\"./data/**\"]\n");
    assert!(read_only.grants(Category::Fs));
    let fs = read_only.fs.expect("granted");
    assert!(granted_path(&fs.read, "./data/a.json"));
    assert!(fs.write.is_empty(), "nothing may be written");
}

// --- hosts -----------------------------------------------------------------

/// `"*"` is how you say "yes, all of it" out loud — worth writing when you want
/// the reader of the manifest to know the question was asked and answered.
#[test]
fn a_bare_star_covers_any_host() {
    assert!(host(&["*"], "api.example.com"));
    assert!(host(&["*"], "api.example.com:443"));
    assert!(host(&["*"], "localhost:8080"));
    assert!(host(&["*"], "10.0.0.1:5432"));
}

/// **A `*` in a host spans dots.** This is what a Content-Security-Policy
/// origin means by it, and surprising somebody into a denied connection is a
/// worse failure than covering a subdomain they did not enumerate. The
/// one-label reading belongs to TLS certificates.
#[test]
fn a_star_in_a_host_spans_dots() {
    assert!(host(&["*.internal:5432"], "db.internal:5432"));
    assert!(host(&["*.internal:5432"], "db.eu.internal:5432"), "any depth of subdomain");
    assert!(!host(&["*.internal:5432"], "internal:5432"), "the literal dot has to be there");
    assert!(!host(&["*.internal:5432"], "db.external:5432"));
    assert!(!host(&["*.internal:5432"], "db.internal:5433"), "the port is still exact");
}

/// A port on its own, which is the common shape for a local service.
#[test]
fn a_star_covers_a_port() {
    assert!(host(&["localhost:*"], "localhost:8080"));
    assert!(host(&["localhost:*"], "localhost:3000"));
    assert!(!host(&["localhost:*"], "example.com:8080"));

    assert!(host(&["*:443"], "api.example.com:443"));
    assert!(!host(&["*:443"], "api.example.com:80"));
}

/// **A grant with no port covers every port**, which is what
/// `--allow-net=example.com` does in Deno and what somebody writing a hostname
/// almost always means.
#[test]
fn a_grant_without_a_port_covers_any_port() {
    assert!(host(&["api.example.com"], "api.example.com:443"));
    assert!(host(&["api.example.com"], "api.example.com:8080"));
    assert!(host(&["api.example.com"], "api.example.com"));
    assert!(!host(&["api.example.com"], "other.example.com:443"));
}

#[test]
fn an_exact_host_is_exact() {
    assert!(host(&["0.0.0.0:8080"], "0.0.0.0:8080"));
    assert!(!host(&["0.0.0.0:8080"], "0.0.0.0:8081"));
    assert!(!host(&["0.0.0.0:8080"], "127.0.0.1:8080"));
}

// --- paths -----------------------------------------------------------------

/// In a path `*` stops at a separator, which is what makes `./data/*.json`
/// mean the files in that directory rather than everything under it.
#[test]
fn a_star_in_a_path_stops_at_a_separator() {
    assert!(path(&["./data/*.json"], "./data/users.json"));
    assert!(!path(&["./data/*.json"], "./data/users.yaml"));
    assert!(!path(&["./data/*.json"], "./data/nested/users.json"));
}

/// `**` is the one that crosses, which is what "this directory and everything
/// under it" needs.
#[test]
fn a_double_star_crosses_separators() {
    assert!(path(&["./tmp/**"], "./tmp/a.txt"));
    assert!(path(&["./tmp/**"], "./tmp/deep/nested/a.txt"));
    assert!(!path(&["./tmp/**"], "./other/a.txt"));

    assert!(path(&["/etc/myapp/**"], "/etc/myapp/conf.d/main.toml"));
    assert!(!path(&["/etc/myapp/**"], "/etc/other/main.toml"));
}

#[test]
fn the_two_wildcards_compose() {
    assert!(path(&["./data/**/*.json"], "./data/nested/users.json"));
    assert!(path(&["./data/**/*.json"], "./data/a/b/c/users.json"));
    assert!(!path(&["./data/**/*.json"], "./data/a/b/users.yaml"));
}

/// A grant is written once and covers a Windows path too. Nobody should have
/// to write the manifest twice.
#[test]
fn a_grant_written_with_slashes_covers_a_windows_path() {
    assert!(path(&["./tmp/**"], ".\\tmp\\a.txt"));
    assert!(path(&["C:/data/*.json"], "C:\\data\\users.json"));
}

// --- names -----------------------------------------------------------------

/// A variable name has no structure to respect, so `*` matches any run.
#[test]
fn a_trailing_star_is_a_prefix() {
    assert!(name(&["DB_*"], "DB_URL"));
    assert!(name(&["DB_*"], "DB_PASSWORD"));
    assert!(name(&["DB_*"], "DB_"), "the empty remainder still matches");
    assert!(!name(&["DB_*"], "REDIS_URL"));
}

#[test]
fn a_literal_grant_is_exact_and_several_are_a_union() {
    assert!(name(&["DB_URL"], "DB_URL"));
    assert!(!name(&["DB_URL"], "DB_URL_2"));
    assert!(!name(&["DB_URL"], "DB_UR"));

    assert!(name(&["A", "B", "C_*"], "B"));
    assert!(name(&["A", "B", "C_*"], "C_1"));
    assert!(!name(&["A", "B", "C_*"], "D"));
}

/// Nothing granted covers nothing, which is what an empty list has to mean for
/// `default = "deny"` to be worth anything.
#[test]
fn no_grants_cover_nothing() {
    assert!(!granted_name(&[], "anything"));
    assert!(!granted_path(&[], "./a"));
    assert!(!granted_host(&[], "example.com:443"));
}

// --- the table `std::permissions` is checked against ------------------------
//
// **Two matchers answer this question and they have to agree.** This one
// decides whether a manifest satisfies `[workspace.policy]`; the one in
// `std/permissions.kh` decides whether a running program may touch a path, a
// variable or a host. A disagreement means a build that passes and a program
// that refuses, or worse the other way round.
//
// The table is duplicated rather than shared, because the two are in different
// languages and a generated fixture would be a third thing to keep true. What
// keeps them honest is that both are written out and both are run: the Khora
// side lives in `std::permissions`'s own tests and asserts the same answers.

#[test]
fn names_span_everything_because_they_have_no_segments() {
    for (grant, value, want) in [
        ("*", "DATABASE_URL", true),
        ("DATABASE_*", "DATABASE_URL", true),
        ("DATABASE_*", "OTHER_URL", false),
        ("PORT", "PORT", true),
        ("PORT", "PORTAL", false),
        ("*_URL", "DATABASE_URL", true),
    ] {
        assert_eq!(
            granted_name(&[grant.to_string()], value),
            want,
            "granted_name({grant:?}, {value:?})"
        );
    }
}

#[test]
fn a_host_grant_spans_dots_and_a_missing_port_is_every_port() {
    for (grant, value, want) in [
        ("example.com", "example.com", true),
        ("example.com", "example.com:443", true),
        ("example.com:443", "example.com:443", true),
        ("example.com:443", "example.com:80", false),
        ("example.com:*", "example.com:80", true),
        ("*.internal", "db.eu.internal", true),
        ("*.internal", "db.internal", true),
        ("*.internal", "elsewhere.com", false),
        ("*", "anything.at.all:9000", true),
    ] {
        assert_eq!(
            granted_host(&[grant.to_string()], value),
            want,
            "granted_host({grant:?}, {value:?})"
        );
    }
}

#[test]
fn a_path_grant_keeps_its_segment_rule() {
    for (grant, value, want) in [
        ("data/*", "data/a.txt", true),
        ("data/*", "data/deep/a.txt", false),
        ("data/**", "data/deep/a.txt", true),
        // `data/**` is what is *inside* `data`, which is `.gitignore`'s reading
        // and the one `std::permissions` was changed to match.
        ("data/**", "data", false),
    ] {
        assert_eq!(
            granted_path(&[grant.to_string()], value),
            want,
            "granted_path({grant:?}, {value:?})"
        );
    }
}
