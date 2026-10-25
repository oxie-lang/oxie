//! Recursive-descent parser for Oxie.

use crate::ast::*;
use crate::diag::{Diag, Span};
use crate::lexer::{self, StrPart, Tok, Token};

/// Words that cannot be used as plain identifiers in expressions and patterns.
pub const RESERVED: &[&str] = &[
    "fun", "fn", "val", "var", "let", "if", "else", "when", "match", "for", "while", "do", "loop", "return", "break",
    "continue", "throw", "try", "class", "interface", "trait", "enum", "struct", "impl", "import", "use", "const",
    "static", "typealias", "mod", "true", "false", "null", "this", "self", "Self", "in", "is", "as", "where", "async",
    "move", "unsafe", "dyn", "mut", "ref", "extern", "pub", "catch",
];

pub fn is_reserved(s: &str) -> bool {
    RESERVED.contains(&s)
}

/// Upper camel case or single uppercase letter: looks like a type or enum variant.
pub fn is_type_like(s: &str) -> bool {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) if c.is_uppercase() => {}
        _ => return false,
    }
    s.len() == 1 || s.chars().any(|c| c.is_lowercase())
}

pub struct Parser<'a> {
    src: &'a str,
    /// `#![...]` attributes seen at file level
    inner_attrs: Vec<String>,
    toks: Vec<Token>,
    pos: usize,
    pub diags: Vec<Diag>,
    /// Rust-style condition context: `{` ends the expression.
    no_struct: bool,
    /// The type after `as`: a `<` after its path starts generic arguments only if they parse,
    /// otherwise it compares (`n as usize < m`).
    cast_ty: bool,
    /// Whether an `Fn(..)` type takes a `-> R` after it (not when that `->` ends a lambda's
    /// parameters).
    fn_ret: bool,
    /// An error message template, where `$0` is the case's first positional field; elsewhere
    /// it is text (`"cost $5"`).
    positional: bool,
    /// Type names this file declares that don't look like types (`class DB`, `enum HTTP`).
    caps_types: std::collections::HashSet<String>,
}

/// Names declared by `class`/`enum`/`interface`/... in `toks` that are all capitals (`DB`, `V8`),
/// which `is_type_like` would take for constants.
fn caps_type_names(toks: &[Token]) -> std::collections::HashSet<String> {
    let mut out = std::collections::HashSet::new();
    for w in toks.windows(2) {
        if let (Tok::Ident(kw), Tok::Ident(name)) = (&w[0].tok, &w[1].tok) {
            if matches!(kw.as_str(), "class" | "enum" | "interface" | "trait" | "struct" | "typealias" | "union")
                && !is_type_like(name)
                && name.starts_with(|c: char| c.is_uppercase())
            {
                out.insert(name.clone());
            }
        }
    }
    out
}

pub fn parse_file(src: &str) -> (File, Vec<Diag>) {
    let (toks, mut diags) = lexer::lex(src);
    let caps_types = caps_type_names(&toks);
    let mut p = Parser { src, inner_attrs: Vec::new(), toks, pos: 0, diags: Vec::new(), no_struct: false, cast_ty: false, positional: false, fn_ret: true, caps_types };
    let file = p.file();
    diags.append(&mut p.diags);
    (file, diags)
}

/// Parses a standalone expression (used for tests and tooling).
pub fn parse_expr_str(src: &str) -> (Expr, Vec<Diag>) {
    let (toks, mut diags) = lexer::lex(src);
    let caps_types = caps_type_names(&toks);
    let mut p = Parser { src, inner_attrs: Vec::new(), toks, pos: 0, diags: Vec::new(), no_struct: false, cast_ty: false, positional: false, fn_ret: true, caps_types };
    let e = p.expr().unwrap_or(Expr::new(ExprKind::Error, Span::default()));
    diags.append(&mut p.diags);
    (e, diags)
}

type PResult<T> = Result<T, ()>;

impl<'a> Parser<'a> {
    // ------------------------------------------------------------------ token helpers

    /// Whether `s` names a type: it looks like one, or this file declares it (`class DB`).
    fn type_like(&self, s: &str) -> bool {
        is_type_like(s) || self.caps_types.contains(s)
    }

    fn tok(&self) -> &Tok {
        &self.toks[self.pos].tok
    }
    fn tok_at(&self, n: usize) -> &Tok {
        let i = (self.pos + n).min(self.toks.len() - 1);
        &self.toks[i].tok
    }
    fn span(&self) -> Span {
        self.toks[self.pos].span
    }
    fn prev_span(&self) -> Span {
        if self.pos == 0 {
            Span::default()
        } else {
            self.toks[self.pos - 1].span
        }
    }
    fn bump(&mut self) -> Token {
        let t = self.toks[self.pos].clone();
        if self.pos < self.toks.len() - 1 {
            self.pos += 1;
        }
        t
    }
    fn is_punct(&self, p: &str) -> bool {
        matches!(self.tok(), Tok::Punct(q) if *q == p)
    }
    fn is_punct_at(&self, n: usize, p: &str) -> bool {
        matches!(self.tok_at(n), Tok::Punct(q) if *q == p)
    }
    fn is_kw(&self, kw: &str) -> bool {
        matches!(self.tok(), Tok::Ident(s) if s == kw)
    }
    fn is_kw_at(&self, n: usize, kw: &str) -> bool {
        matches!(self.tok_at(n), Tok::Ident(s) if s == kw)
    }
    fn eat_punct(&mut self, p: &str) -> bool {
        if self.is_punct(p) {
            self.bump();
            true
        } else {
            false
        }
    }
    fn eat_kw(&mut self, kw: &str) -> bool {
        if self.is_kw(kw) {
            self.bump();
            true
        } else {
            false
        }
    }
    fn is_newline(&self) -> bool {
        matches!(self.tok(), Tok::Newline)
    }
    fn skip_newlines(&mut self) {
        while matches!(self.tok(), Tok::Newline) || self.is_punct(";") {
            self.bump();
        }
    }
    fn skip_nl_only(&mut self) {
        while matches!(self.tok(), Tok::Newline) {
            self.bump();
        }
    }
    /// Peeks past newlines for `kw`; consumes the newlines only if found.
    fn peek_past_newlines_kw(&mut self, kw: &str) -> bool {
        let mut i = 0;
        while matches!(self.tok_at(i), Tok::Newline) {
            i += 1;
        }
        if self.is_kw_at(i, kw) {
            for _ in 0..i {
                self.bump();
            }
            true
        } else {
            false
        }
    }
    fn peek_past_newlines_punct(&mut self, p: &str) -> bool {
        let mut i = 0;
        while matches!(self.tok_at(i), Tok::Newline) {
            i += 1;
        }
        if self.is_punct_at(i, p) {
            for _ in 0..i {
                self.bump();
            }
            true
        } else {
            false
        }
    }
    fn at_eof(&self) -> bool {
        matches!(self.tok(), Tok::Eof)
    }

    fn describe(&self) -> String {
        match self.tok() {
            Tok::Ident(s) => format!("`{}`", s),
            Tok::RawIdent(s) => format!("`r#{}`", s),
            Tok::Punct(p) => format!("`{}`", p),
            Tok::Newline => "end of line".into(),
            Tok::Eof => "end of file".into(),
            Tok::Int(s) | Tok::Float(s) => format!("number `{}`", s),
            Tok::Str(_) | Tok::RawStr(_) => "string literal".into(),
            Tok::Char(_) => "character literal".into(),
            Tok::Lifetime(s) => format!("`'{}`", s),
            Tok::DocOuter(_) | Tok::DocInner(_) => "doc comment".into(),
        }
    }

    fn err<T>(&mut self, msg: impl Into<String>) -> PResult<T> {
        let mut sp = self.span();
        // if the offending token is on a later line, point at the end of the previous one
        if self.pos > 0 {
            let prev = self.prev_span();
            if (prev.end as usize) <= (sp.start as usize) && self.src.get(prev.end as usize..sp.start as usize).is_some_and(|gap| gap.contains('\n')) {
                sp = Span::empty_at(prev.end);
            }
        }
        self.diags.push(Diag::error(sp, msg));
        Err(())
    }
    fn expected<T>(&mut self, what: &str) -> PResult<T> {
        let found = self.describe();
        self.err(format!("expected {}, found {}", what, found))
    }
    fn expect_punct(&mut self, p: &str) -> PResult<Span> {
        if self.is_punct(p) {
            Ok(self.bump().span)
        } else {
            self.expected(&format!("`{}`", p))
        }
    }

    fn ident(&mut self) -> PResult<String> {
        match self.tok().clone() {
            Tok::Ident(s) if !is_reserved(&s) => {
                self.bump();
                Ok(s)
            }
            Tok::RawIdent(s) => {
                self.bump();
                Ok(format!("r#{}", s))
            }
            _ => self.expected("identifier"),
        }
    }
    /// Identifier after `.` or in a path: any word is allowed (`self`, `type`, keywords...).
    fn member_name(&mut self) -> PResult<String> {
        match self.tok().clone() {
            Tok::Ident(s) => {
                self.bump();
                Ok(s)
            }
            Tok::RawIdent(s) => {
                self.bump();
                Ok(format!("r#{}", s))
            }
            Tok::Int(s) => {
                self.bump();
                Ok(s)
            }
            _ => self.expected("name"),
        }
    }
    fn is_ident(&self) -> bool {
        match self.tok() {
            Tok::Ident(s) => !is_reserved(s),
            Tok::RawIdent(_) => true,
            _ => false,
        }
    }
    fn is_ident_at(&self, n: usize) -> bool {
        match self.tok_at(n) {
            Tok::Ident(s) => !is_reserved(s),
            Tok::RawIdent(_) => true,
            _ => false,
        }
    }

    /// Finds the index of the token closing the bracket at `self.pos + 0`.
    fn matching_close(&self, open_idx: usize) -> Option<usize> {
        let mut depth = 0i32;
        let mut i = open_idx;
        while i < self.toks.len() {
            match &self.toks[i].tok {
                Tok::Punct("(") | Tok::Punct("[") | Tok::Punct("{") => depth += 1,
                Tok::Punct(")") | Tok::Punct("]") | Tok::Punct("}") => {
                    depth -= 1;
                    if depth == 0 {
                        return Some(i);
                    }
                }
                Tok::Eof => return None,
                _ => {}
            }
            i += 1;
        }
        None
    }

    /// Consumes a balanced bracket group and returns the raw source text including delimiters.
    fn raw_group(&mut self) -> PResult<String> {
        let open = self.pos;
        let Some(close) = self.matching_close(open) else {
            return self.err("unbalanced brackets");
        };
        let text = self.src[self.toks[open].span.start as usize..self.toks[close].span.end as usize].to_string();
        self.pos = close + 1;
        Ok(text)
    }

    /// Recovery: skip to the end of the current statement.
    fn recover_stmt(&mut self) {
        let mut depth = 0i32;
        loop {
            match self.tok() {
                Tok::Eof => return,
                Tok::Newline | Tok::Punct(";") if depth <= 0 => return,
                Tok::Punct("(") | Tok::Punct("[") | Tok::Punct("{") => depth += 1,
                Tok::Punct(")") | Tok::Punct("]") | Tok::Punct("}") => {
                    if depth <= 0 {
                        return;
                    }
                    depth -= 1;
                }
                _ => {}
            }
            self.bump();
        }
    }

    /// Recovery after a broken `when` arm that started at token `arm_pos`: skips the rest of
    /// it, including a `)` or `]` it opened before the error, and always moves on.
    fn recover_arm(&mut self, arm_pos: usize) {
        self.recover_stmt();
        while self.is_punct(")") || self.is_punct("]") {
            self.bump();
            self.recover_stmt();
        }
        if self.pos == arm_pos {
            self.bump();
        }
    }

    // ------------------------------------------------------------------ file & items

    fn file(&mut self) -> File {
        let mut inner_docs = Vec::new();
        let mut items = Vec::new();
        loop {
            self.skip_newlines();
            if let Tok::DocInner(d) = self.tok().clone() {
                self.bump();
                inner_docs.push(d);
                continue;
            }
            if self.at_eof() {
                break;
            }
            if self.is_punct("}") {
                self.err::<()>("unexpected `}`").ok();
                self.bump();
                continue;
            }
            let start = self.pos;
            match self.item() {
                Ok(Some(item)) => items.push(item),
                Ok(None) if self.is_kw("var") => {
                    let sp = self.span();
                    self.diags.push(Diag::error(sp, "a file can't declare a `var`").with_help(
                        "a value every function can change is a `static var`, which only `unsafe` code may use; an atomic (`static COUNT: AtomicI32 = AtomicI32.new(0)`) or a `Mutex` is safe. Or pass the value to the functions that change it.",
                    ));
                    self.recover_stmt();
                }
                Ok(None) => {
                    if !self.global_var() {
                        let found = self.describe();
                        self.err::<()>(format!("expected a declaration (fun, class, val, import, ...), found {}", found)).ok();
                    }
                    self.recover_stmt();
                }
                Err(()) => self.recover_stmt(),
            }
            if self.pos == start {
                self.bump();
            }
        }
        File { inner_docs, inner_attrs: std::mem::take(&mut self.inner_attrs), items }
    }

    fn docs_and_attrs(&mut self) -> PResult<(Vec<String>, Vec<Attr>)> {
        let mut docs = Vec::new();
        let mut attrs = Vec::new();
        loop {
            match self.tok().clone() {
                Tok::DocOuter(d) => {
                    self.bump();
                    docs.push(d);
                    self.skip_nl_only();
                }
                Tok::Punct("@") => {
                    let start = self.bump().span;
                    let mut name = self.member_name()?;
                    while self.is_punct("::") || (self.is_punct(".") && matches!(self.tok_at(1), Tok::Ident(_))) {
                        self.bump();
                        name.push_str("::");
                        name.push_str(&self.member_name()?);
                    }
                    let args = if self.is_punct("(") {
                        let g = self.raw_group()?;
                        Some(g[1..g.len() - 1].to_string())
                    } else {
                        None
                    };
                    attrs.push(Attr::Oxie { name, args, span: start.to(self.prev_span()) });
                    self.skip_nl_only();
                }
                Tok::Punct("#") if self.is_punct_at(1, "[") || (self.is_punct_at(1, "!") && self.is_punct_at(2, "[")) => {
                    let inner = self.is_punct_at(1, "!");
                    self.bump();
                    if inner {
                        self.bump();
                        let g = self.raw_group()?;
                        self.inner_attrs.push(g[1..g.len() - 1].to_string());
                    } else {
                        let g = self.raw_group()?;
                        attrs.push(Attr::Raw(g[1..g.len() - 1].to_string()));
                    }
                    self.skip_nl_only();
                }
                _ => break,
            }
        }
        Ok((docs, attrs))
    }

    fn visibility(&mut self) -> PResult<Vis> {
        if self.is_kw("pub") {
            self.bump();
            if self.is_punct("(") {
                let g = self.raw_group()?;
                return Ok(Vis::Raw(format!("pub{}", g)));
            }
            return Ok(Vis::Public);
        }
        let item_follows = |p: &Parser, n: usize| {
            matches!(p.tok_at(n), Tok::Ident(s) if matches!(s.as_str(),
                "fun" | "fn" | "val" | "var" | "class" | "data" | "struct" | "enum" | "interface" | "trait" | "const"
                | "static" | "typealias" | "type" | "mod" | "object" | "import" | "use" | "async" | "unsafe"
                | "override" | "operator" | "mut" | "error" | "open" | "abstract" | "sealed" | "inline" | "shared" | "context"))
                || matches!(p.tok_at(n), Tok::Ident(_)) && matches!(p.tok_at(n + 1), Tok::Punct(":"))
        };
        for (kw, vis) in [("private", Vis::Private), ("internal", Vis::Internal), ("public", Vis::Public), ("protected", Vis::Internal)] {
            if self.is_kw(kw) && item_follows(self, 1) {
                self.bump();
                return Ok(vis);
            }
        }
        Ok(Vis::Default)
    }

    fn is_fun_start(&self) -> bool {
        let mut i = 0;
        loop {
            match self.tok_at(i) {
                Tok::Ident(s) => match s.as_str() {
                    "fun" | "fn" => return true,
                    "async" | "unsafe" | "const" | "static" | "mut" | "override" | "operator" | "inline" | "open"
                    | "suspend" | "abstract" | "extern" => i += 1,
                    _ => return false,
                },
                Tok::Str(_) | Tok::RawStr(_) if i > 0 && self.is_kw_at(i - 1, "extern") => i += 1,
                _ => return false,
            }
        }
    }

    /// Parses one item. Returns `Ok(None)` if the tokens don't start an item.
    fn item(&mut self) -> PResult<Option<Item>> {
        let start = self.span();
        let (docs, attrs) = self.docs_and_attrs()?;
        let mut vis = self.visibility()?;
        let context_span = self.span();
        let contexts = if self.is_context_clause() { self.context_clause()? } else { Vec::new() };
        if !contexts.is_empty() && vis == Vis::Default {
            vis = self.visibility()?;
        }
        // skip Kotlin modifiers that have no Rust meaning
        while (self.is_kw("open") || self.is_kw("final") || self.is_kw("inline") || self.is_kw("sealed") || self.is_kw("abstract"))
            && matches!(self.tok_at(1), Tok::Ident(_))
        {
            if (self.is_kw("open") || self.is_kw("abstract")) && (self.is_kw_at(1, "class") || self.is_kw_at(2, "class")) {
                let m = if self.is_kw("open") { "open" } else { "abstract" };
                self.diags.push(Diag::warning(self.span(), format!("`{}` has no effect: Oxie has no class inheritance, so no class extends another; share behavior with an interface or a field", m)));
            }
            self.bump();
        }
        let kind = if self.is_fun_start() {
            ItemKind::Fun(self.fun_decl()?)
        } else if self.is_kw("class") || self.is_kw("struct") || (self.is_kw("union") && self.is_ident_at(1)) {
            ItemKind::Type(self.class_decl(false, false)?)
        } else if self.is_kw("data") && self.is_kw_at(1, "class") {
            self.bump();
            ItemKind::Type(self.class_decl(true, false)?)
        } else if self.is_kw("shared") && self.is_kw_at(1, "class") {
            self.bump();
            let mut decl = self.class_decl(false, false)?;
            decl.shared = true;
            ItemKind::Type(decl)
        } else if self.is_kw("error") && self.is_kw_at(1, "class") {
            self.bump();
            ItemKind::Type(self.class_decl(false, true)?)
        } else if self.is_kw("enum") {
            ItemKind::Type(self.enum_decl(false)?)
        } else if self.is_kw("error") && self.is_kw_at(1, "enum") {
            self.bump();
            ItemKind::Type(self.enum_decl(true)?)
        } else if self.is_kw("interface") || self.is_kw("trait") || (self.is_kw("unsafe") && self.is_kw_at(1, "trait")) {
            ItemKind::Trait(self.trait_decl()?)
        } else if self.is_kw("impl") || (self.is_kw("unsafe") && self.is_kw_at(1, "impl")) {
            ItemKind::Impl(self.impl_decl()?)
        } else if self.is_kw("import") || self.is_kw("use") {
            ItemKind::Import(self.import_decl()?)
        } else if self.is_kw("const") || self.is_kw("val") {
            self.bump();
            ItemKind::Const(self.const_body(false)?)
        } else if self.is_kw("static") {
            self.bump();
            let m = self.eat_kw("mut") || self.eat_kw("var");
            ItemKind::Static(self.const_body(m)?)
        } else if self.is_kw("typealias") || (self.is_kw("type") && self.is_ident_at(1)) {
            self.bump();
            let name = self.ident()?;
            let generics = self.generics_opt()?;
            self.expect_punct("=")?;
            let ty = self.ty()?;
            ItemKind::TypeAlias { name, generics, ty }
        } else if self.is_kw("mod") {
            self.bump();
            let name = self.ident()?;
            if self.is_punct("{") {
                let items = self.item_block()?;
                ItemKind::Mod { name, items }
            } else {
                ItemKind::Raw(format!("mod {};", name))
            }
        } else if self.is_kw("object") && self.is_ident_at(1) {
            self.bump();
            let name = self.ident()?;
            self.skip_nl_only();
            let items = self.item_block()?;
            ItemKind::Object { name, items }
        } else if self.is_kw("test") && matches!(self.tok_at(1), Tok::Str(_)) {
            self.bump();
            ItemKind::Test(self.test_decl(false)?)
        } else if self.is_kw("async") && self.is_kw_at(1, "test") {
            self.bump();
            self.bump();
            ItemKind::Test(self.test_decl(true)?)
        } else if self.is_kw("rust") && self.is_punct_at(1, "{") {
            self.bump();
            let g = self.raw_group()?;
            ItemKind::Raw(dedent_raw(&g[1..g.len() - 1]))
        } else if self.is_kw("extern") && self.is_kw_at(1, "crate") {
            let s = self.pos;
            self.recover_stmt();
            let text = self.src[self.toks[s].span.start as usize..self.prev_span().end as usize].to_string();
            ItemKind::Raw(if text.ends_with(';') { text } else { format!("{};", text) })
        } else if self.is_kw("extern") && self.is_punct_at(1, "{")
            || self.is_kw("extern") && matches!(self.tok_at(1), Tok::Str(_) | Tok::RawStr(_)) && self.is_punct_at(2, "{")
        {
            let s = self.toks[self.pos].span.start as usize;
            self.bump();
            if !self.is_punct("{") {
                self.bump();
            }
            let g = self.raw_group()?;
            let _ = g;
            ItemKind::Raw(self.src[s..self.prev_span().end as usize].to_string())
        } else if let Some(n) = self.item_macro_path() {
            // item macro: macro_rules! name { ... } / thread_local! { ... } / a::b! { ... }
            let s = self.toks[self.pos].span.start as usize;
            for _ in 0..=n {
                self.bump();
            }
            if self.is_ident() {
                self.bump();
            }
            if !(self.is_punct("{") || self.is_punct("(") || self.is_punct("[")) {
                return self.expected("macro body");
            }
            self.raw_group()?;
            let mut text = self.src[s..self.prev_span().end as usize].to_string();
            if !text.ends_with('}') {
                text.push(';');
            }
            self.eat_punct(";");
            ItemKind::Macro(text)
        } else {
            if docs.is_empty() && attrs.is_empty() && vis == Vis::Default && contexts.is_empty() {
                return Ok(None);
            }
            return self.expected(if contexts.is_empty() { "a declaration after attributes" } else { "`fun` or `class` after `context(...)`" });
        };
        let kind = match kind {
            _ if contexts.is_empty() => kind,
            ItemKind::Fun(mut f) => {
                f.contexts = contexts;
                ItemKind::Fun(f)
            }
            ItemKind::Type(mut t) if matches!(t.kind, TypeKind::Class | TypeKind::Data | TypeKind::TupleStruct) && !t.is_error => {
                t.contexts = contexts;
                ItemKind::Type(t)
            }
            kind => {
                self.diags.push(Diag::error(context_span, "`context(...)` goes before `fun` or `class`"));
                kind
            }
        };
        self.eat_punct(";");
        Ok(Some(Item { docs, attrs, vis, kind, span: start.to(self.prev_span()) }))
    }

    /// `context(cx: &mut App)` (Kotlin's context parameters) at the start of a declaration.
    fn is_context_clause(&self) -> bool {
        self.is_kw("context") && self.is_punct_at(1, "(") && self.is_ident_at(2) && self.is_punct_at(3, ":")
    }

    /// The parameters of a `context(...)` clause; the declaration may start on the next line.
    fn context_clause(&mut self) -> PResult<Vec<Param>> {
        self.bump();
        self.expect_punct("(")?;
        let mut out: Vec<Param> = Vec::new();
        loop {
            if self.eat_punct(")") {
                break;
            }
            let span = self.span();
            let name = self.ident()?;
            self.expect_punct(":")?;
            let ty = self.ty()?;
            if self.is_punct("=") {
                let at = self.bump().span;
                self.expr()?;
                self.diags.push(Diag::error(at, "a context has no default value: each call passes its own"));
            }
            if out.iter().any(|p| p.name() == Some(name.as_str())) {
                self.diags.push(Diag::error(span, format!("`{}` is already a context here", name)));
            }
            let pat = Pat::Ident { name, by_ref: false, mutable: false, sub: None, span };
            out.push(Param { pat, ty, default: None, context: true, span: span.to(self.prev_span()) });
            if !self.eat_punct(",") {
                self.expect_punct(")")?;
                break;
            }
        }
        self.skip_nl_only();
        Ok(out)
    }

    fn item_block(&mut self) -> PResult<Vec<Item>> {
        self.expect_punct("{")?;
        let mut items = Vec::new();
        loop {
            self.skip_newlines();
            if self.eat_punct("}") {
                break;
            }
            if self.at_eof() {
                return self.expected("`}`");
            }
            let start = self.pos;
            match self.item() {
                Ok(Some(i)) => items.push(i),
                Ok(None) => {
                    if !self.global_var() {
                        let f = self.describe();
                        self.err::<()>(format!("expected a declaration, found {}", f)).ok();
                    }
                    self.recover_stmt();
                }
                Err(()) => self.recover_stmt(),
            }
            if self.pos == start {
                self.bump();
            }
        }
        Ok(items)
    }

    /// A `var` outside functions and classes (at the top level, in an `object` or a companion
    /// object): reports that Oxie has no mutable globals.
    fn global_var(&mut self) -> bool {
        if !self.is_kw("var") {
            return false;
        }
        let msg = "a `var` outside a function or class would be a mutable global, which Rust allows only in `unsafe` code: keep the value in a class, or use a `static` holding a `Mutex` or an atomic (`static COUNT: AtomicI32 = AtomicI32(0)`)";
        self.diags.push(Diag::error(self.span(), msg));
        true
    }

    fn test_decl(&mut self, is_async: bool) -> PResult<TestDecl> {
        let name = match self.bump().tok {
            Tok::Str(parts) => parts
                .iter()
                .map(|p| match p {
                    StrPart::Lit(s) => s.clone(),
                    StrPart::Interp { src, .. } => src.clone(),
                })
                .collect::<String>(),
            _ => unreachable!(),
        };
        let throws = self.eat_kw("throws");
        self.skip_nl_only();
        let body = self.block()?;
        Ok(TestDecl { name, throws, is_async, body })
    }

    fn const_body(&mut self, mutable: bool) -> PResult<ConstDecl> {
        let name = self.ident()?;
        let ty = if self.eat_punct(":") { Some(self.ty()?) } else { None };
        let value = if self.eat_punct("=") {
            self.skip_nl_only();
            Some(self.expr()?)
        } else {
            None
        };
        Ok(ConstDecl { name, mutable, ty, value })
    }

    // ------------------------------------------------------------------ functions

    fn fun_decl(&mut self) -> PResult<FunDecl> {
        let mut mods = FunModifiers::default();
        loop {
            match self.tok().clone() {
                Tok::Ident(s) => match s.as_str() {
                    "fun" | "fn" => {
                        self.bump();
                        break;
                    }
                    "async" | "suspend" => mods.is_async = true,
                    "unsafe" => mods.is_unsafe = true,
                    "const" => mods.is_const = true,
                    "static" => mods.is_static = true,
                    "mut" => mods.is_mut = true,
                    "override" => mods.is_override = true,
                    "operator" => mods.is_operator = true,
                    "inline" | "open" | "abstract" => {}
                    "extern" => {
                        self.bump();
                        if let Tok::Str(parts) = self.tok().clone() {
                            self.bump();
                            if let Some(StrPart::Lit(s)) = parts.first() {
                                mods.abi = Some(s.clone());
                            }
                        } else {
                            mods.abi = Some("C".into());
                        }
                        continue;
                    }
                    _ => return self.expected("`fun`"),
                },
                _ => return self.expected("`fun`"),
            }
            self.bump();
        }
        // Kotlin-style generics before the name: fun <T> name() / fun <T> Recv<T>.name()
        let mut generics = self.generics_opt()?;
        // receiver?
        let mut receiver = None;
        let save = self.pos;
        let save_diags = self.diags.len();
        if self.is_ident() && !(self.is_punct_at(1, "(") || (self.is_punct_at(1, "<") && !self.looks_like_receiver())) {
            if let Ok(t) = self.ty_no_dot() {
                if self.is_punct(".") {
                    self.bump();
                    receiver = Some(t);
                } else {
                    self.pos = save;
                    self.diags.truncate(save_diags);
                }
            } else {
                self.pos = save;
                self.diags.truncate(save_diags);
            }
        }
        let name_span = self.span();
        let name = self.member_name()?;
        if is_reserved(&name) && name != "new" {
            self.diags.push(Diag::error(name_span, format!("`{}` is a keyword and cannot be a function name", name)));
        }
        let more = self.generics_opt()?;
        if !more.is_empty() {
            generics.params.extend(more.params);
        }
        self.expect_punct("(")?;
        let mut self_param = None;
        let mut params = Vec::new();
        loop {
            if self.eat_punct(")") {
                break;
            }
            if params.is_empty() && self_param.is_none() {
                if let Some(sp) = self.self_param()? {
                    self_param = Some(sp);
                    if !self.eat_punct(",") {
                        self.expect_punct(")")?;
                        break;
                    }
                    continue;
                }
            }
            let pstart = self.span();
            let (_, pattrs) = self.docs_and_attrs()?;
            let _ = pattrs;
            // Kotlin `val`/`var` in params are meaningless; `vararg` unsupported
            if self.is_kw("val") {
                self.bump();
            }
            let mut pat = self.pattern_no_or()?;
            if self.eat_kw("var") {
                pat = self.pattern_no_or()?;
                if let Pat::Ident { mutable, .. } = &mut pat {
                    *mutable = true;
                }
            }
            self.expect_punct(":")?;
            let ty = self.ty()?;
            let default = if self.eat_punct("=") { Some(self.expr()?) } else { None };
            params.push(Param { pat, ty, default, context: false, span: pstart.to(self.prev_span()) });
            if !self.eat_punct(",") {
                self.expect_punct(")")?;
                break;
            }
        }
        let ret = if self.eat_punct(":") || self.eat_punct("->") { Some(self.ty()?) } else { None };
        let mut throws = Throws::No;
        if self.eat_kw("throws") {
            throws = if self.is_punct("{") || self.is_punct("=") || self.is_newline() || self.is_kw("where") || self.is_punct(";") || self.is_punct("}") {
                Throws::Any
            } else {
                Throws::Ty(self.ty()?)
            };
        }
        if self.peek_past_newlines_kw("where") {
            self.where_clause(&mut generics)?;
        }
        let body = if self.peek_past_newlines_punct("{") {
            Some(FunBody::Block(self.block()?))
        } else if self.peek_past_newlines_punct("=") {
            self.bump();
            self.skip_nl_only();
            Some(FunBody::Expr(self.expr_stmt_like()?))
        } else {
            None
        };
        Ok(FunDecl { name, name_span, mods, generics, receiver, self_param, params, contexts: Vec::new(), ret, throws, body })
    }

    /// `fun Vec<T>.x()` vs `fun foo<T>()`: scan the `<...>` and see if a `.` follows.
    fn looks_like_receiver(&self) -> bool {
        let mut depth = 0;
        let mut i = 1;
        loop {
            match self.tok_at(i) {
                Tok::Punct("<") => depth += 1,
                Tok::Punct(">") => {
                    depth -= 1;
                    if depth == 0 {
                        return matches!(self.tok_at(i + 1), Tok::Punct(".") | Tok::Punct("?"));
                    }
                }
                Tok::Eof | Tok::Newline | Tok::Punct("{") | Tok::Punct("(") => return false,
                _ => {}
            }
            i += 1;
        }
    }

    fn self_param(&mut self) -> PResult<Option<SelfParam>> {
        let is_self = |p: &Parser, n: usize| p.is_kw_at(n, "self") || p.is_kw_at(n, "this");
        if is_self(self, 0) {
            self.bump();
            if self.eat_punct(":") {
                return Ok(Some(SelfParam::Typed(self.ty()?)));
            }
            return Ok(Some(SelfParam::Value { mutable: false }));
        }
        if self.is_kw("mut") && is_self(self, 1) {
            self.bump();
            self.bump();
            return Ok(Some(SelfParam::Value { mutable: true }));
        }
        if self.is_punct("&") {
            let mut n = 1;
            let lifetime = match self.tok_at(n) {
                Tok::Lifetime(l) => {
                    n += 1;
                    Some(l.clone())
                }
                _ => None,
            };
            // `&'a self` keeps its lifetime: `self: &'a Self`
            let with_lifetime = |mutable: bool| match &lifetime {
                Some(l) => SelfParam::Typed(Type::Ref { lifetime: Some(l.clone()), mutable, inner: Box::new(Type::simple("Self")) }),
                None if mutable => SelfParam::RefMut,
                None => SelfParam::Ref,
            };
            if is_self(self, n) {
                for _ in 0..=n {
                    self.bump();
                }
                return Ok(Some(with_lifetime(false)));
            }
            if self.is_kw_at(n, "mut") && is_self(self, n + 1) {
                for _ in 0..=n + 1 {
                    self.bump();
                }
                return Ok(Some(with_lifetime(true)));
            }
        }
        Ok(None)
    }

    fn generics_opt(&mut self) -> PResult<Generics> {
        let mut g = Generics::default();
        if !self.is_punct("<") {
            return Ok(g);
        }
        self.bump();
        loop {
            if self.eat_punct(">") {
                break;
            }
            if let Tok::Lifetime(l) = self.tok().clone() {
                self.bump();
                let mut bounds = Vec::new();
                if self.eat_punct(":") {
                    loop {
                        if let Tok::Lifetime(b) = self.tok().clone() {
                            self.bump();
                            bounds.push(format!("'{}", b));
                        }
                        if !self.eat_punct("+") {
                            break;
                        }
                    }
                }
                g.params.push(GenericParam::Lifetime { name: l, bounds });
            } else if self.eat_kw("const") {
                let name = self.ident()?;
                self.expect_punct(":")?;
                let ty = self.ty()?;
                let default = if self.eat_punct("=") { Some(self.raw_until_generic_end()?) } else { None };
                g.params.push(GenericParam::Const { name, ty, default });
            } else {
                // Kotlin variance annotations
                if (self.is_kw("out") || self.is_kw("in") || self.is_kw("reified")) && self.is_ident_at(1) {
                    self.bump();
                }
                let name = self.ident()?;
                let bounds = if self.eat_punct(":") { self.bounds()? } else { Vec::new() };
                let default = if self.eat_punct("=") { Some(self.ty()?) } else { None };
                g.params.push(GenericParam::Type { name, bounds, default });
            }
            if !self.eat_punct(",") {
                self.expect_punct(">")?;
                break;
            }
        }
        Ok(g)
    }

    fn raw_until_generic_end(&mut self) -> PResult<String> {
        if self.is_punct("{") {
            return self.raw_group();
        }
        let t = self.bump();
        Ok(self.src[t.span.start as usize..t.span.end as usize].to_string())
    }

    fn where_clause(&mut self, g: &mut Generics) -> PResult<()> {
        self.eat_kw("where");
        loop {
            self.skip_nl_only();
            if let Tok::Lifetime(l) = self.tok().clone() {
                self.bump();
                self.expect_punct(":")?;
                let mut bounds = Vec::new();
                loop {
                    if let Tok::Lifetime(b) = self.tok().clone() {
                        self.bump();
                        bounds.push(format!("'{}", b));
                    }
                    if !self.eat_punct("+") {
                        break;
                    }
                }
                g.where_clause.push(WherePred::Lifetime { name: l, bounds });
            } else {
                let ty = self.ty()?;
                self.expect_punct(":")?;
                let bounds = self.bounds()?;
                g.where_clause.push(WherePred::Type { ty, bounds });
            }
            if !self.eat_punct(",") {
                break;
            }
            if self.is_punct("{") || self.is_punct("=") {
                break;
            }
        }
        Ok(())
    }

    fn bounds(&mut self) -> PResult<Vec<Type>> {
        let mut out = Vec::new();
        loop {
            if let Tok::Lifetime(l) = self.tok().clone() {
                self.bump();
                out.push(Type::Lifetime(l));
            } else if self.eat_punct("?") {
                out.push(Type::Maybe(Box::new(self.ty_path()?)));
            } else if self.is_punct("(") {
                self.bump();
                let t = self.ty()?;
                self.expect_punct(")")?;
                out.push(t);
            } else if self.is_kw("for") && self.is_punct_at(1, "<") {
                // higher-ranked bound: keep raw
                let s = self.toks[self.pos].span.start as usize;
                self.bump();
                self.generics_opt()?;
                self.ty_path()?;
                out.push(Type::Raw(self.src[s..self.prev_span().end as usize].to_string()));
            } else {
                out.push(self.ty_path()?);
            }
            if !self.eat_punct("+") {
                break;
            }
        }
        Ok(out)
    }

    // ------------------------------------------------------------------ types

    pub fn ty(&mut self) -> PResult<Type> {
        let t = self.ty_inner(true)?;
        Ok(t)
    }

    fn ty_no_dot(&mut self) -> PResult<Type> {
        self.ty_inner(false)
    }

    fn ty_inner(&mut self, dots: bool) -> PResult<Type> {
        let mut t = self.ty_base(dots)?;
        while self.is_punct("?") && !self.is_punct_at(1, ".") {
            self.bump();
            t = Type::Option(Box::new(t));
        }
        Ok(t)
    }

    fn ty_base(&mut self, dots: bool) -> PResult<Type> {
        match self.tok().clone() {
            Tok::Punct("&") | Tok::Punct("&&") => {
                let double = self.is_punct("&&");
                self.bump();
                let lifetime = if let Tok::Lifetime(l) = self.tok().clone() {
                    self.bump();
                    Some(l)
                } else {
                    None
                };
                let mutable = self.eat_kw("mut");
                let inner = self.ty_base(dots)?;
                let inner = if self.is_punct("?") && !self.is_punct_at(1, ".") {
                    // `&T?` means Option<&T>: handled by caller; keep &T here
                    inner
                } else {
                    inner
                };
                let r = Type::Ref { lifetime, mutable, inner: Box::new(inner) };
                Ok(if double { Type::Ref { lifetime: None, mutable: false, inner: Box::new(r) } } else { r })
            }
            Tok::Punct("*") => {
                self.bump();
                let mutable = if self.eat_kw("mut") {
                    true
                } else {
                    self.eat_kw("const");
                    false
                };
                Ok(Type::Ptr { mutable, inner: Box::new(self.ty_base(dots)?) })
            }
            Tok::Punct("(") => {
                self.bump();
                let mut elems = Vec::new();
                let mut trailing_comma = false;
                // inside brackets, `->` is a return type again
                let fn_ret = std::mem::replace(&mut self.fn_ret, true);
                loop {
                    if self.eat_punct(")") {
                        break;
                    }
                    elems.push(self.ty()?);
                    trailing_comma = false;
                    if self.eat_punct(",") {
                        trailing_comma = true;
                    } else {
                        self.expect_punct(")")?;
                        break;
                    }
                }
                self.fn_ret = fn_ret;
                if self.fn_ret && self.eat_punct("->") {
                    let ret = self.ty()?;
                    return Ok(Type::Fn { params: elems, ret: Box::new(ret) });
                }
                if elems.len() == 1 && !trailing_comma {
                    return Ok(elems.pop().unwrap());
                }
                Ok(Type::Tuple(elems))
            }
            Tok::Punct("[") => {
                self.bump();
                let inner = self.ty()?;
                if self.eat_punct(";") {
                    let s = self.toks[self.pos].span.start as usize;
                    let mut depth = 0;
                    while !(depth == 0 && self.is_punct("]")) && !self.at_eof() {
                        if self.is_punct("[") {
                            depth += 1;
                        }
                        if self.is_punct("]") {
                            depth -= 1;
                        }
                        self.bump();
                    }
                    let len = self.src[s..self.span().start as usize].trim().to_string();
                    self.expect_punct("]")?;
                    return Ok(Type::Array(Box::new(inner), len));
                }
                self.expect_punct("]")?;
                Ok(Type::Slice(Box::new(inner)))
            }
            Tok::Punct("!") => {
                self.bump();
                Ok(Type::Never)
            }
            Tok::Punct("<") => {
                self.bump();
                let ty = self.ty()?;
                let trait_ = if self.eat_kw("as") { Some(Box::new(self.ty_path()?)) } else { None };
                self.expect_punct(">")?;
                let mut rest = Vec::new();
                while self.eat_punct("::") {
                    rest.push(self.path_seg(true, false)?);
                }
                Ok(Type::Qualified { ty: Box::new(ty), trait_, rest })
            }
            Tok::Lifetime(l) => {
                self.bump();
                Ok(Type::Lifetime(l))
            }
            Tok::Ident(s) if s == "dyn" => {
                self.bump();
                Ok(Type::Dyn(self.bounds()?))
            }
            Tok::Ident(s) if s == "impl" => {
                self.bump();
                Ok(Type::Impl(self.bounds()?))
            }
            Tok::Ident(s) if s == "_" => {
                self.bump();
                Ok(Type::Infer)
            }
            Tok::Ident(s) if s == "fn" || s == "unsafe" || s == "extern" => {
                let start = self.toks[self.pos].span.start as usize;
                while !self.is_punct("(") && !self.at_eof() {
                    self.bump();
                }
                self.raw_group()?;
                if self.fn_ret && self.eat_punct("->") {
                    self.ty()?;
                }
                Ok(Type::Raw(self.src[start..self.prev_span().end as usize].to_string()))
            }
            Tok::Ident(_) | Tok::RawIdent(_) | Tok::Punct("::") => self.ty_path_dots(dots),
            _ => self.expected("type"),
        }
    }

    fn ty_path(&mut self) -> PResult<Type> {
        self.ty_path_dots(true)
    }

    fn ty_path_dots(&mut self, dots: bool) -> PResult<Type> {
        let start = self.span();
        let cast = std::mem::take(&mut self.cast_ty);
        let global = self.eat_punct("::");
        let mut segs = vec![self.path_seg(false, cast)?];
        loop {
            let dot_path = dots && self.is_punct(".") && matches!(self.tok_at(1), Tok::Ident(_)) && !self.is_punct_at(2, "(");
            if self.is_punct("::") || dot_path {
                self.bump();
                segs.push(self.path_seg(true, cast)?);
            } else {
                break;
            }
        }
        Ok(Type::Path { global, segs, span: start.to(self.prev_span()) })
    }

    /// `cast`: a segment of the path after `as` (see `cast_ty`).
    fn path_seg(&mut self, _after_sep: bool, cast: bool) -> PResult<PathSeg> {
        let name = self.member_name()?;
        let mut seg = PathSeg::plain(name);
        if cast && self.is_punct("<") {
            let save = self.pos;
            let nd = self.diags.len();
            match self.generic_args() {
                Ok(g) => seg.generics = Some(g),
                Err(()) => {
                    self.pos = save;
                    self.diags.truncate(nd);
                }
            }
        } else if self.is_punct("<") || (self.is_punct("::") && self.is_punct_at(1, "<")) {
            if self.is_punct("::") {
                self.bump();
            }
            seg.generics = Some(self.generic_args()?);
        } else if self.is_punct("(") && matches!(seg.name.as_str(), "Fn" | "FnMut" | "FnOnce") {
            self.bump();
            let mut params = Vec::new();
            let fn_ret = std::mem::replace(&mut self.fn_ret, true);
            loop {
                if self.eat_punct(")") {
                    break;
                }
                params.push(self.ty()?);
                if !self.eat_punct(",") {
                    self.expect_punct(")")?;
                    break;
                }
            }
            self.fn_ret = fn_ret;
            let ret = if self.fn_ret && self.eat_punct("->") { Some(Box::new(self.ty()?)) } else { None };
            seg.fn_sugar = Some((params, ret));
        }
        Ok(seg)
    }

    fn generic_args(&mut self) -> PResult<Vec<GenericArg>> {
        let fn_ret = std::mem::replace(&mut self.fn_ret, true);
        let r = self.generic_args_inner();
        self.fn_ret = fn_ret;
        r
    }

    fn generic_args_inner(&mut self) -> PResult<Vec<GenericArg>> {
        self.expect_punct("<")?;
        let mut args = Vec::new();
        loop {
            if self.eat_punct(">") {
                break;
            }
            match self.tok().clone() {
                Tok::Lifetime(l) => {
                    self.bump();
                    args.push(GenericArg::Lifetime(l));
                }
                Tok::Ident(n) if self.is_punct_at(1, "=") && !self.is_punct_at(1, "==") => {
                    self.bump();
                    self.bump();
                    args.push(GenericArg::Binding(n, self.ty()?));
                }
                Tok::Int(n) => {
                    self.bump();
                    args.push(GenericArg::Const(n));
                }
                Tok::Punct("{") => {
                    let g = self.raw_group()?;
                    args.push(GenericArg::Const(g));
                }
                Tok::Punct("-") => {
                    self.bump();
                    let t = self.bump();
                    args.push(GenericArg::Const(format!("-{}", &self.src[t.span.start as usize..t.span.end as usize])));
                }
                _ => args.push(GenericArg::Type(self.ty()?)),
            }
            if !self.eat_punct(",") {
                self.expect_punct(">")?;
                break;
            }
        }
        Ok(args)
    }

    // ------------------------------------------------------------------ type declarations

    fn supertypes(&mut self) -> PResult<Vec<Type>> {
        let mut out = Vec::new();
        if self.eat_punct(":") {
            loop {
                self.skip_nl_only();
                // `interface Ext : IntoElement + 'static`, a lifetime bound as in Rust
                if let Tok::Lifetime(l) = self.tok().clone() {
                    self.bump();
                    out.push(Type::Lifetime(l));
                    if !(self.eat_punct(",") || self.eat_punct("+")) {
                        break;
                    }
                    continue;
                }
                let start = self.span();
                let t = self.ty_path()?;
                // Kotlin's `Base(name)` super constructor call (reported, and the supertype left out)
                if self.is_punct("(") {
                    self.raw_group()?;
                    let name = t.last_name().unwrap_or("it").to_string();
                    self.diags.push(Diag::error(start.to(self.prev_span()), crate::diag::no_inheritance(&name)));
                } else {
                    out.push(t);
                }
                if !(self.eat_punct(",") || self.eat_punct("+")) {
                    break;
                }
            }
        }
        Ok(out)
    }

    fn class_decl(&mut self, is_data: bool, is_error: bool) -> PResult<TypeDecl> {
        let is_union = self.is_kw("union");
        let is_struct = self.is_kw("struct") || is_union;
        self.bump(); // class / struct / union
        let name_span = self.span();
        let name = self.ident()?;
        let mut generics = self.generics_opt()?;
        let mut decl = TypeDecl {
            kind: if is_data { TypeKind::Data } else { TypeKind::Class },
            is_error,
            name,
            name_span,
            generics: Generics::default(),
            fields: Vec::new(),
            tuple_fields: Vec::new(),
            variants: Vec::new(),
            supertypes: Vec::new(),
            members: Vec::new(),
            init: None,
            message: None,
            shared: false,
            contexts: Vec::new(),
        };
        if self.is_punct("(") {
            // Kotlin primary constructor or Rust tuple struct
            let tuple_like = is_struct && !(self.is_ident_at(1) && self.is_punct_at(2, ":"))
                && !self.is_kw_at(1, "val") && !self.is_kw_at(1, "var") && !self.is_punct_at(1, ")");
            if tuple_like {
                self.bump();
                decl.kind = TypeKind::TupleStruct;
                loop {
                    if self.eat_punct(")") {
                        break;
                    }
                    let (_, _) = self.docs_and_attrs()?;
                    let vis = self.visibility()?;
                    let ty = self.ty()?;
                    decl.tuple_fields.push((vis, ty));
                    if !self.eat_punct(",") {
                        self.expect_punct(")")?;
                        break;
                    }
                }
            } else {
                self.bump();
                loop {
                    if self.eat_punct(")") {
                        break;
                    }
                    let fstart = self.span();
                    let (docs, attrs) = self.docs_and_attrs()?;
                    let vis = self.visibility()?;
                    if !self.eat_kw("val") {
                        self.eat_kw("var");
                    }
                    let name = self.member_name()?;
                    self.expect_punct(":")?;
                    let ty = self.ty()?;
                    let default = if self.eat_punct("=") { Some(self.expr()?) } else { None };
                    // a field that exists only in some builds (`@cfg(test)`) can't be an argument
                    let in_ctor = !attrs.iter().any(|a| a.name() == "cfg");
                    decl.fields.push(Field { docs, attrs, vis, name, ty, default, in_ctor, span: fstart.to(self.prev_span()) });
                    if !self.eat_punct(",") {
                        self.expect_punct(")")?;
                        break;
                    }
                }
            }
        } else if is_struct && !self.is_punct("{") && !self.is_punct(":") && !self.is_kw("where") {
            // unit struct
            decl.kind = TypeKind::TupleStruct;
        }
        decl.supertypes = self.supertypes()?;
        if self.peek_past_newlines_kw("where") {
            self.where_clause(&mut generics)?;
        }
        decl.generics = generics;
        if is_error && self.eat_punct("=") {
            decl.message = Some(self.message()?);
        }
        if self.peek_past_newlines_punct("{") {
            if is_struct && decl.fields.is_empty() && decl.kind != TypeKind::TupleStruct {
                self.rust_struct_fields(&mut decl)?;
            } else {
                self.class_body(&mut decl)?;
            }
        }
        if is_error && decl.message.is_none() && self.eat_punct("=") {
            decl.message = Some(self.message()?);
        }
        if is_union {
            decl.kind = TypeKind::Union;
        }
        Ok(decl)
    }

    /// `struct S { a: T, pub b: U }` - also allows methods mixed in.
    fn rust_struct_fields(&mut self, decl: &mut TypeDecl) -> PResult<()> {
        self.expect_punct("{")?;
        loop {
            self.skip_newlines();
            while self.eat_punct(",") {
                self.skip_newlines();
            }
            if self.eat_punct("}") {
                break;
            }
            if self.at_eof() {
                return self.expected("`}`");
            }
            let fstart = self.span();
            let save = self.pos;
            let (docs, attrs) = self.docs_and_attrs()?;
            let vis = self.visibility()?;
            if self.is_fun_start() || self.is_kw("init") {
                self.pos = save;
                self.class_member(decl)?;
                continue;
            }
            if self.is_kw("val") || self.is_kw("var") {
                self.bump();
            }
            let name = self.member_name()?;
            self.expect_punct(":")?;
            let ty = self.ty()?;
            let default = if self.eat_punct("=") { Some(self.expr()?) } else { None };
            let in_ctor = !attrs.iter().any(|a| a.name() == "cfg");
            decl.fields.push(Field { docs, attrs, vis, name, ty, default, in_ctor, span: fstart.to(self.prev_span()) });
        }
        Ok(())
    }

    fn class_body(&mut self, decl: &mut TypeDecl) -> PResult<()> {
        self.expect_punct("{")?;
        loop {
            self.skip_newlines();
            if self.eat_punct("}") {
                break;
            }
            if self.at_eof() {
                return self.expected("`}`");
            }
            let start = self.pos;
            if self.class_member(decl).is_err() {
                self.recover_stmt();
            }
            if self.pos == start {
                self.bump();
            }
        }
        Ok(())
    }

    fn class_member(&mut self, decl: &mut TypeDecl) -> PResult<()> {
        let start = self.span();
        let save = self.pos;
        let (docs, attrs) = self.docs_and_attrs()?;
        let vis = self.visibility()?;
        if self.is_kw("init") && self.is_punct_at(1, "{") {
            self.bump();
            decl.init = Some(self.block()?);
            return Ok(());
        }
        if self.is_kw("companion") && self.is_kw_at(1, "object") {
            // companion object { ... } -> static members
            self.bump();
            self.bump();
            if self.is_ident() {
                self.bump();
            }
            let items = self.item_block()?;
            for mut it in items {
                if let ItemKind::Fun(f) = &mut it.kind {
                    f.mods.is_static = true;
                }
                if let ItemKind::Const(_) = it.kind {}
                decl.members.push(it);
            }
            return Ok(());
        }
        if (self.is_kw("val") || self.is_kw("var")) && !self.is_fun_start() {
            let is_override = false;
            let _ = is_override;
            self.bump();
            let name = self.member_name()?;
            let ty = if self.eat_punct(":") { self.ty()? } else { Type::Infer };
            let default = if self.eat_punct("=") {
                self.skip_nl_only();
                Some(self.expr()?)
            } else {
                None
            };
            decl.fields.push(Field { docs, attrs, vis, name, ty, default, in_ctor: false, span: start.to(self.prev_span()) });
            return Ok(());
        }
        self.pos = save;
        match self.item()? {
            Some(item) => decl.members.push(item),
            None => {
                let f = self.describe();
                return self.err(format!("expected a property, method or `init` block, found {}", f));
            }
        }
        Ok(())
    }

    fn enum_decl(&mut self, is_error: bool) -> PResult<TypeDecl> {
        self.bump(); // enum
        // Kotlin's `enum class Color`
        if self.is_kw("class") && self.is_ident_at(1) {
            self.bump();
        }
        let name_span = self.span();
        let name = self.ident()?;
        let mut generics = self.generics_opt()?;
        let supertypes = self.supertypes()?;
        if self.peek_past_newlines_kw("where") {
            self.where_clause(&mut generics)?;
        }
        let mut decl = TypeDecl {
            kind: TypeKind::Enum,
            is_error,
            name,
            name_span,
            generics,
            fields: Vec::new(),
            tuple_fields: Vec::new(),
            variants: Vec::new(),
            supertypes,
            members: Vec::new(),
            init: None,
            message: None,
            shared: false,
            contexts: Vec::new(),
        };
        self.skip_nl_only();
        self.expect_punct("{")?;
        loop {
            self.skip_newlines();
            while self.eat_punct(",") {
                self.skip_newlines();
            }
            if self.eat_punct("}") {
                break;
            }
            if self.at_eof() {
                return self.expected("`}`");
            }
            let start_pos = self.pos;
            let vstart = self.span();
            let (docs, attrs) = self.docs_and_attrs()?;
            let is_variant = match self.tok() {
                Tok::Ident(s) => !is_reserved(s) && !self.is_fun_start() && !matches!(s.as_str(), "private" | "internal" | "public" | "type" | "companion")
                    || (s == "type" && !self.is_ident_at(1)),
                Tok::RawIdent(_) => true,
                _ => false,
            };
            if !is_variant {
                self.pos = start_pos;
                let r = self.class_member(&mut decl);
                if r.is_err() {
                    self.recover_stmt();
                }
                if self.pos == start_pos {
                    self.bump();
                }
                continue;
            }
            let name = self.ident()?;
            let mut fields = VariantFields::Unit;
            if self.is_punct("(") {
                let named = self.is_ident_at(1) && self.is_punct_at(2, ":") || self.is_kw_at(1, "val");
                self.bump();
                if named {
                    let mut fs = Vec::new();
                    loop {
                        if self.eat_punct(")") {
                            break;
                        }
                        let fstart = self.span();
                        let (fd, fa) = self.docs_and_attrs()?;
                        self.eat_kw("val");
                        let n = self.member_name()?;
                        self.expect_punct(":")?;
                        let ty = self.ty()?;
                        fs.push(Field { docs: fd, attrs: fa, vis: Vis::Default, name: n, ty, default: None, in_ctor: true, span: fstart.to(self.prev_span()) });
                        if !self.eat_punct(",") {
                            self.expect_punct(")")?;
                            break;
                        }
                    }
                    fields = VariantFields::Named(fs);
                } else {
                    let mut ts = Vec::new();
                    loop {
                        if self.eat_punct(")") {
                            break;
                        }
                        ts.push(self.ty()?);
                        if !self.eat_punct(",") {
                            self.expect_punct(")")?;
                            break;
                        }
                    }
                    fields = VariantFields::Tuple(ts);
                }
            } else if self.is_punct("{") {
                self.bump();
                let mut fs = Vec::new();
                loop {
                    self.skip_newlines();
                    if self.eat_punct("}") {
                        break;
                    }
                    let fstart = self.span();
                    let (fd, fa) = self.docs_and_attrs()?;
                    let n = self.member_name()?;
                    self.expect_punct(":")?;
                    let ty = self.ty()?;
                    fs.push(Field { docs: fd, attrs: fa, vis: Vis::Default, name: n, ty, default: None, in_ctor: true, span: fstart.to(self.prev_span()) });
                    self.skip_newlines();
                    if !self.eat_punct(",") {
                        self.skip_newlines();
                        self.expect_punct("}")?;
                        break;
                    }
                }
                fields = VariantFields::Named(fs);
            }
            let mut discriminant = None;
            let mut message = None;
            if self.eat_punct("=") {
                let e = if is_error { self.message()? } else { self.expr()? };
                if is_error {
                    message = Some(e);
                } else {
                    discriminant = Some(e);
                }
            }
            decl.variants.push(Variant { docs, attrs, name, fields, discriminant, message, span: vstart.to(self.prev_span()) });
            if !(self.eat_punct(",") || self.is_newline() || self.is_punct("}") || self.is_punct(";")) {
                let f = self.describe();
                self.err::<()>(format!("expected `,` or a new line after enum variant, found {}", f)).ok();
                self.recover_stmt();
            }
        }
        Ok(decl)
    }

    fn trait_decl(&mut self) -> PResult<TraitDecl> {
        let is_unsafe = self.eat_kw("unsafe");
        self.bump();
        let name_span = self.span();
        let name = self.ident()?;
        let mut generics = self.generics_opt()?;
        let supertraits = self.supertypes()?;
        if self.peek_past_newlines_kw("where") {
            self.where_clause(&mut generics)?;
        }
        // `interface Marker`: no members
        let members = if self.peek_past_newlines_punct("{") { self.trait_body()? } else { Vec::new() };
        Ok(TraitDecl { name, name_span, generics, supertraits, members, is_unsafe })
    }

    /// Body of an interface/impl: functions, `type X = ...`, `const X: T = ...`.
    fn trait_body(&mut self) -> PResult<Vec<Item>> {
        self.expect_punct("{")?;
        let mut items = Vec::new();
        loop {
            self.skip_newlines();
            if self.eat_punct("}") {
                break;
            }
            if self.at_eof() {
                return self.expected("`}`");
            }
            let start = self.pos;
            let r: PResult<()> = (|| {
                let istart = self.span();
                let save = self.pos;
                let (docs, attrs) = self.docs_and_attrs()?;
                let vis = self.visibility()?;
                if self.is_kw("type") && self.is_ident_at(1) {
                    self.bump();
                    let name = self.ident()?;
                    let generics = self.generics_opt()?;
                    let mut bounds = Vec::new();
                    if self.eat_punct(":") {
                        bounds = self.bounds()?;
                    }
                    let mut generics = generics;
                    // generic associated types: `type Item<'a> where Self: 'a`
                    if self.is_kw("where") {
                        self.where_clause(&mut generics)?;
                    }
                    let ty = if self.eat_punct("=") { self.ty()? } else { Type::Bounds(bounds) };
                    if self.is_kw("where") {
                        self.where_clause(&mut generics)?;
                    }
                    items.push(Item { docs, attrs, vis, kind: ItemKind::TypeAlias { name, generics, ty }, span: istart.to(self.prev_span()) });
                    return Ok(());
                }
                if self.is_kw("const") {
                    self.bump();
                    let c = self.const_body(false)?;
                    items.push(Item { docs, attrs, vis, kind: ItemKind::Const(c), span: istart.to(self.prev_span()) });
                    return Ok(());
                }
                // Kotlin's abstract property: `val name: String`
                if (self.is_kw("val") || self.is_kw("var")) && self.is_ident_at(1) {
                    let at = self.span();
                    let prop = match self.tok_at(1) {
                        Tok::Ident(n) => n.clone(),
                        _ => String::new(),
                    };
                    let mut i = 2;
                    while !matches!(self.tok_at(i), Tok::Newline | Tok::Eof | Tok::Punct("}") | Tok::Punct("=") | Tok::Punct(";")) {
                        i += 1;
                    }
                    if self.is_kw("var") || !self.is_punct_at(i, "=") {
                        let ty = self.src.get(self.toks[self.pos + 1].span.end as usize..self.toks[self.pos + i - 1].span.end as usize).unwrap_or("").trim().trim_start_matches(':').trim().to_string();
                        let ty = if ty.is_empty() { "T".to_string() } else { ty };
                        self.diags.push(Diag::error(at, format!("an interface has no properties: declare `fun {}(): {}`, which each class implements (`override fun {}() = {}`)", prop, ty, prop, prop)));
                        self.recover_stmt();
                        return Ok(());
                    }
                }
                self.pos = save;
                match self.item()? {
                    Some(it) => items.push(it),
                    None => {
                        let f = self.describe();
                        return self.err(format!("expected `fun`, `type` or `const`, found {}", f));
                    }
                }
                Ok(())
            })();
            if r.is_err() {
                self.recover_stmt();
            }
            if self.pos == start {
                self.bump();
            }
        }
        Ok(items)
    }

    fn impl_decl(&mut self) -> PResult<ImplDecl> {
        let is_unsafe = self.eat_kw("unsafe");
        self.bump(); // impl
        let mut generics = self.generics_opt()?;
        let negative = self.eat_punct("!");
        let first = self.ty()?;
        let (trait_, self_ty) = if self.eat_kw("for") { (Some(first), self.ty()?) } else { (None, first) };
        if self.peek_past_newlines_kw("where") {
            self.where_clause(&mut generics)?;
        }
        self.skip_nl_only();
        let members = self.trait_body()?;
        Ok(ImplDecl { generics, trait_, negative, self_ty, members, is_unsafe })
    }

    fn import_decl(&mut self) -> PResult<ImportDecl> {
        self.bump(); // import / use
        // TS style: import { A, B as C } from path
        if self.is_punct("{") {
            let items = self.use_group()?;
            if !self.eat_kw("from") {
                return self.expected("`from`");
            }
            let segments = self.use_path_segments()?;
            return Ok(ImportDecl { tree: UseTree::Group { segments, items } });
        }
        let tree = self.use_tree()?;
        Ok(ImportDecl { tree })
    }

    fn use_path_segments(&mut self) -> PResult<Vec<String>> {
        let mut segs = Vec::new();
        if let Tok::Str(parts) = self.tok().clone() {
            // import { x } from "./module" or "std/collections"
            self.bump();
            let s: String = parts.iter().map(|p| if let StrPart::Lit(s) = p { s.clone() } else { String::new() }).collect();
            for seg in s.split(['/', ':', '.']) {
                if !seg.is_empty() {
                    segs.push(seg.to_string());
                }
            }
            return Ok(segs);
        }
        if self.eat_punct("::") {
            segs.push(String::new());
        }
        segs.push(self.member_name()?);
        while (self.is_punct("::") || self.is_punct(".")) && matches!(self.tok_at(1), Tok::Ident(_) | Tok::RawIdent(_)) {
            self.bump();
            segs.push(self.member_name()?);
        }
        Ok(segs)
    }

    fn use_tree(&mut self) -> PResult<UseTree> {
        if self.is_punct("{") {
            let items = self.use_group()?;
            return Ok(UseTree::Group { segments: Vec::new(), items });
        }
        let mut segments = Vec::new();
        if self.eat_punct("::") {
            segments.push(String::new());
        }
        loop {
            if self.eat_punct("*") {
                return Ok(UseTree::Glob { segments });
            }
            if self.is_punct("{") {
                let items = self.use_group()?;
                return Ok(UseTree::Group { segments, items });
            }
            segments.push(self.member_name()?);
            if self.is_punct("::") || self.is_punct(".") {
                self.bump();
                continue;
            }
            break;
        }
        let alias = if self.eat_kw("as") { Some(self.member_name()?) } else { None };
        Ok(UseTree::Path { segments, alias })
    }

    fn use_group(&mut self) -> PResult<Vec<UseTree>> {
        self.expect_punct("{")?;
        let mut items = Vec::new();
        loop {
            self.skip_newlines();
            if self.eat_punct("}") {
                break;
            }
            items.push(self.use_tree()?);
            self.skip_newlines();
            if !self.eat_punct(",") {
                self.skip_newlines();
                self.expect_punct("}")?;
                break;
            }
        }
        Ok(items)
    }

    // ------------------------------------------------------------------ blocks & statements

    pub fn block(&mut self) -> PResult<Block> {
        let start = self.expect_punct("{")?;
        let stmts = self.stmts_until_close()?;
        Ok(Block { stmts, span: start.to(self.prev_span()) })
    }

    /// Statements up to and including the closing `}`.
    fn stmts_until_close(&mut self) -> PResult<Vec<Stmt>> {
        let saved = self.no_struct;
        self.no_struct = false;
        let mut stmts = Vec::new();
        loop {
            self.skip_nl_only();
            while self.eat_punct(";") {
                self.skip_nl_only();
            }
            if self.eat_punct("}") {
                break;
            }
            if self.at_eof() {
                self.no_struct = saved;
                return self.expected("`}`");
            }
            let start = self.pos;
            match self.stmt() {
                Ok(s) => {
                    let mut s = s;
                    if self.is_punct(";") {
                        self.bump();
                        if let Stmt::Expr { semi, .. } = &mut s {
                            *semi = true;
                        }
                    } else if !(self.is_newline() || self.is_punct("}") || self.at_eof()) {
                        let f = self.describe();
                        let sp = self.span();
                        self.diags.push(Diag::error(sp, format!("expected end of statement, found {}", f)).with_help("put each statement on its own line or separate them with `;`"));
                        self.recover_stmt();
                    }
                    stmts.push(s);
                }
                Err(()) => self.recover_stmt(),
            }
            if self.pos == start {
                self.bump();
            }
        }
        self.no_struct = saved;
        // `{ ... }` as a statement is a lambda (as in Kotlin), and one that isn't the block's value never runs
        for (i, s) in stmts.iter().enumerate() {
            if let Stmt::Expr { expr, semi } = s {
                let unused = i + 1 < stmts.len() || *semi;
                let bare = matches!(&expr.kind, ExprKind::Lambda { params, .. } if params.is_empty());
                if unused && bare && self.src.as_bytes().get(expr.span.start as usize) == Some(&b'{') {
                    self.diags.push(Diag::error(expr.span, "this `{ }` is a lambda, and it is never called, so its statements never run").with_help("to run the statements as a block, write `run { ... }`"));
                }
            }
        }
        Ok(stmts)
    }

    /// The number of tokens of the path before `!` when an item macro call starts here
    /// (`thread_local!`, `wasmtime::component::bindgen!`).
    fn item_macro_path(&self) -> Option<usize> {
        let mut i = 0;
        loop {
            if !matches!(self.tok_at(i), Tok::Ident(_)) {
                return None;
            }
            i += 1;
            if self.is_punct_at(i, "::") {
                i += 1;
                continue;
            }
            return (self.is_punct_at(i, "!") && !self.is_punct_at(i + 1, "=")).then_some(i);
        }
    }

    fn is_item_start(&self) -> bool {
        if self.is_fun_start() {
            // `fun` local function - but not a lambda like `fun(x)`?
            return true;
        }
        match self.tok() {
            Tok::Ident(s) => match s.as_str() {
                "class" | "struct" | "interface" | "trait" | "impl" | "import" | "use" | "typealias" | "mod" => true,
                "const" => !self.is_punct_at(1, "{"),
                "union" => self.is_ident_at(1),
                "enum" => self.is_ident_at(1),
                "data" | "error" => self.is_kw_at(1, "class") || self.is_kw_at(1, "enum"),
                "shared" => self.is_kw_at(1, "class"),
                "static" => self.is_ident_at(1) || self.is_kw_at(1, "mut"),
                "type" => self.is_ident_at(1) && (self.is_punct_at(2, "=") || self.is_punct_at(2, "<")),
                "object" => self.is_ident_at(1) && self.is_punct_at(2, "{"),
                "unsafe" => self.is_kw_at(1, "impl") || self.is_kw_at(1, "trait") || self.is_kw_at(1, "interface"),
                _ => false,
            },
            Tok::Punct("@") => true,
            Tok::Punct("#") => self.is_punct_at(1, "["),
            Tok::DocOuter(_) => true,
            _ => false,
        }
    }

    /// Attributes before a `when` arm, as Rust attribute contents.
    fn arm_attrs(&mut self) -> PResult<Vec<String>> {
        if !(self.is_punct("@") || (self.is_punct("#") && self.is_punct_at(1, "["))) {
            return Ok(Vec::new());
        }
        let (_, attrs) = self.docs_and_attrs()?;
        self.skip_nl_only();
        Ok(attrs
            .iter()
            .map(|a| match a {
                Attr::Oxie { name, args: Some(a), .. } => format!("{}({})", name, a),
                Attr::Oxie { name, args: None, .. } => name.clone(),
                Attr::Raw(r) => r.clone(),
            })
            .collect())
    }

    fn stmt(&mut self) -> PResult<Stmt> {
        // a doc comment on a local (`/// the step` before `val step = 2`) is only a comment: items
        // keep theirs
        let doc_start = self.pos;
        if matches!(self.tok(), Tok::DocOuter(_)) {
            while matches!(self.tok(), Tok::DocOuter(_) | Tok::Newline) {
                self.bump();
            }
            let attrs = self.is_punct("@") || (self.is_punct("#") && self.is_punct_at(1, "["));
            if !attrs && (self.is_item_start() || matches!(self.tok(), Tok::Ident(s) if matches!(s.as_str(), "pub" | "private" | "internal" | "public"))) {
                self.pos = doc_start;
            }
        }
        // attributes on a statement (`@cfg(debug_assertions) println(...)`, `#[allow(...)] val x = ...`)
        if self.is_punct("@") || (self.is_punct("#") && self.is_punct_at(1, "[")) {
            let save = doc_start;
            let nd = self.diags.len();
            let (_, attrs) = self.docs_and_attrs()?;
            let declaration = self.is_item_start() || self.is_fun_start() || matches!(self.tok(), Tok::Ident(s) if matches!(s.as_str(), "pub" | "private" | "internal" | "public"));
            if !declaration && !attrs.is_empty() {
                let texts = attrs
                    .iter()
                    .map(|a| match a {
                        Attr::Oxie { name, args: Some(a), .. } => format!("{}({})", name, a),
                        Attr::Oxie { name, args: None, .. } => name.clone(),
                        Attr::Raw(r) => r.clone(),
                    })
                    .collect();
                let inner = self.stmt()?;
                return Ok(Stmt::Attributed { attrs: texts, stmt: Box::new(inner) });
            }
            self.pos = save;
            self.diags.truncate(nd);
        }
        let start = self.span();
        if self.is_kw("val") || self.is_kw("var") || self.is_kw("let") {
            let kw = self.bump();
            let mut mutable = matches!(kw.tok, Tok::Ident(ref s) if s == "var");
            if self.eat_kw("mut") {
                mutable = true;
            }
            let (mut pat, typed) = match self.typed_tuple_pattern() {
                Some((p, t)) => (p, Some(t)),
                None => (self.pattern()?, None),
            };
            if mutable {
                if let Pat::Ident { mutable: m, .. } = &mut pat {
                    *m = true;
                } else {
                    set_all_mut(&mut pat);
                }
                mutable = false;
            }
            let ty = if typed.is_none() && self.eat_punct(":") { Some(self.ty()?) } else { typed };
            let init = if self.eat_punct("=") {
                self.skip_nl_only();
                Some(self.expr()?)
            } else {
                None
            };
            let else_ = if self.is_kw("else") {
                self.bump();
                Some(self.block()?)
            } else {
                None
            };
            return Ok(Stmt::Let { pat, mutable, ty, init, else_, span: start.to(self.prev_span()) });
        }
        if self.is_kw("rust") && self.is_punct_at(1, "{") {
            self.bump();
            let g = self.raw_group()?;
            let sp = start.to(self.prev_span());
            return Ok(Stmt::Expr { expr: Expr::new(ExprKind::Raw(dedent_raw(&g[1..g.len() - 1])), sp), semi: false });
        }
        if self.is_item_start() {
            if let Some(item) = self.item()? {
                return Ok(Stmt::Item(Box::new(ItemBox(item))));
            }
        }
        let expr = self.expr_stmt_like()?;
        Ok(Stmt::Expr { expr, semi: false })
    }

    /// An expression that may be an assignment (statement position).
    fn expr_stmt_like(&mut self) -> PResult<Expr> {
        let lhs = self.expr()?;
        let op = match self.tok() {
            Tok::Punct("=") => Some(None),
            Tok::Punct("+=") => Some(Some(BinOp::Add)),
            Tok::Punct("-=") => Some(Some(BinOp::Sub)),
            Tok::Punct("*=") => Some(Some(BinOp::Mul)),
            Tok::Punct("/=") => Some(Some(BinOp::Div)),
            Tok::Punct("%=") => Some(Some(BinOp::Rem)),
            Tok::Punct("^=") => Some(Some(BinOp::BitXor)),
            Tok::Punct("&=") => Some(Some(BinOp::BitAnd)),
            Tok::Punct("|=") => Some(Some(BinOp::BitOr)),
            Tok::Punct("<<=") => Some(Some(BinOp::Shl)),
            Tok::Punct(">") if self.is_punct_at(1, ">=") && self.adjacent(0, 1) => Some(Some(BinOp::Shr)),
            _ => None,
        };
        if let Some(op) = op {
            if op == Some(BinOp::Shr) {
                self.bump();
            }
            self.bump();
            self.skip_nl_only();
            let rhs = self.expr()?;
            let sp = lhs.span.to(rhs.span);
            return Ok(Expr::new(ExprKind::Assign(op, Box::new(lhs), Box::new(rhs)), sp));
        }
        Ok(lhs)
    }

    /// Are tokens at offsets `a` and `b` from the current one touching? (`usize::MAX` = previous token)
    fn adjacent(&self, a: usize, b: usize) -> bool {
        let i = if a == usize::MAX { self.pos.wrapping_sub(1) } else { self.pos + a };
        let j = self.pos + b;
        i < self.toks.len() && j < self.toks.len() && self.toks[i].span.end == self.toks[j].span.start
    }

    // ------------------------------------------------------------------ expressions

    pub fn expr(&mut self) -> PResult<Expr> {
        self.range_expr()
    }

    fn can_start_expr(&self) -> bool {
        match self.tok() {
            Tok::Newline | Tok::Eof => false,
            Tok::Punct(p) => matches!(*p, "(" | "[" | "{" | "-" | "!" | "*" | "&" | "&&" | "|" | "||" | ".." | "::" | "<" | "!!"),
            Tok::Ident(s) => !matches!(s.as_str(), "else" | "catch" | "in" | "is" | "as" | "where"),
            Tok::DocOuter(_) | Tok::DocInner(_) => false,
            _ => true,
        }
    }

    fn range_expr(&mut self) -> PResult<Expr> {
        if self.is_punct("..") || self.is_punct("..=") || self.is_punct("..<") {
            let start = self.span();
            let inclusive = self.is_punct("..=");
            self.bump();
            let end = if self.can_start_expr() && !(self.no_struct && self.is_punct("{")) { Some(Box::new(self.or_expr()?)) } else { None };
            let sp = start.to(self.prev_span());
            return Ok(Expr::new(ExprKind::Range { start: None, end, inclusive }, sp));
        }
        let lhs = self.or_expr()?;
        if self.is_punct("..") || self.is_punct("..=") || self.is_punct("..<") {
            let inclusive = self.is_punct("..=");
            self.bump();
            let end = if self.can_start_expr() && !(self.no_struct && self.is_punct("{")) { Some(Box::new(self.or_expr()?)) } else { None };
            let sp = lhs.span.to(self.prev_span());
            return Ok(Expr::new(ExprKind::Range { start: Some(Box::new(lhs)), end, inclusive }, sp));
        }
        Ok(lhs)
    }

    fn or_expr(&mut self) -> PResult<Expr> {
        let mut lhs = self.and_expr()?;
        while self.is_punct("||") {
            self.bump();
            let rhs = self.and_expr()?;
            lhs = bin(BinOp::Or, lhs, rhs);
        }
        Ok(lhs)
    }

    fn and_expr(&mut self) -> PResult<Expr> {
        let mut lhs = self.cmp_expr()?;
        while self.is_punct("&&") {
            self.bump();
            let rhs = self.cmp_expr()?;
            lhs = bin(BinOp::And, lhs, rhs);
        }
        Ok(lhs)
    }

    fn cmp_expr(&mut self) -> PResult<Expr> {
        let lhs = self.elvis_expr()?;
        self.cmp_rest(lhs)
    }

    fn cmp_rest(&mut self, mut lhs: Expr) -> PResult<Expr> {
        loop {
            let op = match self.tok() {
                Tok::Punct("==") => BinOp::Eq,
                Tok::Punct("!=") => BinOp::Ne,
                Tok::Punct("<=") => BinOp::Le,
                Tok::Punct(">=") => BinOp::Ge,
                Tok::Punct("<") => BinOp::Lt,
                Tok::Punct(">") if !(self.is_punct_at(1, ">") && self.adjacent(0, 1)) && !(self.is_punct_at(1, ">=") && self.adjacent(0, 1)) => BinOp::Gt,
                Tok::Ident(s) if s == "in" => {
                    self.bump();
                    let rhs = self.in_rhs()?;
                    let sp = lhs.span.to(rhs.span);
                    lhs = Expr::new(ExprKind::In { item: Box::new(lhs), coll: Box::new(rhs), negated: false }, sp);
                    continue;
                }
                Tok::Punct("!") if self.is_kw_at(1, "in") => {
                    self.bump();
                    self.bump();
                    let rhs = self.in_rhs()?;
                    let sp = lhs.span.to(rhs.span);
                    lhs = Expr::new(ExprKind::In { item: Box::new(lhs), coll: Box::new(rhs), negated: true }, sp);
                    continue;
                }
                Tok::Ident(s) if s == "is" => {
                    self.bump();
                    let pat = self.pattern()?;
                    let sp = lhs.span.to(self.prev_span());
                    lhs = Expr::new(ExprKind::Is { expr: Box::new(lhs), pat, negated: false }, sp);
                    continue;
                }
                Tok::Punct("!") if self.is_kw_at(1, "is") => {
                    self.bump();
                    self.bump();
                    let pat = self.pattern()?;
                    let sp = lhs.span.to(self.prev_span());
                    lhs = Expr::new(ExprKind::Is { expr: Box::new(lhs), pat, negated: true }, sp);
                    continue;
                }
                _ => break,
            };
            self.bump();
            let rhs = self.elvis_expr()?;
            lhs = bin(op, lhs, rhs);
        }
        Ok(lhs)
    }

    /// Right side of `in`: a collection or a range (`x in 1..10`).
    fn in_rhs(&mut self) -> PResult<Expr> {
        let lhs = self.elvis_expr()?;
        if self.is_punct("..") || self.is_punct("..=") || self.is_punct("..<") {
            let inclusive = self.is_punct("..=");
            self.bump();
            let end = self.elvis_expr()?;
            let sp = lhs.span.to(end.span);
            return Ok(Expr::new(ExprKind::Range { start: Some(Box::new(lhs)), end: Some(Box::new(end)), inclusive }, sp));
        }
        Ok(lhs)
    }

    fn elvis_expr(&mut self) -> PResult<Expr> {
        let mut lhs = self.to_expr()?;
        while self.is_punct("?:") {
            self.bump();
            let rhs = self.to_expr()?;
            let sp = lhs.span.to(rhs.span);
            lhs = Expr::new(ExprKind::Elvis(Box::new(lhs), Box::new(rhs)), sp);
        }
        Ok(lhs)
    }

    /// Kotlin infix words: `a to b` (pair), `0 until n`, `10 downTo 0`, `range step 2`.
    fn to_expr(&mut self) -> PResult<Expr> {
        let mut lhs = self.bitor_expr()?;
        loop {
            let word = match self.tok() {
                Tok::Ident(w) if matches!(w.as_str(), "to" | "until" | "downTo" | "step") && self.can_start_expr_at(1) => w.clone(),
                _ => break,
            };
            self.bump();
            let rhs = self.bitor_expr()?;
            let sp = lhs.span.to(rhs.span);
            lhs = match word.as_str() {
                "to" => Expr::new(ExprKind::Pair(Box::new(lhs), Box::new(rhs)), sp),
                "until" => Expr::new(ExprKind::Range { start: Some(Box::new(lhs)), end: Some(Box::new(rhs)), inclusive: false }, sp),
                "downTo" => {
                    // (rhs..=lhs).rev()
                    let range = Expr::new(ExprKind::Range { start: Some(Box::new(rhs)), end: Some(Box::new(lhs)), inclusive: true }, sp);
                    let paren = Expr::new(ExprKind::Paren(Box::new(range)), sp);
                    Expr::new(ExprKind::MethodCall { recv: Box::new(paren), name: "rev".into(), generics: None, args: vec![], safe: false }, sp)
                }
                _ => {
                    // range.step_by(n as usize)
                    let recv = match lhs.kind {
                        ExprKind::Range { .. } => Expr::new(ExprKind::Paren(Box::new(lhs)), sp),
                        _ => lhs,
                    };
                    let n = Expr::new(ExprKind::Cast(Box::new(rhs), Type::simple("usize")), sp);
                    Expr::new(ExprKind::MethodCall { recv: Box::new(recv), name: "step_by".into(), generics: None, args: vec![Arg { name: None, value: n, trailing: false }], safe: false }, sp)
                }
            };
        }
        Ok(lhs)
    }

    fn can_start_expr_at(&self, n: usize) -> bool {
        !matches!(self.tok_at(n), Tok::Newline | Tok::Eof | Tok::Punct(")") | Tok::Punct(",") | Tok::Punct("}") | Tok::Punct("]") | Tok::Punct("="))
    }

    fn bitor_expr(&mut self) -> PResult<Expr> {
        let mut lhs = self.bitxor_expr()?;
        while self.is_punct("|") {
            self.bump();
            let rhs = self.bitxor_expr()?;
            lhs = bin(BinOp::BitOr, lhs, rhs);
        }
        Ok(lhs)
    }

    fn bitxor_expr(&mut self) -> PResult<Expr> {
        let mut lhs = self.bitand_expr()?;
        while self.is_punct("^") {
            self.bump();
            let rhs = self.bitand_expr()?;
            lhs = bin(BinOp::BitXor, lhs, rhs);
        }
        Ok(lhs)
    }

    fn bitand_expr(&mut self) -> PResult<Expr> {
        let mut lhs = self.shift_expr()?;
        while self.is_punct("&") {
            self.bump();
            let rhs = self.shift_expr()?;
            lhs = bin(BinOp::BitAnd, lhs, rhs);
        }
        Ok(lhs)
    }

    fn shift_expr(&mut self) -> PResult<Expr> {
        let mut lhs = self.add_expr()?;
        loop {
            if self.is_punct("<<") {
                self.bump();
                let rhs = self.add_expr()?;
                lhs = bin(BinOp::Shl, lhs, rhs);
            } else if self.is_punct(">") && self.is_punct_at(1, ">") && self.adjacent(0, 1) && !(self.is_punct_at(2, "=") && self.adjacent(1, 2)) {
                self.bump();
                self.bump();
                let rhs = self.add_expr()?;
                lhs = bin(BinOp::Shr, lhs, rhs);
            } else {
                break;
            }
        }
        Ok(lhs)
    }

    fn add_expr(&mut self) -> PResult<Expr> {
        let mut lhs = self.mul_expr()?;
        loop {
            let op = match self.tok() {
                Tok::Punct("+") => BinOp::Add,
                Tok::Punct("-") => BinOp::Sub,
                _ => break,
            };
            self.bump();
            let rhs = self.mul_expr()?;
            lhs = bin(op, lhs, rhs);
        }
        Ok(lhs)
    }

    fn mul_expr(&mut self) -> PResult<Expr> {
        let mut lhs = self.cast_expr()?;
        loop {
            let op = match self.tok() {
                Tok::Punct("*") => BinOp::Mul,
                Tok::Punct("/") => BinOp::Div,
                Tok::Punct("%") => BinOp::Rem,
                _ => break,
            };
            self.bump();
            let rhs = self.cast_expr()?;
            lhs = bin(op, lhs, rhs);
        }
        Ok(lhs)
    }

    fn cast_expr(&mut self) -> PResult<Expr> {
        let mut e = self.unary_expr()?;
        while self.is_kw("as") {
            self.bump();
            self.cast_ty = true;
            let t = self.ty_base(true);
            self.cast_ty = false;
            let t = t?;
            let sp = e.span.to(self.prev_span());
            e = Expr::new(ExprKind::Cast(Box::new(e), t), sp);
        }
        Ok(e)
    }

    fn unary_expr(&mut self) -> PResult<Expr> {
        let start = self.span();
        // Kotlin's `++n` and `--n`
        if let Tok::Punct(p @ ("++" | "--")) = self.tok().clone() {
            self.bump();
            let e = self.unary_expr()?;
            return Ok(self.step_by_one(e, p, start));
        }
        let op = match self.tok() {
            Tok::Punct("-") => Some(UnOp::Neg),
            Tok::Punct("!") => Some(UnOp::Not),
            Tok::Punct("*") => Some(UnOp::Deref),
            _ => None,
        };
        if let Some(op) = op {
            self.bump();
            let e = self.unary_expr()?;
            let sp = start.to(e.span);
            return Ok(Expr::new(ExprKind::Unary(op, Box::new(e)), sp));
        }
        if self.is_punct("!!") {
            self.bump();
            let e = self.unary_expr()?;
            let sp = start.to(e.span);
            let inner = Expr::new(ExprKind::Unary(UnOp::Not, Box::new(e)), sp);
            return Ok(Expr::new(ExprKind::Unary(UnOp::Not, Box::new(inner)), sp));
        }
        if self.is_punct("&") || self.is_punct("&&") {
            let double = self.is_punct("&&");
            self.bump();
            let op = if self.eat_kw("mut") { UnOp::RefMut } else { UnOp::Ref };
            let e = self.unary_expr()?;
            let sp = start.to(e.span);
            let r = Expr::new(ExprKind::Unary(op, Box::new(e)), sp);
            return Ok(if double { Expr::new(ExprKind::Unary(UnOp::Ref, Box::new(r)), sp) } else { r });
        }
        if self.is_kw("await") && self.can_start_expr_at(1) && !self.is_punct_at(1, ".") {
            self.bump();
            let inner = self.postfix_expr(true)?;
            let sp = start.to(inner.span);
            let awaited = Expr::new(ExprKind::Await(Box::new(inner)), sp);
            return self.postfix_rest(awaited, false);
        }
        self.postfix_expr(false)
    }

    fn postfix_expr(&mut self, stop_at_try: bool) -> PResult<Expr> {
        let e = self.primary()?;
        self.postfix_rest(e, stop_at_try)
    }

    fn postfix_rest(&mut self, mut e: Expr, stop_at_try: bool) -> PResult<Expr> {
        loop {
            match self.tok().clone() {
                Tok::Punct(".") | Tok::Punct("?.") => {
                    let safe = self.is_punct("?.");
                    self.bump();
                    if self.is_kw("await") {
                        self.bump();
                        let sp = e.span.to(self.prev_span());
                        e = Expr::new(ExprKind::Await(Box::new(e)), sp);
                        continue;
                    }
                    if self.is_kw("let") && self.is_punct_at(1, "{") {
                        // Kotlin scope function x.let { ... }
                        self.bump();
                        let lam = self.lambda_braces()?;
                        let sp = e.span.to(lam.span);
                        e = Expr::new(
                            ExprKind::MethodCall { recv: Box::new(e), name: "let".into(), generics: None, args: vec![Arg { name: None, value: lam, trailing: true }], safe },
                            sp,
                        );
                        continue;
                    }
                    let name = self.member_name()?;
                    let mut generics = None;
                    if self.is_punct("::") && self.is_punct_at(1, "<") {
                        self.bump();
                        generics = Some(self.generic_args()?);
                    } else if self.is_punct("<") {
                        let save = self.pos;
                        let nd = self.diags.len();
                        match self.generic_args() {
                            Ok(g) if self.is_punct("(") => generics = Some(g),
                            _ => {
                                self.pos = save;
                                self.diags.truncate(nd);
                            }
                        }
                    }
                    // `Shape.Rect { w: 1.0, h: 2.0 }`: a struct literal of a variant or nested type
                    let struct_path = if !safe && (self.type_like(&name) || self.is_capitalized_struct_lit(&name)) && self.is_punct("{") && !self.no_struct && self.looks_like_struct_lit() && !self.braces_have_arrow() {
                        dot_path(&e)
                    } else {
                        None
                    };
                    if let Some(mut path) = struct_path {
                        path.push(name);
                        let start = e.span;
                        e = self.struct_lit(path, generics, start)?;
                    } else if self.is_punct("(") {
                        let mut args = self.call_args()?;
                        if self.trailing_lambda_ok() {
                            args.push(Arg { name: None, value: self.lambda_braces()?, trailing: true });
                        }
                        let sp = e.span.to(self.prev_span());
                        e = Expr::new(ExprKind::MethodCall { recv: Box::new(e), name, generics, args, safe }, sp);
                    } else if self.trailing_lambda_ok() {
                        let lam = self.lambda_braces()?;
                        let sp = e.span.to(lam.span);
                        e = Expr::new(ExprKind::MethodCall { recv: Box::new(e), name, generics, args: vec![Arg { name: None, value: lam, trailing: true }], safe }, sp);
                    } else if let (Some(g), Some(mut segs)) = (generics.clone(), dot_path(&e)) {
                        // `std.collections.HashMap::<K, V>.new()`: a type path with arguments
                        segs.push(name);
                        let sp = e.span.to(self.prev_span());
                        e = Expr::new(ExprKind::Path { segs, generics: Some(g), global: false }, sp);
                    } else {
                        let sp = e.span.to(self.prev_span());
                        e = Expr::new(ExprKind::Field { recv: Box::new(e), name, safe }, sp);
                    }
                }
                Tok::Punct("(") => {
                    let mut args = self.call_args()?;
                    if self.trailing_lambda_ok() {
                        args.push(Arg { name: None, value: self.lambda_braces()?, trailing: true });
                    }
                    let sp = e.span.to(self.prev_span());
                    e = Expr::new(ExprKind::Call { callee: Box::new(e), args }, sp);
                }
                Tok::Punct("[") => {
                    self.bump();
                    let saved = self.no_struct;
                    self.no_struct = false;
                    let idx = self.expr()?;
                    self.no_struct = saved;
                    self.expect_punct("]")?;
                    let sp = e.span.to(self.prev_span());
                    e = Expr::new(ExprKind::Index(Box::new(e), Box::new(idx)), sp);
                }
                Tok::Punct("?") if !stop_at_try => {
                    self.bump();
                    let sp = e.span.to(self.prev_span());
                    e = Expr::new(ExprKind::Try(Box::new(e)), sp);
                }
                Tok::Punct(p @ ("++" | "--")) => {
                    let at = self.bump().span;
                    e = self.step_by_one(e, p, at);
                }
                Tok::Punct("!!") if !stop_at_try => {
                    self.bump();
                    let sp = e.span.to(self.prev_span());
                    e = Expr::new(ExprKind::Unwrap(Box::new(e)), sp);
                }
                Tok::Punct("{") if self.trailing_lambda_ok() && is_lambda_callee(&e) => {
                    let lam = self.lambda_braces()?;
                    let sp = e.span.to(lam.span);
                    e = Expr::new(ExprKind::Call { callee: Box::new(e), args: vec![Arg { name: None, value: lam, trailing: true }] }, sp);
                }
                _ => break,
            }
        }
        Ok(e)
    }

    /// `n++` and `--n`, which Oxie doesn't have: an error saying `n += 1`, read as that.
    fn step_by_one(&mut self, e: Expr, op: &str, at: Span) -> Expr {
        let target = self.src.get(e.span.start as usize..e.span.end as usize).unwrap_or("n");
        let fix = format!("{} {}= 1", target, &op[..1]);
        self.diags.push(Diag::error(at, format!("Oxie has no `{}`", op)).with_help(format!("write `{}`", fix)));
        let sp = e.span.to(at);
        let one = Expr::new(ExprKind::Int("1".into()), at);
        Expr::new(ExprKind::Assign(Some(if op == "++" { BinOp::Add } else { BinOp::Sub }), Box::new(e), Box::new(one)), sp)
    }

    fn trailing_lambda_ok(&self) -> bool {
        self.is_punct("{") && !self.no_struct
    }

    fn call_args(&mut self) -> PResult<Vec<Arg>> {
        self.expect_punct("(")?;
        let saved = self.no_struct;
        self.no_struct = false;
        let mut args = Vec::new();
        loop {
            if self.eat_punct(")") {
                break;
            }
            let name = if self.is_ident() && self.is_punct_at(1, "=") {
                let n = self.ident()?;
                self.bump();
                Some(n)
            } else {
                None
            };
            let value = self.expr()?;
            args.push(Arg { name, value, trailing: false });
            if !self.eat_punct(",") {
                if self.expect_punct(")").is_err() {
                    self.no_struct = saved;
                    return Err(());
                }
                break;
            }
        }
        self.no_struct = saved;
        Ok(args)
    }

    fn paren_cond(&mut self) -> PResult<Cond> {
        let saved = self.no_struct;
        self.no_struct = false;
        let c = self.cond();
        self.no_struct = saved;
        c
    }

    fn cond(&mut self) -> PResult<Cond> {
        if !self.chain_ahead() {
            if self.is_let_start() {
                self.bump();
                let pat = self.pattern()?;
                self.expect_punct("=")?;
                let expr = self.expr()?;
                let implicit_some = matches!(&pat, Pat::Ident { sub: None, .. } | Pat::Tuple(_));
                return Ok(Cond::Let { pat, expr, implicit_some });
            }
            return Ok(Cond::Expr(self.expr()?));
        }
        // let chain: parts joined by `&&`; scrutinees and conditions stop at `&&`/`||`
        let mut parts = Vec::new();
        loop {
            if self.is_let_start() {
                self.bump();
                let pat = self.pattern()?;
                self.expect_punct("=")?;
                let expr = self.cmp_expr()?;
                let implicit_some = matches!(&pat, Pat::Ident { sub: None, .. } | Pat::Tuple(_));
                parts.push(Cond::Let { pat, expr, implicit_some });
            } else {
                parts.push(Cond::Expr(self.cmp_expr()?));
            }
            if !self.eat_punct("&&") {
                break;
            }
        }
        Ok(Cond::Chain(parts))
    }

    fn is_let_start(&self) -> bool {
        self.is_kw("val") || self.is_kw("let") || self.is_kw("var")
    }

    /// Is the condition starting here a let chain (`val x = a && ...` or `a && val x = b`)?
    fn chain_ahead(&self) -> bool {
        let mut i = 0;
        let mut depth = 0i32;
        let mut seen_let = self.is_let_start();
        let mut seen_and = false;
        loop {
            match self.tok_at(i) {
                Tok::Eof | Tok::Newline => break,
                Tok::Punct("(") | Tok::Punct("[") => depth += 1,
                // Rust's `if c {` starts the body; inside `if (...)` it is a lambda: `filter { }`
                Tok::Punct("{") if depth == 0 && self.no_struct => break,
                Tok::Punct("{") => depth += 1,
                Tok::Punct(")") | Tok::Punct("]") | Tok::Punct("}") => {
                    if depth == 0 {
                        break;
                    }
                    depth -= 1;
                }
                Tok::Punct("&&") if depth == 0 => {
                    seen_and = true;
                    if matches!(self.tok_at(i + 1), Tok::Ident(s) if s == "val" || s == "let" || s == "var") {
                        seen_let = true;
                    }
                }
                _ => {}
            }
            i += 1;
        }
        seen_let && seen_and
    }

    /// Parses `if`/`while` header. Returns the condition and whether Kotlin-style parens were used.
    fn if_header(&mut self) -> PResult<(Cond, bool)> {
        if self.is_punct("(") {
            let open = self.pos;
            if let Some(close) = self.matching_close(open) {
                // Kotlin style if (cond) - unless the parens are just the start of a longer Rust condition
                let after = &self.toks[close + 1].tok;
                let continues = matches!(after,
                    Tok::Punct("&&") | Tok::Punct("||") | Tok::Punct("==") | Tok::Punct("!=") | Tok::Punct("<") | Tok::Punct(">")
                    | Tok::Punct("<=") | Tok::Punct(">=") | Tok::Punct("+") | Tok::Punct("/") | Tok::Punct("%") | Tok::Punct(".")
                    | Tok::Punct("?") | Tok::Punct("?.") | Tok::Punct("?:") | Tok::Punct("|") | Tok::Punct("^") | Tok::Punct("..")
                ) || matches!(after, Tok::Ident(s) if s == "as" || s == "in" || s == "is");
                if !continues {
                    self.bump();
                    let c = self.paren_cond()?;
                    self.expect_punct(")")?;
                    return Ok((c, true));
                }
            }
        }
        let saved = self.no_struct;
        self.no_struct = true;
        let c = if self.is_kw("let") || self.is_kw("val") {
            self.cond()
        } else {
            self.expr().map(Cond::Expr)
        };
        self.no_struct = saved;
        Ok((c?, false))
    }

    /// Body after a Kotlin-style header: a block or a single statement.
    fn branch_body(&mut self, parens: bool) -> PResult<Block> {
        if parens {
            if self.peek_past_newlines_punct("{") {
                return self.block();
            }
            if self.is_newline() {
                self.skip_nl_only();
            }
            let start = self.span();
            let e = self.expr_stmt_like()?;
            let sp = start.to(e.span);
            Ok(Block { stmts: vec![Stmt::Expr { expr: e, semi: false }], span: sp })
        } else {
            self.block()
        }
    }

    fn primary(&mut self) -> PResult<Expr> {
        let start = self.span();
        let t = self.tok().clone();
        match t {
            Tok::Int(s) => {
                self.bump();
                Ok(Expr::new(ExprKind::Int(s), start))
            }
            Tok::Float(s) => {
                self.bump();
                Ok(Expr::new(ExprKind::Float(s), start))
            }
            Tok::Char(s) => {
                self.bump();
                Ok(Expr::new(ExprKind::Char(s), start))
            }
            Tok::RawStr(s) => {
                self.bump();
                Ok(Expr::new(ExprKind::RawStr(s), start))
            }
            Tok::Str(parts) => {
                self.bump();
                self.string_expr(parts, start)
            }
            // `'outer: for`, and Kotlin's `outer@ for`
            Tok::Lifetime(label) if self.is_punct_at(1, ":") => self.labeled(label, start),
            Tok::Ident(label) if self.is_punct_at(1, "@") && (["for", "while", "loop"].iter().any(|k| self.is_kw_at(2, k)) || self.is_punct_at(2, "{")) => self.labeled(label, start),
            Tok::Punct("(") => self.paren_or_lambda(),
            Tok::Punct("[") => {
                self.bump();
                let saved = self.no_struct;
                self.no_struct = false;
                let mut elems = Vec::new();
                loop {
                    if self.eat_punct("]") {
                        break;
                    }
                    let e = self.expr()?;
                    if elems.is_empty() && self.eat_punct(";") {
                        let n = self.expr()?;
                        self.expect_punct("]")?;
                        self.no_struct = saved;
                        return Ok(Expr::new(ExprKind::ArrayRepeat(Box::new(e), Box::new(n)), start.to(self.prev_span())));
                    }
                    elems.push(e);
                    if !self.eat_punct(",") {
                        self.expect_punct("]")?;
                        break;
                    }
                }
                self.no_struct = saved;
                Ok(Expr::new(ExprKind::Array(elems), start.to(self.prev_span())))
            }
            Tok::Punct("{") => self.lambda_braces(),
            Tok::Punct("|") | Tok::Punct("||") => self.rust_closure(false, start),
            Tok::Punct("::") | Tok::Punct("<") => self.path_expr(),
            Tok::Ident(ref s) => match s.as_str() {
                "true" | "false" => {
                    self.bump();
                    Ok(Expr::new(ExprKind::Bool(s == "true"), start))
                }
                "null" => {
                    self.bump();
                    Ok(Expr::new(ExprKind::Null, start))
                }
                // `self::helper(x)`: this module's `helper`, also where a method has its name
                "self" if self.is_punct_at(1, "::") => self.path_expr(),
                "this" | "self" => {
                    self.bump();
                    Ok(Expr::new(ExprKind::SelfValue, start))
                }
                "if" => self.if_expr(),
                "when" => self.when_expr(),
                "match" => self.match_expr(),
                "for" => self.for_expr(None, start),
                "while" => self.while_expr(None, start),
                // `do { } while (c)`, or with a single statement: `do x += 1 while (c)`
                "do" if self.is_punct_at(1, "{") || self.can_start_expr_at(1) => {
                    self.bump();
                    let body = self.branch_body(true)?;
                    if !self.peek_past_newlines_kw("while") {
                        return self.expected("`while` after `do { }`");
                    }
                    self.bump();
                    let (c, _) = self.if_header()?;
                    let cond = match c {
                        Cond::Expr(e) => e,
                        Cond::Let { .. } | Cond::Chain(_) => return self.err("`do { } while (val ...)` is not supported"),
                    };
                    Ok(Expr::new(ExprKind::DoWhile { label: None, body, cond: Box::new(cond) }, start.to(self.prev_span())))
                }
                "loop" => {
                    self.bump();
                    let body = self.block()?;
                    Ok(Expr::new(ExprKind::Loop { label: None, body }, start.to(self.prev_span())))
                }
                "try" if self.peek_past_newlines_punct_at1("{") => {
                    self.bump();
                    self.skip_nl_only();
                    let body = self.block()?;
                    let catch = if self.peek_past_newlines_kw("catch") {
                        self.bump();
                        let (binding, ty) = if self.is_punct("(") {
                            self.bump();
                            let p = self.pattern()?;
                            let t = if self.eat_punct(":") { Some(self.ty()?) } else { None };
                            self.expect_punct(")")?;
                            (p, t)
                        } else if self.is_ident() {
                            let p = self.pattern_no_or()?;
                            let t = if self.eat_punct(":") { Some(self.ty()?) } else { None };
                            (p, t)
                        } else {
                            (Pat::Wild, None)
                        };
                        self.skip_nl_only();
                        let body = self.block()?;
                        Some(Box::new(Catch { binding, ty, body }))
                    } else {
                        None
                    };
                    Ok(Expr::new(ExprKind::TryCatch { body, catch }, start.to(self.prev_span())))
                }
                "return" => {
                    self.bump();
                    // Kotlin's `return@for_each` leaves a lambda, which a `return` in it does
                    if self.is_punct("@") && matches!(self.tok_at(1), Tok::Ident(_)) {
                        let at = self.bump().span;
                        let Tok::Ident(l) = self.bump().tok else { unreachable!() };
                        self.diags.push(
                            Diag::error(at.to(self.prev_span()), format!("Oxie has no `return@{}`", l))
                                .with_help("`return` in a lambda already returns from the lambda: write `return`. To leave a loop, label it (`outer@ for (...)`) and `break@outer`."),
                        );
                    }
                    let v = if self.can_start_expr() && !self.is_punct("}") && !self.is_punct(")") && !self.is_punct(",") && !self.is_punct(";") {
                        Some(Box::new(self.expr()?))
                    } else {
                        None
                    };
                    Ok(Expr::new(ExprKind::Return(v), start.to(self.prev_span())))
                }
                "throw" => {
                    self.bump();
                    let v = self.expr()?;
                    Ok(Expr::new(ExprKind::Throw(Box::new(v)), start.to(self.prev_span())))
                }
                "break" => {
                    self.bump();
                    let label = self.jump_label();
                    let v = if self.can_start_expr() && !self.is_punct("}") && !self.is_punct(")") && !self.is_punct(",") && !self.is_punct(";") {
                        Some(Box::new(self.expr()?))
                    } else {
                        None
                    };
                    Ok(Expr::new(ExprKind::Break { label, value: v }, start.to(self.prev_span())))
                }
                "continue" => {
                    self.bump();
                    let label = self.jump_label();
                    Ok(Expr::new(ExprKind::Continue(label), start.to(self.prev_span())))
                }
                "const" if self.is_punct_at(1, "{") => {
                    self.bump();
                    let b = self.block()?;
                    Ok(Expr::new(ExprKind::ConstBlock(b), start.to(self.prev_span())))
                }
                "unsafe" if self.is_punct_at(1, "{") => {
                    self.bump();
                    let b = self.block()?;
                    Ok(Expr::new(ExprKind::Unsafe(b), start.to(self.prev_span())))
                }
                "async" => {
                    self.bump();
                    let is_move = self.eat_kw("move");
                    if self.is_punct("|") || self.is_punct("||") {
                        let mut c = self.rust_closure(is_move, start)?;
                        if let ExprKind::Lambda { is_async, .. } = &mut c.kind {
                            *is_async = true;
                        }
                        return Ok(c);
                    }
                    let block = self.block()?;
                    Ok(Expr::new(ExprKind::Async { is_move, block }, start.to(self.prev_span())))
                }
                "move" => {
                    self.bump();
                    let mut e = if self.is_punct("|") || self.is_punct("||") {
                        self.rust_closure(true, start)?
                    } else if self.is_punct("{") {
                        self.lambda_braces()?
                    } else if self.is_punct("(") || self.is_ident() {
                        self.paren_or_lambda_or_ident()?
                    } else {
                        return self.expected("closure after `move`");
                    };
                    if let ExprKind::Lambda { is_move, .. } = &mut e.kind {
                        *is_move = true;
                    } else {
                        return self.err("expected closure after `move`");
                    }
                    Ok(e)
                }
                "rust" if self.is_punct_at(1, "{") => {
                    self.bump();
                    let g = self.raw_group()?;
                    Ok(Expr::new(ExprKind::Raw(dedent_raw(&g[1..g.len() - 1])), start.to(self.prev_span())))
                }
                "fun" if self.is_punct_at(1, "(") => {
                    // anonymous function: fun(x: i32): i32 { ... }
                    self.bump();
                    self.paren_lambda()
                }
                _ => {
                    if self.is_punct_at(1, "=>") && self.is_ident() {
                        return self.arrow_lambda_single();
                    }
                    self.path_expr()
                }
            },
            Tok::RawIdent(_) => {
                if self.is_punct_at(1, "=>") {
                    return self.arrow_lambda_single();
                }
                self.path_expr()
            }
            _ => self.expected("expression"),
        }
    }

    fn peek_past_newlines_punct_at1(&self, p: &str) -> bool {
        let mut i = 1;
        while matches!(self.tok_at(i), Tok::Newline) {
            i += 1;
        }
        self.is_punct_at(i, p)
    }

    fn paren_or_lambda_or_ident(&mut self) -> PResult<Expr> {
        if self.is_ident() && self.is_punct_at(1, "=>") {
            return self.arrow_lambda_single();
        }
        self.paren_or_lambda()
    }

    /// The message of an `error class` or `error enum` case: `= "'$text' is not a number"`.
    fn message(&mut self) -> PResult<Expr> {
        self.positional = true;
        let e = self.expr();
        self.positional = false;
        e
    }

    fn string_expr(&mut self, parts: Vec<StrPart>, span: Span) -> PResult<Expr> {
        // `$5` is text, except `$0` in an error message
        let parts = if self.positional { parts } else { lexer::dollar_digits_as_text(parts) };
        if parts.iter().all(|p| matches!(p, StrPart::Lit(_))) {
            return Ok(Expr::new(ExprKind::Str(parts), span));
        }
        let mut out = Vec::new();
        for p in parts {
            match p {
                StrPart::Lit(s) => out.push(TemplatePart::Lit(s)),
                StrPart::Interp { src, offset, .. } if src.starts_with('$') => {
                    let n = Expr::new(ExprKind::Int(src[1..].to_string()), Span::new(offset + 1, offset + src.len()));
                    out.push(TemplatePart::Expr(n, None));
                }
                StrPart::Interp { src: s, offset, spec } => {
                    let (toks, mut diags) = lexer::lex_at(&s, offset);
                    self.diags.append(&mut diags);
                    let mut sub = Parser { src: self.src, inner_attrs: Vec::new(), toks, pos: 0, diags: Vec::new(), no_struct: false, cast_ty: false, positional: false, fn_ret: true, caps_types: self.caps_types.clone() };
                    let e = sub.expr();
                    if e.is_ok() && !matches!(sub.tok(), Tok::Newline | Tok::Eof) {
                        let sp = sub.span();
                        sub.diags.push(Diag::error(sp, "unexpected tokens in string interpolation"));
                    }
                    self.diags.append(&mut sub.diags);
                    let e = e.unwrap_or_else(|_| Expr::new(ExprKind::Error, Span::new(offset, offset + s.len())));
                    out.push(TemplatePart::Expr(e, spec));
                }
            }
        }
        Ok(Expr::new(ExprKind::Template(out), span))
    }

    fn path_expr(&mut self) -> PResult<Expr> {
        let start = self.span();
        if self.is_punct("<") {
            let t = self.ty_base(true)?;
            return Ok(Expr::new(ExprKind::QualifiedPath(t), start.to(self.prev_span())));
        }
        let global = self.eat_punct("::");
        let first = match self.tok().clone() {
            Tok::Ident(s) if s == "Self" || s == "super" || s == "crate" || (s == "self" && self.is_punct_at(1, "::")) || !is_reserved(&s) => {
                self.bump();
                s
            }
            Tok::RawIdent(s) => {
                self.bump();
                format!("r#{}", s)
            }
            _ => return self.expected("expression"),
        };
        let mut segs = vec![first];
        let mut generics = None;
        loop {
            if self.is_punct("::") && self.is_punct_at(1, "<") {
                self.bump();
                let g = self.generic_args()?;
                // `Vec::<i32>::new`: the arguments belong to `Vec`, so this is `<Vec<i32>>::new`
                if self.is_punct("::") && !self.is_punct_at(1, "<") {
                    let mut type_segs: Vec<PathSeg> = segs.iter().map(PathSeg::plain).collect();
                    type_segs.last_mut().unwrap().generics = Some(g);
                    let mut rest = Vec::new();
                    while self.is_punct("::") {
                        self.bump();
                        rest.push(PathSeg::plain(self.member_name()?));
                        if self.is_punct("::") && self.is_punct_at(1, "<") {
                            self.bump();
                            rest.last_mut().unwrap().generics = Some(self.generic_args()?);
                        }
                    }
                    let ty = Type::Path { global, segs: type_segs, span: start.to(self.prev_span()) };
                    let t = Type::Qualified { ty: Box::new(ty), trait_: None, rest };
                    return Ok(Expr::new(ExprKind::QualifiedPath(t), start.to(self.prev_span())));
                }
                generics = Some(g);
                continue;
            }
            if self.is_punct("::") {
                self.bump();
                segs.push(self.member_name()?);
                continue;
            }
            break;
        }
        // Type<Args>::item or Type<Args>(...) without turbofish
        if generics.is_none() && self.is_punct("<") && self.type_like(segs.last().unwrap()) {
            let save = self.pos;
            let nd = self.diags.len();
            match self.generic_args() {
                Ok(g) if self.is_punct("::") || self.is_punct("(") || self.is_punct("{") || self.is_punct(".") => {
                    generics = Some(g);
                    while self.is_punct("::") {
                        self.bump();
                        segs.push(self.member_name()?);
                    }
                }
                _ => {
                    self.pos = save;
                    self.diags.truncate(nd);
                }
            }
        } else if generics.is_none() && self.is_punct("<") && self.adjacent(usize::MAX, 0) {
            // TypeScript/Kotlin generic call: `mutableListOf<i32>()`, `parse<T>(x)`
            let save = self.pos;
            let nd = self.diags.len();
            match self.generic_args() {
                Ok(g) if self.is_punct("(") => generics = Some(g),
                _ => {
                    self.pos = save;
                    self.diags.truncate(nd);
                }
            }
        }
        let sp = start.to(self.prev_span());
        // macro call
        if self.is_punct("!") && (self.is_punct_at(1, "(") || self.is_punct_at(1, "[") || self.is_punct_at(1, "{")) && self.adjacent(0, 1) {
            self.bump();
            let text = self.raw_group()?;
            return Ok(Expr::new(ExprKind::Macro { path: segs.join("::"), text }, sp.to(self.prev_span())));
        }
        // vec[...]
        if segs.len() == 1 && segs[0] == "vec" && self.is_punct("[") {
            self.bump();
            let saved = self.no_struct;
            self.no_struct = false;
            let mut elems = Vec::new();
            loop {
                if self.eat_punct("]") {
                    break;
                }
                let e = self.expr()?;
                if elems.is_empty() && self.eat_punct(";") {
                    let n = self.expr()?;
                    self.expect_punct("]")?;
                    self.no_struct = saved;
                    let rep = Expr::new(ExprKind::ArrayRepeat(Box::new(e), Box::new(n)), sp.to(self.prev_span()));
                    return Ok(Expr::new(ExprKind::VecLit(vec![rep]), sp.to(self.prev_span())));
                }
                elems.push(e);
                if !self.eat_punct(",") {
                    self.expect_punct("]")?;
                    break;
                }
            }
            self.no_struct = saved;
            return Ok(Expr::new(ExprKind::VecLit(elems), sp.to(self.prev_span())));
        }
        // struct literal
        if self.is_punct("{") && !self.no_struct && (self.type_like(segs.last().unwrap()) || self.is_capitalized_struct_lit(segs.last().unwrap())) && self.looks_like_struct_lit() {
            return self.struct_lit(segs, generics, sp);
        }
        Ok(Expr::new(ExprKind::Path { segs, generics, global }, sp))
    }

    /// Does the `{ ... }` at the current token contain a top-level `->` (a lambda with parameters)?
    fn braces_have_arrow(&self) -> bool {
        let mut depth = 0;
        let mut i = 0;
        loop {
            match self.tok_at(i) {
                Tok::Punct("{") | Tok::Punct("(") | Tok::Punct("[") => depth += 1,
                Tok::Punct("}") | Tok::Punct(")") | Tok::Punct("]") => {
                    depth -= 1;
                    if depth == 0 {
                        return false;
                    }
                }
                Tok::Punct("->") if depth == 1 => return true,
                Tok::Eof => return false,
                _ => {}
            }
            i += 1;
        }
    }

    /// `V2 { x: 1 }` with a capitalized name declared elsewhere: a field name and `:` follow.
    fn is_capitalized_struct_lit(&self, name: &str) -> bool {
        if !name.starts_with(|c: char| c.is_uppercase()) {
            return false;
        }
        let mut i = 1;
        while matches!(self.tok_at(i), Tok::Newline) {
            i += 1;
        }
        matches!(self.tok_at(i), Tok::Ident(_)) && matches!(self.tok_at(i + 1), Tok::Punct(":"))
    }

    fn looks_like_struct_lit(&self) -> bool {
        // `Name {}` / `Name { x: ...` / `Name { x, ...` / `Name { ..base }` / `Name { x }`
        let mut i = 1;
        while matches!(self.tok_at(i), Tok::Newline) {
            i += 1;
        }
        match self.tok_at(i) {
            Tok::Punct("}") => true,
            Tok::Punct("..") => true,
            Tok::Ident(_) | Tok::Int(_) | Tok::RawIdent(_) => {
                let mut j = i + 1;
                while matches!(self.tok_at(j), Tok::Newline) {
                    j += 1;
                }
                matches!(self.tok_at(j), Tok::Punct(":") | Tok::Punct(","))
                    || (matches!(self.tok_at(j), Tok::Punct("}")) && !matches!(self.tok_at(i), Tok::Ident(s) if s == "it"))
            }
            _ => false,
        }
    }

    fn struct_lit(&mut self, path: Vec<String>, generics: Option<Vec<GenericArg>>, start: Span) -> PResult<Expr> {
        self.expect_punct("{")?;
        let mut fields = Vec::new();
        let mut base = None;
        loop {
            self.skip_newlines();
            if self.eat_punct("}") {
                break;
            }
            if self.eat_punct("..") {
                base = Some(Box::new(self.expr()?));
                self.skip_newlines();
                self.expect_punct("}")?;
                break;
            }
            let name = self.member_name()?;
            let value = if self.eat_punct(":") { Some(self.expr()?) } else { None };
            fields.push((name, value));
            self.skip_nl_only();
            if !self.eat_punct(",") {
                self.skip_newlines();
                self.expect_punct("}")?;
                break;
            }
        }
        Ok(Expr::new(ExprKind::StructLit { path, generics, fields, base }, start.to(self.prev_span())))
    }

    /// `(expr)`, `(a, b)`, `()` or an arrow lambda `(a, b) => ...`.
    fn paren_or_lambda(&mut self) -> PResult<Expr> {
        let open = self.pos;
        if let Some(close) = self.matching_close(open) {
            let after = &self.toks[close + 1].tok;
            if matches!(after, Tok::Punct("=>")) || (matches!(after, Tok::Punct(":")) && self.arrow_after_ret_type(close + 2)) {
                return self.paren_lambda();
            }
        }
        let start = self.bump().span;
        let saved = self.no_struct;
        self.no_struct = false;
        let mut elems = Vec::new();
        let mut trailing = false;
        loop {
            if self.eat_punct(")") {
                break;
            }
            elems.push(self.expr_stmt_like()?);
            trailing = false;
            if self.eat_punct(",") {
                trailing = true;
            } else {
                if self.expect_punct(")").is_err() {
                    self.no_struct = saved;
                    return Err(());
                }
                break;
            }
        }
        self.no_struct = saved;
        let sp = start.to(self.prev_span());
        if elems.len() == 1 && !trailing {
            return Ok(Expr::new(ExprKind::Paren(Box::new(elems.pop().unwrap())), sp));
        }
        Ok(Expr::new(ExprKind::Tuple(elems), sp))
    }

    fn arrow_after_ret_type(&self, from: usize) -> bool {
        let mut i = from;
        let mut depth = 0;
        while i < self.toks.len() {
            match &self.toks[i].tok {
                Tok::Punct("=>") if depth == 0 => return true,
                Tok::Punct("(") | Tok::Punct("[") | Tok::Punct("<") => depth += 1,
                Tok::Punct(")") | Tok::Punct("]") | Tok::Punct(">") => depth -= 1,
                Tok::Newline | Tok::Eof | Tok::Punct("{") | Tok::Punct("}") | Tok::Punct("=") | Tok::Punct(",") if depth == 0 => return false,
                _ => {}
            }
            i += 1;
        }
        false
    }

    /// `(a, b: T): R => body`
    fn paren_lambda(&mut self) -> PResult<Expr> {
        let start = self.expect_punct("(")?;
        let mut params = Vec::new();
        loop {
            if self.eat_punct(")") {
                break;
            }
            let pat = self.pattern_no_or()?;
            let ty = if self.eat_punct(":") { Some(self.ty()?) } else { None };
            params.push(LambdaParam { pat, ty });
            if !self.eat_punct(",") {
                self.expect_punct(")")?;
                break;
            }
        }
        let ret = if self.eat_punct(":") { Some(self.ty()?) } else { None };
        if !self.eat_punct("=>") && !self.is_punct("{") {
            return self.expected("`=>`");
        }
        let body = self.arrow_body()?;
        let sp = start.to(body.span);
        Ok(Expr::new(ExprKind::Lambda { params, ret, body: Box::new(body), is_move: false, implicit_it: false, is_async: false }, sp))
    }

    fn arrow_body(&mut self) -> PResult<Expr> {
        if self.is_punct("{") {
            let b = self.block()?;
            let sp = b.span;
            return Ok(Expr::new(ExprKind::Block(b), sp));
        }
        self.skip_nl_only();
        self.expr_stmt_like()
    }

    fn arrow_lambda_single(&mut self) -> PResult<Expr> {
        let start = self.span();
        let pat = self.pattern_no_or()?;
        self.expect_punct("=>")?;
        let body = self.arrow_body()?;
        let sp = start.to(body.span);
        Ok(Expr::new(
            ExprKind::Lambda { params: vec![LambdaParam { pat, ty: None }], ret: None, body: Box::new(body), is_move: false, implicit_it: false, is_async: false },
            sp,
        ))
    }

    /// Rust closure `|a, b| expr` / `|| expr` / `|x: i32| -> i32 { ... }`
    fn rust_closure(&mut self, is_move: bool, start: Span) -> PResult<Expr> {
        let mut params = Vec::new();
        if !self.eat_punct("||") {
            self.expect_punct("|")?;
            loop {
                if self.eat_punct("|") {
                    break;
                }
                let pat = self.pattern_no_or()?;
                let ty = if self.eat_punct(":") { Some(self.ty()?) } else { None };
                params.push(LambdaParam { pat, ty });
                if !self.eat_punct(",") {
                    self.expect_punct("|")?;
                    break;
                }
            }
        }
        let ret = if self.eat_punct("->") { Some(self.ty()?) } else { None };
        let body = if self.is_punct("{") {
            let b = self.block()?;
            let sp = b.span;
            Expr::new(ExprKind::Block(b), sp)
        } else {
            // as in Rust, `|n| total += n` assigns in the body
            self.expr_stmt_like()?
        };
        let sp = start.to(body.span);
        Ok(Expr::new(ExprKind::Lambda { params, ret, body: Box::new(body), is_move, implicit_it: false, is_async: false }, sp))
    }

    /// Kotlin lambda `{ a, b -> body }` or `{ body using it }`.
    fn lambda_braces(&mut self) -> PResult<Expr> {
        let start = self.expect_punct("{")?;
        // capture list `[a, b = expr]`, then a parameter list followed by ->
        let save = self.pos;
        let nd = self.diags.len();
        self.skip_nl_only();
        let captures = if self.is_punct("[") {
            match self.capture_list() {
                Ok(c) => c,
                Err(()) => {
                    self.pos = save;
                    self.diags.truncate(nd);
                    self.skip_nl_only();
                    Vec::new()
                }
            }
        } else {
            Vec::new()
        };
        let save = self.pos;
        let nd = self.diags.len();
        let mut params = Vec::new();
        // `{ f: FnMut(i32) -> f(1) }`: when the `->` after a parameter's `Fn(..)` type can't be its
        // return type, it is the lambda's arrow
        for fn_ret in [true, false] {
            self.fn_ret = fn_ret;
            let r: PResult<()> = (|| {
                if !captures.is_empty() && self.eat_punct("->") {
                    return Ok(());
                }
                loop {
                    let pat = self.pattern_no_or()?;
                    let ty = if self.eat_punct(":") { Some(self.ty()?) } else { None };
                    params.push(LambdaParam { pat, ty });
                    if self.eat_punct(",") {
                        continue;
                    }
                    if self.eat_punct("->") {
                        return Ok(());
                    }
                    return Err(());
                }
            })();
            self.fn_ret = true;
            if r.is_ok() {
                break;
            }
            self.pos = save;
            self.diags.truncate(nd);
            params.clear();
        }
        let stmts = self.stmts_until_close()?;
        let sp = start.to(self.prev_span());
        let body = if stmts.len() == 1 && matches!(&stmts[0], Stmt::Expr { semi: false, .. }) {
            match stmts.into_iter().next().unwrap() {
                Stmt::Expr { expr, .. } => expr,
                _ => unreachable!(),
            }
        } else {
            Expr::new(ExprKind::Block(Block { stmts, span: sp }), sp)
        };
        let implicit_it = params.is_empty();
        let lambda = Expr::new(ExprKind::Lambda { params, ret: None, body: Box::new(body), is_move: !captures.is_empty(), implicit_it, is_async: false }, sp);
        if captures.is_empty() {
            return Ok(lambda);
        }
        // `{ [a] x -> body }` is `{ val a = a.clone(); move { x -> body } }`
        let mut stmts = captures;
        stmts.push(Stmt::Expr { expr: lambda, semi: false });
        Ok(Expr::new(ExprKind::Block(Block { stmts, span: sp }), sp))
    }

    /// `[a, b = expr] ->` at the start of a brace lambda: values the lambda moves in, `a` meaning
    /// `a = a.clone()`. Fails (so the caller backtracks) unless a parameter list or `->` follows.
    fn capture_list(&mut self) -> PResult<Vec<Stmt>> {
        self.expect_punct("[")?;
        let mut out = Vec::new();
        loop {
            let span = self.span();
            let name = self.ident()?;
            let init = if self.eat_punct("=") {
                self.expr()?
            } else {
                let recv = Expr::new(ExprKind::Path { segs: vec![name.clone()], generics: None, global: false }, span);
                Expr::new(ExprKind::MethodCall { recv: Box::new(recv), name: "clone".into(), generics: None, args: vec![], safe: false }, span)
            };
            let pat = Pat::Ident { name, by_ref: false, mutable: false, sub: None, span };
            out.push(Stmt::Let { pat, mutable: false, ty: None, init: Some(init), else_: None, span: span.to(self.prev_span()) });
            if !self.eat_punct(",") {
                break;
            }
        }
        self.expect_punct("]")?;
        // a parameter list or `->` must follow; otherwise this was an array literal
        let save = self.pos;
        let ok = self.is_punct("->")
            || [true, false].into_iter().any(|fn_ret| {
                self.fn_ret = fn_ret;
                let r: PResult<()> = (|| loop {
                    self.pattern_no_or()?;
                    if self.eat_punct(":") {
                        self.ty()?;
                    }
                    if self.eat_punct(",") {
                        continue;
                    }
                    return if self.is_punct("->") { Ok(()) } else { Err(()) };
                })();
                self.fn_ret = true;
                self.pos = save;
                r.is_ok()
            });
        if ok {
            Ok(out)
        } else {
            Err(())
        }
    }

    fn if_expr(&mut self) -> PResult<Expr> {
        let start = self.bump().span;
        let (cond, parens) = self.if_header()?;
        let then = self.branch_body(parens)?;
        // `else ->` is the next arm of an enclosing `when`, not this `if`'s else
        let mut n = 0;
        while matches!(self.tok_at(n), Tok::Newline) {
            n += 1;
        }
        let when_arm = self.is_punct_at(n + 1, "->");
        let else_ = if !when_arm && self.peek_past_newlines_kw("else") {
            self.bump();
            if self.is_kw("if") {
                Some(Box::new(self.if_expr()?))
            } else if self.peek_past_newlines_punct("{") {
                let b = self.block()?;
                let sp = b.span;
                Some(Box::new(Expr::new(ExprKind::Block(b), sp)))
            } else {
                self.skip_nl_only();
                Some(Box::new(self.expr_stmt_like()?))
            }
        } else {
            None
        };
        Ok(Expr::new(ExprKind::If { cond: Box::new(cond), then, else_ }, start.to(self.prev_span())))
    }

    /// A labeled loop or block, after its label (`'outer:` or `outer@`).
    fn labeled(&mut self, label: String, start: Span) -> PResult<Expr> {
        self.bump();
        self.bump();
        let label = Some(label);
        if self.is_kw("loop") {
            self.bump();
            let body = self.block()?;
            return Ok(Expr::new(ExprKind::Loop { label, body }, start.to(self.prev_span())));
        }
        if self.is_kw("while") {
            return self.while_expr(label, start);
        }
        if self.is_kw("for") {
            return self.for_expr(label, start);
        }
        if self.is_punct("{") {
            let block = self.block()?;
            return Ok(Expr::new(ExprKind::LabeledBlock { label: label.unwrap(), block }, start.to(self.prev_span())));
        }
        self.expected("`loop`, `while`, `for` or `{` after label")
    }

    /// The label of `break 'outer` or Kotlin's `break@outer`.
    fn jump_label(&mut self) -> Option<String> {
        match self.tok().clone() {
            Tok::Lifetime(l) => {
                self.bump();
                Some(l)
            }
            Tok::Punct("@") if matches!(self.tok_at(1), Tok::Ident(_)) => {
                self.bump();
                let Tok::Ident(l) = self.bump().tok else { unreachable!() };
                Some(l)
            }
            _ => None,
        }
    }

    fn for_expr(&mut self, label: Option<String>, start: Span) -> PResult<Expr> {
        self.bump(); // for
        let parens = self.is_punct("(");
        if parens {
            self.bump();
        }
        let pat = self.pattern()?;
        if !self.eat_kw("in") {
            return self.expected("`in`");
        }
        let iter = if parens {
            let saved = self.no_struct;
            self.no_struct = false;
            let e = self.expr();
            self.no_struct = saved;
            let e = e?;
            self.expect_punct(")")?;
            e
        } else {
            let saved = self.no_struct;
            self.no_struct = true;
            let e = self.expr();
            self.no_struct = saved;
            e?
        };
        let body = self.branch_body(parens)?;
        Ok(Expr::new(ExprKind::For { label, pat, iter: Box::new(iter), body }, start.to(self.prev_span())))
    }

    fn while_expr(&mut self, label: Option<String>, start: Span) -> PResult<Expr> {
        self.bump(); // while
        let (cond, parens) = self.if_header()?;
        let body = self.branch_body(parens)?;
        Ok(Expr::new(ExprKind::While { label, cond: Box::new(cond), body }, start.to(self.prev_span())))
    }

    fn when_expr(&mut self) -> PResult<Expr> {
        let start = self.bump().span;
        let mut binding = None;
        let subject = if self.is_punct("(") {
            self.bump();
            let saved = self.no_struct;
            self.no_struct = false;
            if self.is_kw("val") || self.is_kw("var") || self.is_kw("let") {
                self.bump();
                binding = Some(self.pattern_no_or()?);
                self.expect_punct("=")?;
            }
            let e = self.expr();
            self.no_struct = saved;
            let e = e?;
            self.expect_punct(")")?;
            Some(Box::new(e))
        } else {
            None
        };
        self.skip_nl_only();
        self.expect_punct("{")?;
        let mut arms = Vec::new();
        loop {
            self.skip_newlines();
            if self.eat_punct("}") {
                break;
            }
            if self.at_eof() {
                return self.expected("`}`");
            }
            let astart = self.span();
            let arm_pos = self.pos;
            let r: PResult<WhenArm> = (|| {
                let attrs = self.arm_attrs()?;
                let mut conds = Vec::new();
                loop {
                    if self.eat_kw("else") {
                        conds.push(WhenCond::Else);
                    } else if self.is_kw("in") {
                        self.bump();
                        conds.push(WhenCond::In { expr: self.when_cond_expr()?, negated: false });
                    } else if self.is_punct("!") && self.is_kw_at(1, "in") {
                        self.bump();
                        self.bump();
                        conds.push(WhenCond::In { expr: self.when_cond_expr()?, negated: true });
                    } else if self.is_kw("is") {
                        self.bump();
                        conds.push(WhenCond::Pat(self.pattern()?));
                    } else if subject.is_some() {
                        conds.push(WhenCond::Pat(self.pattern()?));
                    } else {
                        conds.push(WhenCond::Expr(self.when_cond_expr()?));
                    }
                    if !self.eat_punct(",") {
                        break;
                    }
                    self.skip_nl_only();
                }
                let guard = if self.eat_kw("if") { Some(self.when_cond_expr()?) } else { None };
                if !self.eat_punct("->") && !self.eat_punct("=>") {
                    return self.expected("`->`");
                }
                self.skip_nl_only();
                let body = if self.is_punct("{") {
                    let b = self.block()?;
                    let sp = b.span;
                    Expr::new(ExprKind::Block(b), sp)
                } else {
                    self.expr_stmt_like()?
                };
                Ok(WhenArm { attrs, conds, guard, body, span: astart.to(self.prev_span()) })
            })();
            match r {
                Ok(arm) => arms.push(arm),
                Err(()) => self.recover_arm(arm_pos),
            }
            self.eat_punct(",");
        }
        Ok(Expr::new(ExprKind::When { subject, binding, arms, rust_match: false }, start.to(self.prev_span())))
    }

    /// Expression in a `when` condition: stops before `->` and `,`.
    fn when_cond_expr(&mut self) -> PResult<Expr> {
        let saved = self.no_struct;
        self.no_struct = false;
        let e = self.range_expr();
        self.no_struct = saved;
        e
    }

    fn match_expr(&mut self) -> PResult<Expr> {
        let start = self.bump().span;
        let saved = self.no_struct;
        self.no_struct = true;
        let subject = if self.is_punct("(") && self.matching_close(self.pos).is_some_and(|c| matches!(self.toks[c + 1].tok, Tok::Punct("{") | Tok::Newline)) {
            self.bump();
            self.no_struct = false;
            let e = self.expr();
            self.no_struct = true;
            let e = e?;
            self.expect_punct(")")?;
            e
        } else {
            self.expr()?
        };
        self.no_struct = saved;
        self.skip_nl_only();
        self.expect_punct("{")?;
        let mut arms = Vec::new();
        loop {
            self.skip_newlines();
            while self.eat_punct(",") {
                self.skip_newlines();
            }
            if self.eat_punct("}") {
                break;
            }
            if self.at_eof() {
                return self.expected("`}`");
            }
            let astart = self.span();
            let arm_pos = self.pos;
            let r: PResult<WhenArm> = (|| {
                let attrs = self.arm_attrs()?;
                let pat = if self.eat_kw("else") { None } else { Some(self.pattern()?) };
                let guard = if self.eat_kw("if") { Some(self.when_cond_expr()?) } else { None };
                if !self.eat_punct("=>") && !self.eat_punct("->") {
                    return self.expected("`=>`");
                }
                self.skip_nl_only();
                let body = if self.is_punct("{") {
                    let b = self.block()?;
                    let sp = b.span;
                    Expr::new(ExprKind::Block(b), sp)
                } else {
                    self.expr_stmt_like()?
                };
                let conds = vec![match pat {
                    Some(p) => WhenCond::Pat(p),
                    None => WhenCond::Else,
                }];
                Ok(WhenArm { attrs, conds, guard, body, span: astart.to(self.prev_span()) })
            })();
            match r {
                Ok(a) => arms.push(a),
                Err(()) => self.recover_arm(arm_pos),
            }
            self.eat_punct(",");
        }
        Ok(Expr::new(ExprKind::When { subject: Some(Box::new(subject)), binding: None, arms, rust_match: true }, start.to(self.prev_span())))
    }

    // ------------------------------------------------------------------ patterns

    pub fn pattern(&mut self) -> PResult<Pat> {
        self.eat_punct("|");
        let first = self.pattern_no_or()?;
        if !self.is_punct("|") {
            return Ok(first);
        }
        let mut alts = vec![first];
        while self.eat_punct("|") {
            alts.push(self.pattern_no_or()?);
        }
        Ok(Pat::Or(alts))
    }

    /// `(a: Int, b: String)` after `val`: the plain pattern and the tuple type it declares
    /// (`_` for parts without a type). None, with nothing consumed, when no part has a type.
    fn typed_tuple_pattern(&mut self) -> Option<(Pat, Type)> {
        if !self.is_punct("(") {
            return None;
        }
        let (save, nd) = (self.pos, self.diags.len());
        let parse = |p: &mut Self| -> PResult<Option<(Pat, Type)>> {
            p.bump();
            let (mut pats, mut tys, mut any) = (Vec::new(), Vec::new(), false);
            while !p.eat_punct(")") {
                pats.push(p.pattern()?);
                let typed = p.eat_punct(":");
                any |= typed;
                tys.push(if typed { p.ty()? } else { Type::Infer });
                if !p.eat_punct(",") {
                    p.expect_punct(")")?;
                    break;
                }
            }
            Ok((any && pats.len() > 1).then_some((Pat::Tuple(pats), Type::Tuple(tys))))
        };
        match parse(self) {
            Ok(Some(r)) => Some(r),
            _ => {
                self.pos = save;
                self.diags.truncate(nd);
                None
            }
        }
    }

    fn pattern_no_or(&mut self) -> PResult<Pat> {
        let p = self.pattern_primary()?;
        // range patterns
        if self.is_punct("..=") || self.is_punct("...") || self.is_punct("..") || self.is_punct("..<") {
            if let Pat::Lit(lo) = &p {
                let inclusive = self.is_punct("..=") || self.is_punct("...");
                self.bump();
                let hi = if self.lit_pattern_start() { Some(Box::new(self.lit_pattern_expr()?)) } else { None };
                return Ok(Pat::Range { lo: Some(lo.clone()), hi, inclusive });
            }
            if let Pat::Path(segs, _) = &p {
                let inclusive = self.is_punct("..=");
                self.bump();
                let lo = Expr::new(ExprKind::Path { segs: segs.clone(), generics: None, global: false }, Span::default());
                let hi = if self.lit_pattern_start() || self.is_ident() { Some(Box::new(self.lit_pattern_expr()?)) } else { None };
                return Ok(Pat::Range { lo: Some(Box::new(lo)), hi, inclusive });
            }
        }
        Ok(p)
    }

    fn lit_pattern_start(&self) -> bool {
        matches!(self.tok(), Tok::Int(_) | Tok::Float(_) | Tok::Char(_) | Tok::Punct("-"))
    }

    fn lit_pattern_expr(&mut self) -> PResult<Expr> {
        let start = self.span();
        if self.eat_punct("-") {
            let e = self.primary()?;
            let sp = start.to(e.span);
            return Ok(Expr::new(ExprKind::Unary(UnOp::Neg, Box::new(e)), sp));
        }
        if self.is_ident() {
            return self.path_expr();
        }
        self.primary()
    }

    fn pattern_primary(&mut self) -> PResult<Pat> {
        let start = self.span();
        match self.tok().clone() {
            Tok::Ident(s) if s == "_" => {
                self.bump();
                Ok(Pat::Wild)
            }
            Tok::Punct("..") => {
                self.bump();
                Ok(Pat::Rest)
            }
            Tok::Punct("..=") => {
                self.bump();
                let hi = self.lit_pattern_expr()?;
                Ok(Pat::Range { lo: None, hi: Some(Box::new(hi)), inclusive: true })
            }
            Tok::Ident(s) if s == "null" => {
                self.bump();
                Ok(Pat::Null)
            }
            Tok::Ident(s) if s == "true" || s == "false" => {
                self.bump();
                Ok(Pat::Lit(Box::new(Expr::new(ExprKind::Bool(s == "true"), start))))
            }
            Tok::Int(_) | Tok::Float(_) | Tok::Char(_) | Tok::Punct("-") => Ok(Pat::Lit(Box::new(self.lit_pattern_expr()?))),
            Tok::Str(parts) => {
                self.bump();
                let parts = lexer::dollar_digits_as_text(parts);
                if parts.iter().any(|p| matches!(p, StrPart::Interp { .. })) {
                    self.diags.push(Diag::error(start, "string templates cannot be used as patterns"));
                }
                Ok(Pat::Lit(Box::new(Expr::new(ExprKind::Str(parts), start))))
            }
            Tok::RawStr(s) => {
                self.bump();
                Ok(Pat::Lit(Box::new(Expr::new(ExprKind::RawStr(s), start))))
            }
            Tok::Punct("&") | Tok::Punct("&&") => {
                let double = self.is_punct("&&");
                self.bump();
                let mutable = self.eat_kw("mut");
                let inner = self.pattern_no_or()?;
                let p = Pat::Ref { mutable, pat: Box::new(inner) };
                Ok(if double { Pat::Ref { mutable: false, pat: Box::new(p) } } else { p })
            }
            Tok::Punct("(") => {
                self.bump();
                let mut elems = Vec::new();
                let mut trailing = false;
                loop {
                    if self.eat_punct(")") {
                        break;
                    }
                    elems.push(self.pattern()?);
                    trailing = false;
                    if self.eat_punct(",") {
                        trailing = true;
                    } else {
                        self.expect_punct(")")?;
                        break;
                    }
                }
                if elems.len() == 1 && !trailing {
                    return Ok(elems.pop().unwrap());
                }
                Ok(Pat::Tuple(elems))
            }
            Tok::Punct("[") => {
                self.bump();
                let mut elems = Vec::new();
                loop {
                    if self.eat_punct("]") {
                        break;
                    }
                    elems.push(self.pattern()?);
                    if !self.eat_punct(",") {
                        self.expect_punct("]")?;
                        break;
                    }
                }
                Ok(Pat::Slice(elems))
            }
            Tok::Ident(s) if s == "ref" => {
                self.bump();
                let mutable = self.eat_kw("mut");
                let name = self.ident()?;
                Ok(Pat::Ident { name, by_ref: true, mutable, sub: None, span: start.to(self.prev_span()) })
            }
            Tok::Ident(s) if s == "mut" => {
                self.bump();
                let name = self.ident()?;
                Ok(Pat::Ident { name, by_ref: false, mutable: true, sub: None, span: start.to(self.prev_span()) })
            }
            Tok::Ident(_) | Tok::RawIdent(_) | Tok::Punct("::") => {
                let mut segs = Vec::new();
                if self.eat_punct("::") {
                    segs.push(String::new());
                }
                let first = match self.tok().clone() {
                    Tok::Ident(s) if s == "Self" || s == "crate" || s == "super" || !is_reserved(&s) => {
                        self.bump();
                        s
                    }
                    Tok::RawIdent(s) => {
                        self.bump();
                        format!("r#{}", s)
                    }
                    _ => return self.expected("pattern"),
                };
                segs.push(first);
                while (self.is_punct("::") || self.is_punct(".")) && matches!(self.tok_at(1), Tok::Ident(_) | Tok::RawIdent(_)) {
                    self.bump();
                    segs.push(self.member_name()?);
                }
                if self.is_punct("::") && self.is_punct_at(1, "<") {
                    self.bump();
                    self.generic_args()?;
                }
                let span = start.to(self.prev_span());
                if self.is_punct("(") {
                    self.bump();
                    let mut elems = Vec::new();
                    loop {
                        if self.eat_punct(")") {
                            break;
                        }
                        elems.push(self.pattern()?);
                        if !self.eat_punct(",") {
                            self.expect_punct(")")?;
                            break;
                        }
                    }
                    return Ok(Pat::TupleStruct { path: segs, elems, span });
                }
                if self.is_punct("{") && (segs.len() > 1 || self.type_like(&segs[0])) {
                    self.bump();
                    let mut fields = Vec::new();
                    let mut rest = false;
                    loop {
                        self.skip_newlines();
                        if self.eat_punct("}") {
                            break;
                        }
                        if self.eat_punct("..") {
                            rest = true;
                            self.skip_newlines();
                            self.expect_punct("}")?;
                            break;
                        }
                        let fstart = self.span();
                        let by_ref = self.eat_kw("ref");
                        let mutable = self.eat_kw("mut");
                        let name = self.member_name()?;
                        if self.eat_punct(":") {
                            fields.push((name, Some(self.pattern()?)));
                        } else if by_ref || mutable {
                            let sp = fstart.to(self.prev_span());
                            fields.push((name.clone(), Some(Pat::Ident { name, by_ref, mutable, sub: None, span: sp })));
                        } else {
                            fields.push((name, None));
                        }
                        self.skip_newlines();
                        if !self.eat_punct(",") {
                            self.skip_newlines();
                            self.expect_punct("}")?;
                            break;
                        }
                    }
                    return Ok(Pat::Struct { path: segs, fields, rest, span });
                }
                if segs.len() == 1 && !self.type_like(&segs[0]) && !is_const_like(&segs[0]) {
                    let name = segs.pop().unwrap();
                    let sub = if self.eat_punct("@") { Some(Box::new(self.pattern_no_or()?)) } else { None };
                    return Ok(Pat::Ident { name, by_ref: false, mutable: false, sub, span });
                }
                Ok(Pat::Path(segs, span))
            }
            _ => self.expected("pattern"),
        }
    }
}

fn is_const_like(s: &str) -> bool {
    s.len() > 1 && s.chars().all(|c| c.is_uppercase() || c == '_' || c.is_ascii_digit()) && s.chars().next().unwrap().is_uppercase()
}

fn set_all_mut(p: &mut Pat) {
    match p {
        Pat::Ident { mutable, sub, .. } => {
            *mutable = true;
            if let Some(s) = sub {
                set_all_mut(s);
            }
        }
        Pat::Tuple(v) | Pat::Slice(v) | Pat::Or(v) => v.iter_mut().for_each(set_all_mut),
        Pat::TupleStruct { elems, .. } => elems.iter_mut().for_each(set_all_mut),
        Pat::Struct { fields, .. } => {
            for (name, p) in fields.iter_mut() {
                match p {
                    Some(p) => set_all_mut(p),
                    None => {
                        *p = Some(Pat::Ident { name: name.clone(), by_ref: false, mutable: true, sub: None, span: Span::default() });
                    }
                }
            }
        }
        Pat::Typed { pat, .. } => set_all_mut(pat),
        _ => {}
    }
}

fn bin(op: BinOp, l: Expr, r: Expr) -> Expr {
    let sp = l.span.to(r.span);
    Expr::new(ExprKind::Binary(op, Box::new(l), Box::new(r)), sp)
}

/// Which expressions may take a trailing lambda without parentheses: `foo { }`, `a.b { }`.
fn is_lambda_callee(e: &Expr) -> bool {
    match &e.kind {
        ExprKind::Path { segs, .. } => segs.last().is_some_and(|s| !is_type_like(s)),
        _ => false,
    }
}

/// Removes common indentation from raw Rust blocks.
fn dedent_raw(s: &str) -> String {
    let lines: Vec<&str> = s.lines().collect();
    let min = lines
        .iter()
        .filter(|l| !l.trim().is_empty())
        .map(|l| l.len() - l.trim_start().len())
        .min()
        .unwrap_or(0);
    let mut out = Vec::new();
    for l in lines {
        if l.trim().is_empty() {
            out.push(String::new());
        } else {
            out.push(l[min.min(l.len())..].to_string());
        }
    }
    while out.first().is_some_and(|l| l.is_empty()) {
        out.remove(0);
    }
    while out.last().is_some_and(|l| l.is_empty()) {
        out.pop();
    }
    out.join("\n")
}

/// `a.b.c` written with dots (identifiers only), as path segments.
fn dot_path(e: &Expr) -> Option<Vec<String>> {
    match &e.kind {
        ExprKind::Path { segs, generics: None, global: false } => Some(segs.clone()),
        ExprKind::Field { recv, name, safe: false } => {
            let mut v = dot_path(recv)?;
            v.push(name.clone());
            Some(v)
        }
        _ => None,
    }
}
