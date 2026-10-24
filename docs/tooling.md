# Tooling

## Installing

Oxie needs Rust 1.82 or newer with Cargo ([rustup](https://rustup.rs)): Oxie programs are
compiled by rustc, so a prebuilt `oxie` needs them too. Crates set their own minimum Rust
version (the current `tokio` needs 1.85), and Rust features need the Rust version that
introduced them.

With [Homebrew](https://brew.sh), on macOS or Linux:

```bash
brew install oxie-lang/tap/oxie
```

`brew upgrade oxie` updates it. Each of the others installs `oxie` into `~/.cargo/bin`:

```bash
# macOS and Linux
curl --proto '=https' --tlsv1.2 -LsSf https://github.com/oxie-lang/oxie/releases/latest/download/oxie-installer.sh | sh
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

Homebrew, the installers and `cargo binstall` download a prebuilt binary from the
[releases page](https://github.com/oxie-lang/oxie/releases) (macOS on Apple silicon and Intel, Linux on x86-64 and ARM, Windows on
x86-64), which also has checksums. `cargo install` compiles it, which takes a minute or two.
Running the same command again updates `oxie`.

New projects and scripts depend on the `oxie-build` release from crates.io that matches the
installed `oxie` (`oxie-build = "0.2.0"` for `oxie` 0.2.0). After updating `oxie`, set the
same version in a project's Cargo.toml (`oxie` warns when the two differ); within one release
series, such as 0.2.x, `cargo update -p oxie-build` is enough.

### Working on Oxie

```bash
git clone https://github.com/oxie-lang/oxie.git && cd oxie
cargo install --path crates/oxie-cli
export OXIE_HOME="$PWD"
```

With `OXIE_HOME` set to a checkout, new projects and scripts use its `crates/oxie-build` by
path instead, so they build with your changes. The test suite sets
it to the checkout it runs in.

## The `oxie` command

| Command | Description |
|---|---|
| `oxie new <name>` | New program: `Cargo.toml`, `build.rs`, `src/main.rs`, `src/main.oxi`, and an `AGENTS.md` for AI coding agents |
| `oxie new <name> --lib` | New library (`src/lib.oxi`) |
| `oxie init [--lib]` | Make the current directory an Oxie project; in an existing Rust crate, add Oxie support (`build.rs` + build-dependency, and an `AGENTS.md` if there is none) |
| `oxie run [--release] [-- args]` | Build and run the project |
| `oxie run file.oxi [-- args]` | Run a single file. Inside a project's `src/` this runs the project |
| `oxie build [--release]` | Build |
| `oxie check` | Type-check without producing binaries (fast) |
| `oxie test [file.oxi] [name] [--release]` | Run `test` blocks of the project, or of a script; `name` may be the test's description (`oxie test "adds numbers"`) or part of it |
| `oxie fmt [--check] [paths]` | Format `.oxi` files (the project's `src/` by default) |
| `oxie emit file.oxi [--annotate]` | Print the Rust generated for a file; `--annotate` puts a `// file.oxi:line` comment above each statement and says why at each choice Oxie made (see [What Oxie decided](#what-oxie-decided)) |
| `oxie from-rust file.rs [-o out.oxi]` | Translate Rust to Oxie (`-` reads stdin); see [Translating Rust](#translating-rust) |
| `oxie from-rust dir -o out_dir` | Translate the `.rs` files under `dir` as one crate, into `.oxi` files at the same places under `out_dir` |
| `oxie guide [topic]` | Print the guide to writing this version of Oxie, or a page of this documentation (`oxie guide topics` lists them); see [AI coding agents](#ai-coding-agents) |
| `oxie lsp` | Start the language server on stdin/stdout |
| `oxie clean` | `cargo clean` |
| `oxie version` | Print the version |

Arguments after `--` go to the program (`run`) or to cargo (`build`, `check`, `test`).
`oxie <command> --help` (or `-h`) describes a command and runs nothing.

### Diagnostics

`oxie` first checks the `.oxi` files itself (syntax and Oxie-level errors), then runs cargo and
rewrites rustc's messages so they point at `.oxi` lines, including warnings:

```
error[E0382]: `name` was moved, so it can't be used anymore
 --> src/main.oxi:7:30
  |
1 | fun register(names: &mut Vec<String>, name: String) = names.push(name)
  |                                                                  ---- `register` keeps it: it passes `name` to `.push()`
...
6 |     register(names, name)
  |                     ---- `name` moved into `register()` here
7 |     println("${names.len()} $name")
  |                              ^^^^ used here after the move
  = help: giving a value to a function moves it: `register` keeps it (it passes `name` to
    `.push()`), and `name` can't be used afterwards. Pass a copy with `name.clone()`.
  = fix: pass a copy of `name`: `register(names, name.clone())`
  = rustc: borrow of moved value: `name`
```

All labeled spans of a message are shown on their `.oxi` lines ("moved here", "borrowed
here", "still used here"). The errors newcomers meet most are rewritten in Oxie terms, with
fixes:

| rustc error | Oxie explanation and fix |
|---|---|
| use of moved value (E0382) | where it moved, and where and why the function keeps it; let the function borrow, or pass a copy when the type implements `Clone` (otherwise use the value before the call); for a value a `move` lambda took, give the lambda its own copy in a capture list (`{ [name] -> ... }`) |
| dropped while borrowed (E0597, E0716) | keep the value in a `val`; for a function whose result Oxie tied to all its references, the signature that leaves this argument out |
| borrowed text where a `String` is expected, from `?.` ... `?:` through a reference (E0308) | copy the field: `u.address?.city.clone() ?: "unknown"` |
| `?:` or `?.` on a value that can't be null, such as a `String` (E0277, E0599, E0282) | which value isn't optional, and its type; remove the default, or use `.` |
| a `?:` default of another type than the optional value (E0308, E0277) | the default needs the value's type |
| assign twice / borrow as mutable (E0384, E0596) | it's a `val`; declare it with `var` |
| behind a `&` reference (E0596) | the parameter needs `&mut` (callers updated) |
| borrow conflicts (E0499, E0502, E0505, E0506) | where it's borrowed and still used; copy instead of borrowing |
| a loop over a field while the body changes `this` (E0502) | loop over a copy of the field |
| a map lookup returned before an `insert` into the same map (E0502) | look the key up once: `cache.entry(id).or_insert_with { load(id) }`; when the insert converts the key (`key.to_string()`), the entry API would convert it on every call, so that is explained as a trade-off, without a fix |
| changing `v[i]` in a loop over `v.iter().enumerate()` (E0502) | loop over `v.iter_mut()` and change each element through it: `for (x in v.iter_mut()) { *x = *x * 2 }` |
| not thread-safe (E0277 `Send`/`Sync`, a future held across `await`) | declare the `shared class` `@sync` |
| lambda may outlive (E0373) | make it a `move` lambda |
| `?` without `Result` (E0277) | add `throws` to the function |
| `?` on an optional value in a throwing function, or on a `Result` in a function returning `T?` (E0277) | which it is; `.ok_or("missing value")?` or `.ok()?` |
| no `Display` (E0277) | print with `:?`, or add `toString` |
| missing trait bound (E0277) | `require T: Bound` |
| no method `size`, `toUpperCase`, ... (E0599) | the Rust name (`len`, `to_uppercase`) |
| no method `is_not_empty` on text, where rustc suggests `is_empty` (E0599) | `!value.is_empty()`: a name that means the opposite of the one called is never the fix |
| a list operation on text (E0599) | says it is one of Oxie's list operations, instead of suggesting to implement the runtime's `ListOps` |
| a list operation on an iterator (E0599) | call it on the list (`xs.map_indexed { }` for `xs.iter().map_indexed { }`), or collect the iterator into a list first (`s.chars().collect::<Vec<_>>().join_to_string("")`) when its items aren't references into a list; never rustc's similar name (`is_sorted` for `sorted`) |
| trait / type not imported (E0599, E0433, E0412) | `import` the suggested path |
| unused `var` | declare it with `val` |
| an ignored error, from a call that can fail used as a statement in a function that doesn't throw | add `throws` to the function, or `.unwrap()` |

rustc's own suggestions are kept as fixes when they map exactly onto the `.oxi` source, in
Oxie terms. One that edits several places is a fix only as a whole, titled with rustc's help:
"store this in the heap by calling `Box.new`" puts a value in a box
(`vec[Box.new(Circle(1.5))]`), and a field that holds a function is called as `(s.run)(3)`.
When its parts don't all map onto one line, the help is shown without code, since a part of it
alone would mislead. The original message is shown last (`rustc: ...`) so it can still be
searched for. In editors the fixes are code actions (quick fixes).

The declaration an explanation depends on may be in another file of the project, such as the
function a value moved into. A label there is shown under that file's name, and a fix there
says which file it changes:

```
 ::: src/text.oxi:3:35
  |
3 | fun keep(v: Vec<i32>): Vec<i32> = v
  |                                   - `keep` keeps it: it returns `v`
  ...
  = fix: let `describe` borrow it (parameter type `&Tag`) (in src/text.oxi): `fun describe(t: &Tag): String = "#${t.name}"`
```

In editors the label is a related location, and the fix edits the other file.

Each generated file has a source map next to it (`main.rs.map`) used for the translation.

Some errors can only come from a bug in Oxie: an error inside the support module `__oxie.rs`,
or a Rust syntax error in code generated from a file that has no `rust { }` block or macro call
(those are copied as they are). They are reported as such, at the nearest `.oxi` line, with
rustc's message and the generated line, so they aren't mistaken for mistakes in your code:

```
error: Oxie generated Rust that doesn't compile (a bug in Oxie)
 --> src/main.oxi:4:5
  = rustc: expected `;`, found `2` (/.../out/oxie/main.rs:9:15)
  = generated: `let x = 1 2;`
  = help: please report it at https://github.com/oxie-lang/oxie/issues with the .oxi code;
    `oxie emit <file>` prints the generated Rust
```

The maps are used at run time too: when a program run with `oxie run` or `oxie test` panics,
the message, the backtrace frames (`RUST_BACKTRACE=1`) and failed assertions in tests point at
`.oxi` lines instead of the generated Rust (and the frame of `main() throws` says `main`):

```
thread 'main' panicked at src/main.oxi:12:5:
index out of bounds: the len is 3 but the index is 7
```

To do this, `oxie` reads the program's error output (and, for `oxie test`, its normal output)
as it is written and passes it on. A program run with `cargo run` writes straight to the
terminal and reports the generated file.

### What Oxie decided

Some of the generated Rust isn't spelled out in the source: `&mut self` for a method that
changes its object, `&str` or `&[T]` for a `String` or `Vec` parameter that a function only
reads (and `&T` for a class or `Box<T>` one), a lifetime for a function that returns a
reference, the `&` or `&mut` of an argument
or a loop, an argument computed before its call because it changes `this`
(`let next_id = self.next_id(); /* oxie: computed first because ... */`), and the fields a
method takes instead of `this` (`count: &mut i32 /* oxie: &mut i32 because `note` changes
`count`, and `scan` calls it while it loops over `items` */`). `oxie emit --annotate` shows each
of these choices where it lands, with the reason:

```rust
// borrowing.oxi:4
pub(crate) fn shout(s: &str /* oxie: &str because it only reads `s` */) -> String {
...
    // borrowing.oxi:10
    pub(crate) fn hit(&mut self /* oxie: &mut self because it passes `hits` to `add_one`, which takes `&mut` */) {
        add_one(&mut self.hits /* oxie: &mut because the parameter is `&mut Vec<i32>` */);
```

In editors, hovering a function or method lists the choices about its signature. An error that
one of them causes says so: calling a method Oxie made `&mut self` on a `val`, or while the
value is borrowed, points at the line in the method that changes its object.

### Scripts

A `.oxi` file outside a project runs as a script. Its dependencies go in header comments:

```oxie
// dep: rand = "0.8"
// dep: serde = { version = "1", features = ["derive"] }
fun main() = println(rand.random::<u32>())    // output varies
```

Scripts are built in `~/.cache/oxie` (set `OXIE_CACHE` to change it) with a shared target
directory, so only the first run of a script compiles its dependencies.

### Translating Rust

`oxie from-rust` turns Rust (a file from your crate, an example from a crate's docs, an answer
from Stack Overflow) into idiomatic Oxie, printed to stdout or written with `-o`:

```bash
oxie from-rust src/point.rs -o src/point.oxi
pbpaste | oxie from-rust -
```

```rust
#[derive(Debug, Clone, Copy, PartialEq)]
struct Point { x: f64, y: f64 }

impl Point {
    fn new(x: f64, y: f64) -> Self { Point { x, y } }
    fn scale(&mut self, k: f64) { self.x *= k; self.y *= k; }
}

impl fmt::Display for Point {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result { write!(f, "({}, {})", self.x, self.y) }
}

fn parse_point(s: &str) -> Result<Point, Box<dyn std::error::Error>> {
    let (a, b) = s.split_once(',').ok_or("missing comma")?;
    Ok(Point::new(a.trim().parse()?, b.trim().parse()?))
}
```

becomes

```oxie
data class Point(x: f64, y: f64) {
    fun scale(k: f64) {
        x *= k
        y *= k
    }

    override fun toString() = "($x, $y)"
}

fun parse_point(s: &str): Point throws {
    val (a, b) = s.split_once(',').ok_or("missing comma")?
    Point(a.trim().parse()?, b.trim().parse()?)
}
```

* Structs and their `impl` blocks become classes (a `data class` when the derives are exactly
  the ones Oxie adds and `new` takes every field); traits become interfaces; enums get their
  methods in the enum body. `Display` impls that only `write!` become `toString()`, `impl Add`
  and friends become `operator fun`s, and `#[cfg(test)]` modules become `test` blocks.
* `Result` returns become `throws` (`Ok(v)` → `v`, `return Err(e)` → `throw e`); `Option<T>` is
  `T?` and `None` is `null`; `match` is `when`; `if let Some(x) = e` is `if (val x = e)`.
* `format!`, `println!` and friends use string templates; closures become lambdas (`{ it * 2 }`
  for simple one-parameter bodies) and move outside the parentheses when last; `vec![..]`,
  `HashMap::from([(k, v)])` and `HashSet::from([..])` become `vec[..]`, `mapOf(k to v)` and `setOf(..)`.
* Receivers, `self.`, `Type::new` and literal `.to_string()`s are dropped where Oxie restores
  them, and a call to a function that a method of the same name hides in Oxie becomes
  `self::name(..)`. The translation is compiled with the Oxie compiler before it is printed:
  receivers and derives Oxie would infer differently are written out, and items Oxie can't
  express are kept as Rust in `rust { }` blocks with a `// TODO(from-rust)` comment (also
  reported on stderr). Such a method of an `impl` is kept on its own, in an `impl` block after
  the class, and the class's other methods are translated.
* Attributes stay on their statements and `let`s: `#[cfg(test)] v.push(1);` becomes
  `@cfg(test) v.push(1)`, and a doc comment on a statement, which documents nothing, becomes an
  ordinary comment. Oxie has no attributes on expressions, so an expression with one (`#[cfg]`
  on a block's value or on a `vec!` element) is kept as Rust with a TODO.
* Comments are kept. A `Box<dyn Error>` becomes `AnyError` (`Box<dyn Error + Send + Sync>`), so a
  failing `main` prints `error: ...` instead of Rust's `Error: ...` debug output.

Values that most methods of a class pass down to the next call, like GPUI's `window` and `cx`,
become [context parameters](reference.md#context-parameters):

```rust
pub struct Editor {
    text: String,
    dirty: bool,
}

impl Editor {
    pub fn insert(&mut self, s: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.text.push_str(s);
        self.layout(window, cx);
    }

    pub fn clear(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.text.clear();
        self.layout(window, cx);
    }

    fn layout(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.dirty = true;
        window.request_layout();
        self.changed(cx);
    }

    fn changed(&self, cx: &mut Context<Self>) {
        cx.notify();
    }
}
```

becomes

```oxie
context(window: &mut Window, cx: &mut Context<Self>)
pub class Editor(text: String, dirty: bool) {
    pub fun insert(s: &str) {
        text.push_str(s)
        layout()
    }

    pub fun clear() {
        text.clear()
        layout()
    }

    fun layout() {
        dirty = true
        window.request_layout()
        changed()
    }

    fun changed() {
        cx.notify()
    }
}
```

* A `&mut` parameter becomes a context of its class when at least three methods, and a
  quarter of them, end their parameters with it (with the same name and type), next to the
  class's other contexts. Methods drop those last parameters, and calls that Oxie follows
  (`self.layout(window, cx)`, `Self::open(path, cx)`) drop the arguments that name them.
* Each method keeps the parameters it had in Rust: where Oxie would infer other contexts for it
  than the ones it declared (it passes one on through a call Oxie doesn't follow), it gets a
  `context(...)` of its own. A method with an unused one (`_cx`) keeps its parameters, unless
  it is used as a value (`cx.listener(Self::save)`) and takes them all, as Oxie then gives it.
* Calls on values whose class Oxie doesn't know, like a GPUI listener's `this`, pass their
  contexts as written, and so do calls whose arguments are other values than the contexts'
  variables.
* A class gets no contexts if a method used as a value takes none of them, since Oxie would give
  that method all of them.

Translating a directory (`oxie from-rust src -o oxie-src`) translates its files as one crate. A
class's methods may be spread over the `impl` blocks of many files, as GPUI apps do with their
workspace; translated together, the class gets its `context(...)` where it is declared, and its
methods in the other files drop the same parameters. Each file is still compiled and checked on
its own, next to declarations of the other files' methods that take contexts. A single file is a
crate of its own: an `impl` block of a class declared in another file keeps its parameters.

## Project layout

```
Cargo.toml          [build-dependencies] oxie-build = "0.2.0"
build.rs            fn main() { oxie_build::compile(); }
src/main.rs         include!(concat!(env!("OUT_DIR"), "/oxie/main.rs"));
src/main.oxi        crate root (src/lib.oxi for libraries)
src/*.oxi           modules; directories are nested modules
AGENTS.md           tells AI coding agents to read `oxie guide`
```

`oxie_build::compile()` compiles `src/`; `oxie_build::compile_dir("other")` compiles another
directory. In a Rust crate, `include!(concat!(env!("OUT_DIR"), "/oxie/mod.rs"))` declares the Oxie
modules without an Oxie crate root.

`oxie` checks the sources with the compiler built into it before cargo builds them with the
project's `oxie-build`. When the two versions differ (after updating `oxie`, or a project made
with another one), `oxie build`, `run`, `check` and `test` warn and say how to make them match.

## AI coding agents

AI coding agents (Claude Code, Codex, Cursor, ...) know little about Oxie, and what they know
may be from an older version. Oxie gives them the rules of the version you use:

* `oxie guide` prints a guide to writing Oxie: how to work with the compiler, the language at a
  glance, how it differs from Kotlin and from Rust, and the mistakes to avoid. `oxie guide
  <topic>` prints a page of this documentation (`oxie guide topics` lists them). Both are built
  into the `oxie` binary, so they describe exactly the Oxie it compiles, and they warn when the
  project builds with another `oxie-build`.
* `oxie new` and `oxie init` write an `AGENTS.md` that tells agents to run `oxie guide` before
  they change Oxie code. It states no language rules, so it stays right when you update Oxie.
* The [Claude Code skill](../skills/oxie/SKILL.md) does the same for any work on Oxie code,
  also in projects without an `AGENTS.md`. Install it with
  `claude plugin install oxie --marketplace oxie-lang/oxie`, or copy `skills/oxie` to
  `~/.claude/skills/oxie`.

## Formatter

`oxie fmt` re-indents (4 spaces, one level per open bracket and for continuation lines such as
`.method()` chains), normalizes spaces around commas, colons and operators like `=`, `==`,
`->`, removes trailing whitespace and repeated blank lines. It never changes strings or
comments and leaves files with syntax errors untouched. `oxie fmt --check` exits with status 1
if a file would change (useful in CI).

## Language server

`oxie lsp` implements the Language Server Protocol over stdio:

| Feature | Details |
|---|---|
| Diagnostics | Oxie errors on every keystroke; rustc errors and warnings (mapped to `.oxi`, explained in Oxie terms, with related locations) after `cargo check` on open and save |
| Quick fixes | code actions for the fixes listed under [Diagnostics](#diagnostics) (`var` instead of `val`, `.clone()`, `&mut` parameters, imports, `move` lambdas, `throws`, ...) |
| Completion | locals, members after `.` (using declared types), project declarations, keywords, snippets, std types, builtins; module names in `import` lines |
| Hover | Oxie signature, doc comment, the generated Rust signature, and for a function or method what Oxie decided about it (an inferred `&mut self`, a lent parameter, a lifetime) and why |
| Go to definition / declaration | functions, classes, enums, variants, fields, methods, locals and parameters, across files |
| References, document highlight | whole-word occurrences in the project (locals: within their function) |
| Rename | project-wide for declarations; function-local for locals |
| Signature help | parameter list while typing a call |
| Formatting | same as `oxie fmt` |
| Document and workspace symbols | classes, enums, interfaces, functions, fields, variants, tests |
| Inlay hints | how each method receives its object (`&self`, `&mut self`, static), which Oxie infers |
| `oxie.showRust` command | returns the generated Rust for a file |

Initialization option `{"cargoCheck": false}` (or environment variable `OXIE_LSP_NO_CHECK=1`)
turns off `cargo check` on save.

### Kuana

The extension in [`editors/kuana-oxie`](../editors/kuana-oxie) adds:

* syntax highlighting, outline / breadcrumbs / sticky scroll, bracket matching, indentation,
  text objects (tree-sitter grammar from `editors/tree-sitter-oxie`);
* the language server (needs `oxie` on your `PATH`);
* ▶ buttons on `fun main` (run, run --release, show generated Rust) and on each `test` block;
* snippets (`main`, `fun`, `class`, `data`, `when`, `test`, `errenum`, ...).

Install: Settings → Extensions → **Install from Folder…** → choose `editors/kuana-oxie`. After
editing the grammar, run `editors/build-kuana-extension.sh` and **Reload Extensions**.

Kuana's own source (branch `oxie-language` of the editor) additionally shows a `.oxi` file icon
and adds `oxie run`, `oxie test`, `oxie build` and `oxie fmt` to the Run… list of Oxie projects.

### VS Code

The extension in [`editors/vscode-oxie`](../editors/vscode-oxie) adds:

* syntax highlighting (a TextMate grammar), with `rust { }` blocks highlighted as Rust and
  ` ```oxie ` blocks in Markdown files;
* the language server; if `oxie` isn't on the `PATH` or in `~/.cargo/bin`, it offers to run
  `cargo install oxie`;
* ▶ Run and Run --release above `fun main`, ▶ Run Test above each `test` block, and Run File,
  Run File (Release) and Run Tests in File in the editor's run menu;
* **Show Generated Rust**: the Rust for the current file beside it, updated as you type;
* `oxie build`, `run`, `test`, `check` and `fmt` tasks for Oxie projects (Terminal → Run Task);
* snippets, the same as Kuana's.

Settings: `oxie.path` (the `oxie` command, for example a checkout's `target/debug/oxie`),
`oxie.cargoCheck` (`cargo check` on open and save) and `oxie.trace.server`. In Restricted Mode
only highlighting and snippets work, because building runs the project's build scripts.

Install: build the package with `npm install` and `npm run package` in `editors/vscode-oxie`,
then run `code --install-extension oxie-0.1.0.vsix` (or use **Extensions: Install from VSIX…**).

### Other editors

Any editor with LSP support can use `oxie lsp` for `*.oxi` files. For syntax highlighting, use
the tree-sitter grammar in `editors/tree-sitter-oxie` (Neovim, Helix, Zed), the TextMate grammar
`editors/vscode-oxie/syntaxes/oxie.tmLanguage.json` (JetBrains IDEs through TextMate Bundles,
Shiki), or Kotlin highlighting as a fallback.
