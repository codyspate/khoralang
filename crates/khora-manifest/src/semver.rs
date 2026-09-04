//! `version = "1.2.3"`, checked.
//!
//! Roadmap 10.1: the half of "apply D12 at publication" that can be applied
//! before there is anywhere to publish to.
//!
//! `docs/design/compatibility.md` is a policy written entirely in terms of
//! major, minor and patch — what each may break, and the rule that a bug fix is
//! not automatically a patch release. All of that is unenforceable against a
//! version string nobody parsed. `version = "1.2"`, `"v1.2.3"` and `"latest"`
//! were all accepted, and the first place any of them would have been noticed
//! is a resolver comparing two of them and getting an answer nobody could
//! explain.
//!
//! Deliberately hand-written rather than a dependency. The subset that matters
//! is three numbers, an optional pre-release and an optional build tag, and
//! writing it here means the error message can name the field and say what was
//! wrong with it rather than reporting a parse failure from somewhere else.

use std::fmt;

/// A parsed semantic version.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Version {
    /// Incompatible changes.
    pub major: u64,
    /// Compatible additions.
    pub minor: u64,
    /// Compatible fixes.
    pub patch: u64,
    /// `-alpha.1`, without the dash. Significant to ordering, and lower than
    /// the same version without one -- see the [`Ord`] implementation.
    pub pre: Option<String>,
    /// `+build.5`, without the plus. Never significant to anything.
    pub build: Option<String>,
}

/// Precedence, by the rules in the specification.
///
/// **Derived once, and derived wrong.** `#[derive(Ord)]` compares the fields in
/// order and reaches `pre: Option<String>`, where `None < Some(_)` -- so
/// `0.2.0` sorted *below* `0.2.0-rc.1`, exactly backwards, a release candidate
/// ranking above the release it was a candidate for. It did not matter while
/// nothing compared versions. `[toolchain] version = "latest"` compares them:
/// it means the newest stable toolchain installed, and under the derived order
/// "newest stable" would have picked a candidate.
///
/// So the spec's rules, which are not the obvious ones:
///
/// - A version with a prerelease is *less than* the same version without.
/// - Prereleases compare identifier by identifier, split on `.`.
/// - An all-digits identifier compares numerically and ranks below one that is
///   not, so `rc.2 < rc.10` rather than `rc.10 < rc.2` as strings.
/// - Running out of identifiers first is lower: `rc < rc.1`.
///
/// Build metadata is ignored, which the spec also requires.
impl Ord for Version {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        use std::cmp::Ordering;
        (self.major, self.minor, self.patch)
            .cmp(&(other.major, other.minor, other.patch))
            .then_with(|| match (self.pre.as_deref(), other.pre.as_deref()) {
                (None, None) => Ordering::Equal,
                // A release outranks any candidate for it.
                (None, Some(_)) => Ordering::Greater,
                (Some(_), None) => Ordering::Less,
                (Some(ours), Some(theirs)) => precedence(ours, theirs),
            })
    }
}

impl PartialOrd for Version {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

/// Compares two prerelease strings, identifier by identifier.
fn precedence(ours: &str, theirs: &str) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    let mut ours = ours.split('.');
    let mut theirs = theirs.split('.');
    loop {
        match (ours.next(), theirs.next()) {
            (None, None) => return Ordering::Equal,
            // Fewer identifiers is lower precedence, so `rc` is below `rc.1`.
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(a), Some(b)) => {
                let order = match (a.parse::<u64>(), b.parse::<u64>()) {
                    (Ok(a), Ok(b)) => a.cmp(&b),
                    // Numeric identifiers always rank below alphanumeric ones.
                    (Ok(_), Err(_)) => Ordering::Less,
                    (Err(_), Ok(_)) => Ordering::Greater,
                    (Err(_), Err(_)) => a.cmp(b),
                };
                if order != Ordering::Equal {
                    return order;
                }
            }
        }
    }
}

impl Version {
    /// Parses `major.minor.patch[-pre][+build]`.
    ///
    /// The message names what is wrong rather than only that something is,
    /// because the two mistakes people actually make — a leading `v` and a
    /// two-part version — are both worth saying out loud.
    pub fn parse(text: &str) -> Result<Version, String> {
        if text.is_empty() {
            return Err("a version is required, such as `0.1.0`".to_string());
        }
        if let Some(rest) = text.strip_prefix('v') {
            return Err(format!(
                "`{text}` starts with `v`. Semantic versions do not: write `{rest}`"
            ));
        }

        let (rest, build) = split_once(text, '+');
        let (core, pre) = split_once(rest, '-');

        let parts: Vec<&str> = core.split('.').collect();
        if parts.len() != 3 {
            return Err(format!(
                "`{text}` has {} part{} where a semantic version has three: \
                 major.minor.patch, as in `0.1.0`",
                parts.len(),
                if parts.len() == 1 { "" } else { "s" }
            ));
        }

        let mut numbers = [0u64; 3];
        for (slot, (name, part)) in
            numbers.iter_mut().zip(["major", "minor", "patch"].into_iter().zip(parts))
        {
            if part.is_empty() {
                return Err(format!("`{text}` has an empty {name} version"));
            }
            // Rejected rather than accepted-and-normalised: `01.0.0` and
            // `1.0.0` would otherwise be two spellings of one version, and a
            // lockfile would eventually hold both.
            if part.len() > 1 && part.starts_with('0') {
                return Err(format!(
                    "`{text}` has a leading zero in the {name} version, which makes \
                     `{part}` and `{}` two spellings of one number",
                    part.trim_start_matches('0')
                ));
            }
            match part.parse::<u64>() {
                Ok(value) => *slot = value,
                Err(_) => {
                    return Err(format!(
                        "`{text}` has `{part}` where the {name} version should be a number"
                    ))
                }
            }
        }

        if let Some(pre) = pre {
            if pre.is_empty() {
                return Err(format!("`{text}` ends with `-` and no pre-release"));
            }
        }
        if let Some(build) = build {
            if build.is_empty() {
                return Err(format!("`{text}` ends with `+` and no build metadata"));
            }
        }

        let [major, minor, patch] = numbers;
        Ok(Version {
            major,
            minor,
            patch,
            pre: pre.map(str::to_string),
            build: build.map(str::to_string),
        })
    }

    /// Whether this is before 1.0, where `compatibility.md` says the promise
    /// has not started.
    pub fn is_pre_1_0(&self) -> bool {
        self.major == 0
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)?;
        if let Some(pre) = &self.pre {
            write!(f, "-{pre}")?;
        }
        if let Some(build) = &self.build {
            write!(f, "+{build}")?;
        }
        Ok(())
    }
}

/// Splits at the first `sep`, or the whole thing and `None`.
fn split_once(text: &str, sep: char) -> (&str, Option<&str>) {
    match text.split_once(sep) {
        Some((before, after)) => (before, Some(after)),
        None => (text, None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_ordinary_version_parses() {
        let v = Version::parse("1.2.3").expect("valid");
        assert_eq!((v.major, v.minor, v.patch), (1, 2, 3));
        assert_eq!(v.to_string(), "1.2.3");
    }

    #[test]
    fn a_pre_release_and_build_survive_a_round_trip() {
        for text in ["1.0.0-alpha.1", "1.0.0+build.5", "1.0.0-rc.1+exp.sha.5114f85"] {
            assert_eq!(Version::parse(text).expect("valid").to_string(), text);
        }
    }

    /// The two mistakes people actually make, each with a message that says
    /// which one it was.
    #[test]
    fn the_common_mistakes_are_named() {
        let leading_v = Version::parse("v1.2.3").expect_err("refused");
        assert!(leading_v.contains("write `1.2.3`"), "{leading_v}");

        let two_parts = Version::parse("1.2").expect_err("refused");
        assert!(two_parts.contains("has 2 parts"), "{two_parts}");
    }

    /// `01.0.0` and `1.0.0` must not both be accepted, or a lockfile ends up
    /// holding two spellings of one version.
    #[test]
    fn a_leading_zero_is_refused() {
        assert!(Version::parse("01.0.0").is_err());
        assert!(Version::parse("1.02.0").is_err());
        assert!(Version::parse("0.1.0").is_ok(), "a bare zero is not a leading zero");
    }

    #[test]
    fn nonsense_is_refused() {
        for text in ["", "latest", "1.2.x", "1..0", "1.0.0-", "1.0.0+", "-1.0.0"] {
            assert!(Version::parse(text).is_err(), "`{text}` should be refused");
        }
    }

    #[test]
    fn pre_1_0_is_recognised() {
        assert!(Version::parse("0.9.9").expect("valid").is_pre_1_0());
        assert!(!Version::parse("1.0.0").expect("valid").is_pre_1_0());
    }

    /// Ordering is what a resolver will need first, and the numeric part is
    /// the half that is unambiguous.
    #[test]
    fn versions_order_numerically() {
        let mut all: Vec<Version> = ["1.0.0", "0.9.0", "1.0.1", "0.10.0", "2.0.0"]
            .iter()
            .map(|t| Version::parse(t).expect("valid"))
            .collect();
        all.sort();
        let order: Vec<String> = all.iter().map(Version::to_string).collect();
        assert_eq!(order, ["0.9.0", "0.10.0", "1.0.0", "1.0.1", "2.0.0"]);
    }
}

#[cfg(test)]
mod precedence_tests {
    use super::*;

    fn v(text: &str) -> Version {
        Version::parse(text).expect("a version")
    }

    /// **The bug the derived ordering had**, and the reason `latest` needs
    /// this: a release outranks its own candidates.
    #[test]
    fn a_release_outranks_its_candidates() {
        assert!(v("0.2.0") > v("0.2.0-rc.1"));
        assert!(v("0.2.0") > v("0.2.0-rc.3"));
    }

    /// String ordering puts `rc.10` below `rc.2`. Numeric identifiers compare
    /// as numbers, so it does not.
    #[test]
    fn numeric_identifiers_compare_as_numbers() {
        assert!(v("0.1.0-rc.10") > v("0.1.0-rc.2"));
    }

    /// Fewer identifiers is lower precedence.
    #[test]
    fn a_shorter_prerelease_is_lower() {
        assert!(v("0.1.0-rc") < v("0.1.0-rc.1"));
    }

    /// A numeric identifier ranks below an alphanumeric one.
    #[test]
    fn numbers_rank_below_words() {
        assert!(v("0.1.0-1") < v("0.1.0-alpha"));
    }

    /// The ordinary case still works, and build metadata is ignored.
    #[test]
    fn the_triple_leads_and_build_metadata_does_not_count() {
        assert!(v("0.10.0") > v("0.2.0"));
        assert_eq!(v("0.1.0+a").cmp(&v("0.1.0+b")), std::cmp::Ordering::Equal);
    }

    /// Sorting a realistic set of installed toolchains puts the newest
    /// release last and every candidate below its release.
    #[test]
    fn a_set_of_toolchains_sorts_the_way_latest_needs() {
        let mut have = [v("0.2.0"), v("0.1.0"), v("0.2.0-rc.1"), v("0.1.0-rc.10")];
        have.sort();
        let names: Vec<String> = have.iter().map(|v| v.to_string()).collect();
        assert_eq!(names, ["0.1.0-rc.10", "0.1.0", "0.2.0-rc.1", "0.2.0"]);
    }
}
