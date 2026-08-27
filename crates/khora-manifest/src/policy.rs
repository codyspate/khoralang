//! Enforcing `[workspace.policy]`.
//!
//! A root that caps what any member may request. `docs/design/permissions.md`
//! already makes a package declare what it may reach; in a monorepo that
//! becomes a policy surface, and a rule about which services talk to the
//! internet stops being a code-review convention. Roadmap 14.19.
//!
//! # Where this runs, and why here
//!
//! In [`crate::Manifest::parse_at`], which every command that reads a manifest
//! off disk goes through. Putting it in one command would mean the cap held
//! for `khora build` and not for `khora check`, and a cap with a gap is a
//! convention with extra steps.
//!
//! # What it caps, and what it does not
//!
//! *Which member may ask*, not what it may ask for. `network = ["gateway"]`
//! means only the package called `gateway` may have a `[permissions] network`
//! entry at all; it says nothing about which hosts. Capping the values as well
//! -- "no member may reach anything outside `*.internal`" -- is a real feature
//! and a different one: it needs a rule for what "narrower" means for a glob,
//! and getting that subtly wrong produces a cap that looks enforced and is
//! not.

use crate::error::ManifestError;
use crate::model::{Category, Manifest, Policy};
use crate::workspace::Root;

/// Refuses a manifest that asks for more than the root allows it to.
pub(crate) fn enforce(
    manifest: &Manifest,
    policy: &Policy,
    root: &Root,
) -> Result<(), ManifestError> {
    // A name that matches no member is refused before anything else. A typo in
    // a cap is a cap that does not apply, and it fails open -- so the mistake
    // has to be loud, and it has to be loud at the root's own file rather than
    // wherever it failed to bite.
    let members = root.member_names();
    for name in policy.names() {
        if !members.iter().any(|member| member == name) {
            return Err(ManifestError::invalid_value(
                "workspace.policy",
                format!(
                    "`{name}` is not a member of the workspace at {}. A policy naming a \
                     package that is not there caps nothing, so it is refused rather than \
                     ignored. Members: {}",
                    root.directory.join("khora.toml").display(),
                    if members.is_empty() { "none".to_string() } else { members.join(", ") }
                ),
            ));
        }
    }

    let Some(package) = manifest.package() else { return Ok(()) };
    let name = &package.name;

    for category in [Category::Network, Category::Fs, Category::Env] {
        let Some(allowed) = policy.allowed(category) else { continue };
        if !asks_for(manifest, category) {
            continue;
        }
        if !allowed.iter().any(|member| member == name) {
            return Err(refusal(name, category.key(), allowed, root));
        }
    }

    if let Some(allowed) = policy.extern_.as_deref() {
        // `[permissions] extern` is a package saying which of *its*
        // dependencies may reach outside Khora. Handing that decision out is
        // the thing a root caps: a member that may not extend the door cannot
        // extend it for somebody else either.
        if manifest.permissions.extern_.is_some() && !allowed.iter().any(|m| m == name) {
            return Err(refusal(name, "extern", allowed, root));
        }
    }

    Ok(())
}

/// Whether the manifest grants `category` at all.
///
/// The *presence of an entry*, not whether it grants anything. `network = []`
/// is a package that has thought about the network and decided on none, and a
/// cap has no reason to object -- but it is also not what somebody writes by
/// accident, so treating an empty list as "asked" would refuse a manifest that
/// is being careful.
fn asks_for(manifest: &Manifest, category: Category) -> bool {
    match category {
        Category::Network => manifest.permissions.network.as_ref().is_some_and(|g| !g.is_empty()),
        Category::Env => manifest.permissions.env.as_ref().is_some_and(|g| !g.is_empty()),
        Category::Fs => manifest
            .permissions
            .fs
            .as_ref()
            .is_some_and(|g| !g.read.is_empty() || !g.write.is_empty()),
    }
}

fn refusal(name: &str, key: &str, allowed: &[String], root: &Root) -> ManifestError {
    let who = if allowed.is_empty() {
        "no member at all".to_string()
    } else {
        allowed.join(", ")
    };
    ManifestError::invalid_value(
        &format!("permissions.{key}"),
        format!(
            "`{name}` is not allowed to grant `{key}`. The workspace at {} caps it to {who}. \
             Add `{name}` to `[workspace.policy] {key}` if it should be, or drop the grant",
            root.directory.join("khora.toml").display()
        ),
    )
}
