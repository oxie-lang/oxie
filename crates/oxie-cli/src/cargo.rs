//! Runs cargo and maps rustc diagnostics in generated code back to .oxi sources.

use std::collections::{HashMap, HashSet};
use std::io::{BufRead, BufReader, IsTerminal};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::rc::Rc;

use oxie_compiler::SourceMap;
use serde_json::Value;

use crate::sources::Sources;

pub struct BuildResult {
    pub success: bool,
    pub executables: Vec<PathBuf>,
    pub errors: usize,
    pub warnings: usize,
}

/// A rustc diagnostic mapped to a source location.
#[derive(Clone, Debug)]
pub struct MappedDiag {
    pub level: String,
    pub code: Option<String>,
    pub message: String,
    pub file: PathBuf,
    /// zero-based
    pub line: u32,
    pub col: u32,
    pub end_line: u32,
    pub end_col: u32,
    pub label: Option<String>,
    pub notes: Vec<String>,
    /// Whether the location came from a .oxi source map.
    pub from_oxie: bool,
    /// Other labeled spans ("value moved here", ...).
    pub secondary: Vec<Label>,
    /// rustc's suggestions whose spans map exactly onto the .oxi source (not the parts of a
    /// suggestion that edits several places).
    pub suggestions: Vec<Suggestion>,
    /// rustc's suggestions that edit several places, whose parts all map exactly onto the .oxi
    /// source.
    pub multipart: Vec<Multipart>,
    /// rustc suggestions as plain text (for `use` paths and the like).
    pub raw_suggestions: Vec<String>,
    /// rustc's own message when the explanation replaced it.
    pub rust_message: Option<String>,
    /// The generated Rust text under the primary span (single-line spans only).
    pub gen_text: Option<String>,
    pub fixes: Vec<Fix>,
    /// The explanation names the mistake at this position, so rustc's later messages at the
    /// same position are its consequences and aren't shown.
    pub covers_position: bool,
}

/// A labeled span on one line (zero-based, character columns).
#[derive(Clone, Debug)]
pub struct Label {
    /// Another file of the crate (a function's declaration); None for the diagnostic's file.
    pub file: Option<PathBuf>,
    pub line: u32,
    pub col: u32,
    pub end_col: u32,
    pub text: String,
    /// The span mapped exactly, or an explanation placed it; others are dropped.
    pub exact: bool,
}

#[derive(Clone, Debug)]
pub struct Suggestion {
    pub line: u32,
    pub col: u32,
    pub end_line: u32,
    pub end_col: u32,
    pub replacement: String,
}

/// A rustc suggestion that edits several places (`Box::new(` and `)` around a value); its parts
/// only work together.
#[derive(Clone, Debug)]
pub struct Multipart {
    /// rustc's help message.
    pub message: String,
    pub parts: Vec<Suggestion>,
}

/// A text replacement in a source file (zero-based lines, character columns).
#[derive(Clone, Debug, PartialEq)]
pub struct Edit {
    /// Another file of the crate; None for the diagnostic's file.
    pub file: Option<PathBuf>,
    pub line: u32,
    pub col: u32,
    pub end_line: u32,
    pub end_col: u32,
    pub text: String,
}

/// A quick fix: edits to the diagnostic's file, and to other files of its crate.
#[derive(Clone, Debug)]
pub struct Fix {
    pub title: String,
    pub edits: Vec<Edit>,
}

#[derive(Default)]
pub struct Mapper {
    maps: HashMap<PathBuf, Option<SourceMap>>,
    texts: HashMap<PathBuf, String>,
    sources: Vec<Rc<Sources>>,
}

impl Mapper {
    fn map_for(&mut self, file: &Path) -> Option<&SourceMap> {
        self.maps
            .entry(file.to_path_buf())
            .or_insert_with(|| {
                let p = PathBuf::from(format!("{}.map", file.display()));
                std::fs::read_to_string(p).ok().and_then(|t| SourceMap::parse(&t))
            })
            .as_ref()
    }

    /// Maps a rustc span (1-based) to a source location.
    pub fn map_span(&mut self, root: &Path, span: &Value) -> Option<(PathBuf, u32, u32, u32, u32, bool)> {
        let file = span.get("file_name")?.as_str()?;
        let mut path = PathBuf::from(file);
        if path.is_relative() {
            path = root.join(path);
        }
        let ls = span.get("line_start")?.as_u64()? as u32 - 1;
        let cs = span.get("column_start")?.as_u64()? as u32 - 1;
        let le = span.get("line_end")?.as_u64()? as u32 - 1;
        let ce = span.get("column_end")?.as_u64()? as u32 - 1;
        if self.map_for(&path).is_some() {
            // exactly where the generated text matches the source after the nearest mark (a
            // field after `p.`, a method name in a chain), else at the mark
            let found = self.map_aligned(&path, ls, cs, true).or_else(|| self.map_raw(&path, ls, cs)).or_else(|| self.map_word(&path, ls, cs));
            if let Some((source, sl, sc)) = found {
                let end = (le == ls).then(|| self.map_aligned(&path, le, ce, true)).flatten().filter(|(_, el, ec)| *el == sl && *ec > sc);
                let (el, ec) = end.map_or((sl, sc + ce.saturating_sub(cs).max(1)), |(_, el, ec)| (el, ec));
                return Some((source, sl, sc, el, ec, true));
            }
            let map = self.map_for(&path)?;
            let source = map.source.clone();
            let (sl, sc) = map.lookup(ls, cs)?;
            let (el, ec) = if le == ls {
                // keep the width when the span stays on one generated line
                (sl, sc + (ce.saturating_sub(cs)).max(1))
            } else {
                (sl, sc + 1)
            };
            return Some((source, sl, sc, el, ec, true));
        }
        if file.ends_with("__oxie.rs") {
            return None;
        }
        Some((path, ls, cs, le, ce, false))
    }

    fn text_of(&mut self, file: &Path) -> &str {
        self.texts.entry(file.to_path_buf()).or_insert_with(|| std::fs::read_to_string(file).unwrap_or_default())
    }

    /// Maps a zero-based generated position to the exact source position, or None when the
    /// generated text between the nearest source-map mark and the position differs from the
    /// source text (so the position has no exact counterpart).
    fn map_exact(&mut self, gen: &Path, line: u32, col: u32) -> Option<(PathBuf, u32, u32)> {
        self.map_aligned(gen, line, col, false)
    }

    /// `map_exact`, and with `loose` also across the spellings that differ between the
    /// generated Rust and the source in the same order (`let`/`val`, `use`/`import`, `::`/`.`),
    /// for placing a diagnostic (not for applying an edit).
    fn map_aligned(&mut self, gen: &Path, line: u32, col: u32, loose: bool) -> Option<(PathBuf, u32, u32)> {
        let map = self.map_for(gen)?;
        let source = map.source.clone();
        let &(gl, gc, sl, sc) = map.entries.iter().take_while(|e| (e.0, e.1) <= (line, col)).last()?;
        if gl != line {
            return None;
        }
        let gen_line: String = self.text_of(gen).lines().nth(line as usize)?.to_string();
        let piece: String = gen_line.chars().skip(gc as usize).take((col - gc) as usize).collect();
        let src_line: String = self.text_of(&source).lines().nth(sl as usize)?.to_string();
        let src_rest: String = src_line.chars().skip(sc as usize).collect();
        let n = if loose { align(&piece, &src_rest)? } else { src_rest.starts_with(&piece).then(|| piece.chars().count())? };
        Some((source, sl, sc + n as u32))
    }

    /// Maps a generated position whose text starts with a word (`collect` in `.collect()`, a type
    /// name) that the nearest mark doesn't reach exactly, as on another line of a multi-line
    /// chain or in a `catch (e: Exception)` the generated code repeats elsewhere: to the same
    /// occurrence of the word in the source after the mark, before the source of the next mark.
    fn map_word(&mut self, gen: &Path, line: u32, col: u32) -> Option<(PathBuf, u32, u32)> {
        let map = self.map_for(gen)?;
        let source = map.source.clone();
        let at = map.entries.iter().take_while(|e| (e.0, e.1) <= (line, col)).count().checked_sub(1)?;
        let (gl, gc, sl, sc) = map.entries[at];
        let bound = map.entries.iter().map(|e| (e.2, e.3)).filter(|&p| p > (sl, sc)).min().unwrap_or((u32::MAX, 0));
        let gen_text = self.text_of(gen).to_string();
        let gen_lines: Vec<&str> = gen_text.lines().collect();
        let word: String = gen_lines.get(line as usize)?.chars().skip(col as usize).take_while(|c| c.is_alphanumeric() || *c == '_').collect();
        if word.is_empty() || word.starts_with("__oxie") || word.chars().next().is_some_and(|c| c.is_ascii_digit()) {
            return None;
        }
        // occurrences of the word in the generated text from the mark to the position
        let mut before = 0;
        for l in gl..=line {
            let text: String = gen_lines.get(l as usize)?.chars().enumerate().filter(|(i, _)| (l > gl || *i as u32 >= gc) && (l < line || (*i as u32) < col)).map(|(_, c)| c).collect();
            before += word_positions(&text, &word).len();
        }
        let src_text = self.text_of(&source).to_string();
        let mut found = Vec::new();
        for (l, text) in src_text.lines().enumerate().skip(sl as usize) {
            for c in word_positions(text, &word) {
                let p = (l as u32, c as u32);
                if p >= (sl, sc) && p < bound {
                    found.push(p);
                }
            }
            if (l as u32) >= bound.0 {
                break;
            }
        }
        let &(fl, fc) = found.get(before)?;
        Some((source, fl, fc))
    }

    /// Maps a zero-based generated position inside the Rust of a `rust { }` block, which is
    /// copied line by line (re-indented), to the same text in the block. The block has one mark,
    /// at `rust`, so the nearest mark alone would put every error on its first line.
    fn map_raw(&mut self, gen: &Path, line: u32, col: u32) -> Option<(PathBuf, u32, u32)> {
        let map = self.map_for(gen)?;
        let source = map.source.clone();
        let &(gl, _, sl, sc) = map.entries.iter().take_while(|e| (e.0, e.1) <= (line, col)).last()?;
        let gen_line = self.text_of(gen).lines().nth(line as usize)?.to_string();
        let src = self.text_of(&source).to_string();
        let src_lines: Vec<&str> = src.lines().collect();
        let head = src_lines.get(sl as usize)?;
        let head_start = head.char_indices().nth(sc as usize).map(|(b, _)| b)?;
        let after_rust = head[head_start..].strip_prefix("rust")?.trim_start().strip_prefix('{')?;
        // the block's text starts after `{`, or on the first line that isn't blank
        let (first, from) = if after_rust.trim().is_empty() {
            let first = (sl as usize + 1..src_lines.len()).find(|&i| !src_lines[i].trim().is_empty())?;
            (first, 0)
        } else {
            (sl as usize, head.len() - after_rust.len())
        };
        let src_line = *src_lines.get(first + (line - gl) as usize)?;
        let text = gen_line.trim();
        let indent = gen_line.chars().count() - gen_line.trim_start().chars().count();
        let from = if line == gl { from } else { 0 };
        let at = from + src_line.get(from..)?.find(text).filter(|_| !text.is_empty())?;
        let col = src_line[..at].chars().count() as u32 + col.saturating_sub(indent as u32);
        Some((source, (first + (line - gl) as usize) as u32, col))
    }

    /// Maps a 1-based `line:col` in a generated file to the 1-based source position, exactly
    /// when the generated text matches the source there, else at the nearest mark before it.
    /// None for files without a source map.
    pub fn map_location(&mut self, gen: &Path, line: u32, col: u32) -> Option<(PathBuf, u32, u32)> {
        let (l, c) = (line.checked_sub(1)?, col.checked_sub(1)?);
        if let Some((f, sl, sc)) = self.map_exact(gen, l, c).or_else(|| self.map_raw(gen, l, c)) {
            return Some((f, sl + 1, sc + 1));
        }
        let map = self.map_for(gen)?;
        let (sl, sc) = map.lookup(l, c)?;
        Some((map.source.clone(), sl + 1, sc + 1))
    }

    /// An error only a bug in Oxie can cause: one inside the runtime module, or a Rust syntax
    /// error in code generated from a file without verbatim Rust (`rust { }` blocks and macro
    /// calls are copied as they are, so their syntax errors are the user's). It is reported as
    /// such, at the nearest `.oxi` position, with rustc's message and the generated line.
    fn compiler_bug(&mut self, root: &Path, span: &Value, level: &str, code: Option<&str>, message: &str) -> Option<MappedDiag> {
        const SYNTAX: &[&str] = &["expected ", "unexpected ", "mismatched closing delimiter", "unclosed delimiter", "this file contains an unclosed delimiter", "unknown start of token", "unterminated "];
        if level != "error" {
            return None;
        }
        let gen = Self::gen_path(root, span)?;
        let n = |k: &str| span.get(k).and_then(|v| v.as_u64()).map(|v| v as u32);
        let (gl, gc) = (n("line_start")?, n("column_start")?);
        let (file, line, col) = if gen.file_name().is_some_and(|f| f == "__oxie.rs") {
            (gen.clone(), gl - 1, gc - 1)
        } else if code.is_none() && SYNTAX.iter().any(|p| message.starts_with(p)) {
            let (src, sl, sc) = self.map_location(&gen, gl, gc)?;
            if has_verbatim_rust(self.text_of(&src)) {
                return None;
            }
            (src, sl - 1, sc - 1)
        } else {
            return None;
        };
        let generated = self.text_of(&gen).lines().nth(gl as usize - 1).unwrap_or("").trim().to_string();
        Some(MappedDiag {
            level: "error".into(),
            code: None,
            message: "Oxie generated Rust that doesn't compile (a bug in Oxie)".into(),
            file,
            line,
            col,
            end_line: line,
            end_col: col + 1,
            label: None,
            notes: vec![
                format!("rustc: {} ({}:{}:{})", message, gen.display(), gl, gc),
                format!("generated: `{}`", generated),
                "help: please report it at https://github.com/oxie-lang/oxie/issues with the .oxi code; `oxie emit <file>` prints the generated Rust".into(),
            ],
            from_oxie: true,
            secondary: Vec::new(),
            suggestions: Vec::new(),
            multipart: Vec::new(),
            raw_suggestions: Vec::new(),
            rust_message: None,
            gen_text: None,
            fixes: Vec::new(),
            covers_position: false,
        })
    }

    fn gen_path(root: &Path, span: &Value) -> Option<PathBuf> {
        let mut path = PathBuf::from(span.get("file_name")?.as_str()?);
        if path.is_relative() {
            path = root.join(path);
        }
        Some(path)
    }

    fn span_text(&mut self, root: &Path, span: &Value) -> Option<String> {
        let path = Self::gen_path(root, span)?;
        let n = |k: &str| span.get(k).and_then(|v| v.as_u64()).map(|v| v as usize - 1);
        let (l, c, el, ec) = (n("line_start")?, n("column_start")?, n("line_end")?, n("column_end")?);
        if l != el {
            return None;
        }
        let line = self.text_of(&path).lines().nth(l)?.to_string();
        Some(line.chars().skip(c).take(ec.saturating_sub(c)).collect())
    }

    /// Maps a span exactly (both ends), for applying suggestions.
    fn map_span_exact(&mut self, root: &Path, span: &Value) -> Option<(PathBuf, u32, u32, u32, u32)> {
        let path = Self::gen_path(root, span)?;
        let n = |k: &str| span.get(k).and_then(|v| v.as_u64()).map(|v| v as u32 - 1);
        let (f, sl, sc) = self.map_exact(&path, n("line_start")?, n("column_start")?)?;
        let (_, el, ec) = self.map_exact(&path, n("line_end")?, n("column_end")?)?;
        Some((f, sl, sc, el, ec))
    }

    pub fn map_message(&mut self, root: &Path, msg: &Value) -> Option<MappedDiag> {
        let level = msg.get("level")?.as_str()?.to_string();
        let message = msg.get("message")?.as_str()?.to_string();
        let code = msg.get("code").and_then(|c| c.get("code")).and_then(|c| c.as_str()).map(|s| s.to_string());
        let spans = msg.get("spans")?.as_array()?;
        // prefer the deepest macro expansion site that is in user code
        let primary = spans.iter().find(|s| s.get("is_primary").and_then(|p| p.as_bool()).unwrap_or(false)).or(spans.first())?;
        let span = expansion_site(primary);
        if let Some(d) = self.compiler_bug(root, span, &level, code.as_deref(), &message) {
            return Some(d);
        }
        let (file, line, col, end_line, end_col, from_oxie) = self.map_span(root, span)?;
        let label = primary.get("label").and_then(|l| l.as_str()).map(|s| s.to_string());
        let gen_text = self.span_text(root, span);
        let mut secondary = Vec::new();
        for s in spans {
            if std::ptr::eq(s, primary) {
                continue;
            }
            let Some(text) = s.get("label").and_then(|l| l.as_str()).filter(|l| !l.is_empty()) else { continue };
            let site = expansion_site(s);
            if let Some((f, l, c, el, ec)) = self.map_span_exact(root, site) {
                if f == file && el == l {
                    secondary.push(Label { file: None, line: l, col: c, end_col: ec, text: text.to_string(), exact: true });
                    continue;
                }
            }
            if let Some((f, l, c, el, ec, true)) = self.map_span(root, site) {
                if f == file {
                    secondary.push(Label { file: None, line: l, col: c, end_col: if el == l { ec } else { c + 1 }, text: text.to_string(), exact: false });
                }
            }
        }
        let mut notes = Vec::new();
        let mut suggestions = Vec::new();
        let mut multipart_suggestions = Vec::new();
        let mut raw_suggestions = Vec::new();
        if let Some(children) = msg.get("children").and_then(|c| c.as_array()) {
            for c in children {
                let lvl = c.get("level").and_then(|l| l.as_str()).unwrap_or("note");
                let m = c.get("message").and_then(|l| l.as_str()).unwrap_or("");
                if m.is_empty() {
                    continue;
                }
                let mut text = format!("{}: {}", lvl, m);
                let child_spans = c.get("spans").and_then(|s| s.as_array()).cloned().unwrap_or_default();
                // replacements at different places are parts of one suggestion, which only work
                // together (alternatives replace the same span)
                let range = |sp: &Value| ["file_name", "byte_start", "byte_end"].map(|k| sp.get(k).cloned());
                let parts: Vec<&Value> = child_spans.iter().filter(|sp| sp.get("suggested_replacement").is_some_and(|s| s.is_string())).collect();
                let multipart = parts.windows(2).any(|w| range(w[0]) != range(w[1]));
                // the code of a suggestion with several parts isn't one piece of text
                if let Some(sp) = child_spans.first().filter(|_| !multipart) {
                    if let Some(sugg) = sp.get("suggested_replacement").and_then(|s| s.as_str()) {
                        if !sugg.is_empty() && sugg.len() < 80 && !sugg.contains('\n') {
                            text.push_str(&format!(": `{}`", sugg));
                        }
                    }
                }
                let mut mapped = Vec::new();
                for sp in &parts {
                    let sugg = sp.get("suggested_replacement").and_then(|s| s.as_str()).unwrap_or("");
                    raw_suggestions.push(sugg.to_string());
                    if let Some((f, l, c, el, ec)) = self.map_span_exact(root, sp) {
                        if f == file {
                            mapped.push(Suggestion { line: l, col: c, end_line: el, end_col: ec, replacement: sugg.to_string() });
                        }
                    }
                }
                // a suggestion with several parts (not alternatives that each have several) is kept
                // when they map in order onto one line: a part can map exactly onto another line, as
                // the `)` after a default argument does onto the parameter's declaration
                let one_line = mapped.iter().all(|s| s.line == mapped[0].line && s.end_line == s.line) && mapped.windows(2).all(|w| w[0].end_col <= w[1].col);
                if !multipart {
                    suggestions.extend(mapped);
                } else if mapped.len() == parts.len() && parts.windows(2).all(|w| follows(w[0], w[1])) && one_line {
                    multipart_suggestions.push(Multipart { message: m.to_string(), parts: mapped });
                }
                notes.push(text);
            }
        }
        let mut d = MappedDiag {
            level,
            code,
            message,
            file,
            line,
            col,
            end_line,
            end_col,
            label,
            notes,
            from_oxie,
            secondary,
            suggestions,
            multipart: multipart_suggestions,
            raw_suggestions,
            rust_message: None,
            gen_text,
            fixes: Vec::new(),
            covers_position: false,
        };
        if d.from_oxie {
            let sources = self.sources_of(&d.file);
            crate::explain::explain(&mut d, &sources);
        }
        Some(d)
    }

    /// The `.oxi` files of the crate `file` belongs to, read once per crate.
    fn sources_of(&mut self, file: &Path) -> Rc<Sources> {
        if let Some(s) = self.sources.iter().find(|s| s.file(file).is_some()) {
            return s.clone();
        }
        let s = Rc::new(Sources::of(file));
        self.sources.push(s.clone());
        s
    }
}

/// The character columns where `word` occurs as a whole word in `text`.
fn word_positions(text: &str, word: &str) -> Vec<usize> {
    let chars: Vec<char> = text.chars().collect();
    let w: Vec<char> = word.chars().collect();
    let is_word = |c: &char| c.is_alphanumeric() || *c == '_';
    (0..chars.len().saturating_sub(w.len() - 1))
        .filter(|&i| chars[i..i + w.len()] == w[..] && !(i > 0 && is_word(&chars[i - 1])) && !chars.get(i + w.len()).is_some_and(is_word))
        .collect()
}

/// How many characters of `src` the generated text `gen` stands for, when they are the same
/// apart from spellings Oxie translates in place: `let mut`/`var`, `let`/`val`, `use`/`import`
/// and the `::` of paths written with `.`.
fn align(gen: &str, src: &str) -> Option<usize> {
    const SAME: &[(&str, &str)] = &[("let mut ", "var "), ("let ", "val "), ("let ", "var "), ("use ", "import "), ("::", ".")];
    let (g, s): (Vec<char>, Vec<char>) = (gen.chars().collect(), src.chars().collect());
    let (mut i, mut j) = (0, 0);
    'outer: while i < g.len() {
        if j < s.len() && g[i] == s[j] {
            i += 1;
            j += 1;
            continue;
        }
        for (a, b) in SAME {
            let (a, b): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
            if g[i..].starts_with(&a) && s.get(j..).is_some_and(|r| r.starts_with(&b)) {
                i += a.len();
                j += b.len();
                continue 'outer;
            }
        }
        return None;
    }
    Some(j)
}

/// Does the source hold Rust that Oxie copies as it is: a `rust { }` block or a macro call
/// (`name!(..)`)?
fn has_verbatim_rust(src: &str) -> bool {
    let b = src.as_bytes();
    src.contains("rust {") || src.contains("rust{") || (1..b.len().saturating_sub(1)).any(|i| b[i] == b'!' && matches!(b[i + 1], b'(' | b'[' | b'{') && (b[i - 1].is_ascii_alphanumeric() || b[i - 1] == b'_'))
}

/// Does the replacement `sp` come after `prev` in the same file, as the next part of a
/// suggestion that edits several places?
fn follows(prev: &Value, sp: &Value) -> bool {
    let range = |v: &Value| ["byte_start", "byte_end"].map(|k| v.get(k).and_then(|b| b.as_u64()));
    let ([start, _], [_, prev_end]) = (range(sp), range(prev));
    sp.get("file_name") == prev.get("file_name") && start >= prev_end && range(sp) != range(prev)
}

fn expansion_site(span: &Value) -> &Value {
    let mut cur = span;
    // For errors inside macros (println! etc.), use the call site.
    while let Some(exp) = cur.get("expansion").filter(|e| !e.is_null()) {
        let file = cur.get("file_name").and_then(|f| f.as_str()).unwrap_or("");
        if file.starts_with('<') || file.contains("/rustlib/") || file.contains("/.cargo/registry/") {
            match exp.get("span") {
                Some(s) => cur = s,
                None => break,
            }
        } else {
            break;
        }
    }
    cur
}

pub fn color() -> bool {
    std::io::stderr().is_terminal() && std::env::var("NO_COLOR").is_err()
}

/// The current directory as the system reports it, then in its canonical form when that differs:
/// Windows can report a short name (`C:\Users\RUNNER~1`) where a canonical path has the long one,
/// and a script's path is canonical.
pub fn cwd_forms() -> Vec<PathBuf> {
    let cwd = std::env::current_dir().unwrap_or_default();
    let canonical = oxie_compiler::compile::canonicalize(&cwd).ok().filter(|c| *c != cwd);
    std::iter::once(cwd).chain(canonical).collect()
}

/// `p` relative to the current directory (in either form), when it is inside it.
pub fn relative_to_cwd(p: &Path) -> Option<PathBuf> {
    cwd_forms().iter().find_map(|c| p.strip_prefix(c).ok().map(Path::to_path_buf))
}

pub fn render(d: &MappedDiag, root: &Path) -> String {
    let c = color();
    let (red, yellow, blue, bold, reset) = if c { ("\x1b[31m", "\x1b[33m", "\x1b[34m", "\x1b[1m", "\x1b[0m") } else { ("", "", "", "", "") };
    let lc = if d.level == "error" { red } else { yellow };
    let shown = |p: &Path| relative_to_cwd(p).or_else(|| p.strip_prefix(root).ok().map(Path::to_path_buf)).unwrap_or_else(|| p.to_path_buf()).display().to_string();
    let text = std::fs::read_to_string(&d.file).unwrap_or_default();
    let lines: Vec<&str> = text.lines().collect();
    let primary_width = if d.end_line == d.line { d.end_col.saturating_sub(d.col).max(1) } else { 1 };
    // (line, col, width, label, is_primary)
    let mut labels: Vec<(u32, u32, u32, String, bool)> = vec![(d.line, d.col, primary_width, d.label.clone().unwrap_or_default(), true)];
    // labels in other files of the crate, by file
    let mut elsewhere: Vec<(&Path, Vec<(u32, u32, u32, String, bool)>)> = Vec::new();
    for s in &d.secondary {
        let label = (s.line, s.col, s.end_col.saturating_sub(s.col).max(1), s.text.clone(), false);
        match &s.file {
            None => labels.push(label),
            Some(f) => match elsewhere.iter_mut().find(|(p, _)| p == f) {
                Some((_, ls)) => ls.push(label),
                None => elsewhere.push((f, vec![label])),
            },
        }
    }
    let num_width = labels.iter().chain(elsewhere.iter().flat_map(|(_, ls)| ls)).map(|l| (l.0 + 1).to_string().len()).max().unwrap_or(1);
    let pad = " ".repeat(num_width);
    let snippet = |out: &mut String, lines: &[&str], labels: &mut Vec<(u32, u32, u32, String, bool)>| {
        labels.sort_by_key(|l| (l.0, !l.4, l.1));
        let mut last_line: Option<u32> = None;
        for (line, col, width, label, is_primary) in labels.iter() {
            let line_text = lines.get(*line as usize).copied().unwrap_or("");
            if last_line != Some(*line) {
                if last_line.is_some_and(|l| *line > l + 1) {
                    *out += &format!("{blue}{}{reset}\n", ".".repeat(num_width.max(3)));
                }
                *out += &format!("{blue}{:>num_width$} |{reset} {}\n", line + 1, line_text);
            }
            last_line = Some(*line);
            let width = (*width as usize).min(line_text.chars().count().saturating_sub(*col as usize).max(1));
            let (mark, color) = if *is_primary { ("^", lc) } else { ("-", blue) };
            *out += &format!("{pad} {blue}|{reset} {}{color}{}{reset}", " ".repeat(*col as usize), mark.repeat(width));
            if !label.is_empty() {
                *out += &format!(" {color}{}{reset}", label);
            }
            out.push('\n');
        }
    };
    let code = d.code.as_ref().map(|c| format!("[{}]", c)).unwrap_or_default();
    let mut out = format!("{bold}{lc}{}{}{reset}{bold}: {}{reset}\n", d.level, code, d.message);
    out += &format!("{pad}{blue}-->{reset} {}:{}:{}\n", shown(&d.file), d.line + 1, d.col + 1);
    out += &format!("{pad} {blue}|{reset}\n");
    snippet(&mut out, &lines, &mut labels);
    for (file, mut ls) in elsewhere {
        let text = std::fs::read_to_string(file).unwrap_or_default();
        let first = ls.iter().map(|l| (l.0, l.1)).min().unwrap_or_default();
        out += &format!("{pad} {blue}|{reset}\n{pad}{blue}:::{reset} {}:{}:{}\n{pad} {blue}|{reset}\n", shown(file), first.0 + 1, first.1 + 1);
        snippet(&mut out, &text.lines().collect::<Vec<_>>(), &mut ls);
    }
    for n in &d.notes {
        let n = n.replace('\n', &format!("\n{pad}   "));
        out += &format!("{pad} {blue}={reset} {}\n", n);
    }
    for f in &d.fixes {
        out += &format!("{pad} {blue}={reset} {bold}fix{reset}: {}", f.title);
        // a fix in another file says where
        let other = f.edits.first().and_then(|e| e.file.as_deref()).filter(|o| f.edits.iter().all(|e| e.file.as_deref() == Some(*o)));
        if let Some(o) = other {
            out += &format!(" (in {})", shown(o).replace('\\', "/"));
        }
        let other_text = other.map(|o| std::fs::read_to_string(o).unwrap_or_default());
        let preview_lines: Vec<&str> = other_text.as_deref().unwrap_or(&text).lines().collect();
        if let Some(preview) = fix_preview(&preview_lines, f) {
            out += &format!(": `{}`", preview);
        }
        out.push('\n');
    }
    if let Some(m) = &d.rust_message {
        out += &format!("{pad} {blue}={reset} rustc: {}\n", m);
    }
    out
}

/// The changed line, when a fix only touches one line (of one file).
fn fix_preview(lines: &[&str], f: &Fix) -> Option<String> {
    let first = f.edits.first()?;
    let line = first.line;
    if f.edits.iter().any(|e| e.line != line || e.end_line != line || e.text.contains('\n') || e.file != first.file) {
        return None;
    }
    let mut chars: Vec<char> = lines.get(line as usize)?.chars().collect();
    let mut edits = f.edits.clone();
    edits.sort_by_key(|e| std::cmp::Reverse(e.col));
    for e in edits {
        let (s, t) = (e.col as usize, (e.end_col as usize).min(chars.len()));
        if s > t {
            return None;
        }
        chars.splice(s..t, e.text.chars());
    }
    Some(chars.into_iter().collect::<String>().trim().to_string())
}

/// Runs `cargo <args> --message-format=json-diagnostic-rendered-ansi` and prints mapped diagnostics.
pub fn cargo_json(root: &Path, args: &[String], quiet: bool, shown_name: Option<&str>) -> std::io::Result<BuildResult> {
    let fmt = if color() { "--message-format=json-diagnostic-rendered-ansi" } else { "--message-format=json" };
    let mut cmd = Command::new(std::env::var("CARGO").unwrap_or_else(|_| "cargo".into()));
    cmd.args(args).arg(fmt).current_dir(root).stdout(Stdio::piped());
    if quiet {
        cmd.arg("--quiet");
    }
    // `never` too: a `CARGO_TERM_COLOR=always` in the environment would color piped output
    cmd.arg(if color() { "--color=always" } else { "--color=never" });
    // cargo's error output passes through, apart from its summary lines, which are rewritten at
    // the end. A terminal keeps cargo's progress bar, which needs the terminal's width.
    let pipe_stderr = quiet || !std::io::stderr().is_terminal() || match terminal_width() {
        Some(w) => {
            cmd.env("CARGO_TERM_PROGRESS_WHEN", "always").env("CARGO_TERM_PROGRESS_WIDTH", w.to_string());
            true
        }
        None => false,
    };
    if pipe_stderr {
        cmd.stderr(Stdio::piped());
    }
    let pkg = std::fs::read_to_string(root.join("Cargo.toml")).ok().and_then(|m| crate::project::package_name(&m)).unwrap_or_default();
    let mut child = cmd.spawn()?;
    let stdout = child.stdout.take().unwrap();
    let summaries: std::sync::Arc<std::sync::Mutex<Vec<String>>> = Default::default();
    let summaries_thread = child.stderr.take().map(|err| {
        let summaries = summaries.clone();
        std::thread::spawn(move || {
            let edit = move |l: &str| {
                if is_summary(l.as_bytes()) {
                    summaries.lock().unwrap().push(l.to_string());
                    None
                } else {
                    Some(l.to_string())
                }
            };
            let _ = std::io::copy(&mut crate::locations::EditLines::new(err, is_summary, edit), &mut std::io::stderr());
        })
    });
    let (mut own_errors, mut own_warnings) = (0, 0);
    let mut mapper = Mapper::default();
    let mut seen = HashSet::new();
    let mut covered = Covered::default();
    let mut held = Vec::new();
    let mut result = BuildResult { success: false, executables: Vec::new(), errors: 0, warnings: 0 };
    for line in BufReader::new(stdout).lines() {
        let line = line?;
        let Ok(v) = serde_json::from_str::<Value>(&line) else {
            println!("{}", line);
            continue;
        };
        match v.get("reason").and_then(|r| r.as_str()) {
            Some("compiler-message") => {
                let Some(msg) = v.get("message") else { continue };
                let level = msg.get("level").and_then(|l| l.as_str()).unwrap_or("");
                if level == "failure-note" {
                    continue;
                }
                // the warnings of the user's package only (not of path dependencies like
                // oxie-build), but errors of any package, since they stop the build
                let Some(pkg_root) = package_root(&v, root, level) else { continue };
                let mapped = mapper.map_message(&pkg_root, msg);
                if mapped.as_ref().is_some_and(|d| d.from_oxie && !covered.cover(d)) {
                    continue;
                }
                // one explained error per position: rustc reports some twice (`?:` on a value
                // that isn't optional), with different Rust wording
                let (rendered, key) = match mapped {
                    Some(d) if d.from_oxie => (render(&d, root), same_error(&d)),
                    Some(_) | None => {
                        let r = relative_paths(msg.get("rendered").and_then(|r| r.as_str()).unwrap_or(""), root);
                        // summary lines like "aborting due to ..." have no spans
                        if msg.get("spans").and_then(|s| s.as_array()).is_none_or(|s| s.is_empty()) {
                            let m = msg.get("message").and_then(|m| m.as_str()).unwrap_or("");
                            if m.starts_with("aborting due to") || m.contains("warnings emitted") || m.contains("warning emitted") {
                                continue;
                            }
                        }
                        (r.clone(), r)
                    }
                };
                if rendered.trim().is_empty() || !seen.insert(key) {
                    continue;
                }
                // rustc's borrow check calls a `var` unused when changing it failed: such
                // warnings wait until the build turns out to have no errors
                if is_unused_mut(&v) {
                    held.push(rendered);
                    continue;
                }
                let own = pkg_root == root;
                match level {
                    "error" => {
                        result.errors += 1;
                        own_errors += usize::from(own);
                    }
                    "warning" => {
                        result.warnings += 1;
                        own_warnings += usize::from(own);
                    }
                    _ => {}
                }
                eprintln!("{}", rendered);
            }
            Some("compiler-artifact") => {
                if let Some(exe) = v.get("executable").and_then(|e| e.as_str()) {
                    result.executables.push(PathBuf::from(exe));
                }
            }
            Some("build-finished") => {
                result.success = v.get("success").and_then(|s| s.as_bool()).unwrap_or(false);
            }
            _ => {}
        }
    }
    if result.errors == 0 {
        for rendered in held {
            result.warnings += 1;
            own_warnings += 1;
            eprintln!("{}", rendered);
        }
    }
    let status = child.wait()?;
    result.success = result.success && status.success();
    // cargo's summaries, with the counts of what was shown
    if let Some(t) = summaries_thread {
        let _ = t.join();
    }
    let mut first = true;
    for line in summaries.lock().unwrap().iter() {
        if let Some(l) = rewrite_summary(line, &pkg, shown_name, own_errors, own_warnings, &mut first) {
            eprintln!("{}", l);
        }
    }
    Ok(result)
}

/// Is this (the start of) one of cargo's summary lines, `error: could not compile `app` (bin
/// "app") due to 2 previous errors; 1 warning emitted` or `warning: `app` (bin "app") generated
/// 1 warning`?
fn is_summary(line: &[u8]) -> bool {
    let plain = strip_ansi(&String::from_utf8_lossy(line));
    ["error: could not compile `", "warning: `"].iter().any(|s| if plain.len() < s.len() { s.starts_with(plain.as_str()) } else { plain.starts_with(s) })
}

fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            // `ESC [ ... letter`
            for c in chars.by_ref() {
                if c.is_ascii_alphabetic() {
                    break;
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// cargo's summary line with the numbers of errors and warnings shown, which differ from rustc's:
/// explanations cover some of rustc's messages, `unused_mut` warnings wait for a build without
/// errors, and the warnings of dependencies aren't shown. The first summary of the user's package
/// gets its totals and later ones are dropped; a script is called by its file name, not by the
/// name of its hidden crate. cargo's `cargo fix` hint would change the generated Rust, so it goes.
fn rewrite_summary(line: &str, pkg: &str, shown_name: Option<&str>, errors: usize, warnings: usize, first: &mut bool) -> Option<String> {
    let plain = strip_ansi(line.trim_end());
    let name = plain.split('`').nth(1)?;
    let plural = |n: usize, what: &str| format!("{} {}{}", n, what, if n == 1 { "" } else { "s" });
    let (red, yellow, bold, reset) = if color() { ("\x1b[1m\x1b[31m", "\x1b[1m\x1b[33m", "\x1b[1m", "\x1b[0m") } else { ("", "", "", "") };
    // `app` (bin "app"), or the script's file
    let what = |head: &str| match shown_name {
        Some(s) if name == pkg => format!("`{}`", s),
        _ => head.to_string(),
    };
    if let Some(rest) = plain.strip_prefix("warning: ") {
        let (head, _) = rest.split_once(" generated ")?;
        if name != pkg || !std::mem::take(first) || warnings == 0 {
            return None;
        }
        return Some(format!("{yellow}warning{reset}{bold}:{reset} {} generated {}", what(head), plural(warnings, "warning")));
    }
    let rest = plain.strip_prefix("error: could not compile ")?;
    let (head, counts) = rest.split_once(" due to ")?;
    let theirs = counts.split(';').next().unwrap_or(counts);
    let counts = if name != pkg {
        theirs.to_string()
    } else if !std::mem::take(first) {
        return None;
    } else {
        let errors = if errors == 0 { theirs.to_string() } else { plural(errors, "previous error") };
        if warnings == 0 { errors } else { format!("{}; {} emitted", errors, plural(warnings, "warning")) }
    };
    Some(format!("{red}error{reset}{bold}:{reset} could not compile {} due to {}", what(head), counts))
}

/// The directory of the package a cargo message is about, when it is shown: every message of the
/// package at `root`, and the errors of other packages (a path dependency that doesn't compile),
/// whose warnings aren't the user's to fix.
fn package_root(v: &Value, root: &Path, level: &str) -> Option<PathBuf> {
    let manifest = v.get("manifest_path").and_then(|m| m.as_str()).map(PathBuf::from);
    match manifest.as_deref().and_then(Path::parent) {
        Some(dir) if dir != root => (level == "error").then(|| dir.to_path_buf()),
        _ => Some(root.to_path_buf()),
    }
}

/// rustc's text with the paths of the project's files relative to the current directory, as
/// `render` shows them: a hand-written `.rs` module is included by its absolute path.
fn relative_paths(rendered: &str, root: &Path) -> String {
    let cwd = cwd_forms().into_iter().find(|c| root.starts_with(c) && c.parent().is_some());
    let base = cwd.unwrap_or_else(|| root.to_path_buf());
    rendered.replace(&format!("{}{}", base.display(), std::path::MAIN_SEPARATOR), "")
}

/// The width of the terminal, from `COLUMNS` or `stty size` on the terminal.
fn terminal_width() -> Option<usize> {
    if let Some(w) = std::env::var("COLUMNS").ok().and_then(|c| c.parse().ok()) {
        return Some(w);
    }
    let tty = std::fs::File::open("/dev/tty").ok()?;
    let out = Command::new("stty").arg("size").stdin(tty).stderr(Stdio::null()).output().ok()?;
    String::from_utf8_lossy(&out.stdout).split_whitespace().nth(1)?.parse().ok().filter(|w| *w > 0)
}

/// Is this cargo message rustc's `unused_mut` warning?
fn is_unused_mut(v: &Value) -> bool {
    v.pointer("/message/code/code").and_then(|c| c.as_str()) == Some("unused_mut")
}

/// Collects mapped diagnostics without printing (used by the language server).
pub fn check_collect(root: &Path, target_dir: Option<&Path>) -> std::io::Result<Vec<MappedDiag>> {
    let mut cmd = Command::new(std::env::var("CARGO").unwrap_or_else(|_| "cargo".into()));
    cmd.args(["check", "--message-format=json", "--quiet", "--color=never"]).current_dir(root).stdout(Stdio::piped()).stderr(Stdio::null());
    if let Some(dir) = target_dir {
        cmd.env("CARGO_TARGET_DIR", dir);
    }
    let out = cmd.output()?;
    let mut mapper = Mapper::default();
    let mut diags = Vec::new();
    let mut seen = HashSet::new();
    let mut covered = Covered::default();
    for line in String::from_utf8_lossy(&out.stdout).lines() {
        let Ok(v) = serde_json::from_str::<Value>(line) else { continue };
        if v.get("reason").and_then(|r| r.as_str()) != Some("compiler-message") {
            continue;
        }
        if let Some(msg) = v.get("message") {
            let level = msg.get("level").and_then(|l| l.as_str()).unwrap_or("");
            let Some(pkg_root) = package_root(&v, root, level) else { continue };
            if let Some(d) = mapper.map_message(&pkg_root, msg) {
                let key = same_error(&d);
                if covered.cover(&d) && seen.insert(key) {
                    diags.push(d);
                }
            }
        }
    }
    // as in `cargo_json`: `unused_mut` only counts in a build without errors
    if diags.iter().any(|d| d.level == "error") {
        diags.retain(|d| d.code.as_deref() != Some("unused_mut"));
    }
    Ok(diags)
}

/// Diagnostics with the same key are one error to the user: the same kind and code, explained
/// the same way at the same position.
fn same_error(d: &MappedDiag) -> String {
    format!("{}:{}:{}:{}:{}:{}", d.file.display(), d.line, d.col, d.level, d.code.as_deref().unwrap_or(""), d.message)
}

/// Positions whose mistake an earlier explanation already named (`covers_position`).
#[derive(Default)]
struct Covered(HashSet<(PathBuf, u32, u32)>);

impl Covered {
    /// Records the diagnostic's position if it covers it; false when an earlier one did.
    fn cover(&mut self, d: &MappedDiag) -> bool {
        let at = (d.file.clone(), d.line, d.col);
        if self.0.contains(&at) {
            return false;
        }
        if d.covers_position {
            self.0.insert(at);
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A generated `main.rs` (with its map) and `__oxie.rs` for `src`, in a fresh directory.
    fn generated(name: &str, src: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("oxie-bugs-{}-{}", std::process::id(), name));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("app.oxi"), src).unwrap();
        std::fs::write(dir.join("main.rs"), "fn main() {\n    let x = 1 2;\n}\n").unwrap();
        std::fs::write(dir.join("main.rs.map"), format!("oxie-sourcemap 1\n{}\n0 0 0 0\n1 4 1 4\n", dir.join("app.oxi").display())).unwrap();
        std::fs::write(dir.join("__oxie.rs"), "pub fn helper() {\n    undefined();\n}\n").unwrap();
        dir
    }

    fn message(file: &Path, line: u32, col: u32, code: Option<&str>, text: &str) -> Value {
        serde_json::json!({
            "level": "error",
            "message": text,
            "code": code.map(|c| serde_json::json!({"code": c})),
            "spans": [{"file_name": file.display().to_string(), "line_start": line, "line_end": line, "column_start": col, "column_end": col + 1, "is_primary": true, "label": null, "expansion": null}],
            "children": []
        })
    }

    #[test]
    fn syntax_errors_in_generated_code_are_compiler_bugs() {
        let dir = generated("syntax", "fun main() {\n    val x = 1\n}\n");
        let d = Mapper::default().map_message(&dir, &message(&dir.join("main.rs"), 2, 15, None, "expected `;`, found `2`")).unwrap();
        assert!(d.message.contains("a bug in Oxie"), "{}", d.message);
        assert_eq!((d.file.file_name().unwrap().to_str(), d.line), (Some("app.oxi"), 1));
        assert!(d.notes.iter().any(|n| n.contains("let x = 1 2;")), "{:?}", d.notes);
        // an error with a code is a type or borrow error, which the user can cause
        let d = Mapper::default().map_message(&dir, &message(&dir.join("main.rs"), 2, 13, Some("E0308"), "mismatched types")).unwrap();
        assert!(!d.message.contains("a bug in Oxie"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn syntax_errors_in_verbatim_rust_are_the_users() {
        let dir = generated("verbatim", "fun main() {\n    rust { let x = 1 2; }\n}\n");
        let d = Mapper::default().map_message(&dir, &message(&dir.join("main.rs"), 2, 15, None, "expected `;`, found `2`")).unwrap();
        assert!(!d.message.contains("a bug in Oxie"), "{}", d.message);
        assert!(has_verbatim_rust("val v = my_macro!(1)") && !has_verbatim_rust("if (!(a && b)) x != y"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// A suggestion that edits several places is one fix, in Oxie terms, when its parts map onto
    /// one line of the source, and its note doesn't show the first part alone.
    #[test]
    fn suggestions_with_several_parts_are_whole() {
        // `vec![Circle::new(1.5)]` generated from `src` with these marks, and rustc's suggestion to
        // put `Box::new(` and `)` around the value
        let boxed = |name: &str, src: &str, marks: &str| {
            let dir = std::env::temp_dir().join(format!("oxie-parts-{}-{}", std::process::id(), name));
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("app.oxi"), src).unwrap();
            std::fs::write(dir.join("main.rs"), "fn main() {\n    let v = vec![Circle::new(1.5)];\n}\n").unwrap();
            std::fs::write(dir.join("main.rs.map"), format!("oxie-sourcemap 1\n{}\n{}", dir.join("app.oxi").display(), marks)).unwrap();
            let main = dir.join("main.rs").display().to_string();
            let span = |start: u32, end: u32, text: Option<&str>| {
                serde_json::json!({"file_name": main, "line_start": 2, "line_end": 2, "column_start": start, "column_end": end, "byte_start": 11 + start, "byte_end": 11 + end, "is_primary": true, "label": null, "expansion": null, "suggested_replacement": text})
            };
            let help = serde_json::json!({"level": "help", "message": "store this in the heap by calling `Box::new`", "spans": [span(18, 18, Some("Box::new(")), span(34, 34, Some(")"))]});
            let msg = serde_json::json!({"level": "error", "message": "mismatched types", "code": {"code": "E0308"}, "spans": [span(18, 34, None)], "children": [help]});
            let d = Mapper::default().map_message(&dir, &msg).unwrap();
            std::fs::remove_dir_all(&dir).unwrap();
            d
        };
        let d = boxed("call", "fun main() {\n    val v = vec[Circle(1.5)]\n}\n", "1 4 1 4\n1 17 1 16\n1 29 1 23\n");
        assert!(!d.notes.iter().any(|n| n.contains("Box::new")), "{:?}", d.notes);
        let fix = d.fixes.iter().find(|f| f.title == "store this in the heap by calling `Box.new`").unwrap_or_else(|| panic!("{:?}", d.fixes));
        let edits: Vec<(u32, u32, &str)> = fix.edits.iter().map(|e| (e.line, e.col, e.text.as_str())).collect();
        assert_eq!(edits, vec![(1, 16, "Box.new("), (1, 27, ")")]);
        // the `1.5` of `Circle()` is a default argument, so the `)` after it maps onto the declaration
        let d = boxed("default", "class Circle(r: f64 = 1.5)\n\nfun main() {\n    val v = vec[Circle()]\n}\n", "1 4 3 4\n1 17 3 16\n1 29 0 22\n");
        assert!(d.fixes.is_empty(), "{:?}", d.fixes);
        assert!(d.notes.iter().any(|n| n == "help: store this in the heap by calling `Box::new`"), "{:?}", d.notes);
    }

    #[test]
    fn errors_in_the_runtime_module_are_compiler_bugs() {
        let dir = generated("runtime", "fun main() {}\n");
        let d = Mapper::default().map_message(&dir, &message(&dir.join("__oxie.rs"), 2, 5, Some("E0425"), "cannot find function `undefined` in this scope")).unwrap();
        assert!(d.message.contains("a bug in Oxie"), "{}", d.message);
        assert_eq!(d.file.file_name().unwrap(), "__oxie.rs");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
