//! The permission questions both matchers have to answer the same way.
//!
//! `std/permissions.kh` decides what a *running program* may touch;
//! `khora_manifest`'s `granted_path`, `granted_name` and `granted_host` decide
//! the same thing for the *compiler*. Two matchers, one contract, written
//! twice in two languages -- and they have diverged twice, on `..` and on `.`,
//! both times found by somebody reading the two files side by side rather than
//! by a test.
//!
//! So the cases live here, once, and both sides are driven from this list:
//!
//! - `crates/khora-manifest/tests/permissions_agree.rs` calls the Rust
//!   functions directly.
//! - `crates/khora-codegen-llvm/tests/agreement.rs` compiles a Khora program
//!   generated from this same list, runs it, and compares its transcript to
//!   what the Rust side said.
//!
//! **A case cannot be added to one side only**, which is the entire point: the
//! file is `#[path]`-included by both, so the two suites cannot drift apart in
//! coverage the way the two matchers drifted apart in behaviour.
//!
//! Adding a case is one entry in [`CASES`]. Adding a *pair* -- another
//! contract with two implementations -- is described in
//! `scripts/check-agreement.sh`.

/// Which of the three matchers a case is about.
///
/// They are not variations of one function: `Path` stops a single `*` at a
/// separator and refuses a `..` segment, `Name` lets `*` span everything, and
/// `Host` splits a trailing port off first. A case names the one it means, so
/// nothing can be silently checked against the wrong matcher.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    /// `granted_path` in `khora-manifest`, `granted` in `std::permissions`.
    Path,
    /// `granted_name` on both sides: an environment variable, a command.
    Name,
    /// `granted_host` on both sides: `name` or `name:port`.
    Host,
}

/// One question, and the answer both implementations owe.
pub struct Case {
    pub kind: Kind,
    /// The grants, as a manifest would list them.
    pub grants: &'static [&'static str],
    /// The path, name or host being asked about.
    pub subject: &'static str,
    /// What both sides must say.
    pub granted: bool,
    /// Why this case is here. Printed when it fails, so a failure names the
    /// rule that broke rather than a row number.
    pub why: &'static str,
}

use Kind::{Host, Name, Path};

/// The table.
///
/// Ordinary cases are here to pin the behaviour everything relies on; the rest
/// are the corners where the two matchers have actually disagreed, or where
/// agreeing is not obvious from either file alone.
pub const CASES: &[Case] = &[
    // -- `*` against `**`, and the separator between them ---------------------
    Case {
        kind: Path,
        grants: &["data/*"],
        subject: "data/a.txt",
        granted: true,
        why: "a single `*` covers a whole segment",
    },
    Case {
        kind: Path,
        grants: &["data/*"],
        subject: "data/nested/a.txt",
        granted: false,
        why: "a single `*` stops at a separator",
    },
    Case {
        kind: Path,
        grants: &["data/**"],
        subject: "data/nested/a.txt",
        granted: true,
        why: "`**` crosses separators",
    },
    Case {
        kind: Path,
        grants: &["data/**"],
        subject: "data",
        granted: false,
        why: "`data/**` is what is inside `data`, not `data` itself -- \
              `.gitignore`'s reading, and the reason the separator after a `**` \
              is not eaten",
    },
    Case {
        kind: Path,
        grants: &["**"],
        subject: "anywhere/at/all.txt",
        granted: true,
        why: "bare `**` is the whole filesystem",
    },
    Case {
        kind: Path,
        grants: &["logs/**", "data/*"],
        subject: "data/a.txt",
        granted: true,
        why: "several grants, any one of which may answer yes",
    },
    Case {
        kind: Path,
        grants: &[],
        subject: "data/a.txt",
        granted: false,
        why: "no grants is no",
    },
    // -- `..`, which is where the two diverged the first time -----------------
    Case {
        kind: Path,
        grants: &["./logs/**"],
        subject: "logs/../secret.txt",
        granted: false,
        why: "the escape the probe package reproduced: `**` spans separators, so \
              a grant that reads like one directory would otherwise cover the disk",
    },
    Case {
        kind: Path,
        grants: &["./logs/**"],
        subject: "logs/deep/../../secret.txt",
        granted: false,
        why: "buried, so this is not only a test of the first segment",
    },
    Case {
        kind: Path,
        grants: &["**"],
        subject: "../secret.txt",
        granted: true,
        why: "bare `**` is the absence of a restriction rather than a wide one -- \
              it is what `default = \"allow\"` and a missing `[permissions]` table \
              both compile to, and there is no outside for a `..` to reach",
    },
    Case {
        kind: Path,
        grants: &["**"],
        subject: "a/../../b",
        granted: true,
        why: "the same, doubled and not leading",
    },
    Case {
        kind: Path,
        grants: &["logs/**", "**"],
        subject: "../secret.txt",
        granted: true,
        why: "`unrestricted` asks whether *any* grant is bare `**`, not the first",
    },
    Case {
        kind: Path,
        grants: &["./logs"],
        subject: "logs/../secret.txt",
        granted: false,
        why: "a literal grant refused this before the fix and still does",
    },
    Case {
        kind: Path,
        grants: &["./logs/**"],
        subject: "logs/..config",
        granted: true,
        why: "a segment and not a substring: `..config` is an honest filename",
    },
    Case {
        kind: Path,
        grants: &["./logs/**"],
        subject: "logs/..",
        granted: false,
        why: "a trailing `..` is still a segment",
    },
    // -- `.` segments, which is where they diverged the second time -----------
    Case {
        kind: Path,
        grants: &["./data/**"],
        subject: "data/foo.txt",
        granted: true,
        why: "the `./data/**` the capabilities guide teaches has to cover the \
              path a program actually opens -- two readers wrote it into a \
              manifest and were refused by a grant that looks like it says yes",
    },
    Case {
        kind: Path,
        grants: &["data/**"],
        subject: "./data/foo.txt",
        granted: true,
        why: "and the same the other way round",
    },
    Case {
        kind: Path,
        grants: &["data/**"],
        subject: "data/./a/./b.txt",
        granted: true,
        why: "rebuilt from its pieces rather than by replacing `/./`, which \
              leaves `a/././b` half done",
    },
    Case {
        kind: Path,
        grants: &["."],
        subject: ".",
        granted: true,
        why: "a path that is nothing but `.` names the current directory and \
              stays `.`; the empty string would match nothing",
    },
    // -- separators -----------------------------------------------------------
    Case {
        kind: Path,
        grants: &["data/**"],
        subject: "data\\nested\\a.txt",
        granted: true,
        why: "a grant written with `/` covers a path Windows spelled with `\\`",
    },
    Case {
        kind: Path,
        grants: &["data\\**"],
        subject: "data/nested/a.txt",
        granted: true,
        why: "and a grant spelled the Windows way covers a POSIX path",
    },
    Case {
        kind: Path,
        grants: &["./logs/**"],
        subject: "logs\\..\\secret.txt",
        granted: false,
        why: "separators are levelled before the `..` is looked for, so this is \
              the escape again in another spelling",
    },
    // -- names: a `*` with nothing to stop at ---------------------------------
    Case {
        kind: Name,
        grants: &["DATABASE_*"],
        subject: "DATABASE_URL",
        granted: true,
        why: "the shape almost every env grant takes",
    },
    Case {
        kind: Name,
        grants: &["*"],
        subject: "ANYTHING",
        granted: true,
        why: "`*` in a name covers all of them",
    },
    Case {
        kind: Name,
        grants: &["DATABASE_*"],
        subject: "DB_URL",
        granted: false,
        why: "and does not cover what it does not name",
    },
    Case {
        kind: Name,
        grants: &["*"],
        subject: "PATH/WITH/SLASHES",
        granted: true,
        why: "**a name has no segments**, so `*` spans a `/` here where it would \
              stop at one in a path. That is the difference between the two \
              matchers, so it is asserted rather than assumed",
    },
    Case {
        kind: Name,
        grants: &[],
        subject: "HOME",
        granted: false,
        why: "no grants is no",
    },
    Case {
        kind: Name,
        grants: &["ls"],
        subject: "ls",
        granted: true,
        why: "a literal command grant",
    },
    // -- hosts: `*` spans dots, and a missing port means every port -----------
    Case {
        kind: Host,
        grants: &["*.internal"],
        subject: "db.eu.internal",
        granted: true,
        why: "`*` in a host spans dots, which is what a Content-Security-Policy \
              origin means by it; the one-label reading belongs to TLS \
              certificates",
    },
    Case {
        kind: Host,
        grants: &["api.example.com"],
        subject: "api.example.com:443",
        granted: true,
        why: "a grant with no port covers every port -- what Deno's \
              `--allow-net=example.com` does",
    },
    Case {
        kind: Host,
        grants: &["api.example.com:443"],
        subject: "api.example.com:8443",
        granted: false,
        why: "a grant that names a port means that port",
    },
    Case {
        kind: Host,
        grants: &["api.example.com:*"],
        subject: "api.example.com:8443",
        granted: true,
        why: "`:*` says explicitly what a bare host says implicitly",
    },
    Case {
        kind: Host,
        grants: &["api.example.com:443"],
        subject: "api.example.com",
        granted: false,
        why: "the host asked about has no port, so it is not the one named",
    },
    Case {
        kind: Host,
        grants: &["*.internal:5432"],
        subject: "db.eu.internal:5432",
        granted: true,
        why: "a wildcard name and an exact port together",
    },
    Case {
        kind: Host,
        grants: &["example.com"],
        subject: "evil-example.com",
        granted: false,
        why: "a literal grant is anchored at both ends",
    },
    Case {
        kind: Host,
        grants: &[],
        subject: "example.com",
        granted: false,
        why: "no grants is no",
    },
];

/// What `khora_manifest` says about one case.
///
/// The Rust half of the comparison, in the shared file rather than in the
/// `khora-manifest` test, because the Khora half needs the same answers to
/// compare its transcript against. One caller would have been enough reason to
/// leave it where it was used; two is the reason it is here.
pub fn rust_answer(case: &Case) -> bool {
    let grants: Vec<String> = case.grants.iter().map(|g| (*g).to_string()).collect();
    match case.kind {
        Kind::Path => khora_manifest::granted_path(&grants, case.subject),
        Kind::Name => khora_manifest::granted_name(&grants, case.subject),
        Kind::Host => khora_manifest::granted_host(&grants, case.subject),
    }
}

/// The `std::permissions` function that answers a case's question.
pub fn khora_function(kind: Kind) -> &'static str {
    match kind {
        Kind::Path => "granted",
        Kind::Name => "granted_name",
        Kind::Host => "granted_host",
    }
}

/// A Khora string literal for `text`.
///
/// Only `\` and `"` need escaping, and every subject in the table is plain
/// ASCII -- but the backslash rows are the ones that matter most, so getting
/// this wrong would silently weaken exactly the cases it must not.
pub fn khora_literal(text: &str) -> String {
    let mut out = String::from("\"");
    for c in text.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            _ => out.push(c),
        }
    }
    out.push('"');
    out
}

/// `grants` as a Khora `List<String>` expression.
pub fn khora_list(grants: &[&str]) -> String {
    let mut out = String::from("List::Nil");
    for grant in grants.iter().rev() {
        out = format!("List::Cons({}, {out})", khora_literal(grant));
    }
    out
}

/// How one case's answer is written, on either side.
///
/// Numbered, because the interesting failure is a *disagreement* and a bare
/// list of `granted`/`refused` lines that has gone out of step by one reads as
/// though everything after the first divergence also broke.
pub fn transcript_line(index: usize, granted: bool) -> String {
    format!("{index} {}\n", if granted { "granted" } else { "refused" })
}

/// The whole transcript `khora_manifest` would produce.
pub fn rust_transcript() -> String {
    CASES
        .iter()
        .enumerate()
        .map(|(at, case)| transcript_line(at, rust_answer(case)))
        .collect()
}
