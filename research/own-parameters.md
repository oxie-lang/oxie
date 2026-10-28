# Parameters borrow unless they say `own`

*Draft, 2026-10-27. Phase 1 is built on the branch `worktree-own-parameters`, not merged; see
"Phase 1, as built" at the end. The numbers in the draft come from `oxie emit --annotate` on
0116dc0 (0.5.0).*

## The rule

A parameter is borrowed. A function keeps a parameter only when it says `own`:

```oxie
data class User(name: String, var age: i32)

fun describe(u: User): String = "${u.name} (${u.age})"   // borrows: the caller keeps `u`
fun birthday(u: &mut User) = u.age += 1                   // changes the caller's `u`

class Registry(var users: [User] = []) {
    fun register(own u: User) = users.push(u)             // keeps it: the caller gives it up
}
```

Whether a call keeps your value is written in the signature, and only there: not in the body,
not in `pub`, not in other calls somewhere in the crate.

## Why

Today Oxie decides from the body whether a parameter written `u: User` is lent (`&User`) or
taken. That inference saves more `&`s than any other, but its decision also depends on code
outside the function. The review of 2026-10-27 found:

* An edit in another function changes a signature. If Oxie can't resolve one call to a method,
  anywhere in the crate, it stops lending for every method of that name (`Lowerer::unresolved`
  is keyed by name). Using a function as a value (`xs.map(shout)`) does the same, even in code
  nobody calls. The error then appears at calls nobody touched, and `--annotate` gives no
  reason.
* `pub` in `lib.oxi` changes it: a `pub` function and an identical private one behave
  differently.
* Plain reads keep the value: `u == other`, `t in items`.
* With automatic lifetimes, a parameter declared `name: String` can end up holding the result's
  lifetime (`fun find(people: [Person], name: String): &Person?`).

The rule removes all four. The body can only agree with the signature, or fail with an error in
that function. The reference's "Borrowed parameters" paragraph (about 30 lines of conditions:
who calls it, `pub`, mixed crates, drop) shrinks to the table below.

Swift, Mojo and Hylo, which bring ownership to a friendlier syntax, chose the same: parameters
are borrowed by default and taking ownership is written (`consuming`, owned arguments, `sink`).
None of them infers it from the body, and neither does Rust.

## What a parameter compiles to

| Oxie parameter | Rust | Call `f(x)` |
|---|---|---|
| `s: String` | `s: &str` | `f(&x)`; a literal is passed as it is |
| `xs: [T]` | `xs: &[T]` | `f(&x)` |
| `m: [K: V]`, `HashSet<T>`, `BTreeMap<K, V>` | `&HashMap<K, V>`, ... | `f(&x)` |
| `p: PathBuf` | `p: &Path` | `f(&x)` |
| `u: User` (a class), `e: Expr` (an enum), `b: Box<T>` | `&User`, `&Expr`, `&T` | `f(&x)` |
| `x: T` (a type parameter) | `x: &T`, or `x: T` when `T: Copy` | `f(&x)` |
| `s: Shape` (an interface) | `s: &impl Shape` | `f(&x)` |
| `s: String?`, `u: User?` | `Option<&str>`, `Option<&User>` | `f(x.as_deref())`, `f(x.as_ref())`, `f(null)` |
| `own x: T` | `x: T` (`mut x` when the function changes it) | `f(x)`: `x` moves |
| `x: &mut T` | `x: &mut T` | `f(&mut x)` |
| `x: &T` | `x: &T` | the same as `x: T`; still accepted, as a Rust habit |

These stay values, because copying is what they mean:

* `Copy` types: numbers, `bool`, `char`, `Copy` data classes and enums, references, and tuples,
  arrays and optionals of them;
* `shared class` handles: a call copies the handle, as `val b = a` does, so a function can store
  one without `own`;
* function types, `(A) -> R`: an `impl Fn` value, as today.

Rust fixes these signatures:

* constructor parameters are fields, so they are owned;
* `operator fun`s and std traits (`plus`, `compareTo`, `toString`, ...) take what Rust's trait
  takes;
* an interface method's parameters are the interface's; an implementation writes the same
  `own`s, or gets an error there.

## Inside the function

* Reading works as today.
* Keeping a borrowed parameter is an error at that line, with two fixes: `own`, or a copy
  (`.clone()`). Keeping covers storing it, returning it, passing it to an `own` parameter or a
  constructor, moving it into a `move` lambda, and taking a field out of it.
* **Text** (decision 1): storing a borrowed `String` copies it, as Oxie already does with any
  `&str` (`names.push(s)` → `names.push(s.to_string())`). `own` avoids the copy.
* `.clone()` of a borrowed parameter gives an owned copy, whatever its kind: `to_string()`,
  `to_vec()` and `to_path_buf()` for text, lists and paths. Rust's `clone` of a `&str` would
  only copy the reference.
* Changing a borrowed parameter is an error with two fixes: `&mut` (change the caller's value)
  or `own` (change the function's own).
* An `own` parameter belongs to the function, so the function may change it (`mut` in the
  Rust). Today that takes `var xs: [T]`.

## At the call

* Oxie writes `&` (or `&mut`) for borrowed parameters, as it does today for lent ones.
  Temporaries and literals work: `describe(make_user())`, `greet("ada")`.
* An `own` parameter moves the argument. A later use is the usual error, which now names the
  parameter: "`register` takes `u` (`own u`)".
* Where Oxie can't tell which function a call reaches (an untyped lambda parameter, a method of
  a Rust type with the same name), it can't write the `&`. The error is at that call ("write
  `&n`, or give `g` a type"); no signature changes somewhere else.
* A function used as a value lends through a lambda: `xs.map(shout)` → `.map(|x| shout(&x))`.
  Rust's deref coercion turns both `&String` and `&&String` into `&str`, so this works for
  `iter()` and `into_iter()` alike.
* Rust code that calls an Oxie function sees `&T` for a borrowed parameter. The signature follows
  the source in programs, libraries and mixed crates alike.

## Other features

* **Libraries.** One rule for `pub` and private functions. Adding or removing `own` is a visible
  API change.
* **async.** A borrowed parameter makes the future borrow. That's fine with `await`, but a
  spawned call (`tokio.spawn(fetch(name))`) needs `own`, and the error says so.
* **Lambdas that outlive the call** (spawned, stored, returned) can't capture a borrowed
  parameter: `own`, or a capture list that copies it (`{ [name] -> ... }`).
* **Drop.** The caller drops a borrowed list of guards or files; `own` drops it in the function.
  Today's special rule for such lists goes away.
* **Lifetimes.** More parameters become references, so the rule for returned references matters
  more. Do the review's fix together with this one: tie a returned reference to the parameter it
  comes from (`people.find { … }` → `people`), not to every reference parameter.
* **Receivers** stay inferred, with the review's fixes for interfaces and `pub` library methods.
  Their inference depends only on the method's body and the methods it calls, and their errors
  name the line that decided, so the problems in "Why" don't apply to them.
* **from-rust.** `fn f(x: T)` → `fun f(own x: T)`; `x: &T`, `&str`, `&[T]` → `x: T`, `String`,
  `[T]`; `mut x: T` → `own x: T`.
* **Context parameters** follow the same rule; most are written with `&` or `&mut` anyway.

## Error messages (drafts)

```
error: `add` can't keep `item`: it is borrowed
 --> src/main.oxi:4:39
  |
4 |     fun add(item: Item) = items.push(item)
  |             ----                     ^^^^ kept here
  |             borrowed: the caller keeps its value
  = help: a parameter is borrowed unless it says `own`. A function that keeps one takes it, and its callers give theirs up.
  = fix: take it: `fun add(own item: Item) = items.push(item)`
  = fix: keep a copy: `items.push(item.clone())`
```

```
error[E0382]: `pen` was moved, so it can't be used anymore
  --> src/main.oxi:12:13
   |
11 |     cart.add(pen)
   |              --- `add` takes `pen` here (`own item`)
12 |     println(pen.name)
   |             ^^^ used here after the move
   = fix: pass a copy: `cart.add(pen.clone())`
```

```
error: `n` has to be lent here
  --> src/main.oxi:15:40
   |
15 |     for (g in gs.iter().rev()) g.greet(n)
   |                                        ^ `greet` borrows `name`, but Oxie can't tell the type of `g` here, so it didn't write the `&`
   = fix: lend it: `g.greet(&n)`
```

## What it changes (measured)

**examples/** has 12 files and 94 parameters, leaving out receivers and `main`. Five of them
change. The rest are constructor parameters (42), `Copy` values (18), std trait parameters (12),
parameters lent today (12, which compile to the same Rust afterwards) and declared references
(5).

| Function | Today | With `own` |
|---|---|---|
| bank `Bank.open(id: String, owner: String, policy: MonthlyPolicy)` | all three taken: stored in an `Account` | `own policy`; `id` and `owner` are copied, or take `own` too (decision 1) |
| todo `TodoList.add(title: String)` | taken: stored in a `Task` | copied, or `own title` |
| todo `TodoList.load(path: PathBuf)` | taken, because `path.clone()` needs a `PathBuf` | borrowed; `path.clone()` stores a copy as before |

```oxie
// examples/bank with decision 1 as recommended: `id` and `owner` are copied
fun open(id: String, owner: String, own policy: MonthlyPolicy) {
    accounts[id.clone()] = Account(id, owner, policy = policy)
}

// or, to move all three as today
fun open(own id: String, own owner: String, own policy: MonthlyPolicy) { ... }
```

**examples/, tests/e2e and tests/coverage** have 242 files and 1,420 parameters:

* 82 are lent today: no change.
* 45 are kept owned by today's inference: 25 strings, 9 lists kept for their drop, 6 classes and
  5 others. Of the 9 lists, 5 hold a type parameter `T` and 4 are in the test of that rule (a
  `MutexGuard` and types with their own `Drop`). All 45 need `own`, except that decision 1 copies
  the 25 strings instead. The 9 lists need `own` only to keep their drop where it is; for a list
  of `T`, letting the caller drop it is usually fine.
* 115 non-`Copy` parameters are owned without today's inference reaching them: 33 type parameters,
  26 optionals, 18 interface values and 25 strings and lists that another call, `pub` or Rust
  code keeps owned (the review's surprises, borrowed under the rule). The rest are 5 generated
  setters (internal) and 8 others (enums, a tuple, a `Result`, a `Range`, a function type
  alias). Whether one needs `own` depends on its body: a container's `push(x: T)` does,
  `show(x: T)` doesn't. A prototype would count them.

## Migration

* `oxie migrate` adds `own` where the body keeps a parameter (today's analysis already knows the
  reason). In libraries it also adds `own` to every `pub` parameter that is owned today, so
  published APIs don't change. Behavior stays the same; delete an `own` later to start lending.
* `var xs: [T]` as a parameter keeps working, as `own xs`.
* `writing-oxie.md` gets the old form and the new one ("a function that keeps a parameter says
  `own`"), so agents that learned the old rule correct themselves. The changelog's Upgrading
  section says the same.

## Work

1. Parser and AST: `own` before a parameter name, as a soft keyword (a parameter named `own`
   still works). The formatter, the tree-sitter and VS Code grammars, and the Kuana and Zed
   highlights.
2. `params.rs`: lend every non-`own` parameter of the kinds it lends today, and drop the
   eligibility rules (unresolved names, functions used as values, calls through paths, `pub`,
   mixed crates, drop). `kept_why` becomes the "can't keep" error and its fixes.
3. Lowering: the lambda for functions used as values, and `.clone()` of borrowed text, lists and
   paths. Then the new kinds: optionals (`as_ref`/`as_deref` at calls; passing a `String?` to a
   `&str?` fails today), type parameters, interface values, enums and tuples.
4. `explain.rs`: the messages above. E0382 names `own`, and spawned futures and `'static` lambdas
   suggest it.
5. from-rust, the docs (tour "Ownership", reference "Functions", cheatsheet, performance: the
   text copy), golden files, e2e and diagnostics tests.
6. The review's lifetime fix (tie the result to the parameter it comes from).

Validate with the Kuana acceptance test (translate, build, test; 442/443 today), which also gives
the number of `own`s in a large real program, and with the evals (they cost money: ask first).

## Decisions

1. **Text.** (a) Storing a borrowed `String` copies it, as a `&str` does today. The most common
   Kotlin pattern (`names.push(name)`) then needs no `own`, and 25 of the 45 `own`s above go
   away. The cost is an allocation where today's code moves, which only the rule shows.
   (b) Storing it is an error, as for other types. Nothing allocates behind your back, but most
   adders and setters need `own`, and, to be consistent, a stored `&str` local should stop
   converting too.
   Recommended: (a). Say in performance.md that it copies; `own` is there for hot paths.
2. **Spelling.** `own x: T`, before the name like Kotlin's `vararg`, or `x: own T`, before the
   type like Swift's `consuming`. And the word: `own`, `take`, `keep` or `sink`. Recommended:
   `own x: T`. It reads as a sentence ("add owns item"), and today's errors already say "owns it
   now".
3. **Type parameters.** Borrowed by default, and a value with a `Copy` bound. The alternative,
   values by default, keeps generic code closer to Rust but brings back a second rule.
4. **Optionals.** `String?` borrowed as `Option<&str>`, which callers usually have to build by
   hand in Rust (`as_deref()`), or as `&Option<String>`. Recommended: `Option<&T>`.
5. **Receivers.** They stay inferred, for the reason above. Making them explicit too (`mut fun`,
   like Swift's `mutating`) would complete the picture, but nothing found so far calls for it.

## Phase 1, as built

On the branch `worktree-own-parameters`, with the recommended decisions (text is copied,
`own x: T`):

* `own` before a parameter name (a soft keyword); `var x: T` still takes the value.
* Every parameter of the kinds Oxie lent before (`String`, lists, maps, sets, `PathBuf`, Oxie
  classes, `Box<T>`) is borrowed unless it says `own`, in every function and method, `pub` or
  not, in programs, libraries and mixed crates. The eligibility rules are gone (unresolved
  method names, functions used as values, calls through paths, `pub`, Rust callers, drop), and
  so is the dry run's part in deciding them. Boxed functions (`Box<dyn FnOnce()>`) stay values.
* `check_kept` reports a body that keeps or changes a borrowed parameter, with fixes (`own`, a
  copy, `&mut`). Compiler diagnostics can carry fixes now (`Diag::fixes`), printed as `= fix:`
  and offered by the language server.
* A function or method used as a value lends through a lambda (`|name| greet(&name)`).
* An `own` parameter the body changes gets `mut`; `&name`, `name.as_str()` and `.clone()` of a
  lent parameter are the reference and an owned copy.
* `from-rust` writes `own` for by-value `String`, `Vec`, map, set, `PathBuf`, `Box` and
  non-`Copy` struct parameters.
* E0382 for an `own` parameter says so, and offers to drop `own` when the body only reads it.

The test suite turned up five gaps, fixed on the way: a loop over a borrowed list of a type
parameter or a map that only reads is not an error; `names.contains(&who)` with lent text
compares element by element, as `who in names` does; a lent element compared with a value
(`x > best!!`) is dereferenced; `.unwrap()` has a type (so `m.get_mut(k).unwrap().push(text)`
copies the text); `Self { label }` copies lent text like `Self { label: label }`.

**What it took** (examples/, tests, docs, bench, evals):

| | programs | `own` added |
|---|---|---|
| examples/ | 10 projects | 0 |
| bench/, evals reference solutions | | 0 |
| tests/coverage | 52 | 0 |
| tests/e2e | 178 | 15, in 10 programs |
| tests/emit | 11 | 2 |
| doc examples that run | | 1 (writing-oxie.md) |

The 15 in tests/e2e: 5 in the test of the drop rule (`own` keeps the drop in the function), 3
adders that push, 2 that store or take a field, 2 lambdas that outlive the call, 2 loops that
take the elements, 1 consuming `Box<dyn Iterator>`. Tests and examples written to show a moved
value (tests/diagnostics 11, the language server tests 4, the error examples in the docs 7) got
`own` so they still move: 22 more.

Not done in phase 1: optionals, type parameters, interface values, enums and tuples are still
taken by value; editor grammars (`own` isn't highlighted); `oxie migrate`; the lifetime fix;
the Kuana acceptance test.

## Phase 2, as built

On the same branch, optionals, type parameters, interface values and enums are borrowed unless
they say `own`:

* An enum (not `Copy`, without lifetimes or type parameters) is lent as `&E`, a value of an
  interface of the crate as `&dyn I`, a type parameter as `&T`, and an optional of any lent kind
  as `Option<&T>` (`Option<&str>` for `String?`, `Option<&[T]>` for `[T]?`).
* A type parameter stays a value when it must be `Copy`, by a bound written or inferred from the
  body (`a + b` makes `bounds.rs` infer `Add<Output = T> + Copy`), or is a function
  (`F: Fn(&str) -> usize`): lending a function works, but rustc then can't infer a lambda's
  parameter types (E0282 in the coverage test of higher-ranked bounds).
* An interface value is `&dyn Shape`, not `&impl Shape` as the table above says. Interface values
  are boxed in lists, fields and variables (`Box<dyn Shape>`), and `&impl Shape` (implicitly
  `Sized`) can't take `&**boxed`. Calls pass `&**s`, `&shapes[0]` or `&Square(..)`.
* An optional argument is passed with `as_ref()` (`as_deref()` for text, lists, paths and boxed
  recursive fields), a value as `Some(&x)` (`Some(x)` when it is a reference already), a literal
  as `Some("cy")`, `null` as `None`.
* **Optional text is copied** when a function keeps it, as text is (decision 1, extended):
  `nick = n` stores `n.map(str::to_string)`, so a nullable setter, as common in Kotlin as
  `names.push(name)`, needs no `own`. Without it, `fun set(n: String)` would work and
  `fun set(n: String?)` would be an error. Other optionals are an error when kept.
* An enum or class that derives `Copy` itself (`@derive(Clone, Copy)`, as from-rust writes) is
  copied; phase 1 only knew the derives Oxie adds (`Crate::named_derives`).
* The keep analysis follows the value further: through `?:` (both sides), `!!`, branches and
  list, set, map, tuple and pair literals (`[x]`), into a data class's `copy(y = o)` and an
  enum's case (`Expr.Neg(e)`), and under another name (`val v = u`, the `other ->` of a
  `when (u)`). These were rustc type mismatches before, at the right line but in Rust terms
  (`Option<Named>: From<&mut &Named>`). The copy fix is offered only where `.clone()` makes one:
  not for an optional (it would copy the reference), an interface value or a box.
* from-rust writes `own` for by-value `Option`, `Box` (not boxed functions), generic, and
  non-`Copy` struct and enum parameters of functions and inherent methods.

**What it took**, on top of phase 1:

| | `own` added |
|---|---|
| examples/ | 1 (bank: `open` stores a policy, an interface value) |
| bench/ | 0 |
| evals reference solutions | 1 (generic-stack: `push(own item: T)`) |
| tests/coverage | 1 |
| tests/e2e | 24, in 12 programs |
| tests/emit | 1 |
| doc examples | 0 (one lost a `.clone()` that lending made needless) |

The 24 in tests/e2e by kind: 16 type parameters (generic containers, and `larger(a, b)` that
returns one of them), 5 interface values, 2 enums (simplifiers that return their argument), 1
optional (a function that changes and returns it). By what the function does: 14 store the
value (`push`, `insert`, a field), 8 return it (4 directly, one each as the default of `?:`, in
a list, under another name and as a `when` arm's binding), 1 gives it to `copy(..)` and 1 to a
function-typed field. Each keeps the value; none needed `own` for another reason.

The cost is in generic code: a container's adders and a function that returns one of its
arguments (`fun larger<T>(own a: T, own b: T): T`) say `own`, where Rust writes `T` and Kotlin
nothing. The error names the fix, and writing-oxie.md shows `fun push(own x: T)`.

**Kuana** (translated whole by `oxie from-rust`, built and tested): no `rust { }` block, 442 of
443 tests pass, as on 0.5.0 (the other one checks which file a panic names). The translation
says `own` 325 times in about 3,200 functions. The first run found two translator gaps, both
fixed: `own` for structs and enums declared in another file of the crate (13 build errors), and
a `Copy` type whose derive Oxie couldn't infer in the file alone (its variant's type came from
`use super::*`), which made its parameters lent and two functions fall back to Rust. The editors
(VS Code, Zed, Kuana) highlight `own`.

Not done in phase 2: tuples are still values; `oxie migrate`; the lifetime fix; the evals (they
cost money: ask first).
