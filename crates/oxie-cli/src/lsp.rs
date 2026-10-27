//! Language server for .oxi files (LSP over stdio).
//!
//! Oxie diagnostics (syntax and lowering errors) are reported on every change. On save, the
//! project is checked with `cargo check` and rustc's errors are mapped back to .oxi sources.

use std::collections::{HashMap, HashSet};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use oxie_compiler::ast::{File, ItemKind, Type};
use oxie_compiler::diag::{LineIndex, Severity, Span};
use oxie_compiler::sema::{self, Crate, Recv, SourceUnit};
use oxie_compiler::symbols::{self, Local, SymKind, Symbol};
use oxie_compiler::{compile, parser, Input};
use serde_json::{json, Value};

use crate::cargo;
use crate::project::{self, Project};

type Out = Arc<Mutex<std::io::Stdout>>;

fn send(out: &Out, msg: Value) {
    let text = msg.to_string();
    let mut o = out.lock().unwrap();
    let _ = write!(o, "Content-Length: {}\r\n\r\n{}", text.len(), text);
    let _ = o.flush();
}

fn notify(out: &Out, method: &str, params: Value) {
    send(out, json!({"jsonrpc": "2.0", "method": method, "params": params}));
}

// ---------------------------------------------------------------------------------------------
// URIs

pub fn uri_to_path(uri: &str) -> PathBuf {
    let s = uri.strip_prefix("file://").unwrap_or(uri);
    let mut out = Vec::new();
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    let p = String::from_utf8_lossy(&out).to_string();
    // windows: file:///C:/x
    let p = if p.len() > 2 && p.as_bytes()[0] == b'/' && p.as_bytes()[2] == b':' { p[1..].to_string() } else { p };
    PathBuf::from(p)
}

/// Directories the editor reaches through a symlink, as (canonical path, editor's path), like
/// macOS's `/tmp` for `/private/tmp`. URIs sent back use the editor's path: with the canonical
/// one it would put diagnostics on a file that isn't open and open locations a second time.
static ALIASES: Mutex<Vec<(PathBuf, PathBuf)>> = Mutex::new(Vec::new());

/// Remembers the editor's name for the directory of `path` if it differs from the canonical one.
fn remember_alias(path: &Path) {
    let (mut editor, mut real) = (path.to_path_buf(), canonical(path));
    while editor.file_name().is_some() && editor.file_name() == real.file_name() {
        editor.pop();
        real.pop();
    }
    let mut aliases = ALIASES.lock().unwrap();
    if editor != real && !aliases.iter().any(|(r, _)| *r == real) {
        aliases.push((real, editor));
    }
}

pub fn path_to_uri(p: &Path) -> String {
    // a document that isn't a file (`untitled:Untitled-1`) keeps its URI as its path
    let s = p.to_string_lossy();
    if let Some((scheme, _)) = s.split_once(':') {
        if scheme.len() > 1 && scheme.chars().all(|c| c.is_ascii_alphanumeric() || "+-.".contains(c)) && scheme != "file" {
            return s.to_string();
        }
    }
    let aliases = ALIASES.lock().unwrap();
    let alias = aliases.iter().filter(|(real, _)| p.starts_with(real)).max_by_key(|(real, _)| real.components().count());
    let p = match alias.and_then(|(real, editor)| p.strip_prefix(real).ok().map(|rest| (editor, rest))) {
        Some((editor, rest)) if rest.as_os_str().is_empty() => editor.clone(),
        Some((editor, rest)) => editor.join(rest),
        None => p.to_path_buf(),
    };
    let s = p.to_string_lossy().replace('\\', "/");
    let mut out = String::from("file://");
    if !s.starts_with('/') {
        out.push('/');
    }
    for c in s.chars() {
        if c.is_ascii_alphanumeric() || "/-_.~:".contains(c) {
            out.push(c);
        } else {
            let mut buf = [0u8; 4];
            for b in c.encode_utf8(&mut buf).bytes() {
                out.push_str(&format!("%{:02X}", b));
            }
        }
    }
    out
}

/// An LSP range from zero-based lines and character columns.
fn char_range(text: &str, line: u32, col: u32, end_line: u32, end_col: u32) -> Value {
    let utf16 = |l: u32, c: u32| -> usize { text.lines().nth(l as usize).unwrap_or("").chars().take(c as usize).map(|ch| ch.len_utf16()).sum() };
    json!({"start": {"line": line, "character": utf16(line, col)}, "end": {"line": end_line, "character": utf16(end_line, end_col)}})
}

fn range(text: &str, idx: &LineIndex, start: usize, end: usize) -> Value {
    let (sl, sc) = idx.line_col_utf16(text, start);
    let (el, ec) = idx.line_col_utf16(text, end.max(start));
    json!({"start": {"line": sl, "character": sc}, "end": {"line": el, "character": ec}})
}

// ---------------------------------------------------------------------------------------------
// analysis

struct FileData {
    path: PathBuf,
    text: String,
    ast: File,
    idx: LineIndex,
    symbols: Vec<Symbol>,
}

struct Analysis {
    root: PathBuf,
    is_script: bool,
    files: Vec<FileData>,
    krate: Crate,
    /// Oxie diagnostics per file (LSP JSON)
    diags: HashMap<PathBuf, Vec<Value>>,
    /// generated Rust per file
    rust: HashMap<PathBuf, String>,
    /// what Oxie decided that the source doesn't spell out, per file
    decisions: HashMap<PathBuf, Vec<oxie_compiler::emit::Decision>>,
}

impl Analysis {
    fn file(&self, p: &Path) -> Option<&FileData> {
        self.files.iter().find(|f| f.path == p)
    }
}

/// A `cargo check` of one crate: a project, or the hidden project of a script.
struct Check {
    root: PathBuf,
    files: Vec<PathBuf>,
    is_script: bool,
}

/// Checks run one at a time on a background thread; requests made meanwhile wait here, one per
/// crate, so a script opened during another script's check is checked next.
#[derive(Default)]
struct CheckQueue {
    running: bool,
    pending: Vec<Check>,
}

struct Server {
    out: Out,
    docs: HashMap<PathBuf, String>,
    cargo_diags: Arc<Mutex<HashMap<PathBuf, Vec<Value>>>>,
    oxie_diags: Arc<Mutex<HashMap<PathBuf, Vec<Value>>>>,
    checks: Arc<Mutex<CheckQueue>>,
    cargo_check_enabled: bool,
}

fn canonical(p: &Path) -> PathBuf {
    oxie_compiler::compile::canonicalize(p).unwrap_or_else(|_| p.to_path_buf())
}

impl Server {
    fn analyze(&self, file: &Path) -> Analysis {
        let file = canonical(file);
        let dir = file.parent().map(|d| d.to_path_buf()).unwrap_or_default();
        let proj = Project::find(&dir).filter(|p| file.starts_with(p.src_dir()));
        let (root, mut inputs, opts, is_script) = match &proj {
            Some(p) => {
                let inputs = oxie_compiler::discover(&p.src_dir()).unwrap_or_default();
                (p.root.clone(), inputs, p.options(p.root.join("target/oxie-lsp")), false)
            }
            None => {
                let text = self.docs.get(&file).cloned().or_else(|| std::fs::read_to_string(&file).ok()).unwrap_or_default();
                let inputs = vec![Input { rel_path: PathBuf::from("main.oxi"), abs_path: file.clone(), text: text.clone() }];
                (dir.clone(), inputs, project::script_options("script", &project::script_deps(&text)), true)
            }
        };
        // overlay unsaved editor contents
        for i in inputs.iter_mut() {
            if let Some(t) = self.docs.get(&canonical(&i.abs_path)) {
                i.text = t.clone();
            }
            i.abs_path = canonical(&i.abs_path);
        }
        let output = compile::compile(inputs.clone(), &opts);
        let mut files = Vec::new();
        for i in &inputs {
            let (ast, _) = parser::parse_file(&i.text);
            let idx = LineIndex::new(&i.text);
            let syms = symbols::file_symbols(&ast, &i.text);
            files.push(FileData { path: i.abs_path.clone(), text: i.text.clone(), ast, idx, symbols: syms });
        }
        let units: Vec<SourceUnit> = files
            .iter()
            .zip(inputs.iter())
            .map(|(f, i)| SourceUnit { module: compile::module_of(&i.rel_path).0, file: &f.ast })
            .collect();
        let krate = sema::collect(&units, &opts.crate_name, opts.extern_crates.clone(), false);
        let mut diags = HashMap::new();
        let mut rust = HashMap::new();
        let mut decisions = HashMap::new();
        for (k, (path, text, ds)) in output.diags.iter().enumerate() {
            let idx = LineIndex::new(text);
            let list: Vec<Value> = ds
                .iter()
                .map(|d| {
                    let mut msg = d.message.clone();
                    if let Some(h) = &d.help {
                        msg.push_str(&format!("\nhelp: {}", h));
                    }
                    let mut v = json!({
                        "range": range(text, &idx, d.span.start as usize, d.span.end as usize),
                        "severity": if d.severity == Severity::Error { 1 } else { 2 },
                        "source": "oxie",
                        "message": msg,
                    });
                    // its quick fixes, as `code_actions` reads them
                    if !d.fixes.is_empty() {
                        let fixes: Vec<Value> = d
                            .fixes
                            .iter()
                            .map(|f| {
                                let edits: Vec<Value> = f.edits.iter().map(|(s, t)| json!({"range": range(text, &idx, s.start as usize, s.end as usize), "newText": t})).collect();
                                json!({"title": f.title, "edits": edits})
                            })
                            .collect();
                        v["data"] = json!({ "fixes": fixes });
                    }
                    v
                })
                .collect();
            diags.insert(canonical(path), list);
            if let Some(g) = output.files.get(k) {
                rust.insert(canonical(path), g.text.clone());
                decisions.insert(canonical(path), g.decisions.clone());
            }
        }
        Analysis { root, is_script, files, krate, diags, rust, decisions }
    }

    fn publish(&self, files: &HashSet<PathBuf>) {
        let oxie = self.oxie_diags.lock().unwrap();
        let cg = self.cargo_diags.lock().unwrap();
        for f in files {
            let mut all: Vec<Value> = oxie.get(f).cloned().unwrap_or_default();
            all.extend(cg.get(f).cloned().unwrap_or_default());
            notify(&self.out, "textDocument/publishDiagnostics", json!({"uri": path_to_uri(f), "diagnostics": all}));
        }
    }

    fn refresh_oxie_diags(&self, a: &Analysis) {
        let mut touched = HashSet::new();
        {
            let mut map = self.oxie_diags.lock().unwrap();
            for f in &a.files {
                let d = a.diags.get(&f.path).cloned().unwrap_or_default();
                map.insert(f.path.clone(), d);
                touched.insert(f.path.clone());
            }
        }
        self.publish(&touched);
    }

    fn run_cargo_check(&self, a: &Analysis) {
        if !self.cargo_check_enabled {
            return;
        }
        // scripts: build through the hidden script project
        let (root, files) = if a.is_script {
            let Some(f) = a.files.first() else { return };
            match project::script_project(&f.path) {
                Ok(p) => (p.root, vec![f.path.clone()]),
                Err(_) => return,
            }
        } else {
            (a.root.clone(), a.files.iter().map(|f| f.path.clone()).collect())
        };
        if a.diags.values().any(|d| d.iter().any(|x| x["severity"] == 1)) {
            // Oxie errors already reported; rustc would only repeat them
            return;
        }
        {
            let mut checks = self.checks.lock().unwrap();
            checks.pending.retain(|c| c.root != root);
            checks.pending.push(Check { root, files, is_script: a.is_script });
            if checks.running {
                return;
            }
            checks.running = true;
        }
        let out = self.out.clone();
        let cargo_diags = self.cargo_diags.clone();
        let oxie_diags = self.oxie_diags.clone();
        let checks = self.checks.clone();
        std::thread::spawn(move || loop {
            let Check { root, files, is_script } = {
                let mut checks = checks.lock().unwrap();
                if checks.pending.is_empty() {
                    checks.running = false;
                    break;
                }
                checks.pending.remove(0)
            };
            notify(&out, "$/progress", json!({"token": "oxie-check", "value": {"kind": "begin", "title": "cargo check"}}));
            // scripts share one target directory
            let target_dir = is_script.then(|| project::cache_dir().join("target"));
            let result = cargo::check_collect(&root, target_dir.as_deref()).unwrap_or_default();
            let mut by_file: HashMap<PathBuf, Vec<Value>> = HashMap::new();
            for d in result {
                // a compiler bug inside the runtime module has no .oxi file to show it on
                if !d.from_oxie || d.file.extension().is_none_or(|e| e != "oxi") {
                    continue;
                }
                let text = std::fs::read_to_string(&d.file).unwrap_or_default();
                let idx = LineIndex::new(&text);
                let line_text = text.lines().nth(d.line as usize).unwrap_or("");
                let to_utf16 = |col: u32| line_text.chars().take(col as usize).map(|c| c.len_utf16()).sum::<usize>();
                let _ = idx;
                let mut message = d.message.clone();
                if let Some(l) = &d.label {
                    if !l.is_empty() {
                        message.push_str(&format!("\n{}", l));
                    }
                }
                for n in &d.notes {
                    message.push_str(&format!("\n{}", n));
                }
                if let Some(m) = &d.rust_message {
                    message.push_str(&format!("\nrustc: {}", m));
                }
                let end_line_text = text.lines().nth(d.end_line as usize).unwrap_or("");
                let end_col: usize = end_line_text.chars().take(d.end_col as usize).map(|c| c.len_utf16()).sum();
                let uri = path_to_uri(&d.file);
                // labels and edits may be in another file of the crate
                let in_file = |f: &Option<PathBuf>| match f {
                    Some(f) => (path_to_uri(f), std::fs::read_to_string(f).unwrap_or_default()),
                    None => (uri.clone(), text.clone()),
                };
                let related: Vec<Value> = d
                    .secondary
                    .iter()
                    .map(|s| {
                        let (uri, text) = in_file(&s.file);
                        json!({"location": {"uri": uri, "range": char_range(&text, s.line, s.col, s.line, s.end_col)}, "message": s.text})
                    })
                    .collect();
                let fixes: Vec<Value> = d
                    .fixes
                    .iter()
                    .map(|f| {
                        let edits: Vec<Value> = f
                            .edits
                            .iter()
                            .map(|e| {
                                let (euri, text) = in_file(&e.file);
                                let mut edit = json!({"range": char_range(&text, e.line, e.col, e.end_line, e.end_col), "newText": e.text});
                                // one in the diagnostic's own file goes to the document the editor named
                                if e.file.is_some() {
                                    edit["uri"] = json!(euri);
                                }
                                edit
                            })
                            .collect();
                        json!({"title": f.title, "edits": edits})
                    })
                    .collect();
                let mut v = json!({
                    "range": {"start": {"line": d.line, "character": to_utf16(d.col)}, "end": {"line": d.end_line, "character": end_col}},
                    "severity": match d.level.as_str() { "error" => 1, "warning" => 2, _ => 3 },
                    "source": "rustc",
                    "code": d.code,
                    "message": message,
                });
                if !related.is_empty() {
                    v["relatedInformation"] = json!(related);
                }
                if !fixes.is_empty() {
                    v["data"] = json!({"fixes": fixes});
                }
                by_file.entry(canonical(&d.file)).or_default().push(v);
            }
            let mut touched: HashSet<PathBuf> = files.iter().cloned().collect();
            {
                let mut cg = cargo_diags.lock().unwrap();
                for f in &files {
                    if cg.remove(f).is_some() {
                        touched.insert(f.clone());
                    }
                }
                for (f, v) in by_file {
                    touched.insert(f.clone());
                    cg.insert(f, v);
                }
            }
            {
                let oxie = oxie_diags.lock().unwrap();
                let cg = cargo_diags.lock().unwrap();
                for f in &touched {
                    let mut all: Vec<Value> = oxie.get(f).cloned().unwrap_or_default();
                    all.extend(cg.get(f).cloned().unwrap_or_default());
                    notify(&out, "textDocument/publishDiagnostics", json!({"uri": path_to_uri(f), "diagnostics": all}));
                }
            }
            notify(&out, "$/progress", json!({"token": "oxie-check", "value": {"kind": "end"}}));
        });
    }
}

// ---------------------------------------------------------------------------------------------
// text helpers

fn is_ident_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// The identifier at `offset` and its byte range.
fn word_at(text: &str, offset: usize) -> Option<(String, usize, usize)> {
    let offset = offset.min(text.len());
    let mut s = offset;
    while s > 0 {
        let c = text[..s].chars().next_back().unwrap();
        if is_ident_char(c) {
            s -= c.len_utf8();
        } else {
            break;
        }
    }
    let mut e = offset;
    while e < text.len() {
        let c = text[e..].chars().next().unwrap();
        if is_ident_char(c) {
            e += c.len_utf8();
        } else {
            break;
        }
    }
    if s == e {
        return None;
    }
    Some((text[s..e].to_string(), s, e))
}

/// For `recv.na|`, returns the receiver text before the dot.
fn receiver_before(text: &str, word_start: usize) -> Option<String> {
    let before = text[..word_start].trim_end_matches(' ');
    let before = before.strip_suffix('.').or_else(|| before.strip_suffix("?."))?;
    let b = before.trim_end();
    if b.ends_with(')') || b.ends_with(']') {
        return Some(String::from("<expr>"));
    }
    let mut s = b.len();
    while s > 0 {
        let c = b[..s].chars().next_back().unwrap();
        if is_ident_char(c) {
            s -= c.len_utf8();
        } else {
            break;
        }
    }
    Some(b[s..].to_string())
}

fn kind_num(k: SymKind) -> u32 {
    // LSP SymbolKind
    match k {
        SymKind::Function => 12,
        SymKind::Method => 6,
        SymKind::Class => 5,
        SymKind::Struct => 23,
        SymKind::Enum => 10,
        SymKind::Interface => 11,
        SymKind::Variant => 22,
        SymKind::Field => 8,
        SymKind::Const => 14,
        SymKind::TypeAlias => 26,
        SymKind::Module => 2,
        SymKind::Test => 12,
        SymKind::Variable => 13,
        SymKind::Parameter => 13,
    }
}

fn completion_kind(k: SymKind) -> u32 {
    // LSP CompletionItemKind
    match k {
        SymKind::Function => 3,
        SymKind::Method => 2,
        SymKind::Class | SymKind::Struct => 7,
        SymKind::Enum => 13,
        SymKind::Interface => 8,
        SymKind::Variant => 20,
        SymKind::Field => 5,
        SymKind::Const => 21,
        SymKind::TypeAlias => 7,
        SymKind::Module => 9,
        SymKind::Test => 3,
        SymKind::Variable | SymKind::Parameter => 6,
    }
}

const KEYWORDS: &[&str] = &[
    "fun", "val", "var", "class", "data", "enum", "interface", "object", "when", "if", "else", "for", "in", "while", "do",
    "loop", "return", "break", "continue", "throw", "throws", "try", "catch", "import", "const", "typealias", "impl", "this",
    "null", "true", "false", "is", "as", "override", "operator", "static", "mut", "async", "await", "move", "private",
    "internal", "test", "error", "companion", "init", "match", "unsafe", "where", "struct", "trait", "mod",
];

const BUILTINS: &[(&str, &str)] = &[
    ("println", "println(${1:value})"),
    ("print", "print(${1:value})"),
    ("eprintln", "eprintln(${1:value})"),
    ("format", "format(\"${1}\")"),
    ("listOf", "listOf(${1})"),
    ("mutableListOf", "mutableListOf(${1})"),
    ("mapOf", "mapOf(${1:key} to ${2:value})"),
    ("setOf", "setOf(${1})"),
    ("assert", "assert(${1:condition})"),
    ("assert_eq", "assert_eq(${1:actual}, ${2:expected})"),
    ("panic", "panic(\"${1:message}\")"),
    ("todo", "todo()"),
    ("repeat", "repeat(${1:times}) {\n\t$0\n}"),
    ("run", "run {\n\t$0\n}"),
    ("dbg", "dbg(${1:value})"),
];

const STD_TYPES: &[&str] = &[
    "String", "Vec", "HashMap", "HashSet", "BTreeMap", "BTreeSet", "VecDeque", "Option", "Result", "Box", "Rc", "Arc",
    "RefCell", "Cell", "Mutex", "RwLock", "Some", "None", "Ok", "Err", "AnyError", "i8", "i16", "i32", "i64", "i128",
    "isize", "u8", "u16", "u32", "u64", "u128", "usize", "f32", "f64", "bool", "char", "str", "Self",
];

const COMMON_METHODS: &[&str] = &[
    "len", "is_empty", "push", "pop", "insert", "remove", "get", "contains", "iter", "iter_mut", "into_iter", "map", "filter",
    "filter_map", "flat_map", "fold", "for_each", "collect", "sum", "count", "min", "max", "find", "any", "all", "enumerate",
    "zip", "rev", "take", "skip", "chain", "sort", "sort_by", "sort_by_key", "dedup", "clone", "to_string", "to_owned", "to_list",
    "as_str", "trim", "split", "lines", "chars", "parse", "to_uppercase", "to_lowercase", "starts_with", "ends_with",
    "replace", "join", "unwrap", "expect", "unwrap_or", "unwrap_or_default", "ok", "map_err", "and_then", "is_some",
    "is_none", "is_ok", "is_err", "keys", "values", "entry", "or_insert", "context", "first", "last", "extend", "retain",
    "as_ref", "as_mut", "borrow", "borrow_mut", "lock", "await", "let",
];

const SNIPPETS: &[(&str, &str, &str)] = &[
    ("fun", "fun ${1:name}(${2}): ${3:Unit} {\n\t$0\n}", "function"),
    ("main", "fun main() {\n\t$0\n}", "main function"),
    ("class", "class ${1:Name}(${2:val x: i32}) {\n\t$0\n}", "class"),
    ("data class", "data class ${1:Name}(${2:x: i32})", "data class"),
    ("enum", "enum ${1:Name} {\n\t${2:A}\n\t${3:B}\n}", "enum"),
    ("interface", "interface ${1:Name} {\n\tfun ${2:method}(): ${3:Unit}\n}", "interface"),
    ("when", "when (${1:value}) {\n\t${2:pattern} -> ${3}\n\telse -> ${0}\n}", "when expression"),
    ("if", "if (${1:condition}) {\n\t$0\n}", "if"),
    ("for", "for (${1:item} in ${2:items}) {\n\t$0\n}", "for loop"),
    ("while", "while (${1:condition}) {\n\t$0\n}", "while loop"),
    ("try", "try {\n\t$1\n} catch (e) {\n\t$0\n}", "try / catch"),
    ("test", "test \"${1:description}\" {\n\t$0\n}", "test block"),
    ("error enum", "error enum ${1:MyError} {\n\t${2:NotFound}(${3:name}: String) = \"${4:not found: }\\$${3:name}\"\n}", "error type"),
];

// ---------------------------------------------------------------------------------------------
// request handlers

fn position(params: &Value) -> (PathBuf, u32, u32) {
    let uri = params["textDocument"]["uri"].as_str().unwrap_or("");
    let line = params["position"]["line"].as_u64().unwrap_or(0) as u32;
    let ch = params["position"]["character"].as_u64().unwrap_or(0) as u32;
    (canonical(&uri_to_path(uri)), line, ch)
}

/// Resolves the declaration(s) for the word at `offset`.
fn resolve(a: &Analysis, f: &FileData, offset: usize) -> Vec<(PathBuf, Symbol)> {
    let Some((word, ws, _)) = word_at(&f.text, offset) else { return Vec::new() };
    let mut out = Vec::new();
    let recv = receiver_before(&f.text, ws);
    let all: Vec<(PathBuf, &Symbol)> = a.files.iter().flat_map(|fd| symbols::flatten(&fd.symbols).into_iter().map(move |s| (fd.path.clone(), s))).collect();
    if let Some(r) = recv {
        // member access: prefer the receiver's declared type
        let owner: Option<String> = if r == "this" || r == "self" {
            symbols::enclosing_type(&f.ast, offset as u32).map(|t| t.name.clone())
        } else if r.chars().next().is_some_and(|c| c.is_uppercase()) {
            Some(r.clone())
        } else {
            symbols::locals_at(&f.ast, offset as u32)
                .into_iter()
                .rev()
                .find(|l| l.name == r)
                .and_then(|l| l.ty)
                .and_then(|t| type_owner(&t))
        };
        for (p, s) in &all {
            if s.name == word && s.container.is_some() && (owner.is_none() || s.container == owner) {
                out.push((p.clone(), (*s).clone()));
            }
        }
        if out.is_empty() && owner.is_some() {
            for (p, s) in &all {
                if s.name == word && s.container.is_some() {
                    out.push((p.clone(), (*s).clone()));
                }
            }
        }
        return out;
    }
    // locals shadow everything
    if let Some(l) = symbols::locals_at(&f.ast, offset as u32).into_iter().rev().find(|l| l.name == word) {
        return vec![(f.path.clone(), local_symbol(&l))];
    }
    // members of the enclosing class (implicit this)
    if let Some(t) = symbols::enclosing_type(&f.ast, offset as u32) {
        for (p, s) in &all {
            if s.name == word && s.container.as_deref() == Some(t.name.as_str()) {
                out.push((p.clone(), (*s).clone()));
            }
        }
        if !out.is_empty() {
            return out;
        }
    }
    for (p, s) in &all {
        if s.name == word && (s.container.is_none() || s.kind == SymKind::Variant) {
            out.push((p.clone(), (*s).clone()));
        }
    }
    out
}

fn type_owner(t: &Type) -> Option<String> {
    match t {
        Type::Ref { inner, .. } | Type::Option(inner) => type_owner(inner),
        _ => t.last_name().map(|s| s.to_string()),
    }
}

fn local_symbol(l: &Local) -> Symbol {
    let detail = match &l.ty {
        Some(t) => format!("{}: {}", l.name, symbols::type_text(t)),
        None => l.name.clone(),
    };
    Symbol {
        name: l.name.clone(),
        kind: if l.is_param { SymKind::Parameter } else { SymKind::Variable },
        detail,
        docs: String::new(),
        span: l.span,
        full: l.span,
        container: None,
        children: Vec::new(),
    }
}

fn location(a: &Analysis, path: &Path, span: Span) -> Value {
    let (text, idx) = match a.file(path) {
        Some(f) => (f.text.clone(), LineIndex::new(&f.text)),
        None => {
            let t = std::fs::read_to_string(path).unwrap_or_default();
            let i = LineIndex::new(&t);
            (t, i)
        }
    };
    json!({"uri": path_to_uri(path), "range": range(&text, &idx, span.start as usize, span.end as usize)})
}

fn hover_markdown(s: &Symbol, rust: Option<&str>, decided: &[String]) -> String {
    let mut md = format!("```oxie\n{}\n```", s.detail);
    if let Some(c) = &s.container {
        if s.kind != SymKind::Variant {
            md = format!("```oxie\n{} // in {}\n```", s.detail, c);
        }
    }
    if !s.docs.is_empty() {
        md.push_str("\n\n");
        md.push_str(&s.docs);
    }
    if let Some(r) = rust {
        md.push_str(&format!("\n\n---\nCompiles to:\n```rust\n{}\n```", r));
    }
    if !decided.is_empty() {
        md.push_str("\n\nOxie decided:");
        for d in decided {
            md.push_str(&format!("\n- {}", d));
        }
    }
    md
}

/// What Oxie decided about a function's or method's signature (an inferred `&mut self`, a lent
/// or kept parameter, a lifetime), each with its reason and the line that decided it.
fn signature_decisions(a: &Analysis, path: &Path, s: &Symbol) -> Vec<String> {
    if !matches!(s.kind, SymKind::Function | SymKind::Method) {
        return Vec::new();
    }
    let Some(f) = a.file(path) else { return Vec::new() };
    let Some((_, fun)) = symbols::enclosing_fun(&f.ast, s.span.start).filter(|(_, fun)| fun.name_span == s.span) else { return Vec::new() };
    let params: Vec<Span> = fun.params.iter().map(|p| p.span).collect();
    a.decisions
        .get(path)
        .into_iter()
        .flatten()
        .filter(|d| d.span == fun.name_span || params.contains(&d.span))
        .map(|d| {
            let line = d.cause.map(|c| format!(" (line {})", f.idx.line_col(&f.text, c.start as usize).0 + 1)).unwrap_or_default();
            // `&mut self`, `s: &str`, `v` stays owned, `'a`
            let what = match params.iter().position(|p| *p == d.span) {
                Some(i) if d.chosen == "owned" => format!("`{}` stays owned", fun.params[i].name().unwrap_or("_")),
                Some(i) => format!("`{}: {}`", fun.params[i].name().unwrap_or("_"), d.chosen),
                None => format!("`{}`", d.chosen),
            };
            format!("{}: {}{}", what, d.why, line)
        })
        .collect()
}

/// The generated Rust signature line for a symbol, found by name in the generated file.
fn rust_signature(a: &Analysis, path: &Path, s: &Symbol) -> Option<String> {
    let rust = a.rust.get(path)?;
    let needles: Vec<String> = match s.kind {
        SymKind::Function | SymKind::Method | SymKind::Test => vec![format!("fn {}(", s.name), format!("fn {}<", s.name)],
        SymKind::Class | SymKind::Struct => vec![format!("struct {} ", s.name), format!("struct {}<", s.name), format!("struct {};", s.name), format!("struct {}(", s.name)],
        SymKind::Enum => vec![format!("enum {} ", s.name), format!("enum {}<", s.name)],
        SymKind::Interface => vec![format!("trait {} ", s.name), format!("trait {}<", s.name), format!("trait {}:", s.name)],
        _ => return None,
    };
    for line in rust.lines() {
        if needles.iter().any(|n| line.contains(n.as_str())) {
            return Some(line.trim().trim_end_matches('{').trim_end().to_string());
        }
    }
    None
}

fn handle(server: &mut Server, method: &str, params: &Value) -> Result<Value, String> {
    match method {
        "textDocument/hover" => {
            let (path, line, ch) = position(params);
            let a = server.analyze(&path);
            let Some(f) = a.file(&path) else { return Ok(Value::Null) };
            let off = f.idx.offset(&f.text, line as usize, ch as usize);
            let found = resolve(&a, f, off);
            let Some((p, s)) = found.first() else {
                if let Some((w, _, _)) = word_at(&f.text, off) {
                    if let Some(doc) = keyword_doc(&w) {
                        return Ok(json!({"contents": {"kind": "markdown", "value": doc}}));
                    }
                }
                return Ok(Value::Null);
            };
            let rust = rust_signature(&a, p, s);
            let decided = signature_decisions(&a, p, s);
            Ok(json!({"contents": {"kind": "markdown", "value": hover_markdown(s, rust.as_deref(), &decided)}}))
        }
        "textDocument/definition" | "textDocument/declaration" | "textDocument/typeDefinition" | "textDocument/implementation" => {
            let (path, line, ch) = position(params);
            let a = server.analyze(&path);
            let Some(f) = a.file(&path) else { return Ok(Value::Null) };
            let off = f.idx.offset(&f.text, line as usize, ch as usize);
            let locs: Vec<Value> = resolve(&a, f, off).iter().map(|(p, s)| location(&a, p, s.span)).collect();
            Ok(Value::Array(locs))
        }
        "textDocument/references" => {
            let (path, line, ch) = position(params);
            let a = server.analyze(&path);
            let Some(f) = a.file(&path) else { return Ok(Value::Null) };
            let off = f.idx.offset(&f.text, line as usize, ch as usize);
            let Some((word, _, _)) = word_at(&f.text, off) else { return Ok(json!([])) };
            let is_local = resolve(&a, f, off).first().is_some_and(|(_, s)| matches!(s.kind, SymKind::Variable | SymKind::Parameter));
            let mut locs = Vec::new();
            for fd in &a.files {
                if is_local && fd.path != f.path {
                    continue;
                }
                for (s, e) in occurrences(&fd.text, &word) {
                    if is_local {
                        // only inside the same function
                        let fun_here = symbols::enclosing_fun(&f.ast, off as u32).map(|(_, x)| x.name_span);
                        let fun_there = symbols::enclosing_fun(&fd.ast, s as u32).map(|(_, x)| x.name_span);
                        if fun_here != fun_there {
                            continue;
                        }
                    }
                    locs.push(json!({"uri": path_to_uri(&fd.path), "range": range(&fd.text, &fd.idx, s, e)}));
                }
            }
            Ok(Value::Array(locs))
        }
        "textDocument/documentHighlight" => {
            let (path, line, ch) = position(params);
            let a = server.analyze(&path);
            let Some(f) = a.file(&path) else { return Ok(Value::Null) };
            let off = f.idx.offset(&f.text, line as usize, ch as usize);
            let Some((word, _, _)) = word_at(&f.text, off) else { return Ok(json!([])) };
            let hl: Vec<Value> = occurrences(&f.text, &word).into_iter().map(|(s, e)| json!({"range": range(&f.text, &f.idx, s, e), "kind": 1})).collect();
            Ok(Value::Array(hl))
        }
        "textDocument/rename" => {
            let (path, line, ch) = position(params);
            let new_name = params["newName"].as_str().unwrap_or("").to_string();
            if new_name.is_empty() || !new_name.chars().all(is_ident_char) || parser::is_reserved(&new_name) {
                return Err(format!("`{}` is not a valid name", new_name));
            }
            let a = server.analyze(&path);
            let Some(f) = a.file(&path) else { return Ok(Value::Null) };
            let off = f.idx.offset(&f.text, line as usize, ch as usize);
            let Some((word, _, _)) = word_at(&f.text, off) else { return Ok(Value::Null) };
            let decl = resolve(&a, f, off);
            if decl.is_empty() {
                return Err(format!("`{}` is not declared in this project", word));
            }
            let is_local = matches!(decl[0].1.kind, SymKind::Variable | SymKind::Parameter);
            let mut changes = serde_json::Map::new();
            for fd in &a.files {
                if is_local && fd.path != f.path {
                    continue;
                }
                let fun_here = symbols::enclosing_fun(&f.ast, off as u32).map(|(_, x)| x.name_span);
                let edits: Vec<Value> = occurrences(&fd.text, &word)
                    .into_iter()
                    .filter(|(s, _)| !is_local || symbols::enclosing_fun(&fd.ast, *s as u32).map(|(_, x)| x.name_span) == fun_here)
                    .map(|(s, e)| json!({"range": range(&fd.text, &fd.idx, s, e), "newText": new_name}))
                    .collect();
                if !edits.is_empty() {
                    changes.insert(path_to_uri(&fd.path), Value::Array(edits));
                }
            }
            Ok(json!({"changes": changes}))
        }
        "textDocument/documentSymbol" => {
            let uri = params["textDocument"]["uri"].as_str().unwrap_or("");
            let path = canonical(&uri_to_path(uri));
            let text = server.docs.get(&path).cloned().or_else(|| std::fs::read_to_string(&path).ok()).unwrap_or_default();
            let (ast, _) = parser::parse_file(&text);
            let idx = LineIndex::new(&text);
            let syms = symbols::file_symbols(&ast, &text);
            fn conv(s: &Symbol, text: &str, idx: &LineIndex) -> Value {
                json!({
                    "name": s.name,
                    "detail": s.detail,
                    "kind": kind_num(s.kind),
                    "range": range(text, idx, s.full.start as usize, s.full.end as usize),
                    "selectionRange": range(text, idx, s.span.start as usize, s.span.end as usize),
                    "children": s.children.iter().map(|c| conv(c, text, idx)).collect::<Vec<_>>(),
                })
            }
            Ok(Value::Array(syms.iter().map(|s| conv(s, &text, &idx)).collect()))
        }
        "workspace/symbol" => {
            let query = params["query"].as_str().unwrap_or("").to_lowercase();
            let Some(any) = server.docs.keys().next().cloned() else { return Ok(json!([])) };
            let a = server.analyze(&any);
            let mut out = Vec::new();
            for fd in &a.files {
                for s in symbols::flatten(&fd.symbols) {
                    if s.name.to_lowercase().contains(&query) {
                        out.push(json!({
                            "name": s.name,
                            "kind": kind_num(s.kind),
                            "containerName": s.container,
                            "location": {"uri": path_to_uri(&fd.path), "range": range(&fd.text, &fd.idx, s.span.start as usize, s.span.end as usize)},
                        }));
                    }
                }
            }
            Ok(Value::Array(out))
        }
        "textDocument/formatting" | "textDocument/rangeFormatting" => {
            let uri = params["textDocument"]["uri"].as_str().unwrap_or("");
            let path = canonical(&uri_to_path(uri));
            let text = server.docs.get(&path).cloned().or_else(|| std::fs::read_to_string(&path).ok()).unwrap_or_default();
            let formatted = oxie_compiler::format::format_source(&text);
            if formatted == text {
                return Ok(json!([]));
            }
            let idx = LineIndex::new(&text);
            Ok(json!([{"range": range(&text, &idx, 0, text.len()), "newText": formatted}]))
        }
        "textDocument/completion" => completion(server, params),
        "completionItem/resolve" => Ok(params.clone()),
        "textDocument/signatureHelp" => signature_help(server, params),
        "textDocument/inlayHint" => inlay_hints(server, params),
        "textDocument/codeAction" => Ok(code_actions(server, params)),
        "workspace/executeCommand" => {
            let cmd = params["command"].as_str().unwrap_or("");
            if cmd == "oxie.showRust" {
                let uri = params["arguments"][0].as_str().unwrap_or("");
                let path = canonical(&uri_to_path(uri));
                let a = server.analyze(&path);
                return Ok(json!(a.rust.get(&path).cloned().unwrap_or_default()));
            }
            Ok(Value::Null)
        }
        "shutdown" => Ok(Value::Null),
        _ => Err(format!("unsupported method {}", method)),
    }
}

/// Quick fixes for the rustc diagnostics in the requested range. Fixes travel in each
/// diagnostic's `data`; clients that drop `data` get them from the last published diagnostics.
fn code_actions(server: &Server, params: &Value) -> Value {
    let uri = params["textDocument"]["uri"].as_str().unwrap_or("");
    let path = canonical(&uri_to_path(uri));
    let mut diags: Vec<Value> = params["context"]["diagnostics"].as_array().cloned().unwrap_or_default();
    let mut published = server.cargo_diags.lock().unwrap().get(&path).cloned().unwrap_or_default();
    published.extend(server.oxie_diags.lock().unwrap().get(&path).cloned().unwrap_or_default());
    for d in diags.iter_mut() {
        if d.get("data").is_none() {
            if let Some(p) = published.iter().find(|p| p["range"] == d["range"] && p["message"] == d["message"]) {
                d["data"] = p["data"].clone();
            }
        }
    }
    if diags.is_empty() {
        // no diagnostics sent: offer fixes for published ones touching the requested lines
        let (sl, el) = (params["range"]["start"]["line"].as_u64().unwrap_or(0), params["range"]["end"]["line"].as_u64().unwrap_or(0));
        diags = published.into_iter().filter(|p| p["range"]["start"]["line"].as_u64().is_some_and(|l| l >= sl && l <= el)).collect();
    }
    let mut actions = Vec::new();
    for d in &diags {
        let Some(fixes) = d["data"]["fixes"].as_array() else { continue };
        for (i, f) in fixes.iter().enumerate() {
            let mut diag = d.clone();
            if let Some(o) = diag.as_object_mut() {
                o.remove("data");
            }
            // edits by file: the diagnostic's, or another file of the crate
            let mut changes = serde_json::Map::new();
            for e in f["edits"].as_array().into_iter().flatten() {
                let target = e["uri"].as_str().unwrap_or(uri).to_string();
                if let Value::Array(list) = changes.entry(target).or_insert_with(|| json!([])) {
                    list.push(json!({"range": e["range"], "newText": e["newText"]}));
                }
            }
            actions.push(json!({
                "title": f["title"].as_str().map(capitalize).unwrap_or_default(),
                "kind": "quickfix",
                "diagnostics": [diag],
                "isPreferred": i == 0,
                "edit": {"changes": changes},
            }));
        }
    }
    json!(actions)
}

fn capitalize(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_uppercase().chain(c).collect(),
        None => String::new(),
    }
}

fn keyword_doc(w: &str) -> Option<String> {
    let d = match w {
        "val" => "`val name = value` declares an immutable variable (Rust `let`).",
        "var" => "`var name = value` declares a mutable variable (Rust `let mut`).",
        "fun" => "`fun name(params): Type { ... }` declares a function. `fun f() = expr` returns `expr`.",
        "when" => "`when (x) { pattern -> value ... else -> value }` matches a value (Rust `match`). Without a subject it is an if/else chain.",
        "throws" => "`throws E` makes the function return `Result<T, E>`. Plain `throws` uses `AnyError`. Return values are wrapped in `Ok` automatically and `throw e` returns `Err(e)`.",
        "throw" => "`throw e` returns `Err(e.into())` from the current function (or jumps to the enclosing `catch`).",
        "try" => "`try { ... } catch (e) { ... }` handles errors raised with `?` or `throw` in the block. Without `catch`, `try { }` evaluates to a `Result`.",
        "data" => "`data class` gets `Debug`, `Clone`, `PartialEq` (and `Copy`, `Eq`, `Hash` when possible) automatically, plus a constructor and `copy(field = value)`.",
        "error" => "`error enum` / `error class` declares an error type: messages become `Display`, and `std::error::Error` and `From` conversions are implemented.",
        "null" => "`null` is `None`. `T?` is `Option<T>`.",
        "interface" => "`interface` is a Rust trait. Methods without a body are required; others are default methods.",
        "object" => "`object Name { ... }` is a namespace of constants and functions (a Rust module).",
        "companion" => "`companion object { ... }` holds static functions of a class (called as `Class.fun()`).",
        "init" => "`init { ... }` runs inside the generated constructor after fields are set.",
        "operator" => "`operator fun plus(other: T)` implements `+` (also `minus`, `times`, `div`, `rem`, `unaryMinus`, `not`, `plusAssign`...).",
        "override" => "`override fun` implements a method of an interface listed after `:`.",
        "test" => "`test \"name\" { ... }` declares a unit test, run with `oxie test`.",
        "is" => "`x is Pattern` checks whether a value matches a pattern (Rust `matches!`).",
        "this" => "`this` is `self`. Inside methods, fields and methods can be used without `this.`.",
        _ => return None,
    };
    Some(d.to_string())
}

/// Whole-word occurrences of `word` outside strings and comments (approximate).
fn occurrences(text: &str, word: &str) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let bytes = text.as_bytes();
    let mut i = 0;
    let mut in_str = false;
    let mut in_line_comment = false;
    while i < bytes.len() {
        let c = bytes[i];
        if in_line_comment {
            if c == b'\n' {
                in_line_comment = false;
            }
            i += 1;
            continue;
        }
        if in_str {
            if c == b'\\' {
                i += 2;
                continue;
            }
            if c == b'"' {
                in_str = false;
            }
            // interpolations `$name` / `${...}` reference identifiers
            if c == b'$' && i + 1 < bytes.len() {
                let rest = &text[i + 1..];
                let r = if rest.starts_with('{') { &rest[1..] } else { rest };
                let off = i + 1 + (rest.len() - r.len());
                if r.starts_with(word) && !r[word.len()..].starts_with(|ch: char| is_ident_char(ch)) {
                    out.push((off, off + word.len()));
                }
            }
            i += 1;
            continue;
        }
        if c == b'/' && i + 1 < bytes.len() && bytes[i + 1] == b'/' {
            in_line_comment = true;
            i += 2;
            continue;
        }
        if c == b'"' {
            in_str = true;
            i += 1;
            continue;
        }
        // compared as bytes: `i` may be inside a character (`'€'`); a match starts at a whole one
        if bytes[i..].starts_with(word.as_bytes()) && text.is_char_boundary(i) {
            let before_ok = i == 0 || !is_ident_char(text[..i].chars().next_back().unwrap());
            let after_ok = !text[i + word.len()..].starts_with(|ch: char| is_ident_char(ch));
            if before_ok && after_ok {
                out.push((i, i + word.len()));
                i += word.len();
                continue;
            }
        }
        i += 1;
    }
    out
}

fn completion(server: &Server, params: &Value) -> Result<Value, String> {
    let (path, line, ch) = position(params);
    let a = server.analyze(&path);
    let Some(f) = a.file(&path) else { return Ok(json!([])) };
    let off = f.idx.offset(&f.text, line as usize, ch as usize);
    let word_start = {
        let mut s = off;
        while s > 0 {
            let c = f.text[..s].chars().next_back().unwrap();
            if is_ident_char(c) {
                s -= c.len_utf8();
            } else {
                break;
            }
        }
        s
    };
    let mut items: Vec<Value> = Vec::new();
    let mut seen = HashSet::new();
    let mut add = |items: &mut Vec<Value>, label: &str, kind: u32, detail: &str, doc: &str, insert: Option<&str>, sort: &str| {
        if !seen.insert(label.to_string()) {
            return;
        }
        let mut item = json!({"label": label, "kind": kind, "detail": detail, "sortText": format!("{}{}", sort, label)});
        if !doc.is_empty() {
            item["documentation"] = json!({"kind": "markdown", "value": doc});
        }
        if let Some(text) = insert {
            item["insertText"] = json!(text);
            item["insertTextFormat"] = json!(2);
        }
        items.push(item);
    };
    let line_start = f.text[..word_start].rfind('\n').map(|i| i + 1).unwrap_or(0);
    let line_prefix = f.text[line_start..word_start].trim_start();
    if line_prefix.starts_with("import ") || line_prefix.starts_with("use ") {
        for m in &a.krate.modules {
            add(&mut items, m, 9, "module", "", None, "0");
        }
        for m in ["std", "core", "alloc", "crate", "super"].iter().chain(a.krate.extern_crates.iter().map(|s| s.as_str()).collect::<Vec<_>>().iter()) {
            add(&mut items, m, 9, "crate", "", None, "1");
        }
        for m in ["collections", "fs", "io", "env", "fmt", "time", "thread", "sync", "path", "process", "cmp", "iter", "mem", "rc", "cell", "str", "num", "ops", "error", "net"] {
            add(&mut items, m, 9, "std module", "", None, "2");
        }
        for fd in &a.files {
            for s in symbols::flatten(&fd.symbols) {
                if s.container.is_none() {
                    add(&mut items, &s.name, completion_kind(s.kind), &s.detail, &s.docs, None, "3");
                }
            }
        }
        return Ok(json!({"isIncomplete": false, "items": items}));
    }
    if let Some(r) = receiver_before(&f.text, word_start) {
        // member completion
        let owner: Option<String> = if r == "this" || r == "self" {
            symbols::enclosing_type(&f.ast, off as u32).map(|t| t.name.clone())
        } else if r.chars().next().is_some_and(|c| c.is_uppercase()) {
            Some(r.clone())
        } else {
            symbols::locals_at(&f.ast, off as u32).into_iter().rev().find(|l| l.name == r).and_then(|l| l.ty).and_then(|t| type_owner(&t))
        };
        let statik = r.chars().next().is_some_and(|c| c.is_uppercase());
        // a class's value also has the methods of its interfaces
        let interfaces: Vec<String> = match &owner {
            Some(o) if !statik => a.files.iter().flat_map(|fd| type_decls(&fd.ast.items)).filter(|t| t.name == *o).flat_map(|t| t.supertypes.iter().filter_map(|s| s.last_name().map(str::to_string))).collect(),
            _ => Vec::new(),
        };
        for fd in &a.files {
            for s in symbols::flatten(&fd.symbols) {
                let Some(c) = &s.container else { continue };
                if owner.as_deref().is_none_or(|o| o == c) || interfaces.contains(c) {
                    let is_static = s.detail.starts_with("static ") || s.kind == SymKind::Variant || (s.kind == SymKind::Method && method_is_static(&a.krate, c, &s.name));
                    if owner.is_some() && statik != is_static && s.kind != SymKind::Const {
                        continue;
                    }
                    let insert = if matches!(s.kind, SymKind::Method | SymKind::Function) { Some(format!("{}($0)", s.name)) } else { None };
                    add(&mut items, &s.name, completion_kind(s.kind), &s.detail, &s.docs, insert.as_deref(), "0");
                }
            }
        }
        // the methods of std types only for values of other types than the project's own
        let own = owner.as_deref().is_some_and(|o| a.krate.classes.contains_key(o) || a.krate.enums.contains_key(o));
        if !own {
            for m in COMMON_METHODS {
                add(&mut items, m, 2, "method", "", None, "5");
            }
        }
        return Ok(json!({"isIncomplete": false, "items": items}));
    }
    // locals
    for l in symbols::locals_at(&f.ast, off as u32).into_iter().rev() {
        let detail = l.ty.as_ref().map(symbols::type_text).unwrap_or_default();
        add(&mut items, &l.name, 6, &detail, "", None, "0");
    }
    // members of the enclosing class
    if let Some(t) = symbols::enclosing_type(&f.ast, off as u32) {
        for fd in &a.files {
            for s in symbols::flatten(&fd.symbols) {
                if s.container.as_deref() == Some(t.name.as_str()) {
                    add(&mut items, &s.name, completion_kind(s.kind), &s.detail, &s.docs, None, "1");
                }
            }
        }
    }
    // project declarations
    for fd in &a.files {
        for s in symbols::flatten(&fd.symbols) {
            if s.container.is_none() || s.kind == SymKind::Variant {
                let insert = if s.kind == SymKind::Function { Some(format!("{}($0)", s.name)) } else { None };
                add(&mut items, &s.name, completion_kind(s.kind), &s.detail, &s.docs, insert.as_deref(), "2");
            }
        }
    }
    for (name, snippet) in BUILTINS {
        add(&mut items, name, 3, "builtin", "", Some(snippet), "3");
    }
    for (label, body, detail) in SNIPPETS {
        let mut item = json!({"label": label, "kind": 15, "detail": detail, "insertText": body, "insertTextFormat": 2, "sortText": format!("4{}", label)});
        item["filterText"] = json!(label.replace(' ', ""));
        items.push(item);
    }
    for kw in KEYWORDS {
        add(&mut items, kw, 14, "keyword", "", None, "5");
    }
    for t in STD_TYPES {
        add(&mut items, t, 7, "std", "", None, "6");
    }
    Ok(json!({"isIncomplete": false, "items": items}))
}

/// The classes and enums declared in `items`, also inside modules.
fn type_decls(items: &[oxie_compiler::ast::Item]) -> Vec<&oxie_compiler::ast::TypeDecl> {
    use oxie_compiler::ast::ItemKind;
    items
        .iter()
        .flat_map(|it| match &it.kind {
            ItemKind::Type(t) => vec![t],
            ItemKind::Mod { items, .. } | ItemKind::Object { items, .. } => type_decls(items),
            _ => Vec::new(),
        })
        .collect()
}

fn method_is_static(k: &Crate, owner: &str, name: &str) -> bool {
    k.classes.get(owner).and_then(|c| c.methods.get(name)).or_else(|| k.enums.get(owner).and_then(|e| e.methods.get(name))).is_some_and(|m| m.recv == Recv::Static)
}

fn signature_help(server: &Server, params: &Value) -> Result<Value, String> {
    let (path, line, ch) = position(params);
    let a = server.analyze(&path);
    let Some(f) = a.file(&path) else { return Ok(Value::Null) };
    let off = f.idx.offset(&f.text, line as usize, ch as usize);
    // find the unclosed `(` before the cursor
    let bytes = f.text.as_bytes();
    let mut depth = 0i32;
    let mut commas = 0;
    let mut i = off;
    let mut open = None;
    while i > 0 {
        i -= 1;
        match bytes[i] {
            b')' | b']' | b'}' => depth += 1,
            b'(' | b'[' | b'{' => {
                if depth == 0 {
                    if bytes[i] == b'(' {
                        open = Some(i);
                    }
                    break;
                }
                depth -= 1;
            }
            b',' if depth == 0 => commas += 1,
            b'\n' if depth == 0 && i + 1 < off && f.text[..i].trim_end().ends_with(['{', ';']) => break,
            _ => {}
        }
    }
    let Some(open) = open else { return Ok(Value::Null) };
    let Some((name, ws, _)) = word_at(&f.text, open) else { return Ok(Value::Null) };
    let found = resolve(&a, f, ws);
    let Some((_, s)) = found.into_iter().find(|(_, s)| matches!(s.kind, SymKind::Function | SymKind::Method | SymKind::Class | SymKind::Variant)) else {
        return Ok(Value::Null);
    };
    let detail = s.detail.clone();
    // parameter labels from the text between the outermost parentheses of the signature
    let params_text = detail.find('(').and_then(|p| detail.rfind(')').map(|q| detail[p + 1..q].to_string())).unwrap_or_default();
    let mut labels = Vec::new();
    let mut d = 0;
    let mut cur = String::new();
    for c in params_text.chars() {
        match c {
            '(' | '<' | '[' => {
                d += 1;
                cur.push(c)
            }
            ')' | '>' | ']' => {
                d -= 1;
                cur.push(c)
            }
            ',' if d == 0 => {
                labels.push(cur.trim().to_string());
                cur.clear();
            }
            _ => cur.push(c),
        }
    }
    if !cur.trim().is_empty() {
        labels.push(cur.trim().to_string());
    }
    let labels: Vec<String> = labels.into_iter().filter(|l| !l.ends_with("self")).collect();
    let _ = name;
    Ok(json!({
        "signatures": [{"label": detail, "documentation": s.docs, "parameters": labels.iter().map(|l| json!({"label": l})).collect::<Vec<_>>()}],
        "activeSignature": 0,
        "activeParameter": commas,
    }))
}

/// Shows how methods receive `self` (`&self`, `&mut self`, static), which Oxie infers.
fn inlay_hints(server: &Server, params: &Value) -> Result<Value, String> {
    let uri = params["textDocument"]["uri"].as_str().unwrap_or("");
    let path = canonical(&uri_to_path(uri));
    let a = server.analyze(&path);
    let Some(f) = a.file(&path) else { return Ok(json!([])) };
    let mut hints = Vec::new();
    let mut add_for = |owner: &str, members: &[oxie_compiler::ast::Item]| {
        for m in members {
            let ItemKind::Fun(fd) = &m.kind else { continue };
            if fd.self_param.is_some() || fd.mods.is_static || fd.mods.is_mut || fd.mods.is_operator {
                continue;
            }
            let recv = a
                .krate
                .classes
                .get(owner)
                .and_then(|c| c.methods.get(&fd.name))
                .or_else(|| a.krate.enums.get(owner).and_then(|e| e.methods.get(&fd.name)))
                .map(|mi| mi.recv);
            let label = match recv {
                Some(Recv::Ref) => "&self",
                Some(Recv::RefMut) => "&mut self",
                Some(Recv::Static) => "static",
                Some(Recv::Value) => "self",
                None => continue,
            };
            // position: just after `(`
            let after_name = fd.name_span.end as usize;
            let Some(p) = f.text[after_name..].find('(') else { continue };
            let pos = after_name + p + 1;
            let (l, c) = f.idx.line_col_utf16(&f.text, pos);
            let has_params = !fd.params.is_empty();
            hints.push(json!({
                "position": {"line": l, "character": c},
                "label": if has_params { format!("{}, ", label) } else { label.to_string() },
                "kind": 2,
                "paddingRight": false,
                "tooltip": "receiver inferred by Oxie",
            }));
        }
    };
    fn walk<'a>(items: &'a [oxie_compiler::ast::Item], out: &mut Vec<(String, &'a [oxie_compiler::ast::Item])>) {
        for it in items {
            match &it.kind {
                ItemKind::Type(t) => out.push((t.name.clone(), &t.members)),
                ItemKind::Impl(i) if i.trait_.is_none() => {
                    if let Some(n) = i.self_ty.last_name() {
                        out.push((n.to_string(), &i.members));
                    }
                }
                ItemKind::Mod { items, .. } => walk(items, out),
                _ => {}
            }
        }
    }
    let mut groups = Vec::new();
    walk(&f.ast.items, &mut groups);
    for (owner, members) in groups {
        add_for(&owner, members);
    }
    Ok(Value::Array(hints))
}

// ---------------------------------------------------------------------------------------------
// main loop

/// The next message: None at the end of the input, an error for a message that isn't JSON (or
/// has no length), which the server skips.
fn read_message(r: &mut impl BufRead) -> Option<Result<Value, String>> {
    let mut len = None;
    loop {
        let mut line = String::new();
        if r.read_line(&mut line).ok()? == 0 {
            return None;
        }
        let t = line.trim_end();
        if t.is_empty() {
            break;
        }
        if let Some(v) = t.strip_prefix("Content-Length:") {
            len = v.trim().parse::<usize>().ok();
        }
    }
    let Some(len) = len else { return Some(Err("a message without a valid Content-Length header".into())) };
    let mut buf = vec![0u8; len];
    r.read_exact(&mut buf).ok()?;
    Some(serde_json::from_slice(&buf).map_err(|e| format!("a message that isn't JSON ({})", e)))
}

pub fn run() {
    let out: Out = Arc::new(Mutex::new(std::io::stdout()));
    let mut server = Server {
        out: out.clone(),
        docs: HashMap::new(),
        cargo_diags: Arc::new(Mutex::new(HashMap::new())),
        oxie_diags: Arc::new(Mutex::new(HashMap::new())),
        checks: Arc::new(Mutex::new(CheckQueue::default())),
        cargo_check_enabled: std::env::var("OXIE_LSP_NO_CHECK").is_err(),
    };
    let stdin = std::io::stdin();
    let mut reader = BufReader::new(stdin.lock());
    while let Some(msg) = read_message(&mut reader) {
        let msg = match msg {
            Ok(m) => m,
            Err(e) => {
                eprintln!("oxie lsp: skipped {}", e);
                send(&out, json!({"jsonrpc": "2.0", "id": Value::Null, "error": {"code": -32700, "message": format!("parse error: {}", e)}}));
                continue;
            }
        };
        let method = msg["method"].as_str().unwrap_or("").to_string();
        let id = msg.get("id").cloned();
        let params = msg.get("params").cloned().unwrap_or(Value::Null);
        match method.as_str() {
            "initialize" => {
                if let Some(opts) = params.get("initializationOptions") {
                    if opts.get("cargoCheck") == Some(&Value::Bool(false)) {
                        server.cargo_check_enabled = false;
                    }
                }
                let caps = json!({
                    "capabilities": {
                        "textDocumentSync": {"openClose": true, "change": 1, "save": {"includeText": false}},
                        "hoverProvider": true,
                        "definitionProvider": true,
                        "declarationProvider": true,
                        "referencesProvider": true,
                        "documentHighlightProvider": true,
                        "renameProvider": true,
                        "documentSymbolProvider": true,
                        "workspaceSymbolProvider": true,
                        "documentFormattingProvider": true,
                        "completionProvider": {"triggerCharacters": [".", ":"], "resolveProvider": false},
                        "signatureHelpProvider": {"triggerCharacters": ["(", ","]},
                        "inlayHintProvider": true,
                        "codeActionProvider": {"codeActionKinds": ["quickfix"]},
                        "executeCommandProvider": {"commands": ["oxie.showRust"]},
                    },
                    "serverInfo": {"name": "oxie", "version": env!("CARGO_PKG_VERSION")},
                });
                send(&out, json!({"jsonrpc": "2.0", "id": id, "result": caps}));
            }
            "initialized" | "$/cancelRequest" | "$/setTrace" | "workspace/didChangeConfiguration" | "workspace/didChangeWatchedFiles" => {}
            "exit" => break,
            "textDocument/didOpen" | "textDocument/didChange" => {
                let uri = params["textDocument"]["uri"].as_str().unwrap_or("");
                if method == "textDocument/didOpen" {
                    remember_alias(&uri_to_path(uri));
                }
                let path = canonical(&uri_to_path(uri));
                let text = if method == "textDocument/didOpen" {
                    params["textDocument"]["text"].as_str().unwrap_or("").to_string()
                } else {
                    params["contentChanges"].as_array().and_then(|c| c.last()).and_then(|c| c["text"].as_str()).unwrap_or("").to_string()
                };
                server.docs.insert(path.clone(), text);
                let a = server.analyze(&path);
                server.refresh_oxie_diags(&a);
                if method == "textDocument/didOpen" {
                    server.run_cargo_check(&a);
                }
            }
            "textDocument/didSave" => {
                let uri = params["textDocument"]["uri"].as_str().unwrap_or("");
                let path = canonical(&uri_to_path(uri));
                if let Ok(t) = std::fs::read_to_string(&path) {
                    server.docs.insert(path.clone(), t);
                }
                let a = server.analyze(&path);
                server.refresh_oxie_diags(&a);
                server.run_cargo_check(&a);
            }
            "textDocument/didClose" => {
                let uri = params["textDocument"]["uri"].as_str().unwrap_or("");
                server.docs.remove(&canonical(&uri_to_path(uri)));
            }
            _ => {
                if let Some(id) = id {
                    let resp = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| handle(&mut server, &method, &params))) {
                        Ok(Ok(result)) => json!({"jsonrpc": "2.0", "id": id, "result": result}),
                        Ok(Err(e)) if method == "shutdown" => json!({"jsonrpc": "2.0", "id": id, "result": Value::Null, "error_text": e}),
                        Ok(Err(e)) => json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32601, "message": e}}),
                        Err(_) => json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32603, "message": "internal error"}}),
                    };
                    send(&out, resp);
                }
            }
        }
    }
}
