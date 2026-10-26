//! `oxie from-rust <file.rs> [-o out.oxi]`: translates Rust to Oxie.
//! `oxie from-rust <dir> -o <out dir>`: translates every `.rs` file under `dir` as one crate.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

pub fn run(args: &[String]) -> Result<ExitCode, String> {
    let usage = "usage: oxie from-rust <file.rs> [-o out.oxi]\n       oxie from-rust <dir> -o <out dir>";
    let mut input = None;
    let mut output = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "-o" | "--output" => {
                output = Some(args.get(i + 1).ok_or(usage)?.clone());
                i += 1;
            }
            a if a.starts_with('-') && a != "-" => return Err(format!("unknown option `{}` for `oxie from-rust`; its option is `-o` (`--output`)", a)),
            a if input.is_none() => input = Some(a.to_string()),
            a => return Err(format!("unexpected argument `{}`\n{}", a, usage)),
        }
        i += 1;
    }
    let input = input.ok_or(usage)?;
    if Path::new(&input).is_dir() {
        let output = output.ok_or("give the directory to write the translations to: oxie from-rust <dir> -o <out dir>")?;
        return translate_dir(Path::new(&input), Path::new(&output));
    }
    let src = if input == "-" {
        let mut s = String::new();
        std::io::Read::read_to_string(&mut std::io::stdin(), &mut s).map_err(|e| e.to_string())?;
        s
    } else {
        std::fs::read_to_string(&input).map_err(|e| format!("{}: {}", input, e))?
    };
    let name = if input == "-" { "<stdin>" } else { input.as_str() };
    let t = oxie_from_rust::translate(&src).map_err(|e| format!("{}:{}", name, plain_parse_error(&e)))?;
    for w in &t.warnings {
        eprintln!("warning: {}: {}", name, w);
    }
    report_todos(name, &t.oxie, &t.warnings);
    match output {
        Some(o) => std::fs::write(&o, &t.oxie).map_err(|e| format!("{}: {}", o, e))?,
        None => print!("{}", t.oxie),
    }
    Ok(ExitCode::SUCCESS)
}

/// Translates the `.rs` files under `dir` together, so that the contexts of a class are the
/// same in every file that has methods of it, into `.oxi` files at the same places under `out`.
fn translate_dir(dir: &Path, out: &Path) -> Result<ExitCode, String> {
    let mut files = Vec::new();
    collect(dir, &mut files)?;
    files.sort();
    let srcs: Vec<String> = files.iter().map(|f| std::fs::read_to_string(f).map_err(|e| format!("{}: {}", f.display(), e))).collect::<Result<_, _>>()?;
    let refs: Vec<&str> = srcs.iter().map(String::as_str).collect();
    let mut failed = false;
    let translations = oxie_from_rust::translate_crate(&refs);
    let translated: Vec<&PathBuf> = files.iter().zip(&translations).filter(|(_, t)| t.is_ok()).map(|(f, _)| f).collect();
    for (f, t) in files.iter().zip(&translations) {
        let name = f.display();
        match t {
            Ok(t) => {
                for w in &t.warnings {
                    eprintln!("warning: {}: {}", name, w);
                }
                // `mod util` declares `util.rs`, whose translation `util.oxi` Oxie finds itself
                let text = without_translated_mods(&t.oxie, &module_dir(f), &translated);
                report_todos(&name.to_string(), &text, &t.warnings);
                let rel = f.strip_prefix(dir).unwrap_or(f);
                let dest = out.join(rel).with_extension("oxi");
                if let Some(parent) = dest.parent() {
                    std::fs::create_dir_all(parent).map_err(|e| format!("{}: {}", parent.display(), e))?;
                }
                let mut oxie = text;
                // the crate root: Oxie projects compile `main.oxi` from a `main.rs` that includes the
                // generated code, and crate attributes other than lints go there
                if let Some(kind) = rel.to_str().and_then(|r| r.strip_suffix(".rs")).filter(|k| *k == "main" || *k == "lib") {
                    let (attrs, rest) = crate_attrs(&oxie);
                    oxie = rest;
                    let stub = out.join(rel);
                    std::fs::write(&stub, format!("{}{}", attrs, crate::project::root_stub(kind))).map_err(|e| format!("{}: {}", stub.display(), e))?;
                }
                std::fs::write(&dest, &oxie).map_err(|e| format!("{}: {}", dest.display(), e))?;
            }
            Err(e) => {
                eprintln!("error: {}:{}", name, plain_parse_error(e));
                failed = true;
            }
        }
    }
    Ok(if failed { ExitCode::FAILURE } else { ExitCode::SUCCESS })
}

/// The directory of the files that a Rust file's `mod name;` declares: its own for `main.rs`,
/// `lib.rs` and `mod.rs`, else the directory named after it.
fn module_dir(file: &Path) -> PathBuf {
    let parent = file.parent().unwrap_or(Path::new("")).to_path_buf();
    match file.file_stem().and_then(|s| s.to_str()) {
        Some("main" | "lib" | "mod") | None => parent,
        Some(stem) => parent.join(stem),
    }
}

/// The translation without the `mod name` lines (and their TODO) of the modules translated in
/// the same run. A line with attributes (`@cfg(unix) mod unix`, `@macro_use mod macros`) stays,
/// without its TODO: Oxie declares the module with them.
fn without_translated_mods(oxie: &str, dir: &Path, translated: &[&PathBuf]) -> String {
    let lines: Vec<&str> = oxie.lines().collect();
    let mut out: Vec<&str> = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let module = lines[i].strip_prefix("// TODO(from-rust): remove this line if ").and_then(|r| r.split_once(".rs is translated")).map(|(m, _)| m);
        if let Some(m) = module {
            let next = lines.get(i + 1).map(|l| l.trim().trim_start_matches("pub(crate) ").trim_start_matches("pub "));
            let done = [dir.join(format!("{}.rs", m)), dir.join(m).join("mod.rs")].iter().any(|p| translated.iter().any(|t| *t == p));
            let attributed = out.last().is_some_and(|l| {
                let l = l.trim_start();
                l.starts_with('@') || l.starts_with("#[") || l.starts_with("///")
            });
            if done && attributed {
                i += 1;
                continue;
            }
            if done && next == Some(format!("mod {}", m).as_str()) {
                i += 2;
                // and a blank line after them, unless it separates what is before them
                if lines.get(i).is_some_and(|l| l.trim().is_empty()) && out.last().is_none_or(|l| l.trim().is_empty()) {
                    i += 1;
                }
                continue;
            }
        }
        out.push(lines[i]);
        i += 1;
    }
    let mut text = out.join("\n");
    if oxie.ends_with('\n') {
        text.push('\n');
    }
    text
}

/// Reports the `TODO(from-rust)` comments of a translation that its warnings don't already
/// name, with their lines in the translation.
fn report_todos(name: &str, oxie: &str, warnings: &[String]) {
    for (i, line) in oxie.lines().enumerate() {
        let Some((_, todo)) = line.split_once("// TODO(from-rust): ") else { continue };
        if !warnings.iter().any(|w| w.contains(todo)) {
            eprintln!("warning: {}: line {} of the translation: {}", name, i + 1, todo);
        }
    }
}

/// syn's "cannot parse string into token stream", in words that say what to look for.
fn plain_parse_error(e: &str) -> String {
    e.replace("cannot parse string into token stream", "this Rust doesn't divide into tokens: a bracket without its partner, or a string, character or comment that isn't closed")
}

/// Splits the `#![...]` lines of a crate root's translation that only `src/main.rs` (or
/// `lib.rs`) can hold, all but lint attributes, from the rest.
fn crate_attrs(oxie: &str) -> (String, String) {
    let (mut attrs, mut rest) = (String::new(), String::new());
    for line in oxie.split_inclusive('\n') {
        let lint = ["allow", "warn", "deny", "forbid", "expect"].iter().any(|l| line.starts_with(&format!("#![{}(", l)));
        if line.starts_with("#![") && !lint {
            attrs.push_str(line);
        } else {
            rest.push_str(line);
        }
    }
    (attrs, rest.trim_start_matches('\n').to_string())
}

fn collect(dir: &Path, out: &mut Vec<PathBuf>) -> Result<(), String> {
    for e in std::fs::read_dir(dir).map_err(|e| format!("{}: {}", dir.display(), e))? {
        let p = e.map_err(|e| e.to_string())?.path();
        if p.is_dir() {
            collect(&p, out)?;
        } else if p.extension().is_some_and(|x| x == "rs") {
            out.push(p);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{crate_attrs, module_dir, without_translated_mods};
    use std::path::{Path, PathBuf};

    #[test]
    fn mods_translated_in_the_same_run_are_dropped() {
        let todo = |m: &str| format!("// TODO(from-rust): remove this line if {m}.rs is translated to {m}.oxi too\n");
        let oxie = format!("{}mod util\n\n{}pub mod other\n\nfun main() {{}}\n", todo("util"), todo("other"));
        let util = PathBuf::from("src/util.rs");
        let out = without_translated_mods(&oxie, &module_dir(Path::new("src/main.rs")), &[&util]);
        assert_eq!(out, format!("{}pub mod other\n\nfun main() {{}}\n", todo("other")));
        // attributes stay with their module, which Oxie declares with them
        let oxie = format!("@cfg(unix)\n{}mod util\n\nfun main() {{}}\n", todo("util"));
        let out = without_translated_mods(&oxie, &module_dir(Path::new("src/main.rs")), &[&util]);
        assert_eq!(out, "@cfg(unix)\nmod util\n\nfun main() {}\n");
        // `src/a.rs` declares the modules in `src/a/`
        assert_eq!(module_dir(Path::new("src/a.rs")), Path::new("src/a"));
    }

    #[test]
    fn crate_attributes_go_to_the_root_stub() {
        let (attrs, rest) = crate_attrs("#![cfg_attr(windows, windows_subsystem = \"windows\")]\n#![allow(dead_code)]\n\nfun main() {}\n");
        assert_eq!(attrs, "#![cfg_attr(windows, windows_subsystem = \"windows\")]\n");
        assert_eq!(rest, "#![allow(dead_code)]\n\nfun main() {}\n");
    }
}
