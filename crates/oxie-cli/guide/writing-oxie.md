# Writing Oxie

Oxie is a programming language that compiles to Rust. It reads like Kotlin, and its semantics
are Rust's: ownership and borrowing, traits, generics, `Option` and `Result`, and every Rust
crate. This guide is for the Oxie version that printed it. Oxie changes between versions, so
when this guide or the compiler disagrees with what you remember about Oxie, they are right.

## How to work

* A project is a Cargo crate. Code goes in `src/main.oxi` (`src/lib.oxi` for a library), and
  every other `.oxi` file in `src/` is a module: `src/net/http.oxi` is `net::http`.
  `src/main.rs` and `build.rs` only include the Rust that Oxie generates. Don't edit them, and
  don't write Rust files to get around an Oxie error.
* `oxie check` type-checks the project and is fast. Run it after each change and fix what it
  reports before you write more.
* `oxie run` builds and runs the program (arguments after `--` go to it), `oxie test` runs the
  `test` blocks, and `oxie fmt` formats the sources. A `.oxi` file outside a project runs as a
  script, `oxie run tool.oxi`, with its crates in header comments: `// dep: rand = "0.8"`.
* Errors are explained in Oxie terms at `.oxi` lines. Many end with `= fix:` and the corrected
  line: apply it when it matches what you meant. A fix that adds `.clone()` copies the data;
  when the value is only read, borrowing it (a `&T` parameter) is usually the better fix.
* `oxie emit src/main.oxi --annotate` prints the Rust generated for a file, with the reasons
  for what Oxie inferred. Use it when you aren't sure what a construct does.
* `oxie from-rust file.rs` translates Rust into Oxie, for example code from a crate's docs.
* `oxie guide <topic>` prints the full documentation of this version; the topics are listed at
  the end.

## The language at a glance

```oxie
import std.collections.HashMap

data class Item(name: String, price: f64, qty: u32 = 1)    // fields, ==, copy(), Debug

interface Priced {
    fun total(): f64
    fun label(): String = "costs ${total():.2}"             // default method
}

class Cart(owner: String) : Priced {                        // no inheritance, only interfaces
    var items: [Item] = []                                  // a list; body properties need a value

    fun add(item: Item) = items.push(item)                  // changes the cart: Oxie makes it &mut self
    override fun total() = items.sum_of { it.price * it.qty as f64 }
}

fun cheapest(items: [Item]): &Item? = items.min_by_key { it.price }

fun main() {
    var cart = Cart("ada")                                  // `var`, because add() changes it
    cart.add(Item("pen", 1.5, qty = 4))
    cart.add(Item("book", 12.0))
    println("${cart.owner}'s cart ${cart.label()}")

    val names = cart.items.filter { it.price < 10.0 }.map { it.name.clone() }
    println("cheap: ${names.join_to_string(", ")}, first: ${cheapest(cart.items)?.name ?: "none"}")

    var counts = HashMap()
    for (word in "a b a".split(' ')) *counts.entry(word).or_insert(0) += 1
    println("a appears ${counts["a"]} times, $counts has ${counts.len()} keys")

    for (i in 0 until 3) print("$i ")                       // 0 1 2
    for (i in 1..=3) print("$i ")                           // 1 2 3: `..` excludes the end
    println()
}
```

| Oxie | Meaning (Rust) |
|---|---|
| `val x = 1`, `var y = 2`, `val z: i64 = 3` | `let`, `let mut`, typed `let` |
| `fun f(a: i32, b: i32 = 0): i32 { }`, `fun g(x: i32) = x * 2` | functions; defaults, named arguments `f(1, b = 2)` |
| `context(log: &mut Log) fun f()`, `context(cx: &mut Ctx) class C { }` | context parameters: each call passes the caller's variable named `log`; a class's methods take the ones they use |
| `"$name is ${age + 1}"`, `"${x:.2}"`, `"$list"`, `"${point:?}"` | templates, format specs, lists and optionals, Debug |
| `if (c) a else b`, `when (x) { 1, 2 -> a; in 3..=9 -> b; else -> c }`, `when { c -> a; else -> b }` | expressions; `when` is `match` (or an if chain without a subject) |
| `for (x in xs)`, `for ((i, x) in xs.iter().enumerate())`, `while`, `do { } while ()`, `loop`, `repeat(n) { }` | loops |
| `0 until n`, `1..=n`, `a..b`, `10 downTo 0 step 2` | ranges: `a..b` excludes `b` |
| `T?`, `null`, `a?.b`, `a ?: b`, `a!!`, `if (val v = opt) { }`, `opt?.let { it }` | `Option<T>`, `None`, map, `unwrap_or`, `unwrap`, `if let Some(v)` |
| `fun f(): T throws`, `throws E`, `throw e`, `x?`, `try { } catch (e) { }`, `r ?: default` | `Result<T, AnyError>`, `Result<T, E>`, `return Err`, `?`, a block that catches `?` |
| `class C(a: A, var b: B = d) { }`, `C(a)` | struct with fields `a`, `b`, constructor `C::new` |
| `data class P(x: f64)`, `p.copy(x = 1.0)` | struct deriving `Debug`, `Clone`, `PartialEq` (and `Copy`, `Eq`, `Hash` when the fields allow) |
| `enum E { A(n: i32), B }`, `E.A(1)`, `when (e) { A(n) -> ...; B -> ... }` | enum with data; variants without `E.` in patterns |
| `interface I { fun f(): i32 }`, `class C : I { override fun f() = 1 }` | trait and its impl |
| `&I`, `[I]` (an interface as a type) | `&dyn I`, `Vec<Box<dyn I>>` |
| `shared class S(var n: i32)`, `@sync shared class` | a handle to one object: `Rc<RefCell<..>>`, `Arc<Mutex<..>>` |
| `operator fun plus(o: V): V`, `minus`, `times`, `div`, `rem`, `unaryMinus`, `plusAssign` | `impl Add`, `Sub`, `Mul`, `Div`, `Rem`, `Neg`, `AddAssign` |
| `override fun toString() = "($x, $y)"` | `impl Display` |
| `fun String.shout(): String = this.to_uppercase()`, `fun i32.is_even(): bool = this % 2 == 0` | extension functions (a trait and its impl, imported everywhere) |
| `object O { }`, `companion object { }`, `static fun` | namespace; static functions |
| `{ it * 2 }`, `{ a, b -> a + b }`, `(a, b) => a + b`, `xs.map { }` | lambdas; a last lambda argument goes outside the parentheses |
| `[T]`, `[K: V]`, `&[T]` | `Vec<T>`, `HashMap<K, V>`, a slice |
| `val xs: [i32] = [1, 2]`, `val m: [String: i32] = ["a": 1]`, `[:]`, `setOf(1)` | `vec![1, 2]`, `HashMap::from([..])`, `HashMap::new()`, `HashSet` |
| `x in xs`, `x !in xs`, `x is Some(_)`, `a to b` | `contains` (`contains_key` for a map), `matches!`, a tuple `(a, b)` |
| `Type.function()`, `Enum.Variant`, `module.item`, `HashMap()` | `::` paths; a constructor call `Type::new()` |
| `import std::fs`, `import std.collections.HashMap`, `import { A, B } from module` | `use` |
| `test "adds" { assert_eq(add(1, 1), 2) }` | a `#[test]` function |
| `@derive(Hash)`, `@tokio::main` | `#[derive(Hash)]`, `#[tokio::main]` |
| `println(x)`, `format("{} {}", a, b)`, `vec[0; n]`, `panic("...")`, `assert(c)` | std macros without `!`; crate macros keep it: `json!({})` |

## What works differently from Kotlin

* **Ranges.** `1..n` excludes `n`; Kotlin's `1..n` is `1..=n` here. `until`, `downTo` and
  `step` work as in Kotlin.
* **Collections are Rust's, with short names.** A list is `[T]` (Rust's `Vec<T>`) and a map
  `[K: V]` (std's `HashMap<K, V>`); there is no `List`, `MutableList` or `Map` type, and sets
  are `HashSet<T>`. A literal takes the declared type: `val names: [String] = ["ada", "bob"]`,
  `var stock: [String: i32] = [:]`, `["a": 1]`. Without a declared type `[1, 2]` is a
  fixed-size array, unless the code after a `var` grows it (`xs.push(3)`), which makes it a list.
  After `&`, `[T]` is a slice: `&[T]` takes any list or array. A function that adds to the
  caller's list can still write `&mut [T]`: when it grows it (`push`, `insert`, `clear`) or
  passes it on to a function that does, Oxie makes it `&mut Vec<T>`. `iter.to_list()` collects an iterator into a list.
  `Vec<T>`, `HashMap<K, V>`, `vec[...]`, `listOf(...)` and `mapOf(k to v)` still work.
  `m[k] = v` puts a value in a map, but `m[k] += 1` is an error (the key may be missing): write
  `*m.entry(k).or_default() += 1` to start a missing key at 0.
* **Method names are Rust's**: `len()` (not `size` or `length`), `is_empty()`,
  `to_uppercase()`, `contains()`, `push()`, `insert()`, `remove()`, `to_string()`,
  `trim()`, `split(',')`, `chars()`, `parse::<i32>()` (it returns a `Result`). Lists also have
  Kotlin's collection operations under snake_case names: `map`, `filter`, `filter_map`,
  `flat_map`, `for_each`, `find`, `any`, `all`, `none`, `count`, `sum`, `sum_of`, `max_of`,
  `min_of`, `max_by_key`, `min_by_key`, `sorted`, `sorted_by`, `sorted_by_descending`,
  `reversed`, `distinct`, `group_by`, `associate_by`, `partition`, `chunked`,
  `join_to_string`, `join_with`, `map_indexed`, `index_of_first`, `is_not_empty`. They return
  new `Vec`s (`find`, `first()`, `max_by_key` return references). As in Kotlin, `sorted_by`
  takes a key (`sorted_by { it.age }`); to sort with a comparator, call Rust's `sort_by` on a
  `var` list: `xs.sort_by { a, b -> b.age.cmp(&a.age) }`. `join_to_string(", ")` joins,
  `join_with(", ") { it.name.as_str() }` maps each element first. They exist on lists, arrays
  and slices, not on iterators: what `iter()`, `split()`, `chars()`, `lines()` or a range
  `.map { }` return is a lazy iterator with Rust's methods, so call the operation on the list
  (`xs.map { }`, not `xs.iter().map { }`) or `collect()` first:
  `"b a".split(' ').to_list().sorted()`. `is_not_empty()` is only for lists; for text
  write `!s.is_empty()`. You implement `toString()`, but you call `to_string()`.
* **Numbers don't convert by themselves.** `i32 * f64` is an error: write `n as f64`.
  `len()` is a `usize`; a list can be indexed with any integer type. Integer division truncates. `Int`, `Long` and `Double` are
  names for `i32`, `i64` and `f64`.
* **Values have one owner.** Assigning a `String`, list or class value, or passing it to a
  function that keeps it, moves it, and the old variable can't be used afterwards. Lend it
  instead (`&T` and `&mut T` parameters; Oxie writes the `&` at the call), or pass
  `.clone()` when the function really needs its own copy. Write parameters as values
  (`name: String`, `items: [Item]`, `prices: [String: f64]`, `user: User`): a function or
  method that only reads one borrows it without your help (`&str`, `&[Item]`, ...), and one that
  keeps it takes it. A `for (x in xs)`
  over a local list that is used afterwards borrows it, so `x` is a reference; when the body
  changes `x` (`x.qty += 1`), it borrows the list mutably.
* **Objects are not shared references.** `val b = a` moves or copies `a`; it doesn't alias it.
  When several places must see the same mutable object (accounts shared by customers, nodes
  of a graph, state shared between threads), declare it `shared class`, or
  `@sync shared class` to use it from several threads. Copying such a value copies a handle,
  and `==` on it compares identity. Inside its methods `this` passed on (`child.parent = this`)
  is a handle; a method that reaches its own object again while it runs panics with an
  explanation.
* **Mutability belongs to the variable.** A `val` object can't call a method that changes it;
  declare the variable `var`. Oxie works out which methods change their object.
* **No inheritance.** There are no `open`, `abstract` or `sealed` classes and no `: Base()`.
  Share behavior with interfaces (they can have default methods) and composition; use an
  `enum` with data where Kotlin would use a sealed class (`enum class` is accepted as `enum`).
  Interfaces have no properties: declare `fun name(): String` and implement it with
  `override fun name() = name.clone()`. There are no type tests (`is Int`): match on an enum. Constructor parameters are fields
  (no `val`/`var` needed), there are no getters or setters, and `this.` is only needed when a
  parameter has the field's name. A method hides a function of the module with its name:
  call that function as `self::name(x)`.
* **No exceptions.** `throws`, `throw`, `try`/`catch` and `?` work on `Result` values, and a
  function that can fail says so in its signature. Calling a throwing function (or a Rust call
  that returns a `Result`, like `fs.write(path, text)`) as a statement inside another throwing
  function, or inside `try { }`, passes its error on (to the caller, or to `catch`); anywhere
  else you get the `Result` and
  handle it: `when (val r = f()) { Ok(v) -> ...; Err(e) -> ... }`, `f() ?: default`,
  `if (val Err(e) = f())`, or `.unwrap()`.
* **`null` is `Option`.** A `T?` must be unwrapped before use. A null check unwraps a local
  variable or parameter: in `if (x != null) x.foo()`, `if (x != null && x.len() > 2)` and the
  `else` of `if (x == null)`, `x` is the value (a reference to it), and after
  `if (x == null) return` (or `if (a == null || b == null) return`) it is the value. `x!!.len()`
  reads through `x` without taking it, so `x` stays usable. Fields aren't unwrapped: write `if (val v = &a.field)`, `a.field?.foo()`, or `val v = a.field ?:
  return`. `if (val x = x)` takes the value out of `x`; to use `x` again afterwards, unwrap a
  reference: `if (val v = &x)`. A `when`, `if (val ...)` or `==` on an optional matches its
  value, without `Some(..)`: `when (xs.first()) { 3 -> ...; null -> ...; else -> ... }`,
  `while (val Token.Op(op) = peek())`, `if (peek() == Token.LParen)`. Add a `null ->` (or
  `else ->`) arm, since the value can be missing.
* **Context parameters are found by name**, not by type: a call to `context(log: &mut Log) fun
  f()` passes the variable named `log` where the call is (a local, a parameter, a lambda's
  parameter or the caller's own context), or one given by name, `f(log = &mut other)`.
* **Lambdas** are Rust closures. `return` in a lambda returns from the lambda, not from the
  enclosing function. In predicates (`filter`, `find`, `any`, `count`, ...) the element is a
  reference: comparing it with a literal reads through it (`filter { it > 2 }`), but to pass
  it on write `*it` or `it.clone()`. When the compiler can't tell a parameter's type
  ("type annotations needed", as with `trim_matches { }`), write it:
  `{ c: char -> !c.is_alphanumeric() }`.
* **Text.** `a + b` joins text into a new `String` without moving either side, and
  `s += t` appends; a `when` on a `String` matches its text arms (`"go" -> ...`).
* **Destructuring.** `val (x, y) = point` takes a data class's fields in constructor order, and
  `if (val (k, v) = pair_or_null)` unwraps an optional pair. `val (tx, var rx) = ...` lets only
  `rx` change (write `var`, not Rust's `mut`).
* **Printing.** `"$x"` and `println(x)` print numbers, text and types with `toString()`; lists,
  maps, sets and tuples print in their debug form (`[1, 2]`), and an optional prints its value
  or `null`. Other values (a `data class` without `toString()`, an enum) print with `"${x:?}"`,
  Rust's debug form, which also shows an optional as `Some(..)`. When Oxie can't tell that a
  value is a list or optional (a Rust function's result), write `:?` too. A whole `f64` prints
  without decimals (`43`); write `"${x:.1}"` for a fixed number of decimals.

## What works differently from Rust

Most Rust syntax is accepted (`fn`, `let`, `match`, `impl`, `|x|` closures, `::`, macros), so
Rust habits compile, but they skip what Oxie infers and don't read as Oxie. Write instead:

* `fun`, `val`/`var`, a return type after `:`, `when` with `->` arms, `{ x -> }` lambdas,
  templates instead of `format!`, `T?` instead of `Option<T>`, and `throws` instead of a
  `Result` return type.
* A class instead of a struct and an `impl` block. Leave out `self` parameters (`&self` or
  `&mut self` is inferred from what the method does) and `self.` in front of fields. Implement
  an interface with `: Name` and `override fun`, and `Display` with
  `override fun toString() = "..."`.
* Construct with `Point(1.0, 2.0)`, not `Point::new` or a struct literal; `HashMap()` calls
  `HashMap::new()`. Use `.` in paths: `Color.Red`, `std.mem.swap(a, b)`, `Duration.from_secs(1)`.
* No `&` or `&mut` at call sites of Oxie functions: `total(items)` lends `items` when the
  parameter is a reference. Lifetimes are added for you, and so are the trait bounds of common
  uses (`"$x"`, `a < b`, `x.clone()`, `a + b`, map keys). When the compiler still asks for one,
  as `T: Clone` for `.cloned()`, declare it: `class Stack<T: Clone>`.
* `data class` and enums derive `Debug`, `Clone` and `PartialEq` (and `Copy`, `Eq`, `Hash` when
  possible) on their own. Other derives: `@derive(Hash)` or `class C : Hash`.
* Operators are `operator fun`s in the class body (`operator fun plus(o: Vec2): Vec2`), not
  `impl Add` blocks.
* `error enum` (or `error class`) declares an error type with its messages, in place of a
  hand-written `Display` and `Error` impl.

## Patterns

**Errors.**

```oxie
error enum ParseError {
    Empty = "empty input"
    BadNumber(text: String) = "'$text' is not a number"
}

fun parse_all(text: String): [i32] throws ParseError {
    if (text.trim().is_empty()) throw ParseError.Empty
    var out = []                                 // a list: it grows below
    for (part in text.split(',')) {
        val n: i32 = part.trim().parse().map_err { ParseError.BadNumber(part.trim().to_string()) }?
        out.push(n)
    }
    out                                          // wrapped in Ok
}

fun main() {
    for (input in ["1, 2, 3", "", "4, x"]) {
        when (val r = parse_all(input)) {
            Ok(nums) -> println("sum ${nums.sum()}")
            Err(e) -> println("error: $e")
        }
    }
    val total = try {
        val a = parse_all("5")?
        val b = parse_all("y")?
        a.len() + b.len()
    } catch (e) {
        println("caught: $e")
        0
    }
    println(total)
    val fallback = parse_all("") ?: vec[]        // a default when it fails
    println(fallback)
}
```

`throws` without a type means `AnyError`, which every error and any string converts into
(`throw "bad value $x"`). `fun main() throws` prints the error and exits with status 1.

**Optional values.**

```oxie
data class User(name: String, email: String?)

fun find(users: [User], name: String): &User? = users.find { it.name == name }

fun main() {
    val users: [User] = [User("ada", "ada@example.org"), User("bob", null)]
    println(find(users, "ada")?.email.clone() ?: "no email")
    println(find(users, "bob")?.email.clone() ?: "no email")
    if (val u = find(users, "bob")) println("found ${u.name}")
    val port: u16? = "8080".parse().ok()
    println(port ?: 80)
    val n = "x".parse<i32>().ok().and_then { if (it > 0) Some(it) else null } ?: -1
    println(n)
}
```

* Through a reference (`find` returns `&User?`), `?.email` gives a borrowed value. Where you
  need a `String`, copy it: `?.email.clone() ?: "..."`.
* `?.let { }` on a variable or field borrows it, so `it` is a reference: `v?.let { Celsius(*it) }`
  for a number, or `it.clone()`. On the result of a call, `it` is the value itself:
  `mean(xs)?.let { Celsius(it) }`.
* `map.get(key)` gives an optional reference. `?:` with a literal, a constant or a variable
  reads the value for you (`counts.get(word) ?: 0` is a number), but with `return` or `throw`
  on the right you get the reference: `val n = *(stock.get(item) ?: throw ...)`, or
  `stock.get(item).copied() ?: throw ...`.
* `if (c) x else null` makes `x` optional. When `x` comes from a Rust function and is optional
  already, the compiler asks for the type: declare it, `val v: T? = if (c) x else null`.
* `a ?: b` takes `a`: for an optional field you use again later, write `a.clone() ?: b`.
* `?.` chains flatten as in Kotlin: `user?.address?.city`, `s?.trim()?.len()` and
  `s?.chars().next()` give one optional. When Oxie can't tell that a method of a Rust type
  returns an `Option`, the result is nested (`T??`): use `.and_then { }` there.
* `a ?: b ?: c` tries each in turn, and `a?.b = v` assigns only when `a` isn't null.
* The right side of `?:` can be `return`, `throw`, `continue` or `break`:
  `val v = map.get(key) ?: return`.

**Interfaces as values.**

```oxie
interface Shape {
    fun area(): f64
    fun name(): String
}

class Square(side: f64) : Shape {
    override fun area() = side * side
    override fun name() = "square"
}

class Circle(r: f64) : Shape {
    override fun area() = 3.14159 * r * r
    override fun name() = "circle"
}

fun describe(s: &Shape) = println("${s.name()}: ${s.area():.2}")

fun main() {
    var shapes: [Shape] = [Square(2.0), Circle(1.0)]          // each value is boxed for you
    shapes.sort_by { a, b -> a.area().total_cmp(&b.area()) }
    for (s in shapes) describe(s)
    describe(shapes[0])
}
```

A function returning an interface (`fun pick(): Shape`) returns its value as it is, or a box
when its branches build different classes. A list typed `[Shape]` boxes the values you put in it; with `[Box<Shape>]` you write
`Box.new(...)` yourself. Its elements, and interface-typed fields, are lent to an `&Shape`
parameter as they are: no `.as_ref()` needed. `sorted_by` needs `Clone`, which a box of an interface isn't, so sort
such a list in place with `sort_by`.

**Shared objects.**

```oxie
shared class Account(owner: String, var balance: i64 = 0) {
    fun deposit(amount: i64) {
        balance += amount
    }
}

fun main() {
    val joint = Account("ada")
    val accounts = vec[joint, Account("bob")]     // the list holds a handle to the same account
    accounts[0].deposit(50)
    joint.deposit(25)
    println("${joint.balance} ${accounts[0] == joint}")
    println(accounts.map { "${it.owner()}=${it.balance()}" }.join_to_string(" "))
}
```

Outside its class, a shared object's field reads like a field (`joint.balance`). Inside a lambda
over a list of them, Oxie may not know the element's type: call the getter, `it.balance()`, as
the compiler error suggests.

**Functions as values.**

```oxie
class Step(name: String, run: (i32) -> i32)

fun make_adder(n: i32): (i32) -> i32 = move { it + n }     // `move`: it outlives `n`

fun apply_twice(x: i32, f: (i32) -> i32): i32 {
    val once = f(x)                                        // `f(f(x))` borrows `f` twice
    f(once)
}

fun main() {
    val steps = vec[Step("double", { it * 2 }), Step("add 3", Box.new(make_adder(3)))]
    var v = 5
    for (s in steps) {
        v = (s.run)(v)                                     // call a field that holds a function
        println("${s.name}: $v")
    }
    println(apply_twice(1) { it * 10 })
}
```

* A function-typed parameter is an `impl FnMut`; a function-typed field or list element is a
  boxed `Fn`, and lambdas and function parameters stored there (`handlers.push(f)`,
  `on_click = f`) are boxed for you. Other function values need `Box.new(f)`.
* A lambda returned from a function, stored in a field, or run on another thread must own what
  it captures: `move { ... }`, or a capture list `{ [handle] -> ... }` that clones a shared
  handle or channel sender into it.

**Values every function passes on** (GPUI's `window` and `cx`, a logger, a configuration).

```oxie
class Ctx(var notified: i32 = 0)

context(cx: &mut Ctx)
class Editor(var text: String = "") {
    fun type_text(s: &str) {
        text.push_str(s)
        refresh()                           // passes `cx`
    }
    fun refresh() {
        cx.notified += 1
    }
    fun length(): usize = text.len()        // takes no context
}

fun main() {
    var cx = Ctx()
    var editor = Editor()
    editor.type_text("hi")                  // editor.type_text("hi", &mut cx)
    println("${editor.length()} ${cx.notified}")
}
```

* `context(...)` on a class gives each method the contexts it uses, also through the methods it
  calls (`pub` ones too); don't add `cx` parameters to the methods yourself. Methods used as
  values (`cx.listener(Self.save)`) take all of them.
* Oxie fills contexts in only where it knows the function called. A method called on an untyped
  lambda parameter needs them written out: `entity.update(cx) { e, cx -> e.save(cx) }`, or type
  the parameter (`e: &mut Editor`) and let Oxie pass them.

**Tests.** `test "name" { assert_eq(f(2), 4) }` blocks sit next to the code; `oxie test` runs
them, and `oxie test "name"` runs one.

## Topics

`oxie guide <topic>` prints the documentation of this version:

* `tour` — the language by example
* `reference` — every construct and the Rust it compiles to
* `errors` — error handling in depth
* `cheatsheet` — Kotlin and TypeScript next to Oxie
* `interop` — crates, Cargo, and mixing Oxie with Rust
* `tooling` — the `oxie` command, its diagnostics, scripts and editors
* `performance` — what Oxie code costs
