---
title: Editor setup
sidebar:
  order: 3
---

Khora ships one language server as part of the compiler toolchain: `khora lsp`. It is backed by the same compiler queries used by `khora check`, so the diagnostics and type information you see in your editor come from the compiler itself.

Your editor should launch the server for `.kh` files; you normally do not need to run it in a terminal yourself.

## Language server command

Configure an LSP client with:

```text
command: khora
args:    lsp
```

The server provides compiler-backed diagnostics, hover information, formatting, go-to-definition, references, document/workspace symbols, and navigation for local and module-level names. Rename is performed where the server can produce a complete safe edit; when it cannot, it refuses the rename rather than applying a partial change.

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

Any editor with a Language Server Protocol client can launch `khora lsp`. The repository includes ready-to-use configuration or integration examples for VS Code, Helix, Neovim, Emacs, and Sublime Text.

The important part is always the same: the project root contains `khora.toml`, `.kh` files are associated with Khora, and the editor starts `khora lsp`.

## AI coding tools

Khora also exposes a compiler-backed MCP server. A coding agent can use it to ask the real Khora compiler about source instead of guessing syntax or type behavior.

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
- [Language Guide](/docs/guide/) teaches the language features the server is checking.
- [Language Reference](/docs/reference/) is the lookup-oriented companion when you need exact syntax or semantics.
