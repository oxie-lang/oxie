# Changelog

## Unreleased

* Context parameters: `context(log: &mut Log) fun record(line: &str)` takes `log` without its
  callers writing it. Each call passes the variable named `log` where it is made, or one given by
  name (`record("x", log = &mut other)`). Before a class,
  `context(window: &mut Window, cx: &mut Context<Self>)` gives each method the contexts it uses,
  also through the methods it calls, so GPUI code declares `window` and `cx` once. In the
  generated Rust they are ordinary parameters after the others.
* `oxie from-rust` turns the values that most methods of a class pass down to the next call, like
  GPUI's `window: &mut Window, cx: &mut Context<Self>`, into the class's context parameters: its
  methods drop them, and so do the calls that pass them along. Each method keeps the parameters
  it had in Rust: where Oxie would infer other contexts, it gets a `context(...)` of its own.
  `oxie from-rust <dir> -o <out dir>` translates a directory as one crate,
  so that methods in the `impl` blocks of other files drop them too. On the 752k tokens of
  Kuana's `src/`, the translation drops 12.6k more: 9.4% fewer tokens than the Rust instead of
  7.7%.
* A null check unwraps a local variable or parameter of type `T?`: in `if (x != null) { ... }`
  (also in an `&&` chain) `x` is the value, and after `if (x == null) return` (or `throw`,
  `continue`, `break`) it is the value for the rest of the block. Fields aren't unwrapped; their
  error says how to unwrap them.
* A `shared class` method can pass `this` on (`child.parent = this`, `bus.subscribe(this)`): it
  is a handle to the object. Before, it was a Rust type error about the class's data.
* A method of a shared object that reaches the same object again while it runs (a parameter
  that is the object itself, a callback that reads it) panics at that line with an explanation
  instead of "RefCell already mutably borrowed". An `@sync` object used that way panics too;
  before, the program waited for its own lock forever.
* A list, array or slice can be indexed with any integer type (`xs[i]` with an `i32` `i` is
  `xs[i as usize]`).
* A loop variable has the type of the list's elements, and a method called on a value of a known
  class gives its declared type, so more of what depends on types works in loops and on such
  values (arguments lent to `&` parameters, shared-object fields after `?.`, null checks).
* Rust's floats with a trailing dot (`8.`, as in GPUI's `px(8.)`) and a range after `?`
  (`a?..b?`) read as they do in Rust. Before, `8.` was an error, and inside a `when` arm it made
  the compiler loop forever.
* A broken `when` or `match` arm is reported once and parsing goes on after it; some (`f(x.)`)
  made the compiler loop forever.
* A let chain whose first condition ends in a lambda parses:
  `if (val a = f().filter { it > 0 } && val b = g())`. The lambda's `{` was taken for the end of
  the condition.
* `x == null` and `x != null` are `x.is_none()` and `x.is_some()`, so they work on classes
  without `==`.
* A branch next to `null` is optional without a declared type: `val v = if (c) f() else null`
  is `Some(f())`, or `f().into()` for a function from Rust whose type Oxie doesn't see. When such
  a value is optional already, the error says so and offers the type to declare.
* `?.field ?: b` gives the field's value when `b` is a constant, an enum variant, a variable or
  a field (`server?.encoding ?: Encoding.Utf16`), as it did when `b` was a literal.
* A reference inside an `impl Trait` parameter (`impl Iterator<Item = &Patch>`) gets a lifetime,
  the result's when the function returns a reference. Before, rustc rejected the signature.
* Inside a class with a method `name`, `self::name(x)` calls the module's function of that name
  (a bare `name(x)` calls the method), as the path does in Rust.
* A method that calls a `&mut self` method of an Oxie class through a field (`c.add(1)`,
  `this.c.apply { ... }` with `var c: Counter`, or a generic class like `Entity<Counter>`) takes
  `&mut self` (a call on a `shared class` field still doesn't). Before, it took `&self` and rustc
  rejected the call unless it was a `mut fun`.
* `oxie from-rust` keeps a method Oxie rejects as Rust on its own, in an `impl` block after its
  class, instead of keeping the whole `impl` as Rust; and a `None` beside a value whose type
  Oxie doesn't see stays `None`. On a 70,000-line GPUI application every file now translates
  (17 of 137 made it loop forever) and 7% of the output stays Rust, instead of 43%.
* `oxie from-rust` writes a call to a free function as `self::name(..)` in the methods of a type
  that has a method of the same name. Before, the translation called the method: a wrong
  result, or an error that kept the code as Rust.

## 0.2.0 - 2026-10-24

Oxie 0.2.0 makes the compiler explain itself and removes more of the bookkeeping Rust asks for.
Errors point at `.oxi` lines, panics included, say why Oxie decided what the source doesn't
spell out, and come with fixes that compile. More Kotlin-style code compiles as written: loops
that change the elements of a list, `m[k] = v` on maps, `when` on a `String`, `+` on text,
functions that return an interface, and `?.` on shared objects. `oxie guide` teaches AI coding
agents the installed version of the language, and the documentation has a site at
[oxie.bkymz.net](https://oxie.bkymz.net).

### Upgrading

* Set `oxie-build = "0.2.0"` in the `[build-dependencies]` of each project (`cargo update`
  doesn't move from 0.1 to 0.2). `oxie` warns when a project builds with another `oxie-build`.
* Some code behaves differently: `"cost $5"` is text (it printed `cost 5`); a throwing call
  inside `try { }` jumps to `catch` instead of leaving the function; a function written
  `= expr` without a return type no longer drops a value it can't type; a `{ ... }` statement
  is an error that suggests `run { ... }`; a `String` or `Vec` parameter that a function only
  reads is borrowed, so callers keep their value.

### Changes

* `oxie guide` prints a guide to writing Oxie, for AI coding agents and people: how to work with
  the compiler, the language at a glance, how it differs from Kotlin and from Rust, and the usual
  mistakes. `oxie guide <topic>` prints a page of the documentation (`oxie guide topics` lists
  them). Both are built into the binary, so they describe the installed version.
* `oxie new` and `oxie init` write an `AGENTS.md` that tells agents to read `oxie guide`. It
  states no language rules, so it stays right when Oxie changes.
* `oxie build`, `run`, `check`, `test` and `guide` warn when the project builds with another
  `oxie-build` version than the installed `oxie`, and say how to make the two match.
* A Claude Code skill in `skills/oxie`, installable as a plugin from this repository
  (`claude plugin install oxie --marketplace oxie-lang/oxie`), and `evals/`, which measures how
  well models write Oxie with and without it.
* A documentation site at [oxie.bkymz.net](https://oxie.bkymz.net), generated from `docs/`: every
  example shows the Rust that `oxie emit` produces for it and its output, kept current by the tests.
* Panics point at the `.oxi` source: `oxie run` and `oxie test` rewrite the location of a
  panic, backtrace frames and failed assertions in tests from the generated Rust to the `.oxi`
  line they came from.
* Errors only a bug in Oxie can cause (inside the support module, or a Rust syntax error in
  generated code) are reported as "Oxie generated Rust that doesn't compile", with rustc's
  message and the generated line, instead of as an error in your code.
* A `{ ... }` statement, which is a lambda that never runs, is an error that suggests
  `run { ... }`; before, it compiled with only rustc's "unused closure" warning, and the
  statements in it were skipped.
* `oxie emit --annotate` puts a `// file.oxi:line` comment above each statement of the
  generated Rust. Golden tests in `tests/emit` keep the generated Rust from changing unnoticed.
* Errors caused by what Oxie decided say why: a value moved into a function that keeps its
  `String` or `Vec` parameter points at the line where the function keeps it ("it returns `v`"),
  and a borrow that ends too early because Oxie tied a function's result to all its reference
  parameters shows the signature with a narrower lifetime.
* A VS Code extension in `editors/vscode-oxie`: highlighting, the language server, ▶ run and
  test buttons, the generated Rust beside the source, tasks and snippets.
* Language server: files opened through a symlinked directory (on macOS, anything under `/tmp`)
  get their diagnostics, and definitions point at the open file instead of a second copy. A
  script opened while another script's `cargo check` runs is checked next; before, its rustc
  errors appeared only after saving it.
* Clearer errors: a borrow conflict on `self` says `this`, and a loop over a field whose body
  changes `this` offers to loop over a copy; a `shared class` handle sent to another thread (or
  held across an `await` in a spawned task) offers to declare the class `@sync`.
* `fun city(u: &User): String = u.address?.city ?: "unknown"` used to fail with a bare
  "expected `String`, found `&str`". The chain reads `city` through a reference, so the result is
  borrowed text; Oxie still doesn't copy the field silently, but the error now explains this and
  offers the fix `u.address?.city.clone() ?: "unknown"`.
* `?:` and `?.` on a value that can't be null (`name ?: "nobody"` with a `String`) used to fail
  with rustc's errors about Oxie's runtime helpers (`ElvisOr`, `as_ref`), often twice. The error
  now names the value and its type and offers to remove the default or to use `.`; a `?:`
  default of another type than the optional value says so. Backtrace frames of `main() throws`
  say `main` instead of the generated `__oxie_main`.
* Explanations look at every file of the project: when the function a value moved into, a type
  or a `shared class` is declared in another file, the error points there
  (`::: src/text.oxi:3:35`) and its fix edits that file, in the terminal and as a quick fix in
  editors. Before, they only found declarations in the error's own file.
* An error about a name read in a template (`println("low stock: $low")` with a `Vec`) points
  at `$low` instead of at `println`.
* Generated Rust: `?:` with a default that isn't a literal and `?` inside `try` put each `match`
  arm on its own line instead of one long line; names Oxie introduces (`__oxie_v1`) are
  numbered per function, so changing a function doesn't renumber the rest of the file; the
  header of a script's generated file names the script instead of `main.oxi`.
* Oxie says why it decided what the source doesn't spell out. `oxie emit --annotate` marks
  each choice with the reason (`fn hit(&mut self /* oxie: &mut self because it calls
  `hits.push()` */)`, `s: &str /* oxie: &str because it only reads `s` */`, the `&` of an
  argument, a loop's borrow, a lifetime). Hovering a function or method in an editor lists the
  choices about its signature. Calling a method Oxie made `&mut self` on a `val`, or while the
  value is borrowed, points at the line in the method that changes its object.
* The quick fix "make it a `move` lambda" for a trailing lambda (`thread::spawn { .. }`) writes
  `thread::spawn(move { .. })`; it used to write `thread::spawn move { .. }`, which doesn't
  parse. A test now applies every quick fix offered for `tests/diagnostics` and builds again.
* Borrow errors suggest what costs nothing before a copy, and no copy that doesn't compile. A
  moved value gets "pass a copy" (`.clone()`) only when its type implements `Clone`: a
  `Vec<MutexGuard<i32>>`, or a class without `Clone`, used to get a `.clone()` that rustc
  rejects, and is now told to use the value before the call, or to let the function borrow it.
  `if (val v = cache.get(&id)) return v` before `cache.insert(id, ...)` suggests the entry API,
  `cache.entry(id).or_insert_with { ... }`, with a quick fix when the insert takes the key as it
  was looked up. When it converts the key (`key.to_string()`), the explanation says that the
  entry API makes that key on every call, even when it is already in the map, and offers no fix.
  Changing `v[i]` in a loop over `v.iter().enumerate()` suggests
  `for (x in v.iter_mut()) { *x = ... }`, with a quick fix in the simple cases. A quick fix is
  no longer one part of a rustc suggestion that edits several places (`value` without the
  `let value = name.clone()` it needs), and the quick fix test checks the lines a fix changes
  too.
* rustc's suggestions that edit several places are shown whole, in Oxie terms: a constructed
  value where a box of an interface is expected gets the fix `vec[Box.new(Circle(1.5))]`, and a
  field holding a function called like a method gets `(s.run)(3)`. Their help used to show only
  the first part (`Box::new(`, `(`). A method that rustc suggests for a similar name but that
  means the opposite is no longer a fix: `value.is_not_empty()` or `value.isNotEmpty()` on text
  got `value.is_empty()`, which turns the condition around, and now gets `!value.is_empty()`. A
  list operation called on text or an iterator says so, instead of suggesting to implement
  Oxie's runtime trait `ListOps`. On an iterator it says to call the operation on the list or to
  collect the iterator first, with the fix `xs.map_indexed { }` for `xs.iter().map_indexed { }`
  and `s.chars().collect::<Vec<_>>().join_to_string("")` for `s.chars().join_to_string("")`,
  instead of rustc's similar name, which gave `"b a".split(' ').sorted()` the fix `is_sorted()`.
* A value used after a `move` lambda took it is explained as taken by the lambda. The error
  used to say it moved into the call around its use inside the lambda (`println`), and to
  suggest letting that function borrow it. It now points at the lambda and suggests giving the
  lambda its own copy, with a quick fix that writes the capture list (`{ [name] -> ... }`, or
  `name` added to the lambda's list) when the type implements `Clone`.
* A method that calls an interface method taking `&mut self` takes `&mut self` too. It used to
  stay `&self`, which rustc rejected, when its own class's implementation changed nothing (the
  method being `&mut self` because another implementation changes its object), when it was a
  default method of the interface, or when it called a default method its class inherits.
* Receiver inference no longer stops after 8 rounds: in a chain of 9 or more methods calling
  each other on `this`, declared before the methods they call, the first ones stayed `&self`.
* Classes and enums with the same name in different modules work. Before, Oxie lost track of
  both: their methods were all `&self` (a `Counter` whose `bump` assigns a field didn't
  compile), one's derives were used for the other (a `Copy` derive on a type holding a
  `String`), and their variants, constructors and impl blocks weren't recognized.
* A class constructed through its module (`shapes.Circle(2.0)`, `a.Counter(1)`) calls its
  constructor; it used to be emitted as `shapes::Circle(2.0)`, which rustc rejects.
* A cast compares with `<` without parentheses, as it already shifted with `<<`:
  `n as usize < limit` used to be a parse error ("expected `>`"), as in Rust, which reads the
  `<` as the start of generic arguments. The generated Rust has `(n as usize) < limit`. Written
  with parentheses, the comparison used to lose them, and rustc rejected the result, also when
  the cast ended a longer operand (`i + (n as usize) < limit`).
* A function called through its module (`util.total(v)` or `util::total(v)`) is treated like a
  direct call: arguments are lent to `&` and `&mut` parameters, defaults and named arguments
  work, and a `throws` function's error propagates. Before, arguments were passed as written
  (`util.total(v)` with a `Vec` was a type error), named arguments were rejected, and a failing
  `util.load(x)` statement dropped its error.
* Functions with the same name in different modules keep their own parameters; a call means
  the module's own function or the one it imports. Before, calls to either lost the parameters
  (no lending, defaults or `throws`).
* `oxie from-rust` keeps attributes on statements and `let`s: `#[cfg(test)] v.push(1);` becomes
  `@cfg(test) v.push(1)`. It used to drop them, so a statement meant only for tests ran in every
  build. A doc comment on a statement becomes an ordinary comment (it used to comment out the
  statement), and an expression with an attribute, which Oxie has no syntax for (`#[cfg]` on a
  block's value or on a `vec!` element), is kept as Rust with a TODO.
* Borrowed parameters: a `String` or `Vec<T>` parameter of a top-level non-`pub` function that
  the body only reads becomes `&str` or `&[T]`, so `greet(name)` no longer uses up `name`. A
  `Vec<T>` is lent only when dropping a `T` just frees memory, so a function given mutex guards
  or values with a `Drop` implementation still drops them when it returns. A parameter of an
  Oxie class or `Box<T>` becomes `&T` the same way when the body only reads it (its fields, its
  methods that take `&self`, formatting), so `describe(ada)` no longer uses up `ada`.
  Arguments for `&str` and `&[T]` parameters are lent whatever they are
  (`greet(name.to_uppercase())` used to be a type error). When a function keeps the value, the
  move error no longer suggests borrowing it.
* A call on a field whose argument calls a method of `this` that takes `&mut self`
  (`items.push(next_id())`) compiles: Oxie computes the argument first, into a local named
  after the method (`let next_id = self.next_id(); self.items.push(next_id);`). Rust borrows
  the field for the call before it runs the arguments, so it rejected the statement. Only
  statements that rustc rejects change, and `oxie emit --annotate` says why for each local.
* A loop over a field can call a method that changes other fields: `for (item in items) {
  note(item) }`, with `note` changing `count`, used to be rejected by rustc, since the call
  borrowed all of `this` while the loop borrowed `items`. Oxie now gives such a method the
  fields it uses instead of `this` (`fn note(item: &str, count: &mut i32)`, called as
  `Self::note(&item, &mut self.count)`), when it uses at most three fields, doesn't change the
  one the loop goes over, and is called only from its own class. `oxie emit --annotate` says why
  for each field.
* Capture lists: `{ [counter, tx] -> ... }` gives a lambda its own clones of `counter` and `tx`
  (and `[n = expr]` a computed value) and makes it `move`, replacing Rust's
  `let tx = tx.clone(); move || ...` before every thread or callback.
* `for (x in xs)` no longer uses up `xs` when the list is used after the loop (or by the next
  round of an outer loop): the loop borrows it (`for x in &xs`, or `for &n in &nums` for `Copy`
  elements). Loops over a class's collection fields borrow them too. Before, both were errors.
* A method reading through a new iterator (`input.chars().next()`) is no longer inferred to take
  `&mut self`.
* Classes, structs and enums whose fields hold references get their lifetime parameter
  automatically: `class Parser(input: &str)` used to produce Rust that didn't compile and now
  becomes `struct Parser<'a> { input: &'a str }`. Functions taking or returning such types get
  their lifetimes filled in as for references, and `data class`es holding `&str` derive `Copy`,
  `Eq` and `Hash` when their other fields allow.
* An enum variant without fields is wrapped in `Some` where a `T?` is expected, as one with
  fields already was: `fun f(): Status? = Status.Done`, `return Status.Done`,
  `val s: Status? = Status.Done`, `if (go) Status.Done else null` and a `when` arm
  `-> Status.Done` used to fail with "expected `Status?`, found `Status`". So is a class without
  fields, which is its own value (`if (found) Marker else null` for a `Marker?`). Where an
  interface is expected, both are boxed like any constructed value.
* `return` in a `toString` with a block body gives the text. It used to return from the
  generated `fmt`, which rustc rejected (expected `Result<(), std::fmt::Error>`, found
  `String`). A `return` at the end is just the value; one before it breaks out of the block
  that computes the text.
* `toString` and the fields of its class: a field named `f` (`"$n/$f"`) is the field; it used
  to be the `Formatter` that `Display` gets, which can't be formatted. The same goes for a field
  named `f` in the message of an error enum's variant (`Missing(f: String) = "missing $f"`). A
  field that is the whole text (`override fun toString() = name`) is written as it is; it used
  to be moved out of `this`, which rustc rejected.
* A field that a `toString` gives without being all of it (`return name` after other
  statements, `if (short) nick else name`) needs `.clone()`, as in any method that returns a
  `String`, and the error now offers only that. It used to offer the fix `return &name` too,
  which doesn't compile.
* A field of `this` that can't be moved out (`return name` in a method that returns a `String`)
  gets rustc's `.clone()` as a quick fix: `return name.clone()`. It used to be only a note,
  because the end of the generated `self.name` didn't map onto `name` (or onto `this.name`). An
  element taken out of a list (`fun first(): String = items[0]`) gets `items[0].clone()` too,
  where rustc suggests `<String as Clone>::clone(&...)`, which isn't Oxie.
* That error names the element and its list: "can't take `items[0]` out of `items`", because a
  list keeps its elements and indexing only reaches one where it is. It used to say "can't take
  `Vec<String>` out of a value that is only borrowed", which named the type, and gave a reason
  that doesn't hold for a list nothing borrows.
* Errors about a field of `this` (moved, borrowed while it changes, taken out of a borrow) name
  it as the code does at each place: "`items` can't be changed while it is borrowed", or
  `this.items` where the code spells out `this`, instead of Rust's `self.items`. rustc's own
  message, on the last line, keeps its wording.
* A labeled block (`'found: { ... }`) gets the declared type of what it initializes, as a plain
  block does, so its value is converted: `"text"` to a `String`, a value to `T?`.
* Windows: scripts and projects whose paths Oxie resolves no longer get the `\\?\` prefix
  written into `Cargo.toml` and build scripts, which Cargo rejected ("invalid path url").
* `oxie init` in a crate that already has `[build-dependencies]` adds `oxie-build` to that
  table instead of writing a second one, which Cargo rejected as a duplicate key.
* The repository moved to [github.com/oxie-lang/oxie](https://github.com/oxie-lang/oxie); links
  to the old address redirect.
* The crates declare `rust-version = "1.82"`, the oldest Rust that builds Oxie and its
  generated code, so an older Rust gets a clear error instead of a failed build.
* Homebrew: `brew install oxie-lang/tap/oxie` installs the prebuilt `oxie` on macOS and Linux.
  Each release updates the formula.
* `println(x)` (and `print`, `format`, `assert_eq`, ...) with a value counts as reading it, as
  `println("$x")` already did: a `String` or `Vec` parameter that is only printed is borrowed,
  and a type parameter that is printed gets its `Display` bound. Before, `fun show(s: String) =
  println(s)` took its argument away from the caller.
* `key in map` looks up a key (`contains_key`), and `'e' in text` a character; both were Rust
  errors before.
* In a throwing function, a Rust call that returns a `Result` used as a statement
  (`fs.write(path, text)`, `out.flush()`) passes its error on, as a throwing Oxie function does.
  Before, the error was dropped with a warning and the function went on. In a function that
  doesn't throw, the warning now says so in Oxie terms and offers `throws` and `.unwrap()`.
* `?` on an optional value in a throwing function, and on a `Result` in a function that returns
  `T?`, are explained as such, with `.ok_or(...)?` and `.ok()?` fixes. Before, both said the
  function doesn't declare `throws`, and the help always named `main`.
* An element of a `Vec<Shape>`, a loop variable over one and an interface-typed field can be
  passed to an `&Shape` parameter; before, they needed `.as_ref()`.
* The reference's `join_with` example compiles.
* Inside `try { }`, a throwing call without `?` (as a statement or as the block's value) jumps
  to `catch`, as `?` does. Before, in a throwing function it left the function, skipping
  `catch`, and in one that doesn't throw its error was dropped.
* `$` followed by a digit is text, as in Kotlin: `"cost $5"` prints `cost $5` (it printed
  `cost 5`). `\$` is a dollar sign in `"""` strings too. In `error enum` messages `$0` still
  names the first positional field.
* A function written `= expr` without a return type no longer drops a value it can't type:
  `fun first() = items.first().copied()` is an error that offers `: i32?`, and
  `fun opt(x: i32) = Some(x)` is inferred to return `i32?`. Before, both silently returned `()`.
* `oxie <command> --help` describes the command. Before, most commands took `--help` as an
  argument: `oxie init --help` set up the current directory, `oxie fmt --help` formatted it, and
  `oxie run --help` ran the project. `oxie test file.oxi` (tests of a script) is in the help.
* A function returning an interface (`fun pick(): Shape`) compiles: it returns `impl Shape`
  without boxing, or `Box<dyn Shape>` when its branches build different classes. Before, its
  value was boxed for an `impl Shape` return type, which never compiled.
* A loop that changes the elements of the list it borrows (`for (x in items) x.qty += 1`,
  `x.tags.push(t)`) borrows the list mutably, and a method that loops over a field like that
  takes `&mut self`. Before, it was an error whose hint was cut off ("use a mutable iterator
  instead: `mut `"). A loop that changes elements through a class's own methods gets a fix to
  loop over `&mut items` (or `iter_mut()`).
* A `var` that rustc calls unused when changing it failed is no longer reported as "can be a
  `val`": `unused_mut` warnings are shown only for builds without errors.
* A method of `T` called on a `T?` is explained in Oxie terms: after `if (x != null)` the fix is
  `if (val x = &x)`, after `if (x == null) return` it is `val x = x ?: return`. Before, rustc
  said "method `len` is private" and suggested `.expect("REASON")`. A value moved out by
  `if (val x = opt)` is named (rustc's message had no name) and gets the fix `&opt`.
* `a?.b` on a `shared class` reads the field through its getter (and `a?.items.len()` through
  one borrow), also deeper in a chain (`a.parent?.parent?.name`) and in statements
  (`a.parent?.tags.push(t)`). Before, it was an error asking for `name()`.
* A loop over one field can call a method with an expression body (`fun note(s: String) =
  log.push(s)`) that changes another field, as it already could a method with a block body.
* `when` on a `String` (or `&String`) with text arms matches it as `&str`; before, it was a
  type error.
* `m[k] = v` on a `HashMap` or `BTreeMap` is `m.insert(k, v)`. `m[k] += 1` is an Oxie error
  that explains the choice: `*m.entry(k).or_default() += 1` or `*m.get_mut(&k).unwrap() += 1`.
* `+` on text that Rust rejects (`first + last` with two `String`s, `a + b` with two `&str`)
  joins with `format!`, as a `+` with a string literal already did; neither side is moved.
  `s += t` with a `String` `t` appends `&t`.

## 0.1.0 - 2026-10-23

The first release of Oxie, a programming language for the Rust ecosystem: a concise syntax that
Kotlin and TypeScript developers already know, compiled to plain Rust. An Oxie project is a
Cargo crate, every crate on crates.io works, and programs run exactly as fast as hand-written
Rust.

### Install

```bash
cargo install oxie
```

Prebuilt binaries for macOS, Linux and Windows are attached below, and `cargo binstall oxie`
downloads them instead of compiling.

### The language

* `val`/`var`, `fun` with expression bodies, string templates (`"$name ${x + 1}"`), default and
  named arguments.
* `data class` with derives worked out for you, `class` with inferred `&self`, `&mut self` and
  static receivers, interfaces, extension functions, operator overloading, `shared class` for
  shared mutable objects.
* Null safety over `Option`: `T?`, `null`, `?.`, `?:`, `!!` and smart checks.
* Error handling over `Result`: `throws`, `throw`, `try`/`catch`, `error enum` with messages, and
  `?` added for you on calls that can fail.
* `when`, Kotlin-style ranges (`until`, `downTo`, `step`), lambdas (`{ it * 2 }`, `x => x * 2` or
  Rust closures), collection operations (`filter`, `map`, `sum_of`, ...).
* Arguments borrowed for you at call sites, trait bounds and lifetimes inferred.
* Rust is always available: any Rust type or crate, `rust { ... }` blocks, macros, and `.rs` and
  `.oxi` files mixed in one crate.

### Tools

* `oxie new`, `init`, `run`, `build`, `check`, `test`, `fmt`, `emit` and `clean`; single `.oxi`
  files run as scripts, with dependencies declared in a header comment.
* Compiler errors and rustc errors point at `.oxi` lines; common borrow errors are explained in
  Oxie terms, with fixes.
* `oxie from-rust` translates Rust code into Oxie.
* `oxie lsp`: a language server with diagnostics, completion, hover showing the generated Rust,
  go to definition, references, rename, signature help, formatting, symbols, inlay hints and
  quick fixes.
* Editor support for Kuana and a tree-sitter grammar.

### Crates

[`oxie`](https://crates.io/crates/oxie), [`oxie-compiler`](https://crates.io/crates/oxie-compiler),
[`oxie-build`](https://crates.io/crates/oxie-build) and
[`oxie-from-rust`](https://crates.io/crates/oxie-from-rust), licensed under MIT or Apache-2.0.
