# Khora in your editor

**There is one implementation, and it is `khora lsp`.** Errors as you type,
hover types, formatting and go to definition are answered by the compiler
itself over the Language Server Protocol, so every editor below gets the same
answers from the same queries. A diagnostic in your editor is the diagnostic `khora check`
gives.

That is why this directory is mostly configuration. `editors/vscode` has a
hundred lines of JavaScript because VS Code needs a client object; every other
editor here has an LSP client already and needs to be told a command and a file
extension.

Everything needs the same two things:

- `khora` on `PATH` — both installers arrange it.
- Nothing else. The server is a subcommand, not a second download.

## VS Code

`editors/vscode`, which is packaged and installed rather than configured. See
its README.

## Helix

Helix has an LSP client built in, so this is configuration only. Merge
`helix/languages.toml` into `~/.config/helix/languages.toml` (or
`%AppData%\helix\languages.toml`):

```toml
[language-server.khora]
command = "khora"
args = ["lsp"]

[[language]]
name = "khora"
scope = "source.khora"
file-types = ["kh"]
comment-token = "//"
indent = { tab-width = 4, unit = "    " }
language-servers = ["khora"]
auto-format = true
```

`hx --health khora` says whether it found the binary.

## Neovim

Neovim 0.11 and later:

```lua
vim.filetype.add({ extension = { kh = "khora" } })

vim.lsp.config.khora = {
  cmd = { "khora", "lsp" },
  filetypes = { "khora" },
  root_markers = { "khora.toml", ".git" },
}
vim.lsp.enable("khora")

-- Format on save.
vim.api.nvim_create_autocmd("BufWritePre", {
  pattern = "*.kh",
  callback = function() vim.lsp.buf.format({ async = false }) end,
})
```

`editors/neovim/khora.lua` is the same, ready to `source`. Older Neovim uses
`nvim-lspconfig`'s `configs` table with the same `cmd`; the README there has it.

## Emacs

Eglot, which ships with Emacs 29 and later:

```elisp
(add-to-list 'auto-mode-alist '("\\.kh\\'" . prog-mode))
(with-eval-after-load 'eglot
  (add-to-list 'eglot-server-programs '(prog-mode . ("khora" "lsp"))))
```

A `khora-mode` deriving from `prog-mode` is the tidier version and is not
written; the above works without one.

## Sublime Text

The **LSP** package, with this in its settings:

```json
{
  "clients": {
    "khora": {
      "enabled": true,
      "command": ["khora", "lsp"],
      "selector": "source.khora"
    }
  }
}
```

Sublime reads TextMate grammars directly, so `editors/vscode/syntaxes` gives it
highlighting as-is.

## Zed

Not here, and it is the one that needs real work rather than a snippet. Zed has
no way to register a language server from settings alone — a new language needs
an extension, which is a Rust crate compiled to WebAssembly. Small, but it is
code with a build, and shipping half of one would be worse than saying so.

## What every editor gets, and what it does not

| | |
| --- | --- |
| Errors and warnings as you type | yes |
| Hover: the type under the cursor | yes |
| Format on save | yes, where the editor can be told to format on save |
| Go to definition | yes, for `::` paths — not for locals, see below |
| Completion, find references, rename | **not yet** — roadmap 14.4, 14.8 |
| Semantic highlighting | **not yet** — roadmap 14.5 |

Go to definition answers a `::` path — a function, type, trait, effect, context
or constant, and a constructor by way of the type that declares it. It does not
answer a local binding: that resolves in a body rather than in the module, and
it is the case where the answer is already three lines up the screen. 14.8 wants
the same information for rename, so it is a gap to fill once rather than twice.

A method lands on its **trait** rather than its `impl`, because `khora-hir` does
not collect impl members.

Highlighting today is a TextMate grammar (`editors/vscode/syntaxes`), which
regex-matches text and cannot tell a local from an import. Every editor that
reads TextMate grammars can use it; the real answer is semantic tokens from the
server, which is 14.5.

## A note on what is tested

`crates/khora-cli/tests/lsp.rs` starts `khora lsp` as a subprocess and speaks
the protocol to it over pipes — the same path every client here takes. What it
covers is that the server starts, answers `initialize` with the capabilities
these configurations rely on, publishes diagnostics for a file with a mistake
in it, keeps stdout free of anything that is not a protocol frame, and exits
when told to.

**The configurations themselves are not tested**, and only the VS Code one has
been run. They follow each editor's documented form; if one is wrong, the fix
is a pull request and the server underneath it is the part that was worth
proving.
