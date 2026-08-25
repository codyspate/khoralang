---
title: Editor setup
sidebar:
  order: 3
---

Khora ships a language server backed by the same compiler queries used by `khora check`.

## Start the language server

```bash
khora lsp
```

During source development:

```bash
cargo run -p khora-cli -- lsp
```

The current language server provides diagnostics and hover information. Completion, rename, and richer capability-oriented editor features are planned before the production release.

## Formatting

Use the compiler's formatter rather than editor-specific formatting rules:

```bash
khora fmt .
```

For CI:

```bash
khora fmt . --check
```

## AI coding tools

Khora also exposes an MCP server so an AI coding agent can ask the real compiler rather than guessing syntax from other languages:

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

The most important MCP operation is compiler-backed checking of snippets. This is particularly useful before Khora has enough public code for models to have learned the language independently.
