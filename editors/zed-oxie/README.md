# Oxie for Zed

Support for the [Oxie programming language](../../README.md) in the [Zed](https://zed.dev) editor:

* syntax highlighting (with `rust { }` blocks highlighted as Rust), outline, bracket matching,
  indentation and text objects (tree-sitter grammar from `../tree-sitter-oxie`);
* the Oxie language server: diagnostics (Oxie and rustc errors mapped to `.oxi` lines),
  completion, hover with the generated Rust, go to definition, references, rename, signature
  help, formatting, inlay hints;
* ▶ run buttons on `fun main` and on each `test "..."` block, and `oxie` tasks (`oxie run`,
  `oxie run --release`, `oxie test`, `oxie emit` to see the generated Rust);
* snippets.

## Install

1. Install the `oxie` command (the language server is `oxie lsp`):

   ```bash
   brew install oxie-lang/tap/oxie
   ```

   or `cargo install oxie`. To use another binary, or to turn off `cargo check` on save, set it
   in Zed's settings:

   ```json
   {
     "lsp": {
       "oxie": {
         "binary": { "path": "/path/to/oxie", "arguments": ["lsp"] },
         "initialization_options": { "cargoCheck": false }
       }
     }
   }
   ```

2. In Zed: **zed: install dev extension** from the command palette, and choose this folder. Zed
   needs Rust installed with rustup for that: it compiles `src/lib.rs` (which starts `oxie lsp`)
   to WebAssembly, and builds the grammar from the commit in `extension.toml`.

## Updating

The language files and snippets are copies of the Kuana extension's (`../kuana-oxie`), so the
queries live in one place. Edit `../tree-sitter-oxie/grammar.js` or its `queries/`, run
`../build-kuana-extension.sh` (which runs `../build-zed-extension.sh` at the end), commit the
grammar, and run `../build-zed-extension.sh` again: Zed builds the grammar from the repository,
so `extension.toml` pins the last commit that changed `editors/tree-sitter-oxie`. Then click
**Rebuild** on the extension in Zed's Extensions page.
