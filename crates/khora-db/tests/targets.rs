//! Which source files belong in a build.
//!
//! The rule is in the file's name, as it is in Go. A suffix keeps each
//! target's version of a thing whole and readable on its own, and makes "which
//! files did this build use?" a question `ls` can answer.

use std::path::Path;

use khora_db::{host_target, selected_for_target};

fn on(name: &str, target: &str) -> bool {
    selected_for_target(Path::new(name), target)
}

/// A file with no suffix is every target's.
#[test]
fn an_unsuffixed_file_is_always_built() {
    for target in ["windows", "linux", "macos"] {
        assert!(on("socket.kh", target), "socket.kh on {target}");
        assert!(on("core.kh", target));
        assert!(on("a/deep/path/main.kh", target));
    }
}

/// And one with a suffix belongs to that target alone.
#[test]
fn a_suffixed_file_belongs_to_its_target() {
    assert!(on("socket_windows.kh", "windows"));
    assert!(!on("socket_windows.kh", "linux"));
    assert!(!on("socket_windows.kh", "macos"));

    assert!(on("socket_linux.kh", "linux"));
    assert!(!on("socket_linux.kh", "windows"));

    assert!(on("socket_macos.kh", "macos"));
    assert!(!on("socket_macos.kh", "windows"));
}

/// `_posix` is the one that covers two, because most of what differs from
/// Windows does not differ between Linux and macOS — and saying it once beats
/// two identical files.
#[test]
fn posix_covers_linux_and_macos_but_not_windows() {
    assert!(on("socket_posix.kh", "linux"));
    assert!(on("socket_posix.kh", "macos"));
    assert!(!on("socket_posix.kh", "windows"));
}

/// The suffix is matched on the *stem*, so the extension does not confuse it
/// and a name that merely contains a target word is not one.
#[test]
fn only_a_trailing_suffix_counts() {
    assert!(on("windows_helpers.kh", "linux"), "the word is at the front");
    assert!(on("linux.kh", "windows"), "the whole stem is the word, with no underscore");
    assert!(on("my_windowsish.kh", "linux"));
}

/// Two files may declare the same module when at most one is ever built, which
/// is the whole point of the rule.
#[test]
fn two_targets_versions_never_meet() {
    for target in ["windows", "linux", "macos"] {
        let both = ["net_windows.kh", "net_posix.kh"]
            .into_iter()
            .filter(|f| on(f, target))
            .count();
        assert_eq!(both, 1, "exactly one version is built on {target}");
    }
}

/// And the host names itself the same way a file does, so the two can be
/// compared without a translation table in between.
#[test]
fn the_host_names_itself_as_a_suffix_would() {
    let host = host_target();
    assert!(["windows", "linux", "macos"].contains(&host), "unexpected host `{host}`");
    assert!(on(&format!("net_{host}.kh"), host));
}
