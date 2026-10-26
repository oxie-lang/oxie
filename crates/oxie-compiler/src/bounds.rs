//! Infers trait bounds of type parameters from how a function body uses them.
//!
//! `fun show<T>(x: T): String = "<$x>"` needs `T: Display` in Rust. Oxie looks at what the body
//! does with values of type `T` (and with elements of `Vec<T>`/`&[T]` parameters) and adds the
//! bounds the generated Rust needs:
//!
//! | use                                   | bound                          |
//! |---------------------------------------|--------------------------------|
//! | `"$x"`, `println(x)`, `x.to_string()` | `Display`                      |
//! | `"${x:?}"`                            | `Debug`                        |
//! | `a < b`, `a.partial_cmp(b)`           | `PartialOrd`                   |
//! | `a == b`, `x in list`                 | `PartialEq`                    |
//! | `a.cmp(b)`                            | `Ord`                          |
//! | `a + b` (both `T`)                    | `Add<Output = T> + Copy`, ...  |
//! | `a += b`                              | `AddAssign`, ...               |
//! | `x.clone()`                           | `Clone`                        |
//! | key of a `HashSet`/`HashMap` local    | `Eq + Hash`                    |
//! | `xs.clone()`, `"$xs"`, `xs.contains(x)`, `xs.iter().max()` of a `Vec<T>` | `Clone`, `Debug`, `PartialEq`, `Ord` |
//! | `this.copy(..)` of a generic data class | `Clone`                      |
//!
//! Bounds the declaration already states (or implies, like `Ord` for `PartialOrd`) are not
//! repeated. Only plain uses are recognized; anything else is left to rustc, whose error then
//! suggests the bound.

use std::collections::{BTreeMap, HashMap, HashSet};

use crate::ast::*;
use crate::visit::walk_expr;

/// What a name holds: a value of type parameter `T`, or a collection of them.
#[derive(Clone, Debug, PartialEq)]
enum Kind {
    Elem(String),
    Coll(String),
    /// A hash map or set with keys of the type parameter.
    Keys(String),
}

type Names = HashMap<String, Kind>;
/// Enum case -> what each of its fields holds.
type Cases = HashMap<String, Vec<Option<Kind>>>;

/// What a scan knows besides the names: locals holding hash sets or maps, and the cases of the
/// enum whose method it is.
struct Cx<'a> {
    sets: &'a HashSet<String>,
    cases: &'a Cases,
}
/// Type parameter -> bounds in the order found (deduplicated by name).
type Found = BTreeMap<String, Vec<(&'static str, Type)>>;

/// Returns `f` with inferred bounds added as `where` predicates. `outer` are type parameters
/// of an enclosing generic class or enum, with the class fields typed by them (`fields`) and
/// the enum's cases (`variants`, the types of their fields in order).
pub fn with_inferred_bounds(f: &FunDecl, outer: &[GenericParam], fields: &[(String, Type)], variants: &[(String, Vec<Type>)]) -> FunDecl {
    let mut params: Vec<(String, Vec<Type>)> = Vec::new();
    for g in f.generics.params.iter().chain(outer.iter()) {
        if let GenericParam::Type { name, bounds, .. } = g {
            params.push((name.clone(), bounds.clone()));
        }
    }
    if params.is_empty() {
        return f.clone();
    }
    let tnames: HashSet<String> = params.iter().map(|(n, _)| n.clone()).collect();
    let mut names = Names::new();
    for p in &f.params {
        if let (Some(n), Some(k)) = (p.name(), kind_of(&p.ty, &tnames)) {
            names.insert(n.to_string(), k);
        }
    }
    for (n, t) in fields {
        if let Some(k) = kind_of(t, &tnames) {
            names.entry(n.clone()).or_insert(k);
        }
    }
    let cases: Cases = variants.iter().map(|(n, ts)| (n.clone(), ts.iter().map(|t| kind_of(t, &tnames)).collect())).collect();
    if names.is_empty() && !cases.values().flatten().any(|k| k.is_some()) {
        return f.clone();
    }
    let mut found = Found::new();
    let mut sets = HashSet::new();
    if let Some(FunBody::Block(b)) = &f.body {
        collect_sets(b, &mut sets);
    }
    let cx = Cx { sets: &sets, cases: &cases };
    match &f.body {
        Some(FunBody::Block(b)) => scan_block(b, &names, &cx, &mut found),
        Some(FunBody::Expr(e)) => scan(e, &names, &cx, &mut found),
        None => {}
    }
    let mut out = f.clone();
    for (t, bounds) in found {
        let declared: Vec<String> = params
            .iter()
            .filter(|(n, _)| *n == t)
            .flat_map(|(_, b)| b.iter().filter_map(|b| b.last_name().map(|s| s.to_string())))
            .chain(f.generics.where_clause.iter().filter_map(|w| match w {
                WherePred::Type { ty, bounds, hrtb: None } if ty.single_name() == Some(t.as_str()) => Some(bounds.iter().filter_map(|b| b.last_name().map(|s| s.to_string())).collect::<Vec<_>>()),
                _ => None,
            }).flatten())
            .collect();
        let new: Vec<Type> = bounds.into_iter().filter(|(name, _)| !implied(name, &declared)).map(|(_, ty)| ty).collect();
        if !new.is_empty() {
            out.generics.where_clause.push(WherePred::Type { ty: Type::simple(&t), bounds: new, hrtb: None });
        }
    }
    out
}

/// Is bound `name` already stated or implied by the declared bounds?
fn implied(name: &str, declared: &[String]) -> bool {
    let has = |n: &str| declared.iter().any(|d| d == n);
    has(name)
        || match name {
            "PartialOrd" => has("Ord"),
            "PartialEq" => has("Eq") || has("Ord") || has("PartialOrd"),
            "Clone" => has("Copy"),
            "Display" => has("Error"),
            _ => false,
        }
}

fn kind_of(t: &Type, tnames: &HashSet<String>) -> Option<Kind> {
    match t {
        Type::Path { segs, .. } if segs.len() == 1 && segs[0].generics.is_none() && tnames.contains(&segs[0].name) => Some(Kind::Elem(segs[0].name.clone())),
        Type::Ref { inner, .. } => kind_of(inner, tnames),
        Type::Slice(inner) | Type::Array(inner, _) => match kind_of(inner, tnames) {
            Some(Kind::Elem(n)) => Some(Kind::Coll(n)),
            _ => None,
        },
        Type::Path { segs, .. } if segs.len() == 1 && matches!(segs[0].name.as_str(), "HashMap" | "HashSet") => match segs[0].generics.as_deref() {
            Some([GenericArg::Type(key), ..]) => match kind_of(key, tnames) {
                Some(Kind::Elem(n)) => Some(Kind::Keys(n)),
                _ => None,
            },
            _ => None,
        },
        Type::Path { segs, .. } if segs.len() == 1 && matches!(segs[0].name.as_str(), "Vec" | "VecDeque" | "List" | "MutableList") => {
            match segs[0].generics.as_deref() {
                Some([GenericArg::Type(inner)]) => match kind_of(inner, tnames) {
                    Some(Kind::Elem(n)) => Some(Kind::Coll(n)),
                    _ => None,
                },
                _ => None,
            }
        }
        _ => None,
    }
}

fn path(segs: &[&str]) -> Type {
    Type::Path { global: true, segs: segs.iter().map(|s| PathSeg::plain(*s)).collect(), span: Default::default() }
}

fn op_trait(name: &str, t: &str) -> Type {
    let mut seg = PathSeg::plain(name);
    seg.generics = Some(vec![GenericArg::Binding("Output".into(), Type::simple(t))]);
    Type::Path { global: true, segs: vec![PathSeg::plain("std"), PathSeg::plain("ops"), seg], span: Default::default() }
}

fn add(found: &mut Found, t: &str, name: &'static str, ty: Type) {
    let list = found.entry(t.to_string()).or_default();
    if !list.iter().any(|(n, _)| *n == name) {
        list.push((name, ty));
    }
}

fn display(found: &mut Found, t: &str) {
    add(found, t, "Display", path(&["std", "fmt", "Display"]));
}

/// The type parameter of a value expression, if it is one.
fn elem(e: &Expr, names: &Names) -> Option<String> {
    match &e.kind {
        ExprKind::Paren(x) | ExprKind::Unary(UnOp::Deref, x) | ExprKind::Unary(UnOp::Ref, x) => elem(x, names),
        ExprKind::Index(c, _) => coll(c, names),
        ExprKind::MethodCall { recv, name, args, .. } if args.is_empty() && matches!(name.as_str(), "clone" | "to_owned") => elem(recv, names),
        _ => match names.get(e.ident()?) {
            Some(Kind::Elem(t)) => Some(t.clone()),
            _ => None,
        },
    }
}

/// The element type parameter of a collection expression (`xs`, `&xs`, `xs.iter()`).
fn coll(e: &Expr, names: &Names) -> Option<String> {
    match &e.kind {
        ExprKind::Paren(x) | ExprKind::Unary(UnOp::Ref, x) | ExprKind::Unary(UnOp::RefMut, x) => coll(x, names),
        ExprKind::MethodCall { recv, name, .. } if matches!(name.as_str(), "iter" | "into_iter" | "iter_mut") => coll(recv, names),
        _ => match names.get(e.ident()?) {
            Some(Kind::Coll(t)) => Some(t.clone()),
            _ => None,
        },
    }
}

/// The collection a method-call chain starts from: `xs.iter().filter { .. }.map { .. }`.
fn chain_root(e: &Expr, names: &Names) -> Option<String> {
    match &e.kind {
        ExprKind::MethodCall { recv, .. } => coll(recv, names).or_else(|| chain_root(recv, names)),
        _ => None,
    }
}

/// Locals initialised with a hash set or map (their keys need `Eq + Hash`).
fn collect_sets(b: &Block, out: &mut HashSet<String>) {
    for s in &b.stmts {
        if let Stmt::Let { pat: Pat::Ident { name, .. }, init: Some(init), .. } = s {
            let is_hash = match &init.kind {
                ExprKind::Call { callee, .. } => match &callee.kind {
                    ExprKind::Path { segs, .. } => segs.iter().any(|s| matches!(s.as_str(), "HashSet" | "HashMap" | "mutableSetOf" | "mutableMapOf" | "hashSetOf" | "hashMapOf")),
                    _ => false,
                },
                ExprKind::SetLit(_) | ExprKind::MapLit(_) => true,
                _ => false,
            };
            if is_hash {
                out.insert(name.clone());
            }
        }
    }
}

fn scan_block(b: &Block, names: &Names, cx: &Cx, found: &mut Found) {
    // `val y = x` makes `y` a `T` too
    let mut names = names.clone();
    for s in &b.stmts {
        match s {
            Stmt::Let { pat, init, .. } => {
                if let Some(e) = init {
                    scan(e, &names, cx, found);
                }
                let mut bound = Vec::new();
                pat.bindings(&mut bound);
                for n in bound {
                    names.remove(&n);
                }
                if let (Pat::Ident { name, .. }, Some(e)) = (pat, init) {
                    if let Some(t) = elem(e, &names) {
                        names.insert(name.clone(), Kind::Elem(t));
                    } else if let Some(t) = coll(e, &names) {
                        names.insert(name.clone(), Kind::Coll(t));
                    }
                }
            }
            Stmt::Expr { expr, .. } => scan(expr, &names, cx, found),
            Stmt::Attributed { stmt, .. } => scan_block(&Block { stmts: vec![(**stmt).clone()], span: b.span }, &names, cx, found),
            Stmt::Item(_) => {}
        }
    }
}

fn lambda_names(params: &[LambdaParam], body: &Expr, braces: bool) -> Vec<String> {
    if params.is_empty() {
        return if braces && crate::visit::uses_it(body) { vec!["it".to_string()] } else { Vec::new() };
    }
    let mut out = Vec::new();
    for p in params {
        p.pat.bindings(&mut out);
    }
    out
}

fn scan(e: &Expr, names: &Names, cx: &Cx, found: &mut Found) {
    walk_expr(e, &mut |x: &Expr| {
        use ExprKind::*;
        match &x.kind {
            Template(parts) => {
                for p in parts {
                    if let TemplatePart::Expr(v, spec) = p {
                        let debug = spec.as_deref().is_some_and(|s| s.contains('?'));
                        if let Some(t) = elem(v, names) {
                            if debug {
                                add(found, &t, "Debug", path(&["std", "fmt", "Debug"]));
                            } else {
                                display(found, &t);
                            }
                        // `"$xs"` prints a list in its debug form too
                        } else if let Some(t) = coll(v, names).filter(|_| debug || spec.is_none()) {
                            add(found, &t, "Debug", path(&["std", "fmt", "Debug"]));
                        }
                    }
                }
            }
            // `println(x)` is a call until lowering makes it a macro
            Call { callee, args } if matches!(callee.ident(), Some("println" | "print" | "eprintln" | "eprint")) && !args.is_empty() => {
                if !matches!(args[0].value.kind, Str(_) | RawStr(_)) {
                    for a in args {
                        if let Some(t) = elem(&a.value, names) {
                            display(found, &t);
                        }
                    }
                }
            }
            Binary(op, a, b) => {
                let (ta, tb) = (elem(a, names), elem(b, names));
                // `x + o.x` beside a field `x: T`: the same field of another value of the class
                let field = |e: &Expr, other: &Option<String>| match &e.kind {
                    Field { name, .. } => match names.get(name) {
                        Some(Kind::Elem(t)) if other.as_ref() == Some(t) => Some(t.clone()),
                        _ => None,
                    },
                    _ => None,
                };
                let (ta, tb) = (ta.clone().or_else(|| field(a, &tb)), tb.clone().or_else(|| field(b, &ta)));
                let t = ta.clone().or(tb.clone());
                match op {
                    BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => {
                        if let Some(t) = t {
                            add(found, &t, "PartialOrd", path(&["std", "cmp", "PartialOrd"]));
                        }
                    }
                    BinOp::Eq | BinOp::Ne => {
                        if let Some(t) = t {
                            add(found, &t, "PartialEq", path(&["std", "cmp", "PartialEq"]));
                        }
                    }
                    BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Rem if ta.is_some() && ta == tb => {
                        let t = ta.unwrap();
                        let (n, tr) = match op {
                            BinOp::Add => ("Add", "Add"),
                            BinOp::Sub => ("Sub", "Sub"),
                            BinOp::Mul => ("Mul", "Mul"),
                            BinOp::Div => ("Div", "Div"),
                            _ => ("Rem", "Rem"),
                        };
                        add(found, &t, n, op_trait(tr, &t));
                        add(found, &t, "Copy", Type::simple("Copy"));
                    }
                    _ => {}
                }
            }
            Assign(Some(op), target, _) => {
                if let Some(t) = elem(target, names) {
                    let (n, tr): (&'static str, &str) = match op {
                        BinOp::Add => ("AddAssign", "AddAssign"),
                        BinOp::Sub => ("SubAssign", "SubAssign"),
                        BinOp::Mul => ("MulAssign", "MulAssign"),
                        BinOp::Div => ("DivAssign", "DivAssign"),
                        _ => return true,
                    };
                    add(found, &t, n, path(&["std", "ops", tr]));
                }
            }
            In { item, .. } => {
                if let Some(t) = elem(item, names) {
                    add(found, &t, "PartialEq", path(&["std", "cmp", "PartialEq"]));
                }
            }
            MethodCall { recv, name, args, .. } => {
                if let Some(t) = elem(recv, names) {
                    match name.as_str() {
                        "clone" => add(found, &t, "Clone", Type::simple("Clone")),
                        "to_string" => display(found, &t),
                        "cmp" | "max" | "min" | "clamp" => add(found, &t, "Ord", path(&["std", "cmp", "Ord"])),
                        "partial_cmp" => add(found, &t, "PartialOrd", path(&["std", "cmp", "PartialOrd"])),
                        "eq" | "ne" => add(found, &t, "PartialEq", path(&["std", "cmp", "PartialEq"])),
                        _ => {}
                    }
                }
                let hash_op = matches!(name.as_str(), "insert" | "contains" | "contains_key" | "entry" | "get" | "get_mut" | "remove");
                if recv.ident().is_some_and(|r| cx.sets.contains(r)) && hash_op {
                    if let Some(t) = args.first().and_then(|a| elem(&a.value, names)) {
                        add(found, &t, "Eq", path(&["std", "cmp", "Eq"]));
                        add(found, &t, "Hash", path(&["std", "hash", "Hash"]));
                    }
                }
                // a map or set field keyed by `K`
                if let (true, Some(Kind::Keys(t))) = (hash_op, recv.ident().and_then(|r| names.get(r))) {
                    add(found, t, "Eq", path(&["std", "cmp", "Eq"]));
                    add(found, t, "Hash", path(&["std", "hash", "Hash"]));
                }
                // a `Vec<T>` copied, searched or sorted
                if let Some(t) = coll(recv, names) {
                    match name.as_str() {
                        "clone" | "to_vec" => add(found, &t, "Clone", Type::simple("Clone")),
                        "contains" => add(found, &t, "PartialEq", path(&["std", "cmp", "PartialEq"])),
                        "sort" | "max" | "min" => add(found, &t, "Ord", path(&["std", "cmp", "Ord"])),
                        _ => {}
                    }
                } else if let (Some(t), true) = (chain_root(x, names), matches!(name.as_str(), "max" | "min") && args.is_empty()) {
                    add(found, &t, "Ord", path(&["std", "cmp", "Ord"]));
                }
                // `this.copy(..)` of a generic data class clones the fields
                if name == "copy" && matches!(recv.kind, SelfValue) {
                    for k in names.values() {
                        if let Kind::Elem(t) | Kind::Coll(t) = k {
                            add(found, t, "Clone", Type::simple("Clone"));
                        }
                    }
                }
                // lambdas over a collection of `T` get `T` values
                if let Some(t) = chain_root(x, names) {
                    scan(recv, names, cx, found);
                    for a in args {
                        if let Lambda { params, body, implicit_it, .. } = &a.value.kind {
                            let mut inner = names.clone();
                            for n in lambda_names(params, body, *implicit_it) {
                                inner.insert(n, Kind::Elem(t.clone()));
                            }
                            scan(body, &inner, cx, found);
                        } else {
                            scan(&a.value, names, cx, found);
                        }
                    }
                    return false;
                }
            }
            For { pat, iter, body, .. } => {
                scan(iter, names, cx, found);
                let mut inner = names.clone();
                let mut bound = Vec::new();
                pat.bindings(&mut bound);
                for n in &bound {
                    inner.remove(n);
                }
                if let (Some(t), Pat::Ident { name, .. }) = (coll(iter, names), pat) {
                    inner.insert(name.clone(), Kind::Elem(t));
                }
                scan_block(body, &inner, cx, found);
                return false;
            }
            // the fields of a generic enum's case hold `T`s: `Leaf(x) -> x.clone()`
            When { subject, arms, .. } if !cx.cases.is_empty() => {
                if let Some(s) = subject {
                    scan(s, names, cx, found);
                }
                for arm in arms {
                    let mut inner = names.clone();
                    for c in &arm.conds {
                        if let WhenCond::Pat(Pat::TupleStruct { path, elems, .. }) = c {
                            let Some(kinds) = path.last().and_then(|n| cx.cases.get(n)) else { continue };
                            for (p, k) in elems.iter().zip(kinds) {
                                if let (Pat::Ident { name, sub: None, .. }, Some(k)) = (p, k) {
                                    inner.insert(name.clone(), k.clone());
                                }
                            }
                        }
                    }
                    if let Some(g) = &arm.guard {
                        scan(g, &inner, cx, found);
                    }
                    scan(&arm.body, &inner, cx, found);
                }
                return false;
            }
            Lambda { params, body, implicit_it, .. } => {
                let mut inner = names.clone();
                for n in lambda_names(params, body, *implicit_it) {
                    inner.remove(&n);
                }
                scan(body, &inner, cx, found);
                return false;
            }
            Block(b) | Unsafe(b) | LabeledBlock { block: b, .. } | Loop { body: b, .. } => {
                scan_block(b, names, cx, found);
                return false;
            }
            _ => {}
        }
        true
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bounds_of(src: &str) -> String {
        let (file, diags) = crate::parser::parse_file(src);
        assert!(diags.is_empty(), "{:?}", diags);
        let ItemKind::Fun(f) = &file.items[0].kind else { panic!() };
        let out = with_inferred_bounds(f, &[], &[], &[]);
        out.generics
            .where_clause
            .iter()
            .map(|w| match w {
                WherePred::Type { ty, bounds, .. } => format!("{}: {}", ty.single_name().unwrap(), bounds.iter().map(|b| b.last_name().unwrap().to_string()).collect::<Vec<_>>().join(" + ")),
                _ => String::new(),
            })
            .collect::<Vec<_>>()
            .join(", ")
    }

    #[test]
    fn infers_common_bounds() {
        assert_eq!(bounds_of("fun show<T>(x: T): String = \"<$x>\""), "T: Display");
        assert_eq!(bounds_of("fun show<T>(x: &T): String = \"${x:?}\""), "T: Debug");
        assert_eq!(bounds_of("fun larger<T>(a: T, b: T): T = if (a > b) a else b"), "T: PartialOrd");
        assert_eq!(bounds_of("fun larger<T: Ord>(a: T, b: T): T = if (a > b) a else b"), "");
        assert_eq!(bounds_of("fun sum<T>(a: T, b: T): T = a + b"), "T: Add + Copy");
        assert_eq!(bounds_of("fun biggest<T>(xs: &[T]): T { var m = xs[0].clone()\n for (x in xs) { if (x > &m) m = x.clone() }\n m }"), "T: Clone + PartialOrd");
        assert_eq!(bounds_of("fun names<T>(xs: &[T]): Vec<String> = xs.iter().map { it.to_string() }.collect()"), "T: Display");
        assert_eq!(bounds_of("fun uniq<T>(xs: &[T]): usize { val seen = HashSet()\n for (x in xs) seen.insert(x)\n seen.len() }"), "T: Eq + Hash");
        assert_eq!(bounds_of("fun has<T>(xs: &[T], x: &T): bool = x in xs"), "T: PartialEq");
        assert_eq!(bounds_of("fun f<T>(xs: Vec<T>): Vec<T> = xs.clone()"), "T: Clone");
        assert_eq!(bounds_of("fun f<T>(xs: &[T]): String = \"${xs:?}\""), "T: Debug");
        assert_eq!(bounds_of("fun f<T>(xs: &[T]): String = \"items: $xs\""), "T: Debug");
        assert_eq!(bounds_of("fun f<T>(xs: &[T], x: &T): bool = xs.contains(x)"), "T: PartialEq");
        assert_eq!(bounds_of("fun f<T>(xs: &[T]): Option<&T> = xs.iter().max()"), "T: Ord");
        assert_eq!(bounds_of("fun f<K, V>(m: &mut HashMap<K, V>, k: K, v: V) { m.insert(k, v) }"), "K: Eq + Hash");
        // a lambda over something else doesn't make `it` a `T`
        assert_eq!(bounds_of("fun f<T>(x: T, ys: Vec<i32>): Vec<String> = ys.iter().map { \"$it\" }.collect()"), "");
    }
}
