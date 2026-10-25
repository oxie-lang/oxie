//! Lightweight type inference used for `fun f() = expr` without a declared return type.
//! It only answers when the type is obvious from the syntax and known declarations.

use crate::ast::*;
use crate::lower::{int_suffix, Lowerer};

fn ty(name: &str) -> Type {
    Type::simple(name)
}

/// `a::Counter`
fn path_ty(path: &[String]) -> Type {
    Type::Path { global: false, segs: path.iter().map(|s| PathSeg { name: s.clone(), generics: None, fn_sugar: None }).collect(), span: Default::default() }
}

fn static_str() -> Type {
    Type::Ref { lifetime: Some("static".into()), mutable: false, inner: Box::new(ty("str")) }
}

/// `Name<_, ..>` with `n` inferred arguments.
/// The element type of a list, array or slice (`Type::Infer` when it isn't known).
fn list_elem(t: &Type) -> Option<Type> {
    match t {
        Type::Ref { inner, .. } => list_elem(inner),
        Type::Slice(inner) | Type::Array(inner, _) => Some((**inner).clone()),
        Type::Path { segs, .. } if segs.len() == 1 && segs[0].name == "Vec" => match segs[0].generics.as_deref() {
            Some([GenericArg::Type(inner)]) => Some(inner.clone()),
            _ => Some(Type::Infer),
        },
        _ => None,
    }
}

fn generic(name: &str, n: usize) -> Type {
    Type::Path { global: false, segs: vec![PathSeg { name: name.into(), generics: Some(vec![GenericArg::Type(Type::Infer); n]), fn_sugar: None }], span: Default::default() }
}

/// `Name<args>`, or `Name<_, ..>` (`n` of them) when the arguments aren't written.
fn with_args(name: &str, args: Option<&Vec<GenericArg>>, n: usize) -> Type {
    match args {
        Some(g) => Type::Path { global: false, segs: vec![PathSeg { name: name.into(), generics: Some(g.clone()), fn_sugar: None }], span: Default::default() },
        None => generic(name, n),
    }
}

/// The number of type arguments of a std collection that `Name()` / `Name::new()` builds.
fn std_collection(name: &str) -> Option<usize> {
    match name {
        "Vec" | "VecDeque" | "LinkedList" | "HashSet" | "BTreeSet" | "BinaryHeap" => Some(1),
        "HashMap" | "BTreeMap" => Some(2),
        _ => None,
    }
}

/// Does the type leave some of itself to Rust (`Vec<_>`)?
pub(crate) fn has_infer(t: &Type) -> bool {
    match t {
        Type::Infer => true,
        Type::Path { segs, .. } => segs.iter().flat_map(|s| s.generics.iter().flatten()).any(|g| matches!(g, GenericArg::Type(t) | GenericArg::Binding(_, t) if has_infer(t))),
        Type::Ref { inner, .. } | Type::Slice(inner) | Type::Array(inner, _) | Type::Option(inner) => has_infer(inner),
        Type::Tuple(v) => v.iter().any(has_infer),
        _ => false,
    }
}

impl<'a> Lowerer<'a> {
    pub(crate) fn infer_type(&self, e: &Expr) -> Option<Type> {
        self.infer(e, false)
    }

    /// The class or enum a type names, as a path for `class_at`: its name, or for a name several
    /// modules declare, its whole path (`a::Counter`).
    pub(crate) fn type_owner(&self, t: &Type) -> Option<Vec<String>> {
        match t {
            Type::Ref { inner, .. } => self.type_owner(inner),
            Type::Path { segs, .. } if segs.len() == 1 => Some(vec![segs[0].name.clone()]),
            Type::Path { segs, .. } if segs.last().is_some_and(|s| self.k.same_named.contains(&s.name)) => Some(segs.iter().map(|s| s.name.clone()).collect()),
            _ => None,
        }
    }

    /// The type a path names: its last name, or for a name several modules declare, the whole
    /// path, so it isn't taken for another module's type of that name.
    fn named_ty(&self, path: &[String]) -> Type {
        let name = path.last().map(|s| s.as_str()).unwrap_or_default();
        if path.len() > 1 && self.k.same_named.contains(name) {
            path_ty(path)
        } else {
            ty(name)
        }
    }

    /// The type of a call through a module path: a function's result (`util.total(v)`) or a
    /// constructed class (`a.Counter(1)`).
    fn path_call_type(&self, path: &[String], through_try: bool) -> Option<Type> {
        if let Some(f) = self.fun_at(path) {
            return if f.throws && !through_try || f.generic_ret { None } else { f.ret.clone() };
        }
        let c = self.k.classes.get(&self.k.type_key_at(path, &self.module)?)?;
        (!c.has_generics).then(|| self.named_ty(path))
    }

    /// `through_try`: the expression is the operand of `?`, so a throwing call yields its value type.
    fn infer(&self, e: &Expr, through_try: bool) -> Option<Type> {
        use ExprKind::*;
        match &e.kind {
            Try(x) => self.infer(x, true),
            Int(s) => Some(ty(int_suffix(s).unwrap_or("i32"))),
            Float(s) => Some(ty(if s.ends_with("f32") { "f32" } else { "f64" })),
            Bool(_) | Is { .. } | In { .. } => Some(ty("bool")),
            Char(s) => Some(ty(if s.starts_with('b') { "u8" } else { "char" })),
            Str(_) | RawStr(_) => Some(static_str()),
            Template(_) => Some(ty("String")),
            Path { .. } => {
                let n = e.ident()?;
                if let Some(t) = self.local(n) {
                    return t.clone().filter(|t| *t != Type::Infer);
                }
                if self.implicit_field(n).is_some() {
                    return self.class.as_ref()?.fields.get(n).cloned().filter(|t| *t != Type::Infer);
                }
                None
            }
            SelfValue => Some(ty("Self")),
            Binary(op, l, r) => match op {
                BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge | BinOp::And | BinOp::Or => Some(ty("bool")),
                BinOp::Add if matches!(l.kind, Str(_) | Template(_)) || matches!(r.kind, Str(_) | Template(_)) => Some(ty("String")),
                // arithmetic on a null-checked `x` reads its value: never an optional
                _ => self.infer_type(l).filter(|t| !matches!(t, Type::Ref { .. } | Type::Option(_))).or_else(|| self.infer_type(r).filter(|t| !matches!(t, Type::Option(_)))),
            },
            Unary(UnOp::Neg, x) | Unary(UnOp::Not, x) => self.infer_type(x),
            Unary(UnOp::Ref, x) => Some(Type::Ref { lifetime: None, mutable: false, inner: Box::new(self.infer_type(x).unwrap_or(Type::Infer)) }),
            Unary(UnOp::RefMut, x) => Some(Type::Ref { lifetime: None, mutable: true, inner: Box::new(self.infer_type(x).unwrap_or(Type::Infer)) }),
            Unary(UnOp::Deref, x) => match self.infer_type(x)? {
                Type::Ref { inner, .. } => Some(*inner),
                _ => None,
            },
            Cast(_, t) => Some(t.clone()),
            Paren(x) => self.infer_type(x),
            // `a ?: return`: `a`'s value
            Elvis(l, r) if r.diverges() => match self.infer_type(l)? {
                Type::Option(t) => Some(*t).filter(|t| *t != Type::Infer),
                _ => None,
            },
            Elvis(_, r) => self.infer_type(r).map(|t| if t == static_str() { ty("String") } else { t }),
            Call { callee, args } => {
                // `HashMap<String, i32>()`, `mutableListOf<i32>()`, `Stack<i32>()`: type arguments given
                if let ExprKind::Path { segs, generics: Some(g), global: false } = &callee.kind {
                    let n = segs.last()?.as_str();
                    let built = match (segs.len(), n) {
                        (1, "listOf" | "mutableListOf" | "arrayListOf" | "vecOf") if g.len() == 1 => Some("Vec"),
                        (1, "mapOf" | "mutableMapOf" | "hashMapOf") if g.len() == 2 => Some("HashMap"),
                        (1, "setOf" | "mutableSetOf" | "hashSetOf") if g.len() == 1 => Some("HashSet"),
                        _ => None,
                    };
                    if let Some(t) = built {
                        return Some(with_args(t, Some(g), 0));
                    }
                    // `HashMap<String, i32>::new()`: the arguments belong to the type
                    if let [ty, f] = segs.as_slice() {
                        return match std_collection(ty) {
                            Some(count) if matches!(f.as_str(), "new" | "with_capacity" | "default") && !self.k.is_user_type(ty) && g.len() == count => Some(with_args(ty, Some(g), count)),
                            _ => None,
                        };
                    }
                    if segs.len() != 1 || self.is_local(n) {
                        return None;
                    }
                    if std_collection(n).is_some_and(|count| count == g.len()) && args.is_empty() && !self.k.is_user_type(n) {
                        return Some(with_args(n, Some(g), 0));
                    }
                    return self.class_info(n).filter(|c| c.type_params == g.len()).map(|_| with_args(n, Some(g), 0));
                }
                // `Vec::<i32>::new()`, `std::collections::HashMap::<K, V>::new()`
                if let ExprKind::QualifiedPath(Type::Qualified { ty, trait_: None, rest }) = &callee.kind {
                    let Type::Path { segs, .. } = &**ty else { return None };
                    let last = segs.last()?;
                    let std_path = segs.len() == 1 && !self.k.is_user_type(&last.name) || segs[0].name == "std";
                    return match (std_collection(&last.name), rest.as_slice()) {
                        (Some(count), [f]) if std_path && matches!(f.name.as_str(), "new" | "with_capacity" | "default") => Some(with_args(&last.name, last.generics.as_ref(), count)),
                        _ => None,
                    };
                }
                // `util::total(v)`, `a::Counter(1)`
                if let ExprKind::Path { segs, .. } = &callee.kind {
                    if segs.len() > 1 {
                        // `Vec::new()`, `std::collections::HashMap::new()`
                        if let [.., ty, f] = segs.as_slice() {
                            let std_path = segs.len() == 2 && !self.k.is_user_type(ty) || segs[0] == "std";
                            if let Some(count) = std_collection(ty).filter(|_| std_path && matches!(f.as_str(), "new" | "with_capacity" | "default")) {
                                return Some(generic(ty, count));
                            }
                        }
                        return self.path_call_type(segs, through_try);
                    }
                }
                let n = callee.ident()?;
                if self.is_local(n) {
                    return None;
                }
                if let Some(c) = &self.class {
                    if let Some(m) = c.methods.get(n) {
                        return if m.throws && !through_try { None } else { m.ret.clone() };
                    }
                }
                if let Some(c) = self.class_info(n) {
                    return if c.has_generics { None } else { Some(ty(n)) };
                }
                if let Some(f) = self.fun_info(n) {
                    return if f.throws && !through_try || f.generic_ret { None } else { f.ret.clone() };
                }
                if let Some(en) = self.k.variant_enum_in(n, Some(&self.module)) {
                    return Some(ty(&en.name));
                }
                match n {
                    "Some" if args.len() == 1 => self.infer_type(&args[0].value).map(|t| Type::Option(Box::new(t))),
                    "listOf" | "mutableListOf" | "arrayListOf" | "vecOf" if !args.is_empty() => match self.infer_type(&args[0].value) {
                        Some(t) => Some(Type::Path { global: false, segs: vec![PathSeg { name: "Vec".into(), generics: Some(vec![GenericArg::Type(t)]), fn_sugar: None }], span: Default::default() }),
                        None => Some(generic("Vec", 1)),
                    },
                    "listOf" | "mutableListOf" | "arrayListOf" | "vecOf" => Some(generic("Vec", 1)),
                    "mapOf" | "mutableMapOf" | "hashMapOf" => Some(generic("HashMap", 2)),
                    "setOf" | "mutableSetOf" | "hashSetOf" => Some(generic("HashSet", 1)),
                    "Vec" | "VecDeque" | "HashSet" | "BTreeSet" if args.is_empty() && !self.k.is_user_type(n) => Some(generic(n, 1)),
                    "HashMap" | "BTreeMap" if args.is_empty() && !self.k.is_user_type(n) => Some(generic(n, 2)),
                    "String" if !self.k.is_user_type(n) => Some(ty("String")),
                    "format" => Some(ty("String")),
                    _ => None,
                }
            }
            // a data class's `copy(field = value)`
            MethodCall { recv, name, args, .. } if name == "copy" && !args.is_empty() && args.iter().all(|a| a.name.is_some()) => match self.infer_type(recv)? {
                Type::Ref { inner, .. } => Some(*inner),
                t => Some(t),
            },
            MethodCall { recv, name, .. } => {
                if matches!(recv.kind, SelfValue) {
                    if let Some(m) = self.class.as_ref().and_then(|c| c.methods.get(name)) {
                        return if m.throws && !through_try { None } else { m.ret.clone() };
                    }
                }
                if let Some((p, _)) = self.as_path(recv) {
                    // the type the call goes through: `Self`, `Counter` or `a.Counter`
                    let owner = if p.last()? == "Self" { vec![self.class.as_ref()?.name.clone()] } else { p };
                    // `String.new()`, `String.with_capacity(n)`, `String.from(s)`
                    if owner == ["String"] && !self.k.is_user_type("String") && matches!(name.as_str(), "new" | "with_capacity" | "from") {
                        return Some(ty("String"));
                    }
                    let owner_ty = || self.named_ty(&owner);
                    if let Some(c) = self.class_at(&owner) {
                        if name == "new" && !c.user_new {
                            return if c.has_generics { None } else { Some(owner_ty()) };
                        }
                        return c.methods.get(name).and_then(|m| if m.throws && !through_try { None } else { m.ret.clone() });
                    }
                    if let Some(en) = self.enum_at(&owner) {
                        if en.variant(name).is_some() {
                            return Some(owner_ty());
                        }
                        return en.methods.get(name).and_then(|m| m.ret.clone());
                    }
                    // `util.total(v)` and `a.Counter(1)`, through the module that declares them
                    let path: Vec<String> = owner.iter().cloned().chain([name.clone()]).collect();
                    return self.path_call_type(&path, through_try);
                }
                // a method of a value of a known class or enum (`root.adopt("kid")`)
                if let Some(recv_ty) = self.infer_type(recv) {
                    if let Some(owner) = self.type_owner(&recv_ty) {
                        let own = |m: &crate::sema::MethodInfo| -> Option<Type> {
                            if m.throws && !through_try {
                                return None;
                            }
                            match m.ret.clone()? {
                                t if t.single_name() == Some("Self") => Some(self.named_ty(&owner)),
                                t => Some(t),
                            }
                        };
                        if let Some(c) = self.class_at(&owner).filter(|c| !c.has_generics) {
                            if let Some(m) = c.methods.get(name) {
                                return own(m);
                            }
                        }
                        if let Some(en) = self.enum_at(&owner).filter(|e| !e.has_type_params) {
                            if let Some(m) = en.methods.get(name) {
                                return own(m);
                            }
                        }
                    }
                }
                match name.as_str() {
                    "len" | "count" | "capacity" => Some(ty("usize")),
                    "group_by" | "associate_by" => Some(generic("HashMap", 2)),
                    "to_list" => Some(generic("Vec", 1)),
                    "to_string" | "to_uppercase" | "to_lowercase" | "to_owned" | "repeat" | "join" | "concat" | "replace" | "trim_string" => Some(ty("String")),
                    "is_empty" | "contains" | "contains_key" | "starts_with" | "ends_with" | "is_some" | "is_none" | "is_ok" | "is_err" | "any" | "all"
                    | "is_alphabetic" | "is_numeric" | "is_alphanumeric" | "is_whitespace" | "is_digit" | "eq" | "ne" | "is_nan" => Some(ty("bool")),
                    // Oxie's list operations on a list give a new list; those that keep the
                    // elements keep their type
                    "filter" | "filter_not" | "sorted" | "sorted_descending" | "sorted_by" | "sorted_by_descending" | "reversed" | "distinct" => {
                        self.infer_type(recv).as_ref().and_then(list_elem).map(|t| with_args("Vec", Some(&vec![GenericArg::Type(t)]), 1))
                    }
                    "map" | "map_indexed" | "flat_map" | "filter_map" | "chunked" => self.infer_type(recv).as_ref().and_then(list_elem).map(|_| generic("Vec", 1)),
                    // an element of a list, if there is one
                    "find" | "first" | "last" | "get" | "max_by_key" | "min_by_key" => self
                        .infer_type(recv)
                        .as_ref()
                        .and_then(list_elem)
                        .filter(|t| *t != Type::Infer)
                        .map(|t| Type::Option(Box::new(Type::Ref { lifetime: None, mutable: false, inner: Box::new(t) }))),
                    "clone" | "abs" | "sqrt" | "sin" | "cos" | "tan" | "powi" | "powf" | "floor" | "ceil" | "round" | "min" | "max" | "exp" | "ln"
                    | "log10" | "signum" | "pow" | "wrapping_add" | "wrapping_sub" | "wrapping_mul" | "saturating_sub" | "saturating_add" | "clamp" => {
                        self.infer_type(recv).filter(|t| !matches!(t, Type::Ref { .. }))
                    }
                    _ => None,
                }
            }
            Field { recv, name, safe: false } => {
                if matches!(recv.kind, SelfValue) {
                    return self.class.as_ref()?.fields.get(name).cloned();
                }
                let rt = self.infer_type(recv)?;
                // `t.0` of a tuple
                if let (Type::Tuple(ts), Ok(i)) = (&rt, name.parse::<usize>()) {
                    return ts.get(i).cloned().filter(|t| *t != Type::Infer);
                }
                let tn = match &rt {
                    Type::Ref { inner, .. } => inner.single_name()?.to_string(),
                    t => t.single_name()?.to_string(),
                };
                let c = self.class_info(&tn)?;
                c.fields.iter().find(|f| f.name == *name).map(|f| f.ty.clone())
            }
            // the branch that `if (val x = opt)` or `if (x != null)` unwraps `x` in reads it as the
            // value, which this doesn't see: there the other branch tells the type
            If { cond, then, else_ } => {
                let other = || else_.as_ref().and_then(|e| self.infer_type(e));
                let binds = |c: &Cond| matches!(c, Cond::Let { .. }) || matches!(c, Cond::Chain(cs) if cs.iter().any(|c| matches!(c, Cond::Let { .. })));
                if binds(cond) {
                    return other().or_else(|| self.infer_block(then));
                }
                match self.infer_block(then) {
                    Some(Type::Option(_)) => other().filter(|t| !matches!(t, Type::Option(_))).or_else(|| self.infer_block(then)),
                    t => t.or_else(other),
                }
            }
            When { arms, .. } => arms.iter().find_map(|a| if a.body.diverges() { None } else { self.infer_type(&a.body) }),
            Block(b) => self.infer_block(b),
            StructLit { path, generics: None, .. } => {
                let n = path.last()?;
                if path.len() == 1 && self.class_info(n).is_some_and(|c| !c.has_generics) {
                    Some(ty(n))
                } else {
                    None
                }
            }
            Tuple(items) => items.iter().map(|x| self.infer_type(x)).collect::<Option<Vec<_>>>().map(Type::Tuple),
            // `xs[i]` of a `Vec<T>`, `[T]` or `[T; N]` (ranges index slices, so only plain indices)
            Index(c, i) if !matches!(i.kind, Range { .. }) => {
                let t = match self.infer_type(c)? {
                    Type::Ref { inner, .. } => *inner,
                    t => t,
                };
                match t {
                    Type::Slice(inner) | Type::Array(inner, _) => Some(*inner),
                    Type::Path { segs, .. } if segs.len() == 1 && matches!(segs[0].name.as_str(), "Vec" | "VecDeque") => match segs[0].generics.as_deref() {
                        Some([GenericArg::Type(inner)]) => Some(inner.clone()),
                        _ => None,
                    },
                    // `m[key]` of a map: its value
                    Type::Path { segs, .. } if segs.len() == 1 && matches!(segs[0].name.as_str(), "HashMap" | "BTreeMap") => match segs[0].generics.as_deref() {
                        Some([_, GenericArg::Type(value)]) => Some(value.clone()).filter(|t| *t != Type::Infer),
                        _ => None,
                    },
                    _ => None,
                }
            }
            VecLit(items) => {
                // `vec[x; n]` holds `x`s
                let t = match items.first().map(|x| &x.kind) {
                    Some(ArrayRepeat(x, _)) => self.infer_type(x),
                    Some(_) => self.infer_type(&items[0]),
                    None => None,
                };
                Some(Type::Path { global: false, segs: vec![PathSeg { name: "Vec".into(), generics: Some(vec![GenericArg::Type(t.unwrap_or(Type::Infer))]), fn_sugar: None }], span: Default::default() })
            }
            Array(items) if !items.is_empty() => Some(Type::Array(Box::new(self.infer_type(&items[0])?), items.len().to_string())),
            // only the collection is known (`for` loops over it borrow it when it's used afterwards)
            MapLit(_) => Some(generic("HashMap", 2)),
            SetLit(items) if !items.is_empty() => Some(generic("HashSet", 1)),
            _ => None,
        }
    }

    fn infer_block(&self, b: &Block) -> Option<Type> {
        match b.stmts.last()? {
            Stmt::Expr { expr, semi: false } => self.infer_type(expr),
            _ => None,
        }
    }

    /// Returns the declaration with an inferred return type when it has none.
    pub(crate) fn with_inferred_ret(&mut self, f: &FunDecl, trait_ret: Option<Type>) -> FunDecl {
        if f.ret.is_some() {
            return f.clone();
        }
        let mut f2 = f.clone();
        if let Some(t) = trait_ret {
            f2.ret = Some(t);
            return f2;
        }
        if let Some(FunBody::Expr(e)) = &f.body {
            self.push_scope();
            for p in &f.params {
                self.bind_pat(&p.pat, Some(p.ty.clone()));
            }
            // a type Rust would have to fill in (`Vec<_>`) can't be a function's result
            f2.ret = self.infer_type(e).filter(|t| !has_infer(t));
            self.pop_scope();
        }
        f2
    }
}
