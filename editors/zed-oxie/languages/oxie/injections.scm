; `rust { ... }` blocks contain plain Rust
((call_expression
  function: (identifier) @_rust
  (block) @injection.content)
  (#eq? @_rust "rust")
  (#set! injection.language "rust"))
