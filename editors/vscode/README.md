# Khora for VS Code

Errors as you type, hover types, format on save, and syntax highlighting for
`.kh` files.

**Everything but the highlighting comes from the compiler.** The extension
starts `khora lsp` — a subcommand of the toolchain rather than a second program
to install — and shows what it says. So a diagnostic in the editor is the same
diagnostic `khora check` gives, from the same query, and there is no second
implementation to drift.

Requires `khora` on `PATH`, which both installers arrange. Point
`khora.server.path` at an executable to use a different one, which is what to
do when working on the compiler itself.

## What it does

| | from |
| --- | --- |
| Errors and warnings as you type | `khora_types::diagnostics` and `khora_lint::findings` |
| Hover: the type of the thing under the cursor | the checker's `BodyTypes` |
| Go to definition, on a `::` path | `khora_hir::resolve_path` |
| Format on save | `khora_fmt`, the same formatter the baseline gates on |
| Highlighting, comment toggling, bracket matching | the TextMate grammar here |

Format on save is on by default for `.kh` and nothing else — `package.json`
scopes it under `[khora]`, so it cannot turn the setting on for a language
somebody has deliberately left it off for.

Two commands, both under **Khora:** in the palette — *Restart Language Server*,
for after rebuilding the compiler, and *Show Language Server Output*, which is
where the server's own complaints go. `khora.trace.server` puts the protocol
traffic in the same place.

## Working on it

Open **this folder** (`editors/vscode`) in VS Code, run `npm install` once, and
press **F5**. That launches an Extension Development Host — a second window with
the extension loaded live. Reload the host window after editing `src/`.

## Installing it for real

```bash
cd editors/vscode
npm install
npm run package
code --install-extension khora-lang.khora.vsix --force
```

Then **fully quit and reopen VS Code** — extensions are scanned at startup, so
reloading the window is not enough. Confirm with `code --list-extensions`, which
should list `khora-lang.khora`.

`install.ps1` builds a `.vsix` without npm, by hand, from PowerShell. It was
written when this machine had no Node and is kept for a machine that has none;
it packages the declarative half only, so it cannot produce a working client.

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

**Semantic tokens over LSP is the real answer** (roadmap 14.5). It is
editor-agnostic by construction and reuses the compiler instead of duplicating
it, which is the only way `Type.member` can be colored correctly — see the
limits section below. The client half is now here, so what that needs is the
server half.

A tree-sitter grammar would mean maintaining a *second* parser alongside the
`rowan` one. The cost of even a single duplicate is visible here: the
`keywords_match_the_lexer` test exists to stop this grammar drifting from the
lexer, and it has already caught a real break. Worth doing only if Neovim,
Helix or Zed users are wanted before the language server ships.

The TextMate grammar earns its place regardless of the LSP, because GitHub
Linguist uses it to highlight `.kh` in the repository and on the web.

## What the grammar covers

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
