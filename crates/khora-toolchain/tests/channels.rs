//! `latest` and `latest.rc`: a pin that names the newest rather than a version.
//!
//! **Resolved against installed toolchains, never over the network.** Asking
//! GitHub which release is newest would put an HTTP request in front of every
//! `khora` invocation, including the ones an editor makes while somebody types.
//! So these tests are about `decide` and `Channel::newest`, both of which take
//! the installed set as an argument and touch nothing else.

use khora_toolchain::{decide, Channel, Decision, Toolchain};

/// The installed set, from version numbers alone. The binary path is never
/// examined by anything under test; only [`Decision::Handover`] carries it, and
/// then only to be compared.
fn have(versions: &[&str]) -> Vec<Toolchain> {
    versions
        .iter()
        .map(|v| Toolchain { version: (*v).to_string(), binary: format!("/toolchains/{v}").into() })
        .collect()
}

#[test]
fn only_two_words_are_channels() {
    assert_eq!(Channel::of("latest"), Some(Channel::Stable));
    assert_eq!(Channel::of("latest.rc"), Some(Channel::Any));
    for other in ["0.2.0", "stable", "newest", "latest.stable", "LATEST"] {
        assert_eq!(Channel::of(other), None, "{other}");
    }
}

/// **`latest` means the newest release, and a candidate is not one.** This is
/// the case the derived version ordering got wrong: `Option<String>` puts
/// `None` below `Some`, so `0.2.0` sorted below `0.2.0-rc.1` and "newest
/// stable" picked the candidate.
#[test]
fn latest_skips_release_candidates() {
    let installed = have(&["0.1.0", "0.2.0", "0.2.0-rc.1", "0.3.0-rc.2"]);
    assert_eq!(Channel::Stable.newest("0.1.0", &installed).as_deref(), Some("0.2.0"));
    assert_eq!(Channel::Any.newest("0.1.0", &installed).as_deref(), Some("0.3.0-rc.2"));
}

/// Ordering is numeric, not textual: `0.10.0` is newer than `0.2.0`, and
/// `rc.10` is newer than `rc.2`.
#[test]
fn newest_means_newest_and_not_last_alphabetically() {
    assert_eq!(Channel::Stable.newest("0.1.0", &have(&["0.2.0", "0.10.0"])).as_deref(), Some("0.10.0"));
    assert_eq!(
        Channel::Any.newest("0.1.0", &have(&["0.2.0-rc.2", "0.2.0-rc.10"])).as_deref(),
        Some("0.2.0-rc.10")
    );
}

/// **The running binary is always a candidate**, so a machine with an empty
/// `toolchains` directory still resolves. Otherwise the first thing anybody did
/// with a channel -- write it into a project before installing anything --
/// would report that `latest` is not installed.
#[test]
fn the_running_binary_counts() {
    assert_eq!(Channel::Stable.newest("0.4.0", &[]).as_deref(), Some("0.4.0"));
    assert_eq!(Channel::Stable.newest("0.4.0", &have(&["0.1.0"])).as_deref(), Some("0.4.0"));
}

/// A directory under `toolchains` is named by whoever created it, and one
/// called `scratch` is not a release. Skipped rather than ordered arbitrarily.
#[test]
fn a_directory_that_is_not_a_version_is_not_a_candidate() {
    let installed = have(&["scratch", "my-build", "0.1.0"]);
    assert_eq!(Channel::Stable.newest("0.0.1", &installed).as_deref(), Some("0.1.0"));
}

/// `latest` with nothing stable anywhere -- every install a candidate, and a
/// candidate running -- has no answer. `decide` proceeds rather than refusing:
/// there is nothing else to run.
#[test]
fn a_channel_with_no_answer_proceeds() {
    assert_eq!(Channel::Stable.newest("0.2.0-rc.1", &have(&["0.2.0-rc.2"])), None);
    assert_eq!(decide(Some("latest"), "0.2.0-rc.1", None, &have(&["0.2.0-rc.2"])), Decision::Proceed);
}

/// The channel resolves, and then the ordinary rules apply: the same version is
/// already running, so nothing is handed over.
#[test]
fn a_channel_naming_the_running_version_proceeds() {
    assert_eq!(decide(Some("latest"), "0.2.0", None, &have(&["0.1.0", "0.2.0"])), Decision::Proceed);
}

/// And when it names another one, that one runs.
#[test]
fn a_channel_naming_another_version_hands_over() {
    let installed = have(&["0.1.0", "0.3.0"]);
    match decide(Some("latest"), "0.1.0", None, &installed) {
        Decision::Handover(target) => assert_eq!(target.version, "0.3.0"),
        other => panic!("expected a handover, got {other:?}"),
    }
}

/// **`latest.rc` includes candidates, and that is the whole difference.** The
/// same set, the same running version, two answers.
#[test]
fn the_two_channels_differ_only_over_candidates() {
    let installed = have(&["0.2.0", "0.3.0-rc.1"]);
    assert_eq!(Channel::Stable.newest("0.2.0", &installed).as_deref(), Some("0.2.0"));
    assert_eq!(Channel::Any.newest("0.2.0", &installed).as_deref(), Some("0.3.0-rc.1"));
}

/// An exact pin still means exactly that, and an uninstalled one still stops
/// the command. A channel must not have turned every pin into a suggestion.
#[test]
fn an_exact_pin_is_still_exact() {
    match decide(Some("0.9.0"), "0.2.0", None, &have(&["0.1.0", "0.2.0"])) {
        Decision::Missing { wanted, available } => {
            assert_eq!(wanted, "0.9.0");
            assert_eq!(available, ["0.1.0", "0.2.0"]);
        }
        other => panic!("expected Missing, got {other:?}"),
    }
}
