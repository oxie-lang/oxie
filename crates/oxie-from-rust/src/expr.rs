//! Rust statements, expressions and patterns → Oxie.

use syn::punctuated::Punctuated;

use crate::fmtstr::{self, ArgRef, Interp, Piece, TPart};
use crate::printer::{end_line_of, indent, line_of, mentions, BlockMode, Printer};
use crate::types::{id, ident, tok};

/// How an expression's value is used.
#[derive(Clone, Default)]
pub struct Ctx {
    /// the value is discarded
    pub stmt: bool,
    /// declared type the value goes to (for implicit `String` conversions)
    pub expect: Option<syn::Type>,
    /// tail of a function translated to `throws`: `Ok(v)` → `v`, `Err(e)` → `throw e`
    pub throws_tail: bool,
    /// the value is what the function returns (its body's tail, or a branch of it)
    pub fn_tail: bool,
    /// a `None` branch stays `None`: Oxie makes a branch next to `null` optional
    /// (`if (c) x else null`), which a value that already is an `Option` mustn't be
    pub keep_none: bool,
}

impl Ctx {
    fn value() -> Ctx {
        Ctx::default()
    }
    fn stmt() -> Ctx {
        Ctx { stmt: true, ..Ctx::default() }
    }
    fn expect(t: Option<&syn::Type>) -> Ctx {
        Ctx { expect: t.cloned(), ..Ctx::default() }
    }
}

/// Functions Oxie knows by name; a method with one of these names keeps its `self.`.
const BUILTIN_FNS: &[&str] = &[
    "println",
    "print",
    "eprintln",
    "eprint",
    "format",
    "panic",
    "assert",
    "assert_eq",
    "assert_ne",
    "vec",
    "run",
    "repeat",
    "listOf",
    "mutableListOf",
    "mapOf",
    "setOf",
    "arrayOf",
    "todo",
    "dbg",
    "matches",
    "unreachable",
    "write",
    "writeln",
    "drop",
    "Some",
    "Ok",
    "Err",
    "Box",
    "require",
    "check",
    "error",
    "with",
    "apply",
    "also",
];

pub fn macro_args(m: &syn::Macro) -> Option<Vec<syn::Expr>> {
    let p: Punctuated<syn::Expr, syn::Token![,]> = m.parse_body_with(Punctuated::parse_terminated).ok()?;
    Some(p.into_iter().collect())
}

fn is_path(e: &syn::Expr, name: &str) -> bool {
    matches!(e, syn::Expr::Path(p) if p.qself.is_none() && p.path.is_ident(name))
}

fn call_of<'e>(e: &'e syn::Expr, name: &str) -> Option<&'e Punctuated<syn::Expr, syn::Token![,]>> {
    if let syn::Expr::Call(c) = e {
        if let syn::Expr::Path(p) = &*c.func {
            if p.qself.is_none() && p.path.segments.len() == 1 && p.path.segments[0].ident == name {
                return Some(&c.args);
            }
        }
    }
    None
}

fn is_string_type(t: &syn::Type) -> bool {
    matches!(t, syn::Type::Path(p) if p.qself.is_none() && (p.path.is_ident("String") || tok(&p.path) == "std::string::String"))
}

fn vec_elem(t: &syn::Type) -> Option<&syn::Type> {
    let syn::Type::Path(p) = t else { return None };
    let last = p.path.segments.last()?;
    if last.ident != "Vec" {
        return None;
    }
    let syn::PathArguments::AngleBracketed(a) = &last.arguments else { return None };
    match a.args.first()? {
        syn::GenericArgument::Type(t) => Some(t),
        _ => None,
    }
}

fn str_lit(e: &syn::Expr) -> Option<&syn::LitStr> {
    match e {
        syn::Expr::Lit(syn::ExprLit { lit: syn::Lit::Str(s), .. }) => Some(s),
        syn::Expr::Paren(p) => str_lit(&p.expr),
        _ => None,
    }
}

/// `String::from("x")`, `"x".to_string()`, `"x".to_owned()`, `"x".into()`, `String::from` → the literal.
fn string_literal_conversion(e: &syn::Expr) -> Option<&syn::LitStr> {
    match e {
        syn::Expr::MethodCall(m) if m.args.is_empty() && matches!(m.method.to_string().as_str(), "to_string" | "to_owned" | "into") => str_lit(&m.receiver),
        syn::Expr::Call(c) if c.args.len() == 1 => {
            let syn::Expr::Path(p) = &*c.func else { return None };
            let t = tok(&p.path);
            if t == "String::from" || t == "String::from_str" {
                str_lit(&c.args[0])
            } else {
                None
            }
        }
        _ => None,
    }
}

/// Expressions without side effects that may be evaluated in any order.
fn is_simple(e: &syn::Expr) -> bool {
    match e {
        syn::Expr::Path(_) | syn::Expr::Lit(_) => true,
        syn::Expr::Field(f) => is_simple(&f.base),
        syn::Expr::Reference(r) => is_simple(&r.expr),
        syn::Expr::Paren(p) => is_simple(&p.expr),
        syn::Expr::Unary(u) => matches!(u.op, syn::UnOp::Deref(_)) && is_simple(&u.expr),
        _ => false,
    }
}

/// An expression that can be followed by `?`, `.x` without parentheses.
fn is_postfix_safe(e: &syn::Expr) -> bool {
    matches!(
        e,
        syn::Expr::Path(_)
            | syn::Expr::Call(_)
            | syn::Expr::MethodCall(_)
            | syn::Expr::Field(_)
            | syn::Expr::Index(_)
            | syn::Expr::Paren(_)
            | syn::Expr::Macro(_)
            | syn::Expr::Lit(_)
            | syn::Expr::Tuple(_)
            | syn::Expr::Array(_)
            | syn::Expr::Await(_)
            | syn::Expr::Try(_)
    )
}

impl Printer<'_> {
    // ------------------------------------------------------------------ blocks

    pub fn block_body(&mut self, b: &syn::Block, mode: BlockMode) -> String {
        let ctx = match mode {
            BlockMode::UnitFn => Ctx { stmt: true, throws_tail: self.cx.throws, ..Ctx::default() },
            BlockMode::ValueFn => Ctx { expect: self.cx.ret.clone(), throws_tail: self.cx.throws, fn_tail: true, ..Ctx::default() },
            BlockMode::Value => Ctx::value(),
        };
        let semi_rule = mode == BlockMode::Value;
        self.stmts(b, &ctx, semi_rule)
    }

    fn stmts(&mut self, b: &syn::Block, ctx: &Ctx, semi_rule: bool) -> String {
        let lo = line_of(b);
        let mut out: Vec<String> = Vec::new();
        let mut prev_end: Option<usize> = None;
        let n = b.stmts.len();
        // `#[cfg(a)] { x }` then `#[cfg(b)] { y }` at the end: the configuration keeps one of them,
        // which is the block's value, so each is translated as the last statement
        let cfg_value = |s: &syn::Stmt| matches!(s, syn::Stmt::Expr(e, None) if split_stmt_attrs(e).is_some());
        let mut value_from = n.saturating_sub(1);
        if b.stmts.last().is_some_and(cfg_value) {
            while value_from > 0 && cfg_value(&b.stmts[value_from - 1]) {
                value_from -= 1;
            }
        }
        for (i, s) in b.stmts.iter().enumerate() {
            let start = stmt_start_line(s);
            let cs = self.take_comments(lo, start);
            let first_line = if cs.is_empty() { start } else { self.first_comment_line(start, cs.len()) };
            if let Some(pe) = prev_end {
                if first_line > pe + 1 {
                    out.push(String::new());
                }
            }
            out.extend(cs);
            let last = i >= value_from;
            let pending = self.pending.len();
            let mut text = self.stmt(s, last, ctx, semi_rule);
            if self.pending.len() > pending {
                out.extend(self.pending.drain(pending..));
            }
            if text.is_empty() {
                prev_end = Some(end_line_of(s));
                continue;
            }
            if let Some(tc) = self.trailing_comment(end_line_of(s)) {
                text.push(' ');
                text.push_str(&tc);
            }
            out.push(text);
            prev_end = Some(end_line_of(s));
        }
        let tail = self.take_comments(lo, end_line_of(b));
        if !tail.is_empty() {
            out.extend(tail);
        }
        out.join("\n")
    }

    fn first_comment_line(&self, start: usize, count: usize) -> usize {
        // the comments just taken are the last `count` used comments before `start`
        let mut lines: Vec<usize> = self.comments.iter().filter(|c| c.used && c.line < start).map(|c| c.line).collect();
        lines.sort();
        lines.iter().rev().take(count).min().copied().unwrap_or(start)
    }

    fn stmt(&mut self, s: &syn::Stmt, last: bool, ctx: &Ctx, semi_rule: bool) -> String {
        match s {
            syn::Stmt::Local(l) => self.local(l),
            // an item macro (`macro_rules! name { .. }`, `thread_local! { .. }`) is written as in Rust
            syn::Stmt::Item(it @ syn::Item::Macro(_)) => crate::printer::rust_item(it),
            syn::Stmt::Item(it) => {
                let prefix = std::mem::replace(&mut self.mod_prefix, self.cx.fn_path.clone());
                let saved = std::mem::take(&mut self.cx);
                let r = self.items(std::slice::from_ref(it), line_of(it), false);
                self.cx = saved;
                self.mod_prefix = prefix;
                r
            }
            syn::Stmt::Expr(e, semi) => {
                // `match x { #![deny(lint)] .. }` as a statement or a block's value: the same as
                // `#[deny(lint)] match x { .. }`, which Oxie writes `@deny(lint) when (x) { .. }`
                if let syn::Expr::Match(m) = e {
                    if m.attrs.iter().any(|a| matches!(a.style, syn::AttrStyle::Inner(_))) {
                        let mut m = m.clone();
                        for a in &mut m.attrs {
                            a.style = syn::AttrStyle::Outer;
                        }
                        return self.stmt(&syn::Stmt::Expr(syn::Expr::Match(m), *semi), last, ctx, semi_rule);
                    }
                }
                // attributes written before the statement go before it. A block ending in
                // `#[cfg(a)] { x }` and `#[cfg(b)] { y }` keeps them: Oxie takes the attributed
                // statements at its end as its value, as the configuration leaves one of them
                if let Some((attrs, bare)) = split_stmt_attrs(e) {
                    let (docs, prefix) = self.stmt_attrs(&attrs);
                    let t = self.stmt(&syn::Stmt::Expr(bare, *semi), last, ctx, semi_rule && prefix.is_empty());
                    return if t.is_empty() { t } else { format!("{}{}{}", docs, prefix, t) };
                }
                if last && semi.is_none() {
                    let t = self.ex(e, ctx);
                    if ctx.throws_tail && t == "()" {
                        return String::new();
                    }
                    return t;
                }
                // a throwing call whose result is ignored: Oxie would add `?`
                if semi.is_some() && self.is_throwing_call(e) {
                    return format!("val _ = {}", self.ex(e, &Ctx::value()));
                }
                // `return` ending a block whose value is used: see `Expr::Return` in `ex`
                if last && matches!(e, syn::Expr::Return(_)) && !ctx.stmt && !ctx.fn_tail {
                    return self.ex(e, ctx);
                }
                let mut t = self.ex(e, &Ctx::stmt());
                if last && semi_rule && semi.is_some() && needs_semi(e) {
                    t.push(';');
                }
                t
            }
            syn::Stmt::Macro(m) => {
                let (docs, prefix) = self.stmt_attrs(&m.attrs);
                let e = syn::Expr::Macro(syn::ExprMacro { attrs: Vec::new(), mac: m.mac.clone() });
                let mut t = self.ex(&e, &Ctx::stmt());
                if last && semi_rule && m.semi_token.is_some() && needs_semi(&e) && prefix.is_empty() {
                    t.push(';');
                }
                format!("{}{}{}", docs, prefix, t)
            }
        }
    }

    /// The attributes of a statement: comment lines to print above it, and the Oxie prefix
    /// (`@cfg(test) `). A doc comment documents nothing on a statement (rustc warns about it), so
    /// it becomes an ordinary comment.
    fn stmt_attrs(&self, attrs: &[syn::Attribute]) -> (String, String) {
        let mut docs = String::new();
        let mut prefix = String::new();
        for a in attrs {
            for l in self.attr(a) {
                match l.strip_prefix("///") {
                    Some(doc) => {
                        docs.push_str("//");
                        docs.push_str(doc);
                        docs.push('\n');
                    }
                    None => {
                        prefix.push_str(&l);
                        prefix.push(' ');
                    }
                }
            }
        }
        (docs, prefix)
    }

    fn is_throwing_call(&self, e: &syn::Expr) -> bool {
        match e {
            syn::Expr::Call(c) => match &*c.func {
                syn::Expr::Path(p) => p.path.segments.last().is_some_and(|s| self.info.throwing.contains(&s.ident.to_string())),
                _ => false,
            },
            syn::Expr::MethodCall(m) => self.info.throwing.contains(&m.method.to_string()),
            _ => false,
        }
    }

    fn local(&mut self, l: &syn::Local) -> String {
        let (docs, attrs) = self.stmt_attrs(&l.attrs);
        let attrs = docs + &attrs;
        let (pat, ty) = match &l.pat {
            syn::Pat::Type(pt) => (&*pt.pat, Some(&*pt.ty)),
            p => (p, None),
        };
        let mut ty_text = ty.map(|t| format!(": {}", self.ty(t))).unwrap_or_default();
        // the `Result` the function returns: its error type no longer follows from the
        // function's once the body ends in `r?` (`let r = (|| { .. })(); r`)
        if let (None, syn::Pat::Ident(pi), Some(rt)) = (ty, pat, self.cx.result_ty.clone()) {
            if self.cx.tail_var.as_deref() == Some(pi.ident.to_string().as_str()) && l.init.as_ref().is_some_and(|i| matches!(&*i.expr, syn::Expr::Call(_))) {
                ty_text = format!(": {}", self.ty(&rt));
            }
        }
        let init = l.init.as_ref().map(|i| {
            let v = match &*i.expr {
                syn::Expr::Closure(c) => self.closure(c, true),
                e => self.ex(e, &Ctx::expect(ty)),
            };
            let v = strip_outer_elvis_parens(v);
            (v, i.diverge.as_ref())
        });
        let rhs = |init: &Option<(String, Option<&(syn::token::Else, Box<syn::Expr>)>)>, me: &mut Self| -> String {
            match init {
                Some((v, None)) => format!(" = {}", v),
                Some((v, Some((_, els)))) => {
                    let e = match &**els {
                        syn::Expr::Block(b) if b.label.is_none() => me.block(&b.block, &Ctx::stmt()),
                        other => me.ex(other, &Ctx::stmt()),
                    };
                    format!(" = {} else {}", v, e)
                }
                None => String::new(),
            }
        };
        match pat {
            syn::Pat::Ident(pi) if pi.by_ref.is_none() && pi.subpat.is_none() && init.as_ref().is_none_or(|i| i.1.is_none()) => {
                let kw = if pi.mutability.is_some() { "var" } else { "val" };
                let r = rhs(&init, self);
                format!("{}{} {}{}{}", attrs, kw, id(&pi.ident), ty_text, r)
            }
            syn::Pat::Tuple(t)
                if t.elems.iter().all(|p| matches!(p, syn::Pat::Ident(pi) if pi.by_ref.is_none() && pi.subpat.is_none()) || matches!(p, syn::Pat::Wild(_)))
                    && init.as_ref().is_some_and(|i| i.1.is_none()) =>
            {
                let muts: Vec<bool> = t.elems.iter().map(|p| matches!(p, syn::Pat::Ident(pi) if pi.mutability.is_some())).collect();
                let names: Vec<String> = t
                    .elems
                    .iter()
                    .map(|p| match p {
                        syn::Pat::Ident(pi) => id(&pi.ident),
                        _ => "_".into(),
                    })
                    .collect();
                let idents = t.elems.iter().filter(|p| matches!(p, syn::Pat::Ident(_))).count();
                let n_mut = muts.iter().filter(|m| **m).count();
                let r = rhs(&init, self);
                if n_mut == 0 {
                    format!("{}val ({}){}{}", attrs, names.join(", "), ty_text, r)
                } else if n_mut == idents {
                    format!("{}var ({}){}{}", attrs, names.join(", "), ty_text, r)
                } else {
                    format!("{}let {}{}{}", attrs, self.pat(pat), ty_text, r)
                }
            }
            _ => {
                let r = rhs(&init, self);
                if init.as_ref().is_some_and(|i| i.1.is_some()) {
                    format!("{}val {}{}{}", attrs, self.pat(pat), ty_text, r)
                } else {
                    format!("{}let {}{}{}", attrs, self.pat(pat), ty_text, r)
                }
            }
        }
    }

    /// `{ ... }` with the statements of `b`.
    fn block(&mut self, b: &syn::Block, ctx: &Ctx) -> String {
        let body = self.stmts(b, ctx, !ctx.stmt && !ctx.throws_tail);
        if body.trim().is_empty() {
            "{}".into()
        } else {
            format!("{{\n{}\n}}", indent(&body))
        }
    }

    /// The single tail expression of a block without statements or comments.
    fn single_expr<'b>(&self, b: &'b syn::Block) -> Option<&'b syn::Expr> {
        if b.stmts.len() != 1 || self.has_comments_in(line_of(b), end_line_of(b)) || has_stmt_attrs(&b.stmts[0]) {
            return None;
        }
        match &b.stmts[0] {
            syn::Stmt::Expr(e, None) => Some(e),
            _ => None,
        }
    }

    // ------------------------------------------------------------------ expressions

    pub fn expr(&mut self, e: &syn::Expr) -> String {
        self.ex(e, &Ctx::value())
    }

    pub fn expr_expect(&mut self, e: &syn::Expr, t: Option<&syn::Type>) -> String {
        self.ex(e, &Ctx::expect(t))
    }

    /// The tail of a function translated to `throws`.
    pub fn tail_throws(&mut self, e: &syn::Expr) -> String {
        let ret = self.cx.ret.clone();
        self.ex(e, &Ctx { throws_tail: true, expect: ret, fn_tail: true, ..Ctx::default() })
    }

    /// The tail of a function returning a value.
    pub fn tail_value(&mut self, e: &syn::Expr, t: Option<&syn::Type>) -> String {
        self.ex(e, &Ctx { expect: t.cloned(), fn_tail: true, ..Ctx::default() })
    }

    pub fn ex(&mut self, e: &syn::Expr, ctx: &Ctx) -> String {
        if has_expr_attrs(e) {
            return self.attributed_expr(e);
        }
        if ctx.keep_none && matches!(e, syn::Expr::Path(p) if p.qself.is_none() && p.path.is_ident("None")) {
            return "None".into();
        }
        if ctx.throws_tail {
            if let Some(r) = self.throws_tail_expr(e, ctx) {
                return r;
            }
        }
        if let Some(t) = &ctx.expect {
            if is_string_type(t) {
                if let Some(s) = string_literal_conversion(e) {
                    return self.str_literal(s);
                }
            }
            // `Vec::new()` for a declared list: `[]`
            if vec_elem(t).is_some() && matches!(e, syn::Expr::Call(c) if c.args.is_empty() && matches!(&*c.func, syn::Expr::Path(p) if p.qself.is_none() && tok(&p.path).replace(' ', "") == "Vec::new")) {
                return "[]".into();
            }
        }
        match e {
            syn::Expr::Array(a) => {
                let v: Vec<String> = a.elems.iter().map(|x| self.expr(x)).collect();
                bracket_list(&v)
            }
            syn::Expr::Assign(a) => format!("{} = {}", self.expr(&a.left), self.expr(&a.right)),
            syn::Expr::Async(a) => {
                let saved = self.enter_closure();
                let b = self.block(&a.block, &Ctx::value());
                self.leave_closure(saved);
                format!("async {}{}", if a.capture.is_some() { "move " } else { "" }, b)
            }
            syn::Expr::Await(a) => format!("{}.await", self.postfix(&a.base)),
            syn::Expr::Binary(b) => {
                let right = if matches!(&*b.right, syn::Expr::Macro(_)) { self.operand(&b.right) } else { self.expr(&b.right) };
                format!("{} {} {}", self.operand(&b.left), tok(&b.op), right)
            }
            syn::Expr::Block(b) => {
                let label = b.label.as_ref().map(|l| format!("{}: ", l.name)).unwrap_or_default();
                // a function, method or variable named `run` hides Oxie's `run { }` block
                let shadowed = self.info.run_shadowed || self.cx.bound.contains("run") || self.cx.class.as_ref().is_some_and(|c| c.methods.contains("run")) || self.cx.type_fns.contains("run");
                let run = if shadowed { "'block: " } else { "run " };
                if ctx.throws_tail && label.is_empty() {
                    return format!("{}{}", run, self.block(&b.block, ctx));
                }
                let inner = Ctx { throws_tail: false, ..ctx.clone() };
                let body = self.block(&b.block, &inner);
                if label.is_empty() {
                    format!("{}{}", run, body)
                } else {
                    format!("{}{}", label, body)
                }
            }
            syn::Expr::Break(b) => {
                let mut s = "break".to_string();
                if let Some(l) = &b.label {
                    s.push_str(&format!(" {}", l));
                }
                if let Some(v) = &b.expr {
                    s.push_str(&format!(" {}", self.expr(v)));
                }
                s
            }
            syn::Expr::Call(c) => self.call(c),
            syn::Expr::Cast(c) => format!("{} as {}", self.operand(&c.expr), self.ty(&c.ty)),
            syn::Expr::Closure(c) => self.closure(c, false),
            syn::Expr::Const(c) => format!("const {}", self.block(&c.block, &Ctx::value())),
            syn::Expr::Continue(c) => match &c.label {
                Some(l) => format!("continue {}", l),
                None => "continue".into(),
            },
            syn::Expr::Field(f) => {
                if is_path(&f.base, "self") {
                    if let syn::Member::Named(m) = &f.member {
                        if self.implicit_field(&m.to_string()) {
                            return id(m);
                        }
                    }
                }
                let m = match &f.member {
                    syn::Member::Named(i) => id(i),
                    syn::Member::Unnamed(i) => i.index.to_string(),
                };
                format!("{}.{}", self.postfix(&f.base), m)
            }
            syn::Expr::ForLoop(f) => {
                let label = f.label.as_ref().map(|l| format!("{}: ", l.name)).unwrap_or_default();
                let body = self.block(&f.body, &Ctx::stmt());
                format!("{}for ({} in {}) {}", label, self.pat(&f.pat), self.expr(&f.expr), body)
            }
            syn::Expr::Group(g) => self.ex(&g.expr, ctx),
            syn::Expr::If(i) => {
                let ctx = &self.branch_ctx(e, ctx);
                self.if_expr(i, ctx, true)
            }
            syn::Expr::Index(i) => format!("{}[{}]", self.postfix(&i.expr), self.expr(&i.index)),
            syn::Expr::Infer(_) => "_".into(),
            syn::Expr::Let(l) => self.let_cond(&l.pat, &l.expr),
            syn::Expr::Lit(l) => self.lit(&l.lit),
            syn::Expr::Loop(l) => {
                let label = l.label.as_ref().map(|l| format!("{}: ", l.name)).unwrap_or_default();
                format!("{}loop {}", label, self.block(&l.body, &Ctx::stmt()))
            }
            syn::Expr::Macro(m) => self.mac(&m.mac, ctx),
            syn::Expr::Match(m) => {
                let ctx = &self.branch_ctx(e, ctx);
                self.match_expr(m, ctx)
            }
            syn::Expr::MethodCall(m) => self.method_call(m),
            syn::Expr::Paren(p) => {
                // `(self.f)(x)` calls a closure field; Oxie would read `(f)(x)` as a method call
                if let syn::Expr::Field(f) = &*p.expr {
                    if is_path(&f.base, "self") {
                        return format!("(self.{})", tok(&f.member));
                    }
                }
                format!("({})", self.expr(&p.expr))
            }
            syn::Expr::Path(p) => self.path_expr(p),
            syn::Expr::Range(r) => {
                let mut s = String::new();
                if let Some(a) = &r.start {
                    s.push_str(&self.expr(a));
                }
                s.push_str(match r.limits {
                    syn::RangeLimits::HalfOpen(_) => "..",
                    syn::RangeLimits::Closed(_) => "..=",
                });
                if let Some(b) = &r.end {
                    s.push_str(&self.expr(b));
                }
                s
            }
            syn::Expr::Reference(r) => format!("&{}{}", if r.mutability.is_some() { "mut " } else { "" }, self.expr(&r.expr)),
            syn::Expr::RawAddr(r) => format!("&raw {}{}", if matches!(r.mutability, syn::PointerMutability::Mut(_)) { "mut " } else { "const " }, self.expr(&r.expr)),
            syn::Expr::Repeat(r) => format!("[{}; {}]", self.expr(&r.expr), self.expr(&r.len)),
            syn::Expr::Return(r) => self.return_expr(r),
            syn::Expr::Struct(s) => self.struct_lit(s),
            syn::Expr::Try(t) => {
                if self.cx.throws {
                    if let Some(args) = call_of(&t.expr, "Err") {
                        if args.len() == 1 {
                            return format!("throw {}", self.thrown(&args[0]));
                        }
                    }
                }
                format!("{}?", self.postfix(&t.expr))
            }
            syn::Expr::Tuple(t) => {
                let v: Vec<String> = t.elems.iter().map(|x| self.expr(x)).collect();
                if v.len() == 1 {
                    format!("({},)", v[0])
                } else {
                    format!("({})", v.join(", "))
                }
            }
            syn::Expr::Unary(u) => {
                let op = match u.op {
                    syn::UnOp::Deref(_) => "*",
                    syn::UnOp::Not(_) => "!",
                    syn::UnOp::Neg(_) => "-",
                    _ => "",
                };
                let v = self.expr(&u.expr);
                // `!matches!(x, P)` is `!(x is P)`: `!x is P` would negate `x`
                if matches!(&*u.expr, syn::Expr::Macro(m) if m.mac.path.is_ident("matches")) && v.contains(" is ") {
                    return format!("{}({})", op, v);
                }
                format!("{}{}", op, v)
            }
            syn::Expr::Unsafe(u) => {
                if let Some(e) = self.single_expr(&u.block) {
                    if !is_blocky(e) {
                        let v = self.ex(e, &Ctx { throws_tail: false, ..ctx.clone() });
                        if !v.contains('\n') {
                            return format!("unsafe {{ {} }}", v);
                        }
                    }
                }
                format!("unsafe {}", self.block(&u.block, &Ctx { throws_tail: false, ..ctx.clone() }))
            }
            syn::Expr::While(w) => {
                let label = w.label.as_ref().map(|l| format!("{}: ", l.name)).unwrap_or_default();
                let cond = self.cond(&w.cond);
                format!("{}while ({}) {}", label, cond, self.block(&w.body, &Ctx::stmt()))
            }
            other => self.raw_expr(other),
        }
    }

    /// The left operand of a binary operator or `as`: `if`/`when` and friends need parentheses.
    fn operand(&mut self, e: &syn::Expr) -> String {
        let s = self.expr(e);
        // `matches!(..)` becomes `x is P`, which binds looser than `as` and arithmetic
        let is_test = matches!(e, syn::Expr::Macro(m) if m.mac.path.is_ident("matches")) && s.contains(" is ");
        if is_blocky(e) || matches!(e, syn::Expr::Closure(_)) || is_test {
            format!("({})", s)
        } else {
            s
        }
    }

    /// `rust { ... }` for expressions Oxie has no syntax for.
    pub fn raw_expr(&mut self, e: &syn::Expr) -> String {
        self.todo(line_of(e), "kept as Rust: no Oxie syntax for this expression");
        format!("rust {{ {} }}", self.rust_expr_inline(e))
    }

    /// An expression with attributes (a block's value, an argument, a field of a struct literal),
    /// kept as Rust with them: Oxie has attributes on statements, not on expressions.
    fn attributed_expr(&mut self, e: &syn::Expr) -> String {
        // an argument or element: `@cfg(unix) fd`, `@inline(always) { acc, x -> acc + x }`
        if let Some((attrs, bare)) = split_stmt_attrs(e) {
            let (docs, prefix) = self.stmt_attrs(&attrs);
            if docs.is_empty() && !has_expr_attrs(&bare) {
                let value = match &bare {
                    syn::Expr::Closure(c) => self.closure(c, true),
                    _ => self.expr(&bare),
                };
                return format!("{}{}", prefix, value);
            }
        }
        self.todo(line_of(e), "kept as Rust: Oxie has no attributes on expressions");
        format!("rust {{ {} }}", self.rust_expr_inline(e))
    }

    /// An expression formatted as Rust.
    pub fn rust_expr_inline(&self, e: &syn::Expr) -> String {
        let f: Result<syn::File, _> = syn::parse2(quote::quote!(fn __f() { #e; }));
        match f {
            Ok(f) => {
                let Some(s) = crate::printer::unparse(&f) else { return tok(e) };
                let lines: Vec<&str> = s.lines().collect();
                if lines.len() >= 3 {
                    let body: Vec<String> = lines[1..lines.len() - 1].iter().map(|l| l.strip_prefix("    ").unwrap_or(l).to_string()).collect();
                    let mut b = body.join("\n");
                    if b.ends_with(';') {
                        b.pop();
                    }
                    return b;
                }
                tok(e)
            }
            Err(_) => tok(e),
        }
    }

    fn enter_closure(&mut self) -> (bool, Option<syn::Type>) {
        let saved = (self.cx.throws, self.cx.ret.take());
        self.cx.throws = false;
        saved
    }

    fn leave_closure(&mut self, saved: (bool, Option<syn::Type>)) {
        self.cx.throws = saved.0;
        self.cx.ret = saved.1;
    }

    fn implicit_field(&self, name: &str) -> bool {
        match &self.cx.class {
            Some(c) => c.fields.contains(name) && !self.cx.bound.contains(name) && self.cx.it_rename.is_none(),
            None => false,
        }
    }

    /// An expression used as the receiver of `.x`, `?`, `[i]`.
    fn postfix(&mut self, e: &syn::Expr) -> String {
        let s = self.expr(e);
        // `matches!(x, P).then(..)` → `(x is P).then(..)`
        if matches!(e, syn::Expr::Macro(m) if m.mac.path.is_ident("matches")) && s.contains(" is ") {
            return format!("({})", s);
        }
        if is_postfix_safe(e) && !matches!(e, syn::Expr::Try(_)) {
            s
        } else if matches!(e, syn::Expr::Try(_)) {
            // `x?.y` is safe navigation in Oxie; `(x?).y` is Rust's `?` then `.y`
            format!("({})", s)
        } else if matches!(e, syn::Expr::Unary(_) | syn::Expr::Reference(_)) {
            format!("({})", s)
        } else if s.starts_with('(') && s.ends_with(')') {
            s
        } else {
            match e {
                syn::Expr::Struct(_)
                | syn::Expr::Block(_)
                | syn::Expr::If(_)
                | syn::Expr::Match(_)
                | syn::Expr::Binary(_)
                | syn::Expr::Cast(_)
                | syn::Expr::Range(_)
                | syn::Expr::Closure(_) => format!("({})", s),
                _ => s,
            }
        }
    }

    // ------------------------------------------------------------------ throws

    /// `Ok(v)` → `v`, `Err(e)` → `throw e`, other values → `value?`.
    fn throws_tail_expr(&mut self, e: &syn::Expr, ctx: &Ctx) -> Option<String> {
        let inner = Ctx { throws_tail: false, ..ctx.clone() };
        if let Some(args) = call_of(e, "Ok").or_else(|| ok_turbofish(e)) {
            if args.len() == 1 {
                if matches!(&args[0], syn::Expr::Tuple(t) if t.elems.is_empty()) {
                    return Some("()".into());
                }
                return Some(self.ex(&args[0], &Ctx { stmt: false, ..inner }));
            }
        }
        if let Some(args) = call_of(e, "Err") {
            if args.len() == 1 {
                return Some(format!("throw {}", self.thrown(&args[0])));
            }
        }
        match e {
            syn::Expr::If(_) | syn::Expr::Match(_) => None,
            syn::Expr::Block(b) if b.label.is_none() => None,
            syn::Expr::Paren(p) => self.throws_tail_expr(&p.expr, ctx),
            syn::Expr::Return(_) | syn::Expr::Break(_) | syn::Expr::Continue(_) => Some(self.ex(e, &inner)),
            syn::Expr::Macro(m) if ["panic", "unreachable", "todo", "unimplemented"].iter().any(|n| m.mac.path.is_ident(n)) => Some(self.ex(e, &inner)),
            _ => Some(self.propagated(e)),
        }
    }

    /// `e?` for a `Result` value returned from a throwing function. `collect()`, `sum()` and
    /// `product()` get the declared `Result` type, which `?` would hide from inference.
    fn propagated(&mut self, e: &syn::Expr) -> String {
        if let (syn::Expr::MethodCall(m), Some(rt)) = (e, self.cx.result_ty.clone()) {
            if m.turbofish.is_none() && ["collect", "sum", "product"].contains(&m.method.to_string().as_str()) {
                let mut m2 = m.clone();
                m2.turbofish = Some(syn::parse_quote!(::<#rt>));
                return format!("{}?", self.ex(&syn::Expr::MethodCall(m2), &Ctx::value()));
            }
        }
        // a `loop` nothing breaks out of never ends: it returns from inside, and has no value to
        // unwrap
        if let syn::Expr::Loop(l) = e {
            if !breaks_out(l) {
                return self.ex(e, &Ctx::value());
            }
        }
        // `x.ok_or_else(|| e)?` is `x ?: throw e`, which also leaves `e.into()` its target
        if let syn::Expr::MethodCall(m) = e {
            let err = match (m.method.to_string().as_str(), m.args.first()) {
                ("ok_or", Some(err)) if m.args.len() == 1 => Some(err.clone()),
                ("ok_or_else", Some(syn::Expr::Closure(c))) if m.args.len() == 1 && c.inputs.is_empty() => Some((*c.body).clone()),
                _ => None,
            };
            if let Some(err) = err {
                return format!("{} ?: throw {}", self.postfix(&m.receiver), self.thrown(&err));
            }
        }
        let s = self.ex(e, &Ctx::value());
        if is_postfix_safe(e) && !matches!(e, syn::Expr::Try(_)) {
            format!("{}?", s)
        } else {
            format!("({})?", s)
        }
    }

    /// The error value of `Err(e)` for `throw`: drops `.into()`, `Box::new(..)`, `From::from(..)`.
    fn thrown(&mut self, e: &syn::Expr) -> String {
        if self.cx.err_from_str {
            if let Some(l) = string_literal_conversion(e) {
                return self.str_literal(l);
            }
        }
        match e {
            syn::Expr::MethodCall(m) if m.method == "into" && m.args.is_empty() => self.thrown(&m.receiver),
            syn::Expr::Call(c) if c.args.len() == 1 => {
                if let syn::Expr::Path(p) = &*c.func {
                    let t = tok(&p.path);
                    if t == "Box::new" || t == "Box::from" || t == "From::from" || t.ends_with("::convert::From::from") {
                        return self.thrown(&c.args[0]);
                    }
                }
                self.expr(e)
            }
            _ => self.expr(e),
        }
    }

    /// An `if`/`else` whose branches are all `Ok(..)` or `Err(..)`, as one that returns or throws
    /// in each branch.
    fn returned_branches(&mut self, e: &syn::Expr) -> Option<String> {
        match e {
            syn::Expr::Paren(p) => self.returned_branches(&p.expr),
            syn::Expr::If(i) => {
                let (_, els) = i.else_branch.as_ref()?;
                let then = self.returned_branches(self.single_expr(&i.then_branch)?)?;
                let els = match &**els {
                    syn::Expr::Block(b) if b.label.is_none() => self.returned_branches(self.single_expr(&b.block)?)?,
                    other => self.returned_branches(other)?,
                };
                Some(format!("if ({}) {} else {}", self.cond(&i.cond), then, els))
            }
            _ => {
                if let Some(args) = call_of(e, "Err").filter(|a| a.len() == 1) {
                    return Some(format!("throw {}", self.thrown(&args[0])));
                }
                let args = call_of(e, "Ok").filter(|a| a.len() == 1)?;
                let v = &args[0];
                if matches!(v, syn::Expr::Tuple(t) if t.elems.is_empty()) {
                    return Some("return".into());
                }
                let ret = self.cx.ret.clone();
                Some(format!("return {}", self.ex(v, &Ctx::expect(ret.as_ref()))))
            }
        }
    }

    fn return_expr(&mut self, r: &syn::ExprReturn) -> String {
        let Some(v) = &r.expr else { return "return".into() };
        if self.cx.throws {
            if let Some(args) = call_of(v, "Ok").or_else(|| ok_turbofish(v)) {
                if args.len() == 1 {
                    if matches!(&args[0], syn::Expr::Tuple(t) if t.elems.is_empty()) {
                        return "return".into();
                    }
                    let ret = self.cx.ret.clone();
                    return format!("return {}", self.ex(&args[0], &Ctx::expect(ret.as_ref())));
                }
            }
            if let Some(args) = call_of(v, "Err") {
                if args.len() == 1 {
                    return format!("throw {}", self.thrown(&args[0]));
                }
            }
            // `return if c { Err(e.into()) } else { Ok(x) }`: each branch returns or throws, which
            // also gives `e.into()` its target (`?` on the whole wouldn't)
            if let Some(s) = self.returned_branches(v) {
                return s;
            }
            return format!("return {}", self.propagated(v));
        }
        let ret = self.cx.ret.clone();
        format!("return {}", self.ex(v, &Ctx::expect(ret.as_ref())))
    }

    // ------------------------------------------------------------------ calls

    fn call(&mut self, c: &syn::ExprCall) -> String {
        // `(self.f)(x)`: Oxie turns a parenthesized field call into a method call
        if let syn::Expr::Paren(p) = &*c.func {
            // a field of this class: Oxie calls a callable field by its name, `f(x)`
            if let syn::Expr::Field(f) = &*p.expr {
                if let (true, syn::Member::Named(m)) = (is_path(&f.base, "self"), &f.member) {
                    let name = m.to_string();
                    let method = self.cx.class.as_ref().is_some_and(|c| c.methods.contains(&name));
                    if self.implicit_field(&name) && !method && !self.cx.type_fns.contains(&name) {
                        let args: Vec<String> = c.args.iter().map(|a| self.expr(a)).collect();
                        return format!("{}({})", id(m), args.join(", "));
                    }
                }
            }

        }
        let mut expects: Vec<Option<syn::Type>> = Vec::new();
        let mut func = None;
        if let syn::Expr::Path(p) = &*c.func {
            let segs: Vec<String> = p.path.segments.iter().map(|s| s.ident.to_string()).collect();
            let no_args = p.path.segments.iter().all(|s| s.arguments.is_empty());
            // `HashMap::from([(k, v)])` → `[k: v]`, `HashSet::from([a])` → `setOf(a)`
            let std_path = segs.len() == 2 || (segs.len() == 4 && segs[0] == "std" && segs[1] == "collections");
            if p.qself.is_none() && no_args && std_path && segs.last().is_some_and(|l| l == "from") && c.args.len() == 1 && !self.info.structs.contains_key(&segs[segs.len() - 2]) {
                if let syn::Expr::Array(arr) = &c.args[0] {
                    match segs[segs.len() - 2].as_str() {
                        "HashMap" if arr.elems.iter().all(|e| matches!(e, syn::Expr::Tuple(t) if t.elems.len() == 2)) => {
                            let pairs: Vec<String> = arr
                                .elems
                                .iter()
                                .map(|e| {
                                    let syn::Expr::Tuple(t) = e else { unreachable!() };
                                    format!("{}: {}", self.expr(&t.elems[0]), self.expr(&t.elems[1]))
                                })
                                .collect();
                            return format!("[{}]", pairs.join(", "));
                        }
                        "HashSet" => {
                            let v: Vec<String> = arr.elems.iter().map(|e| self.expr(e)).collect();
                            return format!("setOf({})", v.join(", "));
                        }
                        _ => {}
                    }
                }
            }
            // Oxie runtime helpers in `oxie emit` output
            if segs.len() >= 2 && segs[segs.len() - 2] == "ElvisOr" && segs[segs.len() - 1] == "or_val" && c.args.len() == 2 {
                return format!("({} ?: {})", self.expr(&c.args[0]), self.expr(&c.args[1]));
            }
            if p.qself.is_none() && no_args && segs.len() == 2 && segs[1] == "new" {
                let outside = self.info.outside_imports.contains(&(self.mod_prefix.clone(), segs[0].clone()));
                if let Some(si) = self.info.structs.get(&segs[0]).filter(|_| !outside) {
                    if si.has_new {
                        expects = si.new_params.iter().cloned().map(Some).collect();
                        func = Some(id(&p.path.segments[0].ident));
                    }
                } else if c.args.is_empty() && segs[0] == "HashMap" && self.info.std_collections.contains("HashMap") {
                    return "[:]".into();
                } else if c.args.is_empty() && ["HashMap", "HashSet", "BTreeMap", "BTreeSet", "String", "VecDeque", "Vec"].contains(&segs[0].as_str()) {
                    return format!("{}()", segs[0]);
                }
            }
            if p.qself.is_none() && segs.len() == 1 {
                if let Some(ps) = self.info.fns.get(&segs[0]) {
                    expects = ps.iter().cloned().map(Some).collect();
                }
                // in a method, Oxie would call the type's own function of this name (Rust's bare
                // `name()` never does; it may call a function a macro declares, or the prelude's
                // `drop`)
                let name = &segs[0];
                let prelude = matches!(name.as_str(), "drop" | "Some" | "Ok" | "Err") && !self.info.free_fns.contains(name);
                if p.path.leading_colon.is_none() && self.cx.type_fns.contains(name) && !self.cx.bound.contains(name) && !prelude {
                    func = Some(format!("self::{}", self.postfix(&c.func)));
                }
            }
        }
        let f = match func {
            Some(f) => f,
            None => self.postfix(&c.func),
        };
        let mut args: Vec<&syn::Expr> = c.args.iter().collect();
        // `Self::open(path, window, cx)` → `Self.open(path)`
        if let syn::Expr::Path(p) = &*c.func {
            let segs = &p.path.segments;
            if p.qself.is_none() && p.path.leading_colon.is_none() && segs.len() == 2 {
                let owner = segs[0].ident.to_string();
                let class = if owner == "Self" { self.cx.ctx_class.clone() } else { Some(owner) };
                let name = segs[1].ident.to_string();
                let static_fn = class.as_deref().and_then(|c| self.krate.class(c)).and_then(|c| c.methods.get(&name)).is_some_and(|m| m.sig.receiver().is_none());
                if let (Some(class), true) = (class, static_fn) {
                    let n = self.ctx_args(&class, &name, &args);
                    args.truncate(args.len() - n);
                }
            }
        }
        format!("{}{}", f, self.args(&args, &expects))
    }

    /// How many of the last arguments of a call to method `name` of `class` Oxie fills in as
    /// contexts: those that name the variable the context is named after (`window, cx`).
    fn ctx_args(&self, class: &str, name: &str, args: &[&syn::Expr]) -> usize {
        let Some(takes) = self.krate.takes(class, name) else { return 0 };
        if args.len() < takes.len() || self.fixes.ctx_calls.contains(&(self.cx.fn_path.clone(), name.to_string())) {
            return 0;
        }
        let tail = &args[args.len() - takes.len()..];
        // an argument kept stands for the first contexts, so only the last ones can go
        tail.iter().zip(takes).rev().take_while(|(a, t)| is_path(a, t) && self.cx.it_rename.as_deref() != Some(t.as_str())).count()
    }

    /// `(a, b)`, `(a) { x -> ... }`, ` { x -> ... }`.
    fn args(&mut self, args: &[&syn::Expr], expects: &[Option<syn::Type>]) -> String {
        let n = args.len();
        let trailing = match args.last() {
            Some(syn::Expr::Closure(c)) => self.kotlin_lambda_ok(c) && c.capture.is_none(),
            _ => false,
        };
        let mut v = Vec::new();
        for (i, a) in args.iter().enumerate() {
            if trailing && i + 1 == n {
                break;
            }
            let s = match a {
                syn::Expr::Closure(c) => {
                    if c.capture.is_some() && self.kotlin_lambda_ok(c) {
                        self.closure(c, true)
                    } else {
                        self.closure(c, false)
                    }
                }
                _ => self.ex(a, &Ctx::expect(expects.get(i).and_then(|t| t.as_ref()))),
            };
            v.push(s);
        }
        let mut s = String::new();
        if !(trailing && v.is_empty()) {
            s.push_str(&format!("({})", v.join(", ")));
        }
        if trailing {
            if let Some(syn::Expr::Closure(c)) = args.last() {
                s.push(' ');
                s.push_str(&self.closure(c, true));
            }
        }
        s
    }

    fn method_call(&mut self, m: &syn::ExprMethodCall) -> String {
        let (root, segs) = self.chain(m);
        let one = format!("{}{}", root, segs.concat());
        let first_line = one.lines().next().unwrap_or("");
        if segs.len() >= 2 && first_line.len() > 90 {
            let mut s = root;
            for seg in &segs {
                s.push('\n');
                s.push_str(&indent(seg));
            }
            return s;
        }
        one
    }

    /// A method-call chain as its root and `.method(args)` segments.
    fn chain(&mut self, m: &syn::ExprMethodCall) -> (String, Vec<String>) {
        let (root, mut segs, seg) = self.method_call_parts(m);
        match seg {
            Some(seg) => {
                segs.push(seg);
                (root, segs)
            }
            None => (root, segs),
        }
    }

    fn method_call_parts(&mut self, m: &syn::ExprMethodCall) -> (String, Vec<String>, Option<String>) {
        let name = m.method.to_string();
        let mut prefix = String::new();
        let implicit = is_path(&m.receiver, "self")
            && self.cx.class.as_ref().is_some_and(|c| c.methods.contains(&name))
            && !self.cx.bound.contains(&name)
            && !self.info.fns.contains_key(&name)
            && !BUILTIN_FNS.contains(&name.as_str())
            && self.cx.it_rename.is_none();
        let mut root = None;
        let mut segs = Vec::new();
        if !implicit {
            match &*m.receiver {
                syn::Expr::MethodCall(inner) => {
                    let (r, v) = self.chain(inner);
                    root = Some(r);
                    segs = v;
                }
                // `Quit.name()` on a unit struct: Oxie reads `Type.f()` as a static call
                syn::Expr::Path(p) if p.qself.is_none() && p.path.get_ident().is_some_and(|i| {
                    // a CamelCase name: Oxie reads an all-caps one (`COUNT.with(..)`) as a value already
                    let n = i.to_string();
                    n.starts_with(|c: char| c.is_ascii_uppercase()) && n.contains(|c: char| c.is_ascii_lowercase()) && n != "Self"
                }) => {
                    root = Some(format!("({})", self.postfix(&m.receiver)));
                }
                other => root = Some(self.postfix(other)),
            }
            prefix = ".".into();
        }
        // `collect::<Vec<_>>()` is `to_list()`
        if name == "collect" && m.args.is_empty() && m.turbofish.as_ref().is_some_and(|t| t.args.len() == 1 && tok(&t.args[0]).replace(' ', "") == "Vec<_>") {
            let call = format!("{}to_list()", prefix);
            return match root {
                Some(r) => (r, segs, Some(call)),
                None => (call, segs, None),
            };
        }
        let turbofish = m.turbofish.as_ref().map(|t| self.generic_args(t)).unwrap_or_default();
        let mut args: Vec<&syn::Expr> = m.args.iter().collect();
        if let (true, Some(class)) = (is_path(&m.receiver, "self"), self.cx.ctx_class.clone()) {
            let n = self.ctx_args(&class, &name, &args);
            args.truncate(args.len() - n);
        }
        // zero-argument closures for these may get an argument added by Oxie
        let rust_closures = matches!(name.as_str(), "unwrap_or_else" | "or_else" | "map_or_else" | "or_insert_with" | "get_or_insert_with");
        let a = if rust_closures && args.iter().any(|a| matches!(a, syn::Expr::Closure(c) if c.inputs.is_empty())) {
            let v: Vec<String> = args
                .iter()
                .map(|a| match a {
                    syn::Expr::Closure(c) => self.closure(c, false),
                    other => self.expr(other),
                })
                .collect();
            format!("({})", v.join(", "))
        } else {
            self.args(&args, &[])
        };
        let call = format!("{}{}{}{}", prefix, id(&m.method), turbofish, a);
        match root {
            Some(r) => (r, segs, Some(call)),
            None => (call, segs, None),
        }
    }

    fn kotlin_lambda_ok(&self, c: &syn::ExprClosure) -> bool {
        if !matches!(c.output, syn::ReturnType::Default) || c.asyncness.is_some() || c.lifetimes.is_some() || c.constness.is_some() || c.movability.is_some() {
            return false;
        }
        if !c.attrs.is_empty() {
            return false;
        }
        if c.inputs.is_empty() && mentions(&c.body, "it") {
            return false;
        }
        c.inputs.iter().all(simple_lambda_param)
    }

    /// A closure, as a Kotlin-style lambda when `kotlin` is set (argument and `val` positions).
    pub fn closure(&mut self, c: &syn::ExprClosure, kotlin: bool) -> String {
        if !c.attrs.is_empty() {
            return self.attributed_expr(&syn::Expr::Closure(c.clone()));
        }
        // `{ f(it) }` would have a parameter `it`, hiding the variable it captures: such a closure
        // keeps Rust's `|| f(it)` (`kotlin_lambda_ok`)
        let kotlin = kotlin && self.kotlin_lambda_ok(c);
        let (todos, pending) = (self.todos.len(), self.pending.len());
        let saved = self.enter_closure();
        let saved_it = self.cx.it_rename.clone();
        let mut it_name = None;
        if kotlin && c.inputs.len() == 1 && !self.cx.no_it {
            if let syn::Pat::Ident(pi) = &c.inputs[0] {
                if pi.mutability.is_none()
                    && pi.by_ref.is_none()
                    && pi.subpat.is_none()
                    && pi.ident != "it"
                    && !matches!(&*c.body, syn::Expr::Block(_))
                    && it_safe(&c.body)
                    && mentions(&c.body, &pi.ident.to_string())
                    && !mentions(&c.body, "it")
                {
                    it_name = Some(pi.ident.to_string());
                }
            }
        }
        if it_name.is_some() {
            self.cx.it_rename = it_name.clone();
        }
        let body_text;
        let multiline;
        match &*c.body {
            syn::Expr::Block(b) if b.label.is_none() => {
                if let Some(e) = self.single_expr(&b.block) {
                    if kotlin {
                        body_text = self.expr(e);
                        multiline = body_text.contains('\n');
                    } else {
                        body_text = self.block(&b.block, &Ctx::value());
                        multiline = true;
                    }
                } else if kotlin {
                    body_text = self.stmts(&b.block, &Ctx::value(), true);
                    multiline = true;
                } else {
                    body_text = self.block(&b.block, &Ctx::value());
                    multiline = true;
                }
            }
            e => {
                body_text = self.ex(e, &Ctx::value());
                multiline = body_text.contains('\n');
            }
        }
        self.cx.it_rename = saved_it;
        self.leave_closure(saved);
        // a body kept as Rust still names the parameter, so it keeps its name
        if it_name.is_some() && body_text.contains("rust {") {
            self.todos.truncate(todos);
            self.pending.truncate(pending);
            self.cx.no_it = true;
            let s = self.closure(c, true);
            self.cx.no_it = false;
            return s;
        }
        let mv = if c.capture.is_some() { "move " } else { "" };
        if kotlin {
            let params: Vec<String> = c.inputs.iter().map(|p| self.pat(p)).collect();
            let head = if it_name.is_some() || params.is_empty() { String::new() } else { format!("{} ->", params.join(", ")) };
            if multiline {
                if head.is_empty() {
                    format!("{}{{\n{}\n}}", mv, indent(&body_text))
                } else {
                    format!("{}{{ {}\n{}\n}}", mv, head, indent(&body_text))
                }
            } else if head.is_empty() {
                if body_text.is_empty() {
                    format!("{}{{}}", mv)
                } else {
                    format!("{}{{ {} }}", mv, body_text)
                }
            } else {
                format!("{}{{ {} {} }}", mv, head, body_text)
            }
        } else {
            let params: Vec<String> = c.inputs.iter().map(|p| self.pat(p)).collect();
            let mut s = String::new();
            if c.asyncness.is_some() {
                s.push_str("async ");
            }
            s.push_str(mv);
            s.push_str(&format!("|{}|", params.join(", ")));
            if let syn::ReturnType::Type(_, t) = &c.output {
                s.push_str(&format!(" -> {}", self.ty(t)));
                if !body_text.starts_with('{') {
                    return format!("{} {{ {} }}", s, body_text);
                }
            }
            format!("{} {}", s, body_text)
        }
    }

    fn path_expr(&mut self, p: &syn::ExprPath) -> String {
        if p.qself.is_some() {
            return self.type_path(p.qself.as_ref(), &p.path);
        }
        let segs = &p.path.segments;
        if segs.len() == 1 && segs[0].arguments.is_empty() && p.path.leading_colon.is_none() {
            let n = segs[0].ident.to_string();
            if n == "None" {
                return "null".into();
            }
            if self.cx.it_rename.as_deref() == Some(n.as_str()) {
                return "it".into();
            }
            return id(&segs[0].ident);
        }
        self.value_path(&p.path)
    }

    /// `Type::member` → `Type.member`, `std::mem::swap` → `std.mem.swap`.
    pub fn value_path(&self, path: &syn::Path) -> String {
        let segs: Vec<&syn::PathSegment> = path.segments.iter().collect();
        // `Vec::<i32>::new` must be written `Vec::<i32>.new` (Oxie misreads the Rust form)
        let n = segs.len();
        let std_root = segs.first().is_some_and(|f| f.ident == "std" || f.ident == "core" || f.ident == "alloc");
        if n >= 2 && segs[n - 1].arguments.is_empty() && !segs[n - 2].arguments.is_empty() && segs[..n - 2].iter().all(|s| s.arguments.is_empty()) {
            let prefix = self.type_path(None, &syn::Path { leading_colon: path.leading_colon, segments: segs[..n - 1].iter().map(|s| (*s).clone()).collect() });
            return format!("{}.{}", prefix, id(&segs[n - 1].ident));
        }
        if (path.leading_colon.is_none() || std_root) && segs.len() >= 2 && segs[..segs.len() - 1].iter().all(|s| s.arguments.is_empty()) {
            let first = segs[0].ident.to_string();
            if crate::oxie_aux::strip_runtime_path(path).is_some() {
                return crate::oxie_aux::strip_runtime_path(path).unwrap();
            }
            let dot = first.chars().next().is_some_and(|c| c.is_uppercase())
                || ["std", "core", "alloc"].contains(&first.as_str())
                || (self.info.modules.contains(&first) && !self.cx.bound.contains(&first) && !self.cx.type_fields.contains(&first));
            // `NSURL::fileURLWithPath`: Oxie reads an all-caps name before `.` as a constant
            let const_like = first.len() > 1 && first.chars().all(|c| c.is_uppercase() || c == '_' || c.is_ascii_digit());
            if dot && !const_like && !(first == "Self" && segs.len() > 2) {
                let v: Vec<String> = segs.iter().map(|s| self.path_segment(s)).collect();
                return v.join(".");
            }
        }
        self.type_path(None, path)
    }

    fn struct_lit(&mut self, s: &syn::ExprStruct) -> String {
        let segs: Vec<String> = s.path.segments.iter().map(|x| x.ident.to_string()).collect();
        // named-field enum variant with all fields in order → `Enum.Variant(a, b)`
        if s.qself.is_none() && s.rest.is_none() && segs.len() == 2 {
            if let Some(ei) = self.info.enums.get(&segs[0]) {
                if let Some(names) = ei.named.get(&segs[1]) {
                    let written: Vec<String> = s
                        .fields
                        .iter()
                        .map(|f| match &f.member {
                            syn::Member::Named(i) => i.to_string(),
                            syn::Member::Unnamed(i) => i.index.to_string(),
                        })
                        .collect();
                    if &written == names && !names.is_empty() {
                        let v: Vec<String> = s.fields.iter().map(|f| self.expr(&f.expr)).collect();
                        return format!("{}.{}({})", id(&s.path.segments[0].ident), id(&s.path.segments[1].ident), v.join(", "));
                    }
                }
            }
        }
        let field_types: Vec<(String, syn::Type)> =
            if segs.len() == 1 { self.info.structs.get(&segs[0]).map(|si| si.fields.clone()).unwrap_or_default() } else { Vec::new() };
        let mut v = Vec::new();
        for f in &s.fields {
            let name = match &f.member {
                syn::Member::Named(i) => id(i),
                syn::Member::Unnamed(i) => i.index.to_string(),
            };
            let expect = field_types.iter().find(|(n, _)| *n == name).map(|(_, t)| t.clone());
            // `#[cfg(unix)] fd: 3` → `@cfg(unix) fd: 3`
            let (_, attrs) = self.stmt_attrs(&f.attrs);
            let shorthand = f.colon_token.is_none();
            if shorthand {
                let val = self.ex(&f.expr, &Ctx::value());
                if val == name {
                    v.push(format!("{}{}", attrs, name));
                } else {
                    v.push(format!("{}{}: {}", attrs, name, val));
                }
            } else {
                let val = self.ex(&f.expr, &Ctx::expect(expect.as_ref()));
                v.push(format!("{}{}: {}", attrs, name, val));
            }
        }
        if let Some(r) = &s.rest {
            v.push(format!("..{}", self.expr(r)));
        } else if s.dot2_token.is_some() {
            v.push("..".into());
        }
        let path = self.type_path(s.qself.as_ref(), &s.path);
        if v.is_empty() {
            format!("{} {{}}", path)
        } else {
            format!("{} {{ {} }}", path, v.join(", "))
        }
    }

    fn lit(&self, l: &syn::Lit) -> String {
        match l {
            syn::Lit::Str(s) => self.str_literal(s),
            syn::Lit::ByteStr(b) => {
                let t = tok(b);
                if t.contains('\n') || t.contains('$') {
                    let mut s = String::from("b\"");
                    for byte in b.value() {
                        match byte {
                            b'\\' => s.push_str("\\\\"),
                            b'"' => s.push_str("\\\""),
                            b'$' => s.push_str("\\x24"),
                            0x20..=0x7e => s.push(byte as char),
                            _ => s.push_str(&format!("\\x{:02x}", byte)),
                        }
                    }
                    s.push('"');
                    s
                } else {
                    t
                }
            }
            other => tok(other),
        }
    }

    pub fn str_literal(&self, s: &syn::LitStr) -> String {
        let t = tok(s);
        if t.starts_with('r') && !t.contains('\n') {
            return t;
        }
        let v = s.value();
        // `"say \"hi\""` reads better as a raw string
        if v.contains('"') && !v.contains("\"#") && !v.chars().any(|c| c.is_control()) {
            return format!("r#\"{}\"#", v);
        }
        fmtstr::quote(&v)
    }

    // ------------------------------------------------------------------ control flow

    fn cond(&mut self, e: &syn::Expr) -> String {
        match e {
            syn::Expr::Let(l) => self.let_cond(&l.pat, &l.expr),
            syn::Expr::Binary(b) if matches!(b.op, syn::BinOp::And(_)) && (has_let(&b.left) || has_let(&b.right)) => {
                format!("{} && {}", self.cond(&b.left), self.cond(&b.right))
            }
            other => self.expr(other),
        }
    }

    fn let_cond(&mut self, pat: &syn::Pat, e: &syn::Expr) -> String {
        let v = self.expr(e);
        if let syn::Pat::TupleStruct(ts) = pat {
            if ts.qself.is_none() && ts.path.is_ident("Some") && ts.elems.len() == 1 {
                if let syn::Pat::Ident(pi) = &ts.elems[0] {
                    if pi.by_ref.is_none() && pi.mutability.is_none() && pi.subpat.is_none() {
                        return format!("val {} = {}", id(&pi.ident), v);
                    }
                }
            }
        }
        if !pat_binds(pat) {
            let subject = if is_postfix_safe(e) { v } else { format!("({})", v) };
            return format!("{} is {}", subject, self.pat(pat));
        }
        if let syn::Pat::Ident(_) = pat {
            // irrefutable binding: `val x = e` would mean `Some(x)` in Oxie
            return format!("val ({},) = ({},)", self.pat(pat), v);
        }
        format!("val {} = {}", self.pat(pat), v)
    }

    /// The context of the branches of `if` or `match` `e`: their `None`s stay `None` when
    /// another branch gives an `Option` whose type Oxie may not know (a call into a Rust
    /// module): `null` beside it would make Oxie wrap it in another `Option`.
    fn branch_ctx(&self, e: &syn::Expr, ctx: &Ctx) -> Ctx {
        fn tails<'e>(e: &'e syn::Expr, out: &mut Vec<&'e syn::Expr>) {
            let block = |b: &'e syn::Block, out: &mut Vec<&'e syn::Expr>| {
                if let Some(syn::Stmt::Expr(x, None)) = b.stmts.last() {
                    tails(x, out);
                }
            };
            match e {
                syn::Expr::If(i) => {
                    block(&i.then_branch, out);
                    if let Some((_, el)) = &i.else_branch {
                        tails(el, out);
                    }
                }
                syn::Expr::Match(m) => m.arms.iter().for_each(|a| tails(&a.body, out)),
                syn::Expr::Block(b) if b.label.is_none() => block(&b.block, out),
                syn::Expr::Paren(p) => tails(&p.expr, out),
                _ => out.push(e),
            }
        }
        let mut ts = Vec::new();
        tails(e, &mut ts);
        let is_none = |t: &syn::Expr| matches!(t, syn::Expr::Path(p) if p.qself.is_none() && p.path.is_ident("None"));
        // values Oxie can tell are optional or not, and branches that leave
        let known = |t: &syn::Expr| match t {
            syn::Expr::Call(c) => match &*c.func {
                syn::Expr::Path(p) => p.path.is_ident("Some") || p.path.get_ident().is_some_and(|f| self.info.option_fns.contains(&f.to_string())),
                _ => false,
            },
            syn::Expr::Return(_) | syn::Expr::Break(_) | syn::Expr::Continue(_) | syn::Expr::Lit(_) => true,
            syn::Expr::Macro(m) => ["panic", "unreachable", "todo", "unimplemented"].iter().any(|n| m.mac.path.is_ident(n)),
            _ => false,
        };
        let keep = !ctx.stmt && ts.iter().any(|t| is_none(t)) && ts.iter().any(|t| !is_none(t) && !known(t));
        Ctx { keep_none: ctx.keep_none || keep, ..ctx.clone() }
    }

    fn if_expr(&mut self, i: &syn::ExprIf, ctx: &Ctx, compact: bool) -> String {
        let cond = self.cond(&i.cond);
        // compact `if (c) a else b`
        if compact && !ctx.stmt && !ctx.throws_tail {
            if let (Some(a), Some((_, els))) = (self.single_expr(&i.then_branch), &i.else_branch) {
                if let syn::Expr::Block(eb) = &**els {
                    if let Some(b) = self.single_expr(&eb.block) {
                        if !is_blocky(a) && !is_blocky(b) {
                            let marks = (self.todos.len(), self.pending.len());
                            let at = self.ex(a, ctx);
                            let bt = self.ex(b, ctx);
                            if !at.contains('\n') && !bt.contains('\n') && cond.len() + at.len() + bt.len() < 80 {
                                return format!("if ({}) {} else {}", cond, at, bt);
                            }
                            self.todos.truncate(marks.0);
                            self.pending.truncate(marks.1);
                        }
                    }
                }
            }
        }
        // `else {}` and, at the end of a throwing function, `else { Ok(()) }` add nothing
        let empty_else = match &i.else_branch {
            Some((_, e)) => match &**e {
                syn::Expr::Block(b) if b.label.is_none() && !self.has_comments_in(line_of(&b.block), end_line_of(&b.block)) => {
                    b.block.stmts.is_empty()
                        || (ctx.throws_tail && ctx.stmt && b.block.stmts.len() == 1 && matches!(&b.block.stmts[0], syn::Stmt::Expr(x, None) if is_ok_unit(x)))
                }
                _ => false,
            },
            None => true,
        };
        if empty_else && i.else_branch.is_some() && (ctx.stmt || ctx.throws_tail) {
            let mut j = i.clone();
            j.else_branch = None;
            return self.if_expr(&j, ctx, compact);
        }
        // `if (c) return x` for a single short statement
        if compact
            && ctx.stmt
            && i.else_branch.is_none()
            && i.then_branch.stmts.len() == 1
            && !has_stmt_attrs(&i.then_branch.stmts[0])
            && !self.has_comments_in(line_of(&i.then_branch), end_line_of(&i.then_branch))
        {
            let simple = match &i.then_branch.stmts[0] {
                syn::Stmt::Expr(e, _) => {
                    matches!(
                        e,
                        syn::Expr::Return(_)
                            | syn::Expr::Break(_)
                            | syn::Expr::Continue(_)
                            | syn::Expr::Assign(_)
                            | syn::Expr::Try(_)
                            | syn::Expr::Call(_)
                            | syn::Expr::MethodCall(_)
                            | syn::Expr::Macro(_)
                    ) || (matches!(e, syn::Expr::Binary(_)) && !needs_semi(e))
                }
                syn::Stmt::Macro(_) => true,
                _ => false,
            };
            if simple {
                // a tail expression keeps the context (`Err(e)` → `throw e` in a throwing function)
                let marks = (self.todos.len(), self.pending.len());
                let t = match &i.then_branch.stmts[0] {
                    syn::Stmt::Expr(e, None) => self.ex(e, ctx),
                    st => self.stmt(st, false, &Ctx::stmt(), false),
                };
                // `if (c) <T>::f()` would read as a comparison
                if !t.contains('\n') && !t.starts_with("val ") && !t.starts_with('<') && cond.len() + t.len() < 90 {
                    return format!("if ({}) {}", cond, t);
                }
                self.todos.truncate(marks.0);
                self.pending.truncate(marks.1);
            }
        }
        let then = self.block(&i.then_branch, ctx);
        let mut s = format!("if ({}) {}", cond, then);
        if let Some((_, els)) = &i.else_branch {
            match &**els {
                syn::Expr::If(ei) => {
                    s.push_str(" else ");
                    s.push_str(&self.if_expr(ei, ctx, false));
                }
                syn::Expr::Block(b) if b.label.is_none() => {
                    s.push_str(" else ");
                    s.push_str(&self.block(&b.block, ctx));
                }
                other => {
                    s.push_str(" else ");
                    s.push_str(&self.ex(other, ctx));
                }
            }
        }
        s
    }

    fn match_expr(&mut self, m: &syn::ExprMatch, ctx: &Ctx) -> String {
        if let Some(s) = self.elvis_match(m) {
            return s;
        }
        let subject = self.expr(&m.expr);
        let lo = line_of(m);
        let mut arms = Vec::new();
        let n = m.arms.len();
        for (i, arm) in m.arms.iter().enumerate() {
            let cs = self.take_comments(lo, line_of(&arm.pat));
            arms.extend(cs);
            let mut head = for_arm_attrs(self, &arm.attrs);
            let pat = if matches!(&arm.pat, syn::Pat::Wild(_)) && arm.guard.is_none() && i + 1 == n { "else".to_string() } else { self.arm_pat(&arm.pat) };
            head.push_str(&pat);
            if let Some((_, g)) = &arm.guard {
                head.push_str(&format!(" if {}", self.cond(g)));
            }
            let body = match &*arm.body {
                // `X -> if (c) a` would take a following `else ->` arm as its `else`
                syn::Expr::If(i) if !ends_with_else(i) => format!("{{\n{}\n}}", indent(&self.ex(&arm.body, ctx))),
                syn::Expr::Block(b) if b.label.is_none() => {
                    if let Some(e) = self.single_expr(&b.block) {
                        if (is_blocky(e) && !matches!(e, syn::Expr::If(_) | syn::Expr::Match(_))) || matches!(e, syn::Expr::If(i) if !ends_with_else(i)) {
                            self.block(&b.block, ctx)
                        } else {
                            let t = self.ex(e, ctx);
                            if t == "()" {
                                "{}".into()
                            } else {
                                t
                            }
                        }
                    } else if ctx.stmt
                        && b.block.stmts.len() == 1
                        && !has_stmt_attrs(&b.block.stmts[0])
                        && !self.has_comments_in(line_of(&b.block), end_line_of(&b.block))
                        && (matches!(&b.block.stmts[0], syn::Stmt::Expr(e, Some(_)) if !is_blocky(e)) || matches!(&b.block.stmts[0], syn::Stmt::Macro(_)))
                    {
                        let marks = (self.todos.len(), self.pending.len());
                        let t = self.stmt(&b.block.stmts[0], false, &Ctx::stmt(), false);
                        // a declaration (`val _ = ..`) is no arm body without braces
                        if t.contains('\n') || t.starts_with("val ") || t.starts_with("var ") {
                            self.todos.truncate(marks.0);
                            self.pending.truncate(marks.1);
                            self.block(&b.block, ctx)
                        } else {
                            t
                        }
                    } else {
                        self.block(&b.block, ctx)
                    }
                }
                syn::Expr::Closure(c) => self.closure(c, false),
                e => {
                    let t = self.ex(e, ctx);
                    if t == "()" && ctx.throws_tail {
                        "{}".into()
                    } else if t.starts_with('{') {
                        format!("({})", t)
                    } else {
                        t
                    }
                }
            };
            let mut line = format!("{} -> {}", head, body);
            if let Some(tc) = self.trailing_comment(end_line_of(&arm.body)) {
                line.push(' ');
                line.push_str(&tc);
            }
            arms.push(line);
        }
        let tail = self.take_comments(lo, end_line_of(m));
        arms.extend(tail);
        format!("when ({}) {{\n{}\n}}", subject, indent(&arms.join("\n")))
    }

    /// `match crate::__oxie::Elvis::opt(a) { Some(v) => v, None => b }` (from `oxie emit`) → `a ?: b`
    fn elvis_match(&mut self, m: &syn::ExprMatch) -> Option<String> {
        let syn::Expr::Call(c) = &*m.expr else { return None };
        let syn::Expr::Path(p) = &*c.func else { return None };
        let segs: Vec<String> = p.path.segments.iter().map(|s| s.ident.to_string()).collect();
        if !(segs.len() >= 2 && segs[segs.len() - 2] == "Elvis" && segs[segs.len() - 1] == "opt" && segs.contains(&"__oxie".to_string()))
            || c.args.len() != 1
            || m.arms.len() != 2
        {
            return None;
        }
        let syn::Pat::TupleStruct(ts) = &m.arms[0].pat else { return None };
        if !ts.path.is_ident("Some") || ts.elems.len() != 1 || m.arms[0].guard.is_some() {
            return None;
        }
        let syn::Pat::Ident(v) = &ts.elems[0] else { return None };
        if !is_path(&m.arms[0].body, &v.ident.to_string()) || m.arms[1].guard.is_some() {
            return None;
        }
        if !matches!(&m.arms[1].pat, syn::Pat::Ident(n) if n.ident == "None") {
            return None;
        }
        let a = self.expr(&c.args[0]);
        let b = self.expr(&m.arms[1].body);
        Some(format!("({} ?: {})", a, b))
    }

    // ------------------------------------------------------------------ macros

    fn mac(&mut self, m: &syn::Macro, ctx: &Ctx) -> String {
        let name = if m.path.segments.len() == 1 { m.path.segments[0].ident.to_string() } else { String::new() };
        // a bare `write(..)` would call the function or method `write` in scope
        if self.name_taken(&name) {
            return self.rust_macro(m);
        }
        let r = match name.as_str() {
            "println" | "print" | "eprintln" | "eprint" | "format" | "panic" => self.fmt_macro(m, &name, 0),
            "write" | "writeln" => self.fmt_macro(m, &name, 1),
            "assert" | "debug_assert" => self.fmt_macro(m, &name, 1),
            "assert_eq" | "assert_ne" | "debug_assert_eq" | "debug_assert_ne" => self.fmt_macro(m, &name, 2),
            "vec" => self.vec_macro(m, ctx),
            "todo" | "unreachable" if m.tokens.is_empty() => Some(format!("{}()", name)),
            "matches" => self.matches_macro(m),
            _ => None,
        };
        match r {
            Some(s) => s,
            None => self.rust_macro(m),
        }
    }

    /// A function, method or variable named `name` is in scope, which a bare `name(..)` calls.
    fn name_taken(&self, name: &str) -> bool {
        self.info.free_fns.contains(name) || self.cx.bound.contains(name) || self.cx.class.as_ref().is_some_and(|c| c.methods.contains(name)) || self.cx.type_fns.contains(name)
    }

    /// A macro call passed through as Rust.
    fn rust_macro(&self, m: &syn::Macro) -> String {
        let e = syn::Expr::Macro(syn::ExprMacro { attrs: Vec::new(), mac: m.clone() });
        let s = self.rust_expr_inline(&e);
        if s.contains('\n') && matches!(m.delimiter, syn::MacroDelimiter::Brace(_)) {
            return s;
        }
        s
    }

    /// `println!("x = {}", x)` → `println("x = $x")`. `fixed` arguments precede the format string.
    fn fmt_macro(&mut self, m: &syn::Macro, name: &str, fixed: usize) -> Option<String> {
        let args = macro_args(m)?;
        if args.iter().any(|a| mentions(a, "__oxie_unused")) {
            return None;
        }
        let mut out: Vec<String> = Vec::new();
        for a in args.iter().take(fixed) {
            out.push(self.expr(a));
        }
        if args.len() < fixed {
            return None;
        }
        if args.len() > fixed {
            let s = str_lit(&args[fixed])?;
            let rest = &args[fixed + 1..];
            let marks = (self.todos.len(), self.pending.len());
            match self.format_template(&s.value(), rest) {
                Some(t) => {
                    if name == "format" {
                        if !t.contains('$') {
                            return Some(format!("{}.to_string()", t));
                        }
                        return Some(t);
                    }
                    // `println("$x")` → `println(x)`
                    if matches!(name, "println" | "print" | "eprintln" | "eprint") && fixed == 0 {
                        let single = match fmtstr::parse(&s.value()).as_deref() {
                            Some([Piece::Arg { arg: ArgRef::Next, spec }]) if spec.is_empty() && rest.len() == 1 => Some(self.expr(&rest[0])),
                            Some([Piece::Arg { arg: ArgRef::Name(n), spec }]) if spec.is_empty() && rest.is_empty() => {
                                Some(t.trim_matches('"').trim_start_matches('$').to_string())
                                    .filter(|v| v.chars().all(|c| c.is_alphanumeric() || c == '_') && !n.is_empty())
                            }
                            _ => None,
                        };
                        if let Some(v) = single {
                            if !v.contains('\n') && !matches!(rest.first(), Some(syn::Expr::Lit(_))) {
                                out.push(v);
                                return Some(format!("{}({})", name, out.join(", ")));
                            }
                        }
                    }
                    out.push(t);
                }
                None => {
                    self.todos.truncate(marks.0);
                    self.pending.truncate(marks.1);
                    // a Rust format string with Oxie arguments
                    if rest.is_empty() || rest.iter().any(|a| matches!(a, syn::Expr::Assign(_))) {
                        return None;
                    }
                    out.push(format!("\"{}\"", fmtstr::escape(&s.value())));
                    for a in rest {
                        out.push(self.expr(a));
                    }
                }
            }
        }
        Some(format!("{}({})", name, out.join(", ")))
    }

    /// A Rust format string and its arguments as an Oxie string literal or template.
    pub fn format_template(&mut self, fmt: &str, args: &[syn::Expr]) -> Option<String> {
        let pieces = fmtstr::parse(fmt)?;
        let mut positional: Vec<&syn::Expr> = Vec::new();
        let mut named: Vec<(String, &syn::Expr)> = Vec::new();
        for a in args {
            match a {
                syn::Expr::Assign(asg) => match &*asg.left {
                    syn::Expr::Path(p) if p.path.get_ident().is_some() => named.push((p.path.get_ident().unwrap().to_string(), &asg.right)),
                    _ => return None,
                },
                e => {
                    if !named.is_empty() {
                        return None;
                    }
                    positional.push(e);
                }
            }
        }
        // which argument each placeholder uses: index into positional ++ named
        let mut uses: Vec<Option<usize>> = Vec::new();
        let mut next = 0;
        for p in &pieces {
            if let Piece::Arg { arg, .. } = p {
                let u = match arg {
                    ArgRef::Next => {
                        next += 1;
                        Some(next - 1)
                    }
                    ArgRef::Index(i) => Some(*i),
                    ArgRef::Name(n) => named.iter().position(|(x, _)| x == n).map(|i| positional.len() + i),
                };
                if let Some(i) = u {
                    if i >= positional.len() + named.len() {
                        return None;
                    }
                }
                uses.push(u);
            }
        }
        let all: Vec<&syn::Expr> = positional.iter().copied().chain(named.iter().map(|(_, e)| *e)).collect();
        // every argument used; side-effecting ones exactly once and in order
        for (i, a) in all.iter().enumerate() {
            let count = uses.iter().filter(|u| **u == Some(i)).count();
            if count == 0 {
                return None;
            }
            if count > 1 && !is_simple(a) {
                return None;
            }
        }
        let order: Vec<usize> = uses.iter().flatten().copied().collect();
        let mut seen = std::collections::HashSet::new();
        let firsts: Vec<usize> = order.iter().copied().filter(|i| seen.insert(*i)).collect();
        if firsts.windows(2).any(|w| w[0] > w[1]) && !all.iter().all(|a| is_simple(a)) {
            return None;
        }
        let mut parts = Vec::new();
        let mut ui = 0;
        for p in &pieces {
            match p {
                Piece::Lit(s) => parts.push(TPart::Lit(s.clone())),
                Piece::Arg { arg, spec } => {
                    let u = uses[ui];
                    ui += 1;
                    // `{}` of a string or char literal is its text
                    if spec.is_empty() {
                        if let Some(i) = u {
                            match all[i] {
                                syn::Expr::Lit(syn::ExprLit { lit: syn::Lit::Str(ls), .. }) => {
                                    parts.push(TPart::Lit(ls.value()));
                                    continue;
                                }
                                syn::Expr::Lit(syn::ExprLit { lit: syn::Lit::Char(c), .. }) => {
                                    parts.push(TPart::Lit(c.value().to_string()));
                                    continue;
                                }
                                syn::Expr::Lit(syn::ExprLit { lit: syn::Lit::Int(n), .. }) => {
                                    parts.push(TPart::Lit(n.base10_digits().to_string()));
                                    continue;
                                }
                                syn::Expr::Lit(syn::ExprLit { lit: syn::Lit::Bool(b), .. }) => {
                                    parts.push(TPart::Lit(b.value.to_string()));
                                    continue;
                                }
                                _ => {}
                            }
                        }
                    }
                    let (text, simple) = match u {
                        Some(i) => {
                            let e = all[i];
                            let t = self.expr(e);
                            let simple = t.chars().all(|c| c.is_alphanumeric() || c == '_') && !t.is_empty() && !t.chars().next().unwrap().is_ascii_digit();
                            (t, simple)
                        }
                        None => {
                            let ArgRef::Name(n) = arg else { return None };
                            if self.cx.it_rename.as_deref() == Some(n.as_str()) {
                                ("it".to_string(), true)
                            } else {
                                (ident(n), true)
                            }
                        }
                    };
                    parts.push(TPart::Interp(Interp { expr: text, simple_ident: simple, spec: spec.clone() }));
                }
            }
        }
        fmtstr::template(&parts)
    }

    /// `matches!(x, P)` → `x is P`
    fn matches_macro(&mut self, m: &syn::Macro) -> Option<String> {
        let (e, p) = m
            .parse_body_with(|input: syn::parse::ParseStream| {
                let e: syn::Expr = input.parse()?;
                input.parse::<syn::Token![,]>()?;
                let p = syn::Pat::parse_multi_with_leading_vert(input)?;
                let _ = input.parse::<Option<syn::Token![,]>>()?;
                if !input.is_empty() {
                    return Err(input.error("guard"));
                }
                Ok((e, p))
            })
            .ok()?;
        if matches!(p, syn::Pat::Or(_)) {
            return None;
        }
        let subject = self.expr(&e);
        let subject = if is_postfix_safe(&e) { subject } else { format!("({})", subject) };
        Some(format!("{} is {}", subject, self.pat(&p)))
    }

    fn vec_macro(&mut self, m: &syn::Macro, ctx: &Ctx) -> Option<String> {
        let elem = ctx.expect.as_ref().and_then(vec_elem).cloned();
        // for a declared list, a list literal: Oxie gives it that type
        let vec = if elem.is_some() { "" } else { "vec" };
        if let Ok(rep) = m.parse_body_with(|input: syn::parse::ParseStream| {
            let a: syn::Expr = input.parse()?;
            input.parse::<syn::Token![;]>()?;
            let b: syn::Expr = input.parse()?;
            Ok((a, b))
        }) {
            return Some(format!("{}[{}; {}]", vec, self.expr(&rep.0), self.expr(&rep.1)));
        }
        let args = macro_args(m)?;
        let v: Vec<String> = args.iter().map(|a| self.ex(a, &Ctx::expect(elem.as_ref()))).collect();
        Some(format!("{}{}", vec, bracket_list(&v)))
    }

    // ------------------------------------------------------------------ patterns

    pub fn arm_pat(&mut self, p: &syn::Pat) -> String {
        match p {
            syn::Pat::Or(o) => {
                let v: Vec<String> = o.cases.iter().map(|c| self.pat(c)).collect();
                v.join(", ")
            }
            other => self.pat(other),
        }
    }

    pub fn pat(&mut self, p: &syn::Pat) -> String {
        match p {
            syn::Pat::Ident(pi) => {
                let mut s = String::new();
                if pi.by_ref.is_some() {
                    s.push_str("ref ");
                }
                if pi.mutability.is_some() {
                    s.push_str("mut ");
                }
                s.push_str(&id(&pi.ident));
                if let Some((_, sub)) = &pi.subpat {
                    s.push_str(&format!(" @ {}", self.pat(sub)));
                }
                s
            }
            syn::Pat::Lit(l) => self.lit(&l.lit),
            syn::Pat::Or(o) => {
                let v: Vec<String> = o.cases.iter().map(|c| self.pat(c)).collect();
                v.join(" | ")
            }
            syn::Pat::Paren(pp) => format!("({})", self.pat(&pp.pat)),
            syn::Pat::Path(pp) => self.pat_path(pp.qself.as_ref(), &pp.path),
            syn::Pat::Range(r) => {
                let a = r.start.as_ref().map(|e| self.expr(e)).unwrap_or_default();
                let b = r.end.as_ref().map(|e| self.expr(e)).unwrap_or_default();
                let op = match r.limits {
                    syn::RangeLimits::HalfOpen(_) => "..",
                    syn::RangeLimits::Closed(_) => "..=",
                };
                format!("{}{}{}", a, op, b)
            }
            syn::Pat::Reference(r) => format!("&{}{}", if r.mutability.is_some() { "mut " } else { "" }, self.pat(&r.pat)),
            syn::Pat::Rest(_) => "..".into(),
            syn::Pat::Slice(s) => {
                let v: Vec<String> = s.elems.iter().map(|x| self.pat(x)).collect();
                format!("[{}]", v.join(", "))
            }
            syn::Pat::Struct(s) => {
                let segs: Vec<String> = s.path.segments.iter().map(|x| x.ident.to_string()).collect();
                if s.qself.is_none() && s.rest.is_none() && segs.len() == 2 {
                    if let Some(names) = self.info.enums.get(&segs[0]).and_then(|ei| ei.named.get(&segs[1])) {
                        let written: Vec<String> = s
                            .fields
                            .iter()
                            .map(|f| match &f.member {
                                syn::Member::Named(i) => i.to_string(),
                                syn::Member::Unnamed(i) => i.index.to_string(),
                            })
                            .collect();
                        if &written == names && !names.is_empty() {
                            let v: Vec<String> = s.fields.iter().map(|f| self.pat(&f.pat)).collect();
                            return format!("{}({})", self.pat_path(None, &s.path), v.join(", "));
                        }
                    }
                }
                let mut v: Vec<String> = s
                    .fields
                    .iter()
                    .map(|f| {
                        let name = match &f.member {
                            syn::Member::Named(i) => id(i),
                            syn::Member::Unnamed(i) => i.index.to_string(),
                        };
                        if f.colon_token.is_none() {
                            self.pat(&f.pat)
                        } else {
                            format!("{}: {}", name, self.pat(&f.pat))
                        }
                    })
                    .collect();
                if s.rest.is_some() {
                    v.push("..".into());
                }
                let path = self.pat_path(s.qself.as_ref(), &s.path);
                if v.is_empty() {
                    format!("{} {{}}", path)
                } else {
                    format!("{} {{ {} }}", path, v.join(", "))
                }
            }
            syn::Pat::Tuple(t) => {
                let v: Vec<String> = t.elems.iter().map(|x| self.pat(x)).collect();
                if v.len() == 1 && !matches!(t.elems[0], syn::Pat::Rest(_)) {
                    format!("({},)", v[0])
                } else {
                    format!("({})", v.join(", "))
                }
            }
            syn::Pat::TupleStruct(ts) => {
                let v: Vec<String> = ts.elems.iter().map(|x| self.pat(x)).collect();
                format!("{}({})", self.pat_path(ts.qself.as_ref(), &ts.path), v.join(", "))
            }
            syn::Pat::Type(pt) => format!("{}: {}", self.pat(&pt.pat), self.ty(&pt.ty)),
            syn::Pat::Wild(_) => "_".into(),
            other => tok(other),
        }
    }

    fn pat_path(&self, qself: Option<&syn::QSelf>, path: &syn::Path) -> String {
        if qself.is_some() {
            return self.type_path(qself, path);
        }
        let segs: Vec<&syn::PathSegment> = path.segments.iter().collect();
        if path.leading_colon.is_none() && segs.len() >= 2 && segs.iter().all(|s| s.arguments.is_empty()) {
            let first = segs[0].ident.to_string();
            if first.chars().next().is_some_and(|c| c.is_uppercase()) {
                let v: Vec<String> = segs.iter().map(|s| id(&s.ident)).collect();
                return v.join(".");
            }
        }
        self.type_path(None, path)
    }
}

fn for_arm_attrs(p: &Printer, attrs: &[syn::Attribute]) -> String {
    let mut s = String::new();
    for a in attrs {
        for l in p.attr(a) {
            s.push_str(&l);
            s.push(' ');
        }
    }
    s
}

/// Does a `break` leave this `loop` (not just a loop inside it)?
fn breaks_out(l: &syn::ExprLoop) -> bool {
    use syn::visit::Visit;
    struct V<'a> {
        label: Option<&'a syn::Lifetime>,
        depth: usize,
        found: bool,
    }
    impl<'ast> Visit<'ast> for V<'_> {
        fn visit_expr_break(&mut self, b: &'ast syn::ExprBreak) {
            match &b.label {
                Some(l) => self.found |= self.label.is_some_and(|own| own.ident == l.ident),
                None => self.found |= self.depth == 0,
            }
            syn::visit::visit_expr_break(self, b);
        }
        fn visit_expr_loop(&mut self, e: &'ast syn::ExprLoop) {
            self.depth += 1;
            syn::visit::visit_expr_loop(self, e);
            self.depth -= 1;
        }
        fn visit_expr_while(&mut self, e: &'ast syn::ExprWhile) {
            self.depth += 1;
            syn::visit::visit_expr_while(self, e);
            self.depth -= 1;
        }
        fn visit_expr_for_loop(&mut self, e: &'ast syn::ExprForLoop) {
            self.depth += 1;
            syn::visit::visit_expr_for_loop(self, e);
            self.depth -= 1;
        }
        // a closure's or an item's `break` is its own
        fn visit_expr_closure(&mut self, _: &'ast syn::ExprClosure) {}
        fn visit_item(&mut self, _: &'ast syn::Item) {}
    }
    let mut v = V { label: l.label.as_ref().map(|l| &l.name), depth: 0, found: false };
    v.visit_block(&l.body);
    v.found
}

/// `[a, b]`, one element per line when long.
fn bracket_list(v: &[String]) -> String {
    let one = v.join(", ");
    if one.len() > 80 && v.len() > 1 && !one.contains('\n') {
        format!("[\n{}\n]", indent(&v.join(",\n")))
    } else {
        format!("[{}]", one)
    }
}

/// `Ok(())`
fn is_ok_unit(e: &syn::Expr) -> bool {
    matches!(call_of(e, "Ok"), Some(args) if args.len() == 1 && matches!(&args[0], syn::Expr::Tuple(t) if t.elems.is_empty()))
}

fn ok_turbofish(e: &syn::Expr) -> Option<&Punctuated<syn::Expr, syn::Token![,]>> {
    if let syn::Expr::Call(c) = e {
        if let syn::Expr::Path(p) = &*c.func {
            if p.qself.is_none() && p.path.segments.len() == 1 && p.path.segments[0].ident == "Ok" {
                return Some(&c.args);
            }
        }
    }
    None
}

fn strip_outer_elvis_parens(v: String) -> String {
    if v.starts_with('(') && v.ends_with(')') && v.contains(" ?: ") {
        let inner = &v[1..v.len() - 1];
        // only when the parentheses wrap the whole expression
        let mut depth = 0;
        for c in inner.chars() {
            match c {
                '(' => depth += 1,
                ')' => {
                    if depth == 0 {
                        return v;
                    }
                    depth -= 1;
                }
                _ => {}
            }
        }
        return inner.to_string();
    }
    v
}

/// A `match` on an expression that evaluates `$some` with `$x` bound to the node of every kind
/// that has attributes (all but `Verbatim`).
macro_rules! with_attrs {
    ($e:expr, $x:ident => $some:expr, _ => $none:expr) => {
        with_attrs!(@ $e, $x, $some, $none; Array Assign Async Await Binary Block Break Call Cast Closure Const Continue Field ForLoop
            Group If Index Infer Let Lit Loop Macro Match MethodCall Paren Path Range RawAddr Reference Repeat Return Struct Try
            TryBlock Tuple Unary Unsafe While Yield)
    };
    (@ $e:expr, $x:ident, $some:expr, $none:expr; $($kind:ident)*) => {
        match $e {
            $(syn::Expr::$kind($x) => $some,)*
            _ => $none,
        }
    };
}

/// The attributes of an expression, inner ones (`{ #![allow(x)] .. }`) included.
pub fn expr_attrs(e: &syn::Expr) -> &[syn::Attribute] {
    with_attrs!(e, x => &x.attrs, _ => &[])
}

fn expr_attrs_mut(e: &mut syn::Expr) -> Option<&mut Vec<syn::Attribute>> {
    with_attrs!(e, x => Some(&mut x.attrs), _ => None)
}

/// Attributes on an expression or on a field of a struct literal, which Oxie has no syntax for.
fn has_expr_attrs(e: &syn::Expr) -> bool {
    !expr_attrs(e).is_empty()
}

/// Where `syn` keeps the attributes written before an expression statement: on the leftmost
/// operand of an assignment, binary operation or cast, else on the expression.
fn attr_target(e: &syn::Expr) -> &syn::Expr {
    match e {
        syn::Expr::Assign(a) => attr_target(&a.left),
        syn::Expr::Binary(b) => attr_target(&b.left),
        syn::Expr::Cast(c) => attr_target(&c.expr),
        _ => e,
    }
}

fn attr_target_mut(e: &mut syn::Expr) -> &mut syn::Expr {
    match e {
        syn::Expr::Assign(a) => attr_target_mut(&mut a.left),
        syn::Expr::Binary(b) => attr_target_mut(&mut b.left),
        syn::Expr::Cast(c) => attr_target_mut(&mut c.expr),
        e => e,
    }
}

/// Splits the attributes written before an expression statement off it. Inner attributes
/// (`{ #![allow(x)] .. }`) stay with the expression.
fn split_stmt_attrs(e: &syn::Expr) -> Option<(Vec<syn::Attribute>, syn::Expr)> {
    if !expr_attrs(attr_target(e)).iter().any(|a| matches!(a.style, syn::AttrStyle::Outer)) {
        return None;
    }
    let mut e = e.clone();
    let attrs = expr_attrs_mut(attr_target_mut(&mut e))?;
    let (outer, inner): (Vec<syn::Attribute>, Vec<syn::Attribute>) = std::mem::take(attrs).into_iter().partition(|a| matches!(a.style, syn::AttrStyle::Outer));
    *attrs = inner;
    Some((outer, e))
}

/// Attributes written before a statement. Oxie prints them before it, so such a statement isn't
/// inlined into `if (c) ...` or a `when` arm.
fn has_stmt_attrs(s: &syn::Stmt) -> bool {
    match s {
        syn::Stmt::Local(l) => !l.attrs.is_empty(),
        syn::Stmt::Macro(m) => !m.attrs.is_empty(),
        syn::Stmt::Expr(e, _) => has_expr_attrs(attr_target(e)),
        syn::Stmt::Item(_) => false,
    }
}

fn stmt_start_line(s: &syn::Stmt) -> usize {
    let attrs: &[syn::Attribute] = match s {
        syn::Stmt::Local(l) => &l.attrs,
        syn::Stmt::Macro(m) => &m.attrs,
        _ => &[],
    };
    attrs.first().map(line_of).unwrap_or_else(|| line_of(s)).min(line_of(s))
}

fn needs_semi(e: &syn::Expr) -> bool {
    match e {
        syn::Expr::Assign(_) | syn::Expr::Return(_) | syn::Expr::Break(_) | syn::Expr::Continue(_) | syn::Expr::While(_) | syn::Expr::ForLoop(_) => false,
        syn::Expr::Binary(b) => !matches!(
            b.op,
            syn::BinOp::AddAssign(_)
                | syn::BinOp::SubAssign(_)
                | syn::BinOp::MulAssign(_)
                | syn::BinOp::DivAssign(_)
                | syn::BinOp::RemAssign(_)
                | syn::BinOp::BitXorAssign(_)
                | syn::BinOp::BitAndAssign(_)
                | syn::BinOp::BitOrAssign(_)
                | syn::BinOp::ShlAssign(_)
                | syn::BinOp::ShrAssign(_)
        ),
        syn::Expr::Macro(m) => !["println", "print", "eprintln", "eprint", "assert", "assert_eq", "assert_ne"].iter().any(|n| m.mac.path.is_ident(n)),
        syn::Expr::If(i) => i.else_branch.is_some(),
        _ => true,
    }
}

/// An `if` chain that ends with a plain `else`.
fn ends_with_else(i: &syn::ExprIf) -> bool {
    match &i.else_branch {
        None => false,
        Some((_, e)) => match &**e {
            syn::Expr::If(ei) => ends_with_else(ei),
            _ => true,
        },
    }
}

fn is_blocky(e: &syn::Expr) -> bool {
    matches!(
        e,
        syn::Expr::If(_) | syn::Expr::Match(_) | syn::Expr::Loop(_) | syn::Expr::ForLoop(_) | syn::Expr::While(_) | syn::Expr::Block(_) | syn::Expr::Unsafe(_)
    )
}

fn has_let(e: &syn::Expr) -> bool {
    match e {
        syn::Expr::Let(_) => true,
        syn::Expr::Binary(b) if matches!(b.op, syn::BinOp::And(_)) => has_let(&b.left) || has_let(&b.right),
        _ => false,
    }
}

fn simple_lambda_param(p: &syn::Pat) -> bool {
    match p {
        syn::Pat::Ident(pi) => pi.by_ref.is_none() && pi.subpat.is_none() && pi.mutability.is_none(),
        syn::Pat::Wild(_) => true,
        syn::Pat::Type(pt) => matches!(&*pt.pat, syn::Pat::Ident(pi) if pi.by_ref.is_none() && pi.subpat.is_none() && pi.mutability.is_none()),
        syn::Pat::Tuple(t) => {
            t.elems.iter().all(|e| matches!(e, syn::Pat::Ident(pi) if pi.by_ref.is_none() && pi.subpat.is_none()) || matches!(e, syn::Pat::Wild(_)))
        }
        _ => false,
    }
}

/// Whether a closure body is simple enough to rename its parameter to `it`.
fn it_safe(e: &syn::Expr) -> bool {
    match e {
        syn::Expr::Path(p) => p.qself.is_none(),
        syn::Expr::Lit(_) => true,
        syn::Expr::Field(f) => it_safe(&f.base),
        syn::Expr::MethodCall(m) => it_safe(&m.receiver) && m.args.iter().all(it_safe),
        syn::Expr::Call(c) => it_safe(&c.func) && c.args.iter().all(it_safe),
        syn::Expr::Binary(b) => it_safe(&b.left) && it_safe(&b.right),
        syn::Expr::Unary(u) => it_safe(&u.expr),
        syn::Expr::Paren(p) => it_safe(&p.expr),
        syn::Expr::Reference(r) => it_safe(&r.expr),
        syn::Expr::Index(i) => it_safe(&i.expr) && it_safe(&i.index),
        syn::Expr::Cast(c) => it_safe(&c.expr),
        syn::Expr::Tuple(t) => t.elems.iter().all(it_safe),
        syn::Expr::Try(t) => it_safe(&t.expr),
        _ => false,
    }
}

/// Whether a pattern binds any variable.
fn pat_binds(p: &syn::Pat) -> bool {
    use syn::visit::Visit;
    struct V(bool);
    impl<'ast> Visit<'ast> for V {
        fn visit_pat_ident(&mut self, i: &'ast syn::PatIdent) {
            let n = i.ident.to_string();
            // a capitalized single identifier is a unit struct/variant/const, not a binding
            if !(i.subpat.is_none() && i.by_ref.is_none() && i.mutability.is_none() && n.chars().next().is_some_and(|c| c.is_uppercase())) {
                self.0 = true;
            }
            syn::visit::visit_pat_ident(self, i);
        }
    }
    let mut v = V(false);
    v.visit_pat(p);
    v.0
}
