//! A software bill of materials, from the lockfile.
//!
//! `docs/positioning.md` names an audit-heavy buyer explicitly, and an SBOM is
//! the first thing that buyer asks for. Roadmap 12.9.
//!
//! Almost nothing new is computed here. `khora.lock` already records every
//! resolved package, the immutable revision it came from, the SHA-256 of its
//! visible files, and what each one depends on — which is most of a bill of
//! materials already. This renders it in the shape the tools that consume one
//! expect.
//!
//! # CycloneDX, and why not SPDX
//!
//! CycloneDX 1.5, JSON. It is what dependency-scanning tools read, it maps
//! cleanly onto what the lockfile holds, and its dependency graph is a list of
//! edges rather than a document-relationship vocabulary. SPDX is the other
//! answer and is not ruled out — an SBOM is a rendering of the lockfile, so a
//! second format is another function here rather than another design.
//!
//! # No timestamp, on purpose
//!
//! `docs/project.md` §6.1 asks for bit-for-bit reproducible builds, and a
//! timestamp in a generated artifact is the ordinary way to lose that: the same
//! source and the same lockfile would produce two different documents, and
//! nothing downstream could tell a real change from the clock moving. CycloneDX
//! makes `metadata.timestamp` optional, so it is omitted, and the document is a
//! pure function of the lockfile and the manifest.
//!
//! **What that costs is worth naming**: a consumer that wants to know when a
//! document was produced has to get it from where the file came from rather
//! than from inside it. That is the right trade for an artifact whose value is
//! that two builds of the same input can be compared.
//!
//! # What this is not
//!
//! Not signing, and not provenance. Both are the rest of 12.9 and both need a
//! decision about keys that this does not make. An unsigned SBOM is still what
//! a scanner reads; a signature is what makes it evidence, and inventing a key
//! story to have one sooner would be worse than saying so.

use std::fmt::Write as _;

use crate::lock::{LockedPackage, Lockfile};

/// Renders `lock` as a CycloneDX 1.5 document in JSON.
///
/// `name` and `version` are the root package's, from its manifest — the
/// component the bill of materials is *for*, as distinct from the components
/// it lists.
pub fn cyclonedx(lock: &Lockfile, name: &str, version: &str) -> String {
    let mut out = String::new();
    out.push_str("{\n");
    out.push_str("  \"bomFormat\": \"CycloneDX\",\n");
    out.push_str("  \"specVersion\": \"1.5\",\n");
    out.push_str("  \"version\": 1,\n");

    // No `timestamp`. See the module documentation.
    out.push_str("  \"metadata\": {\n");
    out.push_str("    \"tools\": [\n");
    out.push_str("      {\n");
    out.push_str("        \"vendor\": \"khora\",\n");
    out.push_str("        \"name\": \"khora\",\n");
    let _ = writeln!(out, "        \"version\": {}", quote(env!("CARGO_PKG_VERSION")));
    out.push_str("      }\n");
    out.push_str("    ],\n");
    out.push_str("    \"component\": {\n");
    out.push_str("      \"type\": \"application\",\n");
    let _ = writeln!(out, "      \"bom-ref\": {},", quote(name));
    let _ = writeln!(out, "      \"name\": {},", quote(name));
    let _ = writeln!(out, "      \"version\": {},", quote(version));
    let _ = writeln!(out, "      \"purl\": {}", quote(&purl(name, Some(version))));
    out.push_str("    }\n");
    out.push_str("  },\n");

    // **Sorted, for the same reason there is no timestamp.** A lockfile's
    // order is the resolver's, and a resolver is free to change it without any
    // dependency having changed. Two documents that differ only in the order
    // of a list are two documents as far as a diff is concerned.
    let mut packages: Vec<&LockedPackage> = lock.packages.iter().collect();
    packages.sort_by(|a, b| a.name.cmp(&b.name));

    out.push_str("  \"components\": [\n");
    for (i, package) in packages.iter().enumerate() {
        component(&mut out, package);
        out.push_str(if i + 1 == packages.len() { "\n" } else { ",\n" });
    }
    out.push_str("  ],\n");

    // The graph, root included: a consumer asking "what does this application
    // pull in" reads the edges rather than inferring them from the list.
    out.push_str("  \"dependencies\": [\n");
    let mut edges: Vec<(String, Vec<String>)> =
        vec![(name.to_string(), packages.iter().map(|p| p.name.clone()).collect())];
    for package in &packages {
        let mut on = package.dependencies.clone();
        on.sort();
        edges.push((package.name.clone(), on));
    }
    for (i, (of, on)) in edges.iter().enumerate() {
        out.push_str("    {\n");
        let _ = writeln!(out, "      \"ref\": {},", quote(of));
        if on.is_empty() {
            out.push_str("      \"dependsOn\": []\n");
        } else {
            out.push_str("      \"dependsOn\": [\n");
            for (j, each) in on.iter().enumerate() {
                let _ = write!(out, "        {}", quote(each));
                out.push_str(if j + 1 == on.len() { "\n" } else { ",\n" });
            }
            out.push_str("      ]\n");
        }
        out.push_str("    }");
        out.push_str(if i + 1 == edges.len() { "\n" } else { ",\n" });
    }
    out.push_str("  ]\n");
    out.push_str("}\n");
    out
}

/// One package as a CycloneDX component.
fn component(out: &mut String, package: &LockedPackage) {
    out.push_str("    {\n");
    out.push_str("      \"type\": \"library\",\n");
    let _ = writeln!(out, "      \"bom-ref\": {},", quote(&package.name));
    let _ = writeln!(out, "      \"name\": {},", quote(&package.name));

    // **The revision is the version, for a git package.** A tag can be moved
    // and a branch certainly does; the lockfile pins a full commit id for
    // exactly that reason, and an SBOM naming anything softer would describe a
    // build nobody can reproduce.
    if let Some(revision) = &package.revision {
        let _ = writeln!(out, "      \"version\": {},", quote(revision));
    }

    if let Some(checksum) = &package.checksum {
        out.push_str("      \"hashes\": [\n");
        out.push_str("        {\n");
        out.push_str("          \"alg\": \"SHA-256\",\n");
        let _ = writeln!(out, "          \"content\": {}", quote(checksum));
        out.push_str("        }\n");
        out.push_str("      ],\n");
    }

    if let Some(url) = &package.url {
        out.push_str("      \"externalReferences\": [\n");
        out.push_str("        {\n");
        out.push_str("          \"type\": \"vcs\",\n");
        let _ = writeln!(out, "          \"url\": {}", quote(url));
        out.push_str("        }\n");
        out.push_str("      ],\n");
    }

    // A path package is *not* given a checksum by the resolver, because there
    // is no immutable thing to take one of. Saying so in the document beats
    // leaving a reader to wonder why one component has no hash.
    if package.source == "path" {
        out.push_str("      \"properties\": [\n");
        out.push_str("        {\n");
        out.push_str("          \"name\": \"khora:source\",\n");
        out.push_str("          \"value\": \"path\"\n");
        out.push_str("        },\n");
        out.push_str("        {\n");
        out.push_str("          \"name\": \"khora:unpinned\",\n");
        out.push_str(
            "          \"value\": \"a path dependency has no revision and no checksum; \
             it is whatever is in that directory\"\n",
        );
        out.push_str("        }\n");
        out.push_str("      ],\n");
    }

    let _ = write!(out, "      \"purl\": {}", quote(&purl(&package.name, package.revision.as_deref())));
    out.push_str("\n    }");
}

/// A package URL. `pkg:generic/<name>@<version>`, percent-encoded.
///
/// `generic` rather than a VCS type: purl's `github` and `gitlab` types want an
/// owner and a repository, which the lockfile does not have and which parsing a
/// URL for would guess wrong on a self-hosted remote. The repository is carried
/// as an external reference instead, where it needs no interpretation.
fn purl(name: &str, version: Option<&str>) -> String {
    match version {
        Some(version) => format!("pkg:generic/{}@{}", encode(name), encode(version)),
        None => format!("pkg:generic/{}", encode(name)),
    }
}

/// Percent-encodes everything a purl component may not carry literally.
fn encode(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for byte in text.bytes() {
        match byte {
            b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(byte as char)
            }
            other => {
                let _ = write!(out, "%{other:02X}");
            }
        }
    }
    out
}

/// A JSON string literal.
///
/// Hand-rolled because this crate does not otherwise depend on a JSON writer,
/// and a bill of materials is not worth a dependency. Escapes what RFC 8259
/// requires and nothing else — including the control characters below 0x20,
/// which a package name should never contain and which would otherwise produce
/// a document no parser accepts.
fn quote(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn git(name: &str, revision: &str, deps: &[&str]) -> LockedPackage {
        LockedPackage {
            name: name.to_string(),
            source: "git".to_string(),
            url: Some(format!("https://example.invalid/{name}.git")),
            revision: Some(revision.to_string()),
            path: None,
            checksum: Some(format!("{:0>64}", name)),
            dependencies: deps.iter().map(|d| d.to_string()).collect(),
        }
    }

    fn lock(packages: Vec<LockedPackage>) -> Lockfile {
        Lockfile { version: crate::lock::FORMAT_VERSION, packages }
    }

    #[test]
    fn an_empty_lockfile_is_still_a_document() {
        let out = cyclonedx(&lock(Vec::new()), "app", "1.0.0");
        assert!(out.contains("\"bomFormat\": \"CycloneDX\""));
        assert!(out.contains("\"name\": \"app\""));
        assert!(out.contains("\"components\": [\n  ],"), "an empty list, not a broken one: {out}");
    }

    #[test]
    fn a_package_carries_its_revision_and_its_checksum() {
        let out = cyclonedx(&lock(vec![git("router", "abc123", &[])]), "app", "1.0.0");
        assert!(out.contains("\"version\": \"abc123\""), "the commit is the version: {out}");
        assert!(out.contains("\"alg\": \"SHA-256\""));
        assert!(out.contains("pkg:generic/router@abc123"));
        assert!(out.contains("https://example.invalid/router.git"));
    }

    /// **Sorted, and not in the resolver's order.** A resolver is free to
    /// reorder without any dependency having changed, and two documents that
    /// differ only in the order of a list are two documents to a diff.
    #[test]
    fn components_come_out_in_name_order() {
        let out = cyclonedx(
            &lock(vec![git("zebra", "z", &[]), git("alpha", "a", &[])]),
            "app",
            "1.0.0",
        );
        let alpha = out.find("\"alpha\"").expect("alpha is listed");
        let zebra = out.find("\"zebra\"").expect("zebra is listed");
        assert!(alpha < zebra, "name order, not lockfile order:\n{out}");
    }

    #[test]
    fn the_graph_includes_the_root_and_every_edge() {
        let out = cyclonedx(
            &lock(vec![git("router", "r", &["logging"]), git("logging", "l", &[])]),
            "app",
            "1.0.0",
        );
        assert!(out.contains("\"ref\": \"app\""), "the root is a node: {out}");
        assert!(out.contains("\"ref\": \"router\""));
        assert!(out.contains("\"dependsOn\": []"), "a leaf has an empty list, not none");
    }

    /// A path dependency has nothing immutable to hash, and the document says
    /// so rather than leaving a reader to wonder why one component has no hash.
    #[test]
    fn a_path_package_says_it_is_unpinned() {
        let package = LockedPackage {
            name: "local".to_string(),
            source: "path".to_string(),
            url: None,
            revision: None,
            path: Some("../local".to_string()),
            checksum: None,
            dependencies: Vec::new(),
        };
        let out = cyclonedx(&lock(vec![package]), "app", "1.0.0");
        assert!(out.contains("khora:unpinned"), "{out}");
        assert!(!out.contains("SHA-256"), "and no hash it does not have: {out}");
    }

    /// The same input twice is the same document. There is no timestamp, and
    /// that is the whole point of there not being one.
    #[test]
    fn the_same_lockfile_renders_the_same_bytes() {
        let one = cyclonedx(&lock(vec![git("router", "r", &[])]), "app", "1.0.0");
        let two = cyclonedx(&lock(vec![git("router", "r", &[])]), "app", "1.0.0");
        assert_eq!(one, two);
        assert!(!one.contains("timestamp"), "no clock in a reproducible artifact: {one}");
    }

    #[test]
    fn a_name_that_would_break_the_json_is_escaped() {
        let mut package = git("odd", "r", &[]);
        package.name = "he said \"hi\"\\".to_string();
        let out = cyclonedx(&lock(vec![package]), "app", "1.0.0");
        assert!(out.contains(r#"he said \"hi\"\\"#), "{out}");
        // And the purl percent-encodes rather than escaping.
        assert!(out.contains("pkg:generic/he%20said%20%22hi%22%5C@r"), "{out}");
    }
}
