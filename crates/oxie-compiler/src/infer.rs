//! Lightweight type inference used for `fun f() = expr` without a declared return type.
//! It only answers when the type is obvious from the syntax and known declarations.

use crate::ast::*;
use crate::lower::{int_suffix, Lowerer};
use crate::parser::is_type_like;

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
fn generic(name: &str, n: usize) -> Type {
    Type::Path { global: false, segs: vec![PathSeg { name: name.into(), generics: Some(vec![GenericArg::Type(Type::Infer); n]), fn_sugar: None }], span: Default::default() }
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
                _ => self.infer_type(l).filter(|t| !matches!(t, Type::Ref { .. })).or_else(|| self.infer_type(r)),
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
            Elvis(_, r) => self.infer_type(r).map(|t| if t == static_str() { ty("String") } else { t }),
            Call { callee, args } => {
                // `util::total(v)`, `a::Counter(1)`
                if let ExprKind::Path { segs, .. } = &callee.kind {
                    if segs.len() > 1 {
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
                    "mapOf" | "mutableMapOf" | "hashMapOf" if !args.is_empty() => Some(generic("HashMap", 2)),
                    "setOf" | "mutableSetOf" | "hashSetOf" if !args.is_empty() => Some(generic("HashSet", 1)),
                    "Vec" | "VecDeque" | "HashSet" | "BTreeSet" if args.is_empty() && !self.k.is_user_type(n) => Some(generic(n, 1)),
                    "HashMap" | "BTreeMap" if args.is_empty() && !self.k.is_user_type(n) => Some(generic(n, 2)),
                    "String" if !self.k.is_user_type(n) => Some(ty("String")),
                    "format" => Some(ty("String")),
                    _ => None,
                }
            }
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
                    "to_string" | "to_uppercase" | "to_lowercase" | "to_owned" | "repeat" | "join" | "concat" | "replace" | "trim_string" => Some(ty("String")),
                    "is_empty" | "contains" | "contains_key" | "starts_with" | "ends_with" | "is_some" | "is_none" | "is_ok" | "is_err" | "any" | "all"
                    | "is_alphabetic" | "is_numeric" | "is_alphanumeric" | "is_whitespace" | "is_digit" | "eq" | "ne" | "is_nan" => Some(ty("bool")),
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
            If { then, else_, .. } => self.infer_block(then).or_else(|| else_.as_ref().and_then(|e| self.infer_type(e))),
            When { arms, .. } => arms.iter().find_map(|a| if a.body.diverges() { None } else { self.infer_type(&a.body) }),
            Block(b) => self.infer_block(b),
            StructLit { path, generics: None, .. } => {
                let n = path.last()?;
                if path.len() == 1 && is_type_like(n) && self.class_info(n).is_some_and(|c| !c.has_generics) {
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
                    _ => None,
                }
            }
            VecLit(items) if !items.is_empty() => {
                // `vec[x; n]` holds `x`s
                let t = match &items[0].kind {
                    ArrayRepeat(x, _) => self.infer_type(x)?,
                    _ => self.infer_type(&items[0])?,
                };
                Some(Type::Path { global: false, segs: vec![PathSeg { name: "Vec".into(), generics: Some(vec![GenericArg::Type(t)]), fn_sugar: None }], span: Default::default() })
            }
            Array(items) if !items.is_empty() => Some(Type::Array(Box::new(self.infer_type(&items[0])?), items.len().to_string())),
            // only the collection is known (`for` loops over it borrow it when it's used afterwards)
            MapLit(items) if !items.is_empty() => Some(generic("HashMap", 2)),
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
            f2.ret = self.infer_type(e);
            self.pop_scope();
        }
        f2
    }
}
