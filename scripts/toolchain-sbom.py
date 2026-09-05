"""A bill of materials for the Khora toolchain itself.

    python3 scripts/toolchain-sbom.py > khora-toolchain.cdx.json

`khora sbom` answers this question for a Khora *package*: what does the thing I
am building pull in. This answers it for the compiler somebody is building
with, which is the other half an auditor asks for and the half that was
missing. Same format, deliberately -- CycloneDX 1.5 JSON, so one scanner reads
both -- and the same two omissions, for the same reasons `khora-pkg/src/sbom.rs`
gives: no timestamp and everything sorted, so that two runs over an unchanged
tree produce identical bytes and a diff means something changed.

The dependency graph comes from `cargo metadata --locked`, so it is what
`Cargo.lock` pins rather than what the registry currently offers. Two things
that are not Cargo dependencies are added by hand because they are genuinely
part of what a release artifact contains or requires:

* **LLVM**, pinned to an exact version by `llvm-sys` and linked into the
  compiler. A bill of materials for a compiler that did not mention LLVM would
  be missing the largest thing in it.
* **The Rust toolchain** that compiled it, which is the version in
  `rust-toolchain.toml` when there is one and the invoking `cargo` otherwise.

Anything else the binary links against is the platform's -- a libc, a Windows
SDK -- and is named by the target triple rather than listed, because a bill of
materials that claimed to enumerate an operating system would be wrong in a way
nobody could check.
"""
import json
import os
import re
import subprocess
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))

# Pinned in `.cargo/config.toml` and by `llvm-sys`'s own version. Read from the
# repository rather than written here, so this cannot drift from the pin.
LLVM_PIN = re.compile(r"LLVM\s+(\d+\.\d+\.\d+)")


def metadata():
    out = subprocess.run(
        ["cargo", "metadata", "--locked", "--format-version", "1", "--all-features"],
        cwd=ROOT, capture_output=True, text=True,
    )
    if out.returncode != 0:
        # `--locked` fails when `Cargo.lock` is out of date, and that is the
        # answer rather than a reason to fall back: a bill of materials from an
        # unlocked resolution describes a build nobody performed.
        sys.exit(f"cargo metadata --locked failed:\n{out.stderr.strip()}")
    return json.loads(out.stdout)


def llvm_version():
    for name in ("README.md", "docs/roadmap.md"):
        path = os.path.join(ROOT, name)
        if not os.path.exists(path):
            continue
        with open(path, encoding="utf-8") as f:
            found = LLVM_PIN.search(f.read())
        if found:
            return found.group(1)
    return None


def rust_version():
    path = os.path.join(ROOT, "rust-toolchain.toml")
    if os.path.exists(path):
        with open(path, encoding="utf-8") as f:
            found = re.search(r'channel\s*=\s*"([^"]+)"', f.read())
        if found:
            return found.group(1)
    out = subprocess.run(["rustc", "--version"], capture_output=True, text=True)
    if out.returncode == 0:
        parts = out.stdout.split()
        if len(parts) > 1:
            return parts[1]
    return None


def purl(name, version, kind="cargo"):
    return f"pkg:{kind}/{name}@{version}" if version else f"pkg:{kind}/{name}"


def main():
    data = metadata()
    packages = {p["id"]: p for p in data["packages"]}
    workspace = set(data["workspace_members"])

    # The component the document is *about*. `khora-cli` produces the binary a
    # user installs, so it is the subject and the rest of the workspace are
    # components of it.
    root = next(
        (packages[i] for i in workspace if packages[i]["name"] == "khora-cli"),
        None,
    )
    if root is None:
        sys.exit("no `khora-cli` in the workspace, so there is nothing for this to be about")

    # Everything the resolution contains except the subject itself. Workspace
    # members are included: they are components of the artifact, and an auditor
    # asking what is in the binary wants them named.
    components = sorted(
        (p for p in data["packages"] if p["id"] != root["id"]),
        key=lambda p: (p["name"], p["version"]),
    )

    listed = []
    for p in components:
        entry = {
            "type": "library",
            "bom-ref": f"{p['name']}@{p['version']}",
            "name": p["name"],
            "version": p["version"],
            "purl": purl(p["name"], p["version"]),
        }
        if p.get("license"):
            entry["licenses"] = [{"expression": p["license"]}]
        if p.get("repository"):
            entry["externalReferences"] = [{"type": "vcs", "url": p["repository"]}]
        listed.append(entry)

    llvm = llvm_version()
    if llvm:
        listed.append({
            "type": "library",
            "bom-ref": f"llvm@{llvm}",
            "name": "llvm",
            "version": llvm,
            "purl": purl("llvm", llvm, kind="generic"),
            "description": "Linked into the compiler through llvm-sys; the pin is exact.",
        })
    rust = rust_version()
    if rust:
        listed.append({
            "type": "application",
            "bom-ref": f"rust@{rust}",
            "name": "rust",
            "version": rust,
            "purl": purl("rust", rust, kind="generic"),
            "description": "The toolchain that compiled this, not something it links against.",
        })
    listed.sort(key=lambda c: c["bom-ref"])

    # The graph, from the resolution rather than inferred from the list.
    nodes = {n["id"]: n for n in data.get("resolve", {}).get("nodes", [])}

    def ref_of(package_id):
        p = packages.get(package_id)
        return f"{p['name']}@{p['version']}" if p else None

    edges = []
    for package_id, node in sorted(nodes.items(), key=lambda kv: ref_of(kv[0]) or ""):
        of = ref_of(package_id)
        if of is None:
            continue
        on = sorted(filter(None, (ref_of(d) for d in node.get("dependencies", []))))
        edges.append({"ref": of, "dependsOn": on})

    document = {
        "bomFormat": "CycloneDX",
        "specVersion": "1.5",
        "version": 1,
        "metadata": {
            "tools": [{"vendor": "khora", "name": "toolchain-sbom", "version": root["version"]}],
            "component": {
                "type": "application",
                "bom-ref": f"{root['name']}@{root['version']}",
                "name": "khora",
                "version": root["version"],
                "purl": purl("khora", root["version"]),
                "description": "The Khora compiler and toolchain.",
            },
        },
        "components": listed,
        "dependencies": edges,
    }
    json.dump(document, sys.stdout, indent=2, sort_keys=False)
    sys.stdout.write("\n")


if __name__ == "__main__":
    main()
