# Examples

Each directory is a complete Oxie project. Run one with `oxie run` inside it (or `oxie test` for
its tests). The test suite runs all of them and compares the output with `expected.txt`.

| Example | Shows |
|---|---|
| [hello](hello) | a quick tour: values, templates, collections, lambdas, null safety |
| [calculator](calculator) | a lexer, recursive-descent parser and evaluator; recursive enums, `error enum`, classes with inferred `&mut self`, `when` |
| [todo](todo) | a command-line todo list stored in a file; CLI arguments, file IO, `companion object`, `throws` + `context` |
| [bank](bank) | interfaces stored as values (trait objects), typed errors, `try`/`catch`, `toString()` with format specs |
| [game_of_life](game_of_life) | 2D grid simulation, `"""` strings, `toString()` → `Display` |
| [word_count](word_count) | reading a file, `HashMap` counting, sorting, a unit test |
| [async_tasks](async_tasks) | tokio: `JoinSet`, channels, `async fun ... throws` |
| [json_api](json_api) | serde: `@derive(Serialize, Deserialize)`, `@serde(...)` attributes, default values |
| [geometry](geometry) | a library written in Oxie with a `pub` API, used from a Rust integration test |
| [mixed](mixed) | a Rust program with a module written in Oxie that calls back into Rust |
