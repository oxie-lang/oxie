# Error handling

Oxie uses Rust's error model — errors are values of type `Result<T, E>` — with Kotlin/Swift
style syntax on top. There are no exceptions and no hidden costs: everything below compiles to
ordinary `Result` code.

## `throws`, `throw` and `?`

A function that can fail declares `throws`:

```oxie
fun parse_age(text: &str): u32 throws {
    val age: u32 = text.trim().parse()?          // `?` passes a failure on to the caller
    if (age > 150) throw "age $age is not realistic"
    age                                          // wrapped in Ok automatically
}

fun main() {
    println("${parse_age("42"):?}")
    println("${parse_age("abc"):?}")
    println("${parse_age("200"):?}")
}
```

| Oxie | Rust |
|---|---|
| `fun f(): T throws` | `fn f() -> Result<T, AnyError>` |
| `fun f(): T throws MyError` | `fn f() -> Result<T, MyError>` |
| `fun f() throws` | `fn f() -> Result<(), AnyError>` |
| `return x` / last expression `x` | `return Ok(x)` / `Ok(x)` |
| `throw e` | `return Err(e.into())` |
| `expr?` | `expr?` |

`AnyError` is `Box<dyn std::error::Error + Send + Sync>`: any error type converts into it,
and so do strings, so `throw "message"` and `throw "value $x is bad"` work.

Calling another Oxie function that `throws` from a throwing function propagates its error
automatically when you use the call as a statement or return value — like an exception would:

```oxie
fun check(x: i32) throws {
    if (x < 0) throw "negative: $x"
}

fun check_all(xs: &[i32]) throws {
    for (x in xs) check(*x)              // same as `check(*x)?`
}

fun main() {
    println("${check_all(&[1, 2]):?}")
    println("${check_all(&[1, -2]):?}")
}
```

When you assign the result instead (`val r = check(5)`), you get the `Result` to inspect.

The same goes for Rust calls known to return a `Result`, such as `fs.write(path, text)` or
`out.flush()`: as a statement in a throwing function, their error is passed on rather than
dropped. In a function that doesn't throw, such a statement drops the error, and the compiler
warns about it.

## Error types

`error enum` declares an error type with a message per case. Oxie implements `Display`,
`std::error::Error` and `Debug` for it:

```oxie
import std::num::ParseIntError

error enum ConfigError {
    Missing(key: String) = "missing setting '$key'"
    OutOfRange(key: String, value: i64, max: i64) = "$key = $value is larger than $max"
    BadNumber(ParseIntError)                       // wraps another error
}

fun read_port(text: &str): u16 throws ConfigError {
    if (text.is_empty()) throw ConfigError.Missing("port")
    val n: i64 = text.parse()?                      // ParseIntError converts via From
    if (n > 65535) throw ConfigError.OutOfRange("port", n, 65535)
    n as u16
}

fun main() {
    for (input in listOf("8080", "", "x", "99999")) {
        when (val r = read_port(input)) {
            Ok(port) -> println("port $port")
            Err(e) -> println("error: $e")
        }
    }
}
```

* `= "template"` is the message; `$field` refers to the case's fields and `$0` to positional ones.
* A case with a single field whose type is an error (its name ends in `Error`) gets a `From`
  conversion, so `?` turns that error into this case, and `source()` returns it. Mark a case
  `@from` to get the conversion for other types (`source()` then stays empty unless the wrapped
  type is an error).
* A case without a message shows its wrapped error (and passes on that error's own `source()`),
  its fields' values (`TooMany(count: usize, max: usize)` → "too many (count: 5, max: 3)"), or
  its name in words (`NotFound` → "not found").
* `main() throws` prints each cause once, even when a case shows its wrapped error.

A single error type can be a class:

```oxie
error class TooBig(value: i64, limit: i64) = "$value exceeds the limit of $limit"

fun check(v: i64): i64 throws TooBig {
    if (v > 100) throw TooBig(v, 100)
    v
}

fun main() {
    println("${check(5):?}")
    println("${check(500).unwrap_err()}")
}
```

## `try` / `catch`

`try { }` runs a block in which `?` and `throw` jump to `catch` instead of leaving the
function. So does a throwing call used as a statement or as the block's value, which passes its
error on without `?` as it does in a throwing function:

```oxie
fun main() {
    val total = try {
        val a: i32 = "10".parse()?
        val b: i32 = "x".parse()?
        a + b
    } catch (e) {
        println("could not add: $e")
        0
    }
    println("total = $total")
}
```

* `catch (e)` receives an `AnyError`. Write `catch (e: MyError)` to receive a specific type
  (errors are converted with `From`).
* `try { }` without `catch` evaluates to a `Result`:
  `val r = try { a()? + b()? }`.
* It compiles to a Rust labeled block, so `return`, `break` and `continue` inside it behave
  normally.

## Defaults: `?:`

`?:` gives a value when a `Result` is an error (or an `Option` is `None`):

```oxie
fun main() {
    val n = "abc".parse::<i32>() ?: 0
    val m = "7".parse::<i32>() ?: 0
    println("$n $m")
}
```

The right side can also leave the function: `val user = find(id) ?: throw "user $id not found"`
or `?: return`.

## Adding context

`.context("...")` wraps an error (or a missing `Option` value) with a message, keeping the
original as its cause:

```oxie
import std::fs

fun load(path: &str): String throws {
    val text = fs.read_to_string(path).context("cannot read config '$path'")?
    text
}

fun main() {
    when (val r = load("/no/such/file.toml")) {
        Ok(t) -> println(t)
        Err(e) -> {
            println("error: $e")
            println("cause: ${e.source().unwrap()}")
        }
    }
}
```

`.with_context { "..." }` builds the message only when there is an error.

## `main() throws`

A throwing `main` prints the error and its causes and exits with status 1:

```oxie
import std::fs

fun main() throws {
    val config = fs.read_to_string("/no/such/app.toml").context("loading app.toml")?
    println(config)
}
// exits with an error:
//   error: loading app.toml
//     caused by: No such file or directory (os error 2)
```

## Unrecoverable errors

`panic("message")`, `unreachable()`, `todo()` and `!!` (unwrap) stop the program as in Rust.
Use them for bugs, not for expected failures. Under `oxie run` and `oxie test` the panic
message names the `.oxi` line (`panicked at src/main.oxi:12:5`).

## Using Rust error crates

Everything interoperates with the Rust ecosystem: `thiserror` and `anyhow` work as usual
(`@derive(thiserror::Error)`, `throws anyhow::Error`), and an Oxie `error enum` is a normal Rust
error type that Rust code can match on and convert.
