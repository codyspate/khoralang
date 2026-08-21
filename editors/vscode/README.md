# Khora for VS Code

Syntax highlighting and editor configuration for `.kh` files.

## Install

```bash
powershell -NoProfile -ExecutionPolicy Bypass -File editors/vscode/install.ps1
```

Then **fully quit and reopen VS Code** — extensions are scanned at startup, so
reloading the window is not enough. Confirm with `code --list-extensions`, which
should list `khora-lang.khora`; the status bar should read *Khora* on any `.kh`
file.

The script packages the extension as a `.vsix` and installs it through the VS
Code CLI. It needs no npm — a `.vsix` is a zip with an OPC manifest, which
PowerShell builds directly.

**Do not just drop the folder into `~/.vscode/extensions`.** That was the first
approach here and it silently did nothing: the extension never entered VS Code's
extension index, so nothing loaded and there was no error to see. A junction has
the same problem.

Because this is a real install rather than a link, **re-run the script after
editing the grammar**, then restart.

On macOS and Linux, install `@vscode/vsce` and run `vsce package`, then
`code --install-extension khora-lang.khora-0.1.0.vsix`.

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
