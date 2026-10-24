//! Compiler for the Oxie programming language, which compiles to plain Rust.
//!
//! The compiler is a source-to-source translator. Every construct lowers to the Rust you
//! would write by hand, so programs keep Rust's performance and use any crate directly.

pub mod ast;
pub mod bounds;
pub mod compile;
pub mod contexts;
pub mod diag;
pub mod emit;
pub mod fields;
pub mod format;
pub mod infer;
pub mod lexer;
pub mod lower;
pub mod lower_expr;
pub mod params;
pub mod parser;
pub mod runtime;
pub mod sema;
pub mod symbols;
pub mod visit;

pub use compile::{compile, compile_one, discover, Input, Options, Output, SourceMap};
pub use diag::{Diag, LineIndex, Severity};
