//! Several Khora versions on one machine, and which one a project wants.
//!
//! Two halves, and only the first is interesting. A project says which compiler
//! it expects; a machine holds several; something has to pick. That picking is
//! pure and lives here. Actually replacing the running process with the chosen
//! one is the caller's job, because it is an `exec` and cannot be tested.
//!
//! # The pin lives in `khora.toml`
//!
//! ```toml
//! [toolchain]
//! version = "0.1.0"
//! ```
//!
//! Rust and Node both put this in a file of its own — `rust-toolchain.toml`,
//! `.nvmrc` — and the argument for that is that a toolchain is not a property
//! of the package. The argument against it is that a project then has two files
//! that both have to be found, both have to be committed, and only one of which
//! anybody remembers. One file that says everything about how to build this is
//! easier to keep true.
//!
//! # What a missing version does
//!
//! It stops, and names what is missing. Falling back to whatever is installed
//! is the failure mode this whole feature exists to prevent: a build that
//! quietly used a different compiler than the one the project asked for is
//! worse than no pin at all, because it looks like it worked.
//!
//! # Two ways a toolchain gets here
//!
//! [`install`](install::install) downloads a published release. `link`
//! registers one already on disk, which is what somebody with two checkouts of
//! this repository needs and what no download can provide.
//!
//! # What an unpinned directory gets
//!
//! Whatever is on `PATH`, unless [`install::set_default`] has said otherwise.
//! A default is a machine-wide preference and a pin is a project's
//! requirement, so a pin always wins — see [`wanted_version`].

#![deny(missing_docs)]

pub mod install;

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};

/// The version of the toolchain doing the asking.
///
/// `CARGO_PKG_VERSION` unless a packaged build said otherwise. A release
/// candidate is tagged `v0.2.0-rc.1` while the manifest still says `0.2.0` —
/// bumping every crate for each candidate is churn, and the two disagreeing is
/// worse than either: a pin on `0.2.0-rc.1` would find a toolchain that
/// reports `0.2.0` and decide it was the wrong one.
///
/// `scripts/package.sh` sets `KHORA_RELEASE` from the tag, so what a released
/// binary reports is what it was published as.
pub const RUNNING: &str = match option_env!("KHORA_RELEASE") {
    Some(named) => named,
    None => env!("CARGO_PKG_VERSION"),
};

/// What `khora --version` prints: the version, the commit, and the target.
///
/// **[`RUNNING`] alone is what a *pin* compares against and is deliberately
/// just the version.** This is the other question — "which compiler is this,
/// exactly?" — and a bug report needs its answer. Two builds of `0.1.0` from
/// either side of a fix report the same `RUNNING`, and during a release
/// candidate most reports come from people building the compiler themselves,
/// where the version says almost nothing. `build.rs` assembles it, so a build
/// with no `git` and no repository still gets the version and the target.
pub const VERSION_LINE: &str = env!("KHORA_VERSION_LINE");

/// Set to the version being run, so a shim cannot re-exec forever.
///
/// A link pointing at the wrong binary, or a toolchain whose reported version
/// disagrees with the directory it is filed under, would otherwise be an
/// infinite chain of `exec`s that looks like a hang.
pub const ACTIVE: &str = "KHORA_TOOLCHAIN";

/// Where toolchains and the package store live.
///
/// `$KHORA_HOME`, or `~/.khora`. The same root `khora-pkg` uses, so a machine
/// has one Khora directory rather than two.
pub fn home() -> Result<PathBuf> {
    if let Some(explicit) = std::env::var_os("KHORA_HOME") {
        return Ok(PathBuf::from(explicit));
    }
    let base = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from);
    match base {
        Some(h) => Ok(h.join(".khora")),
        None => bail!(
            "no home directory: neither HOME nor USERPROFILE is set. Set KHORA_HOME to \
             say where Khora should keep its toolchains"
        ),
    }
}

/// `<home>/toolchains`.
pub fn toolchains_dir() -> Result<PathBuf> {
    Ok(home()?.join("toolchains"))
}

/// One installed toolchain.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Toolchain {
    /// The version it is filed under, which is the directory's name.
    pub version: String,
    /// The executable to run.
    pub binary: PathBuf,
}

/// The name a Khora executable has on this platform.
pub(crate) fn executable() -> String {
    format!("khora{}", std::env::consts::EXE_SUFFIX)
}

/// Every toolchain under `<home>/toolchains`, sorted.
///
/// A directory with no executable in it is skipped rather than reported: an
/// interrupted `link` leaves one, and it is not the caller's problem.
pub fn installed() -> Result<Vec<Toolchain>> {
    let dir = toolchains_dir()?;
    let Ok(entries) = std::fs::read_dir(&dir) else { return Ok(Vec::new()) };

    let mut out = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let Some(version) = path.file_name().and_then(|n| n.to_str()) else { continue };
        let binary = path.join("bin").join(executable());
        if binary.is_file() {
            out.push(Toolchain { version: version.to_string(), binary });
        }
    }
    out.sort();
    Ok(out)
}

/// The version a project asks for, if it asks.
///
/// Walks up from `start` to the nearest `khora.toml`, the same way everything
/// else finds a manifest. A manifest that does not parse contributes no pin
/// rather than failing: `khora check` on the manifest is the command whose job
/// it is to complain about the manifest, and refusing to start the compiler at
/// all would mean the error could never be shown.
pub fn pinned_version(start: &Path) -> Option<String> {
    let mut here: Option<&Path> =
        Some(if start.is_dir() { start } else { start.parent().unwrap_or(Path::new(".")) });
    while let Some(dir) = here {
        let candidate = dir.join("khora.toml");
        if candidate.is_file() {
            let parsed = khora_manifest::Manifest::load(&candidate).ok()?;
            return parsed.manifest.toolchain.and_then(|t| t.version);
        }
        here = dir.parent();
    }
    None
}

/// What to do about a pin.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    /// No pin, or the pin names the version already running.
    Proceed,
    /// Hand over to another toolchain.
    Handover(Toolchain),
    /// The pin names something not installed.
    Missing {
        /// The version the project asked for.
        wanted: String,
        /// What is installed instead, for the message.
        available: Vec<String>,
    },
}

/// Decides what a `khora` invocation should do about the project's pin.
///
/// `active` is the value of [`ACTIVE`] in the environment: once a handover has
/// happened, the second process must proceed even if it disagrees about its own
/// version, or a mislinked toolchain becomes an infinite chain of `exec`s.
pub fn decide(pin: Option<&str>, running: &str, active: Option<&str>, have: &[Toolchain]) -> Decision {
    let Some(wanted) = pin else { return Decision::Proceed };
    if wanted == running {
        return Decision::Proceed;
    }
    // Already handed over once. Whatever we are, we are what was asked for.
    if active.is_some() {
        return Decision::Proceed;
    }
    match have.iter().find(|t| t.version == wanted) {
        Some(found) => Decision::Handover(found.clone()),
        None => Decision::Missing {
            wanted: wanted.to_string(),
            available: have.iter().map(|t| t.version.clone()).collect(),
        },
    }
}

/// What to tell somebody whose pinned version is not installed.
///
/// Names the version, what is there, and the command that fixes it. "Not
/// found" on its own leaves them guessing at both the directory layout and the
/// subcommand.
pub fn missing_message(wanted: &str, available: &[String]) -> String {
    let mut out = format!(
        "this project pins Khora {wanted}, which is not installed.\n\n\
         Khora will not fall back to another version: a build that quietly used \
         a different compiler than the one the project asked for is worse than \
         no pin at all, because it looks like it worked.\n"
    );
    if available.is_empty() {
        out.push_str("\nNo other toolchains are installed.\n");
    } else {
        out.push_str(&format!("\nInstalled: {}\n", available.join(", ")));
    }
    out.push_str(&format!(
        "\nGet it:\n\n    khora toolchain install {wanted}\n\n\
         Or register one you built yourself:\n\n    \
         khora toolchain link {wanted} <path-to-khora>\n"
    ));
    out
}

/// The version this directory should be built with, and why.
///
/// A project's pin first, then the machine's default. **A pin always wins**:
/// a default is a preference somebody expressed once, and a pin is a
/// requirement the project restates every time it is built.
///
/// `None` means neither, which is the ordinary case and the one where the
/// `khora` on `PATH` simply runs.
pub fn wanted_version(start: &Path) -> Option<String> {
    pinned_version(start).or_else(install::default_version)
}

/// Registers an executable as the toolchain for `version`.
///
/// Copies rather than symlinks. A symlink into somebody's `target/debug` breaks
/// the next time they run `cargo clean`, and it breaks by pointing at nothing
/// rather than by saying so.
pub fn link(version: &str, binary: &Path) -> Result<PathBuf> {
    if !binary.is_file() {
        bail!("{} is not a file", binary.display());
    }
    khora_manifest::Version::parse(version)
        .map_err(|why| anyhow::anyhow!("`{version}` is not a version: {why}"))?;

    let dir = toolchains_dir()?.join(version).join("bin");
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("creating {}", dir.display()))?;

    let destination = dir.join(executable());
    std::fs::copy(binary, &destination)
        .with_context(|| format!("copying {} to {}", binary.display(), destination.display()))?;
    Ok(destination)
}

/// Forgets a registered toolchain.
pub fn unlink(version: &str) -> Result<()> {
    let dir = toolchains_dir()?.join(version);
    if !dir.is_dir() {
        bail!("no toolchain {version} is registered");
    }
    std::fs::remove_dir_all(&dir).with_context(|| format!("removing {}", dir.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn have(versions: &[&str]) -> Vec<Toolchain> {
        versions
            .iter()
            .map(|v| Toolchain { version: (*v).to_string(), binary: PathBuf::from(v) })
            .collect()
    }

    #[test]
    fn no_pin_proceeds() {
        assert_eq!(decide(None, "0.1.0", None, &have(&["0.2.0"])), Decision::Proceed);
    }

    #[test]
    fn a_pin_naming_the_running_version_proceeds() {
        assert_eq!(decide(Some("0.1.0"), "0.1.0", None, &[]), Decision::Proceed);
    }

    #[test]
    fn a_pin_naming_another_installed_version_hands_over() {
        let installed = have(&["0.1.0", "0.2.0"]);
        match decide(Some("0.2.0"), "0.1.0", None, &installed) {
            Decision::Handover(t) => assert_eq!(t.version, "0.2.0"),
            other => panic!("expected a handover, got {other:?}"),
        }
    }

    /// The whole point of the feature: never silently use a different compiler.
    #[test]
    fn a_pin_naming_nothing_installed_is_refused() {
        let decision = decide(Some("9.9.9"), "0.1.0", None, &have(&["0.1.0"]));
        match decision {
            Decision::Missing { wanted, available } => {
                assert_eq!(wanted, "9.9.9");
                assert_eq!(available, ["0.1.0"]);
            }
            other => panic!("expected a refusal, got {other:?}"),
        }
    }

    /// A mislinked toolchain would otherwise be an infinite chain of `exec`s,
    /// which presents as a hang rather than as an error.
    #[test]
    fn a_second_handover_never_happens() {
        let installed = have(&["0.2.0"]);
        assert_eq!(
            decide(Some("0.2.0"), "0.1.0", Some("0.2.0"), &installed),
            Decision::Proceed,
            "the environment says a handover already happened"
        );
    }

    #[test]
    fn the_missing_message_names_the_fix() {
        let text = missing_message("0.3.0", &["0.1.0".to_string()]);
        assert!(text.contains("0.3.0"), "{text}");
        assert!(text.contains("0.1.0"), "it should say what is there: {text}");
        assert!(text.contains("khora toolchain link 0.3.0"), "{text}");
    }

    #[test]
    fn the_missing_message_copes_with_nothing_installed() {
        let text = missing_message("0.3.0", &[]);
        assert!(text.contains("No other toolchains are installed"), "{text}");
    }

    /// Both ways of getting one, because somebody who has neither installed it
    /// nor built it needs to be told which of the two they want.
    #[test]
    fn the_missing_message_offers_the_download_first() {
        let text = missing_message("0.3.0", &[]);
        let install = text.find("toolchain install 0.3.0").expect("the download: {text}");
        let link = text.find("toolchain link 0.3.0").expect("the local one: {text}");
        assert!(install < link, "the download should come first:\n{text}");
    }
}
