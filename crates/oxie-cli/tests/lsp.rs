//! Drives `oxie lsp` over stdio like an editor would.

mod common;

use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

use serde_json::{json, Value};

struct Client {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next_id: u64,
    notifications: Vec<Value>,
}

impl Client {
    fn start() -> Client {
        Client::start_with(true)
    }

    fn start_with(no_check: bool) -> Client {
        Client::spawn(no_check, None)
    }

    /// A server that checks with cargo and builds scripts in `cache` (as `OXIE_CACHE`).
    fn start_in(cache: &Path) -> Client {
        Client::spawn(false, Some(cache))
    }

    fn spawn(no_check: bool, cache: Option<&Path>) -> Client {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_oxie"));
        // cargo check builds with this checkout's oxie-build, not the one on crates.io
        cmd.env("OXIE_HOME", PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.."));
        if no_check {
            cmd.env("OXIE_LSP_NO_CHECK", "1");
        }
        if let Some(c) = cache {
            cmd.env("OXIE_CACHE", c);
        }
        let mut child = cmd
            .arg("lsp")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let stdin = child.stdin.take().unwrap();
        let stdout = BufReader::new(child.stdout.take().unwrap());
        Client { child, stdin, stdout, next_id: 1, notifications: Vec::new() }
    }

    fn write(&mut self, v: Value) {
        let t = v.to_string();
        write!(self.stdin, "Content-Length: {}\r\n\r\n{}", t.len(), t).unwrap();
        self.stdin.flush().unwrap();
    }

    fn read(&mut self) -> Value {
        let mut len = 0;
        loop {
            let mut line = String::new();
            self.stdout.read_line(&mut line).unwrap();
            let t = line.trim_end();
            if t.is_empty() {
                break;
            }
            if let Some(v) = t.strip_prefix("Content-Length:") {
                len = v.trim().parse().unwrap();
            }
        }
        let mut buf = vec![0; len];
        self.stdout.read_exact(&mut buf).unwrap();
        serde_json::from_slice(&buf).unwrap()
    }

    fn request(&mut self, method: &str, params: Value) -> Value {
        let id = self.next_id;
        self.next_id += 1;
        self.write(json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}));
        loop {
            let m = self.read();
            if m.get("id") == Some(&json!(id)) {
                return m.get("result").cloned().unwrap_or_else(|| m.clone());
            }
            self.notifications.push(m);
        }
    }

    fn notify(&mut self, method: &str, params: Value) {
        self.write(json!({"jsonrpc": "2.0", "method": method, "params": params}));
    }

    fn wait_diagnostics(&mut self, uri: &str) -> Value {
        loop {
            if let Some(i) = self.notifications.iter().position(|n| n["method"] == "textDocument/publishDiagnostics" && n["params"]["uri"] == uri) {
                return self.notifications.remove(i)["params"]["diagnostics"].clone();
            }
            let m = self.read();
            self.notifications.push(m);
        }
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        let _ = self.child.kill();
    }
}

const SRC: &str = r#"/// A point.
data class Point(x: i32, y: i32) {
    fun moved(dx: i32): Point = Point(x + dx, y)
}

class Counter(var n: i32 = 0) {
    fun inc() {
        n += 1
    }
}

fun add(a: i32, b: i32 = 1): i32 = a + b

fun main() {
    val p = Point(1, 2)
    val q = p.moved(3)
    println(add(q.x, ))
    var c = Counter()
    c.inc()
}
"#;

/// A `file://` URI as editors send it: forward slashes, and `file:///C:/...` on Windows.
fn file_uri(p: &Path) -> String {
    let s = p.to_string_lossy().replace('\\', "/");
    if s.starts_with('/') {
        format!("file://{s}")
    } else {
        format!("file:///{s}")
    }
}

fn pos(src: &str, needle: &str, delta: usize) -> Value {
    let off = src.find(needle).unwrap() + delta;
    let before = &src[..off];
    let line = before.matches('\n').count();
    let col = off - before.rfind('\n').map(|i| i + 1).unwrap_or(0);
    json!({"line": line, "character": col})
}

#[test]
fn language_server_features() {
    let dir = std::env::temp_dir().join(format!("oxie-lsp-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file: PathBuf = dir.join("demo.oxi");
    std::fs::write(&file, SRC).unwrap();
    let uri = file_uri(&oxie_compiler::compile::canonicalize(&file).unwrap());

    let mut c = Client::start();
    let init = c.request("initialize", json!({"rootUri": file_uri(&dir), "capabilities": {}}));
    assert_eq!(init["capabilities"]["hoverProvider"], true);
    c.notify("initialized", json!({}));
    c.notify("textDocument/didOpen", json!({"textDocument": {"uri": uri, "languageId": "oxie", "version": 1, "text": SRC}}));
    let diags = c.wait_diagnostics(&uri);
    assert_eq!(diags.as_array().unwrap().len(), 0, "{diags}");

    // hover on a data class shows the Oxie signature, docs and generated Rust
    let h = c.request("textDocument/hover", json!({"textDocument": {"uri": uri}, "position": pos(SRC, "Point(1, 2)", 1)}));
    let text = h["contents"]["value"].as_str().unwrap();
    assert!(text.contains("data class Point(x: i32, y: i32)"), "{text}");
    assert!(text.contains("A point."), "{text}");
    assert!(text.contains("struct Point"), "{text}");

    // hover on a method says why Oxie gave it `&mut self`, and which line decided it
    let h = c.request("textDocument/hover", json!({"textDocument": {"uri": uri}, "position": pos(SRC, "inc()", 1)}));
    let text = h["contents"]["value"].as_str().unwrap();
    assert!(text.contains("Oxie decided:\n- `&mut self`: it changes `n` (line 8)"), "{text}");

    // go to definition of a method through a typed local
    let d = c.request("textDocument/definition", json!({"textDocument": {"uri": uri}, "position": pos(SRC, "moved(3)", 1)}));
    assert_eq!(d[0]["range"]["start"]["line"], 2, "{d}");

    // completion after `p.` lists Point members
    let comp = c.request("textDocument/completion", json!({"textDocument": {"uri": uri}, "position": pos(SRC, "p.moved(3)", 2)}));
    let labels: Vec<String> = comp["items"].as_array().unwrap().iter().map(|i| i["label"].as_str().unwrap().to_string()).collect();
    assert!(labels.contains(&"moved".to_string()) && labels.contains(&"x".to_string()), "{labels:?}");

    // signature help inside add(…)
    let sig = c.request("textDocument/signatureHelp", json!({"textDocument": {"uri": uri}, "position": pos(SRC, "add(q.x, )", 9)}));
    assert_eq!(sig["activeParameter"], 1, "{sig}");
    assert!(sig["signatures"][0]["label"].as_str().unwrap().contains("fun add(a: i32, b: i32 = …): i32"), "{sig}");

    // inlay hints show the inferred receiver of `inc`
    let hints = c.request("textDocument/inlayHint", json!({"textDocument": {"uri": uri}, "range": {"start": {"line": 0, "character": 0}, "end": {"line": 30, "character": 0}}}));
    let labels: Vec<String> = hints.as_array().unwrap().iter().map(|h| h["label"].as_str().unwrap().to_string()).collect();
    assert!(labels.contains(&"&mut self".to_string()), "{labels:?}");
    assert!(labels.contains(&"&self, ".to_string()), "{labels:?}");

    // document symbols
    let syms = c.request("textDocument/documentSymbol", json!({"textDocument": {"uri": uri}}));
    let names: Vec<&str> = syms.as_array().unwrap().iter().map(|s| s["name"].as_str().unwrap()).collect();
    assert_eq!(names, vec!["Point", "Counter", "add", "main"]);

    // rename a local
    let edit = c.request("textDocument/rename", json!({"textDocument": {"uri": uri}, "position": pos(SRC, "val q", 4), "newName": "moved_point"}));
    assert_eq!(edit["changes"][&uri].as_array().unwrap().len(), 2, "{edit}");

    // formatting
    let messy = "fun main(){\nval x=1\n}\n";
    c.notify("textDocument/didChange", json!({"textDocument": {"uri": uri, "version": 2}, "contentChanges": [{"text": messy}]}));
    let _ = c.wait_diagnostics(&uri);
    let fmt = c.request("textDocument/formatting", json!({"textDocument": {"uri": uri}, "options": {"tabSize": 4, "insertSpaces": true}}));
    assert_eq!(fmt[0]["newText"], "fun main() {\n    val x = 1\n}\n");

    // syntax errors are reported immediately
    c.notify("textDocument/didChange", json!({"textDocument": {"uri": uri, "version": 3}, "contentChanges": [{"text": "fun main() {\n    val x = (1 + \n}\n"}]}));
    let diags = c.wait_diagnostics(&uri);
    assert!(!diags.as_array().unwrap().is_empty());
    assert_eq!(diags[0]["source"], "oxie");

    let _ = c.request("shutdown", Value::Null);
    c.notify("exit", Value::Null);
    let _ = std::fs::remove_dir_all(&dir);
}

/// Hover shows a function's `context(...)` clause, and a context used in a method leads to the
/// clause of its class.
#[test]
fn context_parameters_in_hover_and_definition() {
    const CTX: &str = r#"class Log(var lines: Vec<String> = vec[]) {
    fun add(line: String) = lines.push(line)
}

context(log: &mut Log)
fun record(line: &str) = log.add(line.to_string())

context(log: &mut Log)
class Cart(var n: i32 = 0) {
    fun add() {
        n += 1
        log.add("added")
    }
}

fun main() {
    var log = Log()
    record("hi")
    var cart = Cart()
    cart.add()
}
"#;
    let dir = std::env::temp_dir().join(format!("oxie-lsp-contexts-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file: PathBuf = dir.join("contexts.oxi");
    std::fs::write(&file, CTX).unwrap();
    let uri = file_uri(&oxie_compiler::compile::canonicalize(&file).unwrap());

    let mut c = Client::start();
    let _ = c.request("initialize", json!({"rootUri": file_uri(&dir), "capabilities": {}}));
    c.notify("initialized", json!({}));
    c.notify("textDocument/didOpen", json!({"textDocument": {"uri": uri, "languageId": "oxie", "version": 1, "text": CTX}}));
    let diags = c.wait_diagnostics(&uri);
    assert_eq!(diags.as_array().unwrap().len(), 0, "{diags}");

    let h = c.request("textDocument/hover", json!({"textDocument": {"uri": uri}, "position": pos(CTX, "record(\"hi\")", 1)}));
    let text = h["contents"]["value"].as_str().unwrap();
    assert!(text.contains("context(log: &mut Log) fun record(line: &str)"), "{text}");

    let d = c.request("textDocument/definition", json!({"textDocument": {"uri": uri}, "position": pos(CTX, "log.add(\"added\")", 1)}));
    assert_eq!(d[0]["range"]["start"]["line"], 7, "{d}");

    let _ = c.request("shutdown", Value::Null);
    c.notify("exit", Value::Null);
    let _ = std::fs::remove_dir_all(&dir);
}

/// Completion after `c.` on a class value without a declared type lists the class's members and
/// its interfaces' methods only, and hovering the value shows its type; a message that isn't
/// JSON is skipped, and an unsaved document's diagnostics go to its own URI.
#[test]
fn class_values_unsaved_documents_and_bad_messages() {
    const SRC: &str = r#"interface Named {
    fun name(): String
}

class Counter(var n: i32 = 0) : Named {
    fun inc() {
        n += 1
    }

    override fun name() = "counter"
}

class Other(v: i32) {
    fun unrelated() {}
}

fun main() {
    var c = Counter()
    c.inc()
}
"#;
    let dir = std::env::temp_dir().join(format!("oxie-lsp-values-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file: PathBuf = dir.join("values.oxi");
    std::fs::write(&file, SRC).unwrap();
    let uri = file_uri(&oxie_compiler::compile::canonicalize(&file).unwrap());

    let mut c = Client::start();
    let _ = c.request("initialize", json!({"rootUri": file_uri(&dir), "capabilities": {}}));
    c.notify("initialized", json!({}));
    // a broken message doesn't stop the server
    write!(c.stdin, "Content-Length: 9\r\n\r\n{{not json").unwrap();
    c.stdin.flush().unwrap();
    c.notify("textDocument/didOpen", json!({"textDocument": {"uri": uri, "languageId": "oxie", "version": 1, "text": SRC}}));
    let diags = c.wait_diagnostics(&uri);
    assert_eq!(diags.as_array().unwrap().len(), 0, "{diags}");

    let comp = c.request("textDocument/completion", json!({"textDocument": {"uri": uri}, "position": pos(SRC, "c.inc()", 2)}));
    let labels: Vec<String> = comp["items"].as_array().unwrap().iter().map(|i| i["label"].as_str().unwrap().to_string()).collect();
    assert!(labels.contains(&"inc".to_string()) && labels.contains(&"n".to_string()) && labels.contains(&"name".to_string()), "{labels:?}");
    assert!(!labels.iter().any(|l| l == "unrelated" || l == "iter" || l == "push"), "{labels:?}");

    let h = c.request("textDocument/hover", json!({"textDocument": {"uri": uri}, "position": pos(SRC, "c.inc()", 0)}));
    let text = h["contents"]["value"].as_str().unwrap();
    assert!(text.contains("c: Counter"), "{text}");

    let untitled = "untitled:Untitled-1";
    c.notify("textDocument/didOpen", json!({"textDocument": {"uri": untitled, "languageId": "oxie", "version": 1, "text": "fun main() {\n    val x = (1 +\n}\n"}}));
    let diags = c.wait_diagnostics(untitled);
    assert!(!diags.as_array().unwrap().is_empty(), "{diags}");

    let _ = c.request("shutdown", Value::Null);
    c.notify("exit", Value::Null);
    let _ = std::fs::remove_dir_all(&dir);
}

/// A file opened through a symlinked directory (on macOS, any file under /tmp) keeps the editor's
/// URI in diagnostics and locations.
#[cfg(unix)]
#[test]
fn symlinked_directories_keep_the_editors_uri() {
    let dir = std::env::temp_dir().join(format!("oxie-lsp-link-test-{}", std::process::id()));
    std::fs::create_dir_all(dir.join("real")).unwrap();
    let _ = std::os::unix::fs::symlink(dir.join("real"), dir.join("link"));
    let text = "fun one() = 1\n\nfun main() {\n    val x = (one() + \n}\n";
    std::fs::write(dir.join("real/a.oxi"), text).unwrap();
    let uri = file_uri(&dir.join("link/a.oxi"));

    let mut c = Client::start();
    c.request("initialize", json!({"rootUri": file_uri(&dir), "capabilities": {}}));
    c.notify("initialized", json!({}));
    c.notify("textDocument/didOpen", json!({"textDocument": {"uri": uri, "languageId": "oxie", "version": 1, "text": text}}));
    let d = c.request("textDocument/definition", json!({"textDocument": {"uri": uri}, "position": pos(text, "one() +", 1)}));
    assert_eq!(d[0]["uri"], uri, "{d}");
    // the diagnostics for didOpen were sent before the answer
    let published: Vec<&Value> = c.notifications.iter().filter(|n| n["method"] == "textDocument/publishDiagnostics").map(|n| &n["params"]).collect();
    assert_eq!(published.len(), 1, "{published:?}");
    assert_eq!(published[0]["uri"], uri, "{published:?}");
    assert_eq!(published[0]["diagnostics"][0]["source"], "oxie", "{published:?}");

    let _ = c.request("shutdown", Value::Null);
    c.notify("exit", Value::Null);
    let _ = std::fs::remove_dir_all(&dir);
}

// `pub`, so Oxie keeps the owned parameter (it would lend a private function's)
// `consume` only reads `v`, but it is used as a value below, so it keeps its signature and the
// call moves `nums`; being `pub`, it gets the fix that lets it borrow
const MOVED: &str = r#"pub fun consume(v: Vec<i32>): usize = v.len()

fun main() {
    val nums = vec[1, 2, 3]
    val n = consume(nums)
    println("$n ${nums.len()}")
    println(vec[vec[4]].into_iter().map(consume).count())
}
"#;

/// rustc errors arrive with Oxie explanations, related locations and quick fixes.
#[test]
fn borrow_errors_have_quick_fixes() {
    let dir = std::env::temp_dir().join(format!("oxie-lsp-fix-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file: PathBuf = dir.join("moved.oxi");
    std::fs::write(&file, MOVED).unwrap();
    let uri = file_uri(&oxie_compiler::compile::canonicalize(&file).unwrap());

    let mut c = Client::start_with(false);
    let init = c.request("initialize", json!({"rootUri": file_uri(&dir), "capabilities": {}}));
    assert!(init["capabilities"]["codeActionProvider"].is_object());
    c.notify("initialized", json!({}));
    c.notify("textDocument/didOpen", json!({"textDocument": {"uri": uri, "languageId": "oxie", "version": 1, "text": MOVED}}));
    let diag = loop {
        let diags = c.wait_diagnostics(&uri);
        if let Some(d) = diags.as_array().unwrap().iter().find(|d| d["source"] == "rustc") {
            break d.clone();
        }
    };
    assert!(diag["message"].as_str().unwrap().starts_with("`nums` was moved"), "{diag}");
    assert_eq!(diag["range"]["start"]["line"], 5, "{diag}");
    assert_eq!(diag["relatedInformation"][0]["location"]["range"]["start"], json!({"line": 4, "character": 20}), "{diag}");

    let actions = c.request("textDocument/codeAction", json!({"textDocument": {"uri": uri}, "range": diag["range"], "context": {"diagnostics": [diag]}}));
    let titles: Vec<&str> = actions.as_array().unwrap().iter().map(|a| a["title"].as_str().unwrap()).collect();
    assert_eq!(titles, ["Pass a copy of `nums`", "Let `consume` borrow it (parameter type `&[i32]`)"], "{actions}");
    let edit = &actions[0]["edit"]["changes"][&uri][0];
    assert_eq!(edit["range"]["start"], json!({"line": 4, "character": 24}), "{edit}");
    assert_eq!(edit["newText"], ".clone()");

    let _ = c.request("shutdown", Value::Null);
    c.notify("exit", Value::Null);
    let _ = std::fs::remove_dir_all(&dir);
}

const PROJECT_MAIN: &str = r#"import text::{describe, keep, Tag}

fun main() {
    val nums = vec[1, 2, 3]
    val kept = keep(nums)
    println("${kept.len()} ${nums.len()}")
    val tag = Tag("new".to_string())
    println(describe(tag))
    println(tag.name)
}
"#;

const PROJECT_TEXT: &str = r##"fun keep(v: Vec<i32>): Vec<i32> = v
data class Tag(name: String)
pub fun describe(t: Tag): String = "#${t.name}"
fun describe_all(tags: Vec<Tag>): Vec<String> = tags.into_iter().map(describe).collect()
"##;

/// In a project, an explanation can point at another file (where a function keeps the value
/// moved into it), and a quick fix can edit it (let the function borrow the value).
#[test]
fn explanations_and_fixes_reach_other_files_of_the_project() {
    let dir = std::env::temp_dir().join(format!("oxie-lsp-project-test-{}", std::process::id()));
    std::fs::create_dir_all(dir.join("src")).unwrap();
    let build = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../oxie-build").display().to_string().replace('\\', "/");
    std::fs::write(dir.join("Cargo.toml"), format!("[package]\nname = \"two_files\"\nversion = \"0.1.0\"\nedition = \"2021\"\nbuild = \"build.rs\"\n\n[build-dependencies]\noxie-build = {{ path = \"{build}\" }}\n")).unwrap();
    std::fs::write(dir.join("build.rs"), "fn main() {\n    oxie_build::compile();\n}\n").unwrap();
    std::fs::write(dir.join("src/main.rs"), "include!(concat!(env!(\"OUT_DIR\"), \"/oxie/main.rs\"));\n").unwrap();
    std::fs::write(dir.join("src/main.oxi"), PROJECT_MAIN).unwrap();
    std::fs::write(dir.join("src/text.oxi"), PROJECT_TEXT).unwrap();
    let uri = file_uri(&oxie_compiler::compile::canonicalize(dir.join("src/main.oxi")).unwrap());
    let text_uri = file_uri(&oxie_compiler::compile::canonicalize(dir.join("src/text.oxi")).unwrap());

    let mut c = Client::start_with(false);
    c.request("initialize", json!({"rootUri": file_uri(&dir), "capabilities": {}}));
    c.notify("initialized", json!({}));
    c.notify("textDocument/didOpen", json!({"textDocument": {"uri": uri, "languageId": "oxie", "version": 1, "text": PROJECT_MAIN}}));
    let diags = loop {
        let diags = c.wait_diagnostics(&uri);
        if diags.as_array().unwrap().iter().filter(|d| d["source"] == "rustc").count() >= 2 {
            break diags;
        }
    };
    let at_line = |l: u64| diags.as_array().unwrap().iter().find(|d| d["source"] == "rustc" && d["range"]["start"]["line"] == l).cloned().unwrap_or_else(|| panic!("no rustc error on line {l}: {diags}"));

    // `nums` moved into `keep`, which keeps it in text.oxi
    let moved = at_line(5);
    let related = moved["relatedInformation"].as_array().unwrap();
    let kept = related.iter().find(|r| r["location"]["uri"] == text_uri).unwrap_or_else(|| panic!("{moved}"));
    assert_eq!(kept["location"]["range"]["start"], json!({"line": 0, "character": 34}), "{moved}");
    assert!(kept["message"].as_str().unwrap().contains("it returns `v`"), "{moved}");

    // `tag` moved into `describe` (used as a value, so Oxie keeps its signature): the fix changes
    // the parameter in text.oxi
    let moved = at_line(8);
    let actions = c.request("textDocument/codeAction", json!({"textDocument": {"uri": uri}, "range": moved["range"], "context": {"diagnostics": [moved]}}));
    let borrow = actions.as_array().unwrap().iter().find(|a| a["title"] == "Let `describe` borrow it (parameter type `&Tag`)").unwrap_or_else(|| panic!("{actions}"));
    let edit = &borrow["edit"]["changes"][&text_uri][0];
    assert_eq!(edit["range"]["start"], json!({"line": 2, "character": 20}), "{borrow}");
    assert_eq!(edit["newText"], "&Tag", "{borrow}");
    assert!(borrow["edit"]["changes"].get(&uri).is_none(), "{borrow}");

    let _ = c.request("shutdown", Value::Null);
    c.notify("exit", Value::Null);
    let _ = std::fs::remove_dir_all(&dir);
}

/// A script opened while another script is being checked is checked next (editors open several
/// files at once when they restore their tabs).
#[test]
fn scripts_opened_together_are_all_checked() {
    let dir = std::env::temp_dir().join(format!("oxie-lsp-scripts-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let mut uris = Vec::new();
    for name in ["one", "two"] {
        let file = dir.join(format!("{name}.oxi"));
        std::fs::write(&file, MOVED).unwrap();
        uris.push(file_uri(&oxie_compiler::compile::canonicalize(&file).unwrap()));
    }

    let mut c = Client::start_with(false);
    c.request("initialize", json!({"rootUri": file_uri(&dir), "capabilities": {}}));
    c.notify("initialized", json!({}));
    for uri in &uris {
        c.notify("textDocument/didOpen", json!({"textDocument": {"uri": uri, "languageId": "oxie", "version": 1, "text": MOVED}}));
    }
    // two checks run, one after the other
    let mut ended = 0;
    while ended < 2 {
        let m = c.read();
        if m["method"] == "$/progress" && m["params"]["value"]["kind"] == "end" {
            ended += 1;
        }
        c.notifications.push(m);
    }
    for uri in &uris {
        let checked = c.notifications.iter().any(|n| {
            n["method"] == "textDocument/publishDiagnostics" && n["params"]["uri"] == *uri && n["params"]["diagnostics"].as_array().unwrap().iter().any(|d| d["source"] == "rustc")
        });
        assert!(checked, "no rustc diagnostics for {uri}");
    }

    let _ = c.request("shutdown", Value::Null);
    c.notify("exit", Value::Null);
    let _ = std::fs::remove_dir_all(&dir);
}

/// Every quick fix offered for the programs (and projects) in `tests/diagnostics` makes the
/// error it is offered for go away, and adds no error to the lines it changes: each fix is
/// applied on its own, as an editor would, to a copy of the program, which is then built again.
#[test]
fn quick_fixes_fix_their_errors() {
    let dir = oxie_compiler::compile::canonicalize(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/diagnostics")).unwrap();
    let mut items: Vec<PathBuf> = std::fs::read_dir(&dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "oxi") || p.join("Cargo.toml").exists())
        .collect();
    items.sort();
    let results = common::parallel(&items, |p, cache| check_fixes(p, cache));
    let checked: usize = results.iter().map(|(n, _)| n).sum();
    let failures: Vec<String> = results.into_iter().flat_map(|(_, f)| f).collect();
    assert!(checked >= 20, "only {checked} fixes were checked");
    assert!(failures.is_empty(), "{}", failures.join("\n\n"));
}

/// Applies each fix offered for `p` (a program, or a project directory) to a fresh copy of it and
/// builds that: how many fixes there were, and the ones that left their error or added one.
fn check_fixes(p: &Path, cache: &Path) -> (usize, Vec<String>) {
    let name = p.file_name().unwrap().to_string_lossy().to_string();
    let project = p.is_dir();
    let fixes_dir = cache.join("fixes");
    let copy_to = |to: &Path| {
        let _ = std::fs::remove_dir_all(to);
        if project {
            copy_project(p, to);
        } else {
            std::fs::create_dir_all(to).unwrap();
            std::fs::copy(p, to.join(&name)).unwrap();
        }
        oxie_compiler::compile::canonicalize(to).unwrap()
    };
    // the errors and their fixes, as an editor gets them
    let work = copy_to(&fixes_dir.join(&name));
    // with Oxie errors in any file of it, the server doesn't run cargo check
    if oxie_errors(&work, project).is_some() {
        return (0, Vec::new());
    }
    let main = if project { work.join("src/main.oxi") } else { work.join(&name) };
    let main_name = main.file_name().unwrap().to_string_lossy().to_string();
    let uri = file_uri(&main);
    let mut c = Client::start_in(cache);
    c.request("initialize", json!({"rootUri": file_uri(&work), "capabilities": {}}));
    c.notify("initialized", json!({}));
    c.notify("textDocument/didOpen", json!({"textDocument": {"uri": uri, "languageId": "oxie", "version": 1, "text": std::fs::read_to_string(&main).unwrap()}}));
    // Oxie's own errors come first; without them, cargo check runs and its errors follow
    let own = c.wait_diagnostics(&uri);
    if own.as_array().unwrap().iter().any(|d| d["severity"] == 1) {
        return (0, Vec::new());
    }
    loop {
        let m = c.read();
        let end = m["method"] == "$/progress" && m["params"]["value"]["kind"] == "end";
        c.notifications.push(m);
        if end {
            break;
        }
    }
    let diags = c.notifications.iter().rev().find(|n| n["method"] == "textDocument/publishDiagnostics" && n["params"]["uri"] == uri).map(|n| n["params"]["diagnostics"].clone()).unwrap_or_default();
    let _ = c.request("shutdown", Value::Null);
    c.notify("exit", Value::Null);

    // how an error shows in the output: `error[E0382]`, or `error: <message>` without a code
    let kind = |d: &Value| match d["code"].as_str() {
        Some(c) if !c.is_empty() => format!("[{}]", c),
        _ => format!(": {}", d["message"].as_str().unwrap_or("").lines().next().unwrap_or("")),
    };
    let rustc: Vec<&Value> = diags.as_array().into_iter().flatten().filter(|d| d["source"] == "rustc").collect();
    let (mut checked, mut failures) = (0, Vec::new());
    for d in &rustc {
        let code = kind(d);
        let line = d["range"]["start"]["line"].as_i64().unwrap();
        // the errors its line had before the fix
        let before: Vec<String> = rustc.iter().filter(|o| o["range"]["start"]["line"].as_i64() == Some(line)).map(|o| kind(o)).collect();
        for (i, fix) in d["data"]["fixes"].as_array().into_iter().flatten().enumerate() {
            checked += 1;
            let fixed = copy_to(&fixes_dir.join(format!("{}-{}-{}", name, line + 1, i)));
            // the lines of this file the fix replaces: (first, last, lines added, lines of the new text)
            let mut replaced: Vec<(i64, i64, i64, i64)> = Vec::new();
            let mut edits: Vec<&Value> = fix["edits"].as_array().unwrap().iter().collect();
            edits.sort_by_key(|e| std::cmp::Reverse((e["range"]["start"]["line"].as_u64(), e["range"]["start"]["character"].as_u64())));
            for e in edits {
                let file = uri_path(e["uri"].as_str().unwrap_or(&uri));
                let target = fixed.join(file.strip_prefix(&work).unwrap_or_else(|_| panic!("{} isn't in {}", file.display(), work.display())));
                let mut text = std::fs::read_to_string(&target).unwrap();
                let (start, end) = (offset(&text, &e["range"]["start"]), offset(&text, &e["range"]["end"]));
                let new = e["newText"].as_str().unwrap();
                if target.ends_with(&main_name) {
                    let (first, last) = (e["range"]["start"]["line"].as_i64().unwrap(), e["range"]["end"]["line"].as_i64().unwrap());
                    let lines = new.matches('\n').count() as i64;
                    replaced.push((first, last, lines - text[start..end].matches('\n').count() as i64, lines));
                }
                text.replace_range(start..end, new);
                std::fs::write(&target, text).unwrap();
            }
            // a line after the fix, which may add or remove lines above it; a replaced line is
            // where its replacement starts
            let after = |l: i64| replaced.iter().find(|r| r.0 <= l && l <= r.1).map_or(l, |r| r.0) + replaced.iter().filter(|r| r.1 < l).map(|r| r.2).sum::<i64>();
            // the kinds of error lines `from..=to` had before the fix
            let had = |from: i64, to: i64| -> Vec<String> { rustc.iter().filter(|o| o["range"]["start"]["line"].as_i64().is_some_and(|l| from <= l && l <= to)).map(|o| kind(o)).collect() };
            // the lines to check after the fix, with the errors they may have: the error's line,
            // and the lines the fix wrote
            let mut check: Vec<(i64, Vec<String>)> = vec![(after(line), before.clone())];
            for &(first, last, _, lines) in &replaced {
                for l in (0..=lines).map(|k| after(first) + k) {
                    match check.iter_mut().find(|c| c.0 == l) {
                        Some(c) => c.1.extend(had(first, last)),
                        None => check.push((l, had(first, last))),
                    }
                }
            }
            let title = format!("{}:{}: the fix `{}` for {}", p.display(), line + 1, fix["title"].as_str().unwrap_or(""), code);
            // the fixed program is still valid Oxie (rustc never sees it otherwise)
            if let Some(errors) = oxie_errors(&fixed, project) {
                failures.push(format!("{} breaks the Oxie code: {}", title, errors));
                continue;
            }
            let out = if project {
                oxie().arg("build").current_dir(&fixed).env("OXIE_CACHE", cache).env("CARGO_TARGET_DIR", cache.join("fixes-target").join(&name)).output().unwrap()
            } else {
                oxie().arg("run").arg(fixed.join(&name)).current_dir(&fixed).env("OXIE_CACHE", cache).stdin(Stdio::null()).output().unwrap()
            };
            let stderr = String::from_utf8_lossy(&out.stderr);
            // the errors now on these lines: not the one it was for, and none they didn't have
            for (l, had) in &check {
                let at = format!("{}:{}:", main_name, l + 1);
                for head in stderr.split("\n\n").filter(|b| b.contains(&at)).filter_map(|b| b.lines().next()).filter(|h| h.starts_with("error")) {
                    if *l == after(line) && head.contains(&code) {
                        failures.push(format!("{} leaves the error\n{}", title, stderr));
                    } else if !had.iter().any(|k| head.contains(k.as_str())) {
                        failures.push(format!("{} adds an error on line {}: {}\n{}", title, l + 1, head, stderr));
                    }
                }
            }
            let _ = std::fs::remove_dir_all(&fixed);
        }
    }
    (checked, failures)
}

/// The errors Oxie itself reports for the program in `dir` (a script, or a project), if any.
fn oxie_errors(dir: &Path, project: bool) -> Option<String> {
    let inputs = if project {
        oxie_compiler::discover(&dir.join("src")).unwrap()
    } else {
        let file = std::fs::read_dir(dir).unwrap().filter_map(|e| e.ok()).map(|e| e.path()).find(|p| p.extension().is_some_and(|e| e == "oxi")).unwrap();
        vec![oxie_compiler::Input { rel_path: PathBuf::from("main.oxi"), text: std::fs::read_to_string(&file).unwrap(), abs_path: file }]
    };
    let opts = oxie_compiler::Options { crate_name: "main".into(), extern_crates: Default::default(), out_dir: PathBuf::from("/oxie"), script: !project, root_attrs: Vec::new() };
    let out = oxie_compiler::compile(inputs, &opts);
    let errors: Vec<String> = out.diags.iter().flat_map(|(_, _, ds)| ds).filter(|d| d.severity == oxie_compiler::Severity::Error).map(|d| d.message.clone()).collect();
    (!errors.is_empty()).then(|| errors.join("; "))
}

fn oxie() -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_oxie"));
    c.env("NO_COLOR", "1").env("OXIE_HOME", PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.."));
    c
}

/// A copy of a test project, without its build output, that builds with this checkout's
/// oxie-build wherever it is.
fn copy_project(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for e in std::fs::read_dir(from).unwrap().filter_map(|e| e.ok()) {
        let (src, dst) = (e.path(), to.join(e.file_name()));
        if src.is_dir() {
            if e.file_name() != "target" {
                copy_project(&src, &dst);
            }
        } else if e.file_name() == "Cargo.toml" {
            let build = oxie_compiler::compile::canonicalize(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../oxie-build")).unwrap();
            let manifest = std::fs::read_to_string(&src).unwrap().replace("\"../../../crates/oxie-build\"", &format!("\"{}\"", build.display().to_string().replace('\\', "/")));
            std::fs::write(dst, manifest).unwrap();
        } else if e.file_name() != "Cargo.lock" {
            std::fs::copy(&src, &dst).unwrap();
        }
    }
}

/// The path of a `file://` URI (`file:///C:/...` on Windows), percent-decoded.
fn uri_path(uri: &str) -> PathBuf {
    let p = uri.strip_prefix("file://").unwrap_or(uri);
    let p = if p.as_bytes().get(2) == Some(&b':') { &p[1..] } else { p };
    let mut bytes = Vec::new();
    let mut i = 0;
    while i < p.len() {
        match p.get(i + 1..i + 3).filter(|_| p.as_bytes()[i] == b'%').and_then(|h| u8::from_str_radix(h, 16).ok()) {
            Some(b) => {
                bytes.push(b);
                i += 3;
            }
            None => {
                bytes.push(p.as_bytes()[i]);
                i += 1;
            }
        }
    }
    PathBuf::from(String::from_utf8_lossy(&bytes).into_owned())
}

/// The byte offset of an LSP position (line, UTF-16 column).
fn offset(text: &str, pos: &Value) -> usize {
    let line = pos["line"].as_u64().unwrap() as usize;
    let col = pos["character"].as_u64().unwrap() as usize;
    let start: usize = text.split_inclusive('\n').take(line).map(str::len).sum();
    let mut units = 0;
    for (i, c) in text[start..].char_indices() {
        if units >= col || c == '\n' {
            return start + i;
        }
        units += c.len_utf16();
    }
    text.len()
}
