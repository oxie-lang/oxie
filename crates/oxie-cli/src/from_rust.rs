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
    let t = oxie_from_rust::translate(&src).map_err(|e| format!("{}:{}", name, e))?;
    for w in &t.warnings {
        eprintln!("warning: {}: {}", name, w);
    }
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
    for (f, t) in files.iter().zip(oxie_from_rust::translate_crate(&refs)) {
        let name = f.display();
        match t {
            Ok(t) => {
                for w in &t.warnings {
                    eprintln!("warning: {}: {}", name, w);
                }
                let rel = f.strip_prefix(dir).unwrap_or(f);
                let dest = out.join(rel).with_extension("oxi");
                if let Some(parent) = dest.parent() {
                    std::fs::create_dir_all(parent).map_err(|e| format!("{}: {}", parent.display(), e))?;
                }
                let mut oxie = t.oxie.clone();
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
                eprintln!("error: {}:{}", name, e);
                failed = true;
            }
        }
    }
    Ok(if failed { ExitCode::FAILURE } else { ExitCode::SUCCESS })
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
    use super::crate_attrs;

    #[test]
    fn crate_attributes_go_to_the_root_stub() {
        let (attrs, rest) = crate_attrs("#![cfg_attr(windows, windows_subsystem = \"windows\")]\n#![allow(dead_code)]\n\nfun main() {}\n");
        assert_eq!(attrs, "#![cfg_attr(windows, windows_subsystem = \"windows\")]\n");
        assert_eq!(rest, "#![allow(dead_code)]\n\nfun main() {}\n");
    }
}
