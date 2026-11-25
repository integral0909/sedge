# Editor support

`sedge lsp` is a language server on stdio. Any LSP client can use it.

## VS Code

`vscode/` is a minimal extension: syntax highlighting and a client that
starts `sedge lsp`. To try it from a checkout:

```sh
cd editors/vscode
npm install
code --extensionDevelopmentPath="$PWD" ../../examples
```

Set `sedge.path` if `sedge` is not on your `PATH`.

## Neovim

```lua
vim.filetype.add({ extension = { sg = "sedge" } })
vim.api.nvim_create_autocmd("FileType", {
  pattern = "sedge",
  callback = function()
    vim.lsp.start({ name = "sedge", cmd = { "sedge", "lsp" }, root_dir = vim.fn.getcwd() })
  end,
})
```

## Helix

In `languages.toml`:

```toml
[language-server.sedge]
command = "sedge"
args = ["lsp"]

[[language]]
name = "sedge"
scope = "source.sedge"
file-types = ["sg"]
comment-token = "//"
language-servers = ["sedge"]
```

## Other editors

`vscode/syntaxes/sedge.tmLanguage.json` is a standard TextMate grammar,
which Sublime Text, Zed and many other editors can load.
