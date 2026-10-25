//! Tokenizer. Produces tokens with byte spans plus significant `Newline` tokens.
//!
//! Newlines terminate statements, except:
//! - inside `(...)` and `[...]`,
//! - after a token that cannot end an expression (binary operators, `,`, `.`, `->`, ...),
//! - before a line that starts with `.`, `?.`, `?:`, `&&` or `||` (method chains).

use crate::diag::{Diag, Span};

#[derive(Clone, Debug, PartialEq)]
pub enum StrPart {
    /// Literal text, already escaped for a Rust string literal (without format-brace escaping).
    Lit(String),
    /// `${expr}` or `$name`; `offset` is the byte offset of `src` in the file. A `$` followed by
    /// digits keeps its `$` in `src` (`$5`).
    Interp { src: String, offset: usize, spec: Option<String> },
}

/// `$5` as text, as in Kotlin (`"cost $5"`); only error messages read `$0` as a field.
pub fn dollar_digits_as_text(parts: Vec<StrPart>) -> Vec<StrPart> {
    let mut out: Vec<StrPart> = Vec::new();
    for p in parts {
        let text = match p {
            StrPart::Interp { src, .. } if src.starts_with('$') => src,
            StrPart::Lit(s) => s,
            other => {
                out.push(other);
                continue;
            }
        };
        match out.last_mut() {
            Some(StrPart::Lit(prev)) => prev.push_str(&text),
            _ => out.push(StrPart::Lit(text)),
        }
    }
    out
}

#[derive(Clone, Debug, PartialEq)]
pub enum Tok {
    Ident(String),
    /// `r#name`
    RawIdent(String),
    /// `'a` (stored without the quote)
    Lifetime(String),
    Int(String),
    Float(String),
    /// Char or byte literal, raw source text.
    Char(String),
    Str(Vec<StrPart>),
    /// Raw string / byte string / C string literal, raw source text (no interpolation).
    RawStr(String),
    Punct(&'static str),
    DocOuter(String),
    DocInner(String),
    Newline,
    Eof,
}

#[derive(Clone, Debug)]
pub struct Token {
    pub tok: Tok,
    pub span: Span,
}

const PUNCTS: &[&str] = &[
    "...", "..=", "..<", "<<=", "::", "->", "=>", "==", "!=", "<=", ">=", "&&", "||", "+=", "-=", "*=", "/=",
    "%=", "^=", "&=", "|=", "<<", "..", "?.", "?:", "!!", "+", "-", "*", "/", "%", "^", "!", "&", "|", "=",
    "<", ">", "@", ".", ",", ";", ":", "#", "$", "?", "~", "(", ")", "[", "]", "{", "}",
];

/// Tokens after which a newline does not end the statement.
fn continues_after(t: &Tok) -> bool {
    match t {
        Tok::Punct(p) => matches!(
            *p,
            "+" | "-" | "*" | "/" | "%" | "=" | "+=" | "-=" | "*=" | "/=" | "%=" | "&=" | "|=" | "^=" | "<<="
                | "==" | "!=" | "<=" | ">=" | "&&" | "||" | "->" | "=>" | "." | "?." | "?:" | "::" | ","
                | "(" | "[" | "{" | ":" | "<" | "&" | "^" | "<<" | "@" | "#"
        ),
        Tok::DocOuter(_) | Tok::DocInner(_) => false,
        _ => false,
    }
}

/// Tokens that, at the start of a line, continue the previous line.
fn continues_before(t: &Tok) -> bool {
    matches!(t, Tok::Punct(".") | Tok::Punct("?.") | Tok::Punct("?:") | Tok::Punct("&&") | Tok::Punct("||"))
}

pub fn is_ident_start(c: char) -> bool {
    c == '_' || c.is_alphabetic()
}

pub fn is_ident_continue(c: char) -> bool {
    c == '_' || c.is_alphanumeric()
}

pub struct Lexer<'a> {
    src: &'a str,
    pos: usize,
    base: usize,
    tokens: Vec<Token>,
    diags: Vec<Diag>,
    brackets: Vec<char>,
    pending_newline: bool,
}

pub fn lex(src: &str) -> (Vec<Token>, Vec<Diag>) {
    lex_at(src, 0)
}

/// Lexes `src` as if it started at byte `base` of the file (used for interpolations).
pub fn lex_at(src: &str, base: usize) -> (Vec<Token>, Vec<Diag>) {
    let mut lx = Lexer { src, pos: 0, base, tokens: Vec::new(), diags: Vec::new(), brackets: Vec::new(), pending_newline: false };
    lx.run();
    (lx.tokens, lx.diags)
}

impl<'a> Lexer<'a> {
    fn peek(&self) -> Option<char> {
        self.src[self.pos..].chars().next()
    }
    fn peek_at(&self, n: usize) -> Option<char> {
        self.src[self.pos..].chars().nth(n)
    }
    fn rest(&self) -> &'a str {
        &self.src[self.pos..]
    }
    fn span(&self, start: usize) -> Span {
        Span::new(self.base + start, self.base + self.pos)
    }
    fn error(&mut self, start: usize, msg: &str) {
        let sp = self.span(start);
        self.diags.push(Diag::error(sp, msg));
    }

    fn push(&mut self, tok: Tok, start: usize) {
        if self.pending_newline {
            self.pending_newline = false;
            let n = self.tokens.len();
            // `import a::*` ends the line: a glob `*` is not a multiplication
            let glob = n >= 2 && matches!(self.tokens[n - 1].tok, Tok::Punct("*")) && matches!(self.tokens[n - 2].tok, Tok::Punct("::"));
            let prev_continues = !glob && self.tokens.last().map(|t| continues_after(&t.tok)).unwrap_or(true);
            let prev_is_newline = matches!(self.tokens.last().map(|t| &t.tok), Some(Tok::Newline) | None);
            if !prev_continues && !prev_is_newline && !continues_before(&tok) {
                let at = self.base + start;
                self.tokens.push(Token { tok: Tok::Newline, span: Span::new(at, at) });
            }
        }
        let span = self.span(start);
        self.tokens.push(Token { tok, span });
    }

    fn newline_significant(&self) -> bool {
        !matches!(self.brackets.last(), Some('(') | Some('['))
    }

    fn run(&mut self) {
        while let Some(c) = self.peek() {
            let start = self.pos;
            match c {
                '\n' => {
                    self.pos += 1;
                    if self.newline_significant() {
                        self.pending_newline = true;
                    }
                }
                c if c.is_whitespace() => self.pos += c.len_utf8(),
                '/' if self.rest().starts_with("///") && !self.rest().starts_with("////") => {
                    let text = self.line_comment(3);
                    self.push(Tok::DocOuter(text), start);
                }
                '/' if self.rest().starts_with("//!") => {
                    let text = self.line_comment(3);
                    self.push(Tok::DocInner(text), start);
                }
                '/' if self.rest().starts_with("//") => {
                    self.line_comment(2);
                }
                '/' if self.rest().starts_with("/*") => self.block_comment(),
                '"' => {
                    if self.rest().starts_with("\"\"\"") {
                        self.triple_string();
                    } else {
                        self.string();
                    }
                }
                '\'' => self.quote(),
                'r' if self.rest().starts_with("r#") && self.peek_at(2).is_some_and(is_ident_start) => {
                    self.pos += 2;
                    let name = self.ident_text();
                    self.push(Tok::RawIdent(name), start);
                }
                'r' | 'b' | 'c' if self.raw_or_byte_string() => {}
                c if is_ident_start(c) => {
                    let name = self.ident_text();
                    self.push(Tok::Ident(name), start);
                }
                c if c.is_ascii_digit() => self.number(),
                _ => {
                    let rest = self.rest();
                    // `a?..b` is `a?` and a range, not `?.` followed by `.`
                    let p = if rest.starts_with("?..") { Some(&"?") } else { PUNCTS.iter().find(|p| rest.starts_with(**p)) };
                    if let Some(p) = p {
                        self.pos += p.len();
                        match *p {
                            "(" => self.brackets.push('('),
                            "[" => self.brackets.push('['),
                            "{" => self.brackets.push('{'),
                            ")" | "]" | "}" => {
                                self.brackets.pop();
                            }
                            _ => {}
                        }
                        self.push(Tok::Punct(p), start);
                    } else {
                        self.pos += c.len_utf8();
                        self.error(start, &format!("unexpected character `{}`", c));
                    }
                }
            }
        }
        let end = self.base + self.src.len();
        self.tokens.push(Token { tok: Tok::Newline, span: Span::new(end, end) });
        self.tokens.push(Token { tok: Tok::Eof, span: Span::new(end, end) });
    }

    fn line_comment(&mut self, skip: usize) -> String {
        let rest = self.rest();
        let len = rest.find('\n').unwrap_or(rest.len());
        let text = rest[skip..len].to_string();
        self.pos += len;
        text
    }

    fn block_comment(&mut self) {
        let start = self.pos;
        self.pos += 2;
        let mut depth = 1;
        while depth > 0 {
            if self.rest().is_empty() {
                self.error(start, "unterminated block comment");
                return;
            }
            if self.rest().starts_with("/*") {
                depth += 1;
                self.pos += 2;
            } else if self.rest().starts_with("*/") {
                depth -= 1;
                self.pos += 2;
            } else {
                let c = self.peek().unwrap();
                if c == '\n' && self.newline_significant() {
                    self.pending_newline = true;
                }
                self.pos += c.len_utf8();
            }
        }
    }

    fn ident_text(&mut self) -> String {
        let start = self.pos;
        while let Some(c) = self.peek() {
            if is_ident_continue(c) {
                self.pos += c.len_utf8();
            } else {
                break;
            }
        }
        self.src[start..self.pos].to_string()
    }

    fn number(&mut self) {
        let start = self.pos;
        let after_dot = matches!(self.tokens.last().map(|t| &t.tok), Some(Tok::Punct(".")) | Some(Tok::Punct("?.")));
        let mut is_float = false;
        if self.rest().starts_with("0x") || self.rest().starts_with("0o") || self.rest().starts_with("0b") {
            self.pos += 2;
            while let Some(c) = self.peek() {
                if c.is_ascii_hexdigit() || c == '_' {
                    self.pos += 1;
                } else {
                    break;
                }
            }
        } else {
            self.digits();
            if !after_dot && self.peek() == Some('.') && self.peek_at(1).is_some_and(|c| c.is_ascii_digit()) {
                is_float = true;
                self.pos += 1;
                self.digits();
            } else if !after_dot && self.peek() == Some('.') && !self.peek_at(1).is_some_and(|c| c == '.' || is_ident_start(c)) {
                // Rust's `1.` (as in `px(8.)`): a float unless `..`, a field or a method follows
                is_float = true;
                self.pos += 1;
            }
            if !after_dot && matches!(self.peek(), Some('e') | Some('E')) {
                let save = self.pos;
                self.pos += 1;
                if matches!(self.peek(), Some('+') | Some('-')) {
                    self.pos += 1;
                }
                if self.peek().is_some_and(|c| c.is_ascii_digit()) {
                    is_float = true;
                    self.digits();
                } else {
                    self.pos = save;
                }
            }
        }
        // suffix
        let suffix_start = self.pos;
        if !after_dot {
            if let Some(c) = self.peek() {
                if is_ident_start(c) {
                    self.ident_text();
                }
            }
        }
        let mut text = self.src[start..self.pos].to_string();
        if text.ends_with('.') {
            // `1.0` stays a number when the generated Rust calls a method on it
            text.push('0');
        }
        let suffix = &self.src[suffix_start..self.pos];
        if suffix == "L" {
            text = format!("{}i64", &self.src[start..suffix_start]);
        } else if suffix.starts_with('f') && (suffix == "f32" || suffix == "f64") {
            is_float = true;
        }
        self.push(if is_float { Tok::Float(text) } else { Tok::Int(text) }, start);
    }

    fn digits(&mut self) {
        while let Some(c) = self.peek() {
            if c.is_ascii_digit() || c == '_' {
                self.pos += 1;
            } else {
                break;
            }
        }
    }

    /// `'a'`, `'\n'`, or a lifetime/label `'a`.
    fn quote(&mut self) {
        let start = self.pos;
        let c1 = self.peek_at(1);
        let c2 = self.peek_at(2);
        if c1 == Some('\\') || (c1.is_some() && c2 == Some('\'')) {
            self.pos += 1;
            self.char_body(start);
            let text = self.src[start..self.pos].to_string();
            self.push(Tok::Char(text), start);
        } else if c1.is_some_and(is_ident_start) {
            self.pos += 1;
            let name = self.ident_text();
            self.push(Tok::Lifetime(name), start);
        } else {
            self.pos += 1;
            self.error(start, "unexpected `'`");
        }
    }

    fn char_body(&mut self, start: usize) {
        loop {
            match self.peek() {
                None | Some('\n') => {
                    self.error(start, "unterminated character literal");
                    return;
                }
                Some('\\') => {
                    self.pos += 1;
                    if let Some(c) = self.peek() {
                        self.pos += c.len_utf8();
                    }
                }
                Some('\'') => {
                    self.pos += 1;
                    return;
                }
                Some(c) => self.pos += c.len_utf8(),
            }
        }
    }

    /// Handles `r"..."`, `r#"..."#`, `b"..."`, `br"..."`, `b'x'`, `c"..."`. Returns false if not one.
    fn raw_or_byte_string(&mut self) -> bool {
        let rest = self.rest();
        let start = self.pos;
        let (prefix_len, raw) = if rest.starts_with("br") || rest.starts_with("cr") {
            (2, true)
        } else if rest.starts_with('r') {
            (1, true)
        } else if rest.starts_with('b') || rest.starts_with('c') {
            (1, false)
        } else {
            return false;
        };
        let after = &rest[prefix_len..];
        if raw {
            let hashes = after.chars().take_while(|&c| c == '#').count();
            if !after[hashes..].starts_with('"') {
                return false;
            }
            let closing = format!("\"{}", "#".repeat(hashes));
            let body_start = prefix_len + hashes + 1;
            match rest[body_start..].find(&closing) {
                Some(i) => {
                    self.pos += body_start + i + closing.len();
                    self.count_newlines_in(start);
                    let text = self.src[start..self.pos].to_string();
                    self.push(Tok::RawStr(text), start);
                }
                None => {
                    self.pos = self.src.len();
                    self.error(start, "unterminated raw string");
                }
            }
            true
        } else if after.starts_with('"') {
            self.pos += prefix_len + 1;
            loop {
                match self.peek() {
                    None => {
                        self.error(start, "unterminated string");
                        break;
                    }
                    Some('\\') => {
                        self.pos += 1;
                        if let Some(c) = self.peek() {
                            self.pos += c.len_utf8();
                        }
                    }
                    Some('"') => {
                        self.pos += 1;
                        break;
                    }
                    Some(c) => self.pos += c.len_utf8(),
                }
            }
            let text = self.src[start..self.pos].to_string();
            self.push(Tok::RawStr(text), start);
            true
        } else if rest.starts_with("b'") {
            self.pos += 2;
            self.char_body(start);
            let text = self.src[start..self.pos].to_string();
            self.push(Tok::Char(text), start);
            true
        } else {
            false
        }
    }

    fn count_newlines_in(&mut self, _start: usize) {}

    /// Normal `"..."` string with escapes and `$name` / `${expr}` interpolation.
    fn string(&mut self) {
        let start = self.pos;
        self.pos += 1;
        let mut parts = Vec::new();
        let mut lit = String::new();
        loop {
            match self.peek() {
                None => {
                    self.error(start, "unterminated string literal");
                    break;
                }
                Some('"') => {
                    self.pos += 1;
                    break;
                }
                Some('\\') => {
                    self.pos += 1;
                    match self.peek() {
                        Some('$') => {
                            self.pos += 1;
                            lit.push('$');
                        }
                        Some('u') if self.peek_at(1) != Some('{') => {
                            // Kotlin/Java style \uXXXX
                            self.pos += 1;
                            let hex: String = self.rest().chars().take(4).take_while(|c| c.is_ascii_hexdigit()).collect();
                            self.pos += hex.len();
                            lit.push_str(&format!("\\u{{{}}}", hex));
                        }
                        Some('\n') => {
                            // line continuation
                            self.pos += 1;
                            lit.push_str("\\\n");
                        }
                        Some('\r') if self.peek_at(1) == Some('\n') => {
                            self.pos += 2;
                            lit.push_str("\\\n");
                        }
                        Some('b') => {
                            // Kotlin's backspace
                            self.pos += 1;
                            lit.push_str("\\u{8}");
                        }
                        Some(c) if !matches!(c, 'n' | 'r' | 't' | '\\' | '0' | '\'' | '"' | 'x' | 'u') => {
                            self.pos += c.len_utf8();
                            let at = self.pos - c.len_utf8() - 1;
                            self.error(at, &format!("unknown escape `\\{}` (escapes are \\n, \\r, \\t, \\\\, \\0, \\', \\\", \\$, \\b, \\x7F and \\u{{..}})", c));
                        }
                        Some(c) => {
                            self.pos += c.len_utf8();
                            lit.push('\\');
                            lit.push(c);
                        }
                        None => {}
                    }
                }
                Some('$') => {
                    if !self.interpolation(&mut parts, &mut lit) {
                        self.pos += 1;
                        lit.push('$');
                    }
                }
                Some('\n') => {
                    self.pos += 1;
                    lit.push_str("\\n");
                }
                // a Windows line ending is a `\n`, as in Rust
                Some('\r') if self.peek_at(1) == Some('\n') => self.pos += 1,
                Some('\t') => {
                    self.pos += 1;
                    lit.push_str("\\t");
                }
                Some(c) => {
                    self.pos += c.len_utf8();
                    lit.push(c);
                }
            }
        }
        if !lit.is_empty() || parts.is_empty() {
            parts.push(StrPart::Lit(lit));
        }
        self.push(Tok::Str(parts), start);
    }

    /// Parses `$name` or `${...}` at the current position. Returns false if `$` is literal.
    fn interpolation(&mut self, parts: &mut Vec<StrPart>, lit: &mut String) -> bool {
        let next = self.peek_at(1);
        if next == Some('{') {
            let open = self.pos + 2;
            let Some(i) = interp_end(self.src, open) else {
                let p = self.pos;
                self.pos = self.src.len();
                self.error(p, "unterminated `${` in string");
                return true;
            };
            let inner = &self.src[open..i];
            let (expr, spec) = split_format_spec(inner);
            if !lit.is_empty() {
                parts.push(StrPart::Lit(std::mem::take(lit)));
            }
            parts.push(StrPart::Interp { src: expr.to_string(), offset: self.base + open, spec });
            self.pos = i + 1;
            true
        } else if next.is_some_and(|c| is_ident_start(c) || c.is_ascii_digit()) {
            let s = self.pos + 1;
            let mut e = s;
            if next.unwrap().is_ascii_digit() {
                while e < self.src.len() && self.src.as_bytes()[e].is_ascii_digit() {
                    e += 1;
                }
            } else {
                for (i, c) in self.src[s..].char_indices() {
                    if !is_ident_continue(c) {
                        e = s + i;
                        break;
                    }
                    e = s + i + c.len_utf8();
                }
            }
            if !lit.is_empty() {
                parts.push(StrPart::Lit(std::mem::take(lit)));
            }
            // `$5` keeps its `$`: it is text, except in error messages (see `dollar_digits_as_text`)
            let (src, offset) = if next.unwrap().is_ascii_digit() { (self.src[s - 1..e].to_string(), self.base + s - 1) } else { (self.src[s..e].to_string(), self.base + s) };
            parts.push(StrPart::Interp { src, offset, spec: None });
            self.pos = e;
            true
        } else {
            false
        }
    }

    /// `"""` multi-line raw string with interpolation and automatic indentation trimming.
    fn triple_string(&mut self) {
        let start = self.pos;
        self.pos += 3;
        let body_start = self.pos;
        let Some(len) = self.rest().find("\"\"\"") else {
            self.pos = self.src.len();
            self.error(start, "unterminated `\"\"\"` string");
            return;
        };
        let body = &self.src[body_start..body_start + len];
        // indentation of non-blank lines (excluding the first line if it directly follows the quotes)
        let lines: Vec<&str> = body.split('\n').collect();
        let min_indent = lines
            .iter()
            .enumerate()
            .filter(|(i, l)| (*i > 0 || lines.len() == 1) && !l.trim().is_empty())
            .map(|(_, l)| l.chars().take_while(|c| *c == ' ' || *c == '\t').count())
            .min()
            .unwrap_or(0);
        let first_blank = lines.len() > 1 && lines[0].trim().is_empty();
        let last_blank = lines.len() > 1 && lines[lines.len() - 1].trim().is_empty();

        let mut parts = Vec::new();
        let mut lit = String::new();
        let mut line_no = 0;
        let mut at_line_start = true;
        let mut skipped_indent = 0;
        let total_lines = lines.len();
        self.pos = body_start;
        let end = body_start + len;
        while self.pos < end {
            let c = self.peek().unwrap();
            let in_dropped_line = (first_blank && line_no == 0) || (last_blank && line_no == total_lines - 1);
            if c == '\n' {
                self.pos += 1;
                if !(first_blank && line_no == 0) && !(last_blank && line_no == total_lines - 2) {
                    lit.push_str("\\n");
                }
                line_no += 1;
                at_line_start = true;
                skipped_indent = 0;
                continue;
            }
            if in_dropped_line {
                self.pos += c.len_utf8();
                continue;
            }
            if at_line_start && line_no > 0 && (c == ' ' || c == '\t') && skipped_indent < min_indent {
                self.pos += 1;
                skipped_indent += 1;
                continue;
            }
            at_line_start = false;
            match c {
                '$' => {
                    if !self.interpolation(&mut parts, &mut lit) {
                        self.pos += 1;
                        lit.push('$');
                    }
                }
                // the text is raw, except `\$` for a dollar sign before a name (`\$HOME`)
                '\\' if self.peek_at(1) == Some('$') => {
                    self.pos += 2;
                    lit.push('$');
                }
                '\\' => {
                    self.pos += 1;
                    lit.push_str("\\\\");
                }
                '"' => {
                    self.pos += 1;
                    lit.push_str("\\\"");
                }
                '\t' => {
                    self.pos += 1;
                    lit.push_str("\\t");
                }
                '\r' => {
                    self.pos += 1;
                }
                c => {
                    self.pos += c.len_utf8();
                    lit.push(c);
                }
            }
        }
        self.pos = end + 3;
        if !lit.is_empty() || parts.is_empty() {
            parts.push(StrPart::Lit(lit));
        }
        self.push(Tok::Str(parts), start);
    }
}

/// If a string or char literal starts at byte `i` of `s`, the index just past it (the end of `s`
/// when a string is unterminated). `None` for anything else, including a lifetime or label `'a`.
pub(crate) fn skip_literal(s: &str, i: usize) -> Option<usize> {
    let b = s.as_bytes();
    match *b.get(i)? {
        b'"' if s[i..].starts_with("\"\"\"") => Some(s[i + 3..].find("\"\"\"").map_or(s.len(), |e| i + 3 + e + 3)),
        b'"' => {
            let mut j = i + 1;
            while j < b.len() {
                match b[j] {
                    b'\\' => j += 2,
                    b'"' => return Some(j + 1),
                    b'$' if b.get(j + 1) == Some(&b'{') => match interp_end(s, j + 2) {
                        Some(e) => j = e + 1,
                        None => return Some(s.len()),
                    },
                    _ => j += 1,
                }
            }
            Some(s.len())
        }
        b'\'' => {
            let rest = &s[i + 1..];
            let mut cs = rest.chars();
            match cs.next()? {
                // `'\n'`, `'\''`, `'\u{..}'`: the escaped character, then up to the closing quote
                '\\' => {
                    let k = 1 + cs.next()?.len_utf8();
                    let end = rest[k..].find(['\'', '\n'])?;
                    rest[k + end..].starts_with('\'').then_some(i + 1 + k + end + 1)
                }
                c if cs.next() == Some('\'') => Some(i + 1 + c.len_utf8() + 1),
                _ => None,
            }
        }
        _ => None,
    }
}

/// The index of the `}` that closes a `${` whose contents start at byte `open` of `s`, skipping
/// braces inside string and char literals.
pub(crate) fn interp_end(s: &str, open: usize) -> Option<usize> {
    let b = s.as_bytes();
    let mut depth = 0;
    let mut i = open;
    while i < b.len() {
        if let Some(e) = skip_literal(s, i) {
            i = e;
            continue;
        }
        match b[i] {
            b'{' => depth += 1,
            b'}' if depth == 0 => return Some(i),
            b'}' => depth -= 1,
            _ => {}
        }
        i += 1;
    }
    None
}

/// Splits `expr:spec` at the last top-level single `:` if the remainder looks like a format spec.
/// The `:` of `?:` (Elvis) and `::` (paths) never splits.
fn split_format_spec(inner: &str) -> (&str, Option<String>) {
    let bytes = inner.as_bytes();
    let mut depth = 0i32;
    let mut cand = None;
    let mut i = 0;
    while i < bytes.len() {
        if let Some(e) = skip_literal(inner, i) {
            i = e;
            continue;
        }
        match bytes[i] {
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' => depth -= 1,
            b':' if depth == 0 => {
                let prev = if i > 0 { bytes[i - 1] } else { 0 };
                let next_colon = i + 1 < bytes.len() && bytes[i + 1] == b':';
                if prev != b':' && prev != b'?' && !next_colon {
                    cand = Some(i);
                }
            }
            _ => {}
        }
        i += 1;
    }
    if let Some(i) = cand {
        let spec = &inner[i + 1..];
        let ok = !spec.is_empty()
            && spec.chars().all(|c| c.is_ascii_alphanumeric() || "?<>^+-#.$_ ".contains(c))
            && !spec.trim().is_empty()
            && !spec.starts_with(' ');
        if ok {
            return (&inner[..i], Some(spec.to_string()));
        }
    }
    (inner, None)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(src: &str) -> Vec<Tok> {
        lex(src).0.into_iter().map(|t| t.tok).collect()
    }

    #[test]
    fn newlines_and_continuations() {
        let t = kinds("val x = 1 +\n 2\nfoo()\n  .bar()\n");
        let nl = t.iter().filter(|t| **t == Tok::Newline).count();
        assert_eq!(nl, 2, "{:?}", t);
    }

    #[test]
    fn no_newlines_in_parens() {
        let t = kinds("foo(\n1,\n2\n)");
        assert_eq!(t.iter().filter(|t| **t == Tok::Newline).count(), 1);
    }

    #[test]
    fn interpolation() {
        let t = kinds("\"a $b ${c + 1:.2} \\$d\"");
        match &t[0] {
            Tok::Str(parts) => {
                assert_eq!(parts.len(), 5, "{:?}", parts);
                assert!(matches!(&parts[3], StrPart::Interp { spec: Some(s), .. } if s == ".2"));
            }
            _ => panic!(),
        }
    }

    #[test]
    fn literals_inside_interpolations() {
        let interps = |src: &str| match &kinds(src)[0] {
            Tok::Str(parts) => parts.iter().filter_map(|p| if let StrPart::Interp { src, spec, .. } = p { Some((src.clone(), spec.clone())) } else { None }).collect::<Vec<_>>(),
            t => panic!("{:?}", t),
        };
        assert_eq!(interps(r#""${s.trim_matches('{')}""#), [("s.trim_matches('{')".to_string(), None)]);
        assert_eq!(interps(r#""${s.replace('"', "x")}""#), [(r#"s.replace('"', "x")"#.to_string(), None)]);
        assert_eq!(interps(r#""${ if (c == 'x') "yes{" else "no}" }""#), [(r#" if (c == 'x') "yes{" else "no}" "#.to_string(), None)]);
        assert_eq!(interps(r#""${'\''} ${"a ${b} }"}""#), [(r"'\''".to_string(), None), (r#""a ${b} }""#.to_string(), None)]);
        // `?:` and `::` are not format specs
        assert_eq!(interps(r#""${x?:0} ${a::B:>4}""#), [("x?:0".to_string(), None), ("a::B".to_string(), Some(">4".to_string()))]);
    }

    #[test]
    fn windows_line_endings_in_strings() {
        assert_eq!(kinds("\"a\r\nb\"")[0], Tok::Str(vec![StrPart::Lit("a\\nb".into())]));
        assert_eq!(kinds("\"a \\\r\n  b\"")[0], Tok::Str(vec![StrPart::Lit("a \\\n  b".into())]));
    }

    #[test]
    fn escapes() {
        assert_eq!(kinds("\"a\\bc\"")[0], Tok::Str(vec![StrPart::Lit("a\\u{8}c".into())]));
        let (_, diags) = lex("val s = \"ab\\qc\"");
        assert_eq!((diags.len(), diags[0].span.start, diags[0].span.end), (1, 11, 13));
    }

    #[test]
    fn lifetimes_and_chars() {
        let t = kinds("'a 'b' '\\n' b'x'");
        assert_eq!(t[0], Tok::Lifetime("a".into()));
        assert_eq!(t[1], Tok::Char("'b'".into()));
        assert_eq!(t[2], Tok::Char("'\\n'".into()));
        assert_eq!(t[3], Tok::Char("b'x'".into()));
    }

    #[test]
    fn tuple_index() {
        let t = kinds("x.0.1");
        assert_eq!(t[2], Tok::Int("0".into()));
        assert_eq!(t[4], Tok::Int("1".into()));
    }

    #[test]
    fn float_with_trailing_dot() {
        let t = kinds("px(8.) f(1., 2.)");
        assert_eq!(t[2], Tok::Float("8.0".into()));
        assert_eq!(t[6], Tok::Float("1.0".into()));
        assert_eq!(t[8], Tok::Float("2.0".into()));
        // still a range, a method call and a field
        assert_eq!(kinds("1..2")[..3], [Tok::Int("1".into()), Tok::Punct(".."), Tok::Int("2".into())]);
        assert_eq!(kinds("1.max(2)")[..3], [Tok::Int("1".into()), Tok::Punct("."), Tok::Ident("max".into())]);
        assert_eq!(kinds("x.0.")[..4], [Tok::Ident("x".into()), Tok::Punct("."), Tok::Int("0".into()), Tok::Punct(".")]);
    }

    #[test]
    fn question_mark_before_range() {
        let t = kinds("a?..b?");
        assert_eq!(t[..5], [Tok::Ident("a".into()), Tok::Punct("?"), Tok::Punct(".."), Tok::Ident("b".into()), Tok::Punct("?")]);
        assert_eq!(kinds("a?.b")[1], Tok::Punct("?."));
    }

    #[test]
    fn triple_quoted() {
        let t = kinds("\"\"\"\n    hello\n      world\n    \"\"\"");
        assert_eq!(t[0], Tok::Str(vec![StrPart::Lit("hello\\n  world".into())]));
    }

    #[test]
    fn dollar_before_digits_is_text() {
        let text = |src: &str| match &kinds(src)[0] {
            Tok::Str(parts) => dollar_digits_as_text(parts.clone()),
            t => panic!("{:?}", t),
        };
        assert_eq!(text("\"cost $5, only $9.99\""), vec![StrPart::Lit("cost $5, only $9.99".into())]);
        assert_eq!(text("\"\"\"\n    \\$5 \\$HOME $\n    \"\"\""), vec![StrPart::Lit("$5 $HOME $".into())]);
        // `$0` in an error message is a field: the lexer keeps it apart
        assert!(matches!(&text("\"a $n $0\"")[1], StrPart::Interp { src, .. } if src == "n"));
        match &kinds("\"x $0\"")[0] {
            Tok::Str(parts) => assert!(matches!(&parts[1], StrPart::Interp { src, .. } if src == "$0")),
            t => panic!("{:?}", t),
        }
    }
}
