-- Khora in Neovim.
--
-- `source` this, or paste it into your config. Neovim 0.11 and later; the
-- README beside it has the `nvim-lspconfig` form for older versions.
--
-- Nothing to install but `khora` itself: the server is `khora lsp`, a
-- subcommand of the toolchain rather than a second binary.

vim.filetype.add({ extension = { kh = "khora" } })

vim.lsp.config.khora = {
  cmd = { "khora", "lsp" },
  filetypes = { "khora" },
  -- `khora.toml` first, because the server reads the whole package at
  -- `initialize` -- cross-file name resolution needs one source root, and a
  -- root guessed from `.git` in a monorepo would hand it far too much.
  root_markers = { "khora.toml", ".git" },
}

vim.lsp.enable("khora")

-- Format on save, through the same formatter `khora fmt` runs and the baseline
-- gates on. Synchronous, because an async format that lands after the write is
-- a file saved in the state it was not formatted in.
vim.api.nvim_create_autocmd("BufWritePre", {
  pattern = "*.kh",
  callback = function()
    vim.lsp.buf.format({ async = false })
  end,
})
