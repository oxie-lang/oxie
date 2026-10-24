# Coming from Kotlin or TypeScript

Oxie is a programming language that reads like Kotlin with a few TypeScript conveniences and
compiles to Rust. This page maps what you know to Oxie, and lists the Rust ideas you still need.

## Side by side

| | Kotlin | TypeScript | Oxie |
|---|---|---|---|
| immutable | `val x = 1` | `const x = 1` | `val x = 1` |
| mutable | `var x = 1` | `let x = 1` | `var x = 1` |
| function | `fun f(a: Int): Int = a * 2` | `function f(a: number): number` | `fun f(a: i32): i32 = a * 2` |
| default / named args | `f(b = 2)` | — | `f(b = 2)` |
| string template | `"hi $name ${a + b}"` | `` `hi ${name}` `` | `"hi $name ${a + b}"` |
| nullable | `String?` | `string \| null` | `String?` |
| safe call / default | `a?.b ?: c` | `a?.b ?? c` | `a?.b ?: c` |
| non-null assert | `a!!` | `a!` | `a!!` |
| smart cast | `if (a != null) a.b` | `if (a) a.b` | `if (a != null) a.b` (locals and parameters), `if (val a = a) a.b` |
| class | `class P(val x: Int)` | `class P { constructor(public x: number) {} }` | `class P(x: i32)` |
| data class | `data class P(val x: Int)` | — | `data class P(x: i32)` |
| copy | `p.copy(x = 1)` | `{ ...p, x: 1 }` | `p.copy(x = 1)` |
| shared object | any class | any class | `shared class P(var x: i32)` |
| interface | `interface A { fun f(): Int }` | `interface A { f(): number }` | `interface A { fun f(): i32 }` |
| implement | `class B : A { override fun f() = 1 }` | `class B implements A` | `class B : A { override fun f() = 1 }` |
| enum with data | `sealed class` | union types | `enum Shape { Circle(r: f64) }` |
| switch | `when (x) { 1 -> ...; else -> ... }` | `switch` | `when (x) { 1 -> ...; else -> ... }` |
| lambda | `{ x -> x * 2 }`, `{ it * 2 }` | `x => x * 2` | all three styles |
| list | `listOf(1, 2)` | `[1, 2]` | `listOf(1, 2)` / `vec[1, 2]` |
| list operations | `xs.filter { it > 1 }.map { it * 2 }` | `xs.filter(x => x > 1).map(x => x * 2)` | `xs.filter { it > 1 }.map { it * 2 }` |
| sum / sort / group | `sumOf { }`, `sortedBy { }`, `groupBy { }` | `reduce`, `sort`, — | `sum_of { }`, `sorted_by { }`, `group_by { }` |
| map | `mapOf("a" to 1)` | `new Map([["a", 1]])` | `mapOf("a" to 1)` |
| range | `0 until n`, `1..n` | — | `0 until n`, `1..=n` (Rust: `..` excludes the end) |
| loop | `for (i in xs)` | `for (const i of xs)` | `for (i in xs)` |
| exceptions | `throw`, `try/catch` | `throw`, `try/catch` | `throw`, `try/catch` on `Result` values |
| async | `suspend fun` | `async function` / `await x` | `async fun` / `await x` or `x.await` |
| extension | `fun String.shout()` | — | `fun String.shout()` |
| object | `object Config { }` | module | `object Config { }` |
| static | `companion object` | `static` | `companion object` / `static fun` |
| test | JUnit | Jest | `test "name" { assert_eq(a, b) }` |
| import | `import a.b.C` | `import { C } from "a/b"` | both forms |

## What is different from Kotlin and TypeScript

These come from Rust and are what makes Oxie as fast as Rust.

**Values have one owner.** Assigning a `String`, `Vec` or class instance *moves* it; the old
variable can't be used afterwards. A function that only needs to read or change a value takes
a reference (`&T` or `&mut T`); Oxie lends the argument for you (`show(names)` passes
`&names`). A function that only reads a `String`, `Vec`, class or `Box` parameter borrows it, and a `for`
loop over a list you use again afterwards borrows it too. Call `.clone()` when you really want
a copy.

```oxie
fun show(names: &Vec<String>) = println("${names.len()} names")

fun main() {
    var names = vec["ada".to_string()]
    show(&names)              // lend
    names.push("bob".to_string())
    val copy = names.clone()  // explicit copy
    val moved = names         // `names` is moved
    println("${copy:?} ${moved:?}")
}
```

Small value types (numbers, `bool`, `char`, and `data class`es made only of those) are copied
instead of moved.

**Objects aren't shared by default.** In Kotlin and TypeScript every object is a reference that
many variables can point to. In Oxie that's opt-in: declare the class `shared` and copies of a
variable refer to the same object (a reference-counted handle, `Rc<RefCell<..>>`). Use it for
graphs, shared state and observers; plain classes are faster and need no bookkeeping.

```oxie
shared class Player(var name: String, var score: i32 = 0)

fun main() {
    val p = Player("Ada")
    val team = vec[p]       // the list holds the same player
    team[0].score += 10
    println("${p.name}: ${p.score}")
}
```

**Mutability is declared on the variable.** `var c = Counter()` can call methods that change
`c`; `val c = Counter()` can't. Oxie works out which methods change their object.

**Numbers have sizes.** `i32`, `i64`, `u8`, `usize` (for lengths and indexes), `f64`, ... and
don't convert silently: write `x as f64`. (`Int`, `Long`, `Double` are accepted as names for
`i32`, `i64`, `f64`.)

**Two string types.** `&str` is borrowed text (literals, slices); `String` owns its text.
Oxie converts literals and `&str` arguments to `String` where one is required.

**Errors are values.** A failing function returns `Result`; `throws`, `throw`, `?` and
`try`/`catch` make that read like exceptions, but nothing is thrown at runtime and every error
is visible in the function's type. See [Error handling](errors.md).

**`null` is `Option`.** There is no null pointer; `T?` must be unwrapped before use, so
"null pointer exceptions" can't happen.

**Generics are compiled per type.** `fun <T: Display> show(x: T)` produces specialized code for
each type it's used with — no boxing, no runtime type checks. Interfaces used as types
(`&Shape`, `Box<Shape>`) use dynamic dispatch when you want it.

**Ranges.** `a..b` excludes `b` (Kotlin's `a until b`); `a..=b` includes it (Kotlin's `a..b`).

**Iterators are lazy.** `xs.iter().map { ... }` does nothing until something consumes it
(`.collect()`, `.sum()`, `for`). Method names are Rust's: `len()`, `is_empty()`, `push()`,
`iter()`, `collect()`, `to_string()`, `contains()`...

**Equality.** `==` compares values (Kotlin's `==`); there is no reference equality operator.
`data class`es and enums get `==` automatically; for a `class`, add `: PartialEq`.

## Things TypeScript developers should know

* Types are checked at compile time and there is no `any`; `AnyError` is the only "any"-like
  type and it holds errors.
* Objects are classes, not open bags of properties: declare fields in the class.
* `undefined` doesn't exist; optional values are `T?`.
* Arrow functions work: `(a, b) => a + b`, `x => x * 2`. So does prefix `await`.
* Imports: `import { HashMap } from std::collections` works, and so does `import std.fs`.

## Things Rust developers should know

* Your Rust knowledge applies directly: same types, traits, crates, ownership and borrowing.
* Most Rust syntax is accepted (`let`, `match`, `fn`, closures `|x|`, `impl`, `::<T>`,
  `if let`, macros), so you can mix styles while learning.
* Differences: `?.` is Kotlin's safe call on optional values (and error propagation on
  `Result`s); strings use `$name` templates; default visibility is `pub(crate)`.
* `oxie emit file.oxi` shows exactly what your code becomes (`--annotate` marks each statement
  with its `.oxi` line).
