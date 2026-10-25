//! Crate-level driver: parses all .oxi files of a crate, builds the symbol table and
//! produces Rust files plus source maps.

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

use crate::ast::File;
use crate::diag::{Diag, LineIndex, Severity, Span};
use crate::emit::{Decision, Mark};
use crate::lower::Lowerer;
use crate::parser;
use crate::runtime::RUNTIME_RS;
use crate::sema::{self, SourceUnit};

/// One input file, with its path relative to the source root (e.g. `net/http.oxi`).
#[derive(Clone, Debug)]
pub struct Input {
    pub rel_path: PathBuf,
    pub abs_path: PathBuf,
    pub text: String,
}

#[derive(Clone, Debug)]
pub struct Options {
    pub crate_name: String,
    pub extern_crates: HashSet<String>,
    /// Absolute directory the generated files are written to (used in `#[path]` attributes).
    pub out_dir: PathBuf,
    /// A single-file script: its generated `main.rs` carries the crate attributes (`#![...]`).
    pub script: bool,
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
        let (file, mut diags) = parser::parse_file(&input.text);
        let (module, is_root) = module_of(&input.rel_path);
        // a file or directory name becomes a module name
        if let Some(bad) = module.iter().find(|m| !m.starts_with(|c: char| c.is_alphabetic() || c == '_') || !m.chars().all(|c| c.is_alphanumeric() || c == '_')) {
            let fixed: String = bad.chars().map(|c| if c.is_alphanumeric() { c } else { '_' }).collect();
            diags.insert(0, Diag::error(Span::default(), format!("`{}` can't be the name of a module: the names of .oxi files and their directories are module names, so they must be identifiers", bad)).with_help(format!("rename it to `{}`", fixed)));
        }
        parsed.push(Parsed { input, module, is_root, file, diags });
    }
    // context parameters first: every later pass sees them as parameters
    let context_diags = crate::contexts::add_contexts(&mut parsed.iter_mut().map(|p| (p.module.as_slice(), &mut p.file)).collect::<Vec<_>>());
    for (i, d) in context_diags {
        parsed[i].diags.push(d);
    }
    sema::auto_type_lifetimes(&mut parsed.iter_mut().map(|p| &mut p.file).collect::<Vec<_>>());
    let param_notes = crate::params::borrow_params(&mut parsed.iter_mut().map(|p| &mut p.file).collect::<Vec<_>>());
    let has_lib = parsed.iter().any(|p| p.is_root && p.input.rel_path.file_stem().is_some_and(|s| s == "lib"));
    let collect = |parsed: &[Parsed]| {
        let units: Vec<SourceUnit> = parsed.iter().map(|p| SourceUnit { module: p.module.clone(), file: &p.file }).collect();
        sema::collect(&units, &opts.crate_name, opts.extern_crates.clone(), has_lib)
    };
    let mut k = collect(&parsed);
    // what takes the receivers: methods that take only the fields they use, and class and `Box`
    // parameters to lend. Then sema again, for the new signatures.
    let field_notes = crate::fields::take_fields(&mut parsed.iter_mut().map(|p| (p.module.as_slice(), &mut p.file)).collect::<Vec<_>>(), &k);
    let class_notes = crate::params::borrow_class_params(&mut parsed.iter_mut().map(|p| (p.module.as_slice(), &mut p.file)).collect::<Vec<_>>(), &k);
    if !field_notes.is_empty() || class_notes.iter().any(|n| n.borrowed) {
        k = collect(&parsed);
    }
    let param_notes: Vec<crate::params::ParamNote> = param_notes.into_iter().chain(field_notes).chain(class_notes).collect();

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

    for (i, p) in parsed.iter().enumerate() {
        let mut l = Lowerer::new(&k, p.module.clone(), p.is_root);
        l.param_notes = param_notes.iter().filter(|n| n.file == i).map(|n| ((n.fun_span, n.index), n.clone())).collect();
        l.src_dir = p.input.abs_path.parent().map(|d| d.to_path_buf());
        l.crate_attrs_allowed = opts.script;
        l.lower_file(&p.file);
        let mut diags = p.diags.clone();
        diags.append(&mut l.diags);
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
            // crate-level lint attributes (`#![allow(...)]` in the root file) also cover every module
            let decls = module_decls(&module_files, &[], &abs);
            if l.root_lints.is_empty() {
                header.push_str(&decls);
            } else {
                for line in decls.lines() {
                    if line.starts_with("#[path") || (line.starts_with("pub mod") && line.ends_with('{')) {
                        for lint in &l.root_lints {
                            header.push_str(&format!("#[{}]\n", lint));
                        }
                    }
                    header.push_str(line);
                    header.push('\n');
                }
            }
        } else {
            footer.push_str(&module_decls(&module_files, &p.module, &abs));
        }
        let header_lines = header.matches('\n').count() as u32;
        let mut text = header;
        text.push_str(&l.e.buf);
        if !text.ends_with('\n') {
            text.push('\n');
        }
        if !footer.is_empty() {
            text.push('\n');
            text.push_str(&footer);
        }
        let entries = map_entries(&l.e.marks, header_lines, &idx, &p.input.text);
        let rel = if p.is_root {
            PathBuf::from(format!("{}.rs", p.input.rel_path.file_stem().unwrap().to_string_lossy()))
        } else {
            module_files[&p.module].clone()
        };
        let decisions = l.e.decisions.iter().map(|d| Decision { gen_line: d.gen_line + header_lines, ..d.clone() }).collect();
        out.files.push(GenFile { rel_path: rel, text, map: Some(SourceMap { source: p.input.abs_path.clone(), entries }), decisions });
        out.diags.push((p.input.abs_path.clone(), p.input.text.clone(), diags));
    }
    // runtime + plain module include for mixed Rust projects
    out.files.push(GenFile { rel_path: PathBuf::from("__oxie.rs"), text: RUNTIME_RS.to_string(), map: None, decisions: Vec::new() });
    let mut modrs = String::from("// Generated by Oxie. Include with include!(concat!(env!(\"OUT_DIR\"), \"/oxie/mod.rs\"));\n");
    modrs.push_str(&format!("#[path = \"{}\"]\n#[doc(hidden)]\n#[allow(dead_code, unused_imports)]\npub(crate) mod __oxie;\n", abs(Path::new("__oxie.rs"))));
    modrs.push_str(&module_decls(&module_files, &[], &abs));
    out.files.push(GenFile { rel_path: PathBuf::from("mod.rs"), text: modrs, map: None, decisions: Vec::new() });
    out
}

/// `#[path = ".."] pub mod x;` for the direct children of `parent`, with inline modules for directories.
fn module_decls(files: &BTreeMap<Vec<String>, PathBuf>, parent: &[String], abs: &dyn Fn(&Path) -> String) -> String {
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
        match files.get(&path) {
            Some(rel) => {
                s.push_str(&format!("#[path = \"{}\"]\npub mod {};\n", abs(rel), c));
            }
            None => {
                s.push_str(&format!("pub mod {} {{\n", c));
                for line in module_decls(files, &path, abs).lines() {
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

/// Lowers a single file in isolation (for `oxie emit`, tests and tooling).
pub fn compile_one(text: &str) -> (String, Vec<Diag>) {
    let input = Input { rel_path: PathBuf::from("main.oxi"), abs_path: PathBuf::from("main.oxi"), text: text.to_string() };
    let opts = Options { crate_name: "main".into(), extern_crates: HashSet::new(), out_dir: PathBuf::from("/oxie"), script: true };
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
    fn verbatim_prefix_is_removed() {
        assert_eq!(without_verbatim_prefix(r"\\?\D:\a\oxie"), Some(r"D:\a\oxie".to_string()));
        assert_eq!(without_verbatim_prefix(r"\\?\UNC\server\share\x"), Some(r"\\server\share\x".to_string()));
        assert_eq!(without_verbatim_prefix(r"\\?\Volume{1234}\x"), None);
        assert_eq!(without_verbatim_prefix("/home/ada/x.oxi"), None);
    }

    /// The decisions of a one-file program: (the source they are about, what was chosen, why).
    fn decisions(src: &str) -> Vec<(String, String, String)> {
        let input = Input { rel_path: PathBuf::from("main.oxi"), abs_path: PathBuf::from("main.oxi"), text: src.to_string() };
        let opts = Options { crate_name: "main".into(), extern_crates: HashSet::new(), out_dir: PathBuf::from("/oxie"), script: true };
        let out = compile(vec![input], &opts);
        assert!(!out.has_errors(), "{:?}", out.diags);
        out.files[0].decisions.iter().map(|d| (src[d.span.start as usize..d.span.end as usize].to_string(), d.chosen.clone(), d.why.clone())).collect()
    }

    #[test]
    fn decisions_are_about_the_right_function() {
        // the method shares the name of the function whose parameter is lent, but keeps its own
        let src = "fun describe(t: String): usize = t.len()\nclass Shelf(var items: Vec<String> = vec[]) {\n    fun describe(t: String) { items.push(t) }\n}\n";
        let ds = decisions(src);
        assert_eq!(ds.iter().filter(|(about, ..)| about.starts_with("t:")).count(), 1, "{ds:?}");
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
        let opts = Options { crate_name: "main".into(), extern_crates: HashSet::new(), out_dir: PathBuf::from("/oxie"), script: false };
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
        let opts = Options { crate_name: "main".into(), extern_crates: HashSet::new(), out_dir: PathBuf::from("/oxie"), script: false };
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
        let opts = Options { crate_name: "main".into(), extern_crates: HashSet::new(), out_dir: PathBuf::from("/oxie"), script: false };
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
