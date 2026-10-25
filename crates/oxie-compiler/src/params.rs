//! Borrowed parameters: a `String` or `Vec<T>` parameter that a function only reads becomes
//! `&str` / `&[T]`, so callers keep their value (`greet(name)` lends `name` instead of giving it
//! away). Applied to non-`pub` top-level functions, whose callers are all in the crate. A `Vec<T>`
//! is lent only when dropping a `T` just frees memory: lending moves the drop to the caller, and
//! a `MutexGuard` would unlock later.
//!
//! A parameter of an Oxie class or `Box<T>` becomes `&T` the same way, after receiver inference
//! (`borrow_class_params`): whether `u.name.len()` or `u.greeting()` only reads `u` takes the
//! class's fields and methods.

use std::collections::{HashMap, HashSet};

use crate::ast::*;
use crate::diag::Span;
use crate::lower::infer_field_type;
use crate::sema::{is_copy_simple, ClassInfo, Crate, Recv, MUTATING_METHODS};
use crate::symbols::type_text;
use crate::visit::{walk_block, walk_expr};

/// Methods that only read a `String` and exist on `str` with the same meaning.
pub(crate) const STR_READS: &[&str] = &[
    "len", "is_empty", "contains", "starts_with", "ends_with", "chars", "char_indices", "bytes", "lines", "split",
    "splitn", "rsplit", "rsplitn", "split_whitespace", "split_once", "rsplit_once", "trim", "trim_start", "trim_end",
    "strip_prefix", "strip_suffix", "to_uppercase", "to_lowercase", "to_string", "to_owned", "find", "rfind", "replace",
    "parse", "repeat", "as_bytes", "get", "matches", "eq_ignore_ascii_case", "is_char_boundary",
];

/// Methods that only read a `Vec<T>` and exist on slices (including Oxie's list operations).
pub(crate) const VEC_READS: &[&str] = &[
    "len", "is_empty", "contains", "starts_with", "ends_with", "iter", "first", "last", "get", "to_vec", "to_owned",
    "join", "concat", "windows", "chunks", "binary_search", "split_first", "split_last", "map", "map_indexed",
    "filter", "filter_not", "filter_map", "flat_map", "for_each", "for_each_indexed", "find", "find_last",
    "index_of_first", "any", "all", "none", "count", "sum", "sum_of", "max_of", "min_of", "max_by_key", "min_by_key",
    "sorted", "sorted_descending", "sorted_by", "sorted_by_descending", "reversed", "distinct", "distinct_by",
    "chunked", "group_by", "associate_by", "partition", "join_to_string", "join_with", "is_not_empty",
];

/// Formatting macros, which take their arguments by reference.
const PRINTS: &[&str] = &["println", "print", "eprintln", "eprint", "format", "panic", "assert", "assert_eq", "assert_ne", "debug_assert", "debug_assert_eq", "debug_assert_ne"];

/// Methods that only read a value of any class: derived and std ones, and a data class's `copy`.
const CLASS_READS: &[&str] = &["clone", "to_string", "eq", "ne", "cmp", "partial_cmp", "hash", "fmt", "copy"];

#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Str,
    Vec,
    /// an Oxie class that is neither `Copy` nor `shared`, lent as `&T`
    Class,
    /// `Box<T>`, lent as `&T`
    Boxed,
}

/// What the analysis knows. Before receiver inference, the source, which tells what reads a
/// `String` or `Vec<T>` (`Text`); after it, the crate's classes (`Types`), whose fields and
/// methods tell what reads a class or `Box<T>` (and only those are candidates then).
enum Stage<'a> {
    Text,
    Types { k: &'a Crate, copy: HashSet<String> },
}

/// What a parameter of type `ty`, in a function of `module`, is at this stage, and the class of
/// the value it lends (the class, or a box's `T`). None when it isn't a candidate.
fn kind_in<'a>(ty: &Type, stage: &Stage<'a>, module: &[String]) -> Option<(Kind, Option<&'a ClassInfo>)> {
    let Stage::Types { k, copy } = stage else { return kind_of(ty).map(|kind| (kind, None)) };
    let Type::Path { segs, .. } = ty else { return None };
    let [s] = segs.as_slice() else { return None };
    // a `Copy` class is copied, and a `shared` one copies its handle: nothing to lend. One with
    // lifetimes would get one more for the reference.
    let class = |n: &str| k.class_in(n, module).filter(|c| !c.is_shared && c.lifetimes == 0 && !copy.contains(&k.decl_key(&c.module, &c.name)));
    match (s.name.as_str(), s.generics.as_deref()) {
        (n, _) if k.class_in(n, module).is_some() => class(n).map(|c| (Kind::Class, Some(c))),
        ("Box", Some([GenericArg::Type(t)])) => Some((Kind::Boxed, t.single_name().and_then(class))),
        // a `Vec` the source analysis kept: a loop over it may only read elements of a class
        ("Vec", Some([GenericArg::Type(_)])) => Some((Kind::Vec, None)),
        _ => None,
    }
}

/// The span start of the parameter `name` that a field chain like `u.address.city` starts from.
fn chain_root(e: &Expr, name: &str) -> Option<u32> {
    let ExprKind::Field { recv, safe: false, .. } = &e.kind else { return None };
    let mut cur: &Expr = recv;
    while let ExprKind::Field { recv, safe: false, .. } = &cur.kind {
        cur = recv;
    }
    (cur.ident() == Some(name)).then_some(cur.span.start)
}

/// `u.address.city`, for a field chain.
fn chain_text(e: &Expr) -> String {
    match &e.kind {
        ExprKind::Field { recv, name, .. } => format!("{}.{}", chain_text(recv), name),
        _ => e.ident().unwrap_or("this").to_string(),
    }
}

/// The element type of a collection a `for` loop can borrow (`Vec<T>`, `HashSet<T>`, ...).
fn loop_elem(t: &Type) -> Option<&Type> {
    let Type::Path { segs, .. } = t else { return None };
    let [s] = segs.as_slice() else { return None };
    match (s.name.as_str(), s.generics.as_deref()) {
        ("Vec" | "VecDeque" | "LinkedList" | "HashSet" | "BTreeSet" | "BinaryHeap", Some([GenericArg::Type(t)])) => Some(t),
        _ => None,
    }
}

/// Does the type hold a reference or a lifetime?
fn has_ref(t: &Type) -> bool {
    match t {
        Type::Ref { .. } | Type::Lifetime(_) => true,
        Type::Path { segs, .. } => segs.iter().flat_map(|s| s.generics.iter().flatten()).any(|g| match g {
            GenericArg::Lifetime(_) => true,
            GenericArg::Type(t) | GenericArg::Binding(_, t) => has_ref(t),
            GenericArg::Const(_) => false,
        }),
        Type::Tuple(v) => v.iter().any(has_ref),
        Type::Array(t, _) | Type::Slice(t) | Type::Option(t) => has_ref(t),
        _ => false,
    }
}

fn kind_of(t: &Type) -> Option<Kind> {
    let Type::Path { segs, .. } = t else { return None };
    let [s] = segs.as_slice() else { return None };
    match (s.name.as_str(), s.generics.as_deref()) {
        ("String", None) => Some(Kind::Str),
        ("Vec", Some([GenericArg::Type(_)])) => Some(Kind::Vec),
        _ => None,
    }
}

/// Std types whose drop only frees memory, once their type arguments' drops do too. Not `Rc` and
/// `Arc`: dropping one changes a count that `Rc::strong_count` and `Rc::try_unwrap` see.
const DROP_FREE: &[&str] = &[
    "i8", "i16", "i32", "i64", "i128", "isize", "u8", "u16", "u32", "u64", "u128", "usize", "f32", "f64", "bool", "char",
    "str", "Int", "Long", "Short", "Byte", "UInt", "ULong", "UShort", "UByte", "Double", "Float", "Boolean", "Char", "Unit",
    "String", "PathBuf", "OsString", "Duration", "Instant", "SystemTime", "Vec", "VecDeque", "LinkedList", "BinaryHeap",
    "HashMap", "HashSet", "BTreeMap", "BTreeSet", "Option", "Result", "Box", "Cell", "RefCell", "Cow",
];

/// A class, enum or type alias declared in Oxie.
struct Decl<'a> {
    generics: &'a Generics,
    /// The types of its fields (and of its variants' fields), or the aliased type.
    holds: Vec<&'a Type>,
    /// A field without a type whose default isn't a literal, so its type is unknown here.
    untyped: bool,
}

/// The types declared in the crate, to tell whether dropping a value only frees memory.
struct Types<'a> {
    decls: HashMap<&'a str, Vec<Decl<'a>>>,
    /// Types that implement `Drop`, in Oxie or in a `rust { }` block.
    drops: HashSet<String>,
}

impl<'a> Types<'a> {
    fn new(files: &[&'a File]) -> Types<'a> {
        fn gather<'a>(items: &'a [Item], t: &mut Types<'a>) {
            for it in items {
                match &it.kind {
                    ItemKind::Type(d) => {
                        let named = d.variants.iter().flat_map(|v| if let VariantFields::Named(fs) = &v.fields { fs.as_slice() } else { &[] });
                        let mut decl = Decl { generics: &d.generics, holds: d.tuple_fields.iter().map(|(_, ty)| ty).collect(), untyped: false };
                        for f in d.fields.iter().chain(named) {
                            match f.ty {
                                Type::Infer => decl.untyped |= !f.default.as_ref().is_some_and(|e| matches!(e.kind, ExprKind::Int(_) | ExprKind::Float(_) | ExprKind::Bool(_) | ExprKind::Char(_) | ExprKind::Str(_) | ExprKind::Template(_) | ExprKind::RawStr(_))),
                                _ => decl.holds.push(&f.ty),
                            }
                        }
                        for v in &d.variants {
                            if let VariantFields::Tuple(ts) = &v.fields {
                                decl.holds.extend(ts);
                            }
                        }
                        if d.supertypes.iter().any(|s| s.last_name() == Some("Drop")) {
                            t.drops.insert(d.name.clone());
                        }
                        t.decls.entry(d.name.as_str()).or_default().push(decl);
                        gather(&d.members, t);
                    }
                    ItemKind::TypeAlias { name, generics, ty } => t.decls.entry(name.as_str()).or_default().push(Decl { generics, holds: vec![ty], untyped: false }),
                    ItemKind::Impl(i) if i.trait_.as_ref().and_then(|tr| tr.last_name()) == Some("Drop") => {
                        if let Some(n) = i.self_ty.last_name() {
                            t.drops.insert(n.to_string());
                        }
                    }
                    // `impl Drop for Name` (or `impl<T> std::ops::Drop for Name<T>`) written in Rust
                    ItemKind::Raw(text) => {
                        let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
                        for (i, _) in flat.match_indices("Drop for ") {
                            if flat[..i].ends_with(|c: char| c.is_alphanumeric() || c == '_') {
                                continue;
                            }
                            let path: String = flat[i + "Drop for ".len()..].chars().take_while(|c| c.is_alphanumeric() || *c == '_' || *c == ':').collect();
                            if let Some(n) = path.rsplit("::").next().filter(|n| !n.is_empty()) {
                                t.drops.insert(n.to_string());
                            }
                        }
                    }
                    ItemKind::Mod { items, .. } | ItemKind::Object { items, .. } => gather(items, t),
                    _ => {}
                }
            }
        }
        let mut t = Types { decls: HashMap::new(), drops: HashSet::new() };
        for f in files {
            gather(&f.items, &mut t);
        }
        t
    }

    /// Does dropping a `t` only free memory? `params` says it for the type parameters in scope.
    /// Types Oxie can't see into (from crates, type parameters, `dyn`, lambdas) may do more.
    /// `visiting` holds the declarations being checked, with their arguments' answers, so a
    /// recursive type (a tree of nodes) is drop-free unless something else in it isn't.
    fn drop_free(&self, t: &Type, params: &HashMap<&str, bool>, visiting: &mut Vec<(&'a str, Vec<bool>)>) -> bool {
        match t {
            Type::Path { segs, .. } => {
                let Some(seg) = segs.last() else { return false };
                if seg.fn_sugar.is_some() {
                    return false;
                }
                if segs.len() == 1 && seg.generics.is_none() {
                    if let Some(&ok) = params.get(seg.name.as_str()) {
                        return ok;
                    }
                }
                let args: Vec<bool> = seg.generics.iter().flatten().filter_map(|a| if let GenericArg::Type(t) = a { Some(self.drop_free(t, params, visiting)) } else { None }).collect();
                let Some((&name, decls)) = self.decls.get_key_value(seg.name.as_str()) else {
                    return DROP_FREE.contains(&seg.name.as_str()) && args.iter().all(|&a| a);
                };
                if self.drops.contains(name) {
                    return false;
                }
                if visiting.iter().any(|(n, a)| *n == name && *a == args) {
                    return true;
                }
                visiting.push((name, args.clone()));
                // with several declarations of the name (in different modules), all of them
                let ok = decls.iter().all(|d| {
                    let names: Vec<&str> = d.generics.params.iter().filter_map(|g| if let GenericParam::Type { name, .. } = g { Some(name.as_str()) } else { None }).collect();
                    let here: HashMap<&str, bool> = names.iter().copied().zip(args.iter().copied()).collect();
                    !d.untyped && names.len() == args.len() && d.holds.iter().all(|h| self.drop_free(h, &here, visiting))
                });
                visiting.pop();
                ok
            }
            Type::Ref { .. } | Type::Ptr { .. } | Type::Never => true,
            Type::Tuple(ts) => ts.iter().all(|t| self.drop_free(t, params, visiting)),
            Type::Array(t, _) | Type::Slice(t) | Type::Option(t) => self.drop_free(t, params, visiting),
            _ => false,
        }
    }

    /// For a parameter `name` of the function `f` whose value may do more than free memory when
    /// dropped: the type that does, and why the parameter stays owned. For a `Vec<T>` that is
    /// `T` (`whole` false), else the parameter's type.
    fn kept_for_drop(&self, ty: &Type, name: &str, f: &FunDecl, whole: bool) -> Option<(Span, String)> {
        let Type::Path { segs, span, .. } = ty else { return None };
        let elem = match segs.last()?.generics.as_deref() {
            _ if whole => ty,
            Some([GenericArg::Type(elem)]) => elem,
            _ => return None,
        };
        // the function's type parameters can be anything, even when one is named like a type
        let params: HashMap<&str, bool> = f.generics.params.iter().filter_map(|g| if let GenericParam::Type { name, .. } = g { Some((name.as_str(), false)) } else { None }).collect();
        if self.drop_free(elem, &params, &mut Vec::new()) {
            return None;
        }
        let at = if let Type::Path { span, .. } = elem { *span } else { *span };
        Some((at, format!("it drops `{}` when it returns, and dropping a `{}` can do more than free memory", name, type_text(elem))))
    }
}

/// The top-level functions of the items, with the module inside the file that declares each.
fn top_level_funs<'a>(items: &'a [Item], module: &[String], out: &mut Vec<(Vec<String>, &'a Item, &'a FunDecl)>) {
    for it in items {
        match &it.kind {
            ItemKind::Fun(f) => out.push((module.to_vec(), it, f)),
            ItemKind::Mod { name, items } => top_level_funs(items, &[module, std::slice::from_ref(name)].concat(), out),
            _ => {}
        }
    }
}

/// Every function body, test, `init` block and constant in the items.
fn each_body(items: &[Item], f: &mut dyn FnMut(&Block)) {
    let fun = |d: &FunDecl, f: &mut dyn FnMut(&Block)| match &d.body {
        Some(FunBody::Block(b)) => f(b),
        Some(FunBody::Expr(e)) => f(&Block { stmts: vec![Stmt::Expr { expr: e.clone(), semi: false }], span: e.span }),
        None => {}
    };
    for it in items {
        match &it.kind {
            ItemKind::Fun(d) => fun(d, f),
            ItemKind::Type(t) => {
                if let Some(b) = &t.init {
                    f(b);
                }
                each_body(&t.members, f);
            }
            ItemKind::Trait(t) => each_body(&t.members, f),
            ItemKind::Impl(i) => each_body(&i.members, f),
            ItemKind::Mod { items, .. } | ItemKind::Object { items, .. } => each_body(items, f),
            ItemKind::Test(t) => f(&t.body),
            ItemKind::Const(c) | ItemKind::Static(c) => {
                if let Some(e) = &c.value {
                    f(&Block { stmts: vec![Stmt::Expr { expr: e.clone(), semi: false }], span: e.span });
                }
            }
            _ => {}
        }
    }
}

/// Does the body declare another variable called `name` (so uses may not be the parameter)?
fn rebinds(body: &Block, name: &str) -> bool {
    let mut found = false;
    let mut check = |p: &Pat| {
        let mut names = Vec::new();
        p.bindings(&mut names);
        if names.iter().any(|n| n == name) {
            found = true;
        }
    };
    fn lets(b: &Block, check: &mut dyn FnMut(&Pat)) {
        for s in &b.stmts {
            let mut s = s;
            while let Stmt::Attributed { stmt, .. } = s {
                s = stmt;
            }
            if let Stmt::Let { pat, .. } = s {
                check(pat);
            }
        }
    }
    fn conds(c: &Cond, check: &mut dyn FnMut(&Pat)) {
        match c {
            Cond::Let { pat, .. } => check(pat),
            Cond::Chain(parts) => parts.iter().for_each(|c| conds(c, check)),
            Cond::Expr(_) => {}
        }
    }
    lets(body, &mut check);
    walk_block(body, &mut |e| {
        use ExprKind::*;
        match &e.kind {
            Block(b) | Unsafe(b) | ConstBlock(b) | LabeledBlock { block: b, .. } | Async { block: b, .. } | Loop { body: b, .. } | DoWhile { body: b, .. } => lets(b, &mut check),
            While { cond, body, .. } => {
                conds(cond, &mut check);
                lets(body, &mut check);
            }
            If { cond, then, .. } => {
                conds(cond, &mut check);
                lets(then, &mut check);
            }
            For { pat, body, .. } => {
                check(pat);
                lets(body, &mut check);
            }
            Lambda { params, .. } => params.iter().for_each(|p| check(&p.pat)),
            When { binding, arms, .. } => {
                if let Some(p) = binding {
                    check(p);
                }
                for a in arms {
                    for c in &a.conds {
                        if let WhenCond::Pat(p) = c {
                            check(p);
                        }
                    }
                }
            }
            TryCatch { body, catch } => {
                lets(body, &mut check);
                if let Some(c) = catch {
                    check(&c.binding);
                    lets(&c.body, &mut check);
                }
            }
            _ => {}
        }
        true
    });
    found
}

/// A parameter that may be borrowed: its function (and the index of the file declaring it),
/// position, kind and name, and the function's body.
struct Cand<'a> {
    file: usize,
    fun: &'a FunDecl,
    index: usize,
    kind: Kind,
    name: &'a str,
    /// Its type (for a loop variable, the element type).
    ty: Type,
    /// The module of the function.
    module: Vec<String>,
    body: Block,
    /// The class of the value a class or `Box` parameter lends, when it is an Oxie class.
    class: Option<&'a ClassInfo>,
    /// For a value that may do more than free memory when dropped (a `Vec`'s elements, a class
    /// or `Box`), which keeps it owned whatever the body does: the type that does and why.
    drops: Option<(Span, String)>,
}

/// The (function, parameter index) pairs that can be borrowed, found as a greatest fixpoint.
struct Analysis<'a> {
    decls: HashMap<&'a str, &'a FunDecl>,
    cands: Vec<Cand<'a>>,
    alive: HashSet<(String, usize)>,
    stage: Stage<'a>,
}

impl<'a> Analysis<'a> {
    /// The candidates of `stage` in `files` (each with its module).
    fn new(files: &[(&'a [String], &'a File)], stage: Stage<'a>) -> Analysis<'a> {
        // (file index, module, item, function)
        let mut funs = Vec::new();
        for (i, (module, f)) in files.iter().enumerate() {
            let mut here = Vec::new();
            top_level_funs(&f.items, module, &mut here);
            funs.extend(here.into_iter().map(|(m, it, d)| (i, m, it, d)));
        }
        let files: Vec<&File> = files.iter().map(|(_, f)| *f).collect();
        let mut count: HashMap<&str, usize> = HashMap::new();
        for (.., f) in &funs {
            *count.entry(f.name.as_str()).or_default() += 1;
        }
        let decls: HashMap<&str, &FunDecl> = funs.iter().filter(|(.., f)| count[f.name.as_str()] == 1).map(|(.., f)| (f.name.as_str(), *f)).collect();

        // functions used as values (`xs.map(greet)`) or called through a path (`util.greet(x)`, whose
        // arguments this analysis doesn't follow) keep their signature
        let mut as_value: HashSet<String> = HashSet::new();
        for f in &files {
            each_body(&f.items, &mut |b| {
                let mut callees = HashSet::new();
                walk_block(b, &mut |e| {
                    if let ExprKind::Call { callee, .. } = &e.kind {
                        callees.insert(callee.span.start);
                    }
                    if let ExprKind::Path { segs, .. } = &e.kind {
                        if segs.len() > 1 || !callees.contains(&e.span.start) {
                            as_value.insert(segs.last().cloned().unwrap_or_default());
                        }
                    }
                    true
                });
            });
        }

        let types = Types::new(&files);
        let mut cands = Vec::new();
        let mut alive = HashSet::new();
        for (file, module, it, f) in &funs {
            let eligible = it.vis != Vis::Public
                && decls.contains_key(f.name.as_str())
                && !as_value.contains(&f.name)
                && f.name != "main"
                && f.receiver.is_none()
                && !f.mods.is_async
                && !f.mods.is_const
                && f.mods.abi.is_none();
            let body = match &f.body {
                Some(FunBody::Block(b)) if eligible => b.clone(),
                Some(FunBody::Expr(e)) if eligible => Block { stmts: vec![Stmt::Expr { expr: e.clone(), semi: false }], span: e.span },
                _ => continue,
            };
            // with one more reference parameter, a result that borrows would borrow from it too
            if matches!(stage, Stage::Types { .. }) && f.ret.as_ref().is_some_and(has_ref) {
                continue;
            }
            for (i, p) in f.params.iter().enumerate() {
                let Pat::Ident { name, mutable: false, by_ref: false, sub: None, .. } = &p.pat else { continue };
                let Some((kind, class)) = kind_in(&p.ty, &stage, module) else { continue };
                if rebinds(&body, name) {
                    continue;
                }
                let drops = match kind {
                    Kind::Str => None,
                    Kind::Vec => types.kept_for_drop(&p.ty, name, f, false),
                    Kind::Class | Kind::Boxed => types.kept_for_drop(&p.ty, name, f, true),
                };
                if drops.is_none() {
                    alive.insert((f.name.clone(), i));
                }
                cands.push(Cand { file: *file, fun: f, index: i, kind, name: name.as_str(), ty: p.ty.clone(), module: module.clone(), body: body.clone(), class, drops });
            }
        }
        let mut a = Analysis { decls, cands, alive, stage };
        loop {
            let dead: Vec<(String, usize)> = a
                .cands
                .iter()
                .filter(|c| a.alive.contains(&(c.fun.name.clone(), c.index)) && a.first_owned_use(c).is_some())
                .map(|c| (c.fun.name.clone(), c.index))
                .collect();
            if dead.is_empty() {
                break;
            }
            for d in dead {
                a.alive.remove(&d);
            }
        }
        a
    }

    /// Does an argument of a call to `callee` only lend the value: its parameter is a `&`
    /// reference, or one that is borrowed itself?
    fn lends(&self, callee: &str, pos: usize, arg: &Arg) -> bool {
        let Some(d) = self.decls.get(callee) else { return false };
        let i = match &arg.name {
            Some(n) => match d.params.iter().position(|p| p.name() == Some(n.as_str())) {
                Some(i) => i,
                None => return false,
            },
            None => pos,
        };
        match d.params.get(i) {
            Some(p) if matches!(p.ty, Type::Ref { mutable: false, .. }) => true,
            Some(_) => self.alive.contains(&(callee.to_string(), i)),
            None => false,
        }
    }

    /// The position of the first use of the parameter that needs the owned value: a use that
    /// isn't a read, or a `move` lambda or async block capturing it (which would hold a borrow
    /// past the call).
    fn first_owned_use(&self, c: &Cand) -> Option<u32> {
        let name = c.name;
        let reads: &[&str] = match c.kind {
            Kind::Str => STR_READS,
            Kind::Vec => VEC_READS,
            Kind::Class | Kind::Boxed => &[],
        };
        // a class or `Box` compared whole would compare a reference with a value
        let whole_compares = matches!(c.kind, Kind::Str | Kind::Vec);
        let mut ok: HashSet<u32> = HashSet::new();
        let mut uses: Vec<u32> = Vec::new();
        let mut captured: Vec<u32> = Vec::new();
        walk_block(&c.body, &mut |e| {
            let is_p = |x: &Expr| x.ident() == Some(name);
            // the parameter, or a field chain of it (`u.address.city`), read where it is
            let read_at = |x: &Expr, whole: bool| if whole && is_p(x) { Some(x.span.start) } else { chain_root(x, name) };
            match &e.kind {
                ExprKind::Path { .. } if is_p(e) => uses.push(e.span.start),
                ExprKind::MethodCall { recv, name: m, .. } if is_p(recv) && (reads.contains(&m.as_str()) || self.reads_value(c, m)) => {
                    ok.insert(recv.span.start);
                }
                ExprKind::MethodCall { recv, name: m, .. } => {
                    if let Some(at) = chain_root(recv, name).filter(|_| self.chain_type(c, recv).is_some_and(|t| self.reads_type(&t, c, m))) {
                        ok.insert(at);
                    }
                }
                // a field chain of a `Copy` type is copied out, whatever it is used for
                ExprKind::Field { .. } => {
                    if let Some(at) = chain_root(e, name).filter(|_| self.chain_type(c, e).is_some_and(|t| self.is_copy(&t))) {
                        ok.insert(at);
                    }
                }
                ExprKind::Template(parts) => {
                    for part in parts {
                        if let TemplatePart::Expr(x, _) = part {
                            ok.extend(read_at(x, true));
                        }
                    }
                }
                ExprKind::Call { callee, args } if callee.ident().is_some_and(|f| PRINTS.contains(&f) && !self.decls.contains_key(f)) => {
                    for a in args {
                        ok.extend(read_at(&a.value, true));
                    }
                }
                ExprKind::Binary(BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge, a, b) => {
                    for x in [a, b] {
                        ok.extend(read_at(x, whole_compares));
                    }
                }
                ExprKind::In { coll, item, .. } => {
                    ok.extend(read_at(coll, whole_compares));
                    ok.extend(chain_root(item, name));
                }
                ExprKind::Index(a, _) => {
                    ok.extend(read_at(a, whole_compares));
                }
                // a loop over the `Vec` (or a collection field of the value) that only reads the
                // elements: it goes over the borrowed collection
                ExprKind::For { pat, iter, body, .. } => {
                    let (at, coll) = if is_p(iter) { (iter.span.start, Some(c.ty.clone())) } else { (chain_root(iter, name).unwrap_or(u32::MAX), self.chain_type(c, iter)) };
                    if (c.kind == Kind::Vec || !is_p(iter)) && coll.as_ref().and_then(loop_elem).is_some_and(|t| self.elems_only_read(c, t, pat, body)) {
                        ok.insert(at);
                    }
                }
                ExprKind::Call { callee, args } => {
                    if let Some(f) = callee.ident() {
                        for (pos, a) in args.iter().enumerate() {
                            if self.lends(f, pos, a) {
                                ok.extend(read_at(&a.value, true));
                            }
                        }
                    }
                }
                ExprKind::Lambda { is_move: true, body, .. } => {
                    walk_expr(body, &mut |x| {
                        if is_p(x) {
                            captured.push(e.span.start);
                        }
                        true
                    });
                }
                ExprKind::Async { block, .. } => {
                    walk_block(block, &mut |x| {
                        if is_p(x) {
                            captured.push(e.span.start);
                        }
                        true
                    });
                }
                _ => {}
            }
            true
        });
        uses.into_iter().filter(|u| !ok.contains(u)).chain(captured).min()
    }

    /// Does the body of a loop over a collection of `elem`s only read the element `pat` binds,
    /// so the loop can go over the borrowed collection? `Copy` elements are bound by value.
    fn elems_only_read(&self, c: &Cand, elem: &Type, pat: &Pat, body: &Block) -> bool {
        let plain = |p: &Pat| matches!(p, Pat::Ident { by_ref: false, mutable: false, sub: None, .. } | Pat::Wild);
        let copy = match &self.stage {
            Stage::Types { copy, .. } => is_copy_simple(elem, copy),
            Stage::Text => is_copy_simple(elem, &HashSet::new()),
        };
        if matches!(pat, Pat::Wild) || copy && (plain(pat) || matches!(pat, Pat::Tuple(ps) if ps.iter().all(plain))) {
            return true;
        }
        let Pat::Ident { name, by_ref: false, mutable: false, sub: None, .. } = pat else { return false };
        let Some((kind, class)) = kind_in(elem, &self.stage, &c.module) else { return false };
        if rebinds(body, name) {
            return false;
        }
        let elem = Cand { file: c.file, fun: c.fun, index: c.index, kind, name, ty: elem.clone(), module: c.module.clone(), body: body.clone(), class, drops: None };
        self.first_owned_use(&elem).is_none()
    }

    /// Does calling `m` on a class or `Box` parameter only read it: a method of its class that
    /// takes `&self`, or a read every class has? For a `Box` not `clone`, which copies the box.
    fn reads_value(&self, c: &Cand, m: &str) -> bool {
        let own = c.class.and_then(|cl| cl.methods.get(m)).is_some_and(|mi| mi.recv == Recv::Ref);
        match c.kind {
            Kind::Class => own || CLASS_READS.contains(&m),
            Kind::Boxed => own || matches!(m, "to_string" | "fmt"),
            Kind::Str | Kind::Vec => false,
        }
    }

    /// The type of a field chain of a class or `Box` parameter (`u.address.city`), when the
    /// classes on the way say.
    fn chain_type(&self, c: &Cand, e: &Expr) -> Option<Type> {
        let Stage::Types { k, .. } = &self.stage else { return None };
        let mut fields = Vec::new();
        let mut cur = e;
        while let ExprKind::Field { recv, name, .. } = &cur.kind {
            fields.push(name.as_str());
            cur = recv;
        }
        let mut class = c.class?;
        let mut ty: Option<Type> = None;
        for f in fields.iter().rev() {
            if let Some(t) = &ty {
                class = k.class_in(t.single_name()?, &class.module)?;
            }
            let info = class.fields.iter().find(|x| x.name == *f)?;
            ty = Some(if info.ty == Type::Infer { info.default.as_ref().and_then(infer_field_type)? } else { info.ty.clone() });
        }
        ty
    }

    fn is_copy(&self, t: &Type) -> bool {
        matches!(&self.stage, Stage::Types { copy, .. } if is_copy_simple(t, copy))
    }

    /// Does calling `m` on a field of type `t` (of the candidate's value) only read it?
    fn reads_type(&self, t: &Type, c: &Cand, m: &str) -> bool {
        let Stage::Types { k, .. } = &self.stage else { return false };
        if self.is_copy(t) {
            return !MUTATING_METHODS.contains(&m);
        }
        match t.single_name() {
            Some("String") => m == "clone" || STR_READS.contains(&m),
            Some("Vec") => m == "clone" || VEC_READS.contains(&m),
            Some(n) => c.class.and_then(|cl| k.class_in(n, &cl.module)).is_some_and(|cl| CLASS_READS.contains(&m) || cl.methods.get(m).is_some_and(|mi| mi.recv == Recv::Ref)),
            None => false,
        }
    }

    /// Why a candidate stays owned: the first use that needs the owned value, and what that use
    /// does ("it returns `v`"), else the elements' drop. None when it is borrowed.
    fn kept_why(&self, c: &Cand) -> Option<(Span, String)> {
        match self.first_owned_use(c) {
            Some(at) => Some(kept_use(c, at)),
            None => c.drops.clone(),
        }
    }
}

/// What happened to a `String` or `Vec<T>` parameter that could be borrowed, and why.
#[derive(Clone, Debug)]
pub struct ParamNote {
    /// The index of the file declaring the function.
    pub file: usize,
    pub fun: String,
    /// The function's name in that file (methods and local functions may share the name).
    pub fun_span: Span,
    pub index: usize,
    /// Lent as `&str` / `&[T]` (else kept owned).
    pub borrowed: bool,
    /// "it only reads `s`", "it returns `v`"
    pub why: String,
    /// The use that keeps it owned, or the element type of a `Vec` kept for its drop.
    pub cause: Option<Span>,
}

/// Why the top-level function `fun` keeps its `String` or `Vec` parameter number `index`
/// instead of borrowing it: the index of the file declaring it, the first use that needs the
/// owned value (or the element type, for a `Vec` kept for its drop), and why ("it returns
/// `v`"). None when the parameter is borrowed or isn't a candidate.
pub fn kept_reason(files: &[&File], fun: &str, index: usize) -> Option<(usize, Span, String)> {
    let views: Vec<(&[String], &File)> = files.iter().map(|f| (&[][..], *f)).collect();
    let a = Analysis::new(&views, Stage::Text);
    let c = a.cands.iter().find(|c| c.fun.name == fun && c.index == index)?;
    let (span, why) = a.kept_why(c)?;
    Some((c.file, span, why))
}

/// The use of the candidate's parameter at `at` that needs the owned value, as a span and a
/// clause ("it returns `v`").
fn kept_use(c: &Cand, at: u32) -> (Span, String) {
    let name = c.name;
    let is_at = |x: &Expr| x.span.start == at && x.ident() == Some(name);
    let mut what: Option<String> = None;
    for (i, s) in c.body.stmts.iter().enumerate() {
        match s {
            Stmt::Expr { expr, semi: false } if i + 1 == c.body.stmts.len() && is_at(expr) => what = Some(format!("it returns `{}`", name)),
            Stmt::Let { init: Some(x), .. } if is_at(x) => what = Some(format!("it stores `{}` in a variable", name)),
            _ => {}
        }
    }
    walk_block(&c.body, &mut |e| {
        if what.is_some() {
            return false;
        }
        what = match &e.kind {
            ExprKind::Lambda { is_move: true, .. } | ExprKind::Async { .. } if e.span.start == at => Some(format!("a `move` lambda captures `{}`", name)),
            ExprKind::Return(Some(x)) if is_at(x) => Some(format!("it returns `{}`", name)),
            ExprKind::For { iter, .. } if is_at(iter) => Some(format!("a loop takes the elements out of `{}`", name)),
            ExprKind::Call { callee, args } if args.iter().any(|a| is_at(&a.value)) => match callee.ident() {
                Some(f) => Some(format!("it passes `{}` to `{}`, which takes it by value", name, f)),
                None => Some(format!("it passes `{}` to a function that takes it by value", name)),
            },
            ExprKind::MethodCall { recv, name: m, .. } if is_at(recv) => match c.class.and_then(|cl| cl.methods.get(m)) {
                Some(mi) if mi.recv == Recv::RefMut => Some(format!("it calls `.{}()` on `{}`, which changes it", m, name)),
                _ => Some(format!("it calls `.{}()` on `{}`, which needs the value itself", m, name)),
            },
            // a field chain of the parameter: changed, given a method that may change it, or taken out
            ExprKind::Assign(_, target, _) if chain_root(target, name) == Some(at) => Some(format!("it changes `{}`", chain_text(target))),
            ExprKind::MethodCall { recv, name: m, .. } if chain_root(recv, name) == Some(at) => Some(format!("it calls `.{}()` on `{}`", m, chain_text(recv))),
            ExprKind::Field { .. } if chain_root(e, name) == Some(at) => Some(format!("it uses `{}` by value", chain_text(e))),
            ExprKind::MethodCall { args, name: m, .. } if args.iter().any(|a| is_at(&a.value)) => Some(format!("it passes `{}` to `.{}()`", name, m)),
            ExprKind::Assign(_, _, rhs) if is_at(rhs) => Some(format!("it stores `{}`", name)),
            ExprKind::StructLit { fields, .. } if fields.iter().any(|(_, v)| v.as_ref().is_some_and(is_at)) => Some(format!("it stores `{}` in a struct", name)),
            _ => None,
        };
        true
    });
    let len = if what.as_deref().is_some_and(|w| w.starts_with("a `move`")) { 1 } else { name.len() as u32 };
    (Span { start: at, end: at + len }, what.unwrap_or_else(|| format!("it uses `{}` by value", name)))
}

/// Lends the `String` and `Vec<T>` parameters that functions only read (a `Vec` only when
/// dropping its elements just frees memory), and says what happened to each one that could
/// have been lent.
pub fn borrow_params(files: &mut [&mut File]) -> Vec<ParamNote> {
    let mut with_modules: Vec<(&[String], &mut File)> = files.iter_mut().map(|f| (&[][..], &mut **f)).collect();
    lend(&mut with_modules, Stage::Text)
}

/// Lends the parameters of Oxie classes and `Box<T>` that functions only read, like
/// `borrow_params` does for `String` and `Vec<T>`, and says what happened to each one that could
/// have been lent. Runs after receiver inference (`k`), since reading through a method takes its
/// receiver; `files` come with their modules.
pub fn borrow_class_params(files: &mut [(&[String], &mut File)], k: &Crate) -> Vec<ParamNote> {
    let copy: HashSet<String> = k.derives.iter().filter(|(_, d)| d.iter().any(|x| x == "Copy")).map(|(n, _)| n.clone()).collect();
    lend(files, Stage::Types { k, copy })
}

/// Runs the analysis of `stage` on `files`, changes the type of each parameter it lends, and says
/// what happened to each candidate.
fn lend(files: &mut [(&[String], &mut File)], stage: Stage) -> Vec<ParamNote> {
    // the notes, and the type each lent parameter gets, by (function, parameter index)
    let (notes, lent): (Vec<ParamNote>, HashMap<(String, usize), Type>) = {
        let views: Vec<(&[String], &File)> = files.iter().map(|(m, f)| (*m, &**f)).collect();
        let a = Analysis::new(&views, stage);
        let mut lent = HashMap::new();
        let notes = a
            .cands
            .iter()
            .map(|c| {
                let kept = a.kept_why(c);
                let borrowed = kept.is_none();
                if borrowed {
                    lent.insert((c.fun.name.clone(), c.index), lent_type(&c.fun.params[c.index].ty, c.kind));
                }
                let (cause, why) = match kept {
                    Some((span, why)) => (Some(span), why),
                    None => (None, format!("it only reads `{}`", c.name)),
                };
                ParamNote { file: c.file, fun: c.fun.name.clone(), fun_span: c.fun.name_span, index: c.index, borrowed, why, cause }
            })
            .collect();
        (notes, lent)
    };
    fn apply(items: &mut [Item], lent: &HashMap<(String, usize), Type>) {
        for it in items {
            match &mut it.kind {
                ItemKind::Fun(f) => {
                    for (i, p) in f.params.iter_mut().enumerate() {
                        if let Some(t) = lent.get(&(f.name.clone(), i)) {
                            p.ty = t.clone();
                        }
                    }
                }
                ItemKind::Mod { items, .. } => apply(items, lent),
                _ => {}
            }
        }
    }
    if !lent.is_empty() {
        for (_, f) in files.iter_mut() {
            apply(&mut f.items, &lent);
        }
    }
    notes
}

/// The type a parameter of type `ty` gets when it is lent: `&str`, `&[T]`, `&Class`, or `&T` for
/// a `Box<T>`.
fn lent_type(ty: &Type, kind: Kind) -> Type {
    let arg = || match ty {
        Type::Path { segs, .. } => match segs[0].generics.as_deref() {
            Some([GenericArg::Type(t)]) => t.clone(),
            _ => ty.clone(),
        },
        _ => ty.clone(),
    };
    let inner = match kind {
        Kind::Str => Type::simple("str"),
        Kind::Vec => Type::Slice(Box::new(arg())),
        Kind::Boxed => arg(),
        Kind::Class => ty.clone(),
    };
    Type::Ref { lifetime: None, mutable: false, inner: Box::new(inner) }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reason(src: &str, fun: &str) -> Option<(String, String)> {
        let (file, diags) = crate::parser::parse_file(src);
        assert!(diags.is_empty(), "{:?}", diags);
        kept_reason(&[&file], fun, 0).map(|(_, s, w)| (src[s.start as usize..s.end as usize].to_string(), w))
    }

    #[test]
    fn kept_parameters_say_why() {
        let src = r#"
fun returns(v: Vec<i32>): Vec<i32> = v
fun stores(s: String): Holder = Holder(name = s)
fun passes(s: String) = consume(s)
fun consume(s: String) { drop(s) }
fun calls(s: String): Vec<u8> = s.into_bytes()
fun captures(s: String) { std::thread::spawn(move { println(s) }) }
fun reads(s: String): usize = s.len()
fun forwards(s: String): usize = reads(s)
data class Holder(name: String)
"#;
        assert_eq!(reason(src, "returns"), Some(("v".into(), "it returns `v`".into())));
        assert_eq!(reason(src, "stores").map(|r| r.1), Some("it passes `s` to `Holder`, which takes it by value".into()));
        assert_eq!(reason(src, "passes").map(|r| r.1), Some("it passes `s` to `consume`, which takes it by value".into()));
        assert_eq!(reason(src, "calls").map(|r| r.1), Some("it calls `.into_bytes()` on `s`, which needs the value itself".into()));
        assert_eq!(reason(src, "captures").map(|r| r.1), Some("a `move` lambda captures `s`".into()));
        // borrowed, so nothing keeps them
        assert_eq!(reason(src, "reads"), None);
        assert_eq!(reason(src, "forwards"), None);
    }

    #[test]
    fn borrow_params_says_what_it_did() {
        let src = "fun reads(s: String): usize = s.len()\nfun keeps(v: Vec<i32>): Vec<i32> = v\npub fun api(s: String): usize = s.len()\n";
        let (mut file, _) = crate::parser::parse_file(src);
        let notes = borrow_params(&mut [&mut file]);
        let note = |f: &str| notes.iter().find(|n| n.fun == f).map(|n| (n.borrowed, n.why.clone(), n.cause.map(|s| &src[s.start as usize..s.end as usize])));
        assert_eq!(note("reads"), Some((true, "it only reads `s`".into(), None)));
        assert_eq!(note("keeps"), Some((false, "it returns `v`".into(), Some("v"))));
        // `pub` signatures are never changed, so there's nothing to say
        assert_eq!(note("api"), None);
    }

    #[test]
    fn vecs_whose_elements_drop_more_than_memory_stay_owned() {
        let src = r#"
import std::sync::MutexGuard
fun guards(xs: Vec<MutexGuard<i32>>) = println(xs.len())
fun generic<T>(xs: Vec<T>): usize = xs.len()
fun shadows<Point>(xs: Vec<Point>): usize = xs.len()
fun noisy(xs: Vec<Noisy>) = println(xs.len())
fun holders(xs: Vec<Holder<Noisy>>) = println(xs.len())
fun logged(xs: Vec<Logged>) = println(xs.len())
fun counted(xs: Vec<Rc<String>>) = println(xs.len())
fun lazies(xs: Vec<Lazy>) = println(xs.len())
fun keeps(xs: Vec<Noisy>): Vec<Noisy> = xs
fun points(xs: Vec<Point>) = println(xs.len())
fun nested(xs: Vec<Vec<String?>>) = println(xs.len())
fun trees(xs: Vec<Tree>) = println(xs.len())
fun boxes(xs: Vec<Holder<Point>>) = println(xs.len())
fun refs(xs: Vec<&Noisy>) = println(xs.len())
class Noisy(val n: i32)
impl Drop for Noisy { mut fun drop() {} }
class Holder<T>(val inner: T)
class Logged(val id: i32)
rust {
    impl Drop for Logged { fn drop(&mut self) {} }
}
class Lazy {
    var items = vec[1]
}
data class Point(val x: i32, val y: i32)
class Tree(val children: Vec<Tree>, val label: String?) {
    var depth = 0
}
"#;
        let (mut file, diags) = crate::parser::parse_file(src);
        assert!(diags.is_empty(), "{:?}", diags);
        let notes = borrow_params(&mut [&mut file]);
        let note = |f: &str| notes.iter().find(|n| n.fun == f).map(|n| (n.borrowed, n.why.clone(), n.cause.map(|s| src[s.start as usize..s.end as usize].to_string())));
        let dropped = |ty: &str| Some((false, format!("it drops `xs` when it returns, and dropping a `{}` can do more than free memory", ty), Some(ty.to_string())));
        assert_eq!(note("guards"), dropped("MutexGuard<i32>"));
        assert_eq!(note("generic"), dropped("T"));
        // a type parameter, not the class
        assert_eq!(note("shadows"), dropped("Point"));
        assert_eq!(note("noisy"), dropped("Noisy"));
        assert_eq!(note("holders"), dropped("Holder<Noisy>"));
        // `impl Drop` in a `rust { }` block
        assert_eq!(note("logged"), dropped("Logged"));
        // dropping an `Rc` changes its count
        assert_eq!(note("counted"), dropped("Rc<String>"));
        // a field whose type Oxie infers from a call
        assert_eq!(note("lazies"), dropped("Lazy"));
        // what the body does comes first
        assert_eq!(note("keeps"), Some((false, "it returns `xs`".into(), Some("xs".into()))));
        let lent = Some((true, "it only reads `xs`".to_string(), None));
        for f in ["points", "nested", "trees", "boxes", "refs"] {
            assert_eq!(note(f), lent, "{}", f);
        }
    }

    #[test]
    fn class_and_box_parameters_that_are_only_read_are_lent() {
        let src = r#"
data class User(val name: String, val age: i32) {
    fun greeting(): String = "hi $name"
}
class Node(val label: String, var hits: i32 = 0) {
    fun bump() { hits += 1 }
}
class Noisy(val n: i32)
impl Drop for Noisy { mut fun drop() {} }
data class Point(val x: i32, val y: i32)
shared class Account(var balance: i32 = 0)
fun reads(u: User): String = "${u.name.len()} ${u.age > 3} ${u.greeting()}"
fun boxed(b: Box<Node>): String = b.label.clone()
fun compares(u: User, v: User): Boolean = u == v
fun changes(n: Node) { n.bump() }
fun takes(u: User): String = u.name
fun noisy(b: Box<Noisy>) = println(b.n)
fun copied(p: Point): i32 = p.x
fun handle(a: Account) = println(a.balance)
"#;
        let (mut file, diags) = crate::parser::parse_file(src);
        assert!(diags.is_empty(), "{:?}", diags);
        let module: Vec<String> = Vec::new();
        let k = {
            let units = [crate::sema::SourceUnit { module: module.clone(), file: &file }];
            crate::sema::collect(&units, "t", HashSet::new(), false)
        };
        let notes = borrow_class_params(&mut [(&module[..], &mut file)], &k);
        let note = |f: &str, i: usize| notes.iter().find(|n| n.fun == f && n.index == i).map(|n| (n.borrowed, n.why.clone()));
        let lent = |p: &str| Some((true, format!("it only reads `{}`", p)));
        assert_eq!(note("reads", 0), lent("u"));
        assert_eq!(note("boxed", 0), lent("b"));
        // `&User == User` doesn't compile
        assert_eq!(note("compares", 0), Some((false, "it uses `u` by value".into())));
        assert_eq!(note("changes", 0), Some((false, "it calls `.bump()` on `n`, which changes it".into())));
        assert_eq!(note("takes", 0), Some((false, "it uses `u.name` by value".into())));
        assert_eq!(note("noisy", 0), Some((false, "it drops `b` when it returns, and dropping a `Box<Noisy>` can do more than free memory".into())));
        // a `Copy` class is copied and a `shared` one copies its handle: nothing to lend
        assert_eq!(note("copied", 0), None);
        assert_eq!(note("handle", 0), None);
        let ty = |f: &str| file.items.iter().find_map(|it| if let ItemKind::Fun(d) = &it.kind { (d.name == f).then(|| type_text(&d.params[0].ty)) } else { None });
        assert_eq!((ty("reads"), ty("boxed"), ty("takes")), (Some("&User".into()), Some("&Node".into()), Some("User".into())));
    }
}
