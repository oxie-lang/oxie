# Changelog

## Unreleased

A parameter is borrowed unless it says `own`. Oxie used to decide from a function's body whether
a parameter declared as a value (`name: String`, `u: User`) was lent or taken, and that decision
also depended on code elsewhere: a call Oxie couldn't follow, a function used as a value, or
`pub` in a library changed it, and the errors appeared at calls nobody had touched. Now the
signature alone says whether a call keeps the caller's value.

### Upgrading

* A function that keeps a parameter (stores it in a field or a list, returns it, gives it to
  another `own` parameter or a constructor, takes a field out of it, uses it in a lambda that
  outlives the call) is now an error that offers two fixes: write `own` before the parameter
  (`fun add(own item: Item)`), which moves the caller's value as before, or keep a copy. Text
  is the exception: a `String` or `String?` parameter that a function keeps is copied, as any
  `&str` is.
* Enums, interface values, type parameters and optionals are borrowed too, where they used to
  be taken: `e: Expr` is `&Expr`, `s: Shape` `&dyn Shape`, `x: T` `&T` and `u: User?`
  `Option<&User>`. A generic class that stores what it is given takes it:
  `fun push(own x: T) = items.push(x)`. `Copy` enums, type parameters that must be `Copy`
  (`fun sum<T>(a: T, b: T) = a + b`) and function types stay values.
* A function that changes a borrowed parameter is an error whose fixes are `&mut T` and `own`.
* `pub` functions and methods of a library, methods called where Oxie can't tell their type,
  functions used as values (`xs.map(greet)`, which now lend through a lambda) and functions that
  Rust code calls now borrow their parameters too. Rust code calling such a function passes a
  reference (`f(&s)`), or the function takes the value with `own`.
* A list of values whose drop does more than free memory (mutex guards, files) is dropped by the
  caller when it is lent; take it with `own` to drop it in the function, as before.
* `oxie from-rust` writes `own` for the parameters Rust takes by value.

### Changes

* `own name: T` takes a parameter's value; the function may change it (`mut` in the Rust).
* Errors that Oxie finds itself can carry fixes, printed as `= fix:` lines and offered by the
  language server as quick fixes.
* `&name`, `name.as_str()` and `name.clone()` of a borrowed parameter are the reference it is and
  an owned copy (`to_string()`, `to_vec()`, `to_path_buf()`); `names.contains(&who)` with
  borrowed text compares one element at a time, as `who in names` does.
* A moved-value error for an `own` parameter that the function only reads offers to drop `own`.
* An optional argument is lent as it is: `greet(nick)` passes `nick.as_deref()` to a `String?`
  parameter, a value is passed as `Some(&x)` and `null` as `None`.
* A class or enum that derives `Copy` itself (`@derive(Clone, Copy)`) is passed as a copy, like
  one Oxie derives `Copy` for.
* The VS Code, Zed and Kuana extensions highlight `own`.

## 0.5.0 - 2026-10-27

`oxie from-rust` now translates all the Rust it was tested on into Oxie: in 82 crates with
789,000 lines (serde, syn, clap, futures, rand, regex-syntax, the Kuana editor, Oxie's own
compiler, ...) it keeps no item or expression in a `rust { }` block, where 0.4.1 kept 1,510 of
them (58,700 lines). Oxie gained the Rust forms that needed, and fixes for what translating that
code turned up. Translated whole, the Kuana editor (137 files) builds and passes 442 of its 443
tests without any Rust left in it (the other one checks which file a panic names).

A round of bug hunting with several hundred new programs found code that compiled to Rust doing
something else than the Oxie said, and much valid Oxie that generated Rust that doesn't build.
Those are fixed below; the silent ones first.

### Upgrading

* Set `oxie-build = "0.5.0"` in the `[build-dependencies]` of each project (`cargo update`
  doesn't move from 0.4 to 0.5). `oxie` warns when a project builds with another `oxie-build`.
* The first group of changes below makes code do what the Oxie says where 0.4 did something
  else: default arguments, the order of named arguments, `when` arms that name a constant or a
  variable, range bounds, `copy`, loops that change their elements, and a few results. Code that
  relied on the old behaviour runs differently; read them before upgrading.
* A lambda Oxie makes `move` that uses a variable changed after it is now an error; the message
  says to take the value with a capture list. The other forms 0.5 adds were errors in 0.4.

### Changes

* A default argument means what it means where it is declared: a field or method in it is the
  receiver's, and a constant or function of the callee's module is that one. Before, a call from
  another class or module read its own variable of that name.
* Named arguments run in the order they are written (`place(y = next(), x = next())` computes
  `y` first), as in Kotlin; they ran in the order of the parameters.
* A `when` arm naming a class or companion constant, or a variable, compares the subject with
  it, as in Kotlin; it was a new name that matched every value. An arm that uses the name binds
  it, as in Rust (`n if n > 9 -> n`).
* `x in lo()..hi()` computes both bounds, in the order written; `hi()` was skipped when the first
  comparison failed, and `downTo` computed its bounds in reverse.
* `copy` of a class whose properties are computed from the others, or that has `init` blocks,
  builds the copy with the constructor, so `Rect(2, 3).copy(w = 10).area` is `30`, not `6`.
* A loop whose body changes the elements changes them in the collection also over a `[..]`
  literal and with a `var` loop variable, and over a shared object's list field, which the loop
  locks while it runs. Before, these changed copies.
* A lambda Oxie makes `move` (stored, returned, sent to a thread) that uses a variable the code
  changes after it is an error: it kept the old value, where Kotlin's lambda sees the change. A
  capture list takes the value on purpose. A body property's lambda that uses a constructor
  parameter the object keeps too is an error with the same fix (it failed to build).
* An empty sum of floats is `0` (Rust's is `-0.0`, printed `-0`), `max_by_key` keeps the first of
  equal maxima, as Kotlin's `maxBy` and `min_by_key` do, and `${price:.2}` on `null` prints `null`
  (it printed `nu`).

* Rust's slice can be named where `[T]` is a list: `slice<T>` (`Mutex<slice<u8>>`,
  `struct Bytes(slice<u8>)`).
* Crates whose `src/lib.rs` or `src/main.rs` says `#![no_std]` (or `#![cfg_attr(.., no_std)]`)
  build: the generated code uses `core` and `alloc`, and so does Oxie's support module.
* A `mod name` line declares the module of `name.oxi` where it stands, with its attributes
  (`@cfg(unix) mod unix`, `@macro_use mod macros`), so the module sees the macros declared
  before it, as in Rust; `oxie from-rust` keeps such lines.
* A call of a throwing interface method, of a method of a type parameter bound by an interface,
  of a default method or of a throwing extension function passes its error on, also as a single
  statement and after `?.`.
* Attributes go before parameters, arguments, elements, lambdas and the fields of struct
  literals and enum cases: `fun open(path: &str, @cfg(unix) mode: u32)`, `vec[1, @cfg(test) 2]`,
  `fold(0, @inline(always) { acc, x -> acc + x })`, `Fd { @cfg(unix) raw: 3 }`. A call made in the
  builds without a `@cfg` parameter leaves its argument out, and a method declared once per
  configuration (`@cfg(windows) fun new(c, console)` beside `@cfg(not(windows)) fun new(c)`) takes
  either call. The statements a block ends with may carry `@cfg` attributes: the one the build
  keeps is the block's value (`@cfg(unix) run { .. }` then `@cfg(not(unix)) run { .. }`).
* Function pointer types take Oxie's types and may name their parameters:
  `fn(&Field, i32?) -> bool`, `unsafe extern "C" fn(i32) -> i32`, `for<'a> fn(&'a str) -> &'a str`.
* A method can be called through its type with the receiver first (`Counter.get(c)`), and a
  function named like a class (`fun SplitSink(lock): SplitSink`) is what `SplitSink(..)` calls.
* A class that implements an interface's `new` (`impl Make for Pair { fun new(n: i32): Pair }`)
  gets no generated constructor, which hid it.
* More of Rust is accepted as written: `mut self: Box<Self>`, `&raw const x` and `&raw mut x`,
  `extern crate`, `unsafe extern "C" { }` blocks and `extern` blocks, `static var` and
  `macro_rules!` inside functions, macros among a class's or interface's members, type and pattern
  macros (`Token![,]`), `for<'a>` bounds in `where` clauses and supertypes, `Vec<<T as Tr>::A>`
  and `f::<<T as Tr>::A>()`, `pub ()` fields, `self::NAME`, `ref s @ ..` and `1..=LIMIT` patterns,
  struct literals of lowercase types (`libc::timeval { tv_sec: 1 }`), `AssertUnwindSafe { .. }` and
  other trailing lambdas after a capitalized name, `#![..]` before a file's `//!` docs, and
  fill characters in format specs (`"${n: >5}"`, `"${n:*^7}"`).

* After `is Shape.Circle` (a `when` arm on a variable, or `if (s is Shape.Circle)`), `s.radius`
  reads that case's field, as Kotlin's smart cast does.
* Objects hold objects, classes and enums: `Defaults.Colors.BG`, `Shapes.Circle(1.0)`.
* A local `fun` that uses the variables or fields around it is a closure (Rust's nested `fn`
  can't see them).
* A `for_each` statement whose lambda throws is the `for` loop it stands for, so the error leaves
  the function, as with Kotlin's inline `forEach`.
* `x!!` on a value that can't be null is a warning, as in Kotlin, and does nothing.
* `a += b` on a class with `operator fun plus` and no `plusAssign` is `a = a + b`, and an operator
  taking `&T` (`operator fun times(o: &Matrix)`) lends its right side: `a * b` is `a * &b`.
* `fun f() = a?.b` without a declared type is optional for its callers; one that calls a method
  giving nothing (`maybe?.inc()`) is a statement.
* `s += 'c'` appends the character, a format spec may name a field (`"${name:<width$}"`), and
  `std.collections.HashMap()` is `HashMap::new()`.

### Fixes

* Only the last of several `init` blocks ran.
* `?.` after a function or method declared in Oxie with a name std uses for `Result` methods
  (`text`, `open`, `create`, ...) worked like Rust's `?` and returned early from the caller, and
  `s?.rfind('z')` and other std methods that give an `Option` made an optional of an optional, so
  `!= null` was always true.
* Receivers: a method that changes what its fields reach (`inner.counter.add(1)`,
  `list[0].add(1)`, `pair.0`, `maybe!!`, `maybe?.add(1)`, a property made by a constructor, map
  entries in a loop) takes `&mut self`, and one that only changes a `Cell`, `RefCell`, `Mutex`
  or atomic field stays `&self`, so such objects can be shared through `Rc` or `Arc`.
* `c?.inc()`, `xs?.push(x)` and `stack?.pop()` borrow the value mutably when the call changes it.
* Values from `?.` chains, from `if` and `when` with a `null` branch, from generic functions,
  methods of generic classes, interface methods, `x!!` and `collect::<T>()` print as lists and
  optionals.
* Shared objects: `if (val n = head)` copies an optional handle, a block's value can read a
  field of an object declared in the block, `s.log.push("x")` converts the value, calls that
  change a field (`entry(..).or_insert(..)`, `iter_mut()`) borrow the object mutably, and a data
  class that holds a handle derives `Debug` (the handle prints as `Node(..)`).
* A lambda passed to a method or constructor of a generic class named a `T` that isn't defined
  there.
* `when` on a field reached through a reference or a list element moved out of it.
* `break` and `continue` inside `try` in a loop, and a `try` that ends in `return` or `throw`,
  generated Rust that doesn't build.
* The `else` of `if (a != null && b != null)` saw `a` unwrapped, and `else if (x == null) return`
  moved `x` away from the code after it.
* Calls of extension functions and of interface methods on interface values get defaults, named
  arguments and conversions, and interface calls lend references; so do methods called on a list
  operation's element or after `?.`.
* Generic recursive types (`enum List<T> { Nil, Cons(T, List<T>) }`) are boxed, methods are
  called through the boxes, `when (this)` binds the fields of a case in an enum method, and an
  optional recursive field reads as an optional.
* A function returns a lambda unboxed, as `impl FnMut` when it changes its captures, with
  `+ '_` when it (or a returned iterator) uses a borrowed parameter or `this`, and boxes a
  function a returned lambda returns. A lambda assigned to a function-typed field or passed to a
  method that stores it is `move`, and an extension function can take a lambda.
* A field read after a call in a `?.` chain (`parent?.borrow().value`) is read by value; it
  referred to a temporary.
* An interface value that already is a reference (a `&Shape` field, an element in
  `iter().filter { }`) is passed to an `&Shape` parameter as it is, not as `&&dyn Shape`.
* A method whose interface result is an associated type set to `()` returned its last
  expression; a result type came from an untyped number (`if (n == 0) 1 else n` with `n: u64` gave
  `i32`).
* A file that imports `anyhow.Context` gets anyhow's `context` without a clash with Oxie's.
* `Step { n: i32 -> n * 2 }` is a trailing lambda, not a struct literal; `1..=10 step 3` steps
  through the whole range; a `rust { }` block with statements is a value; an enum case named
  `None` matches in a `when` on an optional; an extension of a number takes it by value
  (`fun i32.is_positive() = this > 0`).
* `var label = "none"` that later gets a `String` (`label = "$n items"`) is a `String`.
* The condition of `do { } while (cond)` sees the variables of the body, as in Kotlin, and a `do`
  loop can be labeled (`outer@ do`).
* Tools: the language server crashed finding references in text with characters of several
  bytes; a UTF-8 byte order mark at the start of a file was rejected; `oxie new build` made a
  project cargo refuses; `oxie init` in a crate with a build script didn't call
  `oxie_build::compile()`; `oxie from-rust` kept a type as Rust when two of its traits have a
  method of the same name.
* A string literal that spans lines inside a macro call, a `rust { }` block or an `r"..."` string
  kept the generated code's indentation on its later lines, which changed the string.
* Attributes on a function's parameters, and on the named fields of an enum case, were dropped.
* `return !done` in a function body was read as the start of a macro definition.
* A name a block imports (`import super::escape` inside a test named `escape`) now hides the
  module's function of that name, and `ser.SerializeMap` after `import serde::ser` is serde's,
  not a class of the crate with that name. A type or function imported into a module from
  elsewhere in the crate is the one a call there means, also when another module declares the
  same name.
* `if (val (Some(a), Some(b)) = (x, y))` and the like matched an optional instead of the tuple:
  only a tuple of names unwraps an optional pair, and Rust's `if let` keeps Rust's meaning.
* `var (row, at) = first` lost its `mut` when only a destructuring assignment
  (`(row, at) = (row + 1, 0)`) changed it.
* A lambda parameter that is an `OsStr`, `OsString` or `Cow<str>` compares with a text literal
  (`path.extension().is_some_and { it == "rs" }`).
* `when (self.goal) { Some(g) -> g; else -> self.measure() }` reads `g` by value when the field
  is borrowed for the match and its type may be `Copy`, as Rust's match by value does.
* `Name { 1 }` is a lambda, not a struct literal; a type parameter's default (`T = i32`) no longer
  goes into the class's `impl`; `name! { .. }` macros print with a space, and an item-declaring
  one (`thread_local!`) without a `;`.
* `oxie from-rust`: calls of macros whose name a method or function takes (`write!` in a type
  with a `write` method) keep their `!`; a module function called from a method of the same
  name is called as `self::name(..)`; a closure that captures a variable named `it` stays
  `|| ..`; a `#![..]` inside a `match` becomes an attribute of the statement, and lint
  attributes on `match` arms are kept; inner docs of an
  inline module become its doc comment; `matches!(..)` before a method call gets parentheses;
  types with an interface's `new` stay structs; one TODO line per reason above methods kept as
  Rust; it no longer crashes on items `prettyplease` can't print (`safe fn` in an `unsafe extern`
  block).
* Null safety: a `Copy` value changed after a null check (also after `if (x == null) return`) is
  changed in place; `x != null && val n = ..` in a let chain unwraps `x`; `when (val c = opt)`
  unwraps `c` in the `else` after a `null` arm; a null check unwraps a local holding `map.get(k)`,
  `pop()` and the like; `if (val r = &mut opt)` knows what `r` is; a statement `when` on an
  optional number needs no `else`; `?:` after a lookup of numbers gives the number, between two
  results tries the second, and takes any `&str` as its text default; `opt?` in a function giving
  a `T?` is the value; a `T?` (or `&x`) passed to a `&T?` parameter is lent, and a tuple branch
  with a `null` part makes that part optional.
* `?.` chains: `?.` after `chars()`, `iter()` or `split()` reads as `.`; a call inside one converts
  text arguments (`log?.push("x")`); `?.field` of a call's result takes the field; an iterator's
  `find` or `next` gives owned items after `enumerate()` or `map { }`; `x?.let { "a" }` may ignore
  its value; a throwing call at the end of a chain over results passes its error on.
* Generics: bounds are inferred for map values and loops over maps, values a `() -> T` parameter
  gives, locals declared `[T]`, `T?` fields, copies of a `[T]`, `xs.first()` after a null check,
  and arithmetic of `T`s; a method of a generic class takes its parameters with the receiver's
  type arguments; `Option.replace` and `take` keep their type.
* Ownership: a loop borrows the list mutably when the body passes the element to a `&mut`
  parameter, changes it with an extension function or calls a method that changes one of its
  fields; an array passed to a function that grows it is a list; `x > best!!` compares without
  taking the value; `first_word(c: &mut Cursor): Word` borrows the text, not the cursor, for as
  long as the result lives; arguments that read a field before a call that changes `this` see the
  old value, as in Kotlin; `when (c) { in vowels -> }` lends `vowels`.
* `this = x` in a method that takes `&mut self`, `copy(x = ..)` without `this`, `to_string()` in a
  type with its own `toString`, `*this` in an extension of a number, `(x, y)` destructuring of data
  classes in `for` loops and lambdas, an interface value for a `Shape?` field, a class imported
  under another name (`import shapes.Circle as C`), `import util` in the crate root, a field named
  like a function, an error class or enum with its own `toString`, `try`/`catch` and `if`/`when`
  values that need a conversion, `fun f() = expr` bodies over text iterators, array literals'
  `map`, `if (val Case(n) = xs.last_mut())`, and `.context()` in a function that throws
  `anyhow::Error` all failed to build.
* A nested pattern in a field Oxie boxes (`Add(Num(_), Num(_))` in a recursive enum) is checked in
  the arm's guard; Rust can't match through the box.
* Type inference looked at a call's receiver several times, so a long method chain (a UI builder)
  took exponential time; `oxie from-rust` hung on such code. Each receiver is inferred once now.
* A `filter_map` or `find_map` lambda whose body already gives an optional (of a type Oxie doesn't
  know) was wrapped in `Some`; a loop variable named like its list made the loop borrow the list;
  `+ '_` was added beside a `&'static` parameter; a class constant named `None` took the
  optional's `None` arm; cases imported with a glob (`import super::Slot::*`) got a qualified
  path; a call's result was lent to a reference field (`Box.leak(..)`).
* Messages say what to write instead for Kotlin's `vararg`, `indices`, `toDouble()` on a number,
  `componentN`, `List`/`Map`/`Set`, `maxOf`, `require`, `compareTo`/`get`/`set` operators,
  `Box(x)`, a `String` constant, a growing `val` array, `()` from `sort()`, numbers of two types,
  a missing interface method, a `when` on text without `else`, `null` returned from a function that
  isn't optional, a `T?` used as a `T`, a throwing call printed without `?`, a parse error `?`
  can't convert, and a result borrowed from a parameter the function owns.

## 0.4.1 - 2026-10-26

Fixes for the tools around the compiler; the language is unchanged. Projects on `oxie-build`
0.4.0 can stay on it, or move with `cargo update -p oxie-build`.

* `oxie` keeps cargo's output plain when it doesn't color its own (`NO_COLOR`, or output to a
  pipe), even with `CARGO_TERM_COLOR=always` in the environment. Before, that variable colored
  `oxie test`'s output inside pipes and broke matching on it.
* On Rust before 1.84, rustc's "use parentheses to call this closure" suggestion for a lambda
  passed where a value is expected is no longer offered as a fix: mapped onto Oxie's braces, it
  broke the line.
* On Windows, messages name files relative to the current directory again when the system
  reports it by its short name (`C:\Users\RUNNER~1`): a script's errors and its
  "could not compile" line showed the full path instead.

## 0.4.0 - 2026-10-26

Oxie 0.4.0 removes most of the Rust still visible in everyday code. Optionals are matched and
compared by their value (`when (light) { Red -> ...; null -> ... }`, `xs.first() == 3`), lists,
maps and optionals print with `"$x"`, recursive types need no `Box`, and `when` arms, text and
lambdas read as written: no `*n`, `.as_str()`, `.to_string()` for text put into a list, or `move`
on a lambda a function returns. Around 70 fixes make null checks, `?.` chains, interfaces, loops
and diagnostics behave as Kotlin and TypeScript readers expect, the generated Rust reads more
like hand-written Rust, and Zed gets an extension.

### Upgrading

* Set `oxie-build = "0.4.0"` in the `[build-dependencies]` of each project (`cargo update`
  doesn't move from 0.3 to 0.4). `oxie` warns when a project builds with another `oxie-build`.
* Code that compiled with 0.3 compiles with 0.4: the forms 0.4 adds were errors before, and the
  ones they replace still work (`${xs:?}`, `Some(..)` patterns, `Box<Expr>` fields with
  `Box.new(..)`, `move { }`, `.to_string()`).

### Changes

* Everyday forms that needed Rust's help now read as written:
  * a `when` arm's names have the types of the case's fields, so a text field matches text arms
    (`when (name) { "sqrt" -> ... }`), and a borrowed number, `char` or `bool` is read where its
    value is needed (`Num(n) -> n`, `op == '+'`, `if (on)`) without `*`;
  * `sorted_by`, `sorted_by_descending`, `max_by_key`, `min_by_key` and `join_with` on a list
    borrow a field key: `sorted_by { it.name }`, `join_with(", ") { it.name }`;
  * text put into a list, set or map of `String`s is converted: `names.push("bob")`;
  * `var s = ""` grown with `s.push(c)` or `s.push_str(t)` is a `String`, as with `s += t`;
  * a lambda that a function returns, that is stored in a field, or that runs on another thread
    (`thread.spawn { }`, `tokio.spawn(async { })`) is `move` without writing it.
* Recursive types need no `Box`: a field that holds its own type, directly (`Neg(Expr)`), as an
  optional (`next: Node?`) or through another type of the crate, is boxed, values going into it
  are boxed (`Expr.Neg(x)`), and `when` arms, calls and fields read through the box. Before,
  rustc reported a recursive type with infinite size. The calculator example's `Expr` drops its
  `Box<Expr>` fields and `Box.new` calls.
* `when`, `if (val ...)`, `while (val ...)` and `==` on an optional match and compare its value
  without `Some(..)`: `when (light) { Light.Red -> ...; null -> ... }`,
  `while (val Token.Op(op) = peek())`, `xs.first() == 3`, `name == "ada"` on a `String?`.
  Before, rustc rejected them with mismatched types. A `when` that leaves out `null` is
  explained in Oxie terms, and a name that catches every value of a `when` has the subject's
  type (so `"$x"` prints an optional).
* `"$xs"` and `println(xs)` print lists, arrays, maps, sets and tuples in their debug form
  (`[1, 2]`), and an optional as its value or `null` (format specs apply: `"${price:.2}"`).
  `"${x:?}"` still prints Rust's debug form. Before, they were errors that asked for `:?`, and
  an optional printed as `Some(..)`. A `?.` chain, and a local set from one, prints as an
  optional; Oxie now also knows the types of list operations' results (`nums.map { }`,
  `xs.first()`), of `map[key]`, of `a ?: return`, and of an `if` that unwraps a value.
* The generated Rust reads more like hand-written Rust. Std items are named the way people write
  them (`Rc<RefCell<..>>`, `impl fmt::Display`, `Result<T, E>`) with `use` lines at the top of
  the file, unless something in the file could shadow the name. A shared class gets only the
  getters and setters code calls (a library keeps those of its public fields), generated items
  that are called lose `#[allow(dead_code)]`, and `a.count += 1` on a shared object no longer
  goes through a temporary.
* The docs site labels each example's Rust tab "Generated Rust": it is what `oxie emit` prints,
  not a hand-written comparison.
* A Zed extension (`editors/zed-oxie`): highlighting, outline, ▶ run and test buttons, tasks,
  snippets and the language server, from the same grammar and queries as Kuana's.
* A throwing call in a `when` arm or a brace-less `else` (`if (c) a() else b()`) passes its error
  on, as any statement in a throwing function or a `try` does. Before, the error was dropped with
  only a warning, and the function went on.
* A default value that names another parameter (`fun connect(retries: i32 = 3, verbose: bool =
  retries > 2)`, `class Rect(w: i32, h: i32 = w)`) uses that parameter's argument. Before, the
  default was pasted into the call, where it read a variable of the caller with that name, or
  failed when there was none.
* `val (x, y) = point` destructures a data class into its fields in constructor order, as
  Kotlin's `component1()`, `component2()` do (`let Point { x, y } = point;`). Before, rustc
  rejected the tuple pattern.
* A `when` guard that compares an arm's binding with a literal reads its value
  (`Click(x, y) if x < 0`), as predicates do, also when the subject is a reference. Before, it
  needed `*x`.
* `if (val (k, v) = split(s))` and `while (val (a, b) = pairs.pop())` unwrap an optional pair,
  as `if (val v = opt)` unwraps an optional value.
* `oxie fmt` no longer indents the line after a glob import (`import a::*`), lines up `else`
  with its `if` in an expression-bodied function, and keeps `T: ?Sized`, `x @ 1..=9`,
  `macro_rules! name`, `$x:expr`, `&{ .. }` and the spaces before trailing comments as written.
  It formats files whose doc comments end in spaces, which it used to leave unchanged.
* `oxie fmt` reports paths that don't exist, files that don't parse (with their errors) and
  unknown options, and exits with status 1. Before, it said "0 file(s) already formatted" or
  counted the file as formatted. Outside a project it needs paths, instead of formatting every
  `.oxi` file below the current directory.
* `x in set` works on a `HashSet` or `BTreeSet` of `String` with a `&str`, a `String` or a
  `&String` item, and `in` on a list of `String` with a `&str` item (or a list of `&str` with a
  `String`) compares the text. An item that already is a reference, like a loop variable over a
  borrowed list, isn't lent again. Before, these failed with `Borrow<&str>` or
  "expected `&String`, found `&&str`" errors.
* Predicates compare their parameter with a text literal on any list: `words.any { it == "bob" }`
  works on a `Vec<&str>`.
* Reading a map by index with an owned key or a number works: `ages[name]`,
  `mapOf(1 to "one")[1]`, `xs.group_by { it.len() }[2]`.
* A map index assignment on a field whose value calls a method that changes `this`
  (`ids[k] = alloc()`) computes that call first, like `items.push(next_id())`; so does a call
  after a conversion of a parameter (`ids.insert(name.to_string(), alloc())`).
* `sorted()` and `sorted_descending()` work on lists of `f64` (NaN sorts last, as in Kotlin).
* `x in a..b` no longer makes rustc warn about unneeded parentheses (with a broken suggested
  fix), and a `when` arm with several `in` conditions and a guard applies the guard to all of
  them. Before, the guard applied only to the last.
* Printing a value without a text form (`println(user)`) suggests `println("${user:?}")`.
* Assigning a plain value to a `T?` field from outside its class (`p.age = 5`) and in
  `copy(age = 7)` wraps it in `Some`, as in constructors.
* `?.` after a link that gives a plain value (`p?.display()?.len()`, `s?.trim()?.len()`,
  `n?.name?.len()`) works like Kotlin's. Before, it was a Rust type error, also when only another
  class had an optional field of that name. `x?.let { if (c) it.len() else null }` gives a
  `usize?`, not a `usize??`.
* `?.` chains ending in a std method that gives an optional (`s?.chars().next()`,
  `list?.first()`, `map?.get(k)`) give one optional, not a nested one.
* `a ?: b ?: c` with an optional `b` tries `a`, then `b`, then `c`; a call in the middle runs only
  when needed. Before, it was an error whose fix used `.expect`, which panics.
* `?.` on a method declared to return `&T?` and on `find_last` reads fields by reference. Before,
  it was "can't take `__oxie_v2.name` out of a borrowed value"; error messages no longer show the
  names Oxie generates.
* `x!!.len()` on a variable or field reads through the value instead of moving it out, so `x`
  stays usable (`email!!.split('@')` in a method, `o!!.len()` twice). `val e = o!!` still takes
  the value.
* A null check whose branch (or the rest of the block) doesn't use the variable stays a check
  (`b.is_some()`). Before, it warned about an unused variable, and the offered fix broke the code.
* Safe assignment: `books.get_mut(isbn)?.available = true` and `o?.inner?.n = 2` assign only when
  the values aren't null. Before, it was "invalid left-hand side of assignment".
* A method called on a `T?` that rustc calls private (`best.len()`) is explained as an optional
  value, and rustc's `?` fix, which would read as `?.`, is no longer offered.
* Null checks unwrap in more places, as Kotlin's smart casts do: the `else` branch of
  `if (x == null)`, after `if (a == null || b == null) return`, and the right side of
  `x == null || x.len() > 3` and `x != null && x.len() < 3`.
* Values put into a `Vec` or map of interface values (`push`, `insert`), assigned to an
  interface-typed field of another value, or passed in `&[..]` to a `&[Shape]` parameter are
  boxed, including values of implementing classes and enums and interface parameters (which
  become `impl Shape + 'static`). Before, most of these failed with "expected `Box<dyn Shape>`".
* Lambdas and function parameters pushed onto a `Vec<(A) -> B>` or assigned to a function-typed
  field (also from outside the class) are boxed; a stored function parameter is
  `impl Fn(..) + 'static`. `(field)()` works inside classes, and the `Box.new` fix around a
  lambda is whole (it put `)` inside the braces).
* An interface method whose parameter or result is an interface (`fun next(c: char): State`)
  takes and returns `Box<dyn State>`, so implementations can return different classes. Before it
  was `impl State`, which Rust doesn't allow there.
* A boxed interface value reached any way (`shapes.first()`, `map.get(k)`, `it` in
  `shapes.iter().for_each { describe(it) }`) is accepted for `Shape` and `&Shape` parameters:
  Oxie implements each interface for `Box<T>` and `&T`. Before: "`&Box<dyn Shape>: Shape` is not
  satisfied".
* Extension functions on an interface (`fun Shape.describe()`) apply to every implementing type;
  before they generated an invalid `impl Ext for Shape`.
* Type names in all capitals (`class DB`, `enum HTTP`, `class V8`) are types: `DB.open()`,
  `HTTP.GET`, `V2 { x: 1 }` and boxing `V8()` into an interface work. Before, they were taken for
  constants. `oxie from-rust` keeps such code as Oxie instead of `rust { }`.
* A bare case name in a pattern (`Circle(r)`, `Number(n)`) is a case of the matched value's enum,
  even when a class or another enum has the same name. `is Int` and other names that match
  nothing are reported instead of silently matching everything.
* Constants of a class, enum or companion object can be used unqualified inside it
  (`w * SIZE`). A `var` in an `object`, a companion object or at the top level gets a clear error.
* `@from` works on cases wrapping any type, not only errors. `main() throws` shows each cause
  once. Error cases without a message show their fields (`too many (count: 5, max: 3)`) instead
  of warning that the fields are never read.
* Generic classes and enums get the bounds their `toString`, operators and overrides need
  (`impl<T> Display for P<T> where T: Display`), and the bounds that cloning, printing,
  searching or sorting a `Vec<T>` field and using a `HashMap<K, V>` field need. An override
  without a result type in `class Mem : Repo<String>` returns `Vec<String>`.
* A method returning part of a reference field (`class Parser(input: &str)`,
  `fun next_word(): &str = &input[s..pos]`) returns it with the field's lifetime, so callers can
  keep the result while using the object.
* `when (state)` on a field inside a method borrows the field when an arm takes a value out of
  it (`Done(r) -> Done(r.clone())`), as `for` does.
* A `shared class` implementing an interface no longer makes its methods `&mut self`: the handle
  changes its data behind `&self`, so `fun fill(s: &Store) = s.put(..)` works.
* An enum's `operator fun` is emitted once (no more "method `plus` is never used" warning).
* Kotlin habits get Oxie errors instead of confusing rustc ones: extending a class
  (`: Base(name)`), properties in interfaces, `override` without an interface, overrides using a
  class's contexts, and top-level or object `val`s that allocate (`vec[...]`, `Point(0, 0)`).
  `open class` warns. `enum class` and `interface Marker` without braces are accepted.
* Char literals and nested strings containing `"`, `{` or `}` work inside `${...}`
  (`"${s.trim_matches('{')}"`), and `${x?:0}` is an Elvis expression, not a format spec.
  `oxie fmt` handles them too; before, it stopped formatting at such a template.
* A trailing lambda goes to the last parameter, past parameters with defaults:
  `with_opts(1) { it * 2 }`. Before: "missing argument".
* An `if` without `else` in a `when` arm no longer takes the next `else ->` arm as its else.
* A `///` doc comment before a local `val`/`var` is a plain comment. Before, it turned the local
  into a `const`, or failed.
* Classes, data classes and enums declared inside function bodies get constructors, inferred
  receivers and automatic derives.
* `String()`, `String.with_capacity(n)` and `format(..)` are known to be `String`s, so `s += a`
  and `when (format(..)) { "ab" -> }` work; `var acc = ""` that is later appended to starts as a
  `String`.
* Text arms in `when` work on `&String` loop variables (`for (s in &list)`) and tuple fields
  (`t.0`), and `Some("hi") ->` arms on a `String?` match with `as_deref()`.
* Lambdas passed where an alias of a function type is expected
  (`typealias Handler = (i32) -> String`) are boxed. Error notes say `{lambda}` instead of a
  generated file's path.
* `x in a downTo b` is checked as `b <= x && x <= a`.
* Files with Windows line endings: multi-line strings and `\` line continuations compile, and
  `oxie fmt` keeps CRLF line endings.
* `var (x, y) = ...` makes only the parts that change mutable, and `val (tx, var rx) = ...`
  declares only `rx` changeable. The warning for a part of a `var` destructuring that never
  changes offers that form; before, it described a `var` per part, which didn't parse.
* Kotlin's `Int.MAX`, `Long.MAX_VALUE`, `Double.POSITIVE_INFINITY` work, as do `1.5f`, `\b`,
  labels written `outer@ for` / `break@outer`, `do x += 1 while (c)` without braces, and typed
  destructuring `val (a: Int, b: String) = ...`. `n++` and `return@label` get Oxie errors that
  say what to write.
* `println("total:", n)` prints `total: 3` when the text has no `{}`, and `name + n` with a
  `String` and a number or `char` builds a `String`.
* A non-exhaustive `when` says which cases it doesn't handle and suggests `else ->`; `xs.size`
  and `s.length` point at the field and offer `len()`; Kotlin's `joinToString` maps to
  `join_to_string`.
* A loop that only reads a parameter's elements (`for (x in xs)`, `for (t in list.items)`)
  borrows it, so callers keep their value; before, the loop kept the parameter owned and the
  caller's value was moved. A loop over a field of a reference, or of a variable used after the
  loop, borrows the field instead of failing to move it out.
* A loop whose body changes its elements through a method that takes `&mut self`
  (`for (c in counters) c.bump()`), `&mut x.f`, `*v` of a map entry or an inner loop goes over the
  list mutably; before, it failed with "`c` is a `&` reference".
* A method that returns `this` (a builder: `fun url(u: &str): B { url = u.to_string(); this }`)
  takes `self`, or `mut self` when it changes it; before, it failed with "expected `B`, found
  `&mut B`".
* Variables built empty or with type arguments (`var a = vec[]`, `mutableListOf<i32>()`,
  `HashMap<String, i32>()`, `Stack<i32>()`) have a known type: they are lent `&mut` to functions,
  `m[k] = v` inserts, and calls of their methods fill in defaults, named arguments and contexts.
* `String` parameters that a function only joins with `+` (`fun label(a: String, b: String) =
  a + b`) or passes on as `&s` are lent as `&str`, as the docs say.
* A lambda stored in a `val` that changes a captured variable through a method
  (`{ s: &str -> log.push(s.to_string()) }`) can be called; before, it failed with E0596.
* Extension functions that change their receiver take `&mut self`
  (`fun Temp.warm(by: f64) { c += by }`) without `mut fun`.
* Changing a parameter (`fun fill(c: Cart) { c.add("x") }`) offers `c: &mut Cart`. A function
  that stores or returns its parameter no longer gets a fix that would borrow it (which didn't
  compile); the error says where it keeps it.
* A `&mut [T]` parameter that its function grows (`items.push(x)`, `insert`, `clear`, ...), or
  passes on to a function that does, is a `&mut Vec<T>`, as a grown `var xs = [..]` is a list.
  Before, the `items: &mut [T]` that the fix above offers for a list parameter failed with "no
  method named `push`".
* A class's body properties can be computed from its constructor parameters and from earlier
  properties (`val code = name.to_uppercase()`, `var first = items[0]`). Before, a parameter
  read there was already moved into its field, and earlier properties weren't visible.

* `oxie new` checks the package name as cargo does and rejects `.` (use `oxie init`); before,
  `oxie new .` panicked and `oxie new 123` made a package cargo rejects. Unknown options are
  errors that name them (`oxie run --relase`: did you mean `--release`?), and extra arguments
  are reported; before, they were ignored. `oxie test` takes `-r`.
* A path dependency that doesn't compile shows its errors; before, only cargo's "could not
  compile" was printed.
* `oxie test` shows each warning once, checks a script for Oxie errors first as `oxie run` does,
  matches a test name in any case (`oxie test Client`), and no longer prints cargo's hint with
  the internal name of a script's crate. Warnings about an item point at its name instead of its
  doc comment, and `oxie new --lib` writes a `pub fun`.
* Errors point at the part of the line they are about (the field in `p.nmae`, `.collect()` in a
  multi-line chain, the type in `catch (e: Problem)`, the interface after a class name, an
  unused import's name), errors inside `rust { }` blocks at their line in the block, and a
  missing `}` names the `{` it leaves open. A `rust { }` statement no longer gets a second `;`.
* A module file or directory name that isn't an identifier (`my-util.oxi`) and a script without
  `fun main` are Oxie errors that say what to do, instead of rustc errors in generated files.
* Printing a value without a text form is explained for `println(x)` too and for classes with
  short names; `write!` into a `String` offers to import `std::fmt::Write`; cargo's summary
  counts only the warnings shown; a hand-written `.rs` module's errors use relative paths; and a
  virtual workspace's root says to run `oxie` in a member.
* The language server keeps running after a message it can't read, publishes diagnostics of
  unsaved (`untitled:`) documents, and completes and hovers locals by the type of their value
  (`val c = Counter()`: `c.` lists `Counter`'s members).
* `oxie from-rust` on a directory drops the `mod` lines of modules it translated in the same run,
  reports its TODOs on stderr, and explains input that doesn't divide into tokens.

## 0.3.0 - 2026-10-25

Oxie 0.3.0 makes code look less like Rust. Lists and maps have short types, `[T]` and `[K: V]`,
whose literals take the declared type, and methods borrow the parameters they only read, so
`&str`, `&[T]` and `&T` mostly leave parameter lists. Context parameters declare GPUI's `window`
and `cx` once, `oxie from-rust` translates whole crates into the new forms, and null checks
unwrap optional variables.

### Upgrading

* Set `oxie-build = "0.3.0"` in the `[build-dependencies]` of each project (`cargo update`
  doesn't move from 0.2 to 0.3). `oxie` warns when a project builds with another `oxie-build`.
* Nothing that compiled stops compiling for the new syntax: `Vec<T>`, `HashMap<K, V>`,
  `vec[...]` and `mapOf(...)` keep working, and `[T]` used to be an error everywhere but behind
  `&`, `Box`, `Rc`, `Arc` and in a trait's arguments, where it is still a slice.
* More parameters are borrowed: those of methods, maps, sets and `PathBuf`s, and in a program
  (`main.oxi` without `lib.oxi`) those of `pub` functions and methods. Oxie lends the arguments
  of the calls it compiles; Rust code in `rust { }` blocks and macro arguments keeps calling
  them with the declared types. Hand-written Rust in the same crate (a Rust crate with Oxie
  modules) that calls an Oxie function that isn't `pub` may need `&` where that function only
  reads its parameter, as in 0.2.

### Changes

* Lists and maps have short types: `[T]` is a list (Rust's `Vec<T>`) and `[K: V]` a map (std's
  `HashMap<K, V>`), so `var log: [String] = []` and `val ages: [String: i32] = ["ada": 36]`
  replace `Vec<String>` and `HashMap<String, i32>`. After `&`, in `Box`, `Rc` and `Arc` and in a
  trait's arguments `[T]` is still Rust's slice (`&[T]`, `impl AsRef<[u8]>`). A list or map literal
  takes the declared type, element by element (`["a", "b"]` for a `[String]` makes `String`s),
  also for a `HashSet`, `BTreeMap` or `VecDeque`; `[:]` is an empty map, a `var` array that the
  code after it grows (`xs.push(3)`) is a list, and `iter.to_list()` collects an iterator.
  `Vec<T>`, `HashMap<K, V>`, `vec[...]` and `mapOf(...)` keep working.
* Methods borrow the parameters they only read, as functions did: `fun has(name: String)` takes
  `&str`, `fun total(items: [Item])` takes `&[Item]`, and callers keep their values. A method
  keeps its parameters' types when Oxie can't resolve every call to it (a receiver of unknown
  type, `?.`, a method used as a value, Rust code calling it), and when it implements an
  interface method. Maps and sets (`&HashMap<K, V>`) and `PathBuf` (`&Path`) parameters are
  borrowed too, and in a program (`main.oxi` without `lib.oxi`) so are `pub` functions and
  methods; a library's keep the types they declare.
* A parameter stored through a field written without its value (`Point { x, y }`) or named in a
  `rust { }` block is no longer borrowed. Before, the generated Rust didn't compile.
* The language server shows lists and maps as written, `[String]` and `[String: i32]`, in hovers
  and symbol details (`&Vec<T>` and `Box<[T]>` stay as they are), and completes `to_list()`. An
  error about a list or map parameter that a function keeps says why instead of offering to
  borrow it.
* A method call whose result Oxie knows is a value (`describe(Point.origin())`) is lent to a `&T`
  parameter, as function calls already were.
* `oxie from-rust` writes `[T]` and `[K: V]` (for std's `HashMap`), `[a, b]` and `[]` for a
  declared list, `[k: v]` and `[:]` for maps, and `.to_list()` for `.collect::<Vec<_>>()`.
  `Vec<T>` stays where Oxie reads `[T]` as a slice (`&mut Vec<T>`, `Arc<Vec<T>>`), and an item with
  a slice type Oxie can't write (`type Bytes = [u8]`) stays Rust.
* `oxie new` and `oxie init` also write a `CLAUDE.md` with the line `@AGENTS.md`, so Claude Code
  reads the notes for agents that the other agents read in `AGENTS.md`. A crate's own `CLAUDE.md`
  is kept. The documentation has a page on [AI coding agents](docs/ai-agents.md): setting up
  Claude Code, Codex, Cursor and other agents, the commands they use, and what the compiler tells
  them.
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
* `: Default` on a class with default values builds what its constructor builds:
  `class Settings(width: i32 = 80) : Default` gives a width of 80. Before, it derived `Default`,
  which ignored the declared defaults and gave 0.
* A `&str` variable passed where a `String` or `String?` is declared (a parameter, a field of a
  constructor or an enum case) is converted, as a literal is. Before, only literals and method
  results were, and constructors and enum cases rejected it.
* The arguments of an enum case with positional fields (`Plain(String)`) convert like those of
  one with named fields: `Plain("text")` and `Wrapped(5)` for a `Wrapped(i32?)` compile.
* `sum_of { it }`, `max_of { it }` and `min_of { it }` work on the elements' values; `sum_of`
  failed to compile, since `it` is a reference.
* rustc's advice to add `#[derive(Default)]` reads as Oxie: `class Stats(...) : Default`.
* An `if` or `when` value whose branches mix text literals and templates is a `String`:
  `val s = if (n == 0) "none" else "$n items"` compiles. Before, rustc rejected the branches'
  different types unless the variable declared `String`.
* A lambda passed where a future is expected (`tokio.spawn { ... }`) is explained, and the error
  suggests an async block, instead of rustc's advice to call the closure.
* Storing a predicate's parameter where it outlives the lambda (`filter { seen.insert(it) }`) is
  explained: `it` is a reference to the element, so store `*it` or `it.clone()`.
* `throw` in a function that doesn't declare `throws` is an Oxie error that says to declare it,
  instead of rustc's "expected `()`, found `Result`".
* `?` inside a lambda is explained (it leaves the lambda, not the function), instead of advice to
  add `throws` to a function that already has it. A throwing call's result used in arithmetic
  (`value(x) + 1`) is explained too, with `?` as the fix.

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
