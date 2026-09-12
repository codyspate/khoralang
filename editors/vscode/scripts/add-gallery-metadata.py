"""Add the gallery metadata a marketplace listing needs.

Written as a script rather than by hand because `package.json` is also the
extension's manifest: key order is meaningful to a reader even where it is not
to a parser, and the insertion points are between existing keys rather than at
the end. Run once; the result is committed.
"""

import json
from collections import OrderedDict
from pathlib import Path

MANIFEST = Path("package.json")
existing = json.loads(MANIFEST.read_text(), object_pairs_hook=OrderedDict)

# Keywords are what the gallery's search matches on. Deliberately including the
# things somebody types when they do not yet know the name -- `.kh`, `effects`,
# `capabilities` -- rather than only the name they would already have to know.
additions = {
    "icon": "icon.png",
    "keywords": [
        "khora",
        "kh",
        "language server",
        "lsp",
        "effects",
        "capabilities",
        "systems programming",
    ],
    "galleryBanner": {"color": "#0b1120", "theme": "dark"},
    "homepage": "https://khoralang.com",
    "bugs": {"url": "https://github.com/codyspate/khoralang/issues"},
    # The gallery's Q&A tab defaults to a Microsoft-hosted forum nobody is
    # reading. Pointing it at the issue tracker sends a question where somebody
    # will see it.
    "qna": "https://github.com/codyspate/khoralang/issues",
}

# `repository` as a bare object is valid, but the gallery renders the
# `directory` field as a link into the subtree, which for a monorepo is the
# difference between "here is the extension" and "here is a compiler".
existing["repository"] = OrderedDict(
    [
        ("type", "git"),
        ("url", "https://github.com/codyspate/khoralang"),
        ("directory", "editors/vscode"),
    ]
)

# Placed after `license` so the descriptive keys stay together at the top,
# ahead of the mechanical ones (`engines`, `main`, `contributes`).
out = OrderedDict()
for key, value in existing.items():
    out[key] = value
    if key == "license":
        for name, added in additions.items():
            out[name] = added

MANIFEST.write_text(json.dumps(out, indent=2, ensure_ascii=False) + "\n")
print("added:", ", ".join(additions))
print("keys:", " ".join(list(out.keys())[:12]))
