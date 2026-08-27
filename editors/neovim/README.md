# Khora in Neovim

`khora.lua` beside this file is a complete configuration for Neovim 0.11 and
later. `source` it, or paste it in.

Nothing to install but `khora` itself. The server is `khora lsp`, a subcommand
of the toolchain.

## Before 0.11

`vim.lsp.config` arrived in 0.11. Earlier versions go through
[`nvim-lspconfig`](https://github.com/neovim/nvim-lspconfig), which has no
Khora entry, so register one:

```lua
vim.filetype.add({ extension = { kh = "khora" } })

local configs = require("lspconfig.configs")
local util = require("lspconfig.util")

if not configs.khora then
  configs.khora = {
    default_config = {
      cmd = { "khora", "lsp" },
      filetypes = { "khora" },
      root_dir = util.root_pattern("khora.toml", ".git"),
      single_file_support = true,
    },
  }
end

require("lspconfig").khora.setup({})
```

## What you get

Errors and warnings as you type, the type of the thing under the cursor on
hover, and formatting — all from the compiler, so they agree with
`khora check` and `khora fmt` exactly.

**Highlighting comes from the server**, not from tree-sitter. Khora has no
tree-sitter grammar — a second parser is a cost rather than a feature, and
`editors/vscode/README.md` has the argument — so what colours a buffer here is
LSP semantic tokens, which Neovim applies on its own with no configuration.
It covers what needs resolution: locals, parameters, fields, methods, modules
and what a path resolves to. Keywords and literals are not in it.

`vim.lsp.buf.definition()`, `references()`, `document_symbol()` and completion
all work. `vim.lsp.buf.rename()` works on a local and refuses a declaration with
a reason.

## Checking it

```
:checkhealth vim.lsp
:LspInfo          " nvim-lspconfig
```

Or, without Neovim in the way:

```
khora lsp
```

which sits waiting for a `Content-Length` header. That is the whole protocol
handshake, and it says so if you run it in a terminal by mistake.
