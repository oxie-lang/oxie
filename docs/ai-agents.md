# AI coding agents

Oxie is built to be written by AI coding agents as well as by people. A model has seen little
Oxie, and what it has seen may describe an older version, so Oxie does not rely on what the
model remembers. The installed `oxie` teaches the language, every project tells its agent where
to look, and the compiler explains what the model gets wrong:

* **`oxie guide` teaches the installed version.** It prints the guide to writing Oxie that is
  built into the `oxie` binary: how to work with the compiler, the language at a glance, what
  works differently from Kotlin and from Rust, and the patterns to use. It always matches the
  compiler that runs the code, because it is part of it, and it warns when the project builds
  with another version of `oxie-build`.
* **Every project points its agent at the guide.** `oxie new` and `oxie init` write an
  `AGENTS.md` telling agents to read `oxie guide` and to check their work with `oxie check`, and
  a `CLAUDE.md` that imports it, for Claude Code. There is also a Claude Code plugin that does
  the same for any Oxie work.
* **The compiler explains mistakes in Oxie terms, with the edit.** Errors point at `.oxi` lines,
  including rustc's, which Oxie rewrites: a moved value says where it moved and why, and most
  errors end with a `= fix:` line giving the corrected code.
* **What an agent writes is easy to review.** `oxie emit --annotate` shows the Rust a file
  compiles to, with the reason for every choice Oxie made (a borrow, a clone, a lifetime).

## Set up

Install Oxie (`brew install oxie-lang/tap/oxie`, or [other ways](tooling.md#installing)), then
give your agent a project it can learn from.

**A new project.** `oxie new` makes a Cargo project with everything an agent needs:

```bash
oxie new hello
cd hello
```

It writes `Cargo.toml`, `build.rs`, `src/main.rs`, a `src/main.oxi` to start from, `AGENTS.md`,
and `CLAUDE.md` (one line, `@AGENTS.md`). Then ask your agent for what you want built.

**An existing project.** Run `oxie init` in the project's directory. In an existing Rust crate it
adds Oxie support (`build.rs` and the build-dependency), and `AGENTS.md` and `CLAUDE.md` where the
crate has none, and leaves the rest alone, so the agent can write new code in Oxie next to the
Rust. A `CLAUDE.md` the crate already has is kept; add the line `@AGENTS.md` to it so Claude Code
reads the notes too.

**Claude Code.** Claude Code reads `CLAUDE.md`, and the one Oxie writes imports `AGENTS.md`, so a
project made by `oxie new` or `oxie init` needs nothing more. Install the Oxie plugin as well,
and Claude Code loads its skill for any work on `.oxi` files, in every project, also in ones
without these files:

```bash
claude plugin install oxie --marketplace oxie-lang/oxie
```

Inside Claude Code, the same is `/plugin marketplace add oxie-lang/oxie` followed by
`/plugin install oxie@oxie`. Without the plugin, copy `skills/oxie` to
`~/.claude/skills/oxie`.

**Other agents.** `AGENTS.md` is the convention that Codex, Cursor, GitHub Copilot, and many
other coding agents read from the project's root. For an agent that reads another file, add the
same instruction to it: *run `oxie guide` before writing Oxie, and `oxie check` after each
change.*

## The commands agents use

| Command | What it gives the agent |
|---|---|
| `oxie guide` | The guide to writing the installed version of Oxie, with the version that printed it, and a note when the project builds with another `oxie-build`. |
| `oxie guide topics` | The documentation pages built into `oxie`: the tour, the reference, error handling, the cheat sheet, Rust interop, tooling, performance. |
| `oxie guide <topic>` | One of those pages, such as `oxie guide reference` or `oxie guide interop`. |
| `oxie check` | Type-checks the project without building binaries, with every error at its `.oxi` line and its `= fix:`. Fast enough to run after every change. |
| `oxie run` / `oxie test` | Runs the program or the `test` blocks; panics point at `.oxi` lines. |
| `oxie emit file.oxi --annotate` | The Rust a file compiles to, with the reason for each choice Oxie made, for when the agent (or its reviewer) is unsure what a construct does. |
| `oxie from-rust file.rs` | Rust translated into Oxie, for example code from a crate's documentation. |
| `oxie fmt` | Formats the `.oxi` files. |
| `oxie lsp` | The language server, with the fixes as quick fixes, for agents that work through an editor. |

## What the compiler tells an agent

Oxie follows Rust's rules, so the mistakes a model makes are often Rust's: a value used after a
function took it, a `val` that is changed later. This function has both:

```oxie
fun register(names: &mut Vec<String>, name: String) = names.push(name)

fun sign_up(names: &mut Vec<String>, name: String) {
    register(names, name)
    val count = names.len()
    count += 1
    println("welcome, $name, member $count")
}
```

`oxie check` reports each one at its line, says why in Oxie terms, and gives the fix:

```text
error[E0384]: `count` is a `val`, so it can't be changed
 --> src/main.oxi:6:5
  |
5 |     val count = names.len()
  |     ----- declared with `val` here
6 |     count += 1
  |     ^^^^^ changed here
  = help: declare it with `var` to allow changes: `var count = ...`.
  = fix: declare `count` with `var`: `var count = names.len()`
  = rustc: cannot assign twice to immutable variable `count`

error[E0382]: `name` was moved, so it can't be used anymore
 --> src/main.oxi:7:24
  |
1 | fun register(names: &mut Vec<String>, name: String) = names.push(name)
  |                                                                  ---- `register` keeps it: it passes `name` to `.push()`
...
4 |     register(names, name)
  |                     ---- `name` moved into `register()` here
...
7 |     println("welcome, $name, member $count")
  |                        ^^^^ used here after the move
  = help: giving a value to a function moves it: `register` keeps it (it passes `name` to `.push()`), and `name` can't be used afterwards. Pass a copy with `name.clone()`.
  = fix: pass a copy of `name`: `register(names, name.clone())`
  = rustc: borrow of moved value: `name`
```

With both fixes the program builds and runs:

```oxie
fun register(names: &mut Vec<String>, name: String) = names.push(name)

fun sign_up(names: &mut Vec<String>, name: String) {
    register(names, name.clone())
    var count = names.len()
    count += 1
    println("welcome, $name, member $count")
}

fun main() {
    var names: [String] = ["bob"]
    sign_up(names, "ada")
}
```

A fix is the edit that makes the code mean what it was written to mean; where there is a choice
(borrow the value or copy it), the explanation says which is which, so the agent can decide.
The errors Oxie explains, and their fixes, are listed under
[diagnostics](tooling.md#diagnostics).

## When Oxie changes

Oxie's syntax changes between releases. Three things keep agents current:

* `AGENTS.md`, `CLAUDE.md`, and the Claude Code skill hold no language rules, only commands, so
  they never go stale. Updating `oxie` updates what every agent learns; the test suite keeps Oxie
  code out of them.
* `oxie guide` names the version that printed it, and when a project's `Cargo.lock` has another
  `oxie-build`, it says so first, with the command that makes the two match.
* When a release changes syntax, the guide shows the old form and what to write instead, so an
  agent that learned the old one corrects itself.

## Measured

[`evals/`](../evals) measures how well models write Oxie: it gives 16 programming tasks to
Claude Code, from error enums and generics to shared objects, porting Rust, and fixing a build,
and grades the programs it writes. It compares four conditions: with the Claude Code skill, with
the `AGENTS.md` and `CLAUDE.md` that `oxie new` writes, with only `oxie --help`, and with no help at all, and
reports passed tasks, first builds that succeed, builds and turns per run, and the Rust written
where Oxie has its own form. To run it:

```bash
python3 evals/run.py --reps 3 --conditions skill,agents-md,cli,bare
```

It makes real model calls; the [evals README](../evals/README.md) explains the tasks, the
grading, and the cost.

## See also

* [Writing Oxie](../crates/oxie-cli/guide/writing-oxie.md): the page `oxie guide` prints.
* [Tooling](tooling.md): every command and flag, and the diagnostics in detail.
