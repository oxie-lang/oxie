//! Use from `build.rs` to compile the crate's `.oxi` files:
//!
//! ```no_run
//! fn main() {
//!     oxie_build::compile();
//! }
//! ```
//!
//! Then include the generated code from `src/main.rs` (or `src/lib.rs`):
//!
//! ```ignore
//! include!(concat!(env!("OUT_DIR"), "/oxie/main.rs"));
//! ```
//!
//! For a Rust crate that only has some modules written in Oxie, include `/oxie/mod.rs` instead.

use std::path::{Path, PathBuf};

use oxie_compiler::{compile::extern_crates_from_manifest, compile::inner_attrs_of, compile::write_output, diag, Options, Severity};

/// Compiles every `.oxi` file under `src/`.
pub fn compile() {
    compile_dir("src");
}

/// Compiles every `.oxi` file under `dir` (relative to the crate root).
pub fn compile_dir(dir: &str) {
    let manifest_dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("oxie_build::compile must run from build.rs"));
    let out_dir = PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR not set")).join("oxie");
    let src = manifest_dir.join(dir);
    println!("cargo:rerun-if-changed={}", src.display());
    let inputs = match oxie_compiler::discover(&src) {
        Ok(i) => i,
        Err(e) => {
            eprintln!("error: cannot read {}: {}", src.display(), e);
            std::process::exit(1);
        }
    };
    for i in &inputs {
        println!("cargo:rerun-if-changed={}", i.abs_path.display());
    }
    run(inputs, &manifest_dir, &out_dir, false);
}

/// Compiles a single script file as the crate root (used by `oxie run file.oxi`).
pub fn compile_script(path: &str) {
    let manifest_dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("oxie_build::compile_script must run from build.rs"));
    let out_dir = PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR not set")).join("oxie");
    println!("cargo:rerun-if-changed={}", path);
    let abs_path = PathBuf::from(path);
    let text = match std::fs::read_to_string(&abs_path) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("error: cannot read {}: {}", path, e);
            std::process::exit(1);
        }
    };
    let inputs = vec![oxie_compiler::Input { rel_path: PathBuf::from("main.oxi"), abs_path, text }];
    run(inputs, &manifest_dir, &out_dir, true);
}

fn run(inputs: Vec<oxie_compiler::Input>, manifest_dir: &Path, out_dir: &Path, script: bool) {
    let manifest = std::fs::read_to_string(manifest_dir.join("Cargo.toml")).unwrap_or_default();
    // `#![no_std]` in the Rust root that includes the generated code
    let root_attrs = ["src/lib.rs", "src/main.rs"].iter().filter_map(|f| std::fs::read_to_string(manifest_dir.join(f)).ok()).flat_map(|t| inner_attrs_of(&t)).collect();
    let opts = Options {
        crate_name: std::env::var("CARGO_PKG_NAME").unwrap_or_else(|_| "main".into()),
        extern_crates: extern_crates_from_manifest(&manifest),
        out_dir: out_dir.to_path_buf(),
        script,
        root_attrs,
    };
    let output = oxie_compiler::compile(inputs, &opts);
    let mut failed = false;
    for (path, text, diags) in &output.diags {
        let shown = relative(path, manifest_dir);
        for d in diags {
            let rendered = diag::render(d, &shown, text, false);
            match d.severity {
                Severity::Error => {
                    failed = true;
                    eprintln!("{}", rendered);
                }
                Severity::Warning => {
                    for line in rendered.lines() {
                        println!("cargo:warning={}", line);
                    }
                }
            }
        }
    }
    if failed {
        eprintln!("error: could not compile .oxi sources");
        std::process::exit(1);
    }
    if let Err(e) = write_output(&output, out_dir) {
        eprintln!("error: cannot write generated code to {}: {}", out_dir.display(), e);
        std::process::exit(1);
    }
}

fn relative(path: &Path, base: &Path) -> String {
    path.strip_prefix(base).unwrap_or(path).display().to_string()
}
