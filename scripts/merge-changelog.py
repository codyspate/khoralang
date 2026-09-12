"""Merge `## Unreleased` into `## 0.2.0`, which is what cutting the tag means.

The file grew two sections for one release: a `0.2.0` heading written a week
ago when the editor work landed, and `Unreleased` for everything since. Nothing
was ever tagged, so both describe the same unreleased release and a reader
seeing `0.2.0 — 2026-09-04` above entries written on the 11th would reasonably
conclude the file was lying.

Subsections are merged by name and kept in the file's own order -- Breaking,
Fixed, Changed, Added, Documentation, Editor -- rather than concatenated, so
one release does not end up with two `### Fixed` headings.
"""

import re
import sys
from pathlib import Path

CHANGELOG = Path("CHANGELOG.md")
text = CHANGELOG.read_text()

start_020 = text.index("## 0.2.0 — 2026-09-04")
start_unrel = text.index("## Unreleased")
start_010 = text.index("## 0.1.0 — 2026-09-03")

head = text[:start_020]
body_020 = text[start_020:start_unrel]
body_unrel = text[start_unrel:start_010]
tail = text[start_010:]


def subsections(block):
    """`### Name` -> body, in the order they appear."""
    out = {}
    parts = re.split(r"^### ", block, flags=re.M)
    for part in parts[1:]:
        name, _, rest = part.partition("\n")
        out[name.strip()] = rest.strip("\n")
    return out


old = subsections(body_020)
new = subsections(body_unrel)

# `Unreleased`'s entries are the newer work, so they lead each subsection: a
# reader scanning `### Fixed` should meet the denial of service before three
# diagnostic improvements.
ORDER = ["Breaking", "Fixed", "Changed", "Added", "Documentation", "Editor"]
merged = {}
for name in ORDER:
    pieces = [new[name] for name in [name] if name in new]
    pieces += [old[name] for name in [name] if name in old]
    if pieces:
        merged[name] = "\n\n".join(pieces)

unknown = (set(old) | set(new)) - set(ORDER)
if unknown:
    sys.exit(f"unplaced subsection(s), refusing to guess: {sorted(unknown)}")

intro = """## 0.2.0 — 2026-09-11

Editor work, a manifest change, and -- from five rounds of strangers building
real programs against the published documentation -- four runtime failures and
an installer that could not be run as root.

The one to read first is the denial of service in `std::json::parse`: a service
built the way `cookbook/json-api` describes could be stopped by a single
request carrying a long string.
"""

parts = [intro]
for name in ORDER:
    if name in merged:
        parts.append(f"### {name}\n\n{merged[name]}\n")

CHANGELOG.write_text(head + "\n".join(parts) + "\n" + tail)
print("merged; subsections:", ", ".join(n for n in ORDER if n in merged))
