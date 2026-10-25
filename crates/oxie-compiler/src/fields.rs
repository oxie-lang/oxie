//! Methods that take only the fields they use. Rust rejects `for item in &self.items {
//! self.note(item) }` when `note` takes `&mut self`: the loop borrows `items`, and the call
//! borrows all of `this` again. When `note` uses only other fields, it becomes an associated
//! function that takes them (`fn note(item: &str, log_count: &mut i32)`), and its calls pass
//! them (`Self::note(item, &mut self.log_count)`): borrows of different fields, which Rust allows.
//!
//! Runs on the parsed crate after receiver inference, since only a method that takes `&mut self`
//! is split, and rewrites the method and the calls to it; `compile` infers receivers again
//! afterwards. A method is split when a method of its class calls it inside a loop over another
//! field (or a lambda passed to a method of one), every use of it is such a call in its class,
//! and its fields can be parameters without changing what any use of them means.

use std::collections::{HashMap, HashSet};

use crate::ast::*;
use crate::diag::Span;
use crate::lower::infer_field_type;
use crate::params::{ParamNote, STR_READS, VEC_READS};
use crate::sema::{is_copy_simple, ClassInfo, Crate, Recv, MUTATING_METHODS};
use crate::visit::{walk_block, walk_block_mut, walk_expr_mut, walk_items};

/// The most fields a split method takes. With more, its signature stops reading like Rust a
/// person would write, who would group them in a struct.
const MAX_FIELDS: usize = 3;

/// Methods that only read what they are called on, besides those in `STR_READS` and `VEC_READS`
/// (maps, sets, `Option`s, numbers).
const OTHER_READS: &[&str] = &[
    "clone", "contains_key", "keys", "values", "is_some", "is_none", "as_ref", "as_deref", "max", "min", "abs", "pow",
    "clamp", "signum", "checked_add", "checked_sub", "saturating_add", "saturating_sub", "wrapping_add", "wrapping_sub",
];

/// How a split method gets a field.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Pass {
    /// a `Copy` field it only reads, by value
    Value,
    /// a field it only reads: `&T`, or `&str` and `&[T]` when it only uses what those have
    Ref,
    /// a field it changes: `&mut T`
    Mut,
}

/// Where a method uses a field of `this`.
#[derive(Clone, PartialEq, Debug)]
enum Use {
    /// the receiver of a call of this method
    Recv(String),
    /// the receiver of a field access, or the base of an index
    Place,
    /// the whole target of an assignment
    Assigned,
    /// the start of an assignment's target (`items[0] = x`)
    InTarget,
    /// borrowed, mutably or not (`&items`)
    Borrowed(bool),
    /// formatted in a template
    Template,
    /// what a `for` loop goes through
    Looped,
    /// anything else: its value
    Value,
}

/// A field that a split method takes.
#[derive(Clone, Debug)]
struct FieldParam {
    name: String,
    /// the parameter's type
    ty: Type,
    pass: Pass,
}

/// A method to split, in the class `class` of the file `file`.
struct Split {
    file: usize,
    class: String,
    method: String,
    /// its parameters before the fields (the calls name them or leave defaults out)
    params: Vec<Param>,
    fields: Vec<FieldParam>,
    /// the uses of the fields in its body, by span: the field, and whether it becomes `*field`
    refs: HashMap<(u32, u32), (String, bool)>,
    /// why: the method calling it, how that method borrows another field, and the call
    caller: String,
    borrow: String,
    at: Span,
}

/// Splits the methods that should take only the fields they use (see the module docs), and says
/// why for each field they take. `files` are the crate's files with their modules.
pub fn take_fields(files: &mut [(&[String], &mut File)], k: &Crate) -> Vec<ParamNote> {
    let splits = {
        let views: Vec<(&[String], &File)> = files.iter().map(|(m, f)| (*m, &**f)).collect();
        find(&views, k)
    };
    let mut notes = Vec::new();
    for s in &splits {
        rewrite(&mut *files[s.file].1, s, &mut notes);
    }
    notes
}

/// The methods to split.
fn find(files: &[(&[String], &File)], k: &Crate) -> Vec<Split> {
    let copy: HashSet<String> = k.derives.iter().filter(|(_, d)| d.iter().any(|x| x == "Copy")).map(|(n, _)| n.clone()).collect();
    let mut splits = Vec::new();
    for (i, (module, file)) in files.iter().enumerate() {
        for it in &file.items {
            let ItemKind::Type(d) = &it.kind else { continue };
            if !matches!(d.kind, TypeKind::Class | TypeKind::Data) || d.shared || it.vis == Vis::Public || k.same_named.contains(&d.name) {
                continue;
            }
            let Some(info) = k.classes.get(&k.decl_key(module, &d.name)) else { continue };
            let class = ClassParts::new(i, &file.items, it, d, info, k);
            for m in &d.members {
                if let ItemKind::Fun(f) = &m.kind {
                    splits.extend(split_of(&class, m, f, info, k, &copy, files));
                }
            }
        }
    }
    splits
}

/// A class's fields and the code that belongs to it.
struct ClassParts<'a> {
    file: usize,
    name: &'a str,
    /// its fields in order, with their types when known (`var count = 0` has its default's)
    fields: Vec<(&'a str, Option<Type>)>,
    /// the names its methods can call without `this.`: its methods and its interfaces'
    methods: HashSet<String>,
    /// its methods, in its body and in `impl` blocks for it in its file
    funs: Vec<&'a FunDecl>,
    init: Option<&'a Block>,
    /// where its declaration and those `impl` blocks are in the file
    spans: Vec<Span>,
}

impl<'a> ClassParts<'a> {
    fn new(file: usize, items: &'a [Item], it: &'a Item, d: &'a TypeDecl, info: &ClassInfo, k: &Crate) -> ClassParts<'a> {
        let fields = d.fields.iter().map(|f| (f.name.as_str(), if f.ty == Type::Infer { f.default.as_ref().and_then(infer_field_type) } else { Some(f.ty.clone()) })).collect();
        let mut methods: HashSet<String> = info.methods.keys().cloned().collect();
        for s in &d.supertypes {
            if let Some(t) = s.last_name().and_then(|n| k.traits.get(n)) {
                methods.extend(t.methods.keys().cloned());
            }
        }
        let funs_of = |members: &'a [Item]| members.iter().filter_map(|m| if let ItemKind::Fun(f) = &m.kind { Some(f) } else { None });
        let mut funs: Vec<&FunDecl> = funs_of(&d.members).collect();
        let mut spans = vec![it.span];
        for other in items {
            if let ItemKind::Impl(i) = &other.kind {
                if i.self_ty.last_name() == Some(d.name.as_str()) {
                    funs.extend(funs_of(&i.members));
                    spans.push(other.span);
                }
            }
        }
        ClassParts { file, name: &d.name, fields, methods, funs, init: d.init.as_ref(), spans }
    }

    fn field_names(&self) -> HashSet<&'a str> {
        self.fields.iter().map(|(n, _)| *n).collect()
    }

    /// Is the code at `at` in the file `file` part of the class?
    fn contains(&self, file: usize, at: Span) -> bool {
        file == self.file && self.spans.iter().any(|s| s.start <= at.start && at.end <= s.end)
    }
}

/// How to split the method `f` of a class, if it should take only the fields it uses.
fn split_of(c: &ClassParts, item: &Item, f: &FunDecl, info: &ClassInfo, k: &Crate, copy: &HashSet<String>, files: &[(&[String], &File)]) -> Option<Split> {
    let m = info.methods.get(&f.name)?;
    let plain = f.receiver.is_none()
        && f.self_param.is_none()
        && !f.mods.is_async
        && !f.mods.is_const
        && !f.mods.is_static
        && !f.mods.is_override
        && !f.mods.is_operator
        && !f.mods.is_unsafe
        && f.mods.abi.is_none();
    if m.recv != Recv::RefMut || m.explicit || item.vis == Vis::Public || !plain {
        return None;
    }
    // a result borrowed from a field would borrow from the field parameters
    let lends = |n: &str| k.class(n).is_some_and(|c| c.lifetimes > 0) || k.enum_(n).is_some_and(|e| e.lifetimes > 0);
    let returns_ok = match (&f.ret, &f.body) {
        (Some(t), _) => !borrows(t, &lends),
        // without a return type, a body gives `()` (an expression body that gives a value
        // is an error asking for its type)
        (None, Some(FunBody::Block(_) | FunBody::Expr(_))) => true,
        _ => false,
    };
    if !returns_ok {
        return None;
    }
    let body = body_of(f)?;
    let names = c.field_names();
    let bound = locals(&f.params, &body)?;
    // a variable named like a field hides it, and would hide its parameter
    if names.iter().any(|n| bound.contains(*n) && mentions(&body, n)) {
        return None;
    }
    let uses = field_uses(&body, &names, &bound, &c.methods)?;
    let used: Vec<&str> = c.fields.iter().map(|(n, _)| *n).filter(|n| uses.iter().any(|(x, ..)| x == n)).collect();
    if used.is_empty() || used.len() > MAX_FIELDS {
        return None;
    }
    let mut fields = Vec::new();
    let mut refs = HashMap::new();
    for &n in &used {
        let ty = c.fields.iter().find(|(x, _)| *x == n)?.1.clone()?;
        let here: Vec<&Use> = uses.iter().filter(|(x, ..)| x == n).map(|(.., u)| u).collect();
        let is_copy = is_copy_simple(&ty, copy);
        let (pass, pty) = pass_for(&ty, is_copy, &here)?;
        for (x, span, u) in uses.iter().filter(|(x, ..)| x == n) {
            // through a reference, a value is read and assigned as `*count`; field access,
            // indexing, formatting and a non-`Copy` value's methods see through it
            let deref = match pass {
                Pass::Value => false,
                _ if is_copy => !matches!(u, Use::Place | Use::InTarget | Use::Template),
                _ => matches!(u, Use::Assigned | Use::Borrowed(_)),
            };
            refs.insert((span.start, span.end), (x.clone(), deref));
        }
        fields.push(FieldParam { name: n.to_string(), ty: pty, pass });
    }
    let (caller, borrow, at) = trigger(c, &f.name, &fields)?;
    if used_outside(c, &f.name, files) {
        return None;
    }
    Some(Split { file: c.file, class: c.name.to_string(), method: f.name.clone(), params: f.params.clone(), fields, refs, caller, borrow, at })
}

/// A function's body as a block.
fn body_of(f: &FunDecl) -> Option<Block> {
    match &f.body {
        Some(FunBody::Block(b)) => Some(b.clone()),
        Some(FunBody::Expr(e)) => Some(Block { stmts: vec![Stmt::Expr { expr: e.clone(), semi: false }], span: e.span }),
        None => None,
    }
}

/// Does a type hold a reference or a lifetime (`lends`: an Oxie type with lifetime parameters)?
fn borrows(t: &Type, lends: &dyn Fn(&str) -> bool) -> bool {
    match t {
        Type::Path { segs, .. } => segs.iter().any(|s| {
            lends(&s.name)
                || s.generics.iter().flatten().any(|g| match g {
                    GenericArg::Lifetime(_) => true,
                    GenericArg::Type(t) | GenericArg::Binding(_, t) => borrows(t, lends),
                    GenericArg::Const(_) => false,
                })
        }),
        Type::Tuple(v) => v.iter().any(|t| borrows(t, lends)),
        Type::Array(t, _) | Type::Slice(t) | Type::Option(t) | Type::Maybe(t) => borrows(t, lends),
        Type::Fn { params, ret } => params.iter().any(|t| borrows(t, lends)) || borrows(ret, lends),
        Type::Never | Type::Infer => false,
        _ => true,
    }
}

/// The names a function binds, its parameters included. None when its body declares an item (a
/// local function or class), which this pass doesn't look into.
fn locals(params: &[Param], body: &Block) -> Option<HashSet<String>> {
    let mut names = Vec::new();
    for p in params {
        p.pat.bindings(&mut names);
    }
    // the `val`s of a block (false when it declares an item)
    fn lets(b: &Block, names: &mut Vec<String>) -> bool {
        for s in &b.stmts {
            let mut s = s;
            while let Stmt::Attributed { stmt, .. } = s {
                s = stmt;
            }
            match s {
                Stmt::Let { pat, .. } => pat.bindings(names),
                Stmt::Item(_) => return false,
                Stmt::Expr { .. } | Stmt::Attributed { .. } => {}
            }
        }
        true
    }
    fn conds(c: &Cond, names: &mut Vec<String>) {
        match c {
            Cond::Let { pat, .. } => pat.bindings(names),
            Cond::Chain(parts) => parts.iter().for_each(|c| conds(c, names)),
            Cond::Expr(_) => {}
        }
    }
    let mut ok = lets(body, &mut names);
    walk_block(body, &mut |e| {
        match &e.kind {
            ExprKind::Block(b) | ExprKind::Unsafe(b) | ExprKind::ConstBlock(b) | ExprKind::LabeledBlock { block: b, .. } | ExprKind::Loop { body: b, .. } | ExprKind::Async { block: b, .. } | ExprKind::DoWhile { body: b, .. } => {
                ok &= lets(b, &mut names)
            }
            ExprKind::While { cond, body, .. } | ExprKind::If { cond, then: body, .. } => {
                conds(cond, &mut names);
                ok &= lets(body, &mut names);
            }
            ExprKind::For { pat, body, .. } => {
                pat.bindings(&mut names);
                ok &= lets(body, &mut names);
            }
            ExprKind::Lambda { params, implicit_it, .. } => {
                params.iter().for_each(|p| p.pat.bindings(&mut names));
                if *implicit_it {
                    names.push("it".into());
                }
            }
            ExprKind::When { binding, arms, .. } => {
                if let Some(b) = binding {
                    b.bindings(&mut names);
                }
                for c in arms.iter().flat_map(|a| a.conds.iter()) {
                    if let WhenCond::Pat(p) = c {
                        p.bindings(&mut names);
                    }
                }
            }
            ExprKind::TryCatch { body, catch } => {
                ok &= lets(body, &mut names);
                if let Some(c) = catch {
                    c.binding.bindings(&mut names);
                    ok &= lets(&c.body, &mut names);
                }
            }
            _ => {}
        }
        true
    });
    ok.then(|| names.into_iter().collect())
}

/// Does the body use the name `n` as a variable or a field?
fn mentions(body: &Block, n: &str) -> bool {
    let mut found = false;
    walk_block(body, &mut |e| {
        found |= e.ident() == Some(n);
        !found
    });
    found
}

/// The field `e` names: `count` (when no variable has that name) or `this.count`.
fn field_ref<'e>(e: &'e Expr, fields: &HashSet<&str>, bound: &HashSet<String>) -> Option<&'e str> {
    match &e.kind {
        ExprKind::Path { .. } => e.ident().filter(|n| fields.contains(n) && !bound.contains(*n)),
        ExprKind::Field { recv, name, safe: false } if matches!(recv.kind, ExprKind::SelfValue) && fields.contains(name.as_str()) => Some(name.as_str()),
        _ => None,
    }
}

/// Methods that start a lazy view of what they are called on (`items.iter()`).
const VIEWS: &[&str] = &["iter", "iter_mut", "chars", "char_indices", "bytes", "lines", "split", "split_whitespace", "keys", "values", "values_mut", "windows", "chunks", "drain"];

/// Lazy adapters, which keep the view they are called on.
const ADAPTERS: &[&str] = &["enumerate", "rev", "skip", "take", "step_by", "zip", "peekable", "cloned", "copied", "filter", "map", "filter_map", "take_while", "skip_while", "flat_map", "chain", "inspect"];

/// The field of `this` that `e` keeps borrowed while it is used, and whether mutably: the field
/// itself (`items`), or a lazy view of it (`items.iter().enumerate()`, `items.iter_mut()`). Not a
/// copy (`items.clone()`) or a new list (`items.map { .. }`), which leave the field free.
fn borrowed_field<'e>(e: &'e Expr, fields: &HashSet<&str>, bound: &HashSet<String>) -> Option<(&'e str, bool)> {
    if let Some(n) = field_ref(e, fields, bound) {
        return Some((n, false));
    }
    match &e.kind {
        ExprKind::Paren(x) => borrowed_field(x, fields, bound),
        ExprKind::MethodCall { recv, name, .. } => match field_ref(recv, fields, bound) {
            Some(n) => VIEWS.contains(&name.as_str()).then(|| (n, matches!(name.as_str(), "iter_mut" | "values_mut" | "drain"))),
            None if ADAPTERS.contains(&name.as_str()) && matches!(recv.kind, ExprKind::MethodCall { .. }) => borrowed_field(recv, fields, bound),
            None => None,
        },
        _ => None,
    }
}

/// The uses of the fields of `this` in a method's body: (field, the use's span, where it is
/// used). None when the body uses `this` in a way field parameters can't express: `this` itself,
/// a method of the class, or a macro or `rust { }` code, which may name `self`.
fn field_uses(body: &Block, fields: &HashSet<&str>, bound: &HashSet<String>, methods: &HashSet<String>) -> Option<Vec<(String, Span, Use)>> {
    let key = |s: Span| (s.start, s.end);
    // where each expression is used, set by the expression around it (the outermost one wins)
    let mut at: HashMap<(u32, u32), Use> = HashMap::new();
    let mut found = Vec::new();
    let mut ok = true;
    let is_field = |x: &Expr| field_ref(x, fields, bound).is_some();
    walk_block(body, &mut |e| {
        if let Some(n) = field_ref(e, fields, bound) {
            found.push((n.to_string(), e.span));
            return false;
        }
        let mut put = |s: Span, u: Use| {
            at.entry(key(s)).or_insert(u);
        };
        match &e.kind {
            ExprKind::SelfValue | ExprKind::Macro { .. } | ExprKind::Raw(_) => ok = false,
            ExprKind::Call { callee, .. } => ok &= !callee.ident().is_some_and(|n| methods.contains(n) && !bound.contains(n)),
            ExprKind::MethodCall { recv, name, .. } => put(recv.span, Use::Recv(name.clone())),
            ExprKind::Field { recv, .. } | ExprKind::Index(recv, _) => put(recv.span, Use::Place),
            ExprKind::Assign(_, target, _) if is_field(target) => put(target.span, Use::Assigned),
            ExprKind::Assign(_, target, _) => put(target_root(target, &is_field).span, Use::InTarget),
            ExprKind::Unary(UnOp::Ref, x) => put(x.span, Use::Borrowed(false)),
            ExprKind::Unary(UnOp::RefMut, x) => put(x.span, Use::Borrowed(true)),
            ExprKind::Template(parts) => {
                for p in parts {
                    if let TemplatePart::Expr(x, _) = p {
                        put(x.span, Use::Template);
                    }
                }
            }
            ExprKind::For { iter, .. } => put(iter.span, Use::Looped),
            _ => {}
        }
        ok
    });
    ok.then(|| found.into_iter().map(|(n, s)| (n, s, at.get(&key(s)).cloned().unwrap_or(Use::Value))).collect())
}

/// What an assignment's target starts from: the field in `items[0].count = 1`.
fn target_root<'e>(e: &'e Expr, is_field: &dyn Fn(&Expr) -> bool) -> &'e Expr {
    match &e.kind {
        _ if is_field(e) => e,
        ExprKind::Index(b, _) | ExprKind::Field { recv: b, .. } | ExprKind::Paren(b) => target_root(b, is_field),
        _ => e,
    }
}

/// How a method gets a field of type `ty` that it uses as `uses`, and the parameter's type. None
/// when a use can't go through a parameter: a value that isn't `Copy`, used whole.
fn pass_for(ty: &Type, copy: bool, uses: &[&Use]) -> Option<(Pass, Type)> {
    let by_ref = |t: Type, mutable: bool| Type::Ref { lifetime: None, mutable, inner: Box::new(t) };
    let reads = |m: &str| STR_READS.contains(&m) || VEC_READS.contains(&m) || OTHER_READS.contains(&m);
    // a method Oxie doesn't know may change the field
    let changes = uses.iter().any(|u| match u {
        Use::Assigned | Use::InTarget | Use::Borrowed(true) => true,
        Use::Recv(m) => MUTATING_METHODS.contains(&m.as_str()) || !reads(m),
        _ => false,
    });
    if copy {
        return Some(if changes { (Pass::Mut, by_ref(ty.clone(), true)) } else { (Pass::Value, ty.clone()) });
    }
    // a value that isn't `Copy` can't be used whole through a reference, and a loop over a
    // `&mut` gives `&mut` elements
    if uses.iter().any(|u| matches!(u, Use::Value) || (changes && matches!(u, Use::Looped))) {
        return None;
    }
    if changes {
        return Some((Pass::Mut, by_ref(ty.clone(), true)));
    }
    // `&str` and `&[T]` when every method called on it exists on those
    let only = |list: &[&str]| uses.iter().all(|u| !matches!(u, Use::Recv(m) if !list.contains(&m.as_str())));
    let inner = match ty {
        Type::Path { segs, .. } if segs.len() == 1 && segs[0].name == "String" && segs[0].generics.is_none() && only(STR_READS) => Type::simple("str"),
        Type::Path { segs, .. } if segs.len() == 1 && segs[0].name == "Vec" && only(VEC_READS) => match segs[0].generics.as_deref() {
            Some([GenericArg::Type(t)]) => Type::Slice(Box::new(t.clone())),
            _ => ty.clone(),
        },
        _ => ty.clone(),
    };
    Some((Pass::Ref, by_ref(inner, false)))
}

/// A call of the method `m` that Rust rejects because a field is borrowed: in a loop over a field
/// of `this`, or in a lambda passed to a method of one. The caller's name, how it borrows that
/// field, and the call. None when there is no such call; when `m` changes a field borrowed around
/// a call, or reads one that is borrowed mutably (a real conflict, which stays an error); or when
/// a method binds a variable named `m` (whose calls aren't calls of the method). `fields`: the
/// fields `m` takes.
fn trigger(c: &ClassParts, m: &str, fields: &[FieldParam]) -> Option<(String, String, Span)> {
    let names = c.field_names();
    let mut bodies: Vec<(String, Block, HashSet<String>)> = Vec::new();
    for f in &c.funs {
        let Some(body) = body_of(f) else { continue };
        let bound = locals(&f.params, &body)?;
        bodies.push((f.name.clone(), body, bound));
    }
    if let Some(b) = c.init {
        bodies.push(("init".into(), b.clone(), locals(&[], b)?));
    }
    let mut found = None;
    for (caller, body, bound) in &bodies {
        if bound.contains(m) {
            return None;
        }
        // the parts of the body that run while a field is borrowed: (span, field, mutably, how)
        let mut held: Vec<(Span, String, bool, String)> = Vec::new();
        let mut calls: Vec<Span> = Vec::new();
        walk_block(body, &mut |e| {
            match &e.kind {
                ExprKind::For { iter, body, .. } => {
                    if let Some((f, mutably)) = borrowed_field(iter, &names, bound) {
                        held.push((body.span, f.to_string(), mutably, format!("it loops over `{}`", f)));
                    }
                }
                ExprKind::MethodCall { recv, name, args, .. } => {
                    if matches!(recv.kind, ExprKind::SelfValue) && name == m {
                        calls.push(e.span);
                    }
                    if let Some((f, mutably)) = borrowed_field(recv, &names, bound) {
                        let mutably = mutably || MUTATING_METHODS.contains(&name.as_str());
                        for a in args.iter().filter(|a| matches!(a.value.kind, ExprKind::Lambda { .. })) {
                            held.push((a.value.span, f.to_string(), mutably, format!("`{}.{}()` runs it", f, name)));
                        }
                    }
                }
                ExprKind::Call { callee, .. } if callee_is(callee, m) => calls.push(e.span),
                _ => {}
            }
            true
        });
        for call in calls {
            for (_, f, mutably, how) in held.iter().filter(|(r, ..)| r.start <= call.start && call.end <= r.end) {
                // reading a field that is only read around the call is fine
                if fields.iter().any(|p| p.name == *f && (p.pass == Pass::Mut || *mutably)) {
                    return None;
                }
                found.get_or_insert_with(|| (caller.clone(), how.clone(), call));
            }
        }
    }
    found
}

/// Is `callee` the name `m` (with type arguments or not)?
fn callee_is(callee: &Expr, m: &str) -> bool {
    matches!(&callee.kind, ExprKind::Path { segs, global: false, .. } if segs.len() == 1 && segs[0] == m)
}

/// Is the method `m` used anywhere but in calls from its own class: a call on another value
/// (`other.note()`), a call through a path (`Logger.note()`), a call of that name outside the
/// class, or a function value (`xs.map(note)`)? By name, so another class's method of that name
/// counts too.
fn used_outside(c: &ClassParts, m: &str, files: &[(&[String], &File)]) -> bool {
    let mut outside = false;
    for (i, (_, file)) in files.iter().enumerate() {
        let mut callees = HashSet::new();
        walk_all(&file.items, &mut |e| {
            let inside = c.contains(i, e.span);
            match &e.kind {
                ExprKind::Call { callee, .. } => {
                    callees.insert((callee.span.start, callee.span.end));
                    outside |= !inside && callee_is(callee, m);
                }
                ExprKind::MethodCall { recv, name, .. } => outside |= name == m && !(inside && matches!(recv.kind, ExprKind::SelfValue)),
                ExprKind::Field { name, .. } => outside |= name == m,
                ExprKind::Path { segs, .. } => outside |= segs.last().is_some_and(|s| s == m) && (segs.len() > 1 || !callees.contains(&(e.span.start, e.span.end))),
                _ => {}
            }
            !outside
        });
    }
    outside
}

/// `walk_items`, also going into the items declared inside functions.
fn walk_all(items: &[Item], f: &mut dyn FnMut(&Expr) -> bool) {
    walk_items(items, &mut |e| {
        for it in local_items(e) {
            walk_all(std::slice::from_ref(it), f);
        }
        f(e)
    });
}

/// The items declared in the blocks directly inside `e`.
pub(crate) fn local_items(e: &Expr) -> Vec<&Item> {
    let blocks: Vec<&Block> = match &e.kind {
        ExprKind::Block(b) | ExprKind::Unsafe(b) | ExprKind::ConstBlock(b) | ExprKind::LabeledBlock { block: b, .. } | ExprKind::Loop { body: b, .. } | ExprKind::Async { block: b, .. } | ExprKind::DoWhile { body: b, .. } => vec![b],
        ExprKind::While { body, .. } | ExprKind::If { then: body, .. } | ExprKind::For { body, .. } => vec![body],
        ExprKind::TryCatch { body, catch } => std::iter::once(body).chain(catch.iter().map(|c| &c.body)).collect(),
        _ => Vec::new(),
    };
    blocks.into_iter().flat_map(|b| b.stmts.iter()).filter_map(|s| if let Stmt::Item(i) = s { Some(&i.0) } else { None }).collect()
}

/// Makes the method of `s` take the fields it uses, and passes them in the calls in its class.
fn rewrite(file: &mut File, s: &Split, notes: &mut Vec<ParamNote>) {
    for it in &mut file.items {
        match &mut it.kind {
            ItemKind::Type(d) if d.name == s.class => {
                for m in &mut d.members {
                    if let ItemKind::Fun(f) = &mut m.kind {
                        if f.name == s.method {
                            take_params(f, s, notes);
                        } else {
                            pass_fields(f, s);
                        }
                    }
                }
                if let Some(b) = &mut d.init {
                    walk_block_mut(b, &mut |e| pass_fields_to(e, s));
                }
            }
            ItemKind::Impl(i) if i.self_ty.last_name() == Some(s.class.as_str()) => {
                for m in &mut i.members {
                    if let ItemKind::Fun(f) = &mut m.kind {
                        pass_fields(f, s);
                    }
                }
            }
            _ => {}
        }
    }
}

/// Makes `f` the associated function of `s`, taking the fields after its parameters, and records
/// why for each of them.
fn take_params(f: &mut FunDecl, s: &Split, notes: &mut Vec<ParamNote>) {
    f.mods.is_static = true;
    let first = f.params.len();
    for (j, p) in s.fields.iter().enumerate() {
        let span = f.name_span;
        f.params.push(Param { pat: Pat::Ident { name: p.name.clone(), by_ref: false, mutable: false, sub: None, span }, ty: p.ty.clone(), default: None, context: false, lent: false, span });
        let what = if p.pass == Pass::Mut { "changes" } else { "only reads" };
        let why = format!("`{}` {} `{}`, and `{}` calls it while {}", f.name, what, p.name, s.caller, s.borrow);
        notes.push(ParamNote { file: s.file, fun: f.name.clone(), fun_span: f.name_span, index: first + j, borrowed: true, why, cause: Some(s.at) });
    }
    let mut to_param = |e: &mut Expr| {
        let Some((name, deref)) = s.refs.get(&(e.span.start, e.span.end)) else { return true };
        if !matches!(e.kind, ExprKind::Path { .. } | ExprKind::Field { .. }) {
            return true;
        }
        let path = Expr::new(ExprKind::Path { segs: vec![name.clone()], generics: None, global: false }, e.span);
        *e = if *deref { Expr::new(ExprKind::Unary(UnOp::Deref, Box::new(path)), e.span) } else { path };
        false
    };
    match &mut f.body {
        Some(FunBody::Block(b)) => walk_block_mut(b, &mut to_param),
        Some(FunBody::Expr(e)) => walk_expr_mut(e, &mut to_param),
        None => {}
    }
}

/// Passes the fields in the calls of the method of `s` in `f`'s body.
fn pass_fields(f: &mut FunDecl, s: &Split) {
    match &mut f.body {
        Some(FunBody::Block(b)) => walk_block_mut(b, &mut |e| pass_fields_to(e, s)),
        Some(FunBody::Expr(e)) => walk_expr_mut(e, &mut |x| pass_fields_to(x, s)),
        None => {}
    }
}

/// `note(item)` or `this.note(item)` as `note(item, this.log_count)`: the arguments in the
/// order of the parameters (named ones and left-out defaults too, as Oxie passes them), then the
/// fields. A call whose arguments don't match the parameters stays as it is, for rustc to report.
fn pass_fields_to(e: &mut Expr, s: &Split) -> bool {
    let (generics, args) = match &e.kind {
        ExprKind::Call { callee, args } if callee_is(callee, &s.method) => match &callee.kind {
            ExprKind::Path { generics, .. } => (generics.clone(), args),
            _ => return true,
        },
        ExprKind::MethodCall { recv, name, generics, args, safe: false } if matches!(recv.kind, ExprKind::SelfValue) && *name == s.method => (generics.clone(), args),
        _ => return true,
    };
    let mut slots: Vec<Option<Expr>> = vec![None; s.params.len()];
    let mut next = 0;
    for a in args {
        let i = match &a.name {
            Some(n) => match s.params.iter().position(|p| p.name() == Some(n.as_str())) {
                Some(i) => i,
                None => return true,
            },
            None => {
                while next < slots.len() && slots[next].is_some() {
                    next += 1;
                }
                next
            }
        };
        match slots.get_mut(i) {
            Some(slot) if slot.is_none() => *slot = Some(a.value.clone()),
            _ => return true,
        }
    }
    let mut new_args = Vec::new();
    for (p, slot) in s.params.iter().zip(slots) {
        let Some(value) = slot.or_else(|| p.default.clone()) else { return true };
        new_args.push(Arg { name: None, value, trailing: false });
    }
    for p in &s.fields {
        let this = Expr::new(ExprKind::SelfValue, e.span);
        new_args.push(Arg { name: None, value: Expr::new(ExprKind::Field { recv: Box::new(this), name: p.name.clone(), safe: false }, e.span), trailing: false });
    }
    let callee = Expr::new(ExprKind::Path { segs: vec![s.method.clone()], generics, global: false }, e.span);
    e.kind = ExprKind::Call { callee: Box::new(callee), args: new_args };
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The methods of `src` that are split, with their fields' parameters (`log_count: &mut i32`).
    fn splits(src: &str) -> Vec<(String, Vec<String>)> {
        let (mut file, diags) = crate::parser::parse_file(src);
        assert!(diags.is_empty(), "{:?}", diags);
        let module: Vec<String> = Vec::new();
        let k = {
            let units = [crate::sema::SourceUnit { module: module.clone(), file: &file }];
            crate::sema::collect(&units, "t", HashSet::new(), false)
        };
        let notes = take_fields(&mut [(&module[..], &mut file)], &k);
        let mut out: Vec<(String, Vec<String>)> = Vec::new();
        for n in &notes {
            if !out.iter().any(|(f, _)| *f == n.fun) {
                out.push((n.fun.clone(), Vec::new()));
            }
        }
        for it in &file.items {
            let ItemKind::Type(d) = &it.kind else { continue };
            for m in &d.members {
                let ItemKind::Fun(f) = &m.kind else { continue };
                if let Some((_, ps)) = out.iter_mut().find(|(n, _)| *n == f.name) {
                    for p in &f.params[f.params.len() - notes.iter().filter(|n| n.fun == f.name).count()..] {
                        ps.push(format!("{}: {}", p.name().unwrap(), crate::symbols::type_text(&p.ty)));
                    }
                }
            }
        }
        out
    }

    #[test]
    fn methods_take_the_fields_they_use() {
        let src = r#"
class Log(var items: Vec<String>, var count: i32 = 0, var limit: usize = 9, var names: Vec<String> = vec[]) {
    fun note(item: &str) {
        if ((count as usize) < limit) count += 1
        println("$item ${names.len()}")
    }
    fun scan() {
        for (item in items) { note(item) }
    }
    // uses `items` itself: a real conflict
    fun keep(item: &str) { items.push(item.to_string()) }
    fun copy_all() {
        for (item in items) { keep(item) }
    }
    // called with no loop around it
    fun bump() { count += 1 }
    fun once() = bump()
    // passes `this` on
    fun tell() { count += 1; show(this) }
    fun spread() { for (item in items) { tell() } }
    // a lambda passed to a method of another field
    fun tick() { count += 1 }
    fun each() = items.iter().for_each { tick() }
    // a loop over a copy leaves `items` free: nothing to change
    fun mark() { count += 1 }
    fun copied() { for (item in items.clone()) { mark() } }
    // reads `items` while the loop reads it too: fine
    fun stamp() { count += items.len() as i32 }
    fun stamp_all() { for (item in items) { stamp() } }
    // reads `items` while the loop changes it: a real conflict
    fun peek() { count += items.len() as i32 }
    fun grow() { for (item in items.iter_mut()) { peek() } }
}
fun show(l: &Log) = println(l.count)
"#;
        let s = splits(src);
        let tick = ("tick".to_string(), vec!["count: &mut i32".to_string()]);
        let stamp = ("stamp".to_string(), vec!["items: &[String]".to_string(), "count: &mut i32".into()]);
        assert_eq!(s, vec![("note".to_string(), vec!["count: &mut i32".to_string(), "limit: usize".into(), "names: &[String]".into()]), tick, stamp]);
    }

    #[test]
    fn methods_used_from_outside_their_class_stay() {
        let src = r#"
class Log(var items: Vec<String>, var count: i32 = 0) {
    fun note(item: &str) { count += 1 }
    fun scan() {
        for (item in items) { note(item) }
    }
}
fun main() {
    var l = Log(vec[])
    l.note("x")
}
"#;
        assert!(splits(src).is_empty());
    }
}
