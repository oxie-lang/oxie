//! Diagnostics and source positions.

use std::fmt;

/// A byte range in a source file.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Span {
    pub start: u32,
    pub end: u32,
}

impl Span {
    pub fn new(start: usize, end: usize) -> Span {
        Span { start: start as u32, end: end as u32 }
    }
    pub fn to(self, other: Span) -> Span {
        Span { start: self.start.min(other.start), end: self.end.max(other.end) }
    }
    pub fn empty_at(pos: u32) -> Span {
        Span { start: pos, end: pos }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Severity {
    Error,
    Warning,
}

#[derive(Clone, Debug)]
pub struct Diag {
    pub severity: Severity,
    pub span: Span,
    pub message: String,
    pub help: Option<String>,
    /// Quick fixes, printed as `= fix:` lines and offered by the editor.
    pub fixes: Vec<DiagFix>,
}

/// A quick fix: text that replaces spans of the diagnostic's file.
#[derive(Clone, Debug)]
pub struct DiagFix {
    pub title: String,
    pub edits: Vec<(Span, String)>,
}

/// What Oxie says about a class listed as a supertype (`class Dog : Animal(name)`).
pub fn no_inheritance(name: &str) -> String {
    format!("Oxie has no class inheritance, so `{}` can't be extended: make `{}` an interface (listed without arguments), or keep a `{}` in a field", name, name, name)
}

impl Diag {
    pub fn error(span: Span, message: impl Into<String>) -> Diag {
        Diag { severity: Severity::Error, span, message: message.into(), help: None, fixes: Vec::new() }
    }
    pub fn warning(span: Span, message: impl Into<String>) -> Diag {
        Diag { severity: Severity::Warning, span, message: message.into(), help: None, fixes: Vec::new() }
    }
    pub fn with_help(mut self, help: impl Into<String>) -> Diag {
        self.help = Some(help.into());
        self
    }
    pub fn with_fix(mut self, title: impl Into<String>, edits: Vec<(Span, String)>) -> Diag {
        self.fixes.push(DiagFix { title: title.into(), edits });
        self
    }
}

impl DiagFix {
    /// The line the fix changes, after the change, when it only changes one line of `src`.
    pub fn preview(&self, src: &str) -> Option<String> {
        let idx = LineIndex::new(src);
        let line = idx.line_col(src, self.edits.first()?.0.start as usize).0;
        if self.edits.iter().any(|(s, _)| idx.line_col(src, s.start as usize).0 != line || idx.line_col(src, s.end as usize).0 != line) {
            return None;
        }
        let start = idx.line_start(line);
        let mut text = src[start..].lines().next().unwrap_or("").to_string();
        let mut edits: Vec<&(Span, String)> = self.edits.iter().collect();
        edits.sort_by_key(|(s, _)| std::cmp::Reverse(s.start));
        for (s, new) in edits {
            let (a, b) = (s.start as usize - start, s.end as usize - start);
            if b > text.len() || a > b {
                return None;
            }
            text.replace_range(a..b, new);
        }
        Some(text.trim().to_string())
    }
}

/// Maps byte offsets to zero-based (line, column) pairs. Columns count characters.
#[derive(Clone, Debug)]
pub struct LineIndex {
    starts: Vec<usize>,
}

impl LineIndex {
    pub fn new(src: &str) -> LineIndex {
        let mut starts = vec![0];
        for (i, b) in src.bytes().enumerate() {
            if b == b'\n' {
                starts.push(i + 1);
            }
        }
        LineIndex { starts }
    }

    pub fn line_col(&self, src: &str, offset: usize) -> (usize, usize) {
        let offset = offset.min(src.len());
        let line = match self.starts.binary_search(&offset) {
            Ok(l) => l,
            Err(l) => l - 1,
        };
        let start = self.starts[line];
        let col = src.get(start..offset).map(|s| s.chars().count()).unwrap_or(offset - start);
        (line, col)
    }

    /// Column in UTF-16 code units (what LSP clients expect).
    pub fn line_col_utf16(&self, src: &str, offset: usize) -> (usize, usize) {
        let offset = offset.min(src.len());
        let line = match self.starts.binary_search(&offset) {
            Ok(l) => l,
            Err(l) => l - 1,
        };
        let start = self.starts[line];
        let col = src.get(start..offset).map(|s| s.encode_utf16().count()).unwrap_or(offset - start);
        (line, col)
    }

    pub fn offset(&self, src: &str, line: usize, col_utf16: usize) -> usize {
        let Some(&start) = self.starts.get(line) else { return src.len() };
        let mut units = 0;
        for (i, ch) in src[start..].char_indices() {
            if units >= col_utf16 || ch == '\n' {
                return start + i;
            }
            units += ch.len_utf16();
        }
        src.len()
    }

    pub fn line_count(&self) -> usize {
        self.starts.len()
    }

    pub fn line_start(&self, line: usize) -> usize {
        self.starts.get(line).copied().unwrap_or(usize::MAX)
    }
}

/// Renders a diagnostic in a rustc-like format.
pub fn render(diag: &Diag, path: &str, src: &str, color: bool) -> String {
    let idx = LineIndex::new(src);
    let (line, col) = idx.line_col(src, diag.span.start as usize);
    let (end_line, end_col) = idx.line_col(src, diag.span.end as usize);
    let (red, yellow, blue, bold, reset) = if color {
        ("\x1b[31m", "\x1b[33m", "\x1b[34m", "\x1b[1m", "\x1b[0m")
    } else {
        ("", "", "", "", "")
    };
    let (label, lc) = match diag.severity {
        Severity::Error => ("error", red),
        Severity::Warning => ("warning", yellow),
    };
    let line_text = src.lines().nth(line).unwrap_or("");
    let num = (line + 1).to_string();
    let pad = " ".repeat(num.len());
    let width = if end_line == line { end_col.saturating_sub(col).max(1) } else { 1 };
    let mut out = String::new();
    out += &format!("{bold}{lc}{label}{reset}{bold}: {}{reset}\n", diag.message);
    out += &format!("{pad}{blue}-->{reset} {}:{}:{}\n", path, line + 1, col + 1);
    out += &format!("{pad} {blue}|{reset}\n");
    out += &format!("{blue}{num} |{reset} {}\n", line_text);
    out += &format!("{pad} {blue}|{reset} {}{lc}{}{reset}\n", " ".repeat(col), "^".repeat(width));
    if let Some(help) = &diag.help {
        out += &format!("{pad} {blue}={reset} {bold}help{reset}: {}\n", help);
    }
    for f in &diag.fixes {
        out += &format!("{pad} {blue}={reset} {bold}fix{reset}: {}", f.title);
        if let Some(line) = f.preview(src) {
            out += &format!(": `{}`", line);
        }
        out.push('\n');
    }
    out
}

impl fmt::Display for Diag {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{}", self.message)
    }
}
