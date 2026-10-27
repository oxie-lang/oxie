//! Borrowed parameters: a parameter is borrowed unless it says `own`. A `String`, list, map, set,
//! `PathBuf`, `Box<T>` or Oxie class parameter (of a class that is neither `Copy` nor `shared`)
//! becomes `&str`, `&[T]`, `&HashMap<K, V>`, `&Path` or `&T`, and every argument for it is lent,
//! so callers keep their value: `greet(name)` lends `name`. `own name: String` takes it instead.
//!
//! The signature says it, not the body. A function that keeps a borrowed parameter (returns it,
//! stores it, gives it to a parameter that takes it) or changes it is an error with fixes
//! (`check_kept`); text that it keeps is copied instead, as any `&str` is (`names.push(name)`).
//!
//! The text kinds are lent first (`borrow_params`), then, after receiver inference, classes and
//! boxes (`borrow_class_params`) and the parameters of methods (`borrow_method_params`). An
//! interface's methods, their implementations and operators keep the signatures Rust gives them.

use std::collections::{HashMap, HashSet};

use crate::ast::*;
use crate::diag::{Diag, Span};
use crate::lower::infer_field_type;
use crate::sema::{is_copy_simple, ClassInfo, Crate, Recv, MUTATING_METHODS};
use crate::visit::{walk_block, walk_expr};

/// Methods that only read a `String` and exist on `str` with the same meaning.
pub(crate) const STR_READS: &[&str] = &[
    "len", "is_empty", "contains", "starts_with", "ends_with", "chars", "char_indices", "bytes", "lines", "split",
    "splitn", "rsplit", "rsplitn", "split_whitespace", "split_once", "rsplit_once", "trim", "trim_start", "trim_end",
    "strip_prefix", "strip_suffix", "to_uppercase", "to_lowercase", "to_string", "to_owned", "find", "rfind", "replace",
    "parse", "repeat", "as_bytes", "get", "matches", "eq_ignore_ascii_case", "is_char_boundary", "as_str",
];

/// Methods that only read a `Vec<T>` and exist on slices (including Oxie's list operations).
pub(crate) const VEC_READS: &[&str] = &[
    "len", "is_empty", "contains", "starts_with", "ends_with", "iter", "first", "last", "get", "to_vec", "to_owned",
    "join", "concat", "windows", "chunks", "binary_search", "split_first", "split_last", "map", "map_indexed",
    "filter", "filter_not", "filter_map", "flat_map", "for_each", "for_each_indexed", "find", "find_last",
    "index_of_first", "any", "all", "none", "count", "sum", "sum_of", "max_of", "min_of", "max_by_key", "min_by_key",
    "sorted", "sorted_descending", "sorted_by", "sorted_by_descending", "reversed", "distinct", "distinct_by",
    "chunked", "group_by", "associate_by", "partition", "join_to_string", "join_with", "is_not_empty", "as_slice",
];

/// Methods that only read a map or set and exist on a reference to one.
const COLL_READS: &[&str] = &[
    "len", "is_empty", "contains_key", "contains", "get", "get_key_value", "keys", "values", "iter", "first_key_value",
    "last_key_value", "range", "first", "last", "is_subset", "is_superset", "is_disjoint", "union", "intersection",
    "difference", "symmetric_difference",
];

/// Methods that only read a `PathBuf` and exist on `Path` with the same meaning.
const PATH_READS: &[&str] = &[
    "exists", "try_exists", "is_file", "is_dir", "is_symlink", "is_absolute", "is_relative", "has_root", "display", "join",
    "parent", "ancestors", "file_name", "file_stem", "extension", "to_str", "to_string_lossy", "to_path_buf", "to_owned",
    "starts_with", "ends_with", "with_extension", "with_file_name", "components", "iter", "metadata", "symlink_metadata",
    "read_dir", "read_link", "canonicalize", "as_os_str", "strip_prefix", "as_path",
];

/// Formatting macros, which take their arguments by reference.
const PRINTS: &[&str] = &["println", "print", "eprintln", "eprint", "format", "panic", "assert", "assert_eq", "assert_ne", "debug_assert", "debug_assert_eq", "debug_assert_ne"];

/// Methods that only read a value of any class: derived and std ones, and a data class's `copy`.
const CLASS_READS: &[&str] = &["clone", "to_string", "eq", "ne", "cmp", "partial_cmp", "hash", "fmt", "copy"];

/// Methods that put their arguments into the collection or object they are called on.
const STORING: &[&str] = &["push", "push_back", "push_front", "insert", "add", "set", "put", "send", "replace"];

#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Str,
    Vec,
    /// a std map or set (`HashMap`, `BTreeSet`, ...), lent as `&T`
    Coll,
    /// `PathBuf`, lent as `&Path`
    PathBuf,
    /// an Oxie class that is neither `Copy` nor `shared`, lent as `&T`
    Class,
    /// `Box<T>`, lent as `&T`
    Boxed,
}

/// What the pass knows. Before receiver inference, the source, which tells the `String`,
/// `Vec<T>`, map, set and `PathBuf` parameters of top-level functions (`Text`); after it, the
/// crate's classes (`Types`), which tell their class and `Box<T>` parameters. `Methods` looks at
/// the methods, all of whose kinds of parameters are lent, and at everything that was lent, to
/// check what the bodies do with it.
enum Stage<'a> {
    Text,
    Types { k: &'a Crate, copy: HashSet<String> },
    Methods(Methods<'a>),
}

/// The method stage: the crate and its `Copy` types.
struct Methods<'a> {
    k: &'a Crate,
    copy: HashSet<String>,
}

impl<'a> Stage<'a> {
    /// The crate and its `Copy` types, after receiver inference.
    fn types(&self) -> Option<(&'a Crate, &HashSet<String>)> {
        match self {
            Stage::Text => None,
            Stage::Types { k, copy } => Some((k, copy)),
            Stage::Methods(m) => Some((m.k, &m.copy)),
        }
    }
}

/// What a parameter of type `ty`, in a function of `module`, is at this stage, and the class of
/// the value it lends (the class, or a box's `T`). None when it isn't a candidate.
fn kind_in<'a>(ty: &Type, stage: &Stage<'a>, module: &[String]) -> Option<(Kind, Option<&'a ClassInfo>)> {
    let Some((k, copy)) = stage.types() else { return kind_of(ty).map(|kind| (kind, None)) };
    if let (Stage::Methods(_), Some(kind)) = (stage, kind_of(ty)) {
        return Some((kind, None));
    }
    let Type::Path { segs, .. } = ty else { return None };
    let [s] = segs.as_slice() else { return None };
    // a `Copy` class is copied, and a `shared` one copies its handle: nothing to lend. One with
    // lifetimes would get one more for the reference.
    let class = |n: &str| k.class_in(n, module).filter(|c| !c.is_shared && c.lifetimes == 0 && !copy.contains(&k.decl_key(&c.module, &c.name)));
    match (s.name.as_str(), s.generics.as_deref()) {
        (n, _) if k.class_in(n, module).is_some() => class(n).map(|c| (Kind::Class, Some(c))),
        // a boxed function is a function value, which a call takes
        ("Box", Some([GenericArg::Type(t)])) if !is_fn_type(t) => Some((Kind::Boxed, t.single_name().and_then(class))),
        // a list, as the element of a loop over a list of lists
        ("Vec", Some([GenericArg::Type(_)])) => Some((Kind::Vec, None)),
        _ => None,
    }
}

/// A function type: `(A) -> R`, `fn(A) -> R`, or a trait object or `impl` of `Fn`, `FnMut` or
/// `FnOnce`.
fn is_fn_type(t: &Type) -> bool {
    let is_fn_trait = |b: &Type| matches!(b, Type::Path { segs, .. } if segs.last().is_some_and(|s| matches!(s.name.as_str(), "Fn" | "FnMut" | "FnOnce")));
    match t {
        Type::Fn { .. } | Type::FnPtr { .. } => true,
        Type::Dyn(bounds) | Type::Impl(bounds) | Type::Bounds(bounds) => bounds.iter().any(is_fn_trait),
        _ => is_fn_trait(t),
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

/// Does `text` (Rust) contain `word` as a whole identifier?
fn has_word(text: &str, word: &str) -> bool {
    let ident = |c: char| c.is_alphanumeric() || c == '_';
    text.match_indices(word).any(|(i, _)| {
        let before = text[..i].chars().next_back();
        let after = text[i + word.len()..].chars().next();
        !before.is_some_and(ident) && !after.is_some_and(ident)
    })
}

/// `u.address.city`, for a field chain.
fn chain_text(e: &Expr) -> String {
    match &e.kind {
        ExprKind::Field { recv, name, .. } => format!("{}.{}", chain_text(recv), name),
        _ => e.ident().unwrap_or("this").to_string(),
    }
}

/// The operands of a `+` chain: `a + b + c` → `[a, b, c]`.
fn concat_operands<'e>(e: &'e Expr, out: &mut Vec<&'e Expr>) {
    match &e.kind {
        ExprKind::Binary(BinOp::Add, l, r) => {
            concat_operands(l, out);
            out.push(r);
        }
        _ => out.push(e),
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
        ("HashMap" | "BTreeMap", Some([GenericArg::Type(_), GenericArg::Type(_)])) | ("HashSet" | "BTreeSet", Some([GenericArg::Type(_)])) => Some(Kind::Coll),
        ("PathBuf", None) => Some(Kind::PathBuf),
        _ => None,
    }
}

/// A top-level function or a method, with the module inside the file that declares it.
#[derive(Clone)]
struct Fun<'a> {
    file: usize,
    module: Vec<String>,
    decl: &'a FunDecl,
    /// For a method: its class or enum (by name), whether that type is `pub` (so its members are
    /// by default), and whether its parameters are fixed: it implements an interface method or an
    /// operator.
    owner: Option<(&'a str, bool, bool)>,
}

impl Fun<'_> {
    /// For a method, its class or enum with the module declaring it (`a::Counter`).
    fn owner_key(&self) -> Option<String> {
        self.owner.map(|(o, ..)| crate::sema::scoped_key(&self.module, o))
    }
}

/// The top-level functions of the items.
fn top_level_funs<'a>(file: usize, items: &'a [Item], module: &[String], out: &mut Vec<Fun<'a>>) {
    for it in items {
        match &it.kind {
            ItemKind::Fun(f) => out.push(Fun { file, module: module.to_vec(), decl: f, owner: None }),
            ItemKind::Mod { name, items } | ItemKind::Object { name, items } => top_level_funs(file, items, &[module, std::slice::from_ref(name)].concat(), out),
            _ => {}
        }
    }
}

/// The methods of the classes and enums declared in the items: in their bodies, their companion
/// objects and their `impl` blocks.
fn methods<'a>(file: usize, items: &'a [Item], module: &[String], k: &Crate, out: &mut Vec<Fun<'a>>) {
    // a method named like one of an interface the class implements is that interface's
    let of_iface = |supers: &[Type], name: &str| {
        supers.iter().filter_map(|t| t.last_name()).any(|n| k.traits.get(n).is_some_and(|t| t.methods.contains_key(name)) || crate::lower::std_trait_of_method(name).contains(&n))
    };
    fn members<'a>(file: usize, items: &'a [Item], module: &[String], owner: (&'a str, bool), fixed: &dyn Fn(&FunDecl) -> bool, out: &mut Vec<Fun<'a>>) {
        for it in items {
            match &it.kind {
                ItemKind::Fun(f) => {
                    let fixed = fixed(f) || f.mods.is_override || f.mods.is_operator;
                    out.push(Fun { file, module: module.to_vec(), decl: f, owner: Some((owner.0, owner.1, fixed)) });
                }
                ItemKind::Object { items, .. } => members(file, items, module, owner, fixed, out),
                _ => {}
            }
        }
    }
    for it in items {
        match &it.kind {
            ItemKind::Type(t) if k.class_in(&t.name, module).is_some() || k.enum_in(&t.name, module).is_some() => {
                members(file, &t.members, module, (&t.name, it.vis == Vis::Public), &|f| of_iface(&t.supertypes, &f.name), out)
            }
            ItemKind::Impl(i) => {
                let Some(name) = i.self_ty.last_name().filter(|n| k.class_in(n, module).is_some() || k.enum_in(n, module).is_some()) else { continue };
                // `impl Trait for Name`: the trait's parameters
                let trait_impl = i.trait_.is_some();
                members(file, &i.members, module, (name, false), &|_| trait_impl, out);
            }
            ItemKind::Mod { name, items } | ItemKind::Object { name, items } => methods(file, items, &[module, std::slice::from_ref(name)].concat(), k, out),
            _ => {}
        }
    }
}

/// The names of the functions and methods that Rust written in the items calls (in `rust { }`
/// blocks and macro arguments, which Oxie doesn't lower): `name(` anywhere in that text.
pub(crate) fn raw_calls(items: &[Item], out: &mut HashSet<String>) {
    fn scan(text: &str, out: &mut HashSet<String>) {
        let b = text.as_bytes();
        let mut i = 0;
        let mut prev: Option<(usize, usize)> = None;
        while i < b.len() {
            if b[i].is_ascii_alphabetic() || b[i] == b'_' {
                let start = i;
                while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'_') {
                    i += 1;
                }
                let mut j = i;
                while j < b.len() && b[j].is_ascii_whitespace() {
                    j += 1;
                }
                if j < b.len() && b[j] == b'(' {
                    out.insert(text[start..i].to_string());
                    // `Account::new(..)`: the constructor of `Account`
                    if let Some((ps, pe)) = prev.filter(|(_, pe)| text[*pe..start].trim() == "::") {
                        out.insert(format!("{}::{}", &text[ps..pe], &text[start..i]));
                    }
                }
                prev = Some((start, i));
            } else {
                i += 1;
            }
        }
    }
    fn in_block(b: &Block, out: &mut HashSet<String>) {
        for st in &b.stmts {
            if let Stmt::Item(it) = st {
                raw_calls(std::slice::from_ref(&it.0), out);
            }
        }
    }
    for it in items {
        match &it.kind {
            ItemKind::Raw(text) | ItemKind::Macro(text) => scan(text, out),
            ItemKind::Mod { items, .. } | ItemKind::Object { items, .. } => raw_calls(items, out),
            ItemKind::Type(t) => raw_calls(&t.members, out),
            ItemKind::Trait(t) => raw_calls(&t.members, out),
            ItemKind::Impl(i) => raw_calls(&i.members, out),
            _ => {}
        }
    }
    crate::visit::walk_items(items, &mut |e| {
        match &e.kind {
            ExprKind::Raw(text) | ExprKind::Macro { text, .. } => scan(text, out),
            ExprKind::Block(b) | ExprKind::Unsafe(b) | ExprKind::Loop { body: b, .. } | ExprKind::For { body: b, .. } | ExprKind::While { body: b, .. } | ExprKind::If { then: b, .. } => in_block(b, out),
            _ => {}
        }
        true
    });
    each_body(items, &mut |b| in_block(b, out));
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

/// A lent parameter: its function (and the index of the file declaring it), position, kind,
/// name and declared type, and the function's body.
struct Cand<'a> {
    file: usize,
    fun: &'a FunDecl,
    /// The class or enum whose method the function is (`Fun::owner_key`).
    owner: Option<String>,
    index: usize,
    kind: Kind,
    name: &'a str,
    /// Its declared type (for a loop variable, the element type).
    ty: Type,
    /// The module of the function.
    module: Vec<String>,
    body: Block,
    /// The class of the value a class or `Box` parameter lends, when it is an Oxie class.
    class: Option<&'a ClassInfo>,
}

/// What a function does with a borrowed parameter that a reference can't.
enum Kept {
    /// It changes the value: `xs.push(1)`, `u.age += 1`, `n.bump()` with a `&mut self` method.
    Change { span: Span, why: String },
    /// It keeps the value or a part of it (`what`): returns it, stores it, gives it to a parameter
    /// that takes it, or uses it in a lambda that outlives the call. `copy` is where a `.clone()`
    /// would keep a copy instead; `text`: it keeps the borrowed text itself, which Oxie copies.
    Keep { span: Span, what: String, why: String, copy: Option<u32>, text: bool },
}

impl Kept {
    fn span(&self) -> Span {
        match self {
            Kept::Change { span, .. } | Kept::Keep { span, .. } => *span,
        }
    }

    fn why(&self) -> &str {
        match self {
            Kept::Change { why, .. } | Kept::Keep { why, .. } => why,
        }
    }
}

/// The functions and methods of the crate, to tell what the bodies of those with lent parameters
/// do with them.
struct Analysis<'a> {
    /// The top-level functions, by name (functions of different modules may share one).
    decls: HashMap<&'a str, Vec<&'a FunDecl>>,
    /// The methods of each class or enum (by `Fun::owner_key`), by name.
    owned: HashMap<String, HashMap<&'a str, &'a FunDecl>>,
    /// The names of the classes and enums declared in the files, for a stage without the crate.
    classes: HashSet<&'a str>,
    /// The lent parameters.
    cands: Vec<Cand<'a>>,
    stage: Stage<'a>,
}

/// The parameter of `d` that an argument at `pos` (or named) of a call fills.
fn param_for<'d>(d: &'d FunDecl, pos: usize, arg: &Arg) -> Option<&'d Param> {
    match &arg.name {
        Some(n) => d.params.iter().find(|p| p.name() == Some(n.as_str())),
        None => d.params.get(pos),
    }
}

/// Does the parameter borrow its argument: a `&` reference, or a parameter Oxie lends (a kind it
/// lends, without `own`)?
fn borrows(p: &Param) -> bool {
    matches!(p.ty, Type::Ref { mutable: false, .. }) || p.lent.is_some() || !p.own && !p.context && matches!(p.pat, Pat::Ident { mutable: false, .. }) && kind_of(&p.ty).is_some()
}

/// Does an argument at `pos` (or named) of a call to `d` only lend the value?
fn lends_to(d: &FunDecl, pos: usize, arg: &Arg) -> bool {
    param_for(d, pos, arg).is_some_and(borrows)
}

/// The place an expression names, as written: `u`, `u.address.city`, `xs[..]`.
fn place_text(e: &Expr) -> String {
    match &e.kind {
        ExprKind::Field { recv, name, .. } => format!("{}.{}", place_text(recv), name),
        ExprKind::Index(recv, _) => format!("{}[..]", place_text(recv)),
        ExprKind::Paren(x) => place_text(x),
        _ => e.ident().unwrap_or("this").to_string(),
    }
}

/// The variable at the root of a place: `u` of `u.items[0].name`.
fn place_root(e: &Expr) -> &Expr {
    match &e.kind {
        ExprKind::Field { recv, safe: false, .. } | ExprKind::Index(recv, _) | ExprKind::Paren(recv) => place_root(recv),
        _ => e,
    }
}

/// Calls `f` with the value of every `val`/`var` in the body, in nested blocks too.
fn each_let_init(body: &Block, f: &mut dyn FnMut(&Expr)) {
    fn lets(b: &Block, f: &mut dyn FnMut(&Expr)) {
        for s in &b.stmts {
            let mut s = s;
            while let Stmt::Attributed { stmt, .. } = s {
                s = stmt;
            }
            if let Stmt::Let { init: Some(x), .. } = s {
                f(x);
            }
        }
    }
    lets(body, f);
    walk_block(body, &mut |e| {
        use ExprKind::*;
        match &e.kind {
            Block(b) | Unsafe(b) | ConstBlock(b) | LabeledBlock { block: b, .. } | Async { block: b, .. } | Loop { body: b, .. } | DoWhile { body: b, .. } | While { body: b, .. } | For { body: b, .. } | If { then: b, .. } => lets(b, f),
            TryCatch { body, catch } => {
                lets(body, f);
                if let Some(c) = catch {
                    lets(&c.body, f);
                }
            }
            _ => {}
        }
        true
    });
}

fn capitalized(s: &str) -> String {
    let mut c = s.chars();
    c.next().map(|f| f.to_uppercase().chain(c).collect()).unwrap_or_default()
}

impl<'a> Analysis<'a> {
    /// The functions and methods of `files` (each with its module, and methods only when the
    /// stage knows the classes); the candidates are the parameters Oxie lent.
    fn new(files: &[(&'a [String], &'a File)], stage: Stage<'a>) -> Analysis<'a> {
        let mut funs = Vec::new();
        for (i, (module, f)) in files.iter().enumerate() {
            top_level_funs(i, &f.items, module, &mut funs);
        }
        let mut decls: HashMap<&str, Vec<&FunDecl>> = HashMap::new();
        for f in &funs {
            decls.entry(f.decl.name.as_str()).or_default().push(f.decl);
        }
        let mut owned: HashMap<String, HashMap<&str, &FunDecl>> = HashMap::new();
        if let Some((k, _)) = stage.types() {
            let mut ms = Vec::new();
            for (i, (module, f)) in files.iter().enumerate() {
                methods(i, &f.items, module, k, &mut ms);
            }
            for f in &ms {
                if let Some(owner) = f.owner_key() {
                    owned.entry(owner).or_default().entry(f.decl.name.as_str()).or_insert(f.decl);
                }
            }
            funs.extend(ms);
        }
        let mut cands = Vec::new();
        for fun in &funs {
            let f = fun.decl;
            let body = match &f.body {
                Some(FunBody::Block(b)) => b.clone(),
                Some(FunBody::Expr(e)) => Block { stmts: vec![Stmt::Expr { expr: e.clone(), semi: false }], span: e.span },
                None => continue,
            };
            for (i, p) in f.params.iter().enumerate() {
                let (Some(declared), Some(name)) = (&p.lent, p.name()) else { continue };
                let Some((kind, class)) = kind_in(declared, &stage, &fun.module) else { continue };
                // uses of the name may be another variable's
                if rebinds(&body, name) {
                    continue;
                }
                cands.push(Cand { file: fun.file, fun: f, owner: fun.owner_key(), index: i, kind, name, ty: declared.clone(), module: fun.module.clone(), body: body.clone(), class });
            }
        }
        fn types<'a>(items: &'a [Item], out: &mut HashSet<&'a str>) {
            for it in items {
                match &it.kind {
                    ItemKind::Type(t) => {
                        out.insert(t.name.as_str());
                        types(&t.members, out);
                    }
                    ItemKind::Mod { items, .. } | ItemKind::Object { items, .. } => types(items, out),
                    _ => {}
                }
            }
        }
        let mut classes = HashSet::new();
        for (_, f) in files {
            types(&f.items, &mut classes);
        }
        Analysis { decls, owned, classes, cands, stage }
    }

    /// The functions a call to `callee` from the candidate's function may reach: in a method, a
    /// method of its class of that name when there is one, else the top-level functions.
    fn callees(&self, c: &Cand, callee: &str) -> Vec<&'a FunDecl> {
        if let Some(&d) = c.owner.as_ref().and_then(|o| self.owned.get(o)).and_then(|ms| ms.get(callee)) {
            return vec![d];
        }
        self.decls.get(callee).cloned().unwrap_or_default()
    }

    /// Does an argument of a call to `callee` (from the candidate `c`) only lend the value: every
    /// function the call may reach borrows that parameter?
    fn lends(&self, c: &Cand, callee: &str, pos: usize, arg: &Arg) -> bool {
        let ds = self.callees(c, callee);
        !ds.is_empty() && ds.iter().all(|d| lends_to(d, pos, arg))
    }

    /// Where the parameter is used other than to read it, in order: the uses that aren't reads,
    /// and the lambdas and async blocks that capture it and outlive the call.
    fn owned_uses(&self, c: &Cand) -> Vec<u32> {
        let name = c.name;
        let reads: &[&str] = match c.kind {
            Kind::Str => STR_READS,
            Kind::Vec => VEC_READS,
            Kind::Coll => COLL_READS,
            Kind::PathBuf => PATH_READS,
            Kind::Class | Kind::Boxed => &[],
        };
        // a class, `Box`, map or path compared whole would compare a reference with a value
        let whole_compares = matches!(c.kind, Kind::Str | Kind::Vec);
        // indexing it or looking for an element works through a reference
        let whole_reads = matches!(c.kind, Kind::Str | Kind::Vec | Kind::Coll);
        let mut ok: HashSet<u32> = HashSet::new();
        let mut uses: Vec<u32> = Vec::new();
        let mut captured: Vec<u32> = Vec::new();
        walk_block(&c.body, &mut |e| {
            let is_p = |x: &Expr| x.ident() == Some(name);
            // the parameter, or a field chain of it (`u.address.city`), read where it is
            let read_at = |x: &Expr, whole: bool| if whole && is_p(x) { Some(x.span.start) } else { chain_root(x, name) };
            // a lambda that a constructor stores, or one put into a field or returned: it outlives
            // the call, so what it uses must be owned (`Button(name, { "clicked $name" })`)
            let stored: Vec<&Expr> = match &e.kind {
                ExprKind::Call { callee, args } if matches!(&callee.kind, ExprKind::Path { segs, .. } if segs.last().is_some_and(|n| crate::parser::is_type_like(n))) => {
                    args.iter().map(|a| &a.value).filter(|v| matches!(v.kind, ExprKind::Lambda { .. })).collect()
                }
                ExprKind::Assign(_, target, value) if matches!(target.kind, ExprKind::Field { .. }) && matches!(value.kind, ExprKind::Lambda { .. }) => vec![&**value],
                ExprKind::Return(Some(value)) if matches!(value.kind, ExprKind::Lambda { .. }) => vec![&**value],
                _ => Vec::new(),
            };
            for l in stored {
                walk_expr(l, &mut |x| {
                    if is_p(x) {
                        captured.push(l.span.start);
                    }
                    true
                });
            }
            match &e.kind {
                ExprKind::Path { .. } if is_p(e) => uses.push(e.span.start),
                // `Self { name, .. }` moves the parameter into the struct
                ExprKind::StructLit { fields, .. } if fields.iter().any(|(f, v)| v.is_none() && f == name) => uses.push(e.span.start),
                // a copy of lent text or a lent list is `to_string()` or `to_vec()`, which reads it
                ExprKind::MethodCall { recv, name: m, args, .. } if is_p(recv) && (reads.contains(&m.as_str()) || self.reads_value(c, m) || m == "clone" && args.is_empty() && !matches!(c.kind, Kind::Boxed)) => {
                    ok.insert(recv.span.start);
                }
                ExprKind::MethodCall { recv, name: m, args, .. } => {
                    if let Some(at) = chain_root(recv, name).filter(|_| self.chain_type(c, recv).is_some_and(|t| self.reads_type(&t, c, m))) {
                        ok.insert(at);
                    }
                    // `this.show(name)`: a method of the candidate's class
                    if matches!(recv.kind, ExprKind::SelfValue) && c.owner.as_ref().and_then(|o| self.owned.get(o)).is_some_and(|ms| ms.contains_key(m.as_str())) {
                        for (pos, a) in args.iter().enumerate() {
                            if self.lends(c, m, pos, a) {
                                ok.extend(read_at(&a.value, true));
                            }
                        }
                    }
                    // `Counter.add(c, name)`: a static method of an Oxie class
                    if let Some(&d) = recv.ident().filter(|t| crate::parser::is_type_like(t)).and_then(|t| self.owned.get(&crate::sema::scoped_key(&c.module, t))).and_then(|ms| ms.get(m.as_str())) {
                        for (pos, a) in args.iter().enumerate() {
                            if lends_to(d, pos, a) {
                                ok.extend(read_at(&a.value, true));
                            }
                        }
                    }
                    // `g.greet(name)` with `g` a parameter of an Oxie class: that class's method
                    if let Some(owner) = recv.ident().and_then(|r| c.fun.params.iter().find(|p| p.name() == Some(r))).and_then(|p| {
                        let mut t = &p.ty;
                        while let Type::Ref { inner, .. } = t {
                            t = inner;
                        }
                        t.single_name().map(|n| crate::sema::scoped_key(&c.module, n))
                    }) {
                        if let Some(&d) = self.owned.get(&owner).and_then(|ms| ms.get(m.as_str())) {
                            for (pos, a) in args.iter().enumerate() {
                                if lends_to(d, pos, a) {
                                    ok.extend(read_at(&a.value, true));
                                }
                            }
                        }
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
                // `pred(x)` with a parameter `pred: (&str) -> bool`, which takes a reference
                ExprKind::Call { callee, args } if callee.ident().is_some_and(|f| c.fun.params.iter().any(|p| p.name() == Some(f) && matches!(p.ty, Type::Fn { .. }))) => {
                    let f = callee.ident().unwrap();
                    if let Some(Type::Fn { params, .. }) = c.fun.params.iter().find(|p| p.name() == Some(f)).map(|p| &p.ty) {
                        for (a, t) in args.iter().zip(params) {
                            if matches!(t, Type::Ref { mutable: false, .. }) {
                                ok.extend(read_at(&a.value, true));
                            }
                        }
                    }
                }
                ExprKind::Binary(BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge, a, b) => {
                    for x in [a, b] {
                        ok.extend(read_at(x, whole_compares));
                    }
                }
                ExprKind::In { coll, item, .. } => {
                    ok.extend(read_at(coll, whole_reads));
                    ok.extend(chain_root(item, name));
                }
                // `when (c) { in vowels -> .. }` looks in the collection, as `c in vowels` does
                ExprKind::When { arms, .. } => {
                    for c in arms.iter().flat_map(|a| &a.conds) {
                        if let WhenCond::In { expr, .. } = c {
                            ok.extend(read_at(expr, whole_reads));
                        }
                    }
                }
                ExprKind::Index(a, _) => {
                    ok.extend(read_at(a, whole_reads));
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
                    // `f(x)`, or `module.f(x)` through a path
                    let f = match &callee.kind {
                        ExprKind::Path { segs, .. } => segs.last().map(String::as_str),
                        _ => None,
                    };
                    if let Some(f) = f {
                        for (pos, a) in args.iter().enumerate() {
                            if self.lends(c, f, pos, a) {
                                // `&s` too: a reference to the reference coerces
                                let v = match &a.value.kind {
                                    ExprKind::Unary(UnOp::Ref, x) => x,
                                    _ => &a.value,
                                };
                                ok.extend(read_at(v, true));
                            }
                        }
                    }
                }
                // joining text with `+` formats it: `format!("{}{}", a, b)`
                ExprKind::Binary(BinOp::Add, ..) if c.kind == Kind::Str => {
                    let mut ops = Vec::new();
                    concat_operands(e, &mut ops);
                    ok.extend(ops.into_iter().filter(|o| is_p(o)).map(|o| o.span.start));
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
                // stored by a field written without its value (`Point { x, y }`), or used by Rust
                // that Oxie doesn't read
                ExprKind::StructLit { fields, .. } if fields.iter().any(|(f, v)| v.is_none() && f == name) => uses.push(e.span.start),
                ExprKind::Raw(text) | ExprKind::Macro { text, .. } if has_word(text, name) => uses.push(e.span.start),
                _ => {}
            }
            true
        });
        let mut out: Vec<u32> = uses.into_iter().filter(|u| !ok.contains(u)).chain(captured).collect();
        out.sort();
        out.dedup();
        out
    }

    /// Does the body of a loop over a collection of `elem`s only read the element `pat` binds,
    /// so the loop can go over the borrowed collection? `Copy` elements are bound by value.
    fn elems_only_read(&self, c: &Cand, elem: &Type, pat: &Pat, body: &Block) -> bool {
        let plain = |p: &Pat| matches!(p, Pat::Ident { by_ref: false, mutable: false, sub: None, .. } | Pat::Wild);
        let copy = match &self.stage {
            Stage::Types { copy, .. } | Stage::Methods(Methods { copy, .. }) => is_copy_simple(elem, copy),
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
        let elem = Cand { file: c.file, fun: c.fun, owner: c.owner.clone(), index: c.index, kind, name, ty: elem.clone(), module: c.module.clone(), body: body.clone(), class };
        self.owned_uses(&elem).is_empty()
    }

    /// Does calling `m` on a class or `Box` parameter only read it: a method of its class that
    /// takes `&self`, or a read every class has? For a `Box` not `clone`, which copies the box.
    fn reads_value(&self, c: &Cand, m: &str) -> bool {
        let own = c.class.and_then(|cl| cl.methods.get(m)).is_some_and(|mi| mi.recv == Recv::Ref);
        match c.kind {
            Kind::Class => own || CLASS_READS.contains(&m),
            Kind::Boxed => own || matches!(m, "to_string" | "fmt"),
            Kind::Str | Kind::Vec | Kind::Coll | Kind::PathBuf => false,
        }
    }

    /// The type of a field chain of a class or `Box` parameter (`u.address.city`), when the
    /// classes on the way say.
    fn chain_type(&self, c: &Cand, e: &Expr) -> Option<Type> {
        let (k, _) = self.stage.types()?;
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
        self.stage.types().is_some_and(|(_, copy)| is_copy_simple(t, copy))
    }

    /// Does calling `m` on a field of type `t` (of the candidate's value) only read it?
    fn reads_type(&self, t: &Type, c: &Cand, m: &str) -> bool {
        let Some((k, _)) = self.stage.types() else { return false };
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

    /// Does calling `m` on `recv` (the candidate's parameter, or a place inside it) change it: a
    /// mutating method of a collection, or a `&mut self` method of an Oxie class?
    fn changes(&self, c: &Cand, recv: &Expr, m: &str) -> bool {
        let (reads, class): (&[&str], Option<&ClassInfo>) = if recv.ident() == Some(c.name) {
            let reads: &[&str] = match c.kind {
                Kind::Str => STR_READS,
                Kind::Vec => VEC_READS,
                Kind::Coll => COLL_READS,
                Kind::PathBuf => PATH_READS,
                Kind::Class | Kind::Boxed => &[],
            };
            (reads, c.class)
        } else {
            let t = self.chain_type(c, recv);
            let reads: &[&str] = match t.as_ref().and_then(|t| t.single_name()) {
                Some("String") => STR_READS,
                Some("Vec") => VEC_READS,
                _ => &[],
            };
            let class = t.as_ref().and_then(|t| t.single_name()).and_then(|n| self.stage.types().and_then(|(k, _)| k.class_in(n, &c.module)));
            (reads, class)
        };
        if reads.contains(&m) {
            return false;
        }
        class.and_then(|cl| cl.methods.get(m)).map_or(MUTATING_METHODS.contains(&m), |mi| mi.recv == Recv::RefMut)
    }

    /// The first place where the candidate's function changes the borrowed value: assigns to it
    /// or into it, calls a method that changes it, takes `&mut` of it, gives it to a `&mut`
    /// parameter, or changes the elements in a loop over it.
    fn first_change(&self, c: &Cand) -> Option<Kept> {
        let on_p = |e: &Expr| place_root(e).ident() == Some(c.name);
        let change = |e: &Expr, why: String| Some(Kept::Change { span: e.span, why });
        let mut found = None;
        walk_block(&c.body, &mut |e| {
            if found.is_some() {
                return false;
            }
            found = match &e.kind {
                ExprKind::Assign(_, target, _) if on_p(target) => change(target, format!("it changes `{}`", place_text(target))),
                ExprKind::Unary(UnOp::RefMut, x) if on_p(x) => change(x, format!("it takes `&mut {}`", place_text(x))),
                ExprKind::MethodCall { recv, name: m, safe: false, .. } if on_p(recv) && self.changes(c, recv, m) => change(recv, format!("it calls `.{}()` on `{}`, which changes it", m, place_text(recv))),
                ExprKind::For { pat, iter, body, .. } if on_p(iter) => {
                    let mut names = Vec::new();
                    pat.bindings(&mut names);
                    if names.iter().any(|n| crate::visit::writes_through(body, n, &|_| false)) {
                        change(iter, format!("the loop changes the elements of `{}`", place_text(iter)))
                    } else {
                        None
                    }
                }
                ExprKind::Call { callee, args } => {
                    let f = callee.ident().unwrap_or("");
                    let ds = self.callees(c, f);
                    let changing = |pos: usize, a: &Arg| !ds.is_empty() && ds.iter().all(|d| param_for(d, pos, a).is_some_and(|p| matches!(p.ty, Type::Ref { mutable: true, .. })));
                    args.iter().enumerate().find(|(pos, a)| on_p(&a.value) && changing(*pos, a)).and_then(|(_, a)| change(&a.value, format!("it passes `{}` to `{}`, which changes it", place_text(&a.value), f)))
                }
                _ => None,
            };
            true
        });
        found
    }

    /// What a call to `callee` does with the candidate's value as its argument at `pos` that a
    /// reference can't: an Oxie class's constructor keeps it in a field, and a function (or a
    /// method of the candidate's class) whose parameter there is `own` takes it.
    fn takes(&self, c: &Cand, callee: &Expr, pos: usize, arg: &Arg) -> Option<String> {
        let f = callee.ident()?;
        if crate::parser::is_type_like(f) {
            let class = match self.stage.types() {
                Some((k, _)) => k.class_in(f, &c.module).is_some(),
                None => self.classes.contains(f),
            };
            return class.then(|| format!("`{}`, which keeps it", f));
        }
        let ds = self.callees(c, f);
        let ps: Vec<&Param> = ds.iter().filter_map(|d| param_for(d, pos, arg)).collect();
        if ps.is_empty() || ps.len() != ds.len() || !ps.iter().all(|p| p.own) {
            return None;
        }
        Some(format!("`{}`, which takes it (`own {}`)", f, ps[0].name().unwrap_or("_")))
    }

    /// What the use of the candidate's parameter at `at` keeps: a value it returns, stores or
    /// gives to a parameter that takes it, a field it takes out (`val n = u.name`), or what a
    /// lambda that outlives the call captures. None for a use that may work with the reference
    /// (`val t = name` names the borrowed value; a call Oxie doesn't know), which rustc checks.
    fn kept_at(&self, c: &Cand, at: u32) -> Option<Kept> {
        let name = c.name;
        // the parameter at `at`, or a field chain of it
        let is_use = |x: &Expr| x.span.start == at && (x.ident() == Some(name) || chain_root(x, name) == Some(at));
        // a result that is a reference may borrow from it
        let returns_ref = c.fun.ret.as_ref().is_some_and(has_ref);
        let text = c.kind == Kind::Str;
        let keep = |x: &Expr, why: String| Some(Kept::Keep { span: x.span, what: place_text(x), why, copy: Some(x.span.end), text: text && x.ident().is_some() });
        // the value of a function with an expression body or a declared result
        if let Some(Stmt::Expr { expr, semi: false }) = c.body.stmts.last() {
            if is_use(expr) && !returns_ref && (c.fun.ret.is_some() || matches!(c.fun.body, Some(FunBody::Expr(_)))) {
                return keep(expr, format!("it returns `{}`", place_text(expr)));
            }
        }
        // `val n = u.name` takes the field out (`val t = name` is a name for the borrowed value)
        let mut taken = None;
        each_let_init(&c.body, &mut |init| {
            if taken.is_none() && is_use(init) && init.ident() != Some(name) {
                taken = Some(init.clone());
            }
        });
        if let Some(x) = taken {
            return Some(Kept::Keep { span: x.span, what: place_text(&x), why: format!("it takes `{}` out of `{}`", place_text(&x), name), copy: Some(x.span.end), text: false });
        }
        let mut found = None;
        walk_block(&c.body, &mut |e| {
            if found.is_some() {
                return false;
            }
            found = match &e.kind {
                ExprKind::Lambda { .. } | ExprKind::Async { .. } if e.span.start == at => Some(Kept::Keep { span: Span { start: at, end: at + 1 }, what: name.to_string(), why: format!("a lambda that outlives the call uses `{}`", name), copy: None, text: false }),
                ExprKind::Return(Some(x)) if is_use(x) && !returns_ref => keep(x, format!("it returns `{}`", place_text(x))),
                // a loop over the borrowed list whose elements are of a kind Oxie follows, and which
                // the body doesn't only read (other kinds rustc checks)
                ExprKind::For { iter, .. } if is_use(iter) && iter.ident().is_some() && loop_elem(&c.ty).is_some_and(|t| kind_in(t, &self.stage, &c.module).is_some()) => {
                    Some(Kept::Keep { span: iter.span, what: place_text(iter), why: format!("a loop takes the elements out of `{}`", place_text(iter)), copy: Some(iter.span.end), text: false })
                }
                ExprKind::Call { callee, args } => args.iter().enumerate().find(|(_, a)| is_use(&a.value)).and_then(|(pos, a)| {
                    let to = self.takes(c, callee, pos, a)?;
                    keep(&a.value, format!("it passes `{}` to {}", place_text(&a.value), to))
                }),
                ExprKind::MethodCall { recv, name: m, .. } if is_use(recv) && m.starts_with("into_") => Some(Kept::Keep { span: recv.span, what: place_text(recv), why: format!("it calls `.{}()` on `{}`, which takes the value", m, place_text(recv)), copy: Some(recv.span.end), text: false }),
                ExprKind::MethodCall { name: m, args, .. } if STORING.contains(&m.as_str()) => args.iter().find(|a| is_use(&a.value)).and_then(|a| keep(&a.value, format!("it stores `{}` with `.{}()`", place_text(&a.value), m))),
                ExprKind::Assign(_, target, value) if is_use(value) && !is_use(target) => keep(value, format!("it stores `{}`", place_text(value))),
                ExprKind::StructLit { fields, .. } => fields.iter().find_map(|(f, v)| match v {
                    Some(v) if is_use(v) => keep(v, format!("it stores `{}` in a struct", place_text(v))),
                    None if f == name && e.span.start == at => Some(Kept::Keep { span: e.span, what: name.to_string(), why: format!("it stores `{}` in a struct", name), copy: None, text }),
                    _ => None,
                }),
                _ => None,
            };
            true
        });
        found
    }

    /// Can a `.clone()` of the candidate's value make a copy to keep? Lent text, lists, paths,
    /// maps and sets copy what they lend, and a class must be `Clone`.
    fn copyable(&self, c: &Cand) -> bool {
        match c.kind {
            Kind::Str | Kind::Vec | Kind::Coll | Kind::PathBuf => true,
            Kind::Class => c.class.is_some_and(|cl| self.stage.types().is_some_and(|(k, _)| k.derives.get(&k.decl_key(&cl.module, &cl.name)).is_some_and(|d| d.iter().any(|x| x == "Clone")))),
            Kind::Boxed => false,
        }
    }

    /// The error for a borrowed parameter that the candidate's function changes or keeps, with
    /// the fixes: take it with `own`, or change the caller's value (`&mut`) or keep a copy.
    fn kept_error(&self, c: &Cand, kept: Kept) -> Diag {
        let p = &c.fun.params[c.index];
        let at_name = match &p.pat {
            Pat::Ident { span, .. } => span.start,
            _ => p.span.start,
        };
        let own = (format!("take `{}` (`own {}`)", c.name, c.name), vec![(Span::empty_at(at_name), "own ".to_string())]);
        match kept {
            Kept::Change { span, why } => {
                let mut d = Diag::error(span, format!("`{}` is borrowed, so `{}` can't change it", c.name, c.fun.name)).with_help(format!(
                    "{}. A parameter is borrowed unless it says `own`: to change the caller's value, ask for a mutable reference (`&mut`); to change a value of its own, take it with `own`.",
                    capitalized(&why)
                ));
                if let Type::Path { span: ty, .. } = &c.ty {
                    if *ty != Span::default() {
                        d = d.with_fix(format!("change the caller's `{}` (`&mut`)", c.name), vec![(Span::empty_at(ty.start), "&mut ".to_string())]);
                    }
                }
                d.with_fix(own.0, own.1)
            }
            Kept::Keep { span, what, why, copy, .. } => {
                let message = if what == c.name { format!("`{}` can't keep `{}`: it is borrowed", c.fun.name, what) } else { format!("`{}` can't keep `{}`: `{}` is borrowed", c.fun.name, what, c.name) };
                let mut d = Diag::error(span, message)
                    .with_help(format!("{}. A parameter is borrowed unless it says `own`: a function that keeps one takes it, and its callers give theirs up.", capitalized(&why)))
                    .with_fix(own.0, own.1);
                if let Some(at) = copy.filter(|_| what != c.name || self.copyable(c)) {
                    d = d.with_fix(format!("keep a copy of `{}`", what), vec![(Span::empty_at(at), ".clone()".to_string())]);
                }
                d
            }
        }
    }
}

/// What Oxie did with a parameter it could lend, and why.
#[derive(Clone, Debug)]
pub struct ParamNote {
    /// The index of the file declaring the function.
    pub file: usize,
    pub fun: String,
    /// The function's name in that file (methods and local functions may share the name).
    pub fun_span: Span,
    pub index: usize,
    /// Lent as `&str`, `&[T]`, `&T`, ... (else kept owned).
    pub borrowed: bool,
    /// "`name` isn't `own`"
    pub why: String,
    /// The use that decided it.
    pub cause: Option<Span>,
}

/// The function named `fun`, declared once in the files (at the top level or in a class): the
/// index of its file and its declaration.
fn only_fun<'f>(files: &[&'f File], fun: &str) -> Option<(usize, &'f FunDecl)> {
    fn funs<'a>(items: &'a [Item], out: &mut Vec<&'a FunDecl>) {
        for it in items {
            match &it.kind {
                ItemKind::Fun(f) => out.push(f),
                ItemKind::Type(t) => funs(&t.members, out),
                ItemKind::Impl(i) => funs(&i.members, out),
                ItemKind::Mod { items, .. } | ItemKind::Object { items, .. } => funs(items, out),
                _ => {}
            }
        }
    }
    let mut found = Vec::new();
    for (i, f) in files.iter().enumerate() {
        let mut here = Vec::new();
        funs(&f.items, &mut here);
        found.extend(here.into_iter().filter(|d| d.name == fun).map(|d| (i, d)));
    }
    let [one] = found[..] else { return None };
    Some(one)
}

/// Why the function `fun` (declared once in the crate) takes its parameter number `index`, which
/// it declares `own`: the index of the file declaring it, where, and why ("it returns `v`", or
/// that it says `own`). None when it borrows the parameter.
pub fn kept_reason(files: &[&File], fun: &str, index: usize) -> Option<(usize, Span, String)> {
    let (file, d) = only_fun(files, fun)?;
    let p = d.params.get(index).filter(|p| p.own)?;
    let name = p.name()?;
    let at_name = match &p.pat {
        Pat::Ident { span, .. } => *span,
        _ => p.span,
    };
    let body = match d.body.as_ref()? {
        FunBody::Block(b) => b.clone(),
        FunBody::Expr(e) => Block { stmts: vec![Stmt::Expr { expr: e.clone(), semi: false }], span: e.span },
    };
    let views: Vec<(&[String], &File)> = files.iter().map(|f| (&[][..], *f)).collect();
    let a = Analysis::new(&views, Stage::Text);
    let c = Cand { file, fun: d, owner: None, index, kind: kind_of(&p.ty).unwrap_or(Kind::Class), name, ty: p.ty.clone(), module: Vec::new(), body, class: None };
    let kept = if rebinds(&c.body, name) { None } else { a.owned_uses(&c).into_iter().find_map(|at| a.kept_at(&c, at)).or_else(|| a.first_change(&c)) };
    Some(match kept {
        Some(k) => (file, k.span(), k.why().to_string()),
        None => (file, at_name, format!("its parameter is `own {}`", name)),
    })
}

/// Where and why the function `fun` (declared once in the crate, at the top level or in a
/// class) keeps its parameter number `index` in a way no borrow could serve: it returns it,
/// stores it (in a variable, a field, a struct or a collection), passes it on by value, or a
/// `move` lambda captures it. The index of the file declaring it, the use, and a clause like
/// "it returns `v`". For the fix that would lend the parameter, which can't compile then.
pub fn stored_reason(files: &[&File], fun: &str, index: usize) -> Option<(usize, Span, String)> {
    let (file, d) = only_fun(files, fun)?;
    let p = d.params.get(index)?;
    let name = p.name()?;
    let body = match d.body.as_ref()? {
        FunBody::Block(b) => b.clone(),
        FunBody::Expr(e) => Block { stmts: vec![Stmt::Expr { expr: e.clone(), semi: false }], span: e.span },
    };
    if rebinds(&body, name) {
        return None;
    }
    let views: Vec<(&[String], &File)> = files.iter().map(|f| (&[][..], *f)).collect();
    let a = Analysis::new(&views, Stage::Text);
    let c = Cand { file, fun: d, owner: None, index, kind: Kind::Class, name, ty: p.ty.clone(), module: Vec::new(), body, class: None };
    // the uses, without those lent to a call; a `move` lambda's start for what it captures
    let mut uses: Vec<u32> = Vec::new();
    let mut lent: HashSet<u32> = HashSet::new();
    walk_block(&c.body, &mut |e| {
        let is_p = |x: &Expr| x.ident() == Some(name);
        match &e.kind {
            ExprKind::Path { .. } if is_p(e) => uses.push(e.span.start),
            ExprKind::StructLit { fields, .. } if fields.iter().any(|(f, v)| v.is_none() && f == name) => uses.push(e.span.start),
            ExprKind::Call { callee, args } => {
                if let Some(f) = callee.ident() {
                    for (pos, arg) in args.iter().enumerate() {
                        if is_p(&arg.value) && (PRINTS.contains(&f) && !a.decls.contains_key(f) || a.lends(&c, f, pos, arg)) {
                            lent.insert(arg.value.span.start);
                        }
                    }
                }
            }
            ExprKind::Lambda { is_move: true, body, .. } => walk_expr(body, &mut |x| {
                if is_p(x) {
                    uses.push(e.span.start);
                }
                true
            }),
            _ => {}
        }
        true
    });
    uses.sort();
    uses.dedup();
    let stores = |why: &str| ["it returns", "it stores", "it passes", "a `move` lambda"].iter().any(|s| why.starts_with(s));
    uses.into_iter().filter(|u| !lent.contains(u)).map(|at| kept_use(&c, at)).find(|(_, why)| stores(why)).map(|(span, why)| (file, span, why))
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
            ExprKind::Unary(UnOp::Ref | UnOp::RefMut, x) if is_at(x) => Some(format!("it uses `&{}`, a reference to the value itself", name)),
            ExprKind::In { item, .. } if is_at(item) => Some(format!("it looks for `{}` with `in`, which compares the value itself", name)),
            ExprKind::Assign(_, _, rhs) if is_at(rhs) => Some(format!("it stores `{}`", name)),
            ExprKind::StructLit { fields, .. } if fields.iter().any(|(_, v)| v.as_ref().is_some_and(is_at)) => Some(format!("it stores `{}` in a struct", name)),
            ExprKind::StructLit { fields, .. } if e.span.start == at && fields.iter().any(|(f, v)| v.is_none() && f == name) => Some(format!("it stores `{}` in a struct", name)),
            ExprKind::Raw(_) | ExprKind::Macro { .. } if e.span.start == at => Some(format!("Rust code that Oxie doesn't read uses `{}`", name)),
            _ => None,
        };
        true
    });
    let len = if what.as_deref().is_some_and(|w| w.starts_with("a `move`")) { 1 } else { name.len() as u32 };
    (Span { start: at, end: at + len }, what.unwrap_or_else(|| format!("it uses `{}` by value", name)))
}

/// Lends the `String`, `Vec<T>`, map, set and `PathBuf` parameters of top-level functions that
/// aren't `own`, and says so for each.
pub fn borrow_params(files: &mut [&mut File]) -> Vec<ParamNote> {
    let mut with_modules: Vec<(&[String], &mut File)> = files.iter_mut().map(|f| (&[][..], &mut **f)).collect();
    lend(&mut with_modules, Stage::Text)
}

/// Lends the class and `Box<T>` parameters of top-level functions, like `borrow_params`; `files`
/// come with their modules, and `k` tells the classes, after receiver inference.
pub fn borrow_class_params(files: &mut [(&[String], &mut File)], k: &Crate) -> Vec<ParamNote> {
    lend(files, Stage::Types { k, copy: copy_types(k) })
}

/// Lends the parameters of methods of every kind, like `borrow_params` and `borrow_class_params`
/// do for top-level functions.
pub fn borrow_method_params(files: &mut [(&[String], &mut File)], k: &Crate) -> Vec<ParamNote> {
    lend(files, Stage::Methods(Methods { k, copy: copy_types(k) }))
}

/// The errors for the borrowed parameters that their functions change or keep, with the index
/// of the file declaring each function. Text that a function keeps is copied instead, as any
/// `&str` is. Runs after the three passes lent theirs, with the classes they tell (`k`).
pub fn check_kept(files: &[(&[String], &File)], k: &Crate) -> Vec<(usize, Diag)> {
    let a = Analysis::new(files, Stage::Methods(Methods { k, copy: copy_types(k) }));
    let mut out = Vec::new();
    for c in &a.cands {
        let change = a.first_change(c);
        let keep = a.owned_uses(c).into_iter().filter_map(|at| a.kept_at(c, at)).find(|k| !matches!(k, Kept::Keep { text: true, .. }));
        let first = match (change, keep) {
            (Some(x), Some(y)) => {
                if x.span().start <= y.span().start {
                    x
                } else {
                    y
                }
            }
            (Some(x), None) | (None, Some(x)) => x,
            (None, None) => continue,
        };
        out.push((c.file, a.kept_error(c, first)));
    }
    out
}

/// The types that derive `Copy`.
fn copy_types(k: &Crate) -> HashSet<String> {
    k.derives.iter().filter(|(_, d)| d.iter().any(|x| x == "Copy")).map(|(n, _)| n.clone()).collect()
}

/// Lends every parameter of the stage's kinds that isn't `own` (nor `var`, a context, or a
/// pattern), says so for each, and records its declared type in `Param::lent`. Functions whose
/// parameters Rust fixes keep theirs: interface methods and their implementations, operators,
/// `main`, extension functions, and `const` and `extern` functions.
fn lend(files: &mut [(&[String], &mut File)], stage: Stage) -> Vec<ParamNote> {
    // the type each lent parameter gets, by (file, start of the function's name, parameter index)
    let mut lent: HashMap<(usize, u32, usize), Type> = HashMap::new();
    let mut notes = Vec::new();
    {
        let views: Vec<(&[String], &File)> = files.iter().map(|(m, f)| (*m, &**f)).collect();
        let mut funs = Vec::new();
        for (i, (module, f)) in views.iter().enumerate() {
            match &stage {
                Stage::Methods(m) => methods(i, &f.items, module, m.k, &mut funs),
                _ => top_level_funs(i, &f.items, module, &mut funs),
            }
        }
        for fun in &funs {
            let f = fun.decl;
            let fixed = fun.owner.is_some_and(|(_, _, fixed)| fixed);
            if fixed || fun.owner.is_none() && f.name == "main" || f.receiver.is_some() || f.mods.is_const || f.mods.abi.is_some() {
                continue;
            }
            for (i, p) in f.params.iter().enumerate() {
                let Pat::Ident { name, mutable: false, by_ref: false, sub: None, .. } = &p.pat else { continue };
                if p.own || p.context || p.lent.is_some() {
                    continue;
                }
                let Some((kind, _)) = kind_in(&p.ty, &stage, &fun.module) else { continue };
                lent.insert((fun.file, f.name_span.start, i), lent_type(&p.ty, kind));
                notes.push(ParamNote { file: fun.file, fun: f.name.clone(), fun_span: f.name_span, index: i, borrowed: true, why: format!("`{}` isn't `own`", name), cause: None });
            }
        }
    }
    fn apply(file: usize, items: &mut [Item], lent: &HashMap<(usize, u32, usize), Type>) {
        for it in items {
            match &mut it.kind {
                ItemKind::Fun(f) => {
                    for (i, p) in f.params.iter_mut().enumerate() {
                        if let Some(t) = lent.get(&(file, f.name_span.start, i)) {
                            p.lent = Some(std::mem::replace(&mut p.ty, t.clone()));
                        }
                    }
                }
                ItemKind::Type(t) => apply(file, &mut t.members, lent),
                ItemKind::Impl(i) => apply(file, &mut i.members, lent),
                ItemKind::Mod { items, .. } | ItemKind::Object { items, .. } => apply(file, items, lent),
                _ => {}
            }
        }
    }
    if !lent.is_empty() {
        for (i, (_, f)) in files.iter_mut().enumerate() {
            apply(i, &mut f.items, &lent);
        }
    }
    notes
}

/// The type a parameter of type `ty` gets when it is lent: `&str`, `&[T]`, `&Path`, `&Class` or
/// `&HashMap<K, V>`, or `&T` for a `Box<T>`.
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
        Kind::PathBuf => Type::Path { global: true, segs: ["std", "path", "Path"].into_iter().map(PathSeg::plain).collect(), span: Span::default() },
        Kind::Boxed => arg(),
        Kind::Class | Kind::Coll => ty.clone(),
    };
    Type::Ref { lifetime: None, mutable: false, inner: Box::new(inner) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::symbols::type_text;

    /// Lends the parameters of a one-file program as the compiler does, then checks them: the
    /// file, and the errors as (the source they point at, message, the lines the fixes give).
    fn lent(src: &str) -> (File, Vec<(String, String, Vec<String>)>) {
        let (mut file, diags) = crate::parser::parse_file(src);
        assert!(diags.is_empty(), "{:?}", diags);
        let module: Vec<String> = Vec::new();
        let collect = |file: &File| {
            let units = [crate::sema::SourceUnit { module: module.clone(), file }];
            crate::sema::collect(&units, "t", HashSet::new(), false)
        };
        borrow_params(&mut [&mut file]);
        let k = collect(&file);
        borrow_class_params(&mut [(&module[..], &mut file)], &k);
        borrow_method_params(&mut [(&module[..], &mut file)], &k);
        let k = collect(&file);
        let errors = check_kept(&[(&module[..], &file)], &k)
            .into_iter()
            .map(|(_, d)| (src[d.span.start as usize..d.span.end as usize].to_string(), d.message.clone(), d.fixes.iter().filter_map(|f| f.preview(src)).collect()))
            .collect();
        (file, errors)
    }

    /// The type of the first parameter of the function or method `f`.
    fn first_param(file: &File, f: &str) -> Option<String> {
        fn find(items: &[Item], f: &str) -> Option<String> {
            items.iter().find_map(|it| match &it.kind {
                ItemKind::Fun(d) if d.name == f => Some(type_text(&d.params[0].ty)),
                ItemKind::Type(t) => find(&t.members, f),
                _ => None,
            })
        }
        find(&file.items, f)
    }

    #[test]
    fn parameters_are_borrowed_unless_own() {
        let src = r#"
import std::sync::MutexGuard
data class User(val name: String, val age: i32)
data class Point(val x: i32, val y: i32)
shared class Account(var balance: i32 = 0)
fun text(s: String): usize = s.len()
fun taken(own s: String): String = s
pub fun api(s: String): usize = s.len()
fun guards(xs: Vec<MutexGuard<i32>>) = println(xs.len())
fun user(u: User): String = u.name.clone()
fun point(p: Point): i32 = p.x
fun handle(a: Account) = println(a.balance)
fun changed(var xs: Vec<i32>) { xs.push(1) }
class Shelf(var items: Vec<String> = vec[]) {
    fun has(t: String): bool = items.contains(&t)
    fun put(own t: String) = items.push(t)
}
"#;
        let (file, errors) = lent(src);
        assert!(errors.is_empty(), "{:?}", errors);
        let ty = |f: &str| first_param(&file, f);
        assert_eq!(ty("text").as_deref(), Some("&str"));
        assert_eq!(ty("taken").as_deref(), Some("String"));
        // a library's `pub` functions too: the signature says it
        assert_eq!(ty("api").as_deref(), Some("&str"));
        // the caller drops what it lent
        assert_eq!(ty("guards").as_deref(), Some("&[MutexGuard<i32>]"));
        assert_eq!(ty("user").as_deref(), Some("&User"));
        // a `Copy` class is copied and a `shared` one copies its handle
        assert_eq!(ty("point").as_deref(), Some("Point"));
        assert_eq!(ty("handle").as_deref(), Some("Account"));
        // `var` takes it, to change it
        assert_eq!(ty("changed").as_deref(), Some("[i32]"));
        assert_eq!(ty("has").as_deref(), Some("&str"));
        assert_eq!(ty("put").as_deref(), Some("String"));
    }

    #[test]
    fn borrowed_parameters_that_are_kept_or_changed_are_errors() {
        let src = r#"
data class User(val name: String, val age: i32)
data class Holder(val user: User)
data class Label(val text: String)
fun returns(v: Vec<i32>): Vec<i32> = v
fun stores(u: User): Holder = Holder(u)
fun consume(own u: User) = println(u.name)
fun passes(u: User) = consume(u)
fun calls(v: Vec<u8>): Vec<u8> = v.into_iter().collect()
fun changes(xs: Vec<i32>) { xs.push(1) }
fun takes_field(u: User): String {
    val n = u.name
    return n
}
fun reads(u: User): usize = u.name.len()
fun forwards(u: User): usize = reads(u)
fun compares(u: User, v: User): bool = u == v
fun named(u: User): String {
    val same = u
    same.name.clone()
}
fun text(s: String): Label = Label(s)
fun first(xs: Vec<User>): &User = &xs[0]
"#;
        let (_, errors) = lent(src);
        let message = |at: &str, f: &str| errors.iter().find(|e| e.0 == at && e.1.contains(f)).map(|e| e.1.clone());
        assert_eq!(errors.len(), 6, "{:#?}", errors);
        assert_eq!(message("v", "returns").as_deref(), Some("`returns` can't keep `v`: it is borrowed"));
        assert_eq!(message("u", "stores").as_deref(), Some("`stores` can't keep `u`: it is borrowed"));
        assert_eq!(message("u", "passes").as_deref(), Some("`passes` can't keep `u`: it is borrowed"));
        assert_eq!(message("v", "calls").as_deref(), Some("`calls` can't keep `v`: it is borrowed"));
        assert_eq!(message("xs", "changes").as_deref(), Some("`xs` is borrowed, so `changes` can't change it"));
        assert_eq!(message("u.name", "takes_field").as_deref(), Some("`takes_field` can't keep `u.name`: `u` is borrowed"));
        // the fixes: take it, or keep a copy (or change the caller's value)
        let fixes = |at: &str, f: &str| errors.iter().find(|e| e.0 == at && e.1.contains(f)).map(|e| e.2.clone()).unwrap_or_default();
        assert_eq!(fixes("u", "stores"), ["fun stores(own u: User): Holder = Holder(u)", "fun stores(u: User): Holder = Holder(u.clone())"]);
        assert_eq!(fixes("xs", "changes"), ["fun changes(xs: &mut Vec<i32>) { xs.push(1) }", "fun changes(own xs: Vec<i32>) { xs.push(1) }"]);
    }
}
