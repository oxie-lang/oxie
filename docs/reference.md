# Oxie language reference

Oxie is a programming language that compiles to Rust. Every construct below lowers to ordinary
Rust, and the semantics (ownership, borrowing, traits, generics, `Option`/`Result`, iterators,
macros) are Rust's.
`oxie emit file.oxi` prints the generated Rust for any file.

Contents: [Lexical structure](#lexical-structure) · [Declarations](#declarations) ·
[Functions](#functions) · [Types](#types) · [Classes](#classes) · [Enums](#enums) ·
[Interfaces](#interfaces-and-impl-blocks) · [Extensions and operators](#extension-functions) ·
[Statements](#statements) · [Expressions](#expressions) · [Strings](#strings) ·
[Null safety](#null-safety) · [Errors](#errors) · [Control flow](#control-flow) ·
[Lambdas](#lambdas) · [Collections](#collections) · [Modules](#modules-and-imports) ·
[Visibility](#visibility) · [Attributes](#attributes) · [Tests](#tests) · [Async](#async) ·
[Rust escape hatches](#rust-escape-hatches) · [Keywords](#keywords)

---

## Lexical structure

* **Comments**: `// line`, `/* block (nestable) */`, `/// doc comment` (becomes a Rust doc
  comment), `//! module doc`.
* **Statements end at a newline.** `;` may separate statements on one line. A line continues
  when it ends with an operator, `,`, `.`, `(`, `[` or `{`, when the next line starts with
  `.`, `?.`, `?:`, `&&` or `||`, and everywhere inside `(...)` and `[...]`:

  ```oxie
  val total = items
      .iter()
      .map { it.price }
      .sum::<f64>()
  ```

* **Identifiers** are Rust identifiers. Rust keywords that Oxie does not reserve (`type`,
  `match`, `loop`, ...) can be field and method names; `r#name` works as in Rust.
* **Numbers**: Rust literals (`1_000`, `0xff`, `1e-3`, `8.`, `5u8`, `2.0f32`) plus Kotlin's `10L` (`i64`).
* **Characters**: `'a'`, `'\n'`, `'\u{1F600}'`, byte `b'a'`.
* **Strings**: see [Strings](#strings). Raw strings `r"..."`, `r#"..."#` and byte strings
  `b"..."` are Rust's.

## Declarations

| Oxie | Rust |
|---|---|
| `val x = 1` | `let x = 1;` |
| `var x = 1` | `let mut x = 1;` |
| `val x: T = v` | `let x: T = v;` |
| `val (a, b) = pair` | `let (a, b) = pair;` |
| `var (a, b) = pair` | `let (mut a, mut b) = pair;` |
| `let`/`let mut` (Rust style) | unchanged |
| `val Some(x) = opt else { return }` | `let Some(x) = opt else { return };` |
| top-level `const NAME = 1` / `val NAME = 1` | `const NAME: i32 = 1;` (type inferred from literals) |
| `static NAME: T = v` / `static var NAME: T = v` | `static` / `static mut` |
| `typealias Name = T` / `type Name = T` | `type Name = T;` |

## Functions

```oxie
fun name<T: Bound>(param: Type, other: Type = default): ReturnType throws Error where T: Other {
    body
}
fun short(x: i32) = x * 2
```

* **Return type**: `: T` (or Rust's `-> T`). Omitted means `()`, except for an expression
  body whose type is obvious (a literal, a template, arithmetic on typed values, `Some(x)`, a
  call to a function with a declared type, a constructor, ...), which Oxie infers. An
  expression body whose type Oxie can't tell must give `()` (`fun add(x: i32) = items.push(x)`);
  one that gives a value is an error that offers its type, never a value silently dropped.
* **Block value**: the last expression of a block is its value. In a function returning `()`
  the last expression is a statement.
* **Default arguments** and **named arguments** (`f(x = 1)`) work for functions, methods and
  constructors declared in Oxie; defaults are filled in at the call site. A function called
  through its module (`util.total(v)`) is treated like a direct call: arguments are lent,
  defaults and names apply, and a `throws` function's error propagates.
* **Generics and bounds** are Rust's: `<T: Display + Clone>`, `where T: Into<String>`,
  `<'a>`, `<const N: usize>`. Kotlin's `fun <T> name()` order is accepted too.
* **Inferred bounds**: bounds a function body needs are added to the generated signature
  (methods of generic classes get them as a `where` clause on the method):

  | use of a `T` value (or an element of `&[T]`, `Vec<T>`) | bound added |
  |---|---|
  | `"$x"`, `println(x)`, `x.to_string()` | `Display` |
  | `"${x:?}"` | `Debug` |
  | `a < b`, `a.partial_cmp(b)` / `a == b`, `x in xs` | `PartialOrd` / `PartialEq` |
  | `a.cmp(b)`, `a.max(b)` | `Ord` |
  | `a + b`, `a * b`, ... with both `T` | `Add<Output = T> + Copy`, ... |
  | `a += b` | `AddAssign`, ... |
  | `x.clone()` | `Clone` |
  | a key of a `HashSet`/`HashMap` variable | `Eq + Hash` |

  Bounds already written (or implied, like `Ord` for `PartialOrd`) are not repeated.
  Interface methods keep the interface's signature.
* **Automatic borrowing**: an argument for a `&T` or `&mut T` parameter of a function or method
  declared in Oxie is borrowed: `total(v)` → `total(&v)`, `add_one(v)` → `add_one(&mut v)`.
  For a `&str` or `&[T]` parameter any owned argument is lent (`greet(name.to_uppercase())`).
* **Borrowed parameters**: a `String` or `Vec<T>` parameter of a top-level function that isn't
  `pub` becomes `&str` or `&[T]` when the body only reads it (calls `len`, `contains`, `iter`,
  `map`, ..., prints it, compares it, indexes it, or passes it on to another borrowed parameter),
  so callers keep their value. A function that stores, returns or consumes the value keeps the
  owned type, and so do functions that are `pub`, `async`, used as values (`xs.map(greet)`) or
  called through a path (`util.greet(x)`). A `Vec<T>` is lent only when dropping a `T` just
  frees memory (numbers, strings, std collections of them, and Oxie types made of those that
  don't implement `Drop`). A `Vec` of mutex guards, files, `Rc`s or a type parameter stays
  owned, so the function drops it when it returns, as in Rust: a guard unlocks there, not at
  the end of the caller.
  A parameter of an Oxie class or `Box<T>` becomes `&T` the same way when the body only reads
  it: calls its methods that take `&self` (or `clone`, `to_string`, `copy(...)`), formats it, or
  reads its fields (`u.name.len()`, `u.age > 18`, `"${u.address.city}"`, a `Copy` field
  anywhere). Comparing the value itself (`u == other`), taking a field out (`val n = u.name`),
  changing it or calling a method that does keeps it owned. So do `Copy` classes (a copy is
  passed) and `shared` classes (a handle is), and functions whose result is a reference. The drop
  rule of `Vec<T>` applies to the whole value.
  Arguments that already are references (`&v`, reference-typed variables, `self`, string
  literals) are passed as they are. Passing a field to a `&mut` parameter makes the method
  `&mut self`.
* **Arguments that change `this`**: in a method, a call on a field whose arguments call a
  method of `this` that takes `&mut self` (`items.push(next_id())`) computes those calls first,
  into locals named after the method, in the order they are written:
  `let next_id = self.next_id(); self.items.push(next_id);`. Rust borrows the field for the call
  before it runs the arguments, so it rejects the statement as written. This applies to any
  statement (also a `val`'s initializer, a `when` arm or a lambda's body) when the call borrows
  the field (a std collection, `String`, `Box`, a `shared class`, or an Oxie class whose method
  takes `&self` or `&mut self`) and the calls always run (not in a lambda, an `if`, after `&&`
  or `?.`). Nothing that could notice them running earlier may run before them, like another
  call or a read of a field, so `items.insert(count(), next_id())` stays as written. A `Copy`
  field is copied before the arguments run, so `next.min(next_id())`, which Rust accepts, is
  left as written too.
* **Methods that take only the fields they use**: a method that changes `this`, called inside a
  loop over a field (`for (item in items) { note(item) }`) or a lambda passed to a method of one
  (`items.iter().for_each { note(it) }`), takes the fields it uses instead of `this`:
  `fn note(item: &str, count: &mut i32)`, called as `Self::note(&item, &mut self.count)`. Rust
  rejects `self.note(item)` there, since the loop borrows `items` and the call all of `self`;
  borrows of different fields, or two reads of one, are allowed. A field the method changes is
  passed as `&mut`, a `Copy` field it only reads by value, and another field it only reads as
  `&T` (`&str` or `&[T]` when that is enough). Only for a method that uses at most three fields
  and doesn't change the one the loop goes over (it may read it, unless the loop changes it:
  then the conflict is real, and stays an error), that doesn't use `this` itself or call other
  methods, whose result borrows nothing, that has a block body or a declared result type, and
  that isn't `pub` and is called only from its own class (another value's call, `other.note()`,
  keeps it a method). Its other calls pass the fields too.
* **Modifiers**: `async fun`, `unsafe fun`, `const fun`, `extern "C" fun`.
* **Automatic lifetimes**: when a function returns a reference and Rust cannot infer which
  parameter it borrows from, Oxie ties the result to all reference parameters
  (`fun longest(a: &str, b: &str): &str` → `fn longest<'a>(a: &'a str, b: &'a str) -> &'a str`).
  With no reference parameters the result is `'static`. Types with lifetime parameters get
  `<'_>` in signatures, and count as references here (`fun first(p: &Parser): Token` →
  `fn first<'a>(p: &'a Parser<'a>) -> Token<'a>`). A reference inside an `impl Trait`
  parameter, which Rust doesn't let you leave unnamed, gets a lifetime of its own
  (`patches: impl Iterator<Item = &Patch>` → `fn total<'a>(patches: impl Iterator<Item = &'a Patch>)`),
  or the result's when the function returns a reference.
* **Function-typed parameters**: `f: (i32) -> i32` becomes `impl FnMut(i32) -> i32` (or
  `impl Fn` when the function is called from inside a closure); as a field, a return value
  stored in a variable, or a generic argument it becomes `Box<dyn Fn(i32) -> i32>`, and
  lambdas passed to it are boxed automatically.
* `fun main() throws` prints the error with its causes and exits with status 1.

### Context parameters

```oxie
class Log(var lines: Vec<String> = vec[]) {
    fun add(line: String) = lines.push(line)
}

// every call passes the `log` where it is made
context(log: &mut Log)
fun record(line: &str) = log.add(line.to_string())

// each method takes the contexts it uses
context(log: &mut Log)
class Cart(var items: Vec<String> = vec[]) {
    fun add(item: &str) {
        items.push(item.to_string())
        record("added $item")              // passes its own `log`
    }
    fun count(): usize = items.len()       // takes none
}

fun main() {
    var log = Log()
    var cart = Cart()
    cart.add("pen")                        // cart.add("pen", &mut log)
    record("${cart.count()} item")
    println(log.lines.join(", "))
}
```

* `context(name: Type, ...)` before `fun` declares parameters that calls don't write: each call
  passes the variable of that name where it is made, which can be a parameter, a local
  (borrowed like any argument: `&mut log`), a lambda's parameter or the calling function's own
  context. In the generated Rust they come after the other parameters, where GPUI passes its
  `window` and `cx`: `fn record(line: &str, log: &mut Log)`.
* A call can pass another value by name (`record("x", log = &mut other)`), or positionally after
  the other arguments.
* Before `class`, `context(...)` gives each method the contexts it uses, directly or through the
  methods and functions it calls (inferred like receivers): `add` takes `log` because it calls
  `record`, and `count` takes nothing. Static functions and the methods in the class's `impl`
  blocks, in any file, take theirs the same way. A method with a `context(...)` of its own takes
  exactly those instead.
* A method used as a value (`cx.listener(Self.save)`) takes every context of its class, in order,
  since its caller passes every argument; one it doesn't use is named with a `_` (`_window`).
* Like its receiver, a method's contexts follow its body, `pub` methods' too. To keep a `pub`
  method's signature fixed whatever its body uses, give it a `context(...)` of its own.
* A lambda's parameter hides a context of the same name, like any variable: in
  `entity.update(cx) { e: &mut Editor, cx -> e.save() }`, `save` gets the lambda's `cx`.
* Contexts are passed only in calls that Oxie resolves to a function declared in Oxie, like named
  and default arguments. A method called on a value whose type Oxie doesn't know, such as an
  untyped lambda parameter, or called after `?.`, takes them as written: `e.save(cx)`.
* A call with no variable of a context's name in reach is an Oxie error, which suggests declaring
  the context on the calling function. Interface methods, overrides, operators and `main` can't
  take contexts, since their parameters are fixed.

## Types

| Oxie | Rust |
|---|---|
| `i32`, `u8`, `f64`, `bool`, `char`, `str`, `String`, ... | same |
| `Int`, `Long`, `Short`, `Byte`, `UInt`, `ULong`, `Double`, `Float`, `Boolean`, `Char`, `Unit` | `i32`, `i64`, `i16`, `i8`, `u32`, `u64`, `f64`, `f32`, `bool`, `char`, `()` |
| `T?` | `Option<T>` (`&T?` is `Option<&T>`) |
| `(A, B) -> R` | `impl FnMut(A, B) -> R`, `Box<dyn Fn(A, B) -> R>` or `dyn Fn(..)` depending on position |
| `Shape` (an interface used as a type) | `impl Shape` (parameters, returns), `dyn Shape` (behind `&`, `Box`, `Rc`, `Arc`), `Box<dyn Shape>` (fields, collections, and returns of different classes) |
| `&T`, `&mut T`, `[T]`, `[T; N]`, `(A, B)`, `Vec<T>`, `dyn T`, `impl T`, `*const T` | same |
| `std.io.Error` | `std::io::Error` (dots or `::` in paths) |

A function that returns an interface returns its value unboxed (`impl Shape`), unless its
branches build values of different classes: then it returns `Box<dyn Shape>`, and each value is
boxed.

```oxie
interface Shape { fun area(): f64 }
class Square(side: f64) : Shape { override fun area() = side * side }
class Circle(r: f64) : Shape { override fun area() = 3.14 * r * r }

fun unit(): Shape = Square(1.0)                                   // -> impl Shape
fun pick(round: bool): Shape = if (round) Circle(1.0) else Square(2.0)   // -> Box<dyn Shape>

fun main() = println("${unit().area()} ${pick(true).area()} ${pick(false).area()}")
```

An element of a `Vec<Shape>` (or `Vec<Box<Shape>>`), a loop variable over one and an
interface-typed field are lent to an `&Shape` parameter as the value inside the box
(`describe(shapes[0])` → `describe(&*shapes[0])`).

The standard traits used as types the same way are `Display`, `Debug`, `Error`, `Iterator`,
`IntoIterator`, `Read`, `Write`, `BufRead`, `Future`, `Any`, `ToString` and every interface
declared in Oxie.

## Classes

```oxie
class Name<T>(val a: A, var b: B = default, private c: C) : Interface, Hash {
    var d: D = default            // body property (needs a default)
    init { ... }                  // runs in the constructor after fields are set

    fun read(): A = a             // &self
    fun write(x: B) { b = x }     // &mut self (inferred)
    static fun create(): Name<T>  // no self
    companion object { fun other(): Name<T> = ... }   // also static
    override fun method() { ... } // implements `Interface`
}
```

* Generates `struct Name<T> { a, b, c, d }` and `impl` with `fn new(a, b, c) -> Self`.
* **Construction**: `Name(a, b)` → `Name::new(a, b)`; named arguments and defaults work.
  Zero-argument calls of external types (`HashMap()`, `String()`) become `Type::new()`.
* `val`/`var` before constructor parameters are accepted (Kotlin habit) but don't matter:
  mutability belongs to the variable holding the object, as in Rust.
* **Implicit `this`**: inside methods, fields and methods of the class (and default methods of
  its interfaces) can be used without `this.`; a parameter or local with the same name wins.
  `this` and `self` are the same. A method hides a function of the module with its name: call
  that function as `self::name(x)` (Rust's path to the module's own items).
* **Receivers** are inferred:
  * `&mut self` if the method assigns a field, calls a mutating method on a field (`push`,
    `insert`, `clear`, `entry`, ...), takes `&mut` of a field, calls a `&mut self` method, or
    calls a `&mut self` method of an Oxie class through a field of that type (`c.add(1)` for
    `var c: Counter`, also generic classes like `Entity<Counter>`; not a `shared class`, whose
    handle `&self` reaches);
  * static if it never uses `self` and returns `Self`/the class, or is named `new`, `default`,
    `from`, `from_str`, `try_from` or `from_iter`;
  * `&self` otherwise.
  Override with `mut fun`, `static fun`, or an explicit `self`, `&self`, `&mut self`,
  `mut self` first parameter. Interface methods take the receiver their implementations need.
* **`data class`** additionally derives `Debug`, `Clone`, `PartialEq`, and `Copy`, `Eq`,
  `Hash` when every field type supports them, and supports `x.copy(field = value)`.
* **Supertypes** after `:` are traits to implement. Derivable traits (`Debug`, `Clone`,
  `Copy`, `PartialEq`, `Eq`, `Hash`, `Default`, `PartialOrd`, `Ord`) without methods in the
  body become derives. `override fun toString() = "..."` implements `Display`. A class with
  default values that lists `Default` gets an `impl Default` that builds what the constructor
  builds (`Settings()`, with each default; parameters without one take their type's default),
  not a derive of zeros.
* `@derive(...)` adds derives; `@no_auto_derive` turns the automatic ones off.
* **Reference fields**: a class, struct or enum whose fields hold references without a lifetime
  (`&str`, `Vec<&T>`, `&str?`, or another such type) gets one: `class Parser(input: &str)` →
  `struct Parser<'a> { input: &'a str }`. Write the type as `Parser`; signatures spell it
  `Parser<'_>`. Declaring lifetimes yourself (`class Pair<'a, 'b>`) turns this off.
* Rust-style `struct Point { x: f64, y: f64 }`, tuple structs `struct Meters(f64)` and unit
  structs `struct Marker` are supported. A type without fields is also its own value, so
  `Marker.describe()` calls a method on it (static functions are still `Marker.create()`).
* `union Bits { i: u32, f: f32 }` is a Rust union: built with `Bits { f: 1.0 }`, read in `unsafe`.
* `shared class` — see [Shared classes](#shared-classes).
* `context(cx: &mut App) class ...` gives each method the contexts it uses — see
  [Context parameters](#context-parameters).
* `object Name { ... }` is a namespace (a Rust module) of constants and functions:
  `Name.function()`.
* Classes, enums and interfaces declared inside a class body are placed next to the class;
  refer to them as `Inner` or `Outer.Inner`.

### Shared classes

```oxie
shared class Name(var a: A) : Interface { ... }   // Rc<RefCell<..>> handle
@sync shared class Name(var a: A) { ... }         // Arc<Mutex<..>>, usable across threads
```

* The class is generated as `NameData` (the struct, constructor and methods as for a `class`)
  and `Name`, a handle: `struct Name(Rc<RefCell<NameData>>)`.
* The handle has the same constructor, one method per instance method that borrows the data
  for the call (`self.0.write().deposit(n)`), a getter `a()` and setter `set_a(v)` per
  field, and implements the class's Oxie interfaces and `toString` (`Display`) by forwarding.
* Static methods and `companion object` functions are on the handle.
* `Clone` copies the handle; `==` and `Hash` use identity (`Rc::ptr_eq`).
* Using a shared variable or field by value copies the handle (`val b = a` → `a.clone()`), so
  the original stays usable.
* From outside the class: `a.f` reads through the getter (a copy of the field), `a.f = v` uses
  the setter, `a.f += v`, `a.f[i] = v` and mutating calls like `a.f.push(x)` borrow the object
  mutably for that statement (arguments are evaluated first), and other chains (`a.f.len()`,
  `a.f[0]`) borrow it for reading.
* Methods returning references into the object can't be forwarded (the borrow ends with the
  call); return owned values.
* Inside its methods, fields and methods are reached on the data, and `this` used as a value
  (passed, stored or returned) is the handle: such a method, and one that calls it, gets the
  handle as a last parameter `__this`, and `this` is `__this.clone()`.
* A method that reaches its own object again through another handle while it runs (a parameter
  that is the same object, a callback that reads it) panics at that line with an explanation.
  An `@sync` object panics the same way instead of waiting for its own lock forever.

```oxie
shared class Node(name: String, var children: Vec<Node> = vec[], var parent: Node? = null) {
    fun add(child: Node) {
        child.parent = this          // the handle
        children.push(child)
    }
}

fun main() {
    val root = Node("root")
    val leaf = Node("leaf")
    root.add(leaf.clone())
    println("${leaf.parent?.name ?: "-"} has ${root.children.len()} child")
}
```

## Enums

```oxie
enum Shape : Display {
    Circle(radius: f64)          // named fields → Circle { radius: f64 }
    Pair(i32, i32)               // positional fields
    Empty                        // unit variant
    Coded = 3                    // explicit discriminant (unit-only enums)

    fun area(): f64 = when (this) { Circle(r) -> 3.14 * r * r; else -> 0.0 }
}
```

* Variants are separated by newlines or commas; methods follow in the same body.
* `Shape.Circle(1.0)` or `Circle(1.0)` (when the name is unique, or declared in the same file)
  constructs; named-field variants accept positional or named arguments.
* In patterns, `Circle(r)` matches named-field variants positionally (→ `Circle { radius: r }`),
  and a bare `Circle` matches any `Circle`.
* Enums derive `Debug`, `Clone`, `PartialEq` (and `Copy`, `Eq`, `Hash` when possible).
* `error enum` — see [Errors](#errors).

## Interfaces and impl blocks

```oxie
interface Named : Display {
    fun name(): String                 // required
    fun greet(): String = "hi ${name()}" // default
    type Id                            // associated type
    type Item<'a> where Self: 'a       // generic associated type
    const MAX: u32 = 10                // associated constant
    fun borrow<'a>(&'a self): &'a str  // explicit receiver lifetime
}
```

* `interface` (or `trait`) → `trait`. Methods take `&self` unless declared otherwise (or their
  default body or an implementation needs `&mut self`). Its supertypes may include a lifetime
  bound: `interface Ext : IntoElement + 'static`.
* A class lists interfaces after `:` and implements methods with `override fun`; the
  matching `impl Trait for Class` is generated. Methods named like the trait's (and `type`
  members) go to the right trait even without `override`.
* Rust `impl Type { }` and `impl Trait for Type { }` blocks are supported, also for types not
  declared in Oxie (`impl Display for Point { fun fmt(f: &mut Formatter): fmt::Result = ... }`).
  Inside, methods follow the same receiver rules and fields are available implicitly when the
  type is an Oxie class.

## Extension functions

```oxie
fun String.shout(): String = self.to_uppercase()
fun <T: Clone> Vec<T>.second(): T? = self.get(1).cloned()
mut fun Vec<i32>.bump(by: i32) { for (x in self.iter_mut()) *x += by }
```

Each becomes a trait with one method plus its `impl` for the receiver type; every file of the
crate imports these traits automatically. Inside, `this`/`self` is the receiver; for Oxie
classes their fields can be used directly.

### Operators

`operator fun` inside a class or enum implements a `std::ops` trait:

| Oxie | trait | | Oxie | trait |
|---|---|---|---|---|
| `plus` | `Add` | | `plusAssign` | `AddAssign` |
| `minus` | `Sub` | | `minusAssign` | `SubAssign` |
| `times` | `Mul` | | `timesAssign` | `MulAssign` |
| `div` | `Div` | | `divAssign` | `DivAssign` |
| `rem` | `Rem` | | `remAssign` | `RemAssign` |
| `unaryMinus` | `Neg` | | `not` | `Not` |
| `shl`, `shr`, `and`, `or`, `xor` | `Shl`, `Shr`, `BitAnd`, `BitOr`, `BitXor` | | | |

Rust names (`add`, `sub`, `mul`, `neg`, `add_assign`, ...) work as well. The parameter type is
the right-hand side (`impl Mul<f64> for Vec2` for `operator fun times(k: f64)`).

## Statements

* Variable declarations (see [Declarations](#declarations)).
* Expressions, including assignments `a = b`, `a += b` (all compound operators).
* Local `fun`, `class`, `enum`, ... declarations.
* `return`, `break ['label] [value]`, `continue ['label]`, `throw e`.
* Attributes on a statement: `@cfg(debug_assertions) println("debug")`,
  `#[allow(unused_variables)] val x = 1`.

## Expressions

| Oxie | Rust |
|---|---|
| `a == b`, `a < b`, `a && b`, `a + b`, ... | same (Rust precedence) |
| `x in coll` / `x !in coll` | `coll.contains(&x)` / `!coll.contains(&x)` |
| `key in map` | `map.contains_key(key)` (`HashMap`, `BTreeMap`) |
| `c in text` (a `char`) | `text.contains(c)` |
| `x in a..b` | `a <= x && x < b` |
| `x is Pattern` / `x !is Pattern` | `matches!(x, Pattern)` / `!matches!(..)` |
| `a to b` | `(a, b)` |
| `a until b`, `a downTo b`, `r step n` | `a..b`, `(b..=a).rev()`, `r.step_by(n)` |
| `a..b`, `a..=b`, `a..<b` | `a..b`, `a..=b`, `a..b` |
| `x as T` | `x as T` |
| `x as T < y`, `x as T << n` | `(x as T) < y`, `(x as T) << n` |
| `x?` | `x?` |
| `x!!` | `x.unwrap()` |
| `a ?: b` | `a.unwrap_or(b)`-like; see [Null safety](#null-safety) |
| `a?.b` | safe navigation or error propagation; see [Null safety](#null-safety) |
| `await f()` / `f().await` | `f().await` |
| `Type.member`, `module.item` | `Type::member`, `module::item` |
| `f<T>(x)`, `mutableListOf<i32>()` | `f::<T>(x)`, `Vec::<i32>::new()` |
| `Point { x: 1, y: 2 }` | struct literal (with `..base`) |
| `x.let { it + 1 }` | `{ let it = x; it + 1 }` |
| `p.copy(x = 1)` | clone with `x` replaced |
| `if (c) a else b`, `when`, `try`, blocks | expressions with values |

**Strings and `+`**: when an operand of a `+` chain is a string literal or template, the chain
builds a `String` with `format!` (`"total: " + n` works for any `Display` value). So does a `+`
of text that Rust rejects: an owned `String` on the right (`first + last`) or borrowed text on
the left (`a + b` for two `&str`). Neither side is moved. `s += t` appends `&t`. Rust's own
`a + &b`, which moves `a` and reuses its buffer, stays as written.

```oxie
fun main() {
    val first = "Ada".to_string()
    val last = "Lovelace".to_string()
    val full = first + " " + last      // format!("{} {}", first, last)
    val joined = first + last          // format!("{}{}", first, last)
    var log = "start".to_string()
    log += last                        // log += &last
    println("$full, $joined, $log, $first $last")
}
```

**Implicit conversions** happen only where Rust would otherwise reject the code, and only to
the declared type of a variable, field, parameter or return value:

* a string literal to `String` (`String::from("...")`), a `&str` value to a `String`
  parameter (`.into()`);
* a value to `T?` (`Some(...)`, or `.into()` for variables), `null` to `None`;
* a lambda to a function-typed field (`Box::new(...)`), a constructed value to an interface-typed
  field or collection (`Box::new(...)`);
* a `vec[...]` to `Vec<T>` element by element.

## Strings

```oxie
"Hello, $name!"            // format!("Hello, {name}!")
"sum: ${a + b}"            // format!("sum: {}", a + b)
"${value:.2} ${list:?}"    // format specs after `:` (Rust's format syntax)
"cost: $5, \$price"       // `$` before a digit is text; `\$` before a name too
"""
    multi-line
      text with $vars
    """                    // common indentation and the first/last empty lines are removed
```

* `$` starts a template only before a name or `{`; elsewhere (`$5`, `100$`) it is text, as in
  Kotlin. `\$` is a dollar sign in both kinds of strings (`"""` strings are otherwise raw).
  In `error enum` messages, `$0` is the case's first positional field.
* A literal without `$` is a `&'static str`; a template is a `String`.
* Inside `println`, `print`, `eprintln`, `format`, `panic`, `assert`, `write`, `writeln`, ...
  a template is spliced directly into the format string (no extra allocation).
* A string literal passed to these with extra arguments is a Rust format string:
  `println("{} items", n)`.
* `println(a, b)` prints both separated by a space; `println(x)` prints any `Display` value.

## Null safety

* `T?` is `Option<T>`, `null` is `None`. `x == null` and `x != null` are `x.is_none()` and
  `x.is_some()`, so the value's type needn't support `==`.
* A branch next to a `null` one is optional too: `val v = if (c) x else null` is
  `if c { Some(x) } else { None }`, and the same goes for `when` arms. For a value from Rust
  whose type Oxie doesn't know (a function of a Rust module), it is `x.into()`, which is
  `Some(x)` when `x` isn't optional; when it already is, rustc asks for the type: declare it
  (`val v: T? = ...`).
* `a?.b`, `a?.m()`, `a?.b?.c` — **safe navigation** when `a` is optional: the result is
  `None` if `a` is. Chains flatten like Kotlin (`?.` on a `T?` property continues the chain;
  `?.` after a non-optional property acts like `.`). Reading a field through a borrowed value
  gives a reference (`Option<&T>`).
* When the left side is a `Result` (a call to a `throws` function, `.await`, `parse()`,
  `File::open(...)`, ...), `a?.b` is Rust's `?` followed by `.b`: the error is propagated.
  For a left side of unknown type, `?.` propagates errors inside functions that return
  `Result` and navigates otherwise. To navigate an optional value of unknown type there,
  declare its type first: `val v: String? = lookup(k)`, then `v?.len()`.
* `a ?: b` — `a` if present (`Some`/`Ok`), else `b`, evaluated only when needed. `b` may be
  `return`, `throw`, `break`, `continue` or `panic(...)`. When `b` is a literal or a value that
  costs nothing to read (a constant, an enum variant, a variable, a field), `Option<&T> ?: b`
  yields `T` for a `Copy` `T` (`server?.encoding ?: Encoding.Utf16`), `Option<String> ?: "text"`
  yields `String` and `Option<&String> ?: "text"` yields `&str`. So through a borrowed value (`u: &User`), `u.address?.city ?: "unknown"` is borrowed
  text; where a `String` is needed, copy the field explicitly, as for any field read through a
  reference: `u.address?.city.clone() ?: "unknown"`. The compiler error offers this fix.
* `x!!` — unwrap.
* `if (val v = opt) { }` / `while (val v = opt)` — `if let Some(v)` / `while let Some(v)`;
  with a pattern (`if (val Ok(v) = r)`) the pattern is used as written.
* A null check unwraps a local variable or parameter whose type Oxie knows to be `T?`, when the
  branch doesn't assign it: in `if (x != null) { }` (also as a part of an `&&` chain) `x` is the
  value, `if let Some(x) = &x` (a reference; a `Copy` value is copied, and `&mut x` is used when
  the branch changes it). After `if (x == null) return` (or `throw`, `continue`, `break`), `x` is
  the value for the rest of the block: `let Some(x) = x else { return };`. Fields are not
  unwrapped (unwrap them with `if (val v = &a.field)`).

```oxie
fun initial(name: String?): char {
    if (name == null) return '?'
    name.chars().next().unwrap_or('?')     // `name` is a String here
}

fun main() {
    val nick: String? = "ada".to_string()
    if (nick != null && nick.len() > 2) println("hello ${nick.to_uppercase()}")
    println("${initial(nick.clone())} ${initial(null)}")
}
```
* `x?.let { ... }` runs the block when `x` is present.

## Errors

See [Error handling](errors.md) for the full guide.

| Oxie | Rust |
|---|---|
| `fun f(): T throws` | `-> Result<T, AnyError>` (`Box<dyn Error + Send + Sync>`) |
| `fun f(): T throws E` | `-> Result<T, E>` |
| `return v` / tail `v` in a throwing function | `return Ok(v)` / `Ok(v)` |
| `throw e` | `return Err(e.into())` |
| calling a throwing Oxie function as a statement or result | adds `?` |
| a Rust call that returns a `Result` (`fs.write(path, text)`) as a statement | adds `?` |
| `try { a()?; b } catch (e: E) { c }` | labeled block; `?`/`throw` inside break out with `Err` |
| `error enum E { V(f: T) = "msg $f" }` | enum + `Display` + `Error` (+ `source`) + `From` |
| `error class E(f: T) = "msg $f"` | struct + `Display` + `Error` |
| `x.context("msg")` / `.with_context { ... }` | wrap the error with a message |

## Control flow

```oxie
if (cond) a else b                  // also `if cond { }` (Rust style)
if (val x = opt) { } else { }
if (val x = opt && x > 0 && val y = f(x)) { }   // let chain (any Rust edition)
while (val x = stack.pop() && x != 0) { }
when (x) {                          // match
    1, 2 -> "one or two"            // alternatives
    in 3..=9 -> "small"             // range (or any `contains`)
    is Some(n) if n > 0 -> "guard"  // `is` optional, `if` guard
    else -> "other"                 // `_`
}
when { a < b -> ...; else -> ... }  // if/else chain
when (val r = compute()) { ... }    // binds the subject
match x { Some(v) => v, None => 0 } // Rust match syntax works too
for (item in items) { }             // also `for item in items { }`
for ((i, x) in xs.iter().enumerate()) { }
while (cond) { }
do { } while (cond)
loop { }
'outer: for (...) { break 'outer }
val v = 'found: { if (c) break 'found 1; 0 }   // labeled block with a value
val n = const { SIZE * 2 }                      // evaluated at compile time
```

* A statement `when` on literals doesn't need `else` (Oxie adds `_ => {}`); a `when` used as a
  value must be exhaustive, as in Rust.
* A `when` on a `String` (or `&String`) with text arms matches it as `&str`:
  `when (cmd) { "go" -> ... }` is `match &*cmd` (`match cmd.as_str()` for a `&String`).
* `if`/`for`/`while` bodies can be a single statement without braces.
* `for (x in xs)` consumes `xs`, as in Rust, unless `xs` is a local collection (`Vec`, `VecDeque`,
  `HashMap`, `HashSet`, `BTreeMap`, ...) that is used after the loop or by the next round of an
  enclosing loop: then the loop borrows it (`for x in &xs`), and `x` is a reference to the element
  (`Copy` elements are bound by value: `for &n in &nums`). A loop over a collection field of the
  class always borrows it, except in methods taking `self`. When the body changes the element
  (`x.qty += 1`, `x.tags.push(t)`), the loop borrows the collection mutably
  (`for x in &mut xs`), and a method that does this to a field changes `this`.

```oxie
data class Item(name: String, var qty: i32)

fun main() {
    var items = vec[Item("pen", 1), Item("ink", 2)]
    for (item in items) item.qty += 10      // for item in &mut items
    println("${items:?}")
}
```

## Lambdas

```oxie
{ it * 2 }                      // one implicit parameter `it`
{ a, b -> a + b }               // Kotlin style
{ (key, value) -> key }         // destructuring
{ x: i32 -> x * 2 }             // typed
(a, b) => a + b                 // TypeScript style
x => x * 2
|a, b| a + b                    // Rust style
move { ... }                    // capture by value
{ [counter, tx] -> ... }        // capture list: moves in clones of `counter` and `tx`
{ [n = items.len()] x -> ... }  // a computed capture, then parameters
f(x) { trailing lambda }        // last argument outside the parentheses
```

* A brace lambda without `->` has no parameters, or `it` if it uses `it`. Closures with an
  explicit empty list (`|| it + 1`, `() => it`) take no parameter and use an outer `it`. When a
  standard method always passes a value (`map`, `filter`, `map_err`, `unwrap_or_else` on
  results, ...), an unused parameter is added for you (`map_err { MyError() }`).
* A lambda body is a block; its last expression is the result.
* A lambda stored in a `val` that assigns to captured variables is declared `mut`
  automatically.
* A **capture list** `[a, b = expr]` at the start of a brace lambda (followed by parameters or
  `->`) gives the lambda its own values: `a` is `a.clone()`, `b` is `expr`, and the lambda is
  `move`. `thread.spawn { [counter, tx] -> ... }` becomes
  `thread::spawn({ let counter = counter.clone(); let tx = tx.clone(); move || ... })`, the usual
  Rust pattern for handing shared handles and channel senders to threads and callbacks.
* In predicates (`filter`, `find`, `position`, `any`, `all`, `count`, `take_while`, `retain`,
  ...) the parameter is a reference to the element (`&&T` for `iter().filter`). Comparing or
  computing it with a number, `char` or `bool` literal reads the value through the
  references: `filter { it > 2 }`, `filter { it % 2 == 0 }`.
* `run { ... }` is a block expression; `repeat(n) { ... }` loops `n` times with `it`.
* `{ ... }` on its own as a statement is a lambda too, so it would never run: Oxie reports it
  as an error and suggests `run { ... }`. As the last expression of a block it is the block's
  value (a function returning a lambda can end with `{ 42 }`).

## Collections

| Oxie | Rust |
|---|---|
| `vec[1, 2, 3]`, `listOf(1, 2, 3)`, `mutableListOf()` | `vec![1, 2, 3]`, `Vec::new()` |
| `vec[0; n]` | `vec![0; n]` |
| `[1, 2, 3]`, `arrayOf(...)` | array `[1, 2, 3]` |
| `mapOf(k to v, ...)` | `HashMap::from([(k, v), ...])` |
| `setOf(a, b)` | `HashSet::from([a, b])` |
| `m[key]`, `v[i]`, `&v[1..3]` | indexing and slicing; a list, array or slice takes an index of any integer type (`v[i as usize]` for an `i32` `i`) |
| `m[key] = v` on a `HashMap` or `BTreeMap` | `m.insert(key, v)`: adds the key or replaces its value |

All standard library types and methods are used with their Rust names.

`m[key] += 1` on a map is an error: the key may be missing, and what that should mean is up to
the program. `*m.entry(key).or_default() += 1` starts a missing key at its default value (0,
`""`, an empty list), and `*m.get_mut(&key).unwrap() += 1` stops the program when it is missing.

```oxie
import std::collections::HashMap

fun main() {
    var stock: HashMap<String, i32> = HashMap()
    stock["pen"] = 3                          // stock.insert(String::from("pen"), 3)
    for (item in ["pen", "ink", "pen"]) *stock.entry(item.to_string()).or_default() += 1
    println("${stock["pen"]} ${stock["ink"]}")
}
```

### Collection operations

Lists, arrays and slices (anything that derefs to `[T]`) also have Kotlin-style operations.
Each one is the iterator chain you would write by hand; the element is passed by reference.

| Oxie | Result | Rust |
|---|---|---|
| `xs.map { f(it) }`, `map_indexed { i, x -> }` | `Vec<U>` | `xs.iter().map(f).collect()` |
| `xs.filter { p(it) }`, `filter_not` | `Vec<T>` (copies kept elements, `T: Clone`) | `xs.iter().filter(p).cloned().collect()` |
| `xs.filter_map { }`, `flat_map { }` | `Vec<U>` | `filter_map` / `flat_map` + `collect` |
| `xs.for_each { }`, `for_each_indexed { i, x -> }` | — | `iter().for_each(..)` |
| `xs.find { }`, `find_last { }` | `T?` (a reference) | `iter().find(..)` |
| `xs.index_of_first { }` | `usize?` | `iter().position(..)` |
| `xs.any { }`, `all { }`, `none { }`, `count { }` | `bool` / `usize` | `iter().any(..)`, ... |
| `xs.sum()`, `sum_of { }` | number | `iter().sum()`, `iter().map(..).sum()` |
| `xs.max_of { }`, `min_of { }` | `S?` | `iter().map(..).max_by(..)` |
| `xs.max_by_key { }`, `min_by_key { }` | `T?` (a reference) | works with `f64` keys too |
| `xs.sorted()`, `sorted_descending()`, `sorted_by { }`, `sorted_by_descending { }` | `Vec<T>` | `to_vec()` + `sort` |
| `xs.reversed()`, `distinct()`, `distinct_by { }`, `chunked(n)` | `Vec<T>` / `Vec<Vec<T>>` | |
| `xs.group_by { key }` | `HashMap<K, Vec<T>>` | |
| `xs.associate_by { key }` | `HashMap<K, T>` | |
| `xs.partition { }` | `(Vec<T>, Vec<T>)` | |
| `xs.join_to_string(", ")`, `join_with(", ") { it.name.as_str() }` | `String` | |
| `xs.is_not_empty()` | `bool` | `!xs.is_empty()` |

Inherent Rust methods win over these (arrays keep Rust's by-value `map`), and Rust's iterator
methods are always available: `xs.iter().filter { .. }.map { .. }.collect()`.

## Modules and imports

* Each `.oxi` file under `src/` is a module: `src/util.oxi` → `util`, `src/net/http.oxi` →
  `net::http`, `src/net/mod.oxi` → `net`. `src/main.oxi` (or `src/lib.oxi`) is the crate root.
* `import a::b::C`, `import a.b.C`, `import a::b::{C, D as E}`, `import a::b::*`,
  `import { C, D } from a::b` (TypeScript), `use ...` (Rust). Paths that start with one of
  your modules are resolved from the crate root, so `import util::helper` works in every file.
* `mod name { ... }` declares an inline module; `mod name` (without a body) declares a
  hand-written Rust module file next to the source.
* Paths may use `.` instead of `::` when the left side is a type, module or crate name:
  `std.mem.swap(a, b)`, `Duration.from_secs(1)`, `Color.Red`, `math.double(2)` (inline
  module), `shapes.Circle(2.0)` (a class through its module), `Stack::<i32>.new()`.
* Classes, enums and functions in different modules may share a name. Each keeps its own
  constructor or parameters, inferred receivers and derives; a bare name means the module's
  own one or the one it imports, and a path (`a.Counter(1)`, `b.describe(x)`) names the others.

## Visibility

| Oxie | Rust |
|---|---|
| (default) | `pub(crate)` — visible in the whole project |
| `pub` / `public` | `pub` — exported from a library |
| `private` | private to the file (module) |
| `internal` | `pub(crate)` |
| `pub(super)`, `pub(in path)` | same |

Members of a `pub` class or enum default to `pub`. Keeping items unexported by default lets the
optimizer treat them like private Rust functions; mark library APIs `pub`.

## Attributes

`@name` and `@name(args)` become `#[name]` / `#[name(args)]`: `@derive(Serialize)`,
`@serde(rename_all = "camelCase")`, `@inline`, `@cfg(test)`, `@tokio::main`, ... Rust's `#[...]`
syntax works too. Oxie-specific: `@no_auto_derive` (classes/enums), `@from` (error enum cases).
Attributes also work on statements (see [Statements](#statements)). A class property that exists
only in some builds (`@cfg(test) var built: usize`) is no constructor parameter: it starts with
its default value, or the type's `Default`.

`#![...]` at the top of a file:

* in a module file, it applies to that module (`#![allow(dead_code)]`);
* in the crate root (`main.oxi`/`lib.oxi`), lint attributes (`allow`, `warn`, `deny`, `forbid`,
  `expect`) apply to the whole crate. Other crate attributes (`#![recursion_limit = "..."]`,
  `#![no_std]`, ...) work in single-file scripts; in a project, put them in `src/main.rs` /
  `src/lib.rs`.

## Tests

```oxie
test "addition works" {
    assert_eq(1 + 1, 2)
}
test "can fail" throws { "1".parse::<i32>()? }
async test "needs tokio" { ... }      // add @tokio::test
```

Tests go into a `#[cfg(test)]` module with `#[test]` functions named after the description.
Run with `oxie test` (or `cargo test`).

## Async

`async fun`, `async { }`, `async move { }`, `.await` and TypeScript-style prefix `await expr`.
Use any runtime: `@tokio::main async fun main()`. A throwing async `main` works too.

## Rust escape hatches

* `rust { ... }` — raw Rust items (at top level) or a raw Rust expression/statements.
* `name!(...)`, `name![...]`, `name!{...}` — any Rust macro, passed through unchanged.
  Common std macros can also be called without `!` (`println(...)`, `assert_eq(...)`,
  `vec[...]`, `format(...)`, `panic(...)`, `todo()`, `dbg(...)`, `matches(...)`, ...).
* `macro_rules!` definitions at top level.
* Rust syntax that Oxie shares (`let`, `match` with `=>`, `impl`, `fn`, `|x|` closures,
  `::<T>` turbofish, `if let`, `while let`, let chains, `'label:` loops and blocks, `const { }`,
  `union`, `extern "C" { }`, `#[attr]`, `&'a self`) is accepted directly.
  [`tests/coverage`](../tests/coverage) has one program per Rust feature.

## Keywords

Reserved: `fun fn val var let if else when match for while do loop return break continue throw
try catch class interface trait enum struct impl import use const static typealias mod true
false null this self Self in is as where async move unsafe dyn mut ref extern pub`.

Contextual (usable as names elsewhere): `data`, `error`, `shared`, `object`, `companion`, `init`,
`union`, `override`, `operator`, `private`, `internal`, `public`, `test`, `rust`, `throws`, `to`,
`until`, `downTo`, `step`, `await`, `it`, `context`.
