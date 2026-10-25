<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="docs/assets/oxie-logo-dark.svg">
    <img src="docs/assets/oxie-logo-light.svg" alt="Oxie logo" height="120">
  </picture>
</p>

# Oxie

**Oxie is a programming language for the Rust ecosystem: a concise, familiar syntax that compiles
to plain Rust.**
Every crate works, programs run exactly as fast as hand-written Rust, and you never write
`let mut`, `impl` blocks, `Ok(...)`, `.to_string()` on literals or `#[derive(...)]` boilerplate
unless you want to.

Documentation: **[oxie.bkymz.net](https://oxie.bkymz.net)** · **[AI coding agents](https://oxie.bkymz.net/ai-agents/)**

> **Built for AI coding agents.** Oxie works with Claude Code, Codex, Cursor, and any agent that
> reads `AGENTS.md`: `oxie new` gives every project one, `oxie guide` teaches the agent the exact
> Oxie version installed, and `oxie check` explains what it gets wrong, in Oxie terms and with the
> fix. [Set it up in a minute](#ai-coding-agents).

```oxie
error enum BankError {
    Insufficient(needed: u64, available: u64) = "need $needed, have $available"
    Frozen = "account is frozen"
}

data class Account(owner: String, balance: u64 = 0, frozen: bool = false) {
    fun deposit(amount: u64) { balance += amount }

    fun withdraw(amount: u64): u64 throws BankError {
        if (frozen) throw BankError.Frozen
        if (amount > balance) throw BankError.Insufficient(amount, balance)
        balance -= amount
        balance
    }

    override fun toString() = "$owner: $balance"
}

fun main() {
    var account = Account("Ada")
    account.deposit(100)

    when (account.withdraw(250)) {
        Ok(left) -> println("withdrew, $left left")
        Err(e) -> println("failed: $e")
    }
    println("$account")
}
```

The same program in Rust. This is what `oxie emit` generates, tidied the way you would write it by
hand (shorter `std` paths, no `pub(crate)`):

```rust
use std::fmt;

#[derive(Debug)]
enum BankError {
    Insufficient { needed: u64, available: u64 },
    Frozen,
}

impl fmt::Display for BankError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Insufficient { needed, available } => write!(f, "need {needed}, have {available}"),
            Self::Frozen => f.write_str("account is frozen"),
        }
    }
}

impl std::error::Error for BankError {}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct Account {
    owner: String,
    balance: u64,
    frozen: bool,
}

impl Account {
    fn new(owner: String, balance: u64, frozen: bool) -> Self {
        Self { owner, balance, frozen }
    }

    fn deposit(&mut self, amount: u64) {
        self.balance += amount;
    }

    fn withdraw(&mut self, amount: u64) -> Result<u64, BankError> {
        if self.frozen {
            return Err(BankError::Frozen);
        }
        if amount > self.balance {
            return Err(BankError::Insufficient { needed: amount, available: self.balance });
        }
        self.balance -= amount;
        Ok(self.balance)
    }
}

impl fmt::Display for Account {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.owner, self.balance)
    }
}

fn main() {
    let mut account = Account::new(String::from("Ada"), 0, false);
    account.deposit(100);

    match account.withdraw(250) {
        Ok(left) => println!("withdrew, {left} left"),
        Err(e) => println!("failed: {e}"),
    }
    println!("{account}");
}
```

Oxie wrote the derives, the constructor, both `Display` impls, the `Error` impl, `&mut self`,
`let mut`, `Ok(...)`, `String::from` and the default arguments for you. The program runs exactly
as fast as the Rust version, because it *is* the Rust version.

## Why

Rust is fast, safe and has a fantastic ecosystem, but its syntax is dense. Oxie keeps Rust's
semantics — ownership, borrowing, traits, zero-cost abstractions — and gives it the syntax
Kotlin and TypeScript developers already know:

| You write (Oxie) | Oxie generates (Rust) |
|---|---|
| `val x = 1` / `var y = 2` | `let x = 1;` / `let mut y = 2;` |
| `fun area(): f64 = w * h` | `fn area(&self) -> f64 { self.w * self.h }` |
| `data class Point(x: f64, y: f64)` | struct + constructor + `#[derive(Debug, Clone, Copy, PartialEq)]` |
| `"Hello, $name! ${x + 1}"` | `format!("Hello, {name}! {}", x + 1)` |
| `String?`, `null`, `a?.b ?: c` | `Option<String>`, `None`, `map`/`and_then`/`unwrap_or` |
| `var log: [String] = []`, `val ages: [String: i32] = ["ada": 36]` | `Vec<String>`, `HashMap<String, i32>`, `vec![]`, `HashMap::from([(String::from("ada"), 36)])` |
| `fun f(): T throws`, `throw e` | `-> Result<T, AnyError>`, `return Err(e.into())` |
| `try { ... } catch (e) { ... }` | a labeled block with `?` turned into `break` |
| `error enum E { NotFound(p: String) = "no $p" }` | enum + `Display` + `Error` + `From` impls |
| `when (x) { 1, 2 -> a; in 3..=9 -> b; else -> c }` | `match` |
| `xs.filter { it > 2 }.sum_of { it.price }` | `xs.iter().filter(..).cloned().collect()`, `iter().map(..).sum()` |
| `total(nums)` with `fun total(xs: [i32])` | `fn total(xs: &[i32])` and `total(&nums)`: a parameter that is only read is borrowed |
| `fun show<T>(x: T) = "<$x>"` | `fn show<T>(x: T) -> String where T: Display` |
| `shared class Node(var next: Node?)` | `Rc<RefCell<..>>` handle with getters, setters and forwarding methods |
| `interface Shape { fun area(): f64 }` | `trait Shape { fn area(&self) -> f64; }` |
| `fun String.shout() = ...` | an extension trait, imported everywhere automatically |
| `operator fun plus(o: V): V` | `impl Add for V` |

Method receivers are inferred: a method that changes a field takes `&mut self`, one that only
reads takes `&self`, a function that returns `Self` without touching `self` is static.

When the borrow checker objects, the error says what happened in Oxie terms ("`name` was moved
into `register()` on line 6") and offers fixes your editor can apply.

## AI coding agents

Oxie is built to be written by AI coding agents as well as by people. Models have seen little
Oxie, and what they have seen may describe an older version, so Oxie doesn't rely on what they
remember: the installed `oxie` teaches the language and explains mistakes.

```bash
oxie new hello                   # a new project with AGENTS.md and CLAUDE.md for agents
oxie guide                       # the guide to writing the installed version of Oxie
oxie check                       # errors at .oxi lines, in Oxie terms, each with its fix
oxie emit src/main.oxi --annotate   # the Rust a file becomes, and why
```

* **`oxie guide`** prints the guide to writing Oxie, built into the binary, so it always matches
  the compiler: how to work with it, the language at a glance, what works differently from
  Kotlin and from Rust, and the patterns to use. `oxie guide topics` lists the reference pages it
  also carries, and it warns when a project builds with another `oxie-build`.
* **`oxie new` and `oxie init`** write an `AGENTS.md`, read by Codex, Cursor, GitHub Copilot, and
  other agents, and a `CLAUDE.md` that imports it for Claude Code. Neither holds language rules,
  only commands, so updating Oxie updates what agents learn.
* **`oxie check`** turns rustc's errors into Oxie's terms at `.oxi` lines (where a value moved,
  why a `val` can't change) and ends most with a `= fix:` line giving the corrected code; the
  language server offers the same fixes as quick fixes.
* **Claude Code** also has a plugin whose skill does all this for any Oxie work:

  ```bash
  claude plugin install oxie --marketplace oxie-lang/oxie
  ```

The [AI coding agents guide](docs/ai-agents.md) covers setup for each agent, the commands, and
how agents stay current when Oxie changes, and [`evals/`](evals) measures how well models write
Oxie with and without this help.

## Install

Oxie needs Rust 1.82 or newer with Cargo (install them with [rustup](https://rustup.rs)), since
Oxie programs are compiled by rustc. Crates you use can need a newer Rust: the current `tokio`
needs 1.85.

Then install the `oxie` command (compiler driver, formatter and language server), as a
prebuilt binary or from source:

```bash
# macOS and Linux
curl --proto '=https' --tlsv1.2 -LsSf https://github.com/oxie-lang/oxie/releases/latest/download/oxie-installer.sh | sh

# or with Homebrew
brew install oxie-lang/tap/oxie
```

```powershell
# Windows
powershell -ExecutionPolicy Bypass -c "irm https://github.com/oxie-lang/oxie/releases/latest/download/oxie-installer.ps1 | iex"
```

```bash
# with cargo-binstall, which downloads the prebuilt binary
cargo binstall oxie

# or build it from source
cargo install oxie
```

Binaries for macOS, Linux and Windows are also on the [releases page](https://github.com/oxie-lang/oxie/releases). Projects `oxie`
creates get the `oxie-build` helper from crates.io. To work on Oxie itself, see
[working on Oxie](docs/tooling.md#working-on-oxie).

## Quick start

```bash
oxie new hello && cd hello
oxie run
```

Or run a single file as a script, with crates declared in its header:

```oxie
// dep: rand = "0.8"
fun main() = println("dice: ${rand.random<u8>() % 6 + 1}")    // output varies
```

```bash
oxie run dice.oxi
```

| Command | What it does |
|---|---|
| `oxie new <name> [--lib]` | Create a project (`Cargo.toml`, `build.rs`, `src/main.oxi`, `AGENTS.md`, `CLAUDE.md`) |
| `oxie run [file.oxi] [-- args]` | Build and run the project or a script |
| `oxie build` / `oxie check` | Build / type-check |
| `oxie test [name]` | Run `test "name" { }` blocks |
| `oxie fmt [--check]` | Format `.oxi` files |
| `oxie emit file.oxi [--annotate]` | Show the generated Rust (`--annotate` marks each statement's `.oxi` line) |
| `oxie from-rust file.rs` | Translate Rust to Oxie |
| `oxie guide [topic]` | The guide to writing this version of Oxie, for people and AI agents |
| `oxie lsp` | Language server for editors |

Compiler errors — Oxie's own and rustc's — point at your `.oxi` lines:

```
error[E0308]: mismatched types
 --> src/main.oxi:7:21
  |
7 |     val s: String = add(1, 2)
  |                     ^^^^^^^^^ expected `String`, found `i32`
```

So do panics in programs run with `oxie run` and `oxie test`.

## 100% Rust compatible

An Oxie project *is* a Cargo crate. `build.rs` runs the Oxie compiler and `src/main.rs` includes
the generated code, so `cargo build`, `cargo test`, `cargo doc`, rust-analyzer, clippy and
every crate on crates.io work unchanged. You can:

* use any crate: `import serde::{Serialize, Deserialize}` then `@derive(Serialize, Deserialize)`;
* mix `.rs` and `.oxi` files in one crate and call between them in both directions;
* write a library in Oxie and use it from Rust (and vice versa);
* drop down to Rust anywhere with `rust { ... }` blocks or `macro!(...)` calls.

## Performance

Oxie adds no runtime and no hidden allocations or clones; each construct lowers to the Rust
you would write by hand. The [`bench`](bench) crate implements the same workloads in Oxie and
in idiomatic Rust (`cargo run --release -p oxie-bench`):

| workload | Oxie | Rust | ratio |
|---|---|---|---|
| n-body | 5.28 ms | 5.28 ms | 0.999 |
| word frequency (HashMap) | 11.34 ms | 11.51 ms | 0.986 |
| prime sieve | 4.97 ms | 4.99 ms | 0.997 |
| dynamic dispatch | 20.55 ms | 20.69 ms | 0.993 |
| error handling | 16.41 ms | 16.25 ms | 1.010 |
| recursion (fib) | 2.49 ms | 2.50 ms | 0.994 |
| sorting structs | 13.55 ms | 13.49 ms | 1.004 |

Differences are within timing noise. See [docs/performance.md](docs/performance.md).

## Editor support

* **Kuana**: the extension in [`editors/kuana-oxie`](editors/kuana-oxie) gives highlighting,
  outline, ▶ run/test buttons, snippets and the language server.
* **Zed**: the extension in [`editors/zed-oxie`](editors/zed-oxie) (install it with **zed:
  install dev extension**) gives the same, with `rust { }` blocks highlighted as Rust.
* **VS Code**: install [Oxie](https://marketplace.visualstudio.com/items?itemName=oxie-lang.oxie)
  from the Marketplace (VSCodium, Cursor and other editors that use
  [Open VSX](https://open-vsx.org/extension/oxie-lang/oxie): search for Oxie). It gives
  highlighting, the language server, ▶ run/test buttons, the generated Rust beside the source,
  tasks and snippets; the source is in [`editors/vscode-oxie`](editors/vscode-oxie).
* **Any LSP editor**: run `oxie lsp` for diagnostics, completion, hover (with the generated Rust),
  go to definition, references, rename, signature help, formatting, symbols and inlay hints.
* **Grammars**: tree-sitter in [`editors/tree-sitter-oxie`](editors/tree-sitter-oxie), TextMate
  in [`editors/vscode-oxie/syntaxes`](editors/vscode-oxie/syntaxes).

## Documentation

The documentation is published at [oxie.bkymz.net](https://oxie.bkymz.net), where every example
also shows the Rust it compiles to and its output. The same pages are in `docs/`:

* [A tour of Oxie](docs/guide.md) — start here
* [Language reference](docs/reference.md) — every construct and the Rust it becomes
* [Error handling](docs/errors.md)
* [Coming from Kotlin or TypeScript](docs/cheatsheet.md)
* [Rust interop](docs/rust-interop.md) — crates, mixed projects, calling Oxie from Rust
* [Tooling](docs/tooling.md) — CLI, project layout, formatter, language server, editors
* [AI coding agents](docs/ai-agents.md) — Claude Code, Codex, Cursor and other agents, and
  [writing Oxie](crates/oxie-cli/guide/writing-oxie.md), the guide `oxie guide` prints
* [Performance](docs/performance.md)
* [Compiler internals](docs/internals.md) — for contributors
* [Releasing](RELEASING.md) — where Oxie and its extensions are published, and how

## Examples

[`examples/`](examples) has complete programs: a calculator (lexer, recursive-descent parser,
evaluator), a todo CLI with file storage, a bank with interfaces and typed errors, Game of
Life, a word counter, async tasks with tokio, a JSON API with serde, a geometry library used
from Rust, and a Rust program with Oxie modules.

## Repository layout

```
crates/oxie-compiler   lexer, parser, semantic analysis, lowering to Rust, formatter
crates/oxie-build      build.rs helper used by Oxie projects
crates/oxie-cli        the `oxie` command and language server (guide/: what `oxie guide` prints)
crates/oxie-from-rust  Rust → Oxie translator (`oxie from-rust`)
editors/              tree-sitter grammar, Kuana, Zed and VS Code extensions
skills/oxie/          Claude Code skill, which sends agents to `oxie guide`
evals/                measures how well models write Oxie, with and without the skill
examples/             example projects (also run by the test suite)
bench/                Oxie vs Rust benchmarks
tests/                end-to-end and diagnostics tests
docs/                 documentation
```

Run the tests with `cargo test --workspace`.

## License

Oxie is licensed under either of

* Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
* MIT license ([LICENSE-MIT](LICENSE-MIT))

at your option, the same dual license Rust and most of its crates use.

### Code generated by Oxie

The Rust code `oxie` generates from your `.oxi` files, including the small runtime support
module it adds to your crate, is yours: you may use, change and distribute it under any terms
you choose, without keeping a copyright or license notice. The licenses above cover Oxie
itself (the compiler, tools and editor support), not programs written in it.

### Contribution

Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion
in Oxie by you, as defined in the Apache-2.0 license, shall be dual licensed as above, without
any additional terms or conditions.
