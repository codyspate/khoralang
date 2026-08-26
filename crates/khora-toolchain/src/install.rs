//! Fetching a published toolchain, so that `curl | sh` is only a bootstrap.
//!
//! The installer's job ends once one `khora` exists. Everything after that — a
//! newer version, a candidate to test, a second version alongside the first,
//! going back when the new one breaks something — is this module, reached
//! through `khora toolchain install`, `khora toolchain default` and
//! `khora update`.
//!
//! That is deliberate. Rust splits it: `rustup` manages versions, `cargo`
//! drives builds, and a newcomer has to learn there are two programs before
//! they can use one. There is nothing here `khora` cannot do about itself.
//!
//! # Where a release comes from
//!
//! GitHub, by tag, following the same rules as `install.sh` so the two cannot
//! disagree about which version is "latest":
//!
//! - A stable release is what `/releases/latest` returns; it excludes drafts
//!   and pre-releases.
//! - A candidate is published as a *pre-release*, so `--pre` — "include
//!   candidates", never "only candidates" — takes the newest of any kind.
//! - `--version` takes one verbatim, latest or not, which is how somebody goes
//!   back.
//!
//! # What is shelled out to, and why
//!
//! `curl` or `wget` for the download, `tar` for the unpacking. The alternative
//! is four dependencies — an HTTP client, a TLS stack, gzip and zip — inside a
//! compiler that needs none of them otherwise, to do a job the operating system
//! already ships a program for. `khora-pkg` reached the same conclusion about
//! `git`. A machine missing all of them is told which one rather than getting a
//! link error.
//!
//! The checksum is *not* shelled out to. `sha2` is already here for the package
//! store, and "is this the file the release says it is" is the one question
//! worth answering in a way that cannot be told to skip itself.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context, Result};

/// Where releases are published.
///
/// `KHORA_RELEASE_REPO` overrides it, which is what somebody running their own
/// builds inside an organisation would set.
pub fn repository() -> String {
    std::env::var("KHORA_RELEASE_REPO").unwrap_or_else(|_| "codyspate/khoralang".to_string())
}

/// The target triple this build was compiled for.
///
/// From `build.rs`, because `std::env::consts` cannot tell
/// `x86_64-pc-windows-msvc` from `x86_64-pc-windows-gnu` and those are
/// different downloads.
pub const TARGET: &str = env!("KHORA_TARGET");

/// What a release archive is called on this platform.
///
/// Windows gets a `.zip` and everything else a `.tar.gz`, matching
/// `scripts/package.sh`. One `tar` unpacks both: the one Windows has shipped
/// since 1803 is bsdtar, which reads zip.
pub fn archive_name(version: &str) -> String {
    let extension = if TARGET.contains("windows") { "zip" } else { "tar.gz" };
    format!("khora-{version}-{TARGET}.{extension}")
}

/// Which version to fetch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Wanted {
    /// The newest stable release.
    Latest,
    /// The newest release of any kind, candidates included.
    Newest,
    /// This one, whatever it is.
    Exactly(String),
}

/// Turns [`Wanted`] into a version number, asking GitHub when it has to.
pub fn resolve(wanted: &Wanted) -> Result<String> {
    let repo = repository();
    let (url, describe) = match wanted {
        Wanted::Exactly(version) => return Ok(version.trim_start_matches('v').to_string()),
        Wanted::Latest => {
            (format!("https://api.github.com/repos/{repo}/releases/latest"), "stable")
        }
        Wanted::Newest => (format!("https://api.github.com/repos/{repo}/releases"), "newest"),
    };

    let body =
        fetch_body(&url).with_context(|| format!("asking GitHub for the {describe} release"))?;
    if let Some(tag) = first_tag(&body) {
        // `/releases` is newest first, so the first tag is the answer for both
        // shapes: an object for `latest`, an array for the rest.
        return Ok(tag.trim_start_matches('v').to_string());
    }

    // **A 404 here is an answer, not a failure**, which is why this request
    // does not ask curl to treat one as an error. `/releases/latest` 404s when
    // every release so far is a pre-release — a repository with three
    // candidates and no stable version is in exactly that state, and telling
    // somebody their download failed would send them looking at their network.
    match complaint(&body) {
        Some(said) if said.eq_ignore_ascii_case("Not Found") || said.is_empty() => {}
        Some(said) => bail!("GitHub said: {said}"),
        None => {}
    }
    match wanted {
        Wanted::Latest => bail!(
            "no stable release yet. There may be a candidate: try `--pre`,\n\
             or see https://github.com/{repo}/releases"
        ),
        _ => bail!("nothing has been released yet.\nSee https://github.com/{repo}/releases"),
    }
}

/// The `"message"` GitHub puts in a response that is not what was asked for.
///
/// "Not Found" is ordinary and handled above; a rate limit is not, and reads as
/// "nothing has been released" unless it is repeated.
fn complaint(body: &str) -> Option<String> {
    let at = body.find("\"message\"")?;
    let rest = &body[at + "\"message\"".len()..];
    let open = rest.find('"')?;
    let value = &rest[open + 1..];
    let close = value.find('"')?;
    Some(value[..close].to_string())
}

/// The first `"tag_name"` in a GitHub API response.
///
/// One field out of a document nothing else here reads, so no JSON parser: a
/// tag cannot contain a quote, and this is the same read `install.sh` does with
/// `sed`, for the same reason.
fn first_tag(body: &str) -> Option<String> {
    let at = body.find("\"tag_name\"")?;
    let rest = &body[at + "\"tag_name\"".len()..];
    let open = rest.find('"')?;
    let value = &rest[open + 1..];
    let close = value.find('"')?;
    Some(value[..close].to_string())
}

/// Downloads, verifies and unpacks a release into `<home>/toolchains/<version>`.
///
/// Returns the executable. Reinstalling a version replaces it rather than
/// merging into it: a file left over from an older build is one the new
/// compiler was never tested against.
pub fn install(version: &str) -> Result<PathBuf> {
    khora_manifest::Version::parse(version)
        .map_err(|why| anyhow::anyhow!("`{version}` is not a version: {why}"))?;

    let repo = repository();
    let archive = archive_name(version);
    let base = format!("https://github.com/{repo}/releases/download/v{version}");

    let work = tempdir()?;
    let bundle = work.join(&archive);

    // `map_err` rather than `with_context`: this replaces the downloader's
    // complaint instead of hanging a sentence off the end of it, because
    // "no build for this platform in that release" is the whole story and
    // "`curl` could not fetch it" adds nothing to it.
    fetch(&format!("{base}/{archive}"), &bundle).map_err(|_| {
        anyhow::anyhow!(
            "no build for {TARGET} in v{version} yet.\n\n\
             If that release was just created, its artifacts are still building — \
             try again in a few minutes.\n\
             Otherwise this platform was not published for it.\n\n\
             See https://github.com/{repo}/releases/tag/v{version} for what is there."
        )
    })?;

    let sums = work.join(format!("{archive}.sha256"));
    fetch(&format!("{base}/{archive}.sha256"), &sums)
        .map_err(|_| anyhow::anyhow!("v{version} publishes no checksum for {archive}"))?;
    verify(&bundle, &sums)?;

    unpack(&bundle, &work)?;
    place(&work, version)
}

/// Moves an unpacked release into `<home>/toolchains/<version>` and checks it.
///
/// Separate from [`install`] because it is the half that can be tested: the
/// download needs a release on GitHub, and this needs a directory.
fn place(work: &Path, version: &str) -> Result<PathBuf> {
    // `package.sh` puts everything under one directory named for the release.
    // Flattened away here, because `installed` and `link` both expect
    // `toolchains/0.2.0/bin/khora` rather than
    // `toolchains/0.2.0/khora-0.2.0-<triple>/bin/khora`.
    let unpacked = work.join(format!("khora-{version}-{TARGET}"));
    if !unpacked.is_dir() {
        bail!(
            "the archive does not contain khora-{version}-{TARGET}/ as it should.\n\
             It may not be a Khora release."
        );
    }

    let destination = super::toolchains_dir()?.join(version);
    if destination.exists() {
        std::fs::remove_dir_all(&destination)
            .with_context(|| format!("replacing {}", destination.display()))?;
    }
    std::fs::create_dir_all(destination.parent().unwrap_or(Path::new(".")))
        .with_context(|| format!("creating {}", destination.display()))?;
    // A rename across filesystems fails with `EXDEV`, and on Linux `/tmp` is
    // often its own.
    if std::fs::rename(&unpacked, &destination).is_err() {
        copy_tree(&unpacked, &destination)?;
    }
    let _ = std::fs::remove_dir_all(work);

    let binary = destination.join("bin").join(super::executable());
    if !binary.is_file() {
        bail!("unpacked v{version}, but there is no {}", binary.display());
    }
    make_executable(&binary)?;
    Ok(binary)
}

/// The version an unpinned directory gets.
///
/// `<home>/default`, one line. A file rather than a symlink because Windows
/// wants a privilege for those, and rather than a copy of the binary because
/// this is eighty megabytes.
pub fn default_version() -> Option<String> {
    let text = std::fs::read_to_string(super::home().ok()?.join("default")).ok()?;
    let named = text.trim();
    if named.is_empty() {
        None
    } else {
        Some(named.to_string())
    }
}

/// Makes `version` what an unpinned directory gets.
pub fn set_default(version: &str) -> Result<()> {
    let have = super::installed()?;
    if version != super::RUNNING && !have.iter().any(|t| t.version == version) {
        let names: Vec<String> = have.into_iter().map(|t| t.version).collect();
        let installed = if names.is_empty() {
            String::new()
        } else {
            format!("\n\nInstalled: {}", names.join(", "))
        };
        bail!(
            "no toolchain {version} is installed.{installed}\n\n\
             Install it:\n\n    khora toolchain install {version}\n"
        );
    }
    let home = super::home()?;
    std::fs::create_dir_all(&home).with_context(|| format!("creating {}", home.display()))?;
    std::fs::write(home.join("default"), format!("{version}\n"))
        .with_context(|| format!("writing {}", home.join("default").display()))
}

/// Forgets the default, so an unpinned directory gets whatever is on PATH.
pub fn clear_default() -> Result<()> {
    let path = super::home()?.join("default");
    if path.exists() {
        std::fs::remove_file(&path).with_context(|| format!("removing {}", path.display()))?;
    }
    Ok(())
}

// --- the parts that are somebody else's program ------------------------------

/// Downloads `url` to `into`, refusing anything that is not the file.
///
/// `-f` matters: without it a 404 is saved as an HTML page named
/// `khora-0.2.0-<triple>.tar.gz`, and the failure surfaces two steps later as
/// a checksum mismatch that reads like a corrupted download.
fn fetch(url: &str, into: &Path) -> Result<()> {
    download(url, into, true)
}

/// Downloads `url` and returns it, keeping the body of an error response.
///
/// For the two API calls, where a 404 body says which of several things went
/// wrong and is worth more than the status alone.
fn fetch_body(url: &str) -> Result<String> {
    let work = tempdir()?;
    let file = work.join("response.json");
    let outcome = download(url, &file, false);
    let body = std::fs::read_to_string(&file).unwrap_or_default();
    let _ = std::fs::remove_dir_all(&work);
    // Only when nothing came back at all: with `keep_errors` the downloader
    // reports success on a 404, so a failure here is the network or a missing
    // program rather than an answer.
    outcome?;
    Ok(body)
}

fn download(url: &str, into: &Path, strict: bool) -> Result<()> {
    if have("curl") {
        let flags = if strict { "-fsSL" } else { "-sSL" };
        return run(Command::new("curl").args([flags, url, "-o"]).arg(into), "curl");
    }
    if have("wget") {
        let mut command = Command::new("wget");
        if !strict {
            command.arg("--content-on-error");
        }
        return run(command.arg("-qO").arg(into).arg(url), "wget");
    }
    if cfg!(windows) {
        // `Invoke-WebRequest` throws on a 404 rather than returning it, so the
        // body has to be read off the exception to keep it.
        let script = if strict {
            format!(
                "$ProgressPreference='SilentlyContinue'; \
                 Invoke-WebRequest -Uri '{url}' -OutFile '{}'",
                into.display()
            )
        } else {
            format!(
                "$ProgressPreference='SilentlyContinue'; \
                 try {{ Invoke-WebRequest -Uri '{url}' -OutFile '{}' }} \
                 catch {{ $_.ErrorDetails.Message | Out-File -Encoding utf8 '{}' }}",
                into.display(),
                into.display()
            )
        };
        return run(Command::new("powershell").args(["-NoProfile", "-Command", &script]), "powershell");
    }
    bail!("this needs `curl` or `wget` to download a release, and cannot find either")
}

/// Checks a download against the `<name>.sha256` published beside it.
///
/// **Not optional, and not shelled out to.** `install.sh` skips verification on
/// a machine with no `sha256sum`, because a shell script's only alternative is
/// to refuse to install at all. Here there is no such excuse.
fn verify(bundle: &Path, sums: &Path) -> Result<()> {
    use sha2::{Digest, Sha256};

    let text = std::fs::read_to_string(sums).context("reading the checksum")?;
    let expected = text.split_whitespace().next().unwrap_or_default().to_ascii_lowercase();

    let bytes = std::fs::read(bundle).with_context(|| format!("reading {}", bundle.display()))?;
    let actual = format!("{:x}", Sha256::digest(&bytes));

    if expected != actual {
        bail!(
            "checksum mismatch:\n  expected {expected}\n  got      {actual}\n\
             The download is not what the release says it is. Do not use it."
        );
    }
    Ok(())
}

/// Unpacks a `.tar.gz` or a `.zip` into `into`.
fn unpack(bundle: &Path, into: &Path) -> Result<()> {
    if have("tar") {
        return run(Command::new("tar").arg("-xf").arg(bundle).arg("-C").arg(into), "tar");
    }
    if cfg!(windows) {
        return run(
            Command::new("powershell").args([
                "-NoProfile",
                "-Command",
                &format!(
                    "Expand-Archive -Path '{}' -DestinationPath '{}' -Force",
                    bundle.display(),
                    into.display()
                ),
            ]),
            "powershell",
        );
    }
    bail!("this needs `tar` to unpack a release, and cannot find it")
}

/// Whether `program` is there. `--version` rather than a `which`, because what
/// matters is whether it runs.
fn have(program: &str) -> bool {
    Command::new(program)
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok()
}

/// Runs `command`, quietly, and turns a non-zero status into an error.
///
/// **Its output goes nowhere.** `curl: (22) The requested URL returned error:
/// 404` printed above a message that already explains what was not found is
/// noise that makes the real explanation look like a footnote.
fn run(command: &mut Command, name: &str) -> Result<()> {
    let status = command
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .with_context(|| format!("running `{name}`"))?;
    if !status.success() {
        bail!("`{name}` could not fetch it");
    }
    Ok(())
}

/// A directory of our own under the system temporary directory.
///
/// Not `tempfile`: that is a dev-dependency here, and this is the only place in
/// the crate that would pull it into the shipped binary.
fn tempdir() -> Result<PathBuf> {
    let base = std::env::temp_dir().join(format!(
        "khora-install-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    std::fs::create_dir_all(&base).with_context(|| format!("creating {}", base.display()))?;
    Ok(base)
}

/// A recursive copy, for the rename that crossed a filesystem.
fn copy_tree(from: &Path, to: &Path) -> Result<()> {
    std::fs::create_dir_all(to).with_context(|| format!("creating {}", to.display()))?;
    for entry in std::fs::read_dir(from).with_context(|| format!("reading {}", from.display()))? {
        let entry = entry?;
        let target = to.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_tree(&entry.path(), &target)?;
        } else {
            std::fs::copy(entry.path(), &target)
                .with_context(|| format!("copying to {}", target.display()))?;
        }
    }
    Ok(())
}

/// Restores the executable bit, which neither a zip nor a copy preserves.
#[cfg(unix)]
fn make_executable(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mut permissions = std::fs::metadata(path)?.permissions();
    permissions.set_mode(permissions.mode() | 0o755);
    std::fs::set_permissions(path, permissions)
        .with_context(|| format!("making {} executable", path.display()))
}

#[cfg(not(unix))]
fn make_executable(_path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `KHORA_HOME` is process-wide, so the tests that set it take turns.
    ///
    /// Under `cargo nextest` each test has a process of its own and this is
    /// free; under `cargo test` they are threads and it is what keeps one from
    /// reading the directory another just replaced.
    static HOME_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn the_archive_is_named_the_way_package_sh_names_it() {
        let name = archive_name("0.2.0");
        assert!(name.starts_with("khora-0.2.0-"), "{name}");
        assert!(name.contains(TARGET), "{name}");
        if TARGET.contains("windows") {
            assert!(name.ends_with(".zip"), "{name}");
        } else {
            assert!(name.ends_with(".tar.gz"), "{name}");
        }
    }

    /// The triple has to be a real one, or every download is a 404.
    #[test]
    fn the_target_triple_is_not_the_build_script_giving_up() {
        assert_ne!(TARGET, "unknown");
        assert!(TARGET.matches('-').count() >= 2, "{TARGET} is not a triple");
    }

    #[test]
    fn a_tag_is_read_out_of_what_github_sends() {
        let body = r#"{"url":"https://x","id":1,"tag_name":"v0.2.0","name":"0.2.0"}"#;
        assert_eq!(first_tag(body).as_deref(), Some("v0.2.0"));
    }

    /// `/releases` is newest first, so the first tag is the newest release.
    #[test]
    fn the_first_tag_of_a_list_is_the_one_taken() {
        let body = r#"[{"tag_name":"v0.3.0-rc.1"},{"tag_name":"v0.2.0"}]"#;
        assert_eq!(first_tag(body).as_deref(), Some("v0.3.0-rc.1"));
    }

    #[test]
    fn a_response_with_no_tag_is_none() {
        assert_eq!(first_tag("[]"), None);
        assert_eq!(first_tag(r#"{"message":"Not Found"}"#), None);
    }

    /// `--version 0.2.0` and `--version v0.2.0` are the same request: the
    /// leading `v` belongs to the tag, not to the version.
    #[test]
    fn an_exact_version_is_taken_without_asking_github() {
        assert_eq!(resolve(&Wanted::Exactly("v0.2.0".into())).unwrap(), "0.2.0");
        assert_eq!(resolve(&Wanted::Exactly("0.2.0".into())).unwrap(), "0.2.0");
    }

    #[test]
    fn a_rate_limit_is_read_off_the_body() {
        let body = r#"{"message":"API rate limit exceeded","documentation_url":"..."}"#;
        assert_eq!(complaint(body).as_deref(), Some("API rate limit exceeded"));
    }

    /// Lays out what `package.sh` produces and calls the half of `install` that
    /// does not need a release on GitHub.
    ///
    /// **What this guards is the flattening.** The archive holds
    /// `khora-<v>-<triple>/bin/khora`, and `installed` looks for
    /// `toolchains/<v>/bin/khora`. Leave the top directory in place and the
    /// download succeeds, says so, and registers nothing — which is the failure
    /// that would be hardest to explain.
    #[test]
    fn an_unpacked_release_lands_where_installed_looks_for_it() {
        let home = tempfile::tempdir().expect("a temporary directory");
        // Serialised against the other `KHORA_HOME` test, since the variable is
        // process-wide.
        let _guard = HOME_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var("KHORA_HOME", home.path());

        let work = tempfile::tempdir().expect("a temporary directory");
        let inside = work.path().join(format!("khora-9.9.9-{TARGET}")).join("bin");
        std::fs::create_dir_all(&inside).expect("the archive layout");
        std::fs::write(inside.join(super::super::executable()), b"not really a compiler")
            .expect("the executable");

        let landed = place(work.path(), "9.9.9").expect("placing it");
        assert!(landed.is_file(), "{}", landed.display());
        assert_eq!(
            crate::installed().expect("listing").iter().map(|t| t.version.as_str()).collect::<Vec<_>>(),
            ["9.9.9"]
        );

        std::env::remove_var("KHORA_HOME");
    }

    /// An archive that is not a Khora release says so, rather than reporting a
    /// successful install of nothing.
    #[test]
    fn an_archive_without_the_expected_directory_is_refused() {
        let work = tempfile::tempdir().expect("a temporary directory");
        std::fs::write(work.path().join("surprise.txt"), b"hello").expect("a file");
        let why = place(work.path(), "9.9.9").expect_err("it should refuse");
        assert!(format!("{why}").contains("may not be a Khora release"), "{why}");
    }
}
