//! Crate-level driver: parses all .oxi files of a crate, builds the symbol table and
//! produces Rust files plus source maps.

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

use crate::ast::File;
use crate::diag::{Diag, LineIndex, Severity, Span};
use crate::emit::{Decision, Mark};
use crate::lower::Lowerer;
use crate::parser;
use crate::runtime;
use crate::sema::{self, SourceUnit};

/// One input file, with its path relative to the source root (e.g. `net/http.oxi`).
#[derive(Clone, Debug)]
pub struct Input {
    pub rel_path: PathBuf,
    pub abs_path: PathBuf,
    pub text: String,
}

#[derive(Clone, Debug, Default)]
pub struct Options {
    pub crate_name: String,
    pub extern_crates: HashSet<String>,
    /// Absolute directory the generated files are written to (used in `#[path]` attributes).
    pub out_dir: PathBuf,
    /// A single-file script: its generated `main.rs` carries the crate attributes (`#![...]`).
    pub script: bool,
    /// The `#![...]` attributes of the crate's Rust root (`src/lib.rs`), such as `no_std`.
    pub root_attrs: Vec<String>,
}

#[derive(Clone, Debug, Default)]
pub struct SourceMap {
    pub source: PathBuf,
    /// (generated line, generated col, source line, source col), zero-based, sorted.
    pub entries: Vec<(u32, u32, u32, u32)>,
}

impl SourceMap {
    pub fn to_text(&self) -> String {
        let mut s = format!("oxie-sourcemap 1\n{}\n", self.source.display());
        for (gl, gc, sl, sc) in &self.entries {
            s.push_str(&format!("{} {} {} {}\n", gl, gc, sl, sc));
        }
        s
    }

    pub fn parse(text: &str) -> Option<SourceMap> {
        let mut lines = text.lines();
        if lines.next()? != "oxie-sourcemap 1" {
            return None;
        }
        let source = PathBuf::from(lines.next()?);
        let mut entries = Vec::new();
        for l in lines {
            let n: Vec<u32> = l.split(' ').filter_map(|x| x.parse().ok()).collect();
            if n.len() == 4 {
                entries.push((n[0], n[1], n[2], n[3]));
            }
        }
        Some(SourceMap { source, entries })
    }

    /// Maps a zero-based generated position to a zero-based source position.
    pub fn lookup(&self, line: u32, col: u32) -> Option<(u32, u32)> {
        let mut best: Option<&(u32, u32, u32, u32)> = None;
        for e in &self.entries {
            if (e.0, e.1) <= (line, col) {
                best = Some(e);
            } else {
                break;
            }
        }
        best.map(|e| (e.2, e.3))
    }
}

#[derive(Clone, Debug)]
pub struct GenFile {
    /// Path relative to the output directory.
    pub rel_path: PathBuf,
    pub text: String,
    pub map: Option<SourceMap>,
    /// The choices made for this file that its source doesn't spell out, in the order emitted.
    pub decisions: Vec<Decision>,
}

#[derive(Debug, Default)]
pub struct Output {
    pub files: Vec<GenFile>,
    /// Diagnostics per input (by absolute path).
    pub diags: Vec<(PathBuf, String, Vec<Diag>)>,
}

impl Output {
    pub fn has_errors(&self) -> bool {
        self.diags.iter().any(|(_, _, d)| d.iter().any(|d| d.severity == Severity::Error))
    }
}

/// Module path for a source file relative to the source root.
pub fn module_of(rel: &Path) -> (Vec<String>, bool) {
    let mut parts: Vec<String> = rel.iter().map(|p| p.to_string_lossy().to_string()).collect();
    let file = parts.pop().unwrap_or_default();
    let stem = file.trim_end_matches(".oxi").to_string();
    if parts.is_empty() && (stem == "main" || stem == "lib") {
        return (Vec::new(), true);
    }
    if stem != "mod" {
        parts.push(stem);
    }
    (parts, false)
}

struct Parsed {
    input: Input,
    module: Vec<String>,
    is_root: bool,
    file: File,
    diags: Vec<Diag>,
}

pub fn compile(inputs: Vec<Input>, opts: &Options) -> Output {
    let mut parsed: Vec<Parsed> = Vec::new();
    for input in inputs {
        let (mut file, mut diags) = parser::parse_file(&input.text);
        crate::lists::grow_lists(&mut file.items);
        let (module, is_root) = module_of(&input.rel_path);
        // a file or directory name becomes a module name
        if let Some(bad) = module.iter().find(|m| !m.starts_with(|c: char| c.is_alphabetic() || c == '_') || !m.chars().all(|c| c.is_alphanumeric() || c == '_')) {
            let fixed: String = bad.chars().map(|c| if c.is_alphanumeric() { c } else { '_' }).collect();
            diags.insert(0, Diag::error(Span::default(), format!("`{}` can't be the name of a module: the names of .oxi files and their directories are module names, so they must be identifiers", bad)).with_help(format!("rename it to `{}`", fixed)));
        }
        parsed.push(Parsed { input, module, is_root, file, diags });
    }
    crate::lists::grow_params(&mut parsed.iter_mut().map(|p| &mut p.file).collect::<Vec<_>>());
    crate::boxes::box_recursive(&mut parsed.iter_mut().map(|p| &mut p.file).collect::<Vec<_>>());
    // context parameters first: every later pass sees them as parameters
    let context_diags = crate::contexts::add_contexts(&mut parsed.iter_mut().map(|p| (p.module.as_slice(), &mut p.file)).collect::<Vec<_>>());
    for (i, d) in context_diags {
        parsed[i].diags.push(d);
    }
    sema::auto_type_lifetimes(&mut parsed.iter_mut().map(|p| &mut p.file).collect::<Vec<_>>());
    let has_lib = parsed.iter().any(|p| p.is_root && p.input.rel_path.file_stem().is_some_and(|s| s == "lib"));
    // in a program (an Oxie root without a library) no other crate calls `pub` items, so they lend
    // their parameters too
    let has_root = parsed.iter().any(|p| p.is_root);
    let param_notes = crate::params::borrow_params(&mut parsed.iter_mut().map(|p| &mut p.file).collect::<Vec<_>>());
    let collect = |parsed: &[Parsed]| {
        let units: Vec<SourceUnit> = parsed.iter().map(|p| SourceUnit { module: p.module.clone(), file: &p.file }).collect();
        sema::collect(&units, &opts.crate_name, opts.extern_crates.clone(), has_lib)
    };
    let mut k = collect(&parsed);
    let changing_exts = sema::ext_receivers(&mut parsed.iter_mut().map(|p| (p.module.as_slice(), &mut p.file)).collect::<Vec<_>>(), &k);
    // builder methods that return `this` take `self`; sema again for their receivers (and for the
    // extension functions that change theirs)
    if sema::by_value_receivers(&mut parsed.iter_mut().map(|p| (p.module.as_slice(), &mut p.file)).collect::<Vec<_>>(), &k) || changing_exts {
        k = collect(&parsed);
    }
    // what takes the receivers: methods that take only the fields they use, and the class and
    // `Box` parameters of functions and every kind of parameter of methods to lend. Then sema
    // again, for the new signatures, and the errors for lent parameters that a body keeps.
    let field_notes = crate::fields::take_fields(&mut parsed.iter_mut().map(|p| (p.module.as_slice(), &mut p.file)).collect::<Vec<_>>(), &k);
    let class_notes = crate::params::borrow_class_params(&mut parsed.iter_mut().map(|p| (p.module.as_slice(), &mut p.file)).collect::<Vec<_>>(), &k);
    let method_notes = crate::params::borrow_method_params(&mut parsed.iter_mut().map(|p| (p.module.as_slice(), &mut p.file)).collect::<Vec<_>>(), &k);
    if !field_notes.is_empty() || !class_notes.is_empty() || !method_notes.is_empty() {
        k = collect(&parsed);
    }
    let kept = crate::params::check_kept(&parsed.iter().map(|p| (p.module.as_slice(), &p.file)).collect::<Vec<_>>(), &k);
    for (i, d) in kept {
        parsed[i].diags.push(d);
    }
    let param_notes: Vec<crate::params::ParamNote> = param_notes.into_iter().chain(field_notes).chain(class_notes).chain(method_notes).collect();
    let words: Vec<HashSet<String>> = parsed.iter().map(|p| source_words(&p.input.text)).collect();
    // what the crate calls: the dry run records the Oxie code's calls, beside those in Rust code
    let mut calls = HashSet::new();
    for p in &parsed {
        crate::params::raw_calls(&p.file.items, &mut calls);
    }
    if has_root {
        for (i, p) in parsed.iter().enumerate() {
            let mut l = Lowerer::new(&k, p.module.clone(), p.is_root);
            l.source_words = Some(words[i].clone());
            l.lower_file(&p.file);
            calls.extend(l.calls);
        }
    }

    let mut out = Output::default();
    // module tree: path -> generated file
    let mut module_files: BTreeMap<Vec<String>, PathBuf> = BTreeMap::new();
    for p in &parsed {
        if !p.is_root {
            let mut rel = PathBuf::new();
            for m in &p.module {
                rel.push(m);
            }
            rel.set_extension("rs");
            module_files.insert(p.module.clone(), rel);
        }
    }
    let abs = |rel: &Path| opts.out_dir.join(rel).to_string_lossy().replace('\\', "/");
    // modules a `mod name` line of their parent declares there (`Lowerer::module_files`); the
    // others are declared after the parent's code
    let mut placed: HashSet<Vec<String>> = HashSet::new();
    for p in &parsed {
        for it in &p.file.items {
            let crate::ast::ItemKind::Raw(s) = &it.kind else { continue };
            let Some(name) = s.strip_prefix("mod ").and_then(|r| r.strip_suffix(';')) else { continue };
            let path = [p.module.as_slice(), &[name.to_string()]].concat();
            if module_files.contains_key(&path) {
                placed.insert(path);
            }
        }
    }
    let generated: std::collections::HashMap<Vec<String>, String> = module_files.iter().map(|(m, rel)| (m.clone(), abs(rel))).collect();
    let no_std = std_cfg(opts.root_attrs.iter().chain(parsed.iter().filter(|p| p.is_root).flat_map(|p| &p.file.inner_attrs))).is_some();
    let docs_linted = opts.root_attrs.iter().chain(parsed.iter().filter(|p| p.is_root).flat_map(|p| &p.file.inner_attrs)).any(|a| a.contains("missing_docs") && !a.trim_start().starts_with("allow"));

    // with an Oxie root, generated accessors nobody calls are left out: the lowering runs again
    // in the rare case that it calls one the dry run didn't
    let lowered = loop {
        let mut missed = HashSet::new();
        let mut lowered = Vec::new();
        for (i, p) in parsed.iter().enumerate() {
            let mut l = Lowerer::new(&k, p.module.clone(), p.is_root);
            l.param_notes = param_notes.iter().filter(|n| n.file == i).map(|n| ((n.fun_span, n.index), n.clone())).collect();
            l.src_dir = p.input.abs_path.parent().map(|d| d.to_path_buf());
            l.module_files = generated.clone();
            l.docs_linted = docs_linted;
            l.no_std = no_std;
            l.crate_attrs_allowed = opts.script;
            l.source_words = Some(words[i].clone());
            l.crate_calls = has_root.then_some(&calls);
            l.api = has_lib && p.input.rel_path != std::path::Path::new("main.oxi");
            l.lower_file(&p.file);
            missed.extend(l.calls.iter().filter(|c| !calls.contains(*c)).cloned());
            lowered.push((l.e, l.diags, l.root_lints));
        }
        if !has_root || missed.is_empty() {
            break lowered;
        }
        calls.extend(missed);
    };
    for (p, (e, mut ldiags, root_lints)) in parsed.iter().zip(lowered) {
        let mut diags = p.diags.clone();
        diags.append(&mut ldiags);
        let idx = LineIndex::new(&p.input.text);
        let mut header = String::from("// Generated by Oxie from ");
        // a script is compiled as `main.oxi`; the header names its file
        let shown = if opts.script { p.input.abs_path.file_name().map(PathBuf::from) } else { None };
        header.push_str(&shown.as_ref().unwrap_or(&p.input.rel_path).to_string_lossy());
        header.push_str(". Do not edit.\n");
        let mut footer = String::new();
        if p.is_root {
            header.push_str(&format!(
                "#[path = \"{}\"]\n#[doc(hidden)]\n#[allow(dead_code, unused_imports)]\npub(crate) mod __oxie;\n",
                abs(Path::new("__oxie.rs"))
            ));
            // after the root's code, so the macros it declares reach every module; crate-level lint
            // attributes (`#![allow(...)]` in the root file) also cover every module
            let decls = module_decls(&module_files, &placed, &[], &abs);
            if root_lints.is_empty() {
                footer.push_str(&decls);
            } else {
                for line in decls.lines() {
                    if line.starts_with("#[path") || (line.starts_with("pub mod") && line.ends_with('{')) {
                        for lint in &root_lints {
                            footer.push_str(&format!("#[{}]\n", lint));
                        }
                    }
                    footer.push_str(line);
                    footer.push('\n');
                }
            }
        } else {
            footer.push_str(&module_decls(&module_files, &placed, &p.module, &abs));
        }
        let header_lines = header.matches('\n').count() as u32;
        let mut text = header;
        text.push_str(&e.buf);
        if !text.ends_with('\n') {
            text.push('\n');
        }
        if !footer.is_empty() {
            text.push('\n');
            text.push_str(&footer);
        }
        let entries = map_entries(&e.marks, header_lines, &idx, &p.input.text);
        let rel = if p.is_root {
            PathBuf::from(format!("{}.rs", p.input.rel_path.file_stem().unwrap().to_string_lossy()))
        } else {
            module_files[&p.module].clone()
        };
        let decisions = e.decisions.iter().map(|d| Decision { gen_line: d.gen_line + header_lines, ..d.clone() }).collect();
        out.files.push(GenFile { rel_path: rel, text, map: Some(SourceMap { source: p.input.abs_path.clone(), entries }), decisions });
        out.diags.push((p.input.abs_path.clone(), p.input.text.clone(), diags));
    }
    // runtime + plain module include for mixed Rust projects
    let root_attrs = opts.root_attrs.iter().chain(parsed.iter().filter(|p| p.is_root).flat_map(|p| &p.file.inner_attrs));
    let runtime = runtime::runtime(std_cfg(root_attrs).as_deref());
    out.files.push(GenFile { rel_path: PathBuf::from("__oxie.rs"), text: runtime, map: None, decisions: Vec::new() });
    let mut modrs = String::from("// Generated by Oxie. Include with include!(concat!(env!(\"OUT_DIR\"), \"/oxie/mod.rs\"));\n");
    modrs.push_str(&format!("#[path = \"{}\"]\n#[doc(hidden)]\n#[allow(dead_code, unused_imports)]\npub(crate) mod __oxie;\n", abs(Path::new("__oxie.rs"))));
    modrs.push_str(&module_decls(&module_files, &placed, &[], &abs));
    out.files.push(GenFile { rel_path: PathBuf::from("mod.rs"), text: modrs, map: None, decisions: Vec::new() });
    out
}

/// The `cfg` under which a crate with these root attributes has `std`: `None` when it always
/// has it, `any()` (never) for `no_std`, `not(P)` for `cfg_attr(P, no_std)`.
fn std_cfg<'s>(attrs: impl Iterator<Item = &'s String>) -> Option<String> {
    for a in attrs {
        let a = a.trim();
        if a == "no_std" {
            return Some("any()".into());
        }
        let Some(inner) = a.strip_prefix("cfg_attr(").and_then(|r| r.strip_suffix(')')) else { continue };
        let parts = split_top_level(inner);
        if parts.len() > 1 && parts[1..].iter().any(|p| p.trim() == "no_std") {
            return Some(format!("not({})", parts[0].trim()));
        }
    }
    None
}

/// `a, f(b, c), "d,e"` split at its top-level commas.
fn split_top_level(s: &str) -> Vec<&str> {
    let (mut parts, mut depth, mut start, mut text) = (Vec::new(), 0i32, 0, false);
    for (i, c) in s.char_indices() {
        match c {
            '"' => text = !text,
            '(' | '[' if !text => depth += 1,
            ')' | ']' if !text => depth -= 1,
            ',' if !text && depth == 0 => {
                parts.push(&s[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    parts.push(&s[start..]);
    parts
}

/// The `#![...]` attributes of a Rust file (`src/lib.rs`), as the text inside the brackets.
pub fn inner_attrs_of(rust: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = rust;
    while let Some(i) = rest.find("#![") {
        let body = &rest[i + 3..];
        let mut depth = 1;
        let Some(end) = body.char_indices().find_map(|(j, c)| {
            match c {
                '[' => depth += 1,
                ']' => depth -= 1,
                _ => {}
            }
            (depth == 0).then_some(j)
        }) else {
            break;
        };
        // not in a line comment
        let line_start = rest[..i].rfind('\n').map_or(0, |p| p + 1);
        if !rest[line_start..i].contains("//") {
            out.push(body[..end].to_string());
        }
        rest = &body[end..];
    }
    out
}

/// `#[path = ".."] pub mod x;` for the direct children of `parent`, with inline modules for
/// directories, except those a `mod x` line of the parent declares (`placed`).
fn module_decls(files: &BTreeMap<Vec<String>, PathBuf>, placed: &HashSet<Vec<String>>, parent: &[String], abs: &dyn Fn(&Path) -> String) -> String {
    let mut children: Vec<String> = Vec::new();
    for m in files.keys() {
        if m.len() > parent.len() && m[..parent.len()] == *parent {
            let c = m[parent.len()].clone();
            if !children.contains(&c) {
                children.push(c);
            }
        }
    }
    let mut s = String::new();
    for c in children {
        let mut path = parent.to_vec();
        path.push(c.clone());
        if placed.contains(&path) {
            continue;
        }
        match files.get(&path) {
            Some(rel) => {
                s.push_str(&format!("#[path = \"{}\"]\npub mod {};\n", abs(rel), c));
            }
            None => {
                s.push_str(&format!("pub mod {} {{\n", c));
                for line in module_decls(files, placed, &path, abs).lines() {
                    s.push_str("    ");
                    s.push_str(line);
                    s.push('\n');
                }
                s.push_str("}\n");
            }
        }
    }
    s
}

fn map_entries(marks: &[Mark], line_offset: u32, idx: &LineIndex, src: &str) -> Vec<(u32, u32, u32, u32)> {
    let mut v: Vec<(u32, u32, u32, u32)> = marks
        .iter()
        .map(|m| {
            let (l, c) = idx.line_col(src, m.src as usize);
            (m.gen_line + line_offset, m.gen_col, l as u32, c as u32)
        })
        .collect();
    v.sort();
    v.dedup_by(|a, b| a.0 == b.0 && a.1 == b.1);
    v
}

/// The words of a source file outside comments: generated code doesn't shorten a std path to a
/// name among them.
fn source_words(text: &str) -> HashSet<String> {
    let mut code = String::with_capacity(text.len());
    let b = text.as_bytes();
    let mut i = 0;
    let mut in_str = false;
    while i < b.len() {
        let c = b[i];
        if in_str {
            // the words of a string count (`${name}`), only comments are skipped
            if c == b'\\' {
                code.push(' ');
                i += 2;
                continue;
            }
            in_str = c != b'"';
        } else if c == b'"' {
            in_str = true;
        } else if c == b'\'' && b.get(i + 2) == Some(&b'\'') {
            // `'"'`: a character, not the start of a string
            i += 3;
            continue;
        } else if b[i..].starts_with(b"//") {
            i = b[i..].iter().position(|&c| c == b'\n').map_or(b.len(), |n| i + n);
            continue;
        } else if b[i..].starts_with(b"/*") {
            i = b[i + 2..].windows(2).position(|w| w == b"*/").map_or(b.len(), |n| i + n + 4);
            continue;
        }
        code.push(if c.is_ascii() { c as char } else { ' ' });
        i += 1;
    }
    code.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_')).filter(|w| !w.is_empty()).map(String::from).collect()
}

/// Lowers a single file in isolation (for `oxie emit`, tests and tooling).
pub fn compile_one(text: &str) -> (String, Vec<Diag>) {
    let input = Input { rel_path: PathBuf::from("main.oxi"), abs_path: PathBuf::from("main.oxi"), text: text.to_string() };
    let opts = Options { crate_name: "main".into(), extern_crates: HashSet::new(), out_dir: PathBuf::from("/oxie"), script: true, root_attrs: Vec::new() };
    let out = compile(vec![input], &opts);
    let diags = out.diags.into_iter().next().map(|d| d.2).unwrap_or_default();
    let text = out.files.into_iter().next().map(|f| f.text).unwrap_or_default();
    (text, diags)
}

/// Discovers all .oxi files under `src_dir`.
/// `std::fs::canonicalize` without the `\\?\` prefix it adds on Windows: Cargo manifests and
/// paths written into generated code don't accept it (`D:\a` instead of `\\?\D:\a`).
pub fn canonicalize(p: impl AsRef<Path>) -> std::io::Result<PathBuf> {
    let p = std::fs::canonicalize(p)?;
    let plain = without_verbatim_prefix(&p.to_string_lossy());
    Ok(plain.map(PathBuf::from).unwrap_or(p))
}

fn without_verbatim_prefix(path: &str) -> Option<String> {
    if let Some(rest) = path.strip_prefix(r"\\?\UNC\") {
        return Some(format!(r"\\{rest}"));
    }
    // only drive paths: other verbatim paths have no plain spelling
    path.strip_prefix(r"\\?\").filter(|rest| rest.as_bytes().get(1) == Some(&b':')).map(str::to_string)
}

pub fn discover(src_dir: &Path) -> std::io::Result<Vec<Input>> {
    let mut out = Vec::new();
    fn walk(dir: &Path, root: &Path, out: &mut Vec<Input>) -> std::io::Result<()> {
        let mut entries: Vec<_> = std::fs::read_dir(dir)?.filter_map(|e| e.ok()).collect();
        entries.sort_by_key(|e| e.file_name());
        for e in entries {
            let p = e.path();
            if p.is_dir() {
                walk(&p, root, out)?;
            } else if p.extension().is_some_and(|x| x == "oxi") {
                let text = std::fs::read_to_string(&p)?;
                let rel = p.strip_prefix(root).unwrap().to_path_buf();
                let abs_path = canonicalize(&p).unwrap_or(p.clone());
                out.push(Input { rel_path: rel, abs_path, text });
            }
        }
        Ok(())
    }
    if src_dir.exists() {
        walk(src_dir, src_dir, &mut out)?;
    }
    Ok(out)
}

/// Dependency names from a Cargo.toml (crate names usable as path roots).
pub fn extern_crates_from_manifest(manifest: &str) -> HashSet<String> {
    let mut set = HashSet::new();
    let mut in_deps = false;
    for line in manifest.lines() {
        let t = line.trim();
        if t.starts_with('[') {
            in_deps = t.ends_with("dependencies]") && !t.contains("build-dependencies");
            if let Some(name) = t.strip_prefix("[dependencies.").and_then(|s| s.strip_suffix(']')) {
                set.insert(name.replace('-', "_"));
            }
            continue;
        }
        if in_deps {
            if let Some((name, _)) = t.split_once('=') {
                let name = name.trim().trim_matches('"');
                if !name.is_empty() && !name.starts_with('#') {
                    set.insert(name.replace('-', "_"));
                }
            }
        }
    }
    set
}

/// Writes generated files into `out_dir`, skipping unchanged files.
pub fn write_output(out: &Output, out_dir: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(out_dir)?;
    for f in &out.files {
        let path = out_dir.join(&f.rel_path);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        if std::fs::read_to_string(&path).ok().as_deref() != Some(f.text.as_str()) {
            std::fs::write(&path, &f.text)?;
        }
        if let Some(m) = &f.map {
            let mp = PathBuf::from(format!("{}.map", path.display()));
            let t = m.to_text();
            if std::fs::read_to_string(&mp).ok().as_deref() != Some(t.as_str()) {
                std::fs::write(mp, t)?;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_std_roots_get_a_support_module_without_std() {
        let attrs = inner_attrs_of("//! Docs.\n#![cfg_attr(not(feature = \"std\"), no_std)]\n// #![no_std]\n#![allow(dead_code)]\nmod a;\n");
        assert_eq!(attrs, ["cfg_attr(not(feature = \"std\"), no_std)", "allow(dead_code)"]);
        assert_eq!(std_cfg(attrs.iter()).as_deref(), Some("not(not(feature = \"std\"))"));
        assert_eq!(std_cfg(["no_std".to_string()].iter()).as_deref(), Some("any()"));
        assert_eq!(std_cfg(["allow(dead_code)".to_string(), "cfg_attr(test, allow(unused))".to_string()].iter()), None);
        let support = runtime::runtime(Some("any()"));
        assert!(support.contains("#[cfg(any())] pub trait SharedLock") && !support.contains("/*std*/"));
        assert!(!runtime::runtime(None).contains("#[cfg("));
    }

    #[test]
    fn verbatim_prefix_is_removed() {
        assert_eq!(without_verbatim_prefix(r"\\?\D:\a\oxie"), Some(r"D:\a\oxie".to_string()));
        assert_eq!(without_verbatim_prefix(r"\\?\UNC\server\share\x"), Some(r"\\server\share\x".to_string()));
        assert_eq!(without_verbatim_prefix(r"\\?\Volume{1234}\x"), None);
        assert_eq!(without_verbatim_prefix("/home/ada/x.oxi"), None);
    }

    /// The decisions of a one-file program: (the source they are about, what was chosen, why).
    fn decisions(src: &str) -> Vec<(String, String, String)> {
        let input = Input { rel_path: PathBuf::from("main.oxi"), abs_path: PathBuf::from("main.oxi"), text: src.to_string() };
        let opts = Options { crate_name: "main".into(), extern_crates: HashSet::new(), out_dir: PathBuf::from("/oxie"), script: true, root_attrs: Vec::new() };
        let out = compile(vec![input], &opts);
        assert!(!out.has_errors(), "{:?}", out.diags);
        out.files[0].decisions.iter().map(|d| (src[d.span.start as usize..d.span.end as usize].to_string(), d.chosen.clone(), d.why.clone())).collect()
    }

    /// The messages of the diagnostics of `severity` that compiling `inputs` gives, by file.
    fn messages(inputs: &[(&str, &str)], severity: Severity) -> Vec<(String, String)> {
        let inputs = inputs.iter().map(|(rel, text)| Input { rel_path: PathBuf::from(rel), abs_path: PathBuf::from(rel), text: text.to_string() }).collect();
        let opts = Options { crate_name: "main".into(), extern_crates: HashSet::new(), out_dir: PathBuf::from("/oxie"), script: false, root_attrs: Vec::new() };
        let out = compile(inputs, &opts);
        out.diags.iter().flat_map(|(path, _, ds)| ds.iter().filter(|d| d.severity == severity).map(move |d| (path.display().to_string(), d.message.clone()))).collect()
    }

    #[test]
    fn public_methods_of_a_library_say_mut() {
        // a library's users call `current`: its signature can't follow its body
        let lib = "pub class Counter {\n    var hits = 0\n    pub fun current(): i32 {\n        hits += 1\n        return hits\n    }\n    pub mut fun bump() { hits += 1 }\n    private fun inner() { hits += 1 }\n}\nclass Hidden {\n    var n = 0\n    pub fun touch() { n += 1 }\n}\n";
        let main = "class Local {\n    var n = 0\n    pub fun touch() { n += 1 }\n}\nfun main() {}\n";
        let errors = messages(&[("lib.oxi", lib), ("main.oxi", main)], Severity::Error);
        assert_eq!(errors, [("lib.oxi".to_string(), "`Counter.current` is public and changes its object, so it must say so: `mut fun current`".to_string())]);
        // a program has no users but itself
        assert!(messages(&[("main.oxi", &format!("{lib}fun main() {{}}\n"))], Severity::Error).is_empty());
    }

    #[test]
    fn changing_a_copy_warns_where_kotlin_would_share() {
        let src = "data class Pos(var x: i32, var y: i32)\nshared class Acc(var history: [i64] = [])\nfun main() {\n    var ps: [Pos] = [Pos(1, 1)]\n    var p = ps[0]\n    p.x = 5\n    println(ps[0].x)\n    var q = ps[0].copy()\n    q.x = 6\n    println(ps[0].x)\n    val a = Acc()\n    var h = a.history\n    h.push(1)\n    var kept = a.history.clone()\n    kept.push(2)\n    println(\"${p.x} ${q.x} ${h:?} ${kept:?}\")\n}\n";
        let warnings: Vec<String> = messages(&[("main.oxi", src)], Severity::Warning).into_iter().map(|(_, m)| m).filter(|m| m.contains("copy")).collect();
        assert_eq!(warnings, ["`p` is a copy of `ps[0]`: changing it doesn't change `ps[0]`", "`h` is a copy of `a.history`: changing it doesn't change `a`"]);
    }

    #[test]
    fn decisions_are_about_the_right_function() {
        // the method shares the name of the function whose parameter is lent, but takes its own
        let src = "fun describe(t: String): usize = t.len()\nclass Shelf(var items: Vec<String> = vec[]) {\n    fun describe(own t: String) { items.push(t) }\n}\n";
        let ds = decisions(src);
        let about_t: Vec<_> = ds.iter().filter(|(about, ..)| about.starts_with("t:")).map(|(_, what, why)| (what.as_str(), why.as_str())).collect();
        assert_eq!(about_t, [("&str", "`t` isn't `own`")], "{ds:?}");
        assert!(ds.contains(&("describe".into(), "&mut self".into(), "it calls `items.push()`".into())), "{ds:?}");
    }

    #[test]
    fn mod_lines_of_oxie_modules_are_left_out() {
        // translated crates keep the `mod tools` of the Rust they came from; tools.oxi declares it
        let file = |rel: &str, text: &str| Input { rel_path: PathBuf::from(rel), abs_path: PathBuf::from(rel), text: text.to_string() };
        let inputs = vec![file("main.oxi", "pub(crate) mod tools

fun main() = println(tools.one())
"), file("tools.oxi", "fun one(): i32 = 1
")];
        let opts = Options { crate_name: "main".into(), extern_crates: HashSet::new(), out_dir: PathBuf::from("/oxie"), script: false, root_attrs: Vec::new() };
        let out = compile(inputs, &opts);
        assert!(!out.has_errors(), "{:?}", out.diags);
        let main = &out.files.iter().find(|f| f.rel_path == Path::new("main.rs")).unwrap().text;
        assert_eq!(main.matches("mod tools").count(), 1, "{main}");
    }

    #[test]
    fn child_modules_in_files_are_paths() {
        // in `lint`, `js.count()` calls a function of `lint/js.oxi`, not a method of a value `js`
        let file = |rel: &str, text: &str| Input { rel_path: PathBuf::from(rel), abs_path: PathBuf::from(rel), text: text.to_string() };
        let inputs = vec![
            file("main.oxi", "fun main() = println(lint.check())\n"),
            file("lint/mod.oxi", "fun check(): i32 = js.count() + js.Dialect.Tsx.size()\n"),
            file("lint/js.oxi", "fun count(): i32 = 3\nenum Dialect { Tsx }\nimpl Dialect {\n    fun size(): i32 = 1\n}\n"),
        ];
        let opts = Options { crate_name: "main".into(), extern_crates: HashSet::new(), out_dir: PathBuf::from("/oxie"), script: false, root_attrs: Vec::new() };
        let out = compile(inputs, &opts);
        assert!(!out.has_errors(), "{:?}", out.diags);
        let lint = &out.files.iter().find(|f| f.rel_path == Path::new("lint.rs")).unwrap().text;
        assert!(lint.contains("js::count() + js::Dialect::Tsx.size()"), "{lint}");
    }

    #[test]
    fn child_modules_hide_top_level_ones() {
        // in `editor`, `lsp` is `editor/lsp.oxi`, not the top-level `lsp.oxi`
        let file = |rel: &str, text: &str| Input { rel_path: PathBuf::from(rel), abs_path: PathBuf::from(rel), text: text.to_string() };
        let inputs = vec![
            file("main.oxi", "fun main() = println(\"${lsp.name()} ${editor.which()}\")\n"),
            file("lsp.oxi", "fun name(): String = \"top\"\n"),
            file("editor/mod.oxi", "import lsp::Event\n\nfun which(): String = lsp.name() + \"${Event.Message:?}\"\nfun first(e: lsp.Event): bool = e == Event.Message\n"),
            file("editor/lsp.oxi", "fun name(): String = \"child\"\nenum Event { Message }\n"),
        ];
        let opts = Options { crate_name: "main".into(), extern_crates: HashSet::new(), out_dir: PathBuf::from("/oxie"), script: false, root_attrs: Vec::new() };
        let out = compile(inputs, &opts);
        assert!(!out.has_errors(), "{:?}", out.diags);
        let editor = &out.files.iter().find(|f| f.rel_path == Path::new("editor.rs")).unwrap().text;
        assert!(!editor.contains("crate::lsp"), "{editor}");
        assert!(editor.contains("use lsp::Event;") && editor.contains("lsp::name()") && editor.contains("e: lsp::Event"), "{editor}");
    }

    #[test]
    fn lifetimes_say_which_parameters_they_tie() {
        // `View` holds a reference, so it gets the lifetime too; `n` has none
        let src = "class View(text: &str)\nfun pick(a: View, b: View, n: usize): &str = if (n > 0) a.text else b.text\n";
        let ds = decisions(src);
        assert!(ds.contains(&("pick".into(), "'a".into(), "the result borrows from `a` or `b`".into())), "{ds:?}");
    }
}
