# oxie-build

The `build.rs` helper of [Oxie](https://github.com/oxie-lang/oxie) projects: it compiles the `.oxi` files in `src/` to Rust
before each build. `oxie new` sets it up for you:

```toml
[build-dependencies]
oxie-build = "0.6"
```

```rust
// build.rs
fn main() {
    oxie_build::compile();
}
```

```rust
// src/main.rs
include!(concat!(env!("OUT_DIR"), "/oxie/main.rs"));
```

See [Tooling](https://oxie.bkymz.net/tooling/) for the project layout and
[Rust interop](https://oxie.bkymz.net/rust-interop/) for adding Oxie to an existing crate.

Licensed under either of [Apache License, Version 2.0](https://github.com/oxie-lang/oxie/blob/main/LICENSE-APACHE) or
[MIT license](https://github.com/oxie-lang/oxie/blob/main/LICENSE-MIT) at your option.
