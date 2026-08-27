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
| Go to definition | yes, paths and locals |
| Find references | yes, across files |
| Document outline and workspace search | yes |
| Completion | yes — after `.`, after `Type::`, in an import list, and in scope |
| Rename | **locals only.** A declaration is refused with a reason, see below |
| Semantic highlighting | yes — a local told apart from an import, a field from a method |

**References are found by resolution, not by matching text**, so they cross
files and two modules that each declare an `add` stay apart. The identity is the
declaration, not the spelling.

**Rename covers locals.** For a local the set of edits is provably complete —
one body, and the compiler already recorded which uses bind to which binding.
For a declaration it is not, and a rename that misses one occurrence breaks a
build silently in a file nobody had open, so renaming one is refused with a
sentence saying which two things are missing.

A method lands on its **trait** rather than its `impl`, because `khora-hir` does
not collect impl members. Completion does not have that problem: it reads
methods off `khora-types`' signature keys, which is where impl members *are*
recorded.

**Highlighting is two layers.** A TextMate grammar
(`editors/vscode/syntaxes`) colours keywords, literals and punctuation, and
every editor that reads TextMate grammars can use it. Over that, the server
sends semantic tokens for everything a regular expression cannot decide: a
local against an imported name, a parameter against a local, a field against a
method, and a path's module segments against what the path resolves to.

An editor with no TextMate grammar — Neovim, Helix — gets the semantic layer
alone, which is the more informative half.

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
