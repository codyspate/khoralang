# Khora for VS Code

Syntax highlighting and editor configuration for `.kh` files.

## Install

The extension is a plain folder — link it into VS Code's extensions directory
and reload the window. No packaging or publishing needed for local development.

Windows (no admin required):

```bash
cmd //c mklink //J "%USERPROFILE%\.vscode\extensions\khora-lang.khora-0.1.0" "%CD%\editors\vscode"
```

macOS and Linux:

```bash
ln -s "$PWD/editors/vscode" ~/.vscode/extensions/khora-lang.khora-0.1.0
```

Then run **Developer: Reload Window** from the command palette. Open any `.kh`
file to check it took; the language indicator in the status bar should read
*Khora*.

Because it is a link rather than a copy, edits to the grammar take effect on the
next window reload.

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
scopes or types. Under Khora's "universal dot" rule, `Effect.map`,
`report.risk` and `RiskLevel.Low` are syntactically identical, so a regex
cannot tell a module path from a field access from a constructor. The grammar
approximates by capitalisation — capitalised identifiers are coloured as types.

Precise colouring needs semantic tokens from the language server (roadmap phase
8.4), which will layer over this grammar the same way rust-analyzer layers over
Rust's. That is the intended end state; this is the base layer.

The `:label.operation` rule is provisional and disappears with decision A8
(direct-style algebraic effects). See `docs/roadmap.md` D7.

## Keeping it in sync

The keyword list here is a copy of what the lexer accepts, which is exactly the
kind of duplication that rots. `crates/khora-syntax/tests/editor_grammar.rs`
fails the build if the two disagree, so adding a keyword to the compiler without
updating this grammar is caught by `cargo test`.
