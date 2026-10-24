# Performance

Oxie programs perform exactly like the equivalent hand-written Rust, because they *are* that
Rust: the Oxie compiler produces ordinary Rust source, and `rustc`/LLVM compile it.

## Why there is no overhead

* **No runtime.** The generated code uses only the standard library and your dependencies.
  The small support module (`__oxie`) holds a type alias, `#[inline(always)]` helpers for `?:`,
  `.context()` and the error printer for `main() throws`.
* **No hidden allocations, clones or boxing.** Every construct maps to the code you'd write:
  `data class` → struct + derives; `when` → `match`; templates → `format!` (spliced directly
  into `println!` and friends); `?.` → `Option::map`/`and_then`; `throws` → `Result`;
  `try/catch` → a labeled block; lambdas → closures; extension functions → traits (static
  dispatch); `x in a..b` → two comparisons.
* **Values keep Rust's representation.** `T?` is `Option<T>` (niche-optimized), classes are
  plain structs, enums are Rust enums, generics are monomorphized.
* **Allocation only where you'd allocate anyway.** The implicit conversions Oxie adds where Rust
  would reject the code (a `&str` passed as a `String`, a lambda stored in a field) allocate
  exactly like the `.to_string()` / `Box::new` you would otherwise write.
* **Shared objects cost what they say.** A `shared class` is an `Rc<RefCell<..>>` (`@sync`:
  `Arc<Mutex<..>>`) handle, so each method call or field access checks its borrow (or takes the
  lock, and notes it so that a method reaching its own object again panics instead of
  deadlocking). Reading a whole field from outside (`val h = a.history`) copies it;
  `a.history.len()` reads it in place.
* **Optimizer-friendly defaults.** Declarations are `pub(crate)` unless marked `pub`, so the
  compiler can inline and specialize them as freely as private Rust functions.

## Benchmarks

[`bench/`](../bench) implements seven workloads twice — idiomatic Oxie (`bench/src/lib.oxi`)
and idiomatic Rust (`bench/src/rust_impl.rs`) — and times them with identical inputs, checking
that both produce the same result:

```bash
cargo run --release -p oxie-bench
```

Results (Apple Silicon, median of 15 runs, `--release`):

| workload | what it exercises | Oxie | Rust | ratio |
|---|---|---|---|---|
| n-body | `data class` with `var` fields, float math, nested loops | 5.28 ms | 5.28 ms | 0.999 |
| word frequency | strings, `HashMap`, iterators | 11.34 ms | 11.51 ms | 0.986 |
| prime sieve | `Vec<bool>`, `while` loops, indexing | 4.97 ms | 4.99 ms | 0.997 |
| dynamic dispatch | interfaces, `Box<Shape>`, `override` | 20.55 ms | 20.69 ms | 0.993 |
| error handling | `error class`, `throws`, `throw`, `?`, `?:` | 16.41 ms | 16.25 ms | 1.010 |
| recursion | expression-bodied recursive function | 2.49 ms | 2.50 ms | 0.994 |
| sorting structs | `data class`, `sort_by_key` with a lambda | 13.55 ms | 13.49 ms | 1.004 |

All differences are within run-to-run noise (±2%).

## Seeing the code

`oxie emit file.oxi` prints the Rust generated for a file. For the n-body inner loop it is
identical, token for token, to the hand-written version.

## Build times

Translation takes a few milliseconds per file; build time is dominated by `rustc`, as for any
Rust crate. The first build of a project also compiles the `oxie-build` helper (a few seconds),
after which Cargo caches it.
