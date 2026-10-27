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
pub(crate) fn list_elem(t: &Type) -> Option<Type> {
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
            // through a box (a recursive field), as Rust's method calls go
            Type::Path { segs, .. } if segs.len() == 1 && segs[0].name == "Box" && !self.k.is_user_type("Box") => match segs[0].generics.as_deref() {
                Some([GenericArg::Type(inner)]) => self.type_owner(inner),
                _ => None,
            },
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
            return if f.throws && !through_try { None } else { generic_result(f) };
        }
        let c = self.k.classes.get(&self.k.type_key_at(path, &self.module)?)?;
        (!c.has_generics).then(|| self.named_ty(path))
    }

    /// `through_try`: the expression is the operand of `?`, so a throwing call yields its value type.
    /// The type of an `if` or `when` whose branches give tuples, some with `null` in a part.
    pub(crate) fn tuple_with_null_parts(&self, e: &Expr) -> Option<Type> {
        let mut tails = Vec::new();
        crate::lower_expr::value_tails(e, &mut tails);
        let tuples: Vec<&Vec<Expr>> = tails.iter().map(|t| if let ExprKind::Tuple(xs) = &t.kind { Some(xs) } else { None }).collect::<Option<_>>()?;
        let n = tuples.first()?.len();
        if tuples.len() < 2 || n < 2 || tuples.iter().any(|t| t.len() != n) {
            return None;
        }
        let mut nulls = false;
        let mut parts = Vec::new();
        for i in 0..n {
            let null = tuples.iter().any(|t| matches!(t[i].kind, ExprKind::Null));
            nulls |= null;
            let ty = tuples.iter().filter(|t| !matches!(t[i].kind, ExprKind::Null)).find_map(|t| self.infer_type(&t[i]));
            parts.push(if null { optional(ty) } else { ty.unwrap_or(Type::Infer) });
        }
        nulls.then_some(Type::Tuple(parts))
    }

    /// `Option<&T>` (`Option<&mut T>` when `mutable`) for an element of the list `recv`.
    fn list_elem_ref(&self, recv: &Expr, mutable: bool) -> Option<Type> {
        let t = self.infer_type(recv).as_ref().and_then(list_elem).filter(|t| *t != Type::Infer)?;
        Some(Type::Option(Box::new(Type::Ref { lifetime: None, mutable, inner: Box::new(t) })))
    }

    fn infer(&self, e: &Expr, through_try: bool) -> Option<Type> {
        use ExprKind::*;
        // `r?.m()` on a `Result` is Rust's `?` then the call
        if let MethodCall { recv, name, generics, args, safe: true } = &e.kind {
            if self.is_result_expr(recv) {
                let tried = Expr::new(Try(recv.clone()), recv.span);
                return self.infer(&Expr::new(MethodCall { recv: Box::new(tried), name: name.clone(), generics: generics.clone(), args: args.clone(), safe: false }, e.span), through_try);
            }
        }
        if let Field { recv, name, safe: true } = &e.kind {
            if self.is_result_expr(recv) {
                let tried = Expr::new(Try(recv.clone()), recv.span);
                return self.infer(&Expr::new(Field { recv: Box::new(tried), name: name.clone(), safe: false }, e.span), through_try);
            }
        }
        // `a?.b` and the links after it in the chain: optional
        if let MethodCall { recv, safe, .. } | Field { recv, safe, .. } = &e.kind {
            if (*safe || crate::lower_expr::has_safe_link(recv)) && !self.is_result_expr(recv) {
                return Some(self.safe_link_type(e, through_try));
            }
        }
        match &e.kind {
            Try(x) => self.infer(x, true),
            // `x!!`: the value
            Unwrap(x) => match self.infer_type(x)? {
                Type::Option(t) => Some(*t).filter(|t| *t != Type::Infer),
                _ => None,
            },
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
            Unary(op @ (UnOp::RawConst | UnOp::RawMut), x) => Some(Type::Ptr { mutable: *op == UnOp::RawMut, inner: Box::new(self.infer_type(x).unwrap_or(Type::Infer)) }),
            Unary(UnOp::Deref, x) => match self.infer_type(x)? {
                Type::Ref { inner, .. } => Some(*inner),
                _ => None,
            },
            Cast(_, t) => Some(t.clone()),
            Paren(x) | Attributed { expr: x, .. } => self.infer_type(x),
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
                    return if f.throws && !through_try { None } else { generic_result(f) };
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
            // `xs.collect::<Vec<i32>>()`, `s.parse::<f64>()`: the type it is asked for
            MethodCall { name, generics: Some(g), .. } if name == "collect" || name == "parse" && through_try => match g.as_slice() {
                [GenericArg::Type(t)] if !has_infer(t) => Some(t.clone()),
                _ => None,
            },
            MethodCall { recv, name, args, .. } => {
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
                        if let Some(c) = self.class_at(&owner) {
                            if let Some(m) = c.methods.get(name) {
                                // a generic class's type parameters are the receiver's arguments
                                let args = type_args(&recv_ty);
                                return own(m).map(|t| subst(&t, &c.type_names, &args)).filter(|t| *t != Type::Infer);
                            }
                        }
                        if let Some(en) = self.enum_at(&owner).filter(|e| !e.has_type_params) {
                            if let Some(m) = en.methods.get(name) {
                                return own(m);
                            }
                        }
                    }
                }
                // an interface's method, on an interface value or a type parameter bound by one
                if let Some(m) = self.iface_method(recv, name) {
                    return if m.throws && !through_try { None } else { m.ret.clone().filter(|t| t.single_name() != Some("Self")) };
                }
                match name.as_str() {
                    "len" | "count" | "capacity" => Some(ty("usize")),
                    "group_by" | "associate_by" => Some(generic("HashMap", 2)),
                    "to_list" => Some(generic("Vec", 1)),
                    // `opt.replace(v)` and `opt.take()` give the old value, a `T?`
                    "replace" | "take" if args.len() <= 1 && self.infer_type(recv).is_some_and(|t| matches!(t, Type::Option(_)) || t.single_name() == Some("Option")) => self.infer_type(recv),
                    "to_string" | "to_uppercase" | "to_lowercase" | "to_owned" | "repeat" | "join" | "concat" | "replace" | "trim_string" => Some(ty("String")),
                    // a part of text
                    "trim" | "trim_start" | "trim_end" | "as_str" if args.is_empty() => Some(Type::Ref { lifetime: None, mutable: false, inner: Box::new(ty("str")) }),
                    "is_empty" | "contains" | "contains_key" | "starts_with" | "ends_with" | "is_some" | "is_none" | "is_ok" | "is_err" | "any" | "all"
                    | "is_alphabetic" | "is_numeric" | "is_alphanumeric" | "is_whitespace" | "is_digit" | "eq" | "ne" | "is_nan" => Some(ty("bool")),
                    // Oxie's list operations on a list give a new list; those that keep the
                    // elements keep their type
                    "filter" | "filter_not" | "sorted" | "sorted_descending" | "sorted_by" | "sorted_by_descending" | "reversed" | "distinct" | "distinct_by" => {
                        self.infer_type(recv).as_ref().and_then(list_elem).map(|t| with_args("Vec", Some(&vec![GenericArg::Type(t)]), 1))
                    }
                    "partition" => self.infer_type(recv).as_ref().and_then(list_elem).map(|t| {
                        let list = with_args("Vec", Some(&vec![GenericArg::Type(t)]), 1);
                        Type::Tuple(vec![list.clone(), list])
                    }),
                    "index_of_first" | "index_of_last" | "position" | "rposition" => Some(Type::Option(Box::new(ty("usize")))),
                    "map" | "map_indexed" | "flat_map" | "filter_map" | "chunked" => self.infer_type(recv).as_ref().and_then(list_elem).map(|_| generic("Vec", 1)),
                    // `opt.cloned()` of an optional reference: an optional value
                    "cloned" | "copied" => match self.infer_type(recv)? {
                        Type::Option(t) => match *t {
                            Type::Ref { inner, .. } => Some(Type::Option(inner)),
                            _ => None,
                        },
                        _ => None,
                    },
                    // `m.entry(k).or_default()`: the map's value, to change
                    "or_default" | "or_insert" | "or_insert_with" => match &recv.kind {
                        MethodCall { recv: map, name: entry, .. } if entry == "entry" => map_value(&self.infer_type(map)?).map(|v| Type::Ref { lifetime: None, mutable: true, inner: Box::new(v) }),
                        _ => None,
                    },
                    // a map's value, if there is one
                    "get" | "get_mut" | "remove" if map_value(&self.infer_type(recv)?).is_some() => {
                        let v = map_value(&self.infer_type(recv)?)?;
                        Some(Type::Option(Box::new(match name.as_str() {
                            "remove" => v,
                            "get_mut" => Type::Ref { lifetime: None, mutable: true, inner: Box::new(v) },
                            _ => Type::Ref { lifetime: None, mutable: false, inner: Box::new(v) },
                        })))
                    }
                    // the element taken off a list
                    "pop" | "pop_front" | "pop_back" if args.is_empty() => {
                        let t = self.infer_type(recv)?;
                        let elem = list_elem(&t).or_else(|| match &t {
                            Type::Path { segs, .. } if segs.len() == 1 && segs[0].name == "VecDeque" => match segs[0].generics.as_deref() {
                                Some([GenericArg::Type(inner)]) => Some(inner.clone()),
                                _ => None,
                            },
                            _ => None,
                        })?;
                        Some(Type::Option(Box::new(elem)))
                    }
                    // what an iterator gives, if anything
                    "next" | "next_back" | "max" | "min" | "last" | "nth" if args.is_empty() || name == "nth" => match self.infer_type(recv) {
                        Some(t) if list_elem(&t).is_some() && name == "last" => list_elem(&t).filter(|t| *t != Type::Infer).map(|t| Type::Option(Box::new(Type::Ref { lifetime: None, mutable: false, inner: Box::new(t) }))),
                        _ if is_iterator_chain(recv) => Some(Type::Option(Box::new(text_item(recv).unwrap_or(Type::Infer)))),
                        _ => None,
                    },
                    // an item of an iterator, if there is one
                    "find" | "max_by_key" | "min_by_key" | "max_by" | "min_by" | "find_map" if is_iterator_chain(recv) => {
                        Some(Type::Option(Box::new(if name == "find_map" { Type::Infer } else { text_item(recv).unwrap_or(Type::Infer) })))
                    }
                    // an element of a list, if there is one
                    "find" | "find_last" | "first" | "last" | "get" | "max_by_key" | "min_by_key" => self
                        .infer_type(recv)
                        .as_ref()
                        .and_then(list_elem)
                        .filter(|t| *t != Type::Infer)
                        .map(|t| Type::Option(Box::new(Type::Ref { lifetime: None, mutable: false, inner: Box::new(t) }))),
                    "first_mut" | "last_mut" if args.is_empty() => self.list_elem_ref(recv, true),
                    "get_mut" if args.len() == 1 && !self.infer_type(recv).is_some_and(|t| map_value(&t).is_some()) => self.list_elem_ref(recv, true),
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
                // through references and boxes (a recursive field), as Rust reads fields
                let mut t = &rt;
                loop {
                    match t {
                        Type::Ref { inner, .. } => t = inner,
                        Type::Path { segs, .. } if segs.len() == 1 && segs[0].name == "Box" => match segs[0].generics.as_deref() {
                            Some([GenericArg::Type(inner)]) => t = inner,
                            _ => return None,
                        },
                        _ => break,
                    }
                }
                let tn = t.single_name()?.to_string();
                let c = self.class_info(&tn)?;
                c.fields.iter().find(|f| f.name == *name).map(|f| f.ty.clone())
            }
            // branches giving tuples with `null` in a part (`(k, null)`, `(k, v)`): that part
            // is optional
            If { else_: Some(_), .. } | When { .. } if self.tuple_with_null_parts(e).is_some() => self.tuple_with_null_parts(e),
            // a `null` branch makes the value optional (`if (c) 30 else null`)
            If { then, else_: Some(other), .. } if is_null_tail(other) => Some(optional(self.infer_block(then))),
            If { then, else_, .. } if block_null_tail(then) => Some(optional(else_.as_ref().and_then(|e| self.infer_type(e)))),
            When { arms, .. } if arms.iter().any(|a| is_null_tail(&a.body)) => {
                Some(optional(arms.iter().find_map(|a| if a.body.diverges() || is_null_tail(&a.body) { None } else { self.infer_type(&a.body) })))
            }
            // the branch that `if (val x = opt)` or `if (x != null)` unwraps `x` in reads it as the
            // value, which this doesn't see: there the other branch tells the type
            If { cond, then, else_ } => {
                let other = || else_.as_ref().and_then(|e| self.infer_type(e));
                let binds = |c: &Cond| matches!(c, Cond::Let { .. }) || matches!(c, Cond::Chain(cs) if cs.iter().any(|c| matches!(c, Cond::Let { .. })));
                if binds(cond) {
                    return other().or_else(|| self.infer_block(then));
                }
                // an untyped number (`if (n == 0) 1 else n`) takes the other branch's type
                if block_tail(then).is_some_and(untyped_number) {
                    if let Some(t) = other() {
                        return Some(t);
                    }
                }
                match self.infer_block(then) {
                    Some(Type::Option(_)) => other().filter(|t| !matches!(t, Type::Option(_))).or_else(|| self.infer_block(then)),
                    t => t.or_else(other),
                }
            }
            When { arms, .. } => {
                let typed = |a: &&WhenArm| !a.body.diverges() && !untyped_number(&a.body);
                arms.iter().filter(typed).find_map(|a| self.infer_type(&a.body)).or_else(|| arms.iter().find_map(|a| if a.body.diverges() { None } else { self.infer_type(&a.body) }))
            }
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

    /// The type of `a?.b` (or of a link after it, `a?.b.c`): the link on the value inside `a`,
    /// optional; `T?` when the link's own type isn't known.
    fn safe_link_type(&self, e: &Expr, through_try: bool) -> Type {
        let (ExprKind::MethodCall { recv, .. } | ExprKind::Field { recv, .. }) = &e.kind else { return Type::Option(Box::new(Type::Infer)) };
        // `s?.chars()?.next()`: what an iterator over the text gives
        if let (ExprKind::MethodCall { name, .. }, Some(item)) = (&e.kind, text_item(recv)) {
            match name.as_str() {
                "next" | "next_back" | "last" | "nth" | "max" | "min" => return Type::Option(Box::new(item)),
                "count" => return Type::Option(Box::new(ty("usize"))),
                _ => {}
            }
        }
        let inner = match self.infer_type(recv) {
            Some(Type::Option(t)) => Some(*t),
            Some(Type::Ref { inner, .. }) => match *inner {
                Type::Option(t) => Some(Type::Ref { lifetime: None, mutable: false, inner: t }),
                _ => None,
            },
            _ => None,
        };
        // a field read through a borrowed value is borrowed (`&v.name`), as the lowering does
        let borrowed = matches!(e.kind, ExprKind::Field { .. }) && (self.is_place(recv) || matches!(inner, Some(Type::Ref { .. })));
        let link = inner.filter(|t| *t != Type::Infer).and_then(|inner| {
            // the same link, plain, on a stand-in of the value's type
            let stand_in = Expr::new(ExprKind::Cast(Box::new(Expr::new(ExprKind::Path { segs: vec!["__oxie_x".into()], generics: None, global: false }, e.span)), inner), e.span);
            let mut plain = e.clone();
            if let ExprKind::MethodCall { recv, safe, .. } | ExprKind::Field { recv, safe, .. } = &mut plain.kind {
                **recv = stand_in;
                *safe = false;
            }
            self.infer(&plain, through_try)
        });
        let by_ref = |t: Type| Type::Ref { lifetime: None, mutable: false, inner: Box::new(t) };
        match link {
            Some(Type::Option(t)) if borrowed => Type::Option(Box::new(by_ref(*t))),
            Some(t) if borrowed => Type::Option(Box::new(by_ref(t))),
            t => optional(t),
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

/// `T?` of a type that may already be optional, or of one not known (`_?`).
fn optional(t: Option<Type>) -> Type {
    match t {
        Some(t @ Type::Option(_)) => t,
        Some(t) => Type::Option(Box::new(t)),
        None => Type::Option(Box::new(Type::Infer)),
    }
}

/// `null`, or a block ending in it.
fn is_null_tail(e: &Expr) -> bool {
    match &e.kind {
        ExprKind::Null => true,
        ExprKind::Block(b) => block_null_tail(b),
        ExprKind::Paren(x) => is_null_tail(x),
        _ => false,
    }
}

fn block_null_tail(b: &Block) -> bool {
    matches!(b.stmts.last(), Some(Stmt::Expr { expr, semi: false }) if is_null_tail(expr))
}

/// A function's result type, with its own type parameters left to Rust (`_`): their arguments
/// aren't known here, though the result's form is (`[T]` is a list). `None` when only that is
/// left.
fn generic_result(f: &crate::sema::FunInfo) -> Option<Type> {
    f.ret.as_ref().map(|t| subst(t, &f.type_names, &[])).filter(|t| *t != Type::Infer)
}

/// The type arguments of a type (`[String]` of `Stack<String>`), through references.
fn type_args(t: &Type) -> Vec<Type> {
    match t {
        Type::Ref { inner, .. } => type_args(inner),
        Type::Path { segs, .. } => segs.last().and_then(|s| s.generics.as_deref()).unwrap_or_default().iter().filter_map(|g| if let GenericArg::Type(t) = g { Some(t.clone()) } else { None }).collect(),
        _ => Vec::new(),
    }
}

/// `t` with the type parameters `names` replaced by `args`, or by `_` past the end of `args`.
pub(crate) fn subst(t: &Type, names: &[String], args: &[Type]) -> Type {
    match t {
        Type::Path { segs, .. } if segs.len() == 1 && segs[0].generics.is_none() && names.contains(&segs[0].name) => {
            let i = names.iter().position(|n| *n == segs[0].name).unwrap_or(usize::MAX);
            args.get(i).cloned().unwrap_or(Type::Infer)
        }
        Type::Path { global, segs, span } => Type::Path {
            global: *global,
            span: *span,
            segs: segs
                .iter()
                .map(|s| PathSeg {
                    name: s.name.clone(),
                    fn_sugar: s.fn_sugar.clone(),
                    generics: s.generics.as_ref().map(|g| g.iter().map(|a| if let GenericArg::Type(t) = a { GenericArg::Type(subst(t, names, args)) } else { a.clone() }).collect()),
                })
                .collect(),
        },
        Type::Option(x) => Type::Option(Box::new(subst(x, names, args))),
        Type::Ref { lifetime, mutable, inner } => Type::Ref { lifetime: lifetime.clone(), mutable: *mutable, inner: Box::new(subst(inner, names, args)) },
        Type::Slice(x) => Type::Slice(Box::new(subst(x, names, args))),
        Type::Array(x, n) => Type::Array(Box::new(subst(x, names, args)), n.clone()),
        Type::Tuple(v) => Type::Tuple(v.iter().map(|x| subst(x, names, args)).collect()),
        Type::Fn { params, ret } => Type::Fn { params: params.iter().map(|x| subst(x, names, args)).collect(), ret: Box::new(subst(ret, names, args)) },
        t => t.clone(),
    }
}

/// A number literal without a suffix (`1`, `2.5`), whose type the code around it decides.
fn untyped_number(e: &Expr) -> bool {
    match &e.kind {
        ExprKind::Int(s) => int_suffix(s).is_none(),
        ExprKind::Float(s) => !s.ends_with("f32") && !s.ends_with("f64"),
        ExprKind::Unary(UnOp::Neg, x) | ExprKind::Paren(x) => untyped_number(x),
        ExprKind::Block(b) => block_tail(b).is_some_and(untyped_number),
        _ => false,
    }
}

fn block_tail(b: &Block) -> Option<&Expr> {
    match b.stmts.last()? {
        Stmt::Expr { expr, semi: false } => Some(expr),
        _ => None,
    }
}

/// The value type of a map type (`V` of `HashMap<K, V>`), through references.
fn map_value(t: &Type) -> Option<Type> {
    match t {
        Type::Ref { inner, .. } => map_value(inner),
        Type::Path { segs, .. } if segs.len() == 1 && matches!(segs[0].name.as_str(), "HashMap" | "BTreeMap") => match segs[0].generics.as_deref() {
            Some([_, GenericArg::Type(v)]) if *v != Type::Infer => Some(v.clone()),
            _ => None,
        },
        _ => None,
    }
}

/// Is `e` an iterator made by a method that starts one (`xs.iter()`, `s.chars()`), maybe
/// followed by adapters (`.map { }`, `.filter { }`)?
/// What an iterator over text gives, for `chars()`, `bytes()` and the like right before the call
/// (`s.chars().next()` is a `char?`).
fn text_item(e: &Expr) -> Option<Type> {
    let ExprKind::MethodCall { name, args, .. } = &e.kind else { return None };
    match name.as_str() {
        "chars" if args.is_empty() => Some(ty("char")),
        "bytes" if args.is_empty() => Some(ty("u8")),
        "char_indices" if args.is_empty() => Some(Type::Tuple(vec![ty("usize"), ty("char")])),
        "lines" | "split_whitespace" if args.is_empty() => Some(Type::Ref { lifetime: None, mutable: false, inner: Box::new(ty("str")) }),
        _ => None,
    }
}

pub(crate) fn is_iterator_chain(e: &Expr) -> bool {
    const STARTS: &[&str] = &["iter", "iter_mut", "into_iter", "chars", "char_indices", "bytes", "lines", "split", "split_whitespace", "keys", "values", "drain"];
    const ADAPTERS: &[&str] = &["map", "filter", "filter_map", "rev", "skip", "take", "step_by", "enumerate", "zip", "cloned", "copied", "flat_map", "take_while", "skip_while", "peekable", "chain"];
    match &e.kind {
        ExprKind::MethodCall { recv, name, .. } => STARTS.contains(&name.as_str()) || ADAPTERS.contains(&name.as_str()) && is_iterator_chain(recv),
        _ => false,
    }
}
