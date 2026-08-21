# Khora for VS Code

Syntax highlighting and editor configuration for `.kh` files.

## Working on it

Open **this folder** (`editors/vscode`) in VS Code and press **F5**. That
launches an Extension Development Host — a second window with the extension
loaded live — which is how VS Code extensions are normally developed. No
packaging, no install, and no npm, because this extension contributes only
declarative grammar and configuration.

Restart the host window after editing the grammar.

## Installing it for real

```bash
powershell -NoProfile -ExecutionPolicy Bypass -File editors/vscode/install.ps1
```

Then **fully quit and reopen VS Code** — extensions are scanned at startup, so
reloading the window is not enough. Confirm with `code --list-extensions`, which
should list `khora-lang.khora`.

The script packages a `.vsix` and installs it through the VS Code CLI. It exists
because this machine has no Node; the standard tool is
`npm i -g @vscode/vsce` then `vsce package`, and that is what to use for
publishing. A `.vsix` is a zip with an OPC manifest, which is why PowerShell can
build one at all.

**Do not just drop the folder into `~/.vscode/extensions`.** That was the first
approach here and it silently did nothing: the extension never entered VS Code's
extension index, so nothing loaded and no error appeared. A junction fails the
same way.

## Other editors

Syntax support is three separate layers, and this file is only the first:

| Layer | Reaches | Nature |
| --- | --- | --- |
| TextMate grammar (this) | VS Code, Sublime, TextMate, IntelliJ, GitHub Linguist | Regex; cannot be correct for Khora |
| Tree-sitter | Neovim, Helix, Zed, Emacs 29+, GitHub code navigation | Incremental parser, grammar in JS compiled to C |
| LSP semantic tokens | Every editor with an LSP client | Driven by the actual compiler |

**Semantic tokens over LSP is the real answer** (roadmap phase 8.4). It is
editor-agnostic by construction and reuses the compiler instead of duplicating
it, which is the only way `Type.member` can be colored correctly — see the
limits section below.

A tree-sitter grammar would mean maintaining a *second* parser alongside the
`rowan` one. The cost of even a single duplicate is visible here: the
`keywords_match_the_lexer` test exists to stop this grammar drifting from the
lexer, and it has already caught a real break. Worth doing only if Neovim,
Helix or Zed users are wanted before the language server ships.

The TextMate grammar earns its place regardless of the LSP, because GitHub
Linguist uses it to highlight `.kh` in the repository and on the web.

## What it does

- Highlighting for keywords, types, functions, strings, numbers, row variables
  (`'r`) and operators, with `|>` scoped separately so the language's signature
  operator stands out.
- Nested block comments, matching the lexer.
- `//` and `/* */` comment toggling, bracket matching, auto-closing pairs.

`<` and `>` are deliberately **not** auto-closing pairs. They are comparison
operators as often as they are type brackets, and auto-closing them is more
annoying than helpful.

## Limits

This is a TextMate grammar: it pattern-matches text and knows nothing about
scopes or types.

It used to be much worse. Under the old "universal dot" rule, `Effect.map`,
`report.risk` and `RiskLevel.Low` were syntactically identical, and no regex
could tell a module path from a field access from a constructor — the grammar
could only approximate by capitalization. Splitting `::` from `.` (errata 13)
removed that limit: a path is now visibly a path, so modules, types,
constructors and associated items color correctly, and a name after `.` is
never mistaken for a type.

What a regex still cannot do is anything needing resolution — telling a local
binding from an imported name, or a field from a method. That wants semantic
tokens from the language server (roadmap phase 8.4), which will layer over this
grammar the same way rust-analyzer layers over Rust's. This is the base layer,
and it now carries more of the load than planned.

## Keeping it in sync

The keyword list here is a copy of what the lexer accepts, which is exactly the
kind of duplication that rots. `crates/khora-syntax/tests/editor_grammar.rs`
fails the build if the two disagree, so adding a keyword to the compiler without
updating this grammar is caught by `cargo test`.

There are two lists, and they live in separate repository rules:

- `#keywords` mirrors `KEYWORDS` — the hard keywords, matched as bare words.
- `#contextual-keywords` mirrors `CONTEXTUAL_KEYWORDS` — `handler`, `for`,
  `context`, `test` and `bench`, which are ordinary identifiers everywhere
  except one position each. Matching them as bare words would color a
  parameter named `handler` or a variable named `test`, so each rule instead
  reproduces the position the parser recognizes: `handler` before `for`,
  `context` at the start of a declaration, `test`/`bench` before a name string.
  These approximations are exactly the kind of thing semantic tokens will
  replace.
