//! Text emitter with indentation and source-map marks.

use crate::diag::Span;

#[derive(Clone, Copy, Debug)]
pub struct Mark {
    pub gen_line: u32,
    pub gen_col: u32,
    pub src: u32,
}

/// A choice the source doesn't spell out (an inferred `&mut self`, a borrowed parameter, a
/// lifetime, a borrow at a call or a loop), with the reason: shown by `oxie emit --annotate`,
/// in hovers and in the explanations of errors it causes.
#[derive(Clone, Debug, PartialEq)]
pub struct Decision {
    /// Where its result ends in the generated text (zero-based).
    pub gen_line: u32,
    pub gen_col: u32,
    /// What it is about: a method or parameter, a loop's collection, an argument.
    pub span: Span,
    /// What Oxie generated: `&mut self`, `&str`, `&mut nums`.
    pub chosen: String,
    /// Why, as a clause: "it assigns `hits`".
    pub why: String,
    /// The source that decided it (the assignment, the use after the loop), if there is one.
    pub cause: Option<Span>,
}

/// A place in the text written so far, to `insert` at later.
#[derive(Clone, Copy)]
pub struct Pos {
    offset: usize,
    line: u32,
    col: u32,
}

#[derive(Default)]
pub struct Emitter {
    pub buf: String,
    line: u32,
    col: u32,
    indent: usize,
    at_line_start: bool,
    pub marks: Vec<Mark>,
    pub decisions: Vec<Decision>,
}

impl Emitter {
    pub fn new() -> Emitter {
        Emitter { at_line_start: true, ..Default::default() }
    }

    fn write_indent(&mut self) {
        if self.at_line_start {
            for _ in 0..self.indent {
                self.buf.push_str("    ");
            }
            self.col = (self.indent * 4) as u32;
            self.at_line_start = false;
        }
    }

    /// Writes text. Embedded newlines are followed by the current indentation.
    pub fn w(&mut self, s: &str) {
        for (i, part) in s.split('\n').enumerate() {
            if i > 0 {
                self.buf.push('\n');
                self.line += 1;
                self.col = 0;
                self.at_line_start = true;
            }
            if !part.is_empty() {
                self.write_indent();
                self.buf.push_str(part);
                self.col += part.chars().count() as u32;
            }
        }
    }

    pub fn nl(&mut self) {
        self.buf.push('\n');
        self.line += 1;
        self.col = 0;
        self.at_line_start = true;
    }

    /// Ensures we're at the start of a fresh line.
    pub fn line_break(&mut self) {
        if !self.at_line_start {
            self.nl();
        }
    }

    pub fn indent(&mut self) {
        self.indent += 1;
    }
    pub fn dedent(&mut self) {
        self.indent = self.indent.saturating_sub(1);
    }

    pub fn mark(&mut self, span: Span) {
        if span == Span::default() {
            return;
        }
        let col = if self.at_line_start { (self.indent * 4) as u32 } else { self.col };
        if let Some(last) = self.marks.last() {
            if last.gen_line == self.line && last.gen_col == col {
                self.marks.pop();
            }
        }
        self.marks.push(Mark { gen_line: self.line, gen_col: col, src: span.start });
    }

    pub fn line(&self) -> u32 {
        self.line
    }

    /// Where the next text goes, on a line already started.
    pub fn pos(&self) -> Pos {
        Pos { offset: self.buf.len(), line: self.line, col: self.col }
    }

    /// Inserts `s` (without newlines) at `at`, keeping the marks and decisions after it in place.
    pub fn insert(&mut self, at: Pos, s: &str) {
        self.buf.insert_str(at.offset, s);
        let n = s.chars().count() as u32;
        for m in self.marks.iter_mut().filter(|m| m.gen_line == at.line && m.gen_col >= at.col) {
            m.gen_col += n;
        }
        for d in self.decisions.iter_mut().filter(|d| d.gen_line == at.line && d.gen_col >= at.col) {
            d.gen_col += n;
        }
        if self.line == at.line {
            self.col += n;
        }
    }

    /// Inserts whole lines before line `line` (zero-based), keeping the marks and decisions
    /// after them in place.
    pub fn insert_lines(&mut self, line: u32, lines: &[String]) {
        if lines.is_empty() {
            return;
        }
        let offset = match line {
            0 => 0,
            n => self.buf.match_indices('\n').nth(n as usize - 1).map_or(self.buf.len(), |(i, _)| i + 1),
        };
        let text: String = lines.iter().map(|l| format!("{}\n", l)).collect();
        self.buf.insert_str(offset, &text);
        let n = lines.len() as u32;
        for m in self.marks.iter_mut().filter(|m| m.gen_line >= line) {
            m.gen_line += n;
        }
        for d in self.decisions.iter_mut().filter(|d| d.gen_line >= line) {
            d.gen_line += n;
        }
        self.line += n;
    }

    /// Records a decision whose result was just written.
    pub fn decide(&mut self, span: Span, chosen: impl Into<String>, why: impl Into<String>, cause: Option<Span>) {
        let col = if self.at_line_start { (self.indent * 4) as u32 } else { self.col };
        self.decisions.push(Decision { gen_line: self.line, gen_col: col, span, chosen: chosen.into(), why: why.into(), cause });
    }

    /// Inserts `prefix` lines before every top-level paragraph (an unindented line after a blank
    /// line), keeping source-map marks in place. Used to apply crate-level lint attributes.
    pub fn prefix_paragraphs(&mut self, prefix: &[String]) {
        let lines: Vec<String> = self.buf.split('\n').map(|l| l.to_string()).collect();
        let mut out = String::new();
        let mut shift = vec![0u32; lines.len() + 1];
        let mut count = 0u32;
        for (i, l) in lines.iter().enumerate() {
            let starts_item = !l.is_empty()
                && !l.starts_with(char::is_whitespace)
                && !l.starts_with(['}', ')', ']'])
                && !l.starts_with("//")
                && (i == 0 || lines[i - 1].trim().is_empty());
            if starts_item {
                for p in prefix {
                    out.push_str(p);
                    out.push('\n');
                    count += 1;
                }
            }
            shift[i] = count;
            out.push_str(l);
            if i + 1 < lines.len() {
                out.push('\n');
            }
        }
        self.buf = out;
        for m in &mut self.marks {
            m.gen_line += shift.get(m.gen_line as usize).copied().unwrap_or(count);
        }
        for d in &mut self.decisions {
            d.gen_line += shift.get(d.gen_line as usize).copied().unwrap_or(count);
        }
        self.line += count;
    }

    /// Writes raw multi-line text, re-indenting each line.
    pub fn raw_lines(&mut self, text: &str) {
        for (i, l) in text.lines().enumerate() {
            if i > 0 {
                self.nl();
            }
            self.w(l);
        }
    }
}
