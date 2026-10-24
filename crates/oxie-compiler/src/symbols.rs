//! Symbol index and Oxie-syntax pretty printing, used by the language server.

use crate::ast::*;
use crate::diag::Span;
use crate::visit;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SymKind {
    Function,
    Method,
    Class,
    Struct,
    Enum,
    Interface,
    Variant,
    Field,
    Const,
    TypeAlias,
    Module,
    Test,
    Variable,
    Parameter,
}

#[derive(Clone, Debug)]
pub struct Symbol {
    pub name: String,
    pub kind: SymKind,
    /// Signature in Oxie syntax.
    pub detail: String,
    pub docs: String,
    /// Span of the name.
    pub span: Span,
    /// Span of the whole declaration.
    pub full: Span,
    pub container: Option<String>,
    pub children: Vec<Symbol>,
}

// ---------------------------------------------------------------------------------------------
// pretty printing in Oxie syntax

pub fn type_text(t: &Type) -> String {
    match t {
        Type::Path { global, segs, .. } => {
            let mut s = if *global { "::".to_string() } else { String::new() };
            let parts: Vec<String> = segs
                .iter()
                .map(|seg| {
                    let mut p = seg.name.clone();
                    if let Some(g) = &seg.generics {
                        p.push('<');
                        p.push_str(&g.iter().map(generic_arg_text).collect::<Vec<_>>().join(", "));
                        p.push('>');
                    }
                    if let Some((ps, r)) = &seg.fn_sugar {
                        p.push('(');
                        p.push_str(&ps.iter().map(type_text).collect::<Vec<_>>().join(", "));
                        p.push(')');
                        if let Some(r) = r {
                            p.push_str(" -> ");
                            p.push_str(&type_text(r));
                        }
                    }
                    p
                })
                .collect();
            s.push_str(&parts.join("::"));
            s
        }
        Type::Qualified { ty, trait_, rest } => {
            let mut s = format!("<{}", type_text(ty));
            if let Some(t) = trait_ {
                s.push_str(" as ");
                s.push_str(&type_text(t));
            }
            s.push('>');
            for r in rest {
                s.push_str("::");
                s.push_str(&r.name);
            }
            s
        }
        Type::Ref { lifetime, mutable, inner } => {
            let mut s = String::from("&");
            if let Some(l) = lifetime {
                s.push_str(&format!("'{} ", l));
            }
            if *mutable {
                s.push_str("mut ");
            }
            s.push_str(&type_text(inner));
            s
        }
        Type::Ptr { mutable, inner } => format!("*{} {}", if *mutable { "mut" } else { "const" }, type_text(inner)),
        Type::Tuple(v) => format!("({})", v.iter().map(type_text).collect::<Vec<_>>().join(", ")),
        Type::Array(inner, n) => format!("[{}; {}]", type_text(inner), n),
        Type::Slice(inner) => format!("[{}]", type_text(inner)),
        Type::Option(inner) => format!("{}?", type_text(inner)),
        Type::Fn { params, ret } => format!("({}) -> {}", params.iter().map(type_text).collect::<Vec<_>>().join(", "), type_text(ret)),
        Type::Dyn(b) => format!("dyn {}", b.iter().map(type_text).collect::<Vec<_>>().join(" + ")),
        Type::Impl(b) => format!("impl {}", b.iter().map(type_text).collect::<Vec<_>>().join(" + ")),
        Type::Bounds(b) => b.iter().map(type_text).collect::<Vec<_>>().join(" + "),
        Type::Lifetime(l) => format!("'{}", l),
        Type::Maybe(t) => format!("?{}", type_text(t)),
        Type::Never => "!".into(),
        Type::Infer => "_".into(),
        Type::Raw(s) => s.clone(),
    }
}

fn generic_arg_text(a: &GenericArg) -> String {
    match a {
        GenericArg::Type(t) => type_text(t),
        GenericArg::Lifetime(l) => format!("'{}", l),
        GenericArg::Binding(n, t) => format!("{} = {}", n, type_text(t)),
        GenericArg::Const(c) => c.clone(),
    }
}

pub fn generics_text(g: &Generics) -> String {
    if g.params.is_empty() {
        return String::new();
    }
    let parts: Vec<String> = g
        .params
        .iter()
        .map(|p| match p {
            GenericParam::Lifetime { name, .. } => format!("'{}", name),
            GenericParam::Type { name, bounds, .. } => {
                if bounds.is_empty() {
                    name.clone()
                } else {
                    format!("{}: {}", name, bounds.iter().map(type_text).collect::<Vec<_>>().join(" + "))
                }
            }
            GenericParam::Const { name, ty, .. } => format!("const {}: {}", name, type_text(ty)),
        })
        .collect();
    format!("<{}>", parts.join(", "))
}

fn pat_text(p: &Pat) -> String {
    match p {
        Pat::Ident { name, mutable, .. } => {
            if *mutable {
                format!("mut {}", name)
            } else {
                name.clone()
            }
        }
        Pat::Wild => "_".into(),
        Pat::Tuple(v) => format!("({})", v.iter().map(pat_text).collect::<Vec<_>>().join(", ")),
        _ => "_".into(),
    }
}

pub fn param_text(p: &Param) -> String {
    let mut s = format!("{}: {}", pat_text(&p.pat), type_text(&p.ty));
    if p.default.is_some() {
        s.push_str(" = …");
    }
    s
}

/// `context(cx: &mut App) ` before a declaration that has contexts.
fn contexts_text(ps: &[Param]) -> String {
    if ps.is_empty() {
        return String::new();
    }
    format!("context({}) ", ps.iter().map(param_text).collect::<Vec<_>>().join(", "))
}

pub fn fun_signature(f: &FunDecl) -> String {
    let mut s = contexts_text(&f.contexts);
    if f.mods.is_static {
        s.push_str("static ");
    }
    if f.mods.is_override {
        s.push_str("override ");
    }
    if f.mods.is_operator {
        s.push_str("operator ");
    }
    if f.mods.is_mut {
        s.push_str("mut ");
    }
    if f.mods.is_async {
        s.push_str("async ");
    }
    s.push_str("fun ");
    if let Some(r) = &f.receiver {
        s.push_str(&generics_text(&f.generics));
        if !f.generics.is_empty() {
            s.push(' ');
        }
        s.push_str(&type_text(r));
        s.push('.');
        s.push_str(&f.name);
    } else {
        s.push_str(&f.name);
        s.push_str(&generics_text(&f.generics));
    }
    s.push('(');
    let mut params: Vec<String> = Vec::new();
    match &f.self_param {
        Some(SelfParam::Ref) => params.push("&self".into()),
        Some(SelfParam::RefMut) => params.push("&mut self".into()),
        Some(SelfParam::Value { mutable }) => params.push(if *mutable { "mut self".into() } else { "self".into() }),
        Some(SelfParam::Typed(t)) => params.push(format!("self: {}", type_text(t))),
        None => {}
    }
    params.extend(f.params.iter().map(param_text));
    s.push_str(&params.join(", "));
    s.push(')');
    if let Some(r) = &f.ret {
        s.push_str(": ");
        s.push_str(&type_text(r));
    }
    match &f.throws {
        Throws::No => {}
        Throws::Any => s.push_str(" throws"),
        Throws::Ty(t) => {
            s.push_str(" throws ");
            s.push_str(&type_text(t));
        }
    }
    s
}

fn field_text(f: &Field) -> String {
    let ty = if f.ty == Type::Infer { String::new() } else { format!(": {}", type_text(&f.ty)) };
    format!("{}{}{}", f.name, ty, if f.default.is_some() { " = …" } else { "" })
}

fn type_decl_signature(t: &TypeDecl) -> String {
    let kw = match (&t.kind, t.is_error) {
        (TypeKind::Enum, true) => "error enum",
        (TypeKind::Enum, false) => "enum",
        (_, true) => "error class",
        (TypeKind::Data, _) => "data class",
        (TypeKind::TupleStruct, _) => "struct",
        (TypeKind::Class, _) if t.shared => "shared class",
        (TypeKind::Class, _) => "class",
        (TypeKind::Union, _) => "union",
    };
    let mut s = format!("{}{} {}{}", contexts_text(&t.contexts), kw, t.name, generics_text(&t.generics));
    let ctor: Vec<String> = t.fields.iter().filter(|f| f.in_ctor).map(field_text).collect();
    if !ctor.is_empty() {
        s.push_str(&format!("({})", ctor.join(", ")));
    } else if !t.tuple_fields.is_empty() {
        s.push_str(&format!("({})", t.tuple_fields.iter().map(|(_, t)| type_text(t)).collect::<Vec<_>>().join(", ")));
    }
    if !t.supertypes.is_empty() {
        s.push_str(" : ");
        s.push_str(&t.supertypes.iter().map(type_text).collect::<Vec<_>>().join(", "));
    }
    s
}

fn variant_signature(enum_name: &str, v: &Variant) -> String {
    let mut s = format!("{}.{}", enum_name, v.name);
    match &v.fields {
        VariantFields::Unit => {}
        VariantFields::Tuple(ts) => s.push_str(&format!("({})", ts.iter().map(type_text).collect::<Vec<_>>().join(", "))),
        VariantFields::Named(fs) => s.push_str(&format!("({})", fs.iter().map(field_text).collect::<Vec<_>>().join(", "))),
    }
    s
}

// ---------------------------------------------------------------------------------------------
// symbol index

/// Locates `name` inside `span` of `src` (for declarations that only store a whole-item span).
fn find_name(src: &str, span: Span, name: &str) -> Span {
    let start = span.start as usize;
    let end = (span.end as usize).min(src.len());
    if start <= end {
        let hay = &src[start..end];
        let mut from = 0;
        while let Some(i) = hay[from..].find(name) {
            let s = from + i;
            let e = s + name.len();
            let ok_before = s == 0 || !hay.as_bytes()[s - 1].is_ascii_alphanumeric() && hay.as_bytes()[s - 1] != b'_';
            let ok_after = e >= hay.len() || !hay.as_bytes()[e].is_ascii_alphanumeric() && hay.as_bytes()[e] != b'_';
            if ok_before && ok_after {
                return Span::new(start + s, start + e);
            }
            from = e;
        }
    }
    Span::new(start, start)
}

fn docs_text(d: &[String]) -> String {
    d.iter().map(|l| l.strip_prefix(' ').unwrap_or(l)).collect::<Vec<_>>().join("\n")
}

pub fn file_symbols(file: &File, src: &str) -> Vec<Symbol> {
    file.items.iter().filter_map(|it| item_symbol(it, src, None)).collect()
}

fn item_symbol(it: &Item, src: &str, container: Option<&str>) -> Option<Symbol> {
    let docs = docs_text(&it.docs);
    let sym = |name: &str, kind: SymKind, detail: String, span: Span, children: Vec<Symbol>| Symbol {
        name: name.to_string(),
        kind,
        detail,
        docs: docs.clone(),
        span,
        full: it.span,
        container: container.map(|s| s.to_string()),
        children,
    };
    Some(match &it.kind {
        ItemKind::Fun(f) => {
            let kind = if container.is_some() { SymKind::Method } else { SymKind::Function };
            sym(&f.name, kind, fun_signature(f), f.name_span, Vec::new())
        }
        ItemKind::Type(t) => {
            let mut children = Vec::new();
            for f in &t.fields {
                children.push(Symbol {
                    name: f.name.clone(),
                    kind: SymKind::Field,
                    detail: format!("{}{}", if f.in_ctor { "" } else { "var " }, field_text(f)),
                    docs: docs_text(&f.docs),
                    span: find_name(src, f.span, &f.name),
                    full: f.span,
                    container: Some(t.name.clone()),
                    children: Vec::new(),
                });
            }
            for v in &t.variants {
                children.push(Symbol {
                    name: v.name.clone(),
                    kind: SymKind::Variant,
                    detail: variant_signature(&t.name, v),
                    docs: docs_text(&v.docs),
                    span: find_name(src, v.span, &v.name),
                    full: v.span,
                    container: Some(t.name.clone()),
                    children: Vec::new(),
                });
            }
            for m in &t.members {
                if let Some(s) = item_symbol(m, src, Some(&t.name)) {
                    children.push(s);
                }
            }
            let kind = match t.kind {
                TypeKind::Enum => SymKind::Enum,
                TypeKind::TupleStruct => SymKind::Struct,
                _ => SymKind::Class,
            };
            sym(&t.name, kind, type_decl_signature(t), t.name_span, children)
        }
        ItemKind::Trait(t) => {
            let children = t.members.iter().filter_map(|m| item_symbol(m, src, Some(&t.name))).collect();
            let mut detail = format!("interface {}{}", t.name, generics_text(&t.generics));
            if !t.supertraits.is_empty() {
                detail.push_str(" : ");
                detail.push_str(&t.supertraits.iter().map(type_text).collect::<Vec<_>>().join(" + "));
            }
            sym(&t.name, SymKind::Interface, detail, t.name_span, children)
        }
        ItemKind::Impl(imp) => {
            let owner = imp.self_ty.last_name().unwrap_or("impl").to_string();
            let children = imp.members.iter().filter_map(|m| item_symbol(m, src, Some(&owner))).collect();
            let name = match &imp.trait_ {
                Some(t) => format!("impl {} for {}", type_text(t), type_text(&imp.self_ty)),
                None => format!("impl {}", type_text(&imp.self_ty)),
            };
            let span = find_name(src, it.span, "impl");
            sym(&name, SymKind::Class, name.clone(), span, children)
        }
        ItemKind::Const(c) | ItemKind::Static(c) => {
            let detail = format!("const {}{}", c.name, c.ty.as_ref().map(|t| format!(": {}", type_text(t))).unwrap_or_default());
            sym(&c.name, SymKind::Const, detail, find_name(src, it.span, &c.name), Vec::new())
        }
        ItemKind::TypeAlias { name, ty, generics } => {
            sym(name, SymKind::TypeAlias, format!("typealias {}{} = {}", name, generics_text(generics), type_text(ty)), find_name(src, it.span, name), Vec::new())
        }
        ItemKind::Mod { name, items } | ItemKind::Object { name, items } => {
            let children = items.iter().filter_map(|m| item_symbol(m, src, Some(name))).collect();
            let kw = if matches!(it.kind, ItemKind::Object { .. }) { "object" } else { "mod" };
            sym(name, SymKind::Module, format!("{} {}", kw, name), find_name(src, it.span, name), children)
        }
        ItemKind::Test(t) => {
            let span = Span::new(it.span.start as usize, (it.span.start as usize + 4).min(it.span.end as usize));
            sym(&t.name, SymKind::Test, format!("test \"{}\"", t.name), span, Vec::new())
        }
        ItemKind::Import(_) | ItemKind::Raw(_) | ItemKind::Macro(_) => return None,
    })
}

/// Flattens nested symbols.
pub fn flatten(syms: &[Symbol]) -> Vec<&Symbol> {
    let mut out = Vec::new();
    fn go<'a>(s: &'a Symbol, out: &mut Vec<&'a Symbol>) {
        out.push(s);
        for c in &s.children {
            go(c, out);
        }
    }
    for s in syms {
        go(s, &mut out);
    }
    out
}

// ---------------------------------------------------------------------------------------------
// locals

/// A local binding (parameter or `val`/`var`/loop/lambda binding).
#[derive(Clone, Debug)]
pub struct Local {
    pub name: String,
    pub span: Span,
    pub ty: Option<Type>,
    pub is_param: bool,
}

/// The innermost function (or test/init block) containing `offset`, with the class that owns it.
pub fn enclosing_fun(file: &File, offset: u32) -> Option<(Option<&TypeDecl>, &FunDecl)> {
    fn in_items<'a>(items: &'a [Item], offset: u32, owner: Option<&'a TypeDecl>) -> Option<(Option<&'a TypeDecl>, &'a FunDecl)> {
        for it in items {
            if !(it.span.start <= offset && offset <= it.span.end) {
                continue;
            }
            match &it.kind {
                ItemKind::Fun(f) => return Some((owner, f)),
                ItemKind::Type(t) => {
                    if let Some(r) = in_items(&t.members, offset, Some(t)) {
                        return Some(r);
                    }
                }
                ItemKind::Trait(t) => {
                    if let Some(r) = in_items(&t.members, offset, owner) {
                        return Some(r);
                    }
                }
                ItemKind::Impl(i) => {
                    if let Some(r) = in_items(&i.members, offset, owner) {
                        return Some(r);
                    }
                }
                ItemKind::Mod { items, .. } | ItemKind::Object { items, .. } => {
                    if let Some(r) = in_items(items, offset, owner) {
                        return Some(r);
                    }
                }
                _ => {}
            }
        }
        None
    }
    in_items(&file.items, offset, None)
}

/// The class/enum declaration containing `offset`.
pub fn enclosing_type(file: &File, offset: u32) -> Option<&TypeDecl> {
    fn go(items: &[Item], offset: u32) -> Option<&TypeDecl> {
        for it in items {
            if it.span.start <= offset && offset <= it.span.end {
                match &it.kind {
                    ItemKind::Type(t) => return Some(t),
                    ItemKind::Mod { items, .. } | ItemKind::Object { items, .. } => return go(items, offset),
                    _ => {}
                }
            }
        }
        None
    }
    go(&file.items, offset)
}

/// Bindings declared before `offset` in the enclosing function (later ones shadow earlier ones).
pub fn locals_at(file: &File, offset: u32) -> Vec<Local> {
    let mut out = Vec::new();
    let Some((owner, f)) = enclosing_fun(file, offset) else {
        // test blocks
        for it in &file.items {
            if let ItemKind::Test(t) = &it.kind {
                if it.span.start <= offset && offset <= it.span.end {
                    collect_block(&t.body, offset, &mut out);
                }
            }
        }
        return out;
    };
    // its contexts: those of its own clause, else its class's
    let contexts = if f.contexts.is_empty() { owner.map(|t| t.contexts.as_slice()).unwrap_or_default() } else { &f.contexts };
    for p in f.params.iter().chain(contexts) {
        if let Pat::Ident { name, span, .. } = &p.pat {
            out.push(Local { name: name.clone(), span: *span, ty: Some(p.ty.clone()), is_param: true });
        }
    }
    match &f.body {
        Some(FunBody::Block(b)) => collect_block(b, offset, &mut out),
        Some(FunBody::Expr(e)) => collect_expr(e, offset, &mut out),
        None => {}
    }
    out
}

fn push_pat(p: &Pat, ty: Option<Type>, offset: u32, out: &mut Vec<Local>) {
    match p {
        Pat::Ident { name, span, sub, .. } => {
            if span.start <= offset {
                out.push(Local { name: name.clone(), span: *span, ty, is_param: false });
            }
            if let Some(s) = sub {
                push_pat(s, None, offset, out);
            }
        }
        Pat::Tuple(v) | Pat::Or(v) | Pat::Slice(v) => v.iter().for_each(|x| push_pat(x, None, offset, out)),
        Pat::TupleStruct { elems, .. } => elems.iter().for_each(|x| push_pat(x, None, offset, out)),
        Pat::Struct { fields, .. } => {
            for (_, p) in fields {
                if let Some(p) = p {
                    push_pat(p, None, offset, out);
                }
            }
        }
        Pat::Ref { pat, .. } | Pat::Typed { pat, .. } => push_pat(pat, None, offset, out),
        _ => {}
    }
}

fn collect_block(b: &Block, offset: u32, out: &mut Vec<Local>) {
    if b.span.start > offset {
        return;
    }
    for s in &b.stmts {
        match s {
            Stmt::Let { pat, ty, init, .. } => {
                if let Some(e) = init {
                    collect_expr(e, offset, out);
                }
                push_pat(pat, ty.clone(), offset, out);
            }
            Stmt::Expr { expr, .. } => collect_expr(expr, offset, out),
            Stmt::Attributed { stmt, .. } => collect_block(&Block { stmts: vec![(**stmt).clone()], span: b.span }, offset, out),
            Stmt::Item(_) => {}
        }
    }
}

fn collect_expr(e: &Expr, offset: u32, out: &mut Vec<Local>) {
    if e.span.start > offset {
        return;
    }
    let contains = e.span.start <= offset && offset <= e.span.end;
    visit::walk_expr(e, &mut |x| {
        if x.span.start > offset {
            return false;
        }
        let inside = x.span.start <= offset && offset <= x.span.end;
        match &x.kind {
            ExprKind::For { pat, body, .. } if inside || body.span.start <= offset && offset <= body.span.end => push_pat(pat, None, offset, out),
            ExprKind::Lambda { params, body, .. } if inside => {
                for p in params {
                    push_pat(&p.pat, p.ty.clone(), offset, out);
                }
                if params.is_empty() && visit::uses_it(body) {
                    out.push(Local { name: "it".into(), span: x.span, ty: None, is_param: true });
                }
            }
            ExprKind::If { cond, .. } | ExprKind::While { cond, .. } if inside => {
                let parts: Vec<&Cond> = match cond.as_ref() {
                    Cond::Chain(v) => v.iter().collect(),
                    c => vec![c],
                };
                for c in parts {
                    if let Cond::Let { pat, .. } = c {
                        push_pat(pat, None, offset, out);
                    }
                }
            }
            ExprKind::When { arms, binding, .. } if inside => {
                if let Some(b) = binding {
                    push_pat(b, None, offset, out);
                }
                for a in arms {
                    if a.span.start <= offset && offset <= a.span.end {
                        for c in &a.conds {
                            if let WhenCond::Pat(p) = c {
                                push_pat(p, None, offset, out);
                            }
                        }
                    }
                }
            }
            ExprKind::TryCatch { catch: Some(c), .. } if c.body.span.start <= offset && offset <= c.body.span.end => {
                push_pat(&c.binding, c.ty.clone(), offset, out);
            }
            _ => {}
        }
        // statements inside blocks: walk_expr visits blocks via walk_block; capture lets there
        if let ExprKind::Block(b) | ExprKind::Unsafe(b) | ExprKind::ConstBlock(b) | ExprKind::LabeledBlock { block: b, .. } = &x.kind {
            if b.span.start <= offset && offset <= b.span.end {
                collect_lets(b, offset, out);
            }
        }
        if let ExprKind::If { then, .. } = &x.kind {
            if then.span.start <= offset && offset <= then.span.end {
                collect_lets(then, offset, out);
            }
        }
        if let ExprKind::For { body, .. } | ExprKind::While { body, .. } | ExprKind::Loop { body, .. } | ExprKind::DoWhile { body, .. } = &x.kind {
            if body.span.start <= offset && offset <= body.span.end {
                collect_lets(body, offset, out);
            }
        }
        if let ExprKind::TryCatch { body, catch } = &x.kind {
            if body.span.start <= offset && offset <= body.span.end {
                collect_lets(body, offset, out);
            }
            if let Some(c) = catch {
                if c.body.span.start <= offset && offset <= c.body.span.end {
                    collect_lets(&c.body, offset, out);
                }
            }
        }
        true
    });
    let _ = contains;
}

fn collect_lets(b: &Block, offset: u32, out: &mut Vec<Local>) {
    for s in &b.stmts {
        if let Stmt::Let { pat, ty, .. } = s {
            push_pat(pat, ty.clone(), offset, out);
        }
    }
}
