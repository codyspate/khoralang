The Khora extension for Visual Studio Code.

## Install

Download `khora-vscode-<version>.vsix` below, then:

```
code --install-extension khora-vscode-<version>.vsix
```

or in VS Code: **Extensions**, the `...` menu, **Install from VSIX**.

Then fully quit and reopen VS Code — extensions are scanned at startup, so
reloading the window is not enough. `code --list-extensions` should list
`khora-lang.khora`.

## It needs the toolchain

The extension is a client. It starts `khora lsp`, a subcommand of the compiler,
and shows what the compiler says — so it needs `khora` on your `PATH`. Install
it from the [toolchain releases](https://github.com/codyspate/khoralang/releases),
or point `khora.server.path` at an executable, which is what to do when working
on the compiler itself.

The two are released separately and on purpose. This extension has no compiler
in it, versions on its own, and changes for its own reasons; a fix to hover
rendering should not wait for a language release or drag one along.

## What you get

Everything but the syntax highlighting is answered by the compiler, over LSP,
from the same queries `khora check` runs. There is no second implementation to
drift.

- Errors and warnings as you type, including lints
- Hover types, go to definition, find references, rename
- Completion: methods after `.`, a type's own items after `::`, a module's
  exports inside an import list
- Semantic highlighting the resolver decides — a local told from an import,
  which no regex can do
- **Inlay hints showing what a call costs** — the `with { .. }` capabilities it
  requires and the errors it `raises`, which is the thing worth reading in a
  language with effects
- Signature help, quick fixes, document and workspace symbols, run lenses
- Format on save, by `khora fmt`, scoped to `.kh` only

## Settings

| | |
| --- | --- |
| `khora.server.path` | an executable to use instead of `khora` on `PATH` |
| `khora.trace.server` | protocol traffic, into the server's output channel |

Two commands, both under **Khora:** in the palette — *Restart Language Server*,
for after rebuilding the compiler, and *Show Language Server Output*.
