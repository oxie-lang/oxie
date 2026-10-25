# Compiler internals

This document is for people working on Oxie itself.

## Pipeline

```
.oxi source
  │ lexer.rs      tokens with byte spans; significant newlines; string interpolation parts
  ▼
  │ parser.rs     recursive descent → ast.rs; error recovery per statement
  │ lists.rs      array literals of `var`s that grow (`xs.push(..)`) become lists
  ▼
  │ contexts.rs   context parameters: `context(...)` clauses become parameters whose default is
  │               the caller's variable of that name; a class's methods get the ones they use
  ▼
  │ params.rs     parameters that functions only read become references
  │ sema.rs       crate-wide symbol table: classes, enums, traits, functions, modules,
  │               extension traits; receiver inference; automatic derives
  │ fields.rs     methods that take only the fields they use (then sema.rs again)
  │ params.rs     class parameters, then methods' (after a dry run of the lowering)
  ▼
  │ lower.rs      items, types, patterns → Rust text
  │ lower_expr.rs statements and expressions → Rust text (with source-map marks)
  │ infer.rs      small type inference for obvious cases (return types, receivers)
  ▼
  │ compile.rs    per crate: module tree, `#[path]` module declarations, runtime module,
  │               source maps (.rs.map)
  ▼
generated .rs files → rustc
```

The compiler is a library (`oxie-compiler`) with no dependencies. `oxie-build` calls it from
`build.rs`; the `oxie` command (`crates/oxie-cli`) calls it for checking, `oxie emit`, formatting and the language server.

## Lexer

* Newlines are emitted as tokens only inside `{ }` blocks and at top level, and only when the
  previous token can end an expression and the next line doesn't start with `.`, `?.`, `?:`,
  `&&` or `||`. Inside `( )` and `[ ]` they are skipped. The parser therefore treats `Newline`
  as a statement terminator and skips it where lists continue.
* `>` is always a single token; the parser combines adjacent `>` `>` into `>>` when it is an
  operator, so generics like `Vec<Vec<i32>>` need no special casing.
* Strings are lexed into parts: literal text (already Rust-escaped) and interpolations with
  their source text and byte offset; the parser re-lexes and parses each interpolation, so
  spans inside templates are exact. `"""` strings are dedented while lexing.

## Parser

* One recursive-descent parser for Kotlin-, TypeScript- and Rust-style syntax. Ambiguities are
  settled with small lookaheads:
  * `if (` … `)` is a Kotlin condition unless the closing parenthesis is followed by a binary
    operator (then it is part of a Rust-style condition);
  * `{` in expression position is a lambda; after a lowercase name it is a trailing lambda;
    after a type name followed by `name:`/`name,`/`}` it is a struct literal;
  * `(a, b) =>` and `x =>` are arrow functions (found by scanning to the matching parenthesis);
  * `Type<Args>` in expressions is tried as generic arguments and backtracked if it isn't one;
    so is a `<` after the type of `as`, which is a comparison when it isn't (`n as usize < m`).
* A brace lambda without parameters used as a statement (not the block's value) is an error,
  since it would never run; the help points to `run { }`.
* Errors are collected; after an error the parser skips to the end of the statement.
* `rust { }` blocks and `name!( )` macro calls keep their source text verbatim.
* `[T]` is parsed as a list, `Vec<T>` (`Type::list`), except where Rust takes an unsized slice:
  `ty_base` reads and clears the `slice` flag, which the type before it sets (after `&`/`&mut`
  and `*const`/`*mut`, in the arguments of `Box`, `Rc`, `Arc`, `Cow` and `Weak`, of a trait path
  (`trait_path`: bounds, supertypes, an `impl`'s trait, `Target = `), for an `impl`'s type, an
  associated type and a `where` predicate's type). `[K: V]` is `Type::map`: a single `HashMap`
  segment with `global` set, which no Rust path can be (`::HashMap` would name a crate), so the
  lowering prints std's map (see `std_name` below) while every analysis sees `HashMap`.
* In expressions, `[k: v, ...]` is a `MapLit` of `Pair`s (as `mapOf(k to v)`) and `[:]` an empty
  one. `[1, 2]` stays an `Array`: `coerce` turns it into the declared collection
  (`collection_lit`: `vec![..]` for a `Vec`, `Name::from([..])` for other std collections, maps
  from `MapLit`s), converting each element.

## Semantic analysis (`sema.rs`)

`collect` builds a `Crate` from all files of a crate:

* classes (fields, constructor parameters, methods, generics), enums (variants and their
  shapes), traits (methods, associated types), free functions, module names, extension traits,
  and which enum owns each variant name (for bare variant names in patterns and calls). A class,
  enum or function name that several modules declare is keyed by module (`a::Counter`, see
  `scoped_key`), and lookups take the module they are written in: `class_in`/`fun_in` find
  the module's own declaration or the one it imports (by name or `*`), `class_at`/`fun_at`
  follow a path;
* **receiver inference**: a fixpoint over all methods, default methods of interfaces included.
  A method is `&mut self` if its body assigns into `self` (directly or through implicit
  fields), calls a known mutating method on a field, takes `&mut` of a field, or calls a
  `&mut self` method of the same type or of its interfaces. Interface methods without an
  explicit receiver take `&mut self` if their default body or any implementation needs it, and
  then all implementations use it, also one that changes nothing (and so the methods calling
  it). The first use that changes `self` is kept as the reason (`MethodInfo::recv_why`: "it
  assigns `hits`", with its span);
* **automatic derives**: a fixpoint deciding `Copy` and `Eq + Hash` for data classes and enums
  from their field types.

## Lowering

The `Lowerer` walks one file and writes Rust with an `Emitter` that tracks indentation and
records source-map marks (generated line/column → source offset) at every statement and
expression, and at each name a format string reads inline (`{low}` maps to the `$` of `$low`),
since rustc points inside the string for errors about it. Names Oxie introduces (`__oxie_v1`,
`'__oxie_try1`) are numbered per function, so changing a function renumbers only its own.
Multi-arm `match` lowerings (`?:` with a default that isn't a literal, `?` inside `try`) put
each arm on its own line, as rustfmt does.

Generated code names std items as hand-written Rust does where nothing can shadow the name:
`Lowerer::std_name` is given a path (`std::rc::Rc`) and returns `Rc`, recording a `use` that
`lower_file` inserts below the prelude import (`Emitter::insert_lines` moves the marks after
it). `fmt` and `process` items go through their module (`fmt::Display`, `process::exit`) and
prelude items (`Clone`, `Result`) need no `use`. A name is shadowable when the word appears
anywhere in the file's source (`source_words`, so a user's `Error` class or `Rc` import keeps
std's full path) or is a module declared beside it, when the file imports with a glob, and in
`mod` and `object` blocks, which the file's `use` lines don't reach; there the full path
(`::std::rc::Rc`) is written. `rt_name` does the same for runtime items in the prelude
(`AnyError`).

With an Oxie root, the dry run of the lowering (see `params.rs`) also records what the code calls
(`Lowerer::calls`: methods, getters and setters by name, constructors as `Class::new`), with the
calls in Rust text (`params::raw_calls`). The final lowering gets that set as `crate_calls`:
a shared class's getters and setters that nothing calls are left out (a library keeps those of
its public fields), and generated items that are called lose their `#[allow(dead_code)]`. If
the final lowering calls an accessor the set lacks, it runs again with it added.

The choices the source doesn't spell out are recorded as `Decision`s where their result is
written (`Emitter::decide`): an inferred `&mut self` (with the reason from receiver inference),
a `String`/`Vec` parameter lent or kept (`params::ParamNote`), a lifetime added to a signature,
and the `&`/`&mut` added to an argument or a loop's collection. Each has the source it is about,
what was generated, the reason and the source that decided it, and lands in
`GenFile::decisions`. `oxie emit --annotate` prints them as `/* oxie: ... */` comments, the
language server's hover lists those of a signature, and error explanations use them: the CLI
compiles the crate in memory (`sources.rs`) to find, say, why a method takes `&mut self`.

Contexts tracked while lowering:

* **scopes** of locals (with declared or inferred types) — needed for implicit `this`, deciding
  whether a name is a path root (`std.fs.read` → `std::fs::read`), and inline format captures;
* the **current class** (fields, methods with receivers) for implicit `this` and calls;
* the **function context** (does it throw, its value type, enclosing `try` labels) for `Ok`
  wrapping, `throw`, and `?` inside `try`;
* **loops** for `do { } while` with `continue`.

Notable translations:

* **Tail expressions** are emitted with a `Target` (expected type + optional `Ok` wrapper);
  `if`/`when`/blocks pass the target to each branch, so coercions (`Some(...)`,
  `String::from`) and `Ok(...)` are applied at the leaves.
* **`?.`** is first resolved per link (`resolve_question_dots`): links on known `Result`s become
  `Try` + member access, others are safe navigation lowered to `as_ref`/`map`/`and_then`,
  flattening where the field or method is declared optional.
* **`?:`** uses `ElvisOr::or_val` for literal defaults (handles `Option<&T> ?: 0` and
  `Option<String> ?: "x"`) and a `match` on `Elvis::opt` otherwise.
* **`try`/`catch`** becomes `match ('label: { ...; Ok::<_, E>(value) }) { ... }`; `?` and
  `throw` inside break out of the labeled block.
* **Let chains** (`val a = x && a > 0 && ...`) become nested `if`/`match` (the `else` branch is
  repeated where a part can fail), so they work in every Rust edition.
* **Crate-level lint attributes** in the root file are copied onto every top-level item and
  module declaration, because the generated root is `include!`d and can't hold `#![...]`.
* **Extension functions** become a trait per function; every file imports all of them as
  `_`.
* **Automatic borrowing** (`coerce` with a `&T`/`&mut T` parameter type): value arguments get
  `&`/`&mut`; references (`&x`, reference-typed locals, `self`, string literals, untyped
  lambda parameters) don't. Receiver inference (`sema.rs`) treats a field passed to a `&mut`
  parameter as a mutation of `self`.
* **Borrowed parameters** (`params.rs`, before lowering): a `String`/`Vec<T>` parameter of a
  top-level function becomes `&str`/`&[T]` when every use is a read (a method that exists on
  `str`/slices, printing, comparing, indexing, or passing it to a parameter that is itself
  borrowed); a map or set (`Kind::Coll`) becomes `&HashMap<K, V>` and a `PathBuf` `&Path` the
  same way. `pub` functions are left out, except in a program (`main.oxi` without `lib.oxi`),
  which no other crate calls. A field written without its value (`Point { x, y }`) or Rust text
  (`rust { }`, a macro's arguments) that names the parameter keeps it owned. A greatest fixpoint over the crate handles forwarding and recursion; a
  `move` lambda or async block capturing the parameter keeps it owned. A `Vec<T>` is lent only
  when dropping a `T` just frees memory (`Types::drop_free`: primitives, `String`, std
  containers of such types, and Oxie classes, enums and aliases made of them with no `Drop`
  impl, in Oxie or in a `rust { }` block), since lending moves the drop to the caller. Types
  Oxie can't see into, type parameters and `Rc`/`Arc` (whose counts can be observed) keep it
  owned, and the note names the element type as the cause. `kept_reason` reports the
  first use that keeps a parameter owned ("it returns `v`"), for the explanation of E0382.
  Parameters of Oxie classes and `Box<T>` go through the same analysis after receiver inference
  (`borrow_class_params`, `Stage::Types`), since what reads them takes the classes: their
  methods that take `&self`, and field chains (`chain_type` follows the fields' types) that are
  formatted, compared, indexed, lent on, `Copy`, or the receiver of a method that only reads
  their type. `Copy` and `shared` classes, classes with lifetimes, and functions whose result
  is a reference are left out. The pass runs next to `fields.rs`, before sema collects again.
  Methods go through it last (`borrow_method_params`, `Stage::Methods`), with every kind of
  parameter: a call `x.f(..)` from a method of the same class (`f(..)`, `this.f(..)`) lends when
  `f`'s parameter does. A lent parameter only works where the lowering writes the `&`, in calls
  it resolves to the method, so `compile.rs` first lowers the crate once without keeping the
  result: `Lowerer::unresolved` collects the names of methods called where the receiver's type
  doesn't say which method it is (`may_reach_oxie`: unknown, `Box<..>`, a crate's type), after
  `?.`, or through a path the lowering can't follow, and methods of those names keep their
  types. So do methods used as values, called from Rust text (`raw_calls`) or implementing an
  interface or operator, a library's `pub` methods, and every method of a Rust crate with Oxie
  modules, whose Rust code may call any of them.
* **Methods that take only the fields they use** (`fields.rs`, after receiver inference): a
  `&mut self` method that a method of its class calls inside a loop over another field (or a
  lambda passed to a method of one) becomes `static`, with the fields it uses as parameters.
  The loop has to keep the field borrowed (`borrowed_field`): the field itself or a lazy view
  of it (`items.iter().enumerate()`), not a copy (`items.clone()`). The fields come after its
  own parameters, and its calls pass `this.field` for them (in parameter order, with left-out
  defaults filled in). `field_uses` records where the body uses each field, which decides the
  parameter: by value for a `Copy` field it only reads, `&mut` for one it changes (assigns, calls
  a `MUTATING_METHODS` method or one Oxie doesn't know on, borrows mutably), else `&T`, or
  `&str`/`&[T]` when every method called on it is in `STR_READS`/`VEC_READS`; and where the
  body reads it as `*field`. The method must use at most three fields, not change a field
  borrowed around a call nor read one borrowed mutably there (those conflicts are real), not
  use `this` itself, call no method of the class, have no result borrowed from a field, and
  only be called from its class
  (`used_outside`, by name). The rewritten AST goes through `sema::collect` again, so the
  callers' receivers come from the field parameters, and `ParamNote`s say why each field is a
  parameter.
* **Arguments that change `this`** (`move_args_changing_this` in `lower_expr.rs`): for a
  statement, block tail, `when` arm or lambda body that calls a method on a field of `this`,
  `calls_changing_this` walks the arguments in Rust's evaluation order and collects the calls
  of `&mut self` methods of `this`. It gives up when one may not run (in a lambda, an `if`,
  after `&&` or `?.`) or when something that could notice the new order (a call, a field read,
  an operator) runs before one. Each call becomes a `let` named after its method (`next_id`,
  then `next_id_2`, avoiding locals, functions and imports of that name), and the statement
  reads the locals. Only for receivers the call borrows (`borrowed_by`), so every statement
  this changes is one rustc rejects. A statement with attributes stays as written
  (`keep_args`): `#[cfg(test)]` on the first `let` alone would leave the call without it.
* **Inferred bounds** (`bounds.rs`): before a function or method signature is emitted, its
  body is scanned for uses of values of a type parameter (parameters typed `T`/`&T`, elements of
  `&[T]`/`Vec<T>` in `for` loops, lambdas over such collections, `val y = x` aliases) and the
  matching `where` predicates are added.
* **Collection operations** are the `ListOps` trait in the runtime module, implemented for
  `[T]` and imported as `_` everywhere, so `xs.map { }` lowers to `xs.map(|it| ..)` unchanged.
  Untyped parameters of predicate lambdas (`filter`, `find`, `any`, ...) compared with a scalar
  literal are read with `crate::__oxie::val(it)` (the `Val` trait strips any number of
  references from numbers, `bool` and `char`).
* **Shared classes** (`shared.rs`): the class is lowered as `{Name}Data`, then the handle
  (`Rc<RefCell<..>>` or `Arc<Mutex<..>>`), forwarding methods, the getters and setters the
  crate calls, and identity `PartialEq`/`Hash` are emitted. Expression lowering routes `obj.field` reads, assignments
  and calls on fields of known shared types through getters, setters or a single borrow,
  evaluating arguments before a mutable borrow; shared places used by value get `.clone()`.

## Source maps and diagnostics

Each generated file `x.rs` has `x.rs.map`:

```
oxie-sourcemap 1
/abs/path/src/x.oxi
<gen line> <gen col> <src line> <src col>
...
```

`crates/oxie-cli/src/cargo.rs` runs cargo with `--message-format=json`, maps the primary span of each
message (following macro expansions back to the call site) to the `.oxi` file, and renders it
in rustc's format. Secondary labels are mapped too; those that don't map exactly (the generated
text between the nearest mark and the span differs from the source) are dropped unless an
explanation places them. rustc suggestions whose spans map exactly are kept as edits, except
the parts of a suggestion that edits several places, which only work together.

`explain.rs` then rewrites the common errors (moves, `val` mutation, borrow conflicts, `&`
parameters, closures outliving their scope, missing `throws`, `Display`, bounds and imports,
Kotlin method names) in Oxie terms and computes fixes as text edits on the `.oxi` source,
found from the source text and the mapped spans. The CLI prints the fixes with a preview of
the changed line; the language server attaches them to the diagnostic (`data.fixes`) and
returns them from `textDocument/codeAction`. Errors caused by an Oxie decision say why: a move
into a function that keeps its parameter points at the use `params::kept_reason` finds, and a
borrow that ends too early because of the shared `'a` of automatic lifetimes shows the
signature with a narrower one.

A fix that costs nothing comes before a copy, and a copy is offered only when it compiles: a
moved value gets `.clone()` only where rustc suggests it, which rustc does only for types that
implement `Clone`. Two borrow conflicts are recognized in the parsed file and get a form without
a copy: a map lookup returned before an `insert` into the same map (the entry API), and a loop
over `v.iter().enumerate()` that changes `v[i]` (a loop over `v.iter_mut()`). The entry API
takes an owned key, so it costs nothing only when the insert takes the key as it was looked
up; a key the insert converts (`key.to_string()`) would be converted on every call, which the
explanation says, without a fix.

An explanation sees every `.oxi` file of the crate (`sources.rs`: the project's `src/`, or the
script): the function a value moved into, a type or a `shared class` may be declared in
another file. Labels (`Label::file`) and edits (`Edit::file`) there name their file; the CLI
shows such labels under `::: file:line:col`, and the language server sends them as related
information and the edits as part of the quick fix. A name declared in several files isn't
looked up, since which one is meant isn't known.

`?:` and `?.` on a value that can't be null are recognized from rustc's errors about the
runtime helpers (`ElvisOr`, `Elvis`, the `as_ref`/`map` of a safe call) and the `Elvis` or safe
call expression at the error's position in the parsed file. The explanation `covers_position`:
rustc's later errors at that position (a `?` in a `throws` function) aren't shown.

Before any of that, `Mapper::compiler_bug` sets aside errors that only a bug in Oxie can cause:
one inside `__oxie.rs`, or a Rust syntax error in code generated from a file without verbatim
Rust (`rust { }` blocks and macro calls). They are reported as "Oxie generated Rust that doesn't
compile" with rustc's message and the generated line, never explained as the user's error.

The maps are used at run time too. `oxie run` pipes the program's stderr, and `oxie test`
cargo's stdout and stderr, through `locations.rs`, which rewrites every `path.rs:line:col`
whose file has a map (panic messages, backtrace frames, failed assertions) to the `.oxi`
position, line by line as the output arrives.

## Tree-sitter grammar

`editors/tree-sitter-oxie/grammar.js` is a separate, deliberately shallow grammar for editors:
declarations, calls, member access, strings with interpolation; everything else is a token
stream grouped by brackets. It parses all repository files without errors and is used for
highlighting, outline, indentation, text objects and runnables.
`editors/build-kuana-extension.sh` regenerates it and copies it into the Kuana extension.

VS Code highlights with a TextMate grammar instead,
`editors/vscode-oxie/syntaxes/oxie.tmLanguage.json`: regular expressions, edited as JSON. Its
scope names follow VS Code's Rust grammar wherever Oxie and Rust share a construct, so a `.oxi`
file and the Rust generated from it look alike in any theme. `rust { }` blocks embed the Rust
grammar; braces inside them are counted by the Oxie grammar. `npm run test:grammar` checks the
scopes asserted in `editors/vscode-oxie/test/grammar/*.oxi`.

## Tests

* `crates/oxie-compiler` unit tests: lexer and formatter.
* `crates/oxie-cli/tests/e2e.rs`:
  * `tests/e2e/*.oxi` programs run and compared with `.out` files;
  * `tests/coverage/*.oxi`: one program per Rust feature, compared with `.out` files;
  * `tests/diagnostics/*.oxi` must fail with `// error: <line>: <text>` errors at the right lines
    (and without the `// not: <line>: <text>` ones), each shown once; projects in
    `tests/diagnostics/*/` are built the same way, for explanations that use other files;
  * every example in `examples/` runs (compared with `expected.txt`) and its tests pass;
  * every runnable code block in `README.md` and `docs/*.md` compiles and runs.
* `crates/oxie-cli/tests/emit.rs`: the Rust generated for `tests/emit/*.oxi` must equal the
  `.rs` (and `.annotated.rs`) files next to them; `OXIE_BLESS=1` rewrites them.
* `crates/oxie-cli/tests/runtime.rs`: panics under `oxie run` and `oxie test` point at `.oxi` lines.
* `crates/oxie-cli/tests/lsp.rs` drives the language server over stdio. One test applies every
  quick fix offered for `tests/diagnostics`, one at a time, to a copy of the program and builds
  it again: the error the fix was offered for must be gone, and the lines the fix changed must
  have no new errors.
* `editors/vscode-oxie`: `npm test` runs the grammar tests, then integration tests in VS Code
  (`test/suite`) with the language server on a copy of `test/fixtures`.
* `tests/run_e2e.sh` runs the e2e programs from the shell.

`e2e.rs` and `from_rust.rs` run their programs on as many threads as there are cores. Each
thread builds in its own `OXIE_CACHE` under `target/tmp/oxie-cache/<n>`, because builds that
share a target directory wait for each other; `cargo clean` removes these caches. Failures are
reported in the order of the programs' paths, as they were when the programs ran one at a time.
