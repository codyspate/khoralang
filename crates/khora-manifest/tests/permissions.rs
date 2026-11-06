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

use khora_manifest::{
    granted_host, granted_listen, granted_name, granted_path, Category, Manifest, Permissions,
};

fn permissions(table: &str) -> Permissions {
    let text =
        format!("[package]\nname = \"p\"\nversion = \"0.1.0\"\n\n{table}");
    Manifest::parse(&text).expect("a manifest").manifest.permissions
}

fn owned(patterns: &[&str]) -> Vec<String> {
    patterns.iter().map(|p| p.to_string()).collect()
}

/// **A `network` grant ending in `:` is refused when the manifest loads.**
/// `example.com:` and `[::1]:` name no port; `std` read one as every port and
/// this crate as none, so the same grant meant two things. The refusal names
/// the entry and the spellings that do mean something.
#[test]
fn a_network_grant_with_an_empty_port_is_refused_at_load() {
    for grant in ["example.com:", "[::1]:"] {
        let text = format!(
            "[package]\nname = \"p\"\nversion = \"0.1.0\"\n\n[permissions]\nnetwork = [\"ok.example\", \"{grant}\"]\n"
        );
        let why = Manifest::parse(&text).expect_err(grant).to_string();
        let host = &grant[..grant.len() - 1];
        assert!(why.contains("permissions.network"), "{why}");
        assert!(why.contains(&format!("`{grant}`")), "names the entry: {why}");
        assert!(why.contains(&format!("`{host}`")) && why.contains(&format!("`{host}:*`")), "{why}");
    }
    // The spellings the message offers all load.
    for grant in ["example.com", "example.com:*", "example.com:443", "[::1]", "[::1]:*", "[::1]:5432"] {
        let text = format!(
            "[package]\nname = \"p\"\nversion = \"0.1.0\"\n\n[permissions]\nnetwork = [\"{grant}\"]\n"
        );
        assert!(Manifest::parse(&text).is_ok(), "`{grant}` should load");
    }
}

fn listen_manifest(entries: &str, default: &str) -> Result<Manifest, String> {
    let text = format!(
        "[package]\nname = \"p\"\nversion = \"0.1.0\"\n\n[permissions]\n{default}listen = [{entries}]\n"
    );
    Manifest::parse(&text).map(|parsed| parsed.manifest).map_err(|why| why.to_string())
}

/// **Every shape a `listen` entry may take loads**, and each is kept as
/// written: a port, a range, `0` for a port the operating system chooses,
/// and `*`.
#[test]
fn every_listen_entry_shape_loads() {
    for entry in ["8080", "8000-8099", "0", "*", "65535", "1-65535", "1024-65535"] {
        let manifest = listen_manifest(&format!("\"{entry}\""), "default = \"deny\"\n")
            .unwrap_or_else(|why| panic!("`{entry}` should load: {why}"));
        assert_eq!(
            manifest.permissions.listen.as_deref(),
            Some([entry.to_string()].as_slice()),
            "`{entry}` is kept as written"
        );
    }
}

/// The shapes a refusal offers, and the interface shapes it must not: an
/// entry naming an interface loads nowhere, so advice to write one would
/// send the reader from one refusal to the next.
fn assert_offers_only_ports(entry: &str, why: &str) {
    let advice = why.replacen(&format!("`{entry}`"), "", 1);
    for shape in ["`8080`", "`8000-8099`", "`0`", "`*`"] {
        assert!(advice.contains(shape), "`{entry}`: offers {shape}: {why}");
    }
    for shape in ["`127.0.0.1:9000`", "`[::1]:9000`", "`0.0.0.0:*`"] {
        assert!(!advice.contains(shape), "`{entry}`: does not offer {shape}: {why}");
    }
}

/// **An entry naming an interface is refused when the manifest loads**,
/// `0.0.0.0` included, under `allow` as under `deny`.
///
/// What it prevents: a grant that loads, passes `khora check`, and can never
/// grant anything. `std` binds every interface, so a listener asks about
/// `0.0.0.0:<port>` whatever the program meant, and `127.0.0.1:9000` would
/// match nothing it does. `0.0.0.0:9000` would match, and is refused too: it
/// says the same as `9000` in a shape that invites writing another address.
#[test]
fn an_interface_entry_is_refused_at_load() {
    for entry in [
        "127.0.0.1:9000",
        "127.0.0.1:*",
        "127.0.0.1:0",
        "127.0.0.1:8000-8099",
        "0.0.0.0:8080",
        "0.0.0.0:*",
        "0.0.0.0:0",
        "[::1]:9000",
        "[::1]:*",
        "[::]:*",
        "[::1]:80-90",
    ] {
        for default in ["default = \"deny\"\n", "default = \"allow\"\n", ""] {
            let why = listen_manifest(&format!("\"8080\", \"{entry}\""), default)
                .err()
                .unwrap_or_else(|| panic!("`{entry}` should be refused under `{default}`"));
            assert!(why.contains("permissions.listen"), "`{entry}`: {why}");
            assert!(why.contains(&format!("`{entry}`")), "`{entry}`: names the entry: {why}");
            assert!(why.contains("every interface"), "`{entry}`: says why: {why}");
            assert_offers_only_ports(entry, &why);
        }
    }
}

/// **An entry that does not parse is refused when the manifest loads**,
/// naming the entry and the shapes that work. Under `allow` as under
/// `deny`: an entry nobody can read is a mistake whatever the default is.
///
/// Each of these is a different way to be wrong: a host name where an
/// interface goes, a port past 65535, a range written backwards or one that
/// takes in port 0 (which is the operating system's choice, not a port), a
/// sign, an IPv6 address without its brackets, a `:` with nothing after it,
/// and `*` standing in for an interface.
#[test]
fn a_malformed_listen_entry_is_refused_at_load() {
    for entry in [
        "localhost:8080",
        "example.com",
        "65536",
        "99999",
        "9000-8000",
        "0-100",
        "+80",
        "-80",
        "::1:9000",
        "[::1]",
        "127.0.0.1:",
        "*:8080",
        "",
        "8080-",
        "1.2.3:80",
        "256.0.0.1:80",
        "01.2.3.4:80",
        "[]:80",
        "[::1]x:80",
        "80 ",
    ] {
        for default in ["default = \"deny\"\n", ""] {
            let why = listen_manifest(&format!("\"8080\", \"{entry}\""), default)
                .err()
                .unwrap_or_else(|| panic!("`{entry}` should be refused"));
            assert!(why.contains("permissions.listen"), "`{entry}`: {why}");
            assert!(why.contains(&format!("`{entry}`")), "`{entry}`: names the entry: {why}");
            assert_offers_only_ports(entry, &why);
        }
    }
}

/// **`listen` follows the rest of the table's defaults.** Under `deny` an
/// absent key grants nothing; with `allow`, or no table at all, it grants
/// everything; an empty list grants nothing under either.
#[test]
fn listen_takes_default_like_every_other_key() {
    assert!(permissions("").grants(Category::Listen), "no table grants everything");
    assert!(permissions("[permissions]\ndefault = \"allow\"\n").grants(Category::Listen));
    assert!(!permissions("[permissions]\ndefault = \"deny\"\n").grants(Category::Listen));
    assert!(!permissions("[permissions]\nlisten = []\n").grants(Category::Listen));
    assert!(permissions("[permissions]\ndefault = \"deny\"\nlisten = [\"8080\"]\n").grants(Category::Listen));
    assert!(
        !permissions("[permissions]\ndefault = \"deny\"\nnetwork = [\"*\"]\n").grants(Category::Listen),
        "`network` grants no listening"
    );
    assert!(
        permissions("[permissions]\ndefault = \"deny\"\nlisten = [\"*\"]\n").grants(Category::Listen)
            && !permissions("[permissions]\ndefault = \"deny\"\nlisten = [\"*\"]\n").grants(Category::Network),
        "and `listen` grants no dialing"
    );
    assert_eq!(Category::Listen.key(), "listen");
}

/// What `granted_listen` says about the address a program binds.
fn binds(patterns: &[&str], address: &str) -> bool {
    granted_listen(&owned(patterns), address)
}

/// **Matched as bound.** `listen_on(port)` binds every IPv4 interface, so
/// the address asked about is `0.0.0.0:<port>`: a bare port covers it, an
/// interface entry covers only its own interface, and port 0 -- a port the
/// operating system chooses -- is covered by `0` or a `*` and never by a
/// specific port or a range.
#[test]
fn a_listen_grant_is_matched_against_the_address_as_bound() {
    assert!(binds(&["8080"], "0.0.0.0:8080"));
    assert!(!binds(&["8080"], "0.0.0.0:8081"));
    assert!(binds(&["8000-8099"], "0.0.0.0:8000"));
    assert!(binds(&["8000-8099"], "0.0.0.0:8099"));
    assert!(!binds(&["8000-8099"], "0.0.0.0:8100"));
    assert!(binds(&["0.0.0.0:*"], "0.0.0.0:443"));
    assert!(!binds(&["127.0.0.1:9000"], "0.0.0.0:9000"), "another interface");
    assert!(binds(&["127.0.0.1:9000"], "127.0.0.1:9000"));
    assert!(binds(&["0"], "0.0.0.0:0"));
    assert!(!binds(&["0"], "0.0.0.0:8080"), "`0` is the OS's choice, not every port");
    assert!(!binds(&["8080", "1-65535"], "0.0.0.0:0"), "no port and no range grants port 0");
    assert!(binds(&["0.0.0.0:*"], "0.0.0.0:0"));
    assert!(binds(&["*"], "0.0.0.0:0"));
    assert!(binds(&["*"], "[::1]:9000"));
    assert!(!binds(&[], "0.0.0.0:8080"));
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

/// **`extern` is one of the unmentioned ones.** Under `deny` an absent
/// `extern` key is `[]`: `std` still may, every other package -- the root
/// included -- has to be named. With no table, or `allow`, every package may.
#[test]
fn default_deny_covers_an_absent_extern_key() {
    let strict = permissions("[permissions]\ndefault = \"deny\"\n");
    assert!(strict.may_declare_extern("std"));
    assert!(!strict.may_declare_extern("app"));

    let listed = permissions("[permissions]\ndefault = \"deny\"\nextern = [\"app\"]\n");
    assert!(listed.may_declare_extern("app"));
    assert!(!listed.may_declare_extern("other"));

    assert!(permissions("").may_declare_extern("app"));
    assert!(permissions("[permissions]\ndefault = \"allow\"\n").may_declare_extern("app"));
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
#[cfg(windows)]
#[test]
fn a_grant_written_with_slashes_covers_a_windows_path() {
    assert!(path(&["./tmp/**"], ".\\tmp\\a.txt"));
    assert!(path(&["C:/data/*.json"], "C:\\data\\users.json"));
}

/// **Off Windows a backslash is part of a name, not a separator.**
/// `granted\evil` is a directory beside `granted`, and reading the `\` as a
/// `/` let `granted/**` cover a tree the manifest never named.
#[cfg(not(windows))]
#[test]
fn a_backslash_is_part_of_a_name_off_windows() {
    assert!(!path(&["granted/**"], "granted\\evil/f.txt"));
    assert!(!path(&["./tmp/**"], ".\\tmp\\a.txt"));
    assert!(path(&["granted/**"], "granted/a\\b.txt"), "a name with a backslash, inside the grant");
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

/// **A `..` segment is refused unless the grants are the whole filesystem.**
///
/// `**` crosses separators, so the `./logs/**` the capabilities guide teaches
/// matched `logs/../secret.txt` as text and a grant naming one directory
/// covered the disk. Resolving the `..` away instead would be unsound from the
/// string alone -- where `logs` is a symlink, `logs/..` is the parent of its
/// target -- so the path is not compared at all.
///
/// Bare `**` is the exception, because it is not a restriction there is an
/// outside to: `default = "allow"` and a manifest with no `[permissions]`
/// table both compile to exactly that list.
///
/// The same table as `std::permissions`'s own test, which lives in
/// `crates/khora-codegen-llvm/tests/fs.rs`.
#[test]
fn a_parent_segment_is_refused_however_wide_the_grant() {
    for (grant, value, want) in [
        // The escape, against the grant the docs teach.
        ("./logs/**", "logs/../secret.txt", false),
        // The same grant still covers what is really under the tree.
        ("./logs/**", "logs/a/b.txt", true),
        // Leading, and doubled, against a grant that names everything.
        ("**", "../secret.txt", true),
        ("**", "a/../../b", true),
        // Buried, so this is not just a test of the first segment.
        ("./logs/**", "logs/deep/../../secret.txt", false),
        // Separators are leveled first, so this is the same path again.
        ("./logs/**", "logs\\..\\secret.txt", false),
        // A segment and not a substring: `..config` is a filename.
        ("./logs/**", "logs/..config", true),
        // The literal grant refused this before the fix, and still does.
        ("./logs", "logs/../secret.txt", false),
    ] {
        assert_eq!(
            granted_path(&[grant.to_string()], value),
            want,
            "granted_path({grant:?}, {value:?})"
        );
    }
}
