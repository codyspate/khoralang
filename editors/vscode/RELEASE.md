The Khora extension for Visual Studio Code.

## Upgrading from 0.3.0 or earlier

The extension's ID is `khora.khora`. Version 0.3.0 was published as
`khora-lang.khora`, which VS Code treats as a different extension, so remove
it first or both will run:

```
code --uninstall-extension khora-lang.khora
```

## Changes in 0.3.2

- **The language server starts.** 0.3.0 and 0.3.1 launched `khora lsp --stdio`, which
  every released toolchain refuses with *unexpected argument '--stdio'*, so
  the server exited before it answered and nothing but syntax colouring
  worked. It launches `khora lsp`.

## Changes in 0.3.1

- The publisher is `khora`, the name the project owns on the Marketplace.
- *Install Khora*, offered when no `khora` is found, opens the installation
  page on khoralang.com. In 0.3.0 it opened a domain that does not resolve.
- The status bar shows which toolchain answered, and turns yellow when a
  project pins a version that is not installed.
- Highlighting for character literals (`'a'`), `row` declarations, `///` doc
  comments, string interpolation, and the `?`, `..` and comparison operators.
- Requires VS Code 1.82 or later.

## Install

Download `khora-vscode-<version>.vsix` below, then:

```
code --install-extension khora-vscode-<version>.vsix
```

or in VS Code: **Extensions**, the `...` menu, **Install from VSIX**.

Then fully quit and reopen VS Code — extensions are scanned at startup, so
reloading the window is not enough. `code --list-extensions` should list
`khora.khora`.

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
