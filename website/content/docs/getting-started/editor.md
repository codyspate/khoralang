---
title: Editor setup
sidebar:
  order: 3
---

Khora ships one language server as part of the compiler toolchain: `khora lsp`. It is backed by the same compiler queries used by `khora check`, so the diagnostics and type information you see in your editor come from the compiler itself.

Your editor should launch the server for `.kh` files; you normally do not need to run it in a terminal yourself.

## VS Code

The extension is not on the Marketplace yet. Install it from a release:

1. Download `khora-lang.khora.vsix` from the newest `vscode-v*` release on GitHub.
2. Run `code --install-extension khora-lang.khora.vsix`, or use **Extensions: Install from VSIX** in the command palette.

It needs `khora` on your `PATH`, which both installers arrange. If it cannot find one it says so and offers to open the `khora.server.path` setting rather than failing quietly. Format-on-save is turned on for `.kh` files only.

The status bar shows which toolchain answered, and turns yellow when a project pins a version that is not installed.

## Language server command

Configure an LSP client with:

```text
command: khora
args:    lsp
```

The server currently provides:

- compiler diagnostics and hover information;
- formatting;
- completion and signature help;
- go-to-definition and find-references;
- document and workspace symbols;
- semantic tokens;
- inlay hints;
- code actions and code lenses;
- highlighting every mention of the name under the cursor;
- local-variable rename.

Rename is deliberately conservative. The current implementation renames locals and refuses a symbol it cannot edit completely rather than applying a partial project-wide change.

The installed toolchain is the only server installation you need. Updating or pinning Khora also selects the compiler behavior your editor sees for that project.

## Formatting

Use Khora's formatter rather than editor-specific formatting rules:

```bash
khora fmt .
```

For CI:

```bash
khora fmt . --check
```

Editors that support LSP formatting can delegate format-on-save to `khora lsp`.

## Editor clients

Any editor with a Language Server Protocol client can launch `khora lsp`. The repository ships working configuration for VS Code, Helix and Neovim. Emacs and Sublime Text have LSP snippets in `editors/README.md` that nobody has run; they are a starting point rather than a supported setup. Zed needs an extension compiled to WebAssembly and has none.

The important part is always the same: the project root contains `khora.toml`, `.kh` files are associated with Khora, and the editor starts `khora lsp`.

## AI coding tools

Khora also exposes a compiler-backed MCP server. A coding agent can use it to ask the real Khora compiler about source instead of guessing syntax or type behavior.

It is optional. Nothing about writing, building or testing correct Khora requires it, and the compiler and language server behave identically whether or not an agent is connected.

A typical MCP client configuration is:

```json
{
  "mcpServers": {
    "khora": {
      "command": "khora",
      "args": ["mcp"]
    }
  }
}
```

As with the language server, the client starts the process for you. You normally do not run `khora mcp` interactively.

## Next

- [Your first Khora project](/docs/getting-started/first-project/) shows the command-line workflow the editor complements.
- The [Language Reference](/docs/reference/) covers the language features the server is checking.
- [Language Reference](/docs/reference/) is the lookup-oriented companion when you need exact syntax or semantics.
