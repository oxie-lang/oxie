//! Rewrites locations in generated Rust (`…/out/oxie/main.rs:10:5`) in a running program's
//! output, such as panic messages and backtraces, to the `.oxi` positions they came from.

use std::io::{Read, Write};
use std::path::Path;

use crate::cargo::Mapper;

/// Rewrites every `path.rs:line:col` whose file has a source map; other text is unchanged,
/// except that backtrace frames of `main() throws` (generated as `__oxie_main`) say `main`.
pub fn rewrite(text: &str, mapper: &mut Mapper) -> String {
    let text = text.replace("::__oxie_main", "::main");
    let mut out = String::with_capacity(text.len());
    let mut rest = text.as_str();
    while let Some(i) = rest.find(".rs:") {
        let (before, after) = rest.split_at(i + 3);
        // the path starts after the last space, quote or parenthesis
        let start = before.rfind(|c: char| c.is_whitespace() || matches!(c, '\'' | '"' | '(' | '<')).map_or(0, |s| s + 1);
        if let Some((line, col, len)) = line_col(&after[1..]) {
            if let Some((src, l, c)) = mapper.map_location(Path::new(&before[start..]), line, col) {
                let cwd = std::env::current_dir().unwrap_or_default();
                out.push_str(&before[..start]);
                out.push_str(&format!("{}:{}:{}", src.strip_prefix(&cwd).unwrap_or(&src).display(), l, c));
                rest = &after[1 + len..];
                continue;
            }
        }
        out.push_str(before);
        rest = after;
    }
    out.push_str(rest);
    out
}

/// `10:5` at the start of `s`: the line, the column and the length of the text.
fn line_col(s: &str) -> Option<(u32, u32, usize)> {
    let digits = |s: &str| s.find(|c: char| !c.is_ascii_digit()).unwrap_or(s.len());
    let n = digits(s);
    let line = s[..n].parse().ok()?;
    let rest = s[n..].strip_prefix(':')?;
    let m = digits(rest);
    let col = rest[..m].parse().ok()?;
    Some((line, col, n + 1 + m))
}

/// Copies `from` to `to` as it arrives, rewriting locations. Whole lines are rewritten; a
/// partial line (a prompt) is passed on at once unless it may be the start of a path.
pub fn forward(mut from: impl Read, mut to: impl Write) {
    let mut mapper = Mapper::default();
    let mut buf = [0u8; 8192];
    let mut pending: Vec<u8> = Vec::new();
    loop {
        let n = match from.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => n,
        };
        pending.extend_from_slice(&buf[..n]);
        let lines_end = pending.iter().rposition(|&b| b == b'\n').map_or(0, |i| i + 1);
        let mut cut = if pending[lines_end..].iter().any(|&b| b == b'/' || b == b'\\') { lines_end } else { pending.len() };
        // a backtrace frame's name is written in pieces (`app`, `::`, `__oxie_main`): a `::` at the
        // end waits for the next piece, so that `::__oxie_main` is seen whole
        if cut == pending.len() && pending[lines_end..].ends_with(b"::") {
            cut -= 2;
        }
        // keep a character split between two reads for the next one
        if let Err(e) = std::str::from_utf8(&pending[..cut]) {
            if e.error_len().is_none() {
                cut = e.valid_up_to();
            }
        }
        if cut > 0 {
            let _ = to.write_all(rewrite(&String::from_utf8_lossy(&pending[..cut]), &mut mapper).as_bytes());
            let _ = to.flush();
            pending.drain(..cut);
        }
    }
    if !pending.is_empty() {
        let _ = to.write_all(rewrite(&String::from_utf8_lossy(&pending), &mut mapper).as_bytes());
        let _ = to.flush();
    }
}

/// A reader that passes `inner` on with some of its lines changed: a line whose start `hold`
/// accepts is held until it is complete and replaced by `edit` (`None` drops it); other lines
/// pass as they arrive, so a partial line like a prompt isn't delayed.
pub struct EditLines<R> {
    inner: R,
    hold: fn(&[u8]) -> bool,
    edit: fn(&str) -> Option<String>,
    /// The held start of the current line.
    line: Vec<u8>,
    holding: bool,
    at_start: bool,
    ready: Vec<u8>,
    done: bool,
}

impl<R: Read> EditLines<R> {
    pub fn new(inner: R, hold: fn(&[u8]) -> bool, edit: fn(&str) -> Option<String>) -> Self {
        EditLines { inner, hold, edit, line: Vec::new(), holding: false, at_start: true, ready: Vec::new(), done: false }
    }

    fn finish_line(&mut self) {
        let line = std::mem::take(&mut self.line);
        if let Some(l) = (self.edit)(&String::from_utf8_lossy(&line)) {
            self.ready.extend_from_slice(l.as_bytes());
        }
        self.holding = false;
    }
}

impl<R: Read> Read for EditLines<R> {
    fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
        let mut buf = [0u8; 8192];
        while self.ready.is_empty() && !self.done {
            let n = self.inner.read(&mut buf)?;
            if n == 0 {
                self.done = true;
                if self.holding {
                    self.finish_line();
                }
            }
            for &b in &buf[..n] {
                if self.at_start {
                    self.at_start = false;
                    self.holding = true;
                }
                if self.holding {
                    self.line.push(b);
                    if b == b'\n' {
                        self.finish_line();
                    } else if !(self.hold)(&self.line) {
                        self.ready.append(&mut self.line);
                        self.holding = false;
                    }
                } else {
                    self.ready.push(b);
                }
                if b == b'\n' {
                    self.at_start = true;
                }
            }
        }
        let n = out.len().min(self.ready.len());
        out[..n].copy_from_slice(&self.ready[..n]);
        self.ready.drain(..n);
        Ok(n)
    }
}

/// Is this (the start of) a line one of cargo's JSON messages (`--message-format=json`)?
pub fn is_cargo_message(line: &[u8]) -> bool {
    const START: &[u8] = b"{\"reason\":";
    if line.len() < START.len() {
        START.starts_with(line)
    } else {
        line.starts_with(START)
    }
}

/// cargo's `error: test failed, to rerun pass `--bin app_1f2e`` without the hint, which names a
/// cargo target (for a script, its hidden crate) that `oxie test` doesn't take.
pub fn without_rerun_hint(line: &str) -> Option<String> {
    match line.find(", to rerun pass `") {
        Some(i) => {
            let end = line[i..].find("`\n").or_else(|| line[i..].rfind('`')).map_or(line.len(), |e| i + e + 1);
            Some(format!("{}{}", &line[..i], &line[end..]))
        }
        None => Some(line.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cargo_messages_are_dropped_and_rerun_hints_removed() {
        let mut out = Vec::new();
        let text = "{\"reason\":\"compiler-message\"}\nrunning 1 test\ntest a ... ok\n{\"reason\":\"build-finished\"}\n{not json\n";
        forward(EditLines::new(Pieces(vec![&text.as_bytes()[..5], &text.as_bytes()[5..40], &text.as_bytes()[40..]]), is_cargo_message, |_| None), &mut out);
        assert_eq!(String::from_utf8(out).unwrap(), "running 1 test\ntest a ... ok\n{not json\n");
        let mut out = Vec::new();
        forward(EditLines::new("error: test failed, to rerun pass `--bin x_1f`\nother\n".as_bytes(), |_| true, without_rerun_hint), &mut out);
        assert_eq!(String::from_utf8(out).unwrap(), "error: test failed\nother\n");
    }

    #[test]
    fn rewrites_mapped_locations_only() {
        let dir = std::env::temp_dir().join(format!("oxie-locations-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let src = dir.join("app.oxi");
        std::fs::write(&src, "fun nth(xs: &[i32], i: usize): i32 = xs[i]\n").unwrap();
        let gen = dir.join("main.rs");
        std::fs::write(&gen, "pub(crate) fn nth(xs: &[i32], i: usize) -> i32 {\n    xs[i]\n}\n").unwrap();
        std::fs::write(dir.join("main.rs.map"), format!("oxie-sourcemap 1\n{}\n0 0 0 0\n1 4 0 37\n", src.display())).unwrap();
        let mut m = Mapper::default();
        let text = format!("thread 'main' panicked at {}:2:5:\nindex out of bounds\n  at /rustc/library/core/src/slice/index.rs:12:3\n   6: app::__oxie_main\n", gen.display());
        let out = rewrite(&text, &mut m);
        assert!(out.contains(&format!("panicked at {}:1:38:\n", src.display())), "{out}");
        assert!(out.contains("/rustc/library/core/src/slice/index.rs:12:3"), "{out}");
        assert!(out.contains("   6: app::main\n"), "{out}");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn forwards_partial_lines() {
        let mut out = Vec::new();
        forward("name? ".as_bytes(), &mut out);
        assert_eq!(out, b"name? ");
        assert_eq!(line_col("10:5: x"), Some((10, 5, 4)));
        assert_eq!(line_col("10 x"), None);
    }

    /// Output that arrives one piece per read.
    struct Pieces(Vec<&'static [u8]>);

    impl Read for Pieces {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            if self.0.is_empty() {
                return Ok(0);
            }
            let p = self.0.remove(0);
            buf[..p.len()].copy_from_slice(p);
            Ok(p.len())
        }
    }

    #[test]
    fn frame_names_arriving_in_pieces_are_renamed() {
        let mut out = Vec::new();
        forward(Pieces(vec![b"   6: app", b"::", b"__oxie_main\n"]), &mut out);
        assert_eq!(String::from_utf8(out).unwrap(), "   6: app::main\n");
    }
}
