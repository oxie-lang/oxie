//! Source formatter for .oxi files.
//!
//! Only whitespace is changed: indentation follows bracket depth (4 spaces), continuation
//! lines (`.method()`, `?:`, `&&`, ...) get one extra level, spacing around commas, colons and
//! unambiguous operators is normalized, trailing whitespace and repeated blank lines are removed.
//! The spaces before a trailing `//` comment are kept, so aligned comments stay aligned.
//! Strings and comments are never touched. If the result does not lex to the same tokens as the
//! input, the input is returned unchanged.

use crate::lexer::{self, Tok};

const INDENT: &str = "    ";

#[derive(Clone, Copy, PartialEq, Debug)]
enum Seg {
    Code,
    Verbatim,
}

struct Line {
    /// Line begins inside a multi-line string or comment: keep it exactly.
    raw: bool,
    segs: Vec<(Seg, String)>,
}

/// Splits source into lines of code and verbatim (string/comment) segments.
fn split(src: &str) -> Vec<Line> {
    let chars: Vec<char> = src.chars().collect();
    // byte offset of each char (and of the end)
    let byte_of: Vec<usize> = src.char_indices().map(|(b, _)| b).chain([src.len()]).collect();
    let mut lines = Vec::new();
    let mut cur = Line { raw: false, segs: Vec::new() };
    let mut buf = String::new();
    let mut kind = Seg::Code;
    let mut i = 0;
    // state for verbatim regions
    #[derive(PartialEq)]
    enum St {
        Code,
        /// `until`: inside `${ }`, the char index of its closing `}`
        Str { until: Option<usize> },
        Triple,
        Raw { hashes: usize },
        Char,
        LineComment,
        Block { depth: usize },
    }
    let mut st = St::Code;
    let push = |cur: &mut Line, buf: &mut String, kind: Seg| {
        if !buf.is_empty() {
            cur.segs.push((kind, std::mem::take(buf)));
        }
    };
    while i < chars.len() {
        let c = chars[i];
        if c == '\n' {
            push(&mut cur, &mut buf, kind);
            lines.push(cur);
            let raw = matches!(st, St::Triple | St::Raw { .. } | St::Block { .. } | St::Str { .. });
            cur = Line { raw, segs: Vec::new() };
            if st == St::LineComment {
                st = St::Code;
                kind = Seg::Code;
            }
            i += 1;
            continue;
        }
        match &mut st {
            St::Code => {
                let rest: String = chars[i..chars.len().min(i + 4)].iter().collect();
                let start_verbatim = |st_new: St, len: usize, cur: &mut Line, buf: &mut String, kind: &mut Seg, st: &mut St, i: &mut usize| {
                    push(cur, buf, *kind);
                    *kind = Seg::Verbatim;
                    for k in 0..len {
                        buf.push(chars[*i + k]);
                    }
                    *i += len;
                    *st = st_new;
                };
                if rest.starts_with("//") {
                    start_verbatim(St::LineComment, 2, &mut cur, &mut buf, &mut kind, &mut st, &mut i);
                } else if rest.starts_with("/*") {
                    start_verbatim(St::Block { depth: 1 }, 2, &mut cur, &mut buf, &mut kind, &mut st, &mut i);
                } else if rest.starts_with("\"\"\"") {
                    start_verbatim(St::Triple, 3, &mut cur, &mut buf, &mut kind, &mut st, &mut i);
                } else if c == '"' {
                    let prev = if i > 0 { chars[i - 1] } else { ' ' };
                    let prev2 = if i > 1 { chars[i - 2] } else { ' ' };
                    if (prev == 'r' || prev == '#') && !(prev2.is_alphanumeric() && prev == 'r' && prev2 != 'b') {
                        // raw string r"..." / r#"..."#: count hashes before the quote
                        let mut h = 0;
                        let mut j = i;
                        while j > 0 && chars[j - 1] == '#' {
                            h += 1;
                            j -= 1;
                        }
                        if j > 0 && chars[j - 1] == 'r' {
                            start_verbatim(St::Raw { hashes: h }, 1, &mut cur, &mut buf, &mut kind, &mut st, &mut i);
                            continue;
                        }
                    }
                    start_verbatim(St::Str { until: None }, 1, &mut cur, &mut buf, &mut kind, &mut st, &mut i);
                } else if c == '\'' {
                    // char literal vs lifetime
                    let is_char = i + 2 < chars.len() && (chars[i + 1] == '\\' || chars[i + 2] == '\'');
                    if is_char {
                        start_verbatim(St::Char, 1, &mut cur, &mut buf, &mut kind, &mut st, &mut i);
                    } else {
                        buf.push(c);
                        i += 1;
                    }
                } else {
                    buf.push(c);
                    i += 1;
                }
            }
            St::Str { until } => {
                buf.push(c);
                i += 1;
                if let Some(end) = *until {
                    // inside ${ ... }: kept verbatim up to its closing brace
                    if i > end {
                        *until = None;
                    }
                    continue;
                }
                if c == '\\' && i < chars.len() {
                    buf.push(chars[i]);
                    i += 1;
                } else if c == '$' && i < chars.len() && chars[i] == '{' {
                    buf.push('{');
                    i += 1;
                    // the closing brace, skipping braces in nested string and char literals
                    let end = lexer::interp_end(src, byte_of[i]).map(|b| byte_of.partition_point(|&x| x < b));
                    *until = Some(end.unwrap_or(chars.len()));
                } else if c == '"' {
                    push(&mut cur, &mut buf, kind);
                    kind = Seg::Code;
                    st = St::Code;
                }
            }
            St::Triple => {
                if chars[i..].starts_with(&['"', '"', '"']) {
                    buf.push_str("\"\"\"");
                    i += 3;
                    push(&mut cur, &mut buf, kind);
                    kind = Seg::Code;
                    st = St::Code;
                } else {
                    buf.push(c);
                    i += 1;
                }
            }
            St::Raw { hashes } => {
                buf.push(c);
                i += 1;
                let h = *hashes;
                if c == '"' && chars[i..].iter().take(h).filter(|x| **x == '#').count() == h {
                    for _ in 0..h {
                        buf.push('#');
                    }
                    i += h;
                    push(&mut cur, &mut buf, kind);
                    kind = Seg::Code;
                    st = St::Code;
                }
            }
            St::Char => {
                buf.push(c);
                i += 1;
                if c == '\\' && i < chars.len() {
                    buf.push(chars[i]);
                    i += 1;
                } else if c == '\'' {
                    push(&mut cur, &mut buf, kind);
                    kind = Seg::Code;
                    st = St::Code;
                }
            }
            St::LineComment => {
                buf.push(c);
                i += 1;
            }
            St::Block { depth } => {
                if chars[i..].starts_with(&['/', '*']) {
                    *depth += 1;
                    buf.push_str("/*");
                    i += 2;
                } else if chars[i..].starts_with(&['*', '/']) {
                    *depth -= 1;
                    buf.push_str("*/");
                    i += 2;
                    if *depth == 0 {
                        push(&mut cur, &mut buf, kind);
                        kind = Seg::Code;
                        st = St::Code;
                    }
                } else {
                    buf.push(c);
                    i += 1;
                }
            }
        }
    }
    push(&mut cur, &mut buf, kind);
    lines.push(cur);
    lines
}

/// Punctuation, longest first (same set as the lexer).
const PUNCTS: &[&str] = &[
    "...", "..=", "..<", "<<=", "::", "->", "=>", "==", "!=", "<=", ">=", "&&", "||", "+=", "-=", "*=", "/=", "%=", "^=", "&=",
    "|=", "<<", "..", "?.", "?:", "!!",
];

fn tokens(code: &str) -> Vec<String> {
    let chars: Vec<char> = code.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c.is_whitespace() {
            let s = i;
            while i < chars.len() && chars[i].is_whitespace() {
                i += 1;
            }
            out.push(chars[s..i].iter().collect());
        } else if c.is_alphanumeric() || c == '_' {
            let s = i;
            while i < chars.len() && (chars[i].is_alphanumeric() || chars[i] == '_') {
                i += 1;
            }
            // decimal part of a float literal
            if c.is_ascii_digit() && i + 1 < chars.len() && chars[i] == '.' && chars[i + 1].is_ascii_digit() {
                i += 1;
                while i < chars.len() && (chars[i].is_alphanumeric() || chars[i] == '_') {
                    i += 1;
                }
            }
            out.push(chars[s..i].iter().collect());
        } else {
            let rest: String = chars[i..chars.len().min(i + 3)].iter().collect();
            if let Some(p) = PUNCTS.iter().find(|p| rest.starts_with(**p)) {
                out.push(p.to_string());
                i += p.chars().count();
            } else {
                out.push(c.to_string());
                i += 1;
            }
        }
    }
    out
}

fn is_space(t: &str) -> bool {
    t.chars().all(|c| c.is_whitespace())
}

#[derive(Clone, Copy, Default)]
struct LineFlags {
    /// `import`/`use` line: keep `{A, B}` as written
    import: bool,
    /// class/interface/enum/object header: keep ` : Base`
    decl: bool,
}

fn line_flags(text: &str) -> LineFlags {
    let mut words = text.split_whitespace().peekable();
    let mut first = words.next().unwrap_or("");
    while matches!(first, "pub" | "private" | "internal" | "public" | "data" | "error" | "shared" | "open" | "sealed" | "abstract" | "unsafe") {
        first = words.next().unwrap_or("");
    }
    LineFlags {
        import: matches!(first, "import" | "use"),
        decl: matches!(first, "class" | "interface" | "trait" | "enum" | "object" | "struct"),
    }
}

const SPACED_OPS: &[&str] = &["=", "==", "!=", "<=", ">=", "&&", "||", "+=", "-=", "*=", "/=", "%=", "->", "=>", "?:", "^=", "&=", "|="];

/// Normalizes spacing inside a code segment. `first`/`last` say whether the segment starts/ends the line.
fn space_code(code: &str, prev_tail: Option<&str>, next_head: Option<&str>, flags: LineFlags) -> String {
    let toks = tokens(code);
    let mut depth = 0i32;
    let mut out: Vec<String> = Vec::new();
    for (idx, t) in toks.iter().enumerate() {
        if is_space(t) {
            out.push(" ".into());
            continue;
        }
        let prev = out.iter().rev().find(|x| !is_space(x)).cloned().or_else(|| prev_tail.map(|s| s.to_string()));
        let had_space = out.last().map_or(prev_tail.is_some_and(|p| p.ends_with(' ')), |x| is_space(x));
        let t = t.as_str();
        let at_top = depth == 0;
        match t {
            "(" | "[" | "{" => depth += 1,
            ")" | "]" | "}" => depth -= 1,
            _ => {}
        }
        // `class A : Base` keeps the colon spacing as written
        if t == ":" && flags.decl && at_top {
            out.push(t.to_string());
            continue;
        }
        // remove spaces that shouldn't be there
        // `class A : Base` keeps its space; `x : Int` becomes `x: Int`
        let colon_tight = t == ":" && !matches!(prev.as_deref(), Some(")") | Some(">") | Some("?"));
        // `x?` and `T?` are tight, `T: ?Sized` is not
        let postfix_q = t == "?" && prev.as_deref().and_then(|p| p.chars().last()).is_some_and(|c| c.is_alphanumeric() || matches!(c, '_' | ')' | ']' | '}' | '>' | '"' | '\''));
        let no_space_before = matches!(t, "," | ";" | ")" | "]" | "?." | "!!" | "::") || postfix_q || colon_tight;
        // `name!` of a macro and the `@` of a binding (`x @ 1..=9`) keep the space after them as written
        let prev_idx = toks[..idx].iter().rposition(|x| !is_space(x));
        let spaced_as_written = prev_idx.is_some_and(|p| match toks[p].as_str() {
            "!" => p > 0 && is_word(&toks[p - 1]),
            "@" => toks[..p].iter().rev().find(|x| !is_space(x)).is_some_and(|x| is_word(x)),
            _ => false,
        });
        let after_opener = matches!(prev.as_deref(), Some("(") | Some("[") | Some("::") | Some("!") | Some("@") | Some("#") | Some("?.") | Some(".")) && !spaced_as_written;
        if had_space && (no_space_before || after_opener) && !out.is_empty() {
            out.pop();
        }
        let need_space_before = SPACED_OPS.contains(&t)
            || (t == "{" && prev.as_deref().is_some_and(|p| !matches!(p, "(" | "[" | "{" | "$" | "!" | "#" | "::" | "&")))
            || (t == "(" && matches!(prev.as_deref(), Some("if") | Some("for") | Some("while") | Some("when") | Some("catch")));
        let has_space_now = out.last().map_or(prev_tail.is_none_or(|p| p.is_empty() || p.ends_with(' ')), |x| is_space(x));
        if need_space_before && !has_space_now && (!out.is_empty() || prev_tail.is_some()) {
            out.push(" ".into());
        }
        out.push(t.to_string());
        // space after
        let next = toks[idx + 1..].iter().find(|x| !is_space(x)).map(|s| s.as_str()).or(next_head);
        let next_is_space = toks.get(idx + 1).is_some_and(|x| is_space(x));
        let want_after = (matches!(t, "," | ";") && !matches!(next, Some(")") | Some("]") | Some("}") | None) && !repetition_op(&toks[idx + 1..]))
            || (t == ":" && next.is_some() && !matches!(next, Some(":")) && !macro_fragment(&toks[..idx]))
            || SPACED_OPS.contains(&t)
            || (t == "{" && !matches!(next, Some("}") | None) && !flags.import);
        let want_before_close = t != "{" && next == Some("}") && !matches!(t, "{" | "$") && !flags.import;
        if (want_after || want_before_close) && !next_is_space && next.is_some() && toks.get(idx + 1).is_some() {
            out.push(" ".into());
        }
    }
    // collapse multiple spaces
    let mut s = String::new();
    for t in out {
        if is_space(&t) && s.ends_with(' ') {
            continue;
        }
        s.push_str(&t);
    }
    s
}

fn is_word(t: &str) -> bool {
    t.chars().next().is_some_and(|c| c.is_alphanumeric() || c == '_')
}

/// Whether the tokens before a `:` end with a macro metavariable, as in `$x:expr`.
fn macro_fragment(before: &[String]) -> bool {
    matches!(before, [.., d, w] if d == "$" && is_word(w))
}

/// Whether the tokens after a separator are a macro repetition operator, as in `$(..),*`.
fn repetition_op(after: &[String]) -> bool {
    matches!(after.first().map(|s| s.as_str()), Some("*" | "+" | "?"))
        && after.get(1).is_none_or(|s| is_space(s) || matches!(s.as_str(), ")" | "]" | "}" | "=>" | ";"))
}

/// The formatted source; a file with syntax errors is returned unchanged.
pub fn format_source(src: &str) -> String {
    format_checked(src).unwrap_or_else(|_| src.to_string())
}

/// The formatted source, or the syntax errors that keep it from being formatted
/// (like rustfmt, files with syntax errors are left alone).
pub fn format_checked(src: &str) -> Result<String, Vec<crate::diag::Diag>> {
    let (_, diags) = crate::parser::parse_file(src);
    let errors: Vec<_> = diags.into_iter().filter(|d| d.severity == crate::diag::Severity::Error).collect();
    if !errors.is_empty() {
        return Err(errors);
    }
    let mut out = format_inner(src);
    // keep Windows line endings
    if src.find('\n').is_some_and(|i| src[..i].ends_with('\r')) {
        out = out.replace('\n', "\r\n");
    }
    Ok(if same_tokens(src, &out) { out } else { src.to_string() })
}

fn same_tokens(a: &str, b: &str) -> bool {
    let (ta, ea) = lexer::lex(a);
    let (tb, eb) = lexer::lex(b);
    if ea.len() != eb.len() {
        return false;
    }
    let norm = |t: &Tok| -> Tok {
        match t {
            Tok::Str(parts) => Tok::Str(
                parts
                    .iter()
                    .map(|p| match p {
                        lexer::StrPart::Interp { src, spec, .. } => lexer::StrPart::Interp { src: src.clone(), offset: 0, spec: spec.clone() },
                        other => other.clone(),
                    })
                    .collect(),
            ),
            // the formatter removes trailing whitespace, also after a doc comment
            Tok::DocOuter(s) => Tok::DocOuter(s.trim_end().to_string()),
            Tok::DocInner(s) => Tok::DocInner(s.trim_end().to_string()),
            other => other.clone(),
        }
    };
    let ka: Vec<Tok> = ta.iter().map(|t| norm(&t.tok)).collect();
    let kb: Vec<Tok> = tb.iter().map(|t| norm(&t.tok)).collect();
    ka == kb
}

fn ends_with_continuation(code: &str) -> bool {
    let t = code.trim_end();
    // `{ a, b ->` starts a lambda body, not a continued expression
    if (t.ends_with("->") || t.ends_with("=>")) && t.contains('{') {
        return false;
    }
    ["=", "->", "=>", "&&", "||", "+", "-", "*", "/", "?:", ",", "(", "["].iter().any(|op| t.ends_with(op))
        && !t.ends_with("==")
        && !t.ends_with("!=")
        && !t.ends_with("<=")
        && !t.ends_with(">=")
        && !t.ends_with("..")
        // a glob import, `import a::*` or `import a.*`
        && !t.ends_with("::*")
        && !t.ends_with(".*")
}

fn starts_with_continuation(code: &str) -> bool {
    let t = code.trim_start();
    (t.starts_with('.') && !t.starts_with("..")) || t.starts_with("?.") || t.starts_with("?:") || t.starts_with("&&") || t.starts_with("||")
}

fn format_inner(src: &str) -> String {
    let lines = split(src);
    let mut out: Vec<String> = Vec::new();
    // the indentation level of the line that opened each currently open bracket
    let mut open: Vec<i32> = Vec::new();
    // level of the last line at each bracket depth, for `else` lines
    let mut depth_level: Vec<i32> = Vec::new();
    let mut prev_continues = false;
    let mut prev_open = false;
    let mut level = 0;
    for line in &lines {
        let text: String = line.segs.iter().map(|(_, s)| s.as_str()).collect();
        if line.raw {
            out.push(text.trim_end_matches('\r').to_string());
            let ct = code_text(line);
            prev_continues = !ct.trim().is_empty() && ends_with_continuation(&ct);
            // brackets inside the verbatim continuation don't count, but a string ending here may
            // be followed by code: its brackets belong to the statement's level
            for (k, s) in &line.segs {
                if *k == Seg::Code {
                    track(&mut open, s, level);
                }
            }
            continue;
        }
        let trimmed_first = text.trim_start();
        if trimmed_first.is_empty() {
            // blank line: collapse runs and drop blank lines after an opening bracket
            if out.last().is_none_or(|l| l.trim().is_empty()) || prev_open {
                continue;
            }
            out.push(String::new());
            continue;
        }
        // leading closers dedent
        let mut lead_close = 0;
        for c in trimmed_first.chars() {
            match c {
                '}' | ')' | ']' => lead_close += 1,
                ' ' | '\t' => {}
                _ => break,
            }
        }
        let first_seg_code = line.segs.iter().find(|(_, s)| !s.trim().is_empty()).is_none_or(|(k, _)| *k == Seg::Code);
        let starts_else = first_seg_code && trimmed_first.split(|c: char| !c.is_alphanumeric() && c != '_').next() == Some("else");
        let cont = (first_seg_code && starts_with_continuation(trimmed_first)) || (prev_continues && lead_close == 0 && !starts_else);
        let depth = open.len().saturating_sub(lead_close);
        level = if lead_close > 0 {
            // a closer goes back to the level of the line that opened it
            open.get(depth).copied().unwrap_or(0)
        } else if starts_else && depth < depth_level.len() {
            // `else` lines up with its `if` (or the previous `else if`)
            depth_level[depth]
        } else {
            depth.checked_sub(1).map_or(0, |d| open[d] + 1) + if cont { 1 } else { 0 }
        };
        depth_level.resize(depth, 0);
        depth_level.push(level);
        // drop a blank line right before a closing bracket
        if lead_close > 0 && out.last().is_some_and(|l| l.trim().is_empty()) {
            out.pop();
        }
        // format segments
        let flags = line_flags(&text);
        let mut body = String::new();
        let n = line.segs.len();
        let mut first = true;
        for (idx, (k, s)) in line.segs.iter().enumerate() {
            match k {
                Seg::Verbatim => body.push_str(s),
                Seg::Code => {
                    let s2 = if first { s.trim_start() } else { s.as_str() };
                    let s2 = if idx == n - 1 { s2.trim_end() } else { s2 };
                    let prev_tail = if body.is_empty() { None } else { Some(body.as_str()) };
                    let next_head = line.segs.get(idx + 1).map(|(_, x)| x.as_str());
                    let mut formatted = space_code(s2, prev_tail, next_head, flags);
                    if idx + 1 < n && line.segs[idx + 1].1.starts_with("//") && !formatted.trim().is_empty() {
                        // keep the spaces before a trailing comment: they often align comments
                        let gap = &s[s.trim_end().len()..];
                        let gap = if gap.is_empty() || gap.contains('\t') { " " } else { gap };
                        formatted = format!("{}{}", formatted.trim_end(), gap);
                    }
                    body.push_str(&formatted);
                    track(&mut open, s, level);
                }
            }
            if !s.trim().is_empty() {
                first = false;
            }
        }
        let body = body.trim_end().to_string();
        out.push(format!("{}{}", INDENT.repeat(level as usize), body));
        let code_only = code_text(line);
        prev_continues = ends_with_continuation(&code_only) && !code_only.trim_end().ends_with('(') && !code_only.trim_end().ends_with('[') && !code_only.trim_end().ends_with(',');
        prev_open = body.ends_with('{') || body.ends_with('(') || body.ends_with('[');
    }
    while out.last().is_some_and(|l| l.trim().is_empty()) {
        out.pop();
    }
    let mut s = out.join("\n");
    s.push('\n');
    s
}

/// The line's code with strings replaced by `""` and comments removed.
fn code_text(line: &Line) -> String {
    line.segs
        .iter()
        .map(|(k, s)| match k {
            Seg::Code => s.clone(),
            Seg::Verbatim if s.starts_with("//") || s.starts_with("/*") => String::new(),
            Seg::Verbatim => "\"\"".to_string(),
        })
        .collect()
}

/// Updates the open-bracket stack with the brackets of `code`, opened on a line indented
/// `level`: what they enclose is indented one more, so a line opening `({` indents once.
fn track(open: &mut Vec<i32>, code: &str, level: i32) {
    for c in code.chars() {
        match c {
            '{' | '(' | '[' => open.push(level),
            '}' | ')' | ']' => {
                open.pop();
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::format_source;

    #[test]
    fn reindents_and_spaces() {
        let src = "fun main(){\nval x=1\n  if (x==1) {\nprintln(\"a , b\")\n}\n}\n";
        let want = "fun main() {\n    val x = 1\n    if (x == 1) {\n        println(\"a , b\")\n    }\n}\n";
        assert_eq!(format_source(src), want);
    }

    #[test]
    fn continuation_and_comments() {
        let src = "fun f() {\n  val v = xs\n  .iter()   // comment\n  .map { it * 2 }\n}\n";
        let want = "fun f() {\n    val v = xs\n        .iter()   // comment\n        .map { it * 2 }\n}\n";
        assert_eq!(format_source(src), want);
    }

    #[test]
    fn keeps_strings() {
        let src = "val s = \"\"\"\n   keep   this\n\"\"\"\nval t = \"a  {b}  ${ x }\"\n";
        assert_eq!(format_source(src), src);
    }

    #[test]
    fn glob_imports_do_not_continue() {
        let src = "import std::collections::*\n\nenum Op { Add, Sub }\n\nfun main() {\n    import Op::*\n    println(\"x\")\n}\n";
        assert_eq!(format_source(src), src);
    }

    #[test]
    fn else_lines_up_with_if() {
        let src = "fun f(a: i32): i32 =\n  if (a == 1) 1\nelse if (a == 2) 2\n      else 3\n\nfun g(a: bool): i32 =\nif (a) {\n1\n} else {\n2\n}\n";
        let want = "fun f(a: i32): i32 =\n    if (a == 1) 1\n    else if (a == 2) 2\n    else 3\n\nfun g(a: bool): i32 =\n    if (a) {\n        1\n    } else {\n        2\n    }\n";
        assert_eq!(format_source(src), want);
        assert_eq!(format_source(want), want);
    }

    #[test]
    fn keeps_spacing_of_patterns_macros_and_bounds() {
        let src = "fun k<T: ?Sized>(x: &T) {\n    when (3) {\n        x @ 1..=9 -> println(\"a\")   // aligned\n        n @ &_ -> println(\"b\")       // comments\n    }\n    val r = &{ n: i32 -> n + 1 }\n}\n\nmacro_rules! square {\n    ($($x:expr),*) => { 0 $(+ $x * $x)* }\n}\n";
        assert_eq!(format_source(src), src);
        let tight = src.replace("T: ?Sized", "T:?Sized").replace("x @ 1", "x @  1").replace("rules! square", "rules!  square");
        assert_eq!(format_source(&tight), src);
    }

    #[test]
    fn trailing_spaces_after_doc_comments() {
        let src = "/// Doc.   \nfun f() {\nval x = 1\n}\n";
        assert_eq!(format_source(src), "/// Doc.\nfun f() {\n    val x = 1\n}\n");
    }

    #[test]
    fn literals_inside_interpolations() {
        let src = "fun main() {\nval c = '{'\nprintln(\"${ if (c == '{') \"a\" else \"}\" }\")\nprintln(\"${s.replace('\"', \"x\")}\")\n  val x=1\n}\n";
        let want = "fun main() {\n    val c = '{'\n    println(\"${ if (c == '{') \"a\" else \"}\" }\")\n    println(\"${s.replace('\"', \"x\")}\")\n    val x = 1\n}\n";
        assert_eq!(format_source(src), want);
    }

    #[test]
    fn keeps_crlf() {
        let src = "fun main() {\r\nval x=1\r\n}\r\n";
        assert_eq!(format_source(src), "fun main() {\r\n    val x = 1\r\n}\r\n");
    }

    #[test]
    fn idempotent() {
        let src = "class A(x: i32) {\n\n\n    fun f(a: i32, b: i32): i32 = a + b\n\n}\n";
        let once = format_source(src);
        assert_eq!(format_source(&once), once);
        assert_eq!(once, "class A(x: i32) {\n    fun f(a: i32, b: i32): i32 = a + b\n}\n");
    }
}

