# A tour of Oxie

This tour introduces the Oxie programming language by example. Every program here runs as-is: save it as `tour.oxi` and run
`oxie run tour.oxi`. If you know Kotlin or TypeScript you can read most Oxie code already; the
notes point out where Oxie follows Rust instead.

## Hello, world

```oxie
fun main() {
    val name = "World"
    println("Hello, $name!")
}
```

`fun main` is the entry point. Statements end at the end of the line; `;` is optional.

## Variables and types

```oxie
fun main() {
    val answer = 42              // immutable (Rust `let`)
    var count = 0                // mutable (Rust `let mut`)
    count += 1

    val pi: f64 = 3.14159        // explicit type
    val big: Long = 10_000_000L  // Kotlin names work too: Int, Long, Double, Boolean, Char
    val flag = true
    val letter = 'x'
    val (q, r) = (17 / 5, 17 % 5)  // destructuring
    val few = count as usize < 3   // `as` converts between number types

    println("$answer $count $pi $big $flag $letter $q $r $few")
}
```

The Rust primitive types (`i32`, `u64`, `f64`, `bool`, `char`, `usize`, ...) are the native
names; `Int`, `Long`, `Double`, `Float`, `Boolean`, `Char`, `Unit` are accepted as aliases.
Integer literals default to `i32`, floats to `f64`, as in Rust. Numbers never convert silently:
`as` converts them, and `count as usize < 3` compares the result, where Rust needs
`(count as usize) < 3`.

## Functions

```oxie
fun add(a: i32, b: i32): i32 {
    return a + b
}

// expression body; the return type is inferred when it is obvious
fun square(x: i32) = x * x

// the last expression of a block is its value, like Rust
fun max(a: i32, b: i32): i32 {
    if (a > b) a else b
}

// default and named arguments
fun greet(name: String, greeting: String = "Hello"): String = "$greeting, $name!"

fun main() {
    println(add(2, 3))
    println(square(7))
    println(max(4, 9))
    println(greet("Ada"))
    println(greet(name = "Bob", greeting = "Hi"))
}
```

Parameters are written `name: Type`, the return type after `:` (Rust's `->` also works). A
parameter that the function only reads is borrowed: `greet` takes `&str` in the generated Rust,
so `greet(name)` doesn't use up `name` (see [Ownership](#ownership)).

A value that many functions pass along, such as a configuration, a logger or GPUI's `window` and
`cx`, can be a *context parameter*: each call passes the variable of that name where it is made,
so callers don't write it.

```oxie
class Config(verbose: bool)

context(config: &Config)
fun log(msg: String) {
    if (config.verbose) println(msg)
}

context(config: &Config)
fun run_step(n: i32) = log("step $n")     // passes its own `config` on

fun main() {
    val config = Config(true)
    run_step(1)                           // run_step(1, &config)
    run_step(2)
}
```

Before a class, `context(...)` gives each method the contexts it uses, also through the methods it
calls; see [Context parameters](reference.md#context-parameters).

## Strings

```oxie
fun main() {
    val name = "Oxie"
    val version = 1.25
    println("$name ${version:.1}")       // format spec after `:` (Rust format syntax)
    println("next: ${version + 0.75}")
    println("debug: ${listOf(1, 2):?}")  // `:?` is Debug formatting

    val greeting = "Hello, " + name + "!"  // `+` with strings builds a String
    println(greeting)

    val poem = """
        Roses are red,
          $name is new.
        """                                // indentation is trimmed automatically
    println(poem)
    println("a dollar: \$5")
}
```

String literals are `&str` (borrowed, free). Templates and `+` produce `String`. Where a
`String` is expected, a literal or `&str` is converted for you.

## Control flow

`if` and `when` are expressions.

```oxie
fun describe(n: i32): &str = when {
    n < 0 -> "negative"
    n == 0 -> "zero"
    n < 10 -> "small"
    else -> "large"
}

fun main() {
    val n = 7
    val parity = if (n % 2 == 0) "even" else "odd"
    println("$n is $parity and ${describe(n)}")

    val kind = when (n) {
        0 -> "zero"
        1, 2, 3 -> "tiny"
        in 4..=9 -> "single digit"
        x if x < 0 -> "negative"
        else -> "big"
    }
    println(kind)

    for (i in 0 until 3) print("$i ")          // 0 1 2
    for (i in 10 downTo 0 step 5) print("$i ")  // 10 5 0
    for (i in 1..=3) print("$i ")               // Rust ranges: `..` excludes the end, `..=` includes it
    println()

    var i = 0
    while (i < 3) i += 1
    do {
        i -= 1
    } while (i > 0)

    val found = loop {
        i += 7
        if (i > 20) break i
    }
    println("found $found")
}
```

`when` with a subject becomes a Rust `match`; patterns are Rust patterns (`Some(x)`,
`(a, b)`, `1..=9`, `x if cond`). Without a subject it is an if/else-if chain. Conditions are
written in parentheses as in Kotlin; Rust's style without them works too.

## Collections and lambdas

```oxie
fun main() {
    val nums: [i32] = [5, 3, 8, 1]             // a list (Rust's Vec<i32>)
    val doubled = nums.map { it * 2 }          // a new list: [10, 6, 16, 2]
    val big = nums.count { it > 3 }
    val sum = nums.iter().fold(0) { acc, x -> acc + x }
    println("${doubled:?} $big $sum ${nums.sorted():?}")

    val ages: [String: i32] = ["ada": 36, "bob": 25]   // a map (Rust's HashMap<String, i32>)
    println("ada is ${ages["ada"]}")

    var counts = [:]
    for (word in "a b a c a".split(' ')) {
        *counts.entry(word).or_insert(0) += 1
    }
    println("a: ${counts["a"]}")

    val add = (a: i32, b: i32) => a + b        // TypeScript-style arrow function
    val twice = { x: i32 -> x * 2 }            // Kotlin-style lambda
    println("${add(2, 3)} ${twice(4)}")
}
```

`[T]` is a list and `[K: V]` a map, Rust's `Vec<T>` and `HashMap<K, V>` (which work too). A
literal takes the type you declare: `[5, 3, 8, 1]` is `vec![5, 3, 8, 1]` for a `[i32]`, and the
strings in `["ada": 36]` become `String`s for a `[String: i32]`. Without a declared type, `[1, 2]`
is a fixed-size array, unless the code after a `var` grows it (`xs.push(3)`): then it is a list.
`[:]` is an empty map, and `iter.to_list()` collects an iterator into a list. After `&`, `[T]`
is Rust's slice, so `fun total(xs: &[i32])` takes any list or array.

A lambda without parameters can use `it`. A lambda as the last argument goes outside the
parentheses (`xs.fold(0) { acc, x -> ... }`).

Lists, arrays and slices have Kotlin's collection operations, each returning a new collection:
`map`, `filter`, `filter_map`, `flat_map`, `sorted`, `sorted_by`, `reversed`, `distinct`,
`group_by`, `associate_by`, `partition`, `chunked`, `sum`, `sum_of`, `max_of`, `count`, `any`,
`all`, `none`, `find`, `join_to_string` and more (Rust names, so `sumOf` is `sum_of`). They
compile to the iterator chain you would write by hand, such as `iter().map(..).collect()`.
For longer pipelines, Rust's iterators are there too: `nums.iter().filter { it > 3 }.map { it * 10 }`.

```oxie
data class User(name: String, age: u32, city: String)

fun main() {
    val users: [User] = [User("Ada", 36, "London"), User("Linus", 28, "Helsinki"), User("Grace", 45, "London")]
    println(users.filter { it.age > 30 }.map { it.name.clone() }.join_to_string(", "))
    println("total age ${users.sum_of { it.age }}, oldest ${users.max_by_key { it.age }?.name ?: "-"}")
    println(users.sorted_by { it.age }.join_with(" < ") { it.name.as_str() })
    val by_city = users.group_by { it.city.clone() }
    println("London: ${by_city["London"].len()}")
}
```

In `filter`, `find`, `any`, `count` and the other predicates, the element is a reference, as in
Rust; comparing it with a literal (`it > 3`, `it == 'a'`, `it % 2 == 0`) reads the value for
you.

## Null safety

`T?` is `Option<T>`; `null` is `None`.

```oxie
data class Address(city: String)
data class Person(name: String, address: Address?)

fun find(people: [Person], name: String): &Person? = people.find { it.name == name }

fun main() {
    val people = listOf(Person("Ada", Address("London")), Person("Bob", null))

    val city = find(people, "Ada")?.address?.city   // safe navigation
    println("${city:?}")

    val len = find(people, "Zed")?.name?.len() ?: 0  // elvis: default when null
    println(len)

    if (val p = find(&people, "Bob")) {             // unwraps (Rust `if let Some(p)`)
        println("found ${p.name}")
    }
    find(&people, "Ada")?.let { println("hello ${it.name}") }

    var x: i32? = null
    println(x.is_none())
    x = 5                                           // wrapped in Some automatically
    println("${x!! + 1}")                           // `!!` unwraps (panics on null)
}
```

`a ?: b` evaluates `b` only when `a` is null (or an error). The right side can also be
`return`, `throw`, `break` or `continue`.

## Classes

```oxie
class Counter(var count: i32 = 0, step: i32 = 1) {
    var history: [i32] = []

    fun increment() {              // changes fields, so it takes `&mut self` automatically
        count += step
        history.push(count)
    }

    fun current(): i32 = count     // only reads: `&self`

    companion object {
        fun starting_at(n: i32): Counter = Counter(n)   // static
    }
}

data class Point(x: f64, y: f64) {
    fun distance_to(other: Point): f64 = ((x - other.x).powi(2) + (y - other.y).powi(2)).sqrt()
}

fun main() {
    var c = Counter(step = 5)
    c.increment()
    c.increment()
    println("${c.current()} ${c.history:?}")
    println(Counter.starting_at(10).current())

    val p = Point(0.0, 0.0)
    val q = p.copy(x = 3.0, y = 4.0)
    println("${q:?} ${p.distance_to(q)} ${p == Point(0.0, 0.0)}")
}
```

* Constructor parameters are fields. Call the constructor like a function: `Counter()`.
* Inside methods, fields and methods are used without `this.` (write `this.x` when a parameter
  has the same name).
* Oxie decides how each method receives the object: `&self` if it only reads, `&mut self` if it
  assigns a field or calls something that does, and static if it never uses the object and
  returns `Self`. Write `mut fun`, `static fun` or an explicit `self` parameter to choose.
* `data class` derives `Debug`, `Clone`, `PartialEq` — plus `Copy`, `Eq` and `Hash` when all
  fields allow it — and has `copy(field = value)`.
* `init { }` runs after the fields are set; body properties need a default value.

### Shared classes

A plain class value has one owner, like every Rust value. When several parts of a program need
the *same* object — a graph node, an account two customers share, application state — declare
it `shared`. Copying a shared value copies a handle to the object, as in Kotlin, Java or
TypeScript:

```oxie
shared class Account(var owner: String, var balance: i64 = 0) {
    var history: [i64] = []

    fun deposit(amount: i64) {
        balance += amount
        history.push(amount)
    }
}

fun main() {
    val a = Account("Ada")
    val b = a                  // the same account
    b.deposit(50)
    a.balance += 5             // fields can be read and changed through any handle
    a.history.push(5)
    println("${a.balance} ${b.history:?} ${a == b}")
}
```

* A shared class is a `Rc<RefCell<..>>` handle; `@sync shared class` uses `Arc<Mutex<..>>` so
  handles can be sent to other threads.
* Methods and handles never need `var`: changes go to the shared object.
* `==` compares identity (the same object), and handles work as `HashMap` keys.
* Reading a whole field (`val h = a.history`) copies it; `a.history.len()` and
  `a.history.push(x)` work on the object itself.
* Each method call borrows the object once for the whole call, so methods can call each
  other freely, and `this` passed from a method (`child.parent = this`) is a handle to the
  object. A method that reaches the *same* object again through another handle while it runs
  (a node whose `next` is itself, a callback that reads it) panics at that line with an
  explanation, instead of corrupting memory or, for `@sync`, waiting forever.

A lambda that should keep its own handle lists it in a *capture list*: `{ [hits] -> ... }`
clones the handle into the lambda (and makes it `move`), which is how threads and callbacks get
one:

```oxie
@sync shared class Stats(var hits: i32 = 0)

fun main() {
    val stats = Stats()
    val workers = (1..=3).map { n -> std.thread.spawn { [stats] -> stats.hits += n } }.to_list()
    for (w in workers) w.join().unwrap()
    println(stats.hits)   // 6
}
```

## Enums

```oxie
enum Shape {
    Circle(radius: f64)
    Rect(w: f64, h: f64)
    Empty

    fun area(): f64 = when (this) {
        Circle(r) -> 3.14159 * r * r      // variants can be used without `Shape.`
        Rect(w, h) -> w * h
        Empty -> 0.0
    }
}

enum Direction { North, East, South, West }

fun main() {
    val shapes = listOf(Shape.Circle(1.0), Shape.Rect(2.0, 3.0), Shape.Empty)
    for (s in shapes) println("${s:?} has area ${s.area():.2}")
    println(Direction.North == Direction.East)
}
```

Variants can carry named fields `Circle(radius: f64)`, positional fields `Pair(i32, i32)`, or
nothing. Enums derive `Debug`, `Clone`, `PartialEq` (and `Copy`/`Eq`/`Hash` when possible).

## Interfaces

```oxie
interface Animal {
    fun name(): String
    fun sound(): String
    fun speak(): String = "${name()} says ${sound()}"    // default method
}

class Dog : Animal {
    override fun name() = "Dog"
    override fun sound() = "woof"
}

class Cat(name: String) : Animal {
    override fun name() = name.clone()
    override fun sound() = "meow"
}

fun introduce(a: &Animal) = println(a.speak())           // `&dyn Animal`

fun main() {
    val zoo: [Animal] = [Dog(), Cat("Tom")]
    for (a in zoo) introduce(a)                           // lends the boxed value
}
```

An interface is a Rust trait. Used as a type it becomes a trait object behind a reference or
`Box` (`&Animal` → `&dyn Animal`), or `impl Animal` for a parameter passed by value. `: Display`
or any other trait works the same way; for derivable traits (`: Hash`, `: Ord`, ...) Oxie adds
a derive. Rust's `impl Trait for Type { }` blocks work too.

## Extension functions and operators

```oxie
fun str.shout(): String = self.to_uppercase() + "!"
fun i32.is_even(): bool = this % 2 == 0

data class Vec2(x: f64, y: f64) {
    operator fun plus(o: Vec2): Vec2 = Vec2(x + o.x, y + o.y)
    operator fun times(k: f64): Vec2 = Vec2(x * k, y * k)
    override fun toString() = "($x, $y)"                  // implements Display
}

fun main() {
    println("hey".shout())
    println(4.is_even())
    println(Vec2(1.0, 2.0) + Vec2(3.0, 4.0) * 2.0)
}
```

Operator names follow Kotlin: `plus`, `minus`, `times`, `div`, `rem`, `unaryMinus`, `not`,
`plusAssign`, ... (Rust names like `add` work too).

## Errors

```oxie
error enum ConfigError {
    Missing(key: String) = "missing key: $key"
    Invalid(key: String, value: String) = "invalid value '$value' for $key"
}

fun port(text: String): u16 throws ConfigError {
    if (text.is_empty()) throw ConfigError.Missing("port")
    text.parse().map_err { ConfigError.Invalid("port", text) }?
}

fun main() {
    println(port("8080").unwrap())
    when (val r = port("eighty")) {
        Ok(p) -> println("port $p")
        Err(e) -> println("error: $e")
    }
    val fallback = port("") ?: 80
    println(fallback)
}
```

`throws` turns the return type into a `Result`; returned values are wrapped in `Ok`, `throw`
returns an error, and `?` propagates one. Read [Error handling](errors.md) for `try`/`catch`,
`context`, `AnyError` and `main() throws`.

## Modules and visibility

Every `.oxi` file in `src/` is a module named after the file (`src/net/http.oxi` is
`net::http`). Import with `import`:

```oxie
import std.collections.{HashMap, HashSet}
import std::fs                    // Rust's `::` works too
import { Deserialize, Serialize } from serde
import model.user.User            // your own module
```

Declarations are visible in the whole project by default. Mark them `private` to keep them in
their file, or `pub` to export them from a library. Members of a `pub` class are `pub` too.

## Ownership

Oxie keeps Rust's ownership model, so `&`, `&mut`, moves and `clone()` work exactly as in Rust:

```oxie
fun total(xs: &[i32]): i32 = xs.iter().sum()
fun add_one(xs: &mut [i32]) = xs.push(1)

fun main() {
    var v: [i32] = [1, 2, 3]
    add_one(v)            // the parameter is `&mut`, so `v` is lent mutably: add_one(&mut v)
    println(total(v))     // ... and here lent for reading: total(&v)
    val w = v.clone()     // explicit copy
    val moved = v         // `v` can't be used after this
    println("${w:?} ${moved:?}")
}
```

`&[i32]` is Rust's slice: any list or array, read-only. `&mut [i32]` can change the elements of
any list or array. When a function grows it (`push`, `insert`, `clear`, ...), or passes it on to
a function that does, Oxie makes the parameter `&mut Vec<i32>`, the caller's list, since a slice
can't change its length.

What Oxie removes is the bookkeeping around it:

* arguments for `&T` and `&mut T` parameters of Oxie functions are borrowed for you (writing
  `&v` yourself works too);
* a `String` or list parameter that a function or method only reads becomes `&str` or `&[T]`,
  so `greet(name)` lends `name` instead of giving it away (for a list only when dropping its
  elements just frees memory: a list of mutex guards is still dropped, and unlocked, when the
  function returns). A map or set becomes `&HashMap<K, V>`, a `PathBuf` `&Path`, and a class or
  `Box<T>` parameter `&T` the same way (below). A library's `pub` functions and methods keep the
  types they declare, since other crates call them;
* lifetimes are filled in when Rust can't infer them (a function returning a reference gets one
  lifetime tied to its reference parameters, a class holding a `&str` gets a lifetime
  parameter, and so does a reference inside an `impl Trait` parameter);
* `&self`/`&mut self` are inferred, and closures that modify captured variables are declared
  `mut` for you;
* `for (x in xs)` borrows `xs` when it is used after the loop, so iterating doesn't use up the
  list, and borrows it mutably when the body changes the elements (`x.qty += 1`);
* a call on a field whose argument changes the same object runs that argument first (below);
* a loop over one field can call a method that changes other fields (below);
* trait bounds on type parameters are inferred from the body: `fun show<T>(x: T) = "<$x>"`
  becomes `fn show<T>(x: T) -> String where T: Display`.

A function that only reads a value of a class, through its fields and methods that take
`&self`, borrows it, so the caller can go on using it:

```oxie
data class User(val name: String, val age: i32)

// fn describe(u: &User) -> String, called as describe(&ada)
fun describe(u: User): String = "${u.name} (${u.age})"

fun main() {
    val ada = User("Ada", 36)
    println(describe(ada))
    println(ada.name)    // still `ada`'s: `describe` only borrowed it
}
```

Rust rejects `self.items.push(self.next_id())` when `next_id` changes `self`: it borrows
`items` for the call before it runs the argument. Oxie computes such an argument first, as you
would by hand in Rust:

```oxie
class Ids {
    var items: [usize] = []
    var next: usize = 0

    fun next_id(): usize {
        next += 1
        return next
    }

    // let next_id = self.next_id(); self.items.push(next_id);
    fun add() = items.push(next_id())
}

fun main() {
    var ids = Ids()
    ids.add()
    ids.add()
    println("${ids.items:?}")
}
```

It does this only where the new order can't be noticed, so a statement keeps doing what it says.

Rust also rejects `self.note(item)` inside `for item in &self.items` when `note` changes
`self`: the loop borrows `items`, and the call borrows all of `self`. When `note` uses only
other fields, Oxie gives it those fields instead of `self`, which is what you would write by
hand:

```oxie
class Inventory(var items: [String]) {
    var count = 0

    // fn note(item: &str, count: &mut i32) { *count += 1; ... }
    fun note(item: String) {
        count += 1
        println("$count: $item")
    }

    // Self::note(&item, &mut self.count)
    fun scan() {
        for (item in items) { note(item) }
    }
}

fun main() {
    var inv = Inventory(["apple", "pear"])
    inv.scan()
}
```

A method that changes the list the loop goes over stays a method, and the error stays: there,
the loop really would see the list change.

When the borrow checker still says no, the error is explained in Oxie terms, with fixes. Here
`register` stores the name in a list, so it has to own it:

```
error[E0382]: `name` was moved, so it can't be used anymore
 --> main.oxi:7:30
  |
6 |     register(names, name)
  |                     ---- `name` moved into `register()` here
7 |     println("${names.len()} $name")
  |                              ^^^^ used here after the move
  = help: giving a value to a function moves it: `register` keeps it, and `name` can't be used
    afterwards. Pass a copy with `name.clone()`.
  = fix: pass a copy of `name`: `register(names, name.clone())`
```

In the editor the fixes are quick fixes (code actions).

## Tests

```oxie
fun add(a: i32, b: i32) = a + b

test "adds numbers" {
    assert_eq(add(2, 2), 4)
}

test "parsing fails on letters" throws {
    val r = "x".parse<i32>()
    assert(r.is_err())
}

fun main() = println(add(1, 1))
```

`oxie test` runs them (`oxie test "adds numbers"` runs one).

## Async

```oxie
// dep: tokio = { version = "1", features = ["full"] }
import std.time.Duration

async fun fetch(id: u32): String {
    tokio.time.sleep(Duration.from_millis(10)).await
    "item $id"
}

@tokio::main
async fun main() {
    val a = fetch(1)
    val b = fetch(2)
    val (x, y) = tokio::join!(a, b)
    println("$x, $y")
    println(await fetch(3))                       // TypeScript-style `await` works too
}
```

## Rust inside Oxie

Any Rust syntax Oxie doesn't cover is still available:

```oxie
macro_rules! square {
    ($x:expr) => { $x * $x };
}

rust {
    fn rust_only(x: u32) -> u32 {
        x.rotate_left(3)
    }
}

fun main() {
    println("${square!(5)} ${rust_only(1)}")
    println!("plain Rust macro: {}", 1 + 1)
}
```

Next: the [language reference](reference.md) lists every construct with the Rust it compiles to.
