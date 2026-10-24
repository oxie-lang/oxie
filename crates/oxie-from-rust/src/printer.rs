//! Rust items → Oxie declarations.

use std::collections::{HashMap, HashSet};

use quote::ToTokens;
use syn::spanned::Spanned;

use crate::comments::Comment;
use crate::contexts::{self, Crate};
use crate::facts::{self, MethodKey, Recv};
use crate::fmtstr;
use crate::types::{id, is_any_error, tok};

/// Corrections found by compiling the translation back to Rust (see `facts`).
#[derive(Default, Clone, Debug)]
pub struct Fixes {
    /// Methods whose receiver must be written out.
    pub explicit_recv: HashSet<MethodKey>,
    /// Types whose derives must be written out (`@no_auto_derive` / `class` instead of `data class`).
    pub explicit_derives: HashSet<String>,
    /// Top-level items (by index) emitted as Rust in a `rust { }` block, with the reason.
    pub raw_items: HashMap<usize, String>,
    /// Methods of top-level inherent impls (item index, index in the impl) emitted as Rust in
    /// an `impl` block of their own after the item, with the reason. The rest stays Oxie.
    pub raw_methods: HashMap<(usize, usize), String>,
    /// Methods whose contexts Oxie infers differently than they are declared: they get a
    /// `context(...)` of their own.
    pub ctx_clause: HashSet<MethodKey>,
    /// Calls (function by `Cx::fn_path`, callee name) that pass their contexts as written.
    pub ctx_calls: HashSet<(String, String)>,
}

/// Starts a line that marks where a top-level item (`i3`) or a method that can be kept as Rust
/// on its own (`m3:5`) begins, or where it ends (`/`); see [`Printer::strip_marks`].
const MARK: char = '\u{1}';

/// A method's text between marks.
fn mark_method(item: usize, index: usize, text: &str) -> String {
    format!("{MARK}m{item}:{index}\n{text}\n{MARK}/")
}

#[derive(Clone, Debug)]
pub struct StructInfo {
    pub fields: Vec<(String, syn::Type)>,
    /// Has a `new` function (generated or written) so `Name(args)` constructs it.
    pub has_new: bool,
    /// Parameter types of `new`.
    pub new_params: Vec<syn::Type>,
}

#[derive(Clone, Debug, Default)]
pub struct EnumInfo {
    /// variant name → named fields (empty for tuple and unit variants)
    pub named: HashMap<String, Vec<String>>,
}

#[derive(Clone, Debug, Default)]
pub struct Info {
    pub structs: HashMap<String, StructInfo>,
    pub enums: HashMap<String, EnumInfo>,
    pub traits: HashSet<String>,
    /// free functions → parameter types
    pub fns: HashMap<String, Vec<syn::Type>>,
    /// free functions that return an `Option`
    pub option_fns: HashSet<String>,
    /// every free function's name, in any module of the file
    pub free_fns: HashSet<String>,
    /// the functions of each type's impls (`Counter`) and of each trait (`trait Shape`)
    pub type_fns: HashMap<String, HashSet<String>>,
    /// functions and methods that are translated to `throws`
    pub throwing: HashSet<String>,
    /// lowercase names usable as module paths with `.` (`fs.read_to_string`)
    pub modules: HashSet<String>,
    /// a function named `run` hides Oxie's `run { }` block
    pub run_shadowed: bool,
}

#[derive(Clone, Default)]
pub struct ClassCx {
    pub fields: HashSet<String>,
    pub methods: HashSet<String>,
}

/// Context of the function being printed.
#[derive(Clone, Default)]
pub struct Cx {
    pub class: Option<ClassCx>,
    /// receivers key: (type, trait)
    pub impl_key: Option<(String, String)>,
    pub throws: bool,
    /// the error type of a `throws` function converts from `&str` (`String`, `AnyError`)
    pub err_from_str: bool,
    pub ret: Option<syn::Type>,
    /// the declared `Result<T, E>` of a function translated to `throws`
    pub result_ty: Option<syn::Type>,
    pub bound: HashSet<String>,
    pub it_rename: Option<String>,
    pub in_trait: bool,
    /// prefix for items declared inside the current function (matches `facts`)
    pub fn_path: String,
    /// the functions of the type whose method this is: in Oxie a bare `name()` there calls
    /// one of them, so a free function of that name is called as `self::name()`
    pub type_fns: HashSet<String>,
    /// the class whose method this is, when it has contexts (see `contexts`)
    pub ctx_class: Option<String>,
}

pub struct Printer<'a> {
    pub info: Info,
    pub fixes: &'a Fixes,
    pub krate: &'a Crate,
    pub comments: Vec<Comment>,
    pub cx: Cx,
    /// Warnings with line numbers (for the command line).
    pub todos: Vec<String>,
    /// `TODO(from-rust)` comments to print above the next statement or item.
    pub pending: Vec<String>,
    /// Byte ranges of top-level items in the output, for mapping Oxie diagnostics back.
    pub item_ranges: Vec<(usize, std::ops::Range<usize>)>,
    /// Byte ranges of the methods [`Fixes::raw_methods`] can keep as Rust.
    pub method_ranges: Vec<((usize, usize), std::ops::Range<usize>)>,
    pub mod_prefix: String,
    /// Printing the file's own items (not those of a module inside it).
    top_level: bool,
    /// `rust { impl .. }` blocks of methods kept as Rust, printed after the current item.
    after_item: Vec<String>,
}

pub fn indent(s: &str) -> String {
    let mut out = String::new();
    for (i, l) in s.split('\n').enumerate() {
        if i > 0 {
            out.push('\n');
        }
        if !l.is_empty() {
            out.push_str("    ");
            out.push_str(l);
        }
    }
    out
}

pub fn line_of<T: Spanned>(t: &T) -> usize {
    t.span().start().line
}

pub fn end_line_of<T: Spanned>(t: &T) -> usize {
    t.span().end().line
}

const AUTO_STATIC: &[&str] = &["new", "default", "from", "from_str", "try_from", "from_iter"];

impl<'a> Printer<'a> {
    pub fn new(info: Info, fixes: &'a Fixes, krate: &'a Crate, comments: Vec<Comment>) -> Self {
        Printer {
            info,
            fixes,
            krate,
            comments,
            cx: Cx::default(),
            todos: Vec::new(),
            pending: Vec::new(),
            item_ranges: Vec::new(),
            method_ranges: Vec::new(),
            mod_prefix: String::new(),
            top_level: false,
            after_item: Vec::new(),
        }
    }

    /// Records something kept as Rust: a warning, and a `TODO(from-rust)` comment.
    pub fn todo(&mut self, line: usize, msg: &str) {
        let w = format!("line {}: {}", line, msg);
        if self.todos.last() == Some(&w) {
            return;
        }
        self.todos.push(w);
        self.pending.push(format!("// TODO(from-rust): {}", msg));
    }

    /// Removes the marks of items and methods from the printed text, recording where they are.
    fn strip_marks(&mut self, text: &str) -> String {
        enum Open {
            Item(usize, usize),
            Method((usize, usize), usize),
        }
        let mut out = String::with_capacity(text.len());
        let mut open = Vec::new();
        for line in text.split_inclusive('\n') {
            let Some(m) = line.trim_start().strip_prefix(MARK) else {
                out.push_str(line);
                continue;
            };
            let m = m.trim_end();
            if m == "/" {
                match open.pop() {
                    Some(Open::Item(i, start)) => self.item_ranges.push((i, start..out.len())),
                    Some(Open::Method(k, start)) => self.method_ranges.push((k, start..out.len())),
                    None => {}
                }
            } else if let Some(Ok(i)) = m.strip_prefix('i').map(str::parse) {
                open.push(Open::Item(i, out.len()));
            } else if let Some((Ok(i), Ok(j))) = m.strip_prefix('m').and_then(|k| k.split_once(':')).map(|(a, b)| (a.parse(), b.parse())) {
                open.push(Open::Method((i, j), out.len()));
            }
        }
        out
    }

    /// The methods of `imp` kept as Rust (`raw`: index, reason, comments before and inside),
    /// in an `impl` block of their own.
    fn raw_methods(&mut self, imp: &syn::ItemImpl, raw: Vec<(usize, String, Vec<String>)>) -> String {
        let mut only = imp.clone();
        only.items = raw.iter().map(|(j, ..)| imp.items[*j].clone()).collect();
        let mut lines = Vec::new();
        for (j, reason, comments) in raw {
            lines.extend(comments);
            let msg = format!("kept as Rust: {}", reason);
            self.todos.push(format!("line {}: {}", line_of(&imp.items[j]), msg));
            lines.push(format!("// TODO(from-rust): {}", msg));
        }
        lines.push(format!("rust {{\n{}\n}}", indent(&rust_item(&syn::Item::Impl(only)))));
        lines.join("\n")
    }

    // ------------------------------------------------------------------ comments

    /// Comments that start in `[lo, hi)` and were not printed yet, as lines.
    pub fn take_comments(&mut self, lo: usize, hi: usize) -> Vec<String> {
        let mut out = Vec::new();
        for c in self.comments.iter_mut() {
            if !c.used && c.line >= lo && c.line < hi {
                c.used = true;
                out.extend(c.text.split('\n').map(|l| l.trim_end().to_string()));
            }
        }
        out
    }

    /// A comment after code on line `line`.
    pub fn trailing_comment(&mut self, line: usize) -> Option<String> {
        for c in self.comments.iter_mut() {
            if !c.used && !c.own_line && c.line == line && c.end_line == line {
                c.used = true;
                return Some(c.text.clone());
            }
        }
        None
    }

    pub fn has_comments_in(&self, lo: usize, hi: usize) -> bool {
        self.comments.iter().any(|c| !c.used && c.line >= lo && c.line <= hi)
    }

    // ------------------------------------------------------------------ file

    pub fn file(&mut self, f: &syn::File) -> String {
        let mut out = String::new();
        for a in &f.attrs {
            for l in self.attr(a) {
                out.push_str(&l);
                out.push('\n');
            }
        }
        let body = self.items(&f.items, 0, true);
        if !out.is_empty() && !body.is_empty() {
            out.push('\n');
        }
        out.push_str(&body);
        let rest = self.take_comments(0, usize::MAX);
        if !rest.is_empty() {
            out.push('\n');
            out.push_str(&rest.join("\n"));
        }
        while out.ends_with("\n\n") {
            out.pop();
        }
        if !out.ends_with('\n') {
            out.push('\n');
        }
        self.strip_marks(&out)
    }

    /// A list of items (file or module body). Inherent impls of structs and enums declared in
    /// the same list are merged into them.
    pub fn items(&mut self, items: &[syn::Item], lo: usize, top: bool) -> String {
        let outer_top = std::mem::replace(&mut self.top_level, top);
        let plan = self.plan(items, top);
        let mut out = String::new();
        let mut first = true;
        let mut prev_use: Option<usize> = None;
        for (i, it) in items.iter().enumerate() {
            if plan.merged.contains(&i) || plan.dropped.contains(&i) {
                continue;
            }
            if matches!(it, syn::Item::Macro(m) if m.mac.path.is_ident("__oxie_skip")) {
                continue;
            }
            let start = item_start_line(it);
            let cs = self.take_comments(lo, start);
            let pending = self.pending.len();
            let mut text = match self.fixes.raw_items.get(&i) {
                Some(reason) if top => {
                    let reason = reason.clone();
                    self.raw_item(it, &reason)
                }
                _ => self.item(it, &plan, i, items),
            };
            if self.pending.len() > pending {
                let todo: Vec<String> = self.pending.drain(pending..).collect();
                text = format!("{}\n{}", todo.join("\n"), text);
            }
            if !text.is_empty() {
                if let Some(tc) = self.trailing_comment(end_line_of(it)) {
                    text.push(' ');
                    text.push_str(&tc);
                }
            }
            // its methods kept as Rust follow it
            for raw in std::mem::take(&mut self.after_item) {
                if !text.is_empty() {
                    text.push_str("\n\n");
                }
                text.push_str(&raw);
            }
            if text.is_empty() {
                continue;
            }
            if !first {
                // keep `use` lines together
                let tight = matches!(it, syn::Item::Use(_)) && prev_use == Some(i.wrapping_sub(1)) && cs.is_empty() && prev_end_adjacent(items, i);
                out.push_str(if tight { "\n" } else { "\n\n" });
            }
            first = false;
            prev_use = if matches!(it, syn::Item::Use(_)) { Some(i) } else { None };
            for c in cs {
                out.push_str(&c);
                out.push('\n');
            }
            if top {
                out.push_str(&format!("{MARK}i{i}\n{text}\n{MARK}/"));
            } else {
                out.push_str(&text);
            }
        }
        self.top_level = outer_top;
        out
    }

    fn raw_item(&mut self, it: &syn::Item, reason: &str) -> String {
        // comments inside the item are not part of the Rust printed by prettyplease; print them
        // first rather than letting them drift to the next item
        let (lo, hi) = (item_start_line(it), end_line_of(it));
        let inner = self.take_comments(lo, hi + 1);
        let mut s = String::new();
        for c in inner {
            s.push_str(&c);
            s.push('\n');
        }
        if let syn::Item::Macro(m) = it {
            if m.mac.path.is_ident("macro_rules") {
                s.push_str(&rust_item(it));
                return s;
            }
        }
        self.todo(lo, &format!("kept as Rust: {}", reason));
        s.push_str(&format!("rust {{\n{}\n}}", indent(&rust_item(it))));
        s
    }

    // ------------------------------------------------------------------ planning

    fn plan(&mut self, items: &[syn::Item], top: bool) -> Plan {
        let mut plan = Plan::default();
        // types declared here
        let mut types: HashMap<String, usize> = HashMap::new();
        for (i, it) in items.iter().enumerate() {
            if top && self.fixes.raw_items.contains_key(&i) {
                continue;
            }
            match it {
                syn::Item::Struct(s) => {
                    types.insert(s.ident.to_string(), i);
                }
                syn::Item::Enum(e) => {
                    types.insert(e.ident.to_string(), i);
                }
                _ => {}
            }
        }
        for (i, it) in items.iter().enumerate() {
            let syn::Item::Impl(imp) = it else { continue };
            if top && self.fixes.raw_items.contains_key(&i) {
                continue;
            }
            let syn::Type::Path(tp) = &*imp.self_ty else { continue };
            if tp.qself.is_some() || tp.path.segments.len() != 1 {
                continue;
            }
            let name = tp.path.segments[0].ident.to_string();
            let Some(&ti) = types.get(&name) else { continue };
            let (generics, is_class_like) = match &items[ti] {
                syn::Item::Struct(s) => (&s.generics, self.struct_is_class(s)),
                syn::Item::Enum(e) => (&e.generics, true),
                _ => continue,
            };
            if !is_class_like || imp.unsafety.is_some() || imp.defaultness.is_some() || !imp.attrs.is_empty() {
                continue;
            }
            // same generics as the declaration, applied in order
            if tok(&imp.generics.params) != tok(&generics.params) || imp.generics.where_clause.is_some() != generics.where_clause.is_some() {
                continue;
            }
            if imp.generics.where_clause.is_some() && tok(&imp.generics.where_clause) != tok(&generics.where_clause) {
                continue;
            }
            let expected_args: Vec<String> = generics
                .params
                .iter()
                .map(|p| match p {
                    syn::GenericParam::Lifetime(l) => l.lifetime.to_string(),
                    syn::GenericParam::Type(t) => t.ident.to_string(),
                    syn::GenericParam::Const(c) => c.ident.to_string(),
                })
                .collect();
            let actual_args: Vec<String> = match &tp.path.segments[0].arguments {
                syn::PathArguments::None => Vec::new(),
                syn::PathArguments::AngleBracketed(a) => a.args.iter().map(tok).collect(),
                _ => continue,
            };
            if expected_args != actual_args {
                continue;
            }
            let all_fns = imp.items.iter().all(|x| matches!(x, syn::ImplItem::Fn(f) if f.attrs.iter().all(|a| !a.path().is_ident("cfg"))));
            match &imp.trait_ {
                None => {
                    if all_fns {
                        plan.merged.insert(i);
                        plan.members.entry(ti).or_default().push(Member::Inherent(i));
                    }
                }
                Some((None, path, _)) => {
                    let tname = path.segments.last().map(|s| s.ident.to_string()).unwrap_or_default();
                    let trait_args = path.segments.last().is_some_and(|s| !s.arguments.is_empty());
                    if tname == "Display" && generics.params.is_empty() && imp.items.len() == 1 {
                        if let Some(body) = self.display_body(imp, &items[ti]) {
                            plan.merged.insert(i);
                            plan.members.entry(ti).or_default().push(Member::ToString(i, body));
                            continue;
                        }
                    }
                    if let Some(op) = self.operator_member(imp, &tname, generics, &name) {
                        plan.merged.insert(i);
                        plan.members.entry(ti).or_default().push(Member::Operator(i, op));
                        continue;
                    }
                    let local_trait = path.segments.len() == 1 && self.info.traits.contains(&tname);
                    if local_trait && !trait_args && all_fns {
                        plan.merged.insert(i);
                        plan.members.entry(ti).or_default().push(Member::Trait(i, tname));
                    }
                }
                _ => {}
            }
        }
        plan
    }

    /// `impl Add for V { type Output = V; fn add(self, o: V) -> V }` as `operator fun plus(o: V): V`.
    fn operator_member(&self, imp: &syn::ItemImpl, tname: &str, generics: &syn::Generics, ty_name: &str) -> Option<String> {
        const OPS: &[(&str, &str, &str, usize)] = &[
            ("Add", "add", "plus", 1),
            ("Sub", "sub", "minus", 1),
            ("Mul", "mul", "times", 1),
            ("Div", "div", "div", 1),
            ("Rem", "rem", "rem", 1),
            ("Neg", "neg", "unaryMinus", 0),
            ("Not", "not", "not", 0),
            ("AddAssign", "add_assign", "plusAssign", 1),
            ("SubAssign", "sub_assign", "minusAssign", 1),
            ("MulAssign", "mul_assign", "timesAssign", 1),
            ("DivAssign", "div_assign", "divAssign", 1),
            ("RemAssign", "rem_assign", "remAssign", 1),
        ];
        let &(_, rust, oxie, params) = OPS.iter().find(|o| o.0 == tname)?;
        if self.info.traits.contains(tname) || !generics.params.is_empty() || !imp.generics.params.is_empty() || imp.unsafety.is_some() || !imp.attrs.is_empty()
        {
            return None;
        }
        let (_, path, _) = imp.trait_.as_ref()?;
        let rhs: Option<String> = match &path.segments.last()?.arguments {
            syn::PathArguments::None => None,
            syn::PathArguments::AngleBracketed(a) if a.args.len() == 1 => match &a.args[0] {
                syn::GenericArgument::Type(t) => Some(tok(t)),
                _ => return None,
            },
            _ => return None,
        };
        let mut output = None;
        let mut fun = None;
        for ii in &imp.items {
            match ii {
                syn::ImplItem::Type(t) if t.ident == "Output" && t.generics.params.is_empty() => output = Some(tok(&t.ty)),
                syn::ImplItem::Fn(f) if f.sig.ident == rust && f.attrs.iter().all(|a| a.path().is_ident("inline") || a.path().is_ident("doc")) => fun = Some(f),
                _ => return None,
            }
        }
        let f = fun?;
        let assign = rust.ends_with("_assign");
        let recv = facts::recv_of(&f.sig);
        if (assign && recv != Recv::RefMut) || (!assign && recv != Recv::Value) || f.sig.inputs.len() != params + 1 || !f.sig.generics.params.is_empty() {
            return None;
        }
        if assign != output.is_none() {
            return None;
        }
        if params == 1 {
            let syn::FnArg::Typed(pt) = &f.sig.inputs[1] else { return None };
            let pty = tok(&*pt.ty);
            let expected = rhs.unwrap_or_else(|| ty_name.to_string());
            if pty != expected && !(pty == "Self" && expected == ty_name) {
                return None;
            }
        }
        if let Some(o) = &output {
            match &f.sig.output {
                syn::ReturnType::Type(_, t) => {
                    let r = tok(&**t);
                    if r != *o && r != "Self::Output" && !(r == "Self" && o == ty_name) {
                        return None;
                    }
                }
                _ => return None,
            }
        }
        Some(oxie.to_string())
    }

    /// `impl Display for X { fn fmt(&self, f) -> fmt::Result { write!(f, "...", ...) } }` as the
    /// body of `override fun toString()`.
    fn display_body(&mut self, imp: &syn::ItemImpl, decl: &syn::Item) -> Option<String> {
        let syn::ImplItem::Fn(f) = &imp.items[0] else { return None };
        if f.sig.ident != "fmt" || f.block.stmts.len() != 1 || facts::recv_of(&f.sig) != Recv::Ref {
            return None;
        }
        if self.has_comments_in(line_of(f), end_line_of(f)) {
            return None;
        }
        let fname = match f.sig.inputs.iter().nth(1)? {
            syn::FnArg::Typed(pt) => match &*pt.pat {
                syn::Pat::Ident(pi) => pi.ident.to_string(),
                _ => return None,
            },
            _ => return None,
        };
        let syn::Stmt::Expr(body, None) = &f.block.stmts[0] else { return None };
        if !crate::expr::expr_attrs(body).is_empty() {
            return None;
        }
        // the printer context of a method of this type
        let saved = std::mem::take(&mut self.cx);
        self.cx = self.method_cx(decl, &f.sig, &f.block, &HashSet::new());
        let marks = (self.todos.len(), self.pending.len());
        let r = (|| {
            if let Some(t) = self.write_template(body, &fname) {
                return Some(t);
            }
            let syn::Expr::Match(m) = body else { return None };
            if !matches!(&*m.expr, syn::Expr::Path(p) if p.path.is_ident("self"))
                && !matches!(&*m.expr, syn::Expr::Unary(u) if matches!(&*u.expr, syn::Expr::Path(p) if p.path.is_ident("self")))
            {
                return None;
            }
            let mut arms = Vec::new();
            for arm in &m.arms {
                if arm.guard.is_some() || !arm.attrs.is_empty() {
                    return None;
                }
                let b = match &*arm.body {
                    syn::Expr::Block(b) if b.block.stmts.len() == 1 => match &b.block.stmts[0] {
                        syn::Stmt::Expr(e, None) => e,
                        _ => return None,
                    },
                    e => e,
                };
                let t = self.write_template(b, &fname)?;
                arms.push(format!("{} -> {}", self.arm_pat(&arm.pat), t));
            }
            Some(format!("when (self) {{\n{}\n}}", indent(&arms.join("\n"))))
        })();
        self.cx = saved;
        // anything kept as Rust inside: leave the impl as it is
        if self.todos.len() > marks.0 || self.pending.len() > marks.1 {
            self.todos.truncate(marks.0);
            self.pending.truncate(marks.1);
            return None;
        }
        r
    }

    /// `write!(f, "...", args)` / `f.write_str("...")` as an Oxie string.
    fn write_template(&mut self, e: &syn::Expr, f: &str) -> Option<String> {
        if !crate::expr::expr_attrs(e).is_empty() {
            return None;
        }
        match e {
            syn::Expr::Macro(m) if m.mac.path.is_ident("write") => {
                let args = crate::expr::macro_args(&m.mac)?;
                if args.len() < 2 || !matches!(&args[0], syn::Expr::Path(p) if p.path.is_ident(f)) {
                    return None;
                }
                if args[1..].iter().any(|a| mentions(a, f)) {
                    return None;
                }
                let syn::Expr::Lit(syn::ExprLit { lit: syn::Lit::Str(s), .. }) = &args[1] else { return None };
                let t = self.format_template(&s.value(), &args[2..])?;
                Some(t)
            }
            syn::Expr::MethodCall(mc) if mc.method == "write_str" && mc.args.len() == 1 => {
                if !matches!(&*mc.receiver, syn::Expr::Path(p) if p.path.is_ident(f)) {
                    return None;
                }
                match &mc.args[0] {
                    syn::Expr::Lit(syn::ExprLit { lit: syn::Lit::Str(s), .. }) => Some(fmtstr::quote(&s.value())),
                    _ => None,
                }
            }
            _ => None,
        }
    }

    fn struct_is_class(&self, s: &syn::ItemStruct) -> bool {
        match &s.fields {
            syn::Fields::Named(n) => !n.named.is_empty() && n.named.iter().all(|f| only_docs(&f.attrs) && f.mutability == syn::FieldMutability::None),
            _ => false,
        }
    }

    // ------------------------------------------------------------------ items

    fn item(&mut self, it: &syn::Item, plan: &Plan, idx: usize, items: &[syn::Item]) -> String {
        match it {
            syn::Item::Fn(f) => {
                let saved = std::mem::take(&mut self.cx);
                let s = self.fun(&f.attrs, &f.vis, &f.sig, Some(&f.block), FunKind::Free);
                self.cx = saved;
                s
            }
            syn::Item::Struct(s) => self.struct_item(s, plan.members.get(&idx).map(|v| v.as_slice()).unwrap_or(&[]), items),
            syn::Item::Enum(e) => self.enum_item(e, plan.members.get(&idx).map(|v| v.as_slice()).unwrap_or(&[]), items),
            syn::Item::Impl(i) => self.impl_item(i, idx),
            syn::Item::Trait(t) => self.trait_item(t),
            syn::Item::Use(u) => self.use_item(u),
            syn::Item::Const(c) => {
                let mut lines = self.attrs(&c.attrs, false);
                let saved = std::mem::take(&mut self.cx);
                let v = self.expr_expect(&c.expr, Some(&c.ty));
                self.cx = saved;
                lines.push(format!("{}const {}: {} = {}", self.vis(&c.vis), id(&c.ident), self.ty(&c.ty), v));
                lines.join("\n")
            }
            syn::Item::Static(s) => {
                let mut lines = self.attrs(&s.attrs, false);
                let saved = std::mem::take(&mut self.cx);
                let v = self.expr_expect(&s.expr, Some(&s.ty));
                self.cx = saved;
                let kw = if matches!(s.mutability, syn::StaticMutability::Mut(_)) { "static var" } else { "static" };
                lines.push(format!("{}{} {}: {} = {}", self.vis(&s.vis), kw, id(&s.ident), self.ty(&s.ty), v));
                lines.join("\n")
            }
            syn::Item::Type(t) => {
                let mut lines = self.attrs(&t.attrs, false);
                lines.push(format!("{}typealias {}{} = {}", self.vis(&t.vis), id(&t.ident), self.generics(&t.generics), self.ty(&t.ty)));
                lines.join("\n")
            }
            syn::Item::Mod(m) => {
                if let Some(t) = self.test_blocks(m) {
                    return t;
                }
                let mut lines = self.attrs(&m.attrs, false);
                match &m.content {
                    Some((_, items)) => {
                        if m.ident == "__oxie" {
                            return String::new();
                        }
                        let np = format!("{}{}::", self.mod_prefix, m.ident);
                        let saved = std::mem::replace(&mut self.mod_prefix, np);
                        let body = self.items(items, line_of(m), false);
                        let close = self.take_comments(line_of(m), end_line_of(m));
                        self.mod_prefix = saved;
                        let mut b = body;
                        if !close.is_empty() {
                            b.push('\n');
                            b.push_str(&close.join("\n"));
                        }
                        lines.push(format!("{}mod {} {{\n{}\n}}", self.vis(&m.vis), id(&m.ident), indent(&b)));
                    }
                    None => {
                        // `mod name` declares a Rust file; Oxie finds `name.oxi` by itself
                        lines.push(format!("// TODO(from-rust): remove this line if {}.rs is translated to {}.oxi too", m.ident, m.ident));
                        lines.push(format!("{}mod {}", self.vis(&m.vis), id(&m.ident)));
                    }
                }
                lines.join("\n")
            }
            syn::Item::Macro(m) if m.mac.path.is_ident("macro_rules") => rust_item(it),
            syn::Item::Union(u) => {
                let mut lines = self.attrs(&u.attrs, false);
                let fs: Vec<String> = u
                    .fields
                    .named
                    .iter()
                    .map(|f| format!("{}{}{}: {}", self.field_attrs_inline(&f.attrs), self.vis(&f.vis), id(f.ident.as_ref().unwrap()), self.ty(&f.ty)))
                    .collect();
                lines.push(format!(
                    "{}union {}{}{} {{\n{}\n}}",
                    self.vis(&u.vis),
                    id(&u.ident),
                    self.generics(&u.generics),
                    self.where_clause(&u.generics),
                    indent(&fs.join("\n"))
                ));
                lines.join("\n")
            }
            syn::Item::Macro(_) => self.raw_item(it, "item macro"),
            _ => self.raw_item(it, "no Oxie syntax for this item"),
        }
    }

    /// `#[cfg(test)] mod tests { use super::*; #[test] fn adds() { .. } }` → `test "adds" { .. }`
    fn test_blocks(&mut self, m: &syn::ItemMod) -> Option<String> {
        let (_, items) = m.content.as_ref()?;
        let cfg_test = m.attrs.len() == 1 && m.attrs[0].path().is_ident("cfg") && tok(&m.attrs[0].meta) == "cfg(test)";
        if !cfg_test {
            return None;
        }
        let mut tests = Vec::new();
        for it in items {
            match it {
                syn::Item::Use(u) if tok(&u.tree) == "super::*" && u.attrs.iter().all(|a| a.path().is_ident("allow")) => {}
                syn::Item::Fn(f) => {
                    let is_test = f.attrs.iter().any(|a| a.path().is_ident("test") && matches!(a.meta, syn::Meta::Path(_)));
                    let others_ok = f.attrs.iter().all(|a| a.path().is_ident("test") || a.path().is_ident("doc"));
                    if !is_test || !others_ok || !f.sig.inputs.is_empty() || !f.sig.generics.params.is_empty() || f.sig.asyncness.is_some() {
                        return None;
                    }
                    match &f.sig.output {
                        syn::ReturnType::Default => {}
                        syn::ReturnType::Type(_, t) if result_parts(t).is_some_and(|(ok, _)| is_unit(&ok)) => {}
                        _ => return None,
                    }
                    tests.push(f);
                }
                _ => return None,
            }
        }
        if tests.is_empty() {
            return None;
        }
        let mut out = Vec::new();
        let lo = line_of(m);
        for f in tests {
            let mut lines = Vec::new();
            for c in self.take_comments(lo, line_of(f)) {
                lines.push(c);
            }
            // the description: a doc comment written by `oxie emit`, else the function name
            let doc: Vec<String> = f
                .attrs
                .iter()
                .filter(|a| a.path().is_ident("doc"))
                .filter_map(|a| match &a.meta {
                    syn::Meta::NameValue(nv) => match &nv.value {
                        syn::Expr::Lit(syn::ExprLit { lit: syn::Lit::Str(s), .. }) => Some(s.value().trim().to_string()),
                        _ => None,
                    },
                    _ => None,
                })
                .collect();
            let name = f.sig.ident.to_string();
            let desc = if doc.len() == 1
                && name == format!("test_{}", doc[0].chars().map(|c| if c.is_alphanumeric() { c.to_ascii_lowercase() } else { '_' }).collect::<String>())
            {
                doc[0].clone()
            } else {
                for d in &doc {
                    lines.push(format!("///{}", if d.is_empty() { String::new() } else { format!(" {}", d) }));
                }
                name.trim_start_matches("test_").replace('_', " ")
            };
            let saved = std::mem::take(&mut self.cx);
            self.cx.bound = bound_names(&f.sig, &f.block);
            let throws = !matches!(f.sig.output, syn::ReturnType::Default);
            self.cx.throws = throws;
            self.cx.err_from_str =
                throws && matches!(&f.sig.output, syn::ReturnType::Type(_, t) if result_parts(t).is_some_and(|(_, e)| is_any_error(&e) || tok(&e) == "String"));
            let body = self.block_body(&f.block, BlockMode::UnitFn);
            self.cx = saved;
            let head = format!("test {}{}", fmtstr::quote(&desc), if throws { " throws" } else { "" });
            if body.is_empty() {
                lines.push(format!("{} {{}}", head));
            } else {
                lines.push(format!("{} {{\n{}\n}}", head, indent(&body)));
            }
            out.push(lines.join("\n"));
        }
        let _ = self.take_comments(lo, end_line_of(m));
        Some(out.join("\n\n"))
    }

    fn use_item(&mut self, u: &syn::ItemUse) -> String {
        let mut lines = self.attrs(&u.attrs, false);
        let mut tree = use_tree(&u.tree);
        // a trailing `*` would continue the statement onto the next line
        if tree.ends_with('*') {
            tree.push(';');
        }
        if matches!(u.vis, syn::Visibility::Inherited) {
            lines.push(format!("import {}{}", if u.leading_colon.is_some() { "::" } else { "" }, tree));
        } else {
            lines.push(format!("{}use {}{}", self.vis(&u.vis), if u.leading_colon.is_some() { "::" } else { "" }, tree));
        }
        lines.join("\n")
    }

    // ------------------------------------------------------------------ structs

    fn struct_item(&mut self, s: &syn::ItemStruct, members: &[Member], items: &[syn::Item]) -> String {
        let name = s.ident.to_string();
        let key = format!("{}{}", self.mod_prefix, name);
        let derives = facts::derives_of(&s.attrs);
        let mut lines = Vec::new();
        if !self.struct_is_class(s) {
            // Rust-style struct
            lines.extend(self.attrs(&s.attrs, false));
            let head = format!("{}struct {}{}", self.vis(&s.vis), id(&s.ident), self.generics(&s.generics));
            match &s.fields {
                syn::Fields::Unit => lines.push(format!("{}{}", head, self.where_clause(&s.generics))),
                syn::Fields::Unnamed(u) => {
                    let fs: Vec<String> =
                        u.unnamed.iter().map(|f| format!("{}{}{}", self.field_attrs_inline(&f.attrs), self.vis(&f.vis), self.ty(&f.ty))).collect();
                    lines.push(format!("{}({}){}", head, fs.join(", "), self.where_clause(&s.generics)));
                }
                syn::Fields::Named(n) => {
                    let fs: Vec<String> = n
                        .named
                        .iter()
                        .map(|f| format!("{}{}{}: {}", self.field_attrs_inline(&f.attrs), self.vis(&f.vis), id(f.ident.as_ref().unwrap()), self.ty(&f.ty)))
                        .collect();
                    if fs.is_empty() {
                        lines.push(format!("{}{} {{}}", head, self.where_clause(&s.generics)));
                    } else {
                        lines.push(format!("{}{} {{\n{}\n}}", head, self.where_clause(&s.generics), indent(&fs.join("\n"))));
                    }
                }
            }
            return lines.join("\n");
        }
        let syn::Fields::Named(fields) = &s.fields else { unreachable!() };
        // data class: derives exactly what Oxie derives automatically
        let auto_ok = ["Debug", "Clone", "PartialEq"].iter().all(|d| derives.contains(*d))
            && derives.iter().all(|d| ["Debug", "Clone", "Copy", "PartialEq", "Eq", "Hash"].contains(&d.as_str()))
            && !self.fixes.explicit_derives.contains(&key);
        let data = auto_ok;
        let other_attrs: Vec<&syn::Attribute> = s.attrs.iter().filter(|a| !a.path().is_ident("derive")).collect();
        for a in &other_attrs {
            lines.extend(self.attr(a));
        }
        if !data && !derives.is_empty() {
            lines.push(format!("@derive({})", derive_list(&s.attrs).join(", ")));
        }
        if let Some(c) = self.krate.class(&name) {
            lines.push(self.context_clause(&c.contexts));
        }
        let params: Vec<String> = fields
            .named
            .iter()
            .map(|f| format!("{}{}{}: {}", self.field_attrs_inline(&f.attrs), self.vis(&f.vis), id(f.ident.as_ref().unwrap()), self.ty(&f.ty)))
            .collect();
        let mut supers = Vec::new();
        for m in members {
            if let Member::Trait(_, t) = m {
                supers.push(t.clone());
            }
        }
        let mut head =
            format!("{}{}class {}{}({})", self.vis(&s.vis), if data { "data " } else { "" }, id(&s.ident), self.generics(&s.generics), params.join(", "));
        if head.len() > 100 || head.contains('\n') {
            head = format!(
                "{}{}class {}{}(\n{},\n)",
                self.vis(&s.vis),
                if data { "data " } else { "" },
                id(&s.ident),
                self.generics(&s.generics),
                indent(&params.join(",\n"))
            );
        }
        if !supers.is_empty() {
            head.push_str(&format!(" : {}", supers.join(", ")));
        }
        head.push_str(&self.where_clause(&s.generics));
        let body = self.class_body(&syn::Item::Struct(s.clone()), members, items);
        if body.trim().is_empty() {
            lines.push(head);
        } else {
            lines.push(format!("{} {{\n{}\n}}", head, indent(&body)));
        }
        lines.join("\n")
    }

    /// Attributes before a field, variant or parameter: `@serde(skip) ` inline, doc comments on
    /// their own lines.
    pub fn field_attrs_inline(&self, attrs: &[syn::Attribute]) -> String {
        let mut s = String::new();
        for a in attrs {
            for l in self.attr(a) {
                s.push_str(&l);
                s.push(if l.starts_with("//") { '\n' } else { ' ' });
            }
        }
        s
    }

    /// Methods of the merged impls, `toString`, trait implementations.
    fn class_body(&mut self, decl: &syn::Item, members: &[Member], all: &[syn::Item]) -> String {
        let mut items: HashMap<usize, &syn::ItemImpl> = HashMap::new();
        let mut methods: HashSet<String> = HashSet::new();
        for m in members {
            let i = match m {
                Member::Inherent(i) | Member::Trait(i, _) | Member::ToString(i, _) | Member::Operator(i, _) => *i,
            };
            if let Some(syn::Item::Impl(imp)) = all.get(i) {
                items.insert(i, imp);
                if let Member::Inherent(_) | Member::Trait(..) = m {
                    for (j, ii) in imp.items.iter().enumerate() {
                        if let syn::ImplItem::Fn(f) = ii {
                            // one kept as Rust is called as `self.name()`: Oxie doesn't know it
                            let raw = matches!(m, Member::Inherent(_)) && self.top_level && self.fixes.raw_methods.contains_key(&(i, j));
                            if f.sig.receiver().is_some() && !raw {
                                methods.insert(f.sig.ident.to_string());
                            }
                        }
                    }
                }
            }
        }
        let mut parts: Vec<String> = Vec::new();
        for m in members {
            match m {
                Member::Inherent(i) | Member::Trait(i, _) => {
                    let Some(imp) = items.get(i) else { continue };
                    let trait_name = if let Member::Trait(_, t) = m { Some(t.clone()) } else { None };
                    // the methods of an inherent impl can be kept as Rust one by one
                    let separable = trait_name.is_none() && self.top_level;
                    let lo = line_of(imp);
                    let mut raw = Vec::new();
                    for (j, ii) in imp.items.iter().enumerate() {
                        let syn::ImplItem::Fn(f) = ii else { continue };
                        if trait_name.is_none() && f.sig.ident == "new" && self.is_plain_new(decl, f) {
                            // the primary constructor generates it; drop comments inside
                            let _ = self.take_comments(lo, end_line_of(f) + 1);
                            continue;
                        }
                        if let Some(reason) = self.fixes.raw_methods.get(&(*i, j)).filter(|_| separable) {
                            let reason = reason.clone();
                            raw.push((j, reason, self.take_comments(lo, end_line_of(f) + 1)));
                            continue;
                        }
                        let cs = self.take_comments(lo, line_of(f));
                        let saved = std::mem::take(&mut self.cx);
                        self.cx = self.method_cx(decl, &f.sig, &f.block, &methods);
                        let tkey = facts::type_key(&imp.self_ty);
                        self.cx.impl_key = Some((format!("{}{}", self.mod_prefix, tkey), trait_name.clone().unwrap_or_default()));
                        let kind = FunKind::Method { class: true, overrides: trait_name.is_some() };
                        let mut s = cs.join("\n");
                        if !s.is_empty() {
                            s.push('\n');
                        }
                        s.push_str(&self.fun(&f.attrs, &f.vis, &f.sig, Some(&f.block), kind));
                        if let Some(tc) = self.trailing_comment(end_line_of(f)) {
                            s.push(' ');
                            s.push_str(&tc);
                        }
                        self.cx = saved;
                        parts.push(if separable { mark_method(*i, j, &s) } else { s });
                    }
                    if !raw.is_empty() {
                        let block = self.raw_methods(imp, raw);
                        self.after_item.push(block);
                    }
                    let tail = self.take_comments(lo, end_line_of(imp));
                    if !tail.is_empty() {
                        parts.push(tail.join("\n"));
                    }
                }
                Member::Operator(i, op) => {
                    let Some(imp) = items.get(i) else { continue };
                    let ty_name = match decl {
                        syn::Item::Struct(st) => st.ident.to_string(),
                        syn::Item::Enum(en) => en.ident.to_string(),
                        _ => String::new(),
                    };
                    let output = imp.items.iter().find_map(|ii| match ii {
                        syn::ImplItem::Type(t) => Some(t.ty.clone()),
                        _ => None,
                    });
                    let Some(f) = imp.items.iter().find_map(|ii| match ii {
                        syn::ImplItem::Fn(f) => Some(f),
                        _ => None,
                    }) else {
                        continue;
                    };
                    let lo = line_of(imp);
                    let cs = self.take_comments(lo, line_of(f));
                    // `fn add(self, o: Self) -> Self::Output` → `fun plus(o: V): V`
                    let mut sig = f.sig.clone();
                    sig.ident = syn::Ident::new(op, sig.ident.span());
                    let mut inputs: syn::punctuated::Punctuated<syn::FnArg, syn::Token![,]> = syn::punctuated::Punctuated::new();
                    for a in sig.inputs.iter().skip(1) {
                        let mut a = a.clone();
                        if let syn::FnArg::Typed(pt) = &mut a {
                            if tok(&*pt.ty) == "Self" {
                                *pt.ty = syn::parse_str(&ty_name).unwrap_or((*pt.ty).clone());
                            }
                        }
                        inputs.push(a);
                    }
                    sig.inputs = inputs;
                    if let (Some(o), syn::ReturnType::Type(arrow, _)) = (&output, &sig.output) {
                        sig.output = syn::ReturnType::Type(*arrow, Box::new(o.clone()));
                    }
                    let saved = std::mem::take(&mut self.cx);
                    self.cx = self.method_cx(decl, &f.sig, &f.block, &methods);
                    let mut s = cs.join("\n");
                    if !s.is_empty() {
                        s.push('\n');
                    }
                    s.push_str(&self.fun(&[], &syn::Visibility::Inherited, &sig, Some(&f.block), FunKind::Operator));
                    self.cx = saved;
                    let _ = self.take_comments(lo, end_line_of(imp) + 1);
                    parts.push(s);
                }
                Member::ToString(i, body) => {
                    if let Some(imp) = items.get(i) {
                        let _ = self.take_comments(line_of(imp), end_line_of(imp) + 1);
                    }
                    parts.push(format!("override fun toString() = {}", body));
                }
            }
        }
        parts.join("\n\n")
    }

    /// `fn new(a: A, b: B) -> Self { Self { a, b } }` for fields `a, b` in this order.
    pub fn is_plain_new(&self, decl: &syn::Item, f: &syn::ImplItemFn) -> bool {
        let syn::Item::Struct(s) = decl else { return false };
        let syn::Fields::Named(fields) = &s.fields else { return false };
        if !f.attrs.iter().all(|a| a.path().is_ident("doc") || a.path().is_ident("allow") || a.path().is_ident("inline")) {
            return false;
        }
        if f.sig.asyncness.is_some() || f.sig.constness.is_some() || f.sig.unsafety.is_some() || !f.sig.generics.params.is_empty() {
            return false;
        }
        match &f.sig.output {
            syn::ReturnType::Type(_, t) => {
                let ts = tok(&**t);
                let mut name_with_args = s.ident.to_string();
                if !s.generics.params.is_empty() {
                    name_with_args = tok(&syn::Type::Path(syn::TypePath { qself: None, path: syn::Path::from(s.ident.clone()) }));
                }
                if ts != "Self" && ts != name_with_args && ts != tok(&self_type_of(s)) {
                    return false;
                }
            }
            _ => return false,
        }
        if f.sig.inputs.len() != fields.named.len() {
            return false;
        }
        for (arg, field) in f.sig.inputs.iter().zip(fields.named.iter()) {
            let syn::FnArg::Typed(pt) = arg else { return false };
            let syn::Pat::Ident(pi) = &*pt.pat else { return false };
            if pi.by_ref.is_some() || pi.mutability.is_some() || Some(&pi.ident) != field.ident.as_ref() || tok(&*pt.ty) != tok(&field.ty) {
                return false;
            }
        }
        if f.block.stmts.len() != 1 {
            return false;
        }
        let syn::Stmt::Expr(syn::Expr::Struct(st), None) = &f.block.stmts[0] else { return false };
        if st.rest.is_some() || st.qself.is_some() {
            return false;
        }
        let p = tok(&st.path);
        if p != "Self" && s.ident != p {
            return false;
        }
        if st.fields.len() != fields.named.len() {
            return false;
        }
        for fv in &st.fields {
            let syn::Member::Named(m) = &fv.member else { return false };
            match &fv.expr {
                syn::Expr::Path(ep) if ep.path.is_ident(m) => {}
                _ => return false,
            }
        }
        true
    }

    pub fn method_cx(&self, decl: &syn::Item, sig: &syn::Signature, block: &syn::Block, methods: &HashSet<String>) -> Cx {
        let mut cx = Cx::default();
        let mut class = ClassCx::default();
        if let syn::Item::Struct(s) = decl {
            if let syn::Fields::Named(n) = &s.fields {
                for f in &n.named {
                    class.fields.insert(f.ident.as_ref().unwrap().to_string());
                }
            }
        }
        class.methods = methods.clone();
        cx.class = Some(class);
        cx.bound = bound_names(sig, block);
        let name = match decl {
            syn::Item::Struct(s) => s.ident.to_string(),
            syn::Item::Enum(e) => e.ident.to_string(),
            _ => String::new(),
        };
        cx.type_fns = self.type_fns(&name);
        cx.ctx_class = self.krate.class(&name).map(|_| name);
        cx
    }

    /// The functions of type `name`, in this file and (for a class) the rest of the crate.
    fn type_fns(&self, name: &str) -> HashSet<String> {
        let mut fns = self.info.type_fns.get(name).cloned().unwrap_or_default();
        if let Some(c) = self.krate.classes.get(name) {
            fns.extend(c.methods.keys().cloned());
        }
        fns
    }

    /// `context(window: &mut Window, cx: &mut Context<Self>)`
    pub fn context_clause(&self, contexts: &[(String, syn::Type)]) -> String {
        let ps: Vec<String> = contexts.iter().map(|(n, t)| format!("{}: {}", n, self.ty(t))).collect();
        format!("context({})", ps.join(", "))
    }

    /// How a method of a class with contexts gets those it takes (see `contexts`): Oxie infers
    /// them (it uses them, or it is used as a value and takes them all), or it gets a
    /// `context(...)` of its own.
    fn ctx_decl(&self, sig: &syn::Signature, block: Option<&syn::Block>, kind: FunKind) -> CtxDecl {
        let mut d = CtxDecl::default();
        let (FunKind::Method { overrides: false, .. }, Some((ty, tr))) = (kind, &self.cx.impl_key) else { return d };
        let Some(class) = self.cx.ctx_class.as_deref().and_then(|c| self.krate.class(c)) else { return d };
        let Some(m) = class.methods.get(&sig.ident.to_string()).filter(|_| tr.is_empty()) else { return d };
        let Some(takes) = m.takes.as_ref().filter(|t| !t.is_empty()) else { return d };
        d.drop = takes.len();
        let key = (ty.clone(), String::new(), sig.ident.to_string());
        let all: Vec<String> = class.contexts.iter().map(|(n, _)| n.clone()).collect();
        let used = block.map(|b| contexts::uses(b, &all)).unwrap_or_default();
        let inferred: Vec<String> = all.into_iter().filter(|n| m.value || used.contains(n)).collect();
        if inferred != *takes || self.fixes.ctx_clause.contains(&key) {
            d.clause = Some(class.contexts.iter().filter(|(n, _)| takes.contains(n)).cloned().collect());
        }
        d
    }

    // ------------------------------------------------------------------ enums

    fn enum_item(&mut self, e: &syn::ItemEnum, members: &[Member], items: &[syn::Item]) -> String {
        let key = format!("{}{}", self.mod_prefix, e.ident);
        let mut lines = Vec::new();
        let explicit = self.fixes.explicit_derives.contains(&key);
        for a in &e.attrs {
            if a.path().is_ident("derive") && !explicit {
                continue;
            }
            lines.extend(self.attr(a));
        }
        if explicit {
            if !e.attrs.iter().any(|a| a.path().is_ident("derive")) {
                // keep the order: @no_auto_derive before the type
            }
            lines.insert(0, "@no_auto_derive".into());
        }
        let mut supers = Vec::new();
        for m in members {
            if let Member::Trait(_, t) = m {
                supers.push(t.clone());
            }
        }
        let mut head = format!("{}enum {}{}", self.vis(&e.vis), id(&e.ident), self.generics(&e.generics));
        if !supers.is_empty() {
            head.push_str(&format!(" : {}", supers.join(", ")));
        }
        head.push_str(&self.where_clause(&e.generics));
        let mut vs = Vec::new();
        let lo = line_of(e);
        for v in &e.variants {
            let cs = self.take_comments(lo, line_of(v));
            for c in cs {
                vs.push(c);
            }
            let mut s = self.field_attrs_inline(&v.attrs);
            s.push_str(&id(&v.ident));
            match &v.fields {
                syn::Fields::Unit => {}
                syn::Fields::Unnamed(u) => {
                    let fs: Vec<String> = u.unnamed.iter().map(|f| format!("{}{}", self.field_attrs_inline(&f.attrs), self.ty(&f.ty))).collect();
                    s.push_str(&format!("({})", fs.join(", ")));
                }
                syn::Fields::Named(n) => {
                    let fs: Vec<String> =
                        n.named.iter().map(|f| format!("{}{}: {}", self.field_attrs_inline(&f.attrs), id(f.ident.as_ref().unwrap()), self.ty(&f.ty))).collect();
                    s.push_str(&format!("({})", fs.join(", ")));
                }
            }
            if let Some((_, d)) = &v.discriminant {
                s.push_str(&format!(" = {}", self.rust_expr_inline(d)));
            }
            if let Some(tc) = self.trailing_comment(end_line_of(v)) {
                s.push(' ');
                s.push_str(&tc);
            }
            vs.push(s);
        }
        let short = vs.iter().all(|v| !v.contains("//") && !v.contains('(') && !v.contains('@') && !v.contains('\n')) && vs.join(", ").len() < 60 && members.is_empty();
        let mut body = if short { vs.join(", ") } else { vs.join("\n") };
        let methods = self.class_body(&syn::Item::Enum(e.clone()), members, items);
        if !methods.trim().is_empty() {
            body.push_str("\n\n");
            body.push_str(&methods);
        }
        let tail = self.take_comments(lo, end_line_of(e));
        if !tail.is_empty() {
            body.push('\n');
            body.push_str(&tail.join("\n"));
        }
        if short && !body.contains('\n') {
            lines.push(format!("{} {{ {} }}", head, body));
        } else {
            lines.push(format!("{} {{\n{}\n}}", head, indent(&body)));
        }
        lines.join("\n")
    }

    // ------------------------------------------------------------------ impls and traits

    /// `idx`: the impl's position among the items of its list.
    fn impl_item(&mut self, imp: &syn::ItemImpl, idx: usize) -> String {
        let mut lines = self.attrs(&imp.attrs, false);
        // the methods of an inherent impl can be kept as Rust one by one
        let separable = imp.trait_.is_none() && self.top_level;
        let mut raw = Vec::new();
        let mut head = String::new();
        if imp.unsafety.is_some() {
            head.push_str("unsafe ");
        }
        head.push_str(&format!("impl{} ", self.generics(&imp.generics)));
        let tkey = format!("{}{}", self.mod_prefix, facts::type_key(&imp.self_ty));
        let mut trait_key = String::new();
        if let Some((neg, path, _)) = &imp.trait_ {
            if neg.is_some() {
                head.push('!');
            }
            head.push_str(&self.type_path(None, path));
            head.push_str(" for ");
            trait_key = facts::trait_key(path);
        }
        head.push_str(&self.ty(&imp.self_ty));
        head.push_str(&self.where_clause(&imp.generics));
        let lo = line_of(imp);
        let mut parts = Vec::new();
        for (j, ii) in imp.items.iter().enumerate() {
            if let (syn::ImplItem::Fn(f), Some(reason)) = (ii, self.fixes.raw_methods.get(&(idx, j)).filter(|_| separable)) {
                let reason = reason.clone();
                raw.push((j, reason, self.take_comments(lo, end_line_of(f) + 1)));
                continue;
            }
            let cs = self.take_comments(lo, line_of(ii));
            let mut s = cs.join("\n");
            if !s.is_empty() {
                s.push('\n');
            }
            let mut method = false;
            match ii {
                syn::ImplItem::Fn(f) => {
                    let saved = std::mem::take(&mut self.cx);
                    self.cx.bound = bound_names(&f.sig, &f.block);
                    self.cx.impl_key = Some((tkey.clone(), trait_key.clone()));
                    self.cx.type_fns = self.type_fns(&facts::type_name(&imp.self_ty));
                    self.cx.ctx_class = Some(facts::type_name(&imp.self_ty)).filter(|n| self.krate.class(n).is_some());
                    s.push_str(&self.fun(&f.attrs, &f.vis, &f.sig, Some(&f.block), FunKind::Method { class: false, overrides: false }));
                    self.cx = saved;
                    method = true;
                }
                syn::ImplItem::Type(t) => {
                    let mut l = self.attrs(&t.attrs, false);
                    l.push(format!("type {}{} = {}{}", id(&t.ident), self.generics(&t.generics), self.ty(&t.ty), self.where_clause(&t.generics)));
                    s.push_str(&l.join("\n"));
                }
                syn::ImplItem::Const(c) => {
                    let mut l = self.attrs(&c.attrs, false);
                    let saved = std::mem::take(&mut self.cx);
                    let v = self.expr_expect(&c.expr, Some(&c.ty));
                    self.cx = saved;
                    l.push(format!("{}const {}: {} = {}", self.vis(&c.vis), id(&c.ident), self.ty(&c.ty), v));
                    s.push_str(&l.join("\n"));
                }
                other => {
                    self.todo(line_of(other), "impl item kept as Rust");
                    s.push_str(&format!("rust {{ {} }}", tok(other)));
                }
            }
            if let Some(tc) = self.trailing_comment(end_line_of(ii)) {
                s.push(' ');
                s.push_str(&tc);
            }
            parts.push(if method && separable { mark_method(idx, j, &s) } else { s });
        }
        let all_raw = parts.is_empty() && !raw.is_empty();
        if !raw.is_empty() {
            let block = self.raw_methods(imp, raw);
            self.after_item.push(block);
        }
        let tail = self.take_comments(lo, end_line_of(imp));
        if !tail.is_empty() {
            parts.push(tail.join("\n"));
        }
        if all_raw && parts.is_empty() {
            // nothing left in Oxie: the Rust block is the whole impl
            return String::new();
        }
        if parts.is_empty() {
            lines.push(format!("{} {{}}", head));
        } else {
            lines.push(format!("{} {{\n{}\n}}", head, indent(&parts.join("\n\n"))));
        }
        lines.join("\n")
    }

    fn trait_item(&mut self, t: &syn::ItemTrait) -> String {
        let mut lines = self.attrs(&t.attrs, false);
        let mut head =
            format!("{}{}interface {}{}", self.vis(&t.vis), if t.unsafety.is_some() { "unsafe " } else { "" }, id(&t.ident), self.generics(&t.generics));
        if !t.supertraits.is_empty() {
            head.push_str(&format!(" : {}", self.bounds(&t.supertraits)));
        }
        head.push_str(&self.where_clause(&t.generics));
        let tkey = format!("{}trait {}", self.mod_prefix, t.ident);
        let lo = line_of(t);
        let mut parts = Vec::new();
        for ti in &t.items {
            let cs = self.take_comments(lo, line_of(ti));
            let mut s = cs.join("\n");
            if !s.is_empty() {
                s.push('\n');
            }
            match ti {
                syn::TraitItem::Fn(f) => {
                    let saved = std::mem::take(&mut self.cx);
                    if let Some(b) = &f.default {
                        self.cx.bound = bound_names(&f.sig, b);
                    }
                    self.cx.impl_key = Some((tkey.clone(), String::new()));
                    self.cx.in_trait = true;
                    self.cx.type_fns = self.info.type_fns.get(&format!("trait {}", t.ident)).cloned().unwrap_or_default();
                    s.push_str(&self.fun(&f.attrs, &syn::Visibility::Inherited, &f.sig, f.default.as_ref(), FunKind::TraitDecl));
                    self.cx = saved;
                }
                syn::TraitItem::Type(ty) => {
                    let mut l = self.attrs(&ty.attrs, false);
                    let mut x = format!("type {}{}", id(&ty.ident), self.generics(&ty.generics));
                    if !ty.bounds.is_empty() {
                        x.push_str(&format!(": {}", self.bounds(&ty.bounds)));
                    }
                    x.push_str(&self.where_clause(&ty.generics));
                    if let Some((_, d)) = &ty.default {
                        x.push_str(&format!(" = {}", self.ty(d)));
                    }
                    l.push(x);
                    s.push_str(&l.join("\n"));
                }
                syn::TraitItem::Const(c) => {
                    let mut l = self.attrs(&c.attrs, false);
                    let mut x = format!("const {}: {}", id(&c.ident), self.ty(&c.ty));
                    if let Some((_, d)) = &c.default {
                        let saved = std::mem::take(&mut self.cx);
                        x.push_str(&format!(" = {}", self.expr_expect(d, Some(&c.ty))));
                        self.cx = saved;
                    }
                    l.push(x);
                    s.push_str(&l.join("\n"));
                }
                other => s.push_str(&format!("rust {{ {} }}", tok(other))),
            }
            if let Some(tc) = self.trailing_comment(end_line_of(ti)) {
                s.push(' ');
                s.push_str(&tc);
            }
            parts.push(s);
        }
        let tail = self.take_comments(lo, end_line_of(t));
        if !tail.is_empty() {
            parts.push(tail.join("\n"));
        }
        let all_short = parts.iter().all(|p| !p.contains('\n'));
        if parts.is_empty() {
            lines.push(format!("{} {{}}", head));
        } else {
            lines.push(format!("{} {{\n{}\n}}", head, indent(&parts.join(if all_short { "\n" } else { "\n\n" }))));
        }
        lines.join("\n")
    }

    // ------------------------------------------------------------------ functions

    pub fn fun(&mut self, attrs: &[syn::Attribute], vis: &syn::Visibility, sig: &syn::Signature, block: Option<&syn::Block>, kind: FunKind) -> String {
        let mut lines = self.attrs(attrs, false);
        let recv = facts::recv_of(sig);
        let key: Option<MethodKey> = self.cx.impl_key.as_ref().map(|(t, tr)| (t.clone(), tr.clone(), sig.ident.to_string()));
        let explicit = key.as_ref().is_some_and(|k| self.fixes.explicit_recv.contains(k));
        let is_method = !matches!(kind, FunKind::Free | FunKind::Operator);
        let ctx = self.ctx_decl(sig, block, kind);
        if let Some(c) = &ctx.clause {
            lines.push(self.context_clause(c));
        }
        let mut prefix = self.vis(vis);
        if let FunKind::Method { overrides: true, .. } = kind {
            prefix = "override ".into();
        }
        if kind == FunKind::Operator {
            prefix = "operator ".into();
        }
        if sig.constness.is_some() {
            prefix.push_str("const ");
        }
        if sig.asyncness.is_some() {
            prefix.push_str("async ");
        }
        if sig.unsafety.is_some() {
            prefix.push_str("unsafe ");
        }
        if let Some(abi) = &sig.abi {
            prefix.push_str(&tok(abi));
            prefix.push(' ');
        }
        // receiver
        let mut params: Vec<String> = Vec::new();
        let mut write_recv = false;
        if is_method {
            match recv {
                Recv::None => {
                    let predicted_static = match kind {
                        FunKind::TraitDecl => false,
                        _ => AUTO_STATIC.contains(&sig.ident.to_string().as_str()) || returns_self(sig, self.cx.impl_key.as_ref().map(|k| k.0.as_str())),
                    };
                    if explicit || !predicted_static {
                        prefix.push_str("static ");
                    }
                }
                Recv::Ref | Recv::RefMut => {
                    let has_lifetime = matches!(sig.inputs.first(), Some(syn::FnArg::Receiver(r)) if r.reference.as_ref().is_some_and(|(_, l)| l.is_some()));
                    // a trait method declaration has no body to infer `&mut self` from
                    if explicit || has_lifetime || (recv == Recv::RefMut && matches!(kind, FunKind::TraitDecl)) {
                        write_recv = true;
                    }
                }
                Recv::Value | Recv::Typed => write_recv = true,
            }
        }
        for (i, a) in sig.inputs.iter().enumerate().take(sig.inputs.len() - ctx.drop) {
            match a {
                syn::FnArg::Receiver(r) => {
                    if write_recv && i == 0 {
                        params.push(receiver_text(r, self));
                    }
                }
                syn::FnArg::Typed(pt) => {
                    params.push(format!("{}{}: {}", self.field_attrs_inline(&pt.attrs), self.pat(&pt.pat), self.ty(&pt.ty)));
                }
            }
        }
        if let Some(v) = &sig.variadic {
            params.push(tok(v));
        }
        // return type
        let (ret_text, throws, ret_ty) = self.ret_type(&sig.output);
        let mut head = format!("{}fun {}{}({})", prefix, id(&sig.ident), self.generics(&sig.generics), params.join(", "));
        head.push_str(&ret_text);
        head.push_str(&self.where_clause(&sig.generics));
        let Some(block) = block else {
            lines.push(head);
            return lines.join("\n");
        };
        self.cx.bound.extend(bound_names(sig, block));
        self.cx.fn_path = match &self.cx.impl_key {
            Some((t, _)) => format!("{}::{}()::", t, sig.ident),
            None => format!("{}{}()::", self.mod_prefix, sig.ident),
        };
        self.cx.throws = throws.is_some();
        self.cx.err_from_str = matches!(throws.as_deref(), Some("") | Some(" String"));
        self.cx.ret = ret_ty.clone();
        self.cx.result_ty = match &sig.output {
            syn::ReturnType::Type(_, t) if throws.is_some() => Some((**t).clone()),
            _ => None,
        };
        let unit = ret_ty.is_none();
        let body_lo = line_of(block);
        let body_hi = end_line_of(block);
        // expression body
        if !unit && block.stmts.len() == 1 && !self.has_comments_in(body_lo, body_hi) {
            if let syn::Stmt::Expr(e, None) = &block.stmts[0] {
                if !matches!(e, syn::Expr::Loop(_) | syn::Expr::ForLoop(_) | syn::Expr::While(_) | syn::Expr::Block(_) | syn::Expr::Unsafe(_)) {
                    // a trial print: no comments are involved, and its notes are dropped if unused
                    let marks = (self.todos.len(), self.pending.len());
                    let v = if self.cx.throws { self.tail_throws(e) } else { self.tail_value(e, ret_ty.as_ref()) };
                    let first_line = v.lines().next().unwrap_or("");
                    let ok = if matches!(e, syn::Expr::Match(_)) { v.starts_with("when") } else { !v.contains('\n') };
                    if ok && head.len() + first_line.len() + 3 <= 100 && !v.is_empty() {
                        lines.push(format!("{} = {}", head, v));
                        return lines.join("\n");
                    }
                    self.todos.truncate(marks.0);
                    self.pending.truncate(marks.1);
                }
            }
        }
        let body = self.block_body(block, if unit { BlockMode::UnitFn } else { BlockMode::ValueFn });
        if body.is_empty() {
            lines.push(format!("{} {{}}", head));
        } else {
            lines.push(format!("{} {{\n{}\n}}", head, indent(&body)));
        }
        lines.join("\n")
    }

    /// `: T`, `: T throws E`; also returns the error type (if `throws`) and the value type.
    fn ret_type(&self, out: &syn::ReturnType) -> (String, Option<String>, Option<syn::Type>) {
        let syn::ReturnType::Type(_, t) = out else { return (String::new(), None, None) };
        if let Some((ok, err)) = result_parts(t) {
            let (ok, err) = (&ok, &err);
            let e = if is_any_error(err) || matches!(err, syn::Type::Path(p) if p.path.is_ident("AnyError")) || tok(err) == "crate::__oxie::AnyError" {
                String::new()
            } else {
                format!(" {}", self.ty(err))
            };
            let ok_text = if is_unit(ok) { String::new() } else { format!(": {}", self.ty(ok)) };
            let ret = if is_unit(ok) { None } else { Some(ok.clone()) };
            return (format!("{} throws{}", ok_text, e), Some(e), ret);
        }
        if is_unit(t) {
            return (String::new(), None, None);
        }
        (format!(": {}", self.ty(t)), None, Some((**t).clone()))
    }
}

fn receiver_text(r: &syn::Receiver, p: &Printer) -> String {
    if r.colon_token.is_some() {
        let m = if r.mutability.is_some() { "mut " } else { "" };
        return format!("{}self: {}", m, p.ty(&r.ty));
    }
    let mut s = String::new();
    if let Some((_, l)) = &r.reference {
        s.push('&');
        if let Some(l) = l {
            s.push_str(&l.to_string());
            s.push(' ');
        }
    }
    if r.mutability.is_some() {
        s.push_str("mut ");
    }
    s.push_str("self");
    s
}

fn returns_self(sig: &syn::Signature, ty_key: Option<&str>) -> bool {
    let syn::ReturnType::Type(_, t) = &sig.output else { return false };
    let s = tok(&**t);
    if s == "Self" {
        return true;
    }
    if let Some(k) = ty_key {
        let k = k.rsplit("::").next().unwrap_or(k);
        let base = k.split('<').next().unwrap_or(k);
        return s == k || s == base || s.starts_with(&format!("{}<", base));
    }
    false
}

/// Whether a function returns `Result<T, E>` (and becomes `throws`).
pub fn result_parts_of(out: &syn::ReturnType) -> bool {
    matches!(out, syn::ReturnType::Type(_, t) if result_parts(t).is_some())
}

pub fn is_unit(t: &syn::Type) -> bool {
    matches!(t, syn::Type::Tuple(tt) if tt.elems.is_empty())
}

/// Attributes that are all doc comments.
pub fn only_docs(attrs: &[syn::Attribute]) -> bool {
    attrs.iter().all(|a| a.path().is_ident("doc"))
}

/// `Result<T, E>` → (T, E); also `io::Result<T>` → (T, io::Error)
pub fn result_parts(t: &syn::Type) -> Option<(syn::Type, syn::Type)> {
    let syn::Type::Path(p) = t else { return None };
    if p.qself.is_some() {
        return None;
    }
    let names: Vec<String> = p.path.segments.iter().map(|s| s.ident.to_string()).collect();
    let io_alias = matches!(names.iter().map(|s| s.as_str()).collect::<Vec<_>>().as_slice(), ["io", "Result"] | ["std", "io", "Result"]);
    if io_alias {
        let syn::PathArguments::AngleBracketed(a) = &p.path.segments.last()?.arguments else { return None };
        if a.args.len() != 1 {
            return None;
        }
        let syn::GenericArgument::Type(ok) = &a.args[0] else { return None };
        let err: syn::Type = syn::parse_str(if names.len() == 3 { "std::io::Error" } else { "io::Error" }).ok()?;
        return Some((ok.clone(), err));
    }
    let ok = match names.len() {
        1 => names[0] == "Result",
        3 => (names[0] == "std" || names[0] == "core") && names[1] == "result" && names[2] == "Result",
        _ => false,
    };
    if !ok {
        return None;
    }
    let syn::PathArguments::AngleBracketed(a) = &p.path.segments.last()?.arguments else { return None };
    if a.args.len() != 2 {
        return None;
    }
    match (&a.args[0], &a.args[1]) {
        (syn::GenericArgument::Type(a), syn::GenericArgument::Type(b)) => Some((a.clone(), b.clone())),
        _ => None,
    }
}

fn self_type_of(s: &syn::ItemStruct) -> syn::Type {
    let args: Vec<String> = s
        .generics
        .params
        .iter()
        .map(|p| match p {
            syn::GenericParam::Lifetime(l) => l.lifetime.to_string(),
            syn::GenericParam::Type(t) => t.ident.to_string(),
            syn::GenericParam::Const(c) => c.ident.to_string(),
        })
        .collect();
    let text = if args.is_empty() { s.ident.to_string() } else { format!("{}<{}>", s.ident, args.join(", ")) };
    syn::parse_str(&text).unwrap_or_else(|_| syn::parse_str("Self").unwrap())
}

pub fn derive_list(attrs: &[syn::Attribute]) -> Vec<String> {
    let mut out = Vec::new();
    for a in attrs {
        if a.path().is_ident("derive") {
            let _ = a.parse_nested_meta(|m| {
                out.push(tok(&m.path));
                Ok(())
            });
        }
    }
    out
}

fn use_tree(t: &syn::UseTree) -> String {
    match t {
        syn::UseTree::Path(p) => format!("{}::{}", p.ident, use_tree(&p.tree)),
        syn::UseTree::Name(n) => n.ident.to_string(),
        syn::UseTree::Rename(r) => format!("{} as {}", r.ident, r.rename),
        syn::UseTree::Glob(_) => "*".into(),
        syn::UseTree::Group(g) => {
            let v: Vec<String> = g.items.iter().map(use_tree).collect();
            format!("{{{}}}", v.join(", "))
        }
    }
}

fn item_start_line(it: &syn::Item) -> usize {
    // attributes (including doc comments) come first
    let attrs: &[syn::Attribute] = match it {
        syn::Item::Fn(x) => &x.attrs,
        syn::Item::Struct(x) => &x.attrs,
        syn::Item::Enum(x) => &x.attrs,
        syn::Item::Impl(x) => &x.attrs,
        syn::Item::Trait(x) => &x.attrs,
        syn::Item::Use(x) => &x.attrs,
        syn::Item::Const(x) => &x.attrs,
        syn::Item::Static(x) => &x.attrs,
        syn::Item::Mod(x) => &x.attrs,
        syn::Item::Type(x) => &x.attrs,
        syn::Item::Macro(x) => &x.attrs,
        _ => &[],
    };
    attrs.first().map(line_of).unwrap_or_else(|| line_of(it)).min(line_of(it))
}

fn prev_end_adjacent(items: &[syn::Item], i: usize) -> bool {
    i > 0 && item_start_line(&items[i]) <= end_line_of(&items[i - 1]) + 1
}

/// Whether `e` mentions the identifier `name`.
pub fn mentions(e: &syn::Expr, name: &str) -> bool {
    e.to_token_stream().into_iter().any(|t| tt_mentions(&t, name))
}

fn tt_mentions(t: &proc_macro2::TokenTree, name: &str) -> bool {
    match t {
        proc_macro2::TokenTree::Ident(i) => i == name,
        proc_macro2::TokenTree::Group(g) => g.stream().into_iter().any(|t| tt_mentions(&t, name)),
        _ => false,
    }
}

/// Names bound by parameters, `let`s, patterns and closures anywhere in a function.
pub fn bound_names(sig: &syn::Signature, block: &syn::Block) -> HashSet<String> {
    use syn::visit::Visit;
    struct V(HashSet<String>);
    impl<'ast> Visit<'ast> for V {
        fn visit_pat_ident(&mut self, i: &'ast syn::PatIdent) {
            self.0.insert(i.ident.to_string());
            syn::visit::visit_pat_ident(self, i);
        }
        // a local `fn` binds its name, but its body isn't part of this function
        fn visit_item(&mut self, i: &'ast syn::Item) {
            if let syn::Item::Fn(f) = i {
                self.0.insert(f.sig.ident.to_string());
            }
        }
    }
    let mut v = V(HashSet::new());
    for a in &sig.inputs {
        if let syn::FnArg::Typed(pt) = a {
            v.visit_pat(&pt.pat);
        }
    }
    v.visit_block(block);
    // macro bodies may bind names too (e.g. inside `vec![...]` closures); be conservative
    v.0
}

/// A Rust item formatted by prettyplease.
pub fn rust_item(it: &syn::Item) -> String {
    let f = syn::File { shebang: None, attrs: Vec::new(), items: vec![it.clone()] };
    prettyplease::unparse(&f).trim_end().to_string()
}

#[derive(Default)]
pub struct Plan {
    pub merged: HashSet<usize>,
    pub dropped: HashSet<usize>,
    pub members: HashMap<usize, Vec<Member>>,
}

#[derive(Clone, Debug)]
pub enum Member {
    Inherent(usize),
    Trait(usize, String),
    ToString(usize, String),
    Operator(usize, String),
}

/// See [`Printer::ctx_decl`].
#[derive(Default)]
struct CtxDecl {
    /// How many trailing parameters become contexts.
    drop: usize,
    /// A `context(...)` of its own.
    clause: Option<Vec<(String, syn::Type)>>,
}

#[derive(Clone, Copy, PartialEq)]
pub enum FunKind {
    Free,
    Operator,
    Method { class: bool, overrides: bool },
    TraitDecl,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum BlockMode {
    /// body of a function returning `()`: the value of the last expression is discarded
    UnitFn,
    /// body of a function with a value
    ValueFn,
    /// a block whose value is used (closures, `run { }`, `let x = if ...`)
    Value,
}
