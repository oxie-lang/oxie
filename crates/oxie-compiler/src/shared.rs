//! `shared class`: objects with several owners, like classes in Kotlin or TypeScript.
//!
//! ```text
//! shared class Account(var balance: i64) { fun deposit(n: i64) { balance += n } }
//! ```
//!
//! becomes the class as usual under the name `AccountData`, plus a handle:
//!
//! ```text
//! pub(crate) struct Account(pub(crate) Rc<RefCell<AccountData>>);   // Arc<Mutex<..>> with @sync
//! impl Account {
//!     fn new(balance: i64) -> Self { .. }                 // same constructor
//!     fn deposit(&self, n: i64) { self.0.write().deposit(n) }   // one borrow per call
//!     fn balance(&self) -> i64 { self.0.read().balance.clone() }   // getter
//!     fn set_balance(&self, value: i64) { .. }                       // setter
//! }
//! ```
//!
//! Copying a handle shares the object; `==` compares identity. Methods run on the data with a
//! single borrow, so a method calling other methods of the same object never re-borrows it.
//! From outside, `a.balance` reads through the getter, `a.balance = v` uses the setter, and
//! `a.items.push(x)` / `a.items.len()` borrow the object for that one call.

use super::*;

impl<'a> Lowerer<'a> {
    pub(crate) fn shared_data_name(name: &str) -> String {
        format!("{}Data", name)
    }

    /// Emits the handle type of a shared class (the data type was emitted as `{Name}Data`).
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn shared_handle(&mut self, it: &Item, t: &TypeDecl, fields: &[(Field, Type)], statics: &[&Item], forwarded: &[(Type, Vec<&Item>)], derives: &[String], g: &str, gn: &str, wc: &str) {
        let sync = it.attrs.iter().any(|a| a.name() == "sync");
        let vis = self.vis(&it.vis, ItemCtx::Top);
        let mvis = if self.members_pub { "pub " } else { "pub(crate) " };
        let data = format!("{}{}", Self::shared_data_name(&t.name), gn);
        let handle = format!("{}{}", t.name, gn);
        let (cell, new_cell) = if sync {
            (format!("::std::sync::Arc<::std::sync::Mutex<{}>>", data), "::std::sync::Arc::new(::std::sync::Mutex::new(".to_string())
        } else {
            (format!("::std::rc::Rc<::std::cell::RefCell<{}>>", data), "::std::rc::Rc::new(::std::cell::RefCell::new(".to_string())
        };
        let ptr = if sync { "::std::sync::Arc" } else { "::std::rc::Rc" };
        // `read()` / `write()` (`__oxie::SharedCell`, `SharedLock`) explain a conflicting use
        let (read, write) = ("read()", "write()");

        self.e.nl();
        self.docs(&it.docs);
        self.e.w(&format!("{}struct {}{}(pub(crate) {}){};", vis, t.name, g, cell, wc));
        self.e.nl();

        // Clone shares the object; equality and hashing use its identity
        self.e.nl();
        self.e.w(&format!("impl{} ::std::clone::Clone for {}{} {{\n    fn clone(&self) -> Self {{\n        Self(self.0.clone())\n    }}\n}}", g, handle, wc));
        self.e.nl();
        self.e.w(&format!("impl{} ::std::cmp::PartialEq for {}{} {{\n    fn eq(&self, other: &Self) -> bool {{\n        {}::ptr_eq(&self.0, &other.0)\n    }}\n}}", g, handle, wc, ptr));
        self.e.nl();
        self.e.w(&format!("impl{} ::std::cmp::Eq for {}{} {{}}", g, handle, wc));
        self.e.nl();
        self.e.w(&format!(
            "impl{} ::std::hash::Hash for {}{} {{\n    fn hash<H: ::std::hash::Hasher>(&self, state: &mut H) {{\n        ({}::as_ptr(&self.0) as *const () as usize).hash(state)\n    }}\n}}",
            g, handle, wc, ptr
        ));
        self.e.nl();
        if derives.iter().any(|d| d == "Debug") {
            self.e.w(&format!("impl{} ::std::fmt::Debug for {}{} {{\n    fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {{\n        ::std::fmt::Debug::fmt(&*self.0.{}, f)\n    }}\n}}", g, handle, wc, read));
            self.e.nl();
        }
        let has_display = t.members.iter().any(|m| matches!(&m.kind, ItemKind::Fun(f) if (f.name == "toString" || f.name == "to_string") && f.mods.is_override));
        if has_display {
            self.e.w(&format!("impl{} ::std::fmt::Display for {}{} {{\n    fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {{\n        ::std::fmt::Display::fmt(&*self.0.{}, f)\n    }}\n}}", g, handle, wc, read));
            self.e.nl();
        }

        self.e.nl();
        self.e.w(&format!("impl{} {}{} {{", g, handle, wc));
        self.e.indent();
        // constructor
        let ctor: Vec<&(Field, Type)> = fields.iter().filter(|(f, _)| f.in_ctor).collect();
        let params: Vec<String> = ctor.iter().map(|(f, ty)| format!("{}: {}", esc(&f.name), self.ty(ty, TyPos::Field))).collect();
        let args: Vec<String> = ctor.iter().map(|(f, _)| esc(&f.name)).collect();
        self.e.nl();
        self.e.w(&format!(
            "#[allow(dead_code)]\n{}fn new({}) -> Self {{\n    Self({}{}::new({}))))\n}}",
            mvis,
            params.join(", "),
            new_cell,
            Self::shared_data_name(&t.name),
            args.join(", ")
        ));
        self.e.nl();
        // getters and setters, unless a method has the name
        let method_names: Vec<&str> = t.members.iter().filter_map(|m| if let ItemKind::Fun(f) = &m.kind { Some(f.name.as_str()) } else { None }).collect();
        let tparams: Vec<String> = t.generics.params.iter().filter_map(|p| if let GenericParam::Type { name, .. } = p { Some(name.clone()) } else { None }).collect();
        for (f, ty) in fields {
            let name = esc(&f.name);
            let fty = self.ty(ty, TyPos::Ret);
            let fvis = if f.vis == Vis::Private { "" } else { mvis };
            if !method_names.contains(&f.name.as_str()) {
                self.e.nl();
                if self.is_cloneable(ty, &tparams) {
                    let bound = match ty.single_name() {
                        Some(n) if tparams.iter().any(|p| p == n) => format!(" where {}: ::std::clone::Clone", n),
                        _ => String::new(),
                    };
                    self.e.w(&format!("#[allow(dead_code)]\n{}fn {}(&self) -> {}{} {{\n    ::std::clone::Clone::clone(&self.0.{}.{})\n}}", fvis, name, fty, bound, read, name));
                } else if !sync {
                    self.e.w(&format!(
                        "#[allow(dead_code)]\n{}fn {}(&self) -> ::std::cell::Ref<'_, {}> {{\n    ::std::cell::Ref::map(self.0.read(), |d| &d.{})\n}}",
                        fvis, name, fty, name
                    ));
                }
                self.e.nl();
            }
            let setter = format!("set_{}", f.name);
            if !method_names.contains(&setter.as_str()) {
                self.e.nl();
                self.e.w(&format!("#[allow(dead_code)]\n{}fn {}(&self, value: {}) {{\n    self.0.{}.{} = value;\n}}", fvis, esc(&setter), self.ty(ty, TyPos::Field), write, name));
                self.e.nl();
            }
        }
        // forwarding methods
        let inherent: Vec<&Item> = t.members.iter().filter(|m| !forwarded.iter().any(|(_, ms)| ms.iter().any(|x| std::ptr::eq(*x, *m)))).collect();
        for m in inherent {
            let ItemKind::Fun(f) = &m.kind else { continue };
            if f.mods.is_operator || (f.mods.is_override && (f.name == "toString" || f.name == "to_string")) {
                continue;
            }
            let recv = self.recv_of(&t.name, f);
            if matches!(recv, Recv::Static | Recv::Value) {
                continue;
            }
            self.e.nl();
            self.forwarder(m, f, recv, mvis, read, write);
        }
        // static methods live on the handle
        for m in statics {
            if let ItemKind::Fun(f) = &m.kind {
                self.e.nl();
                self.method(m, f, Recv::Static, true, None);
            }
        }
        self.e.dedent();
        self.e.nl();
        self.e.w("}");
        self.e.nl();
        // interfaces: the handle implements them too, forwarding to the data
        for (tr, members) in forwarded {
            let tname = self.ty(tr, TyPos::Plain);
            let trait_info = tr.last_name().and_then(|n| self.k.traits.get(n)).cloned();
            self.e.nl();
            self.e.w(&format!("impl{} {} for {}{} {{", g, tname, handle, wc));
            self.e.indent();
            for m in members {
                let ItemKind::Fun(f) = &m.kind else { continue };
                let recv = trait_info
                    .as_ref()
                    .and_then(|ti| ti.methods.get(&f.name))
                    .map(|mi| mi.recv)
                    .unwrap_or_else(|| self.recv_of(&t.name, f));
                if matches!(recv, Recv::Static | Recv::Value) {
                    continue;
                }
                self.e.nl();
                self.pending_trait_ret = trait_info.as_ref().and_then(|ti| ti.methods.get(&f.name)).and_then(|mi| mi.ret.clone());
                self.forwarder(m, f, recv, "", read, write);
            }
            self.e.dedent();
            self.e.nl();
            self.e.w("}");
            self.e.nl();
        }
    }

    /// `fn name(&self, a: A) -> R { self.0.write().name(a) }`
    fn forwarder(&mut self, it: &Item, f: &FunDecl, recv: Recv, vis: &str, read: &str, write: &str) {
        let trait_ret = self.pending_trait_ret.take();
        let f = &auto_lifetimes(&self.with_inferred_ret(f, trait_ret), true, &|n| self.k.type_lifetimes_in(n, &self.module));
        let f = &match self.class.as_ref() {
            Some(c) if !f.mods.is_override => {
                let fields: Vec<(String, Type)> = c.fields.iter().map(|(n, t)| (n.clone(), t.clone())).collect();
                crate::bounds::with_inferred_bounds(f, &c.generics, &fields)
            }
            _ => f.clone(),
        };
        self.e.mark(it.span);
        self.docs(&it.docs);
        let recv_text = if recv == Recv::RefMut && !vis.is_empty() { "&self" } else if recv == Recv::RefMut { "&mut self" } else { "&self" };
        let header = self.fn_header(f);
        let g = self.generics_decl(&f.generics);
        self.push_scope();
        let params = self.params_text(f, Some(recv_text.to_string()));
        let ret = self.ret_type(f, TyPos::Ret);
        self.pop_scope();
        let wc = self.where_clause(&f.generics);
        let mut args: Vec<String> = f.params.iter().enumerate().map(|(i, p)| p.name().map(esc).unwrap_or_else(|| format!("__arg{}", i))).collect();
        // a method that passes `this` on gets the handle
        if !f.mods.is_override && self.needs_this.contains(&f.name) {
            args.push("self".into());
        }
        let access = if recv == Recv::RefMut { write } else { read };
        self.e.w(&format!("#[allow(dead_code)]\n{}{}fn {}{}({})", vis, header, esc(&f.name), g, params));
        if let Some(r) = ret {
            self.e.w(&format!(" -> {}", r));
        }
        self.e.w(&format!("{} {{\n    self.0.{}.{}({})\n}}", wc, access, esc(&f.name), args.join(", ")));
        self.e.nl();
    }

    /// Can a getter return a copy of a field of this type?
    pub(crate) fn is_cloneable(&self, t: &Type, tparams: &[String]) -> bool {
        match t {
            Type::Ref { mutable: false, .. } | Type::Never => true,
            Type::Tuple(v) => v.iter().all(|x| self.is_cloneable(x, tparams)),
            Type::Array(inner, _) | Type::Option(inner) => self.is_cloneable(inner, tparams),
            Type::Fn { .. } | Type::Ref { .. } | Type::Ptr { .. } | Type::Dyn(_) | Type::Impl(_) => false,
            Type::Path { segs, .. } => {
                let Some(last) = segs.last() else { return false };
                let args_ok = last.generics.as_ref().is_none_or(|g| {
                    g.iter().all(|a| match a {
                        GenericArg::Type(x) => self.is_cloneable(x, tparams),
                        _ => true,
                    })
                });
                let n = last.name.as_str();
                let known = matches!(
                    n,
                    "i8" | "i16" | "i32" | "i64" | "i128" | "isize" | "u8" | "u16" | "u32" | "u64" | "u128" | "usize" | "f32" | "f64" | "bool" | "char" | "String"
                        | "Int" | "Long" | "Double" | "Float" | "Boolean" | "Char" | "Byte" | "Short" | "Vec" | "List" | "MutableList" | "HashMap" | "HashSet"
                        | "BTreeMap" | "BTreeSet" | "VecDeque" | "Option" | "Result" | "Box" | "Rc" | "Arc" | "PathBuf" | "Duration" | "Instant"
                ) || tparams.iter().any(|p| p == n)
                    || self.class_info(n).is_some_and(|c| c.is_shared)
                    || self.k.type_key_in(n, &self.module).and_then(|key| self.k.derives.get(&key)).is_some_and(|d| d.iter().any(|x| x == "Clone" || x == "Copy"));
                known && args_ok
            }
            _ => false,
        }
    }

    // ------------------------------------------------------------------ uses from outside

    /// Is `recv.field` a field of a shared-class handle (seen from outside the class)?
    /// Returns whether it is `@sync`.
    pub(crate) fn shared_field(&self, recv: &Expr, field: &str) -> Option<bool> {
        if matches!(recv.kind, ExprKind::SelfValue) {
            return None;
        }
        let t = self.infer_type(recv)?;
        let name = match &t {
            Type::Ref { inner, .. } => inner.single_name()?.to_string(),
            _ => t.single_name()?.to_string(),
        };
        let c = self.class_info(&name)?;
        if !c.is_shared || !c.fields.iter().any(|f| f.name == field) {
            return None;
        }
        Some(c.is_sync)
    }

    /// The shared class of an expression's value type (not of a reference to it).
    pub(crate) fn shared_class_of(&self, e: &Expr) -> Option<String> {
        let t = self.infer_type(e)?;
        let n = t.single_name()?;
        self.class_info(n).filter(|c| c.is_shared).map(|_| n.to_string())
    }

    /// A variable or field holding a shared handle: using it by value copies the handle, so the
    /// variable stays usable (`val b = a` shares the object, like in Kotlin).
    pub(crate) fn is_shared_place(&self, e: &Expr) -> bool {
        if let ExprKind::Field { recv, name, safe: false } = &e.kind {
            if self.shared_field(recv, name).is_some() {
                return false; // the getter already returns a copy of the handle
            }
        }
        self.is_place(e) && !matches!(e.kind, ExprKind::SelfValue) && self.shared_class_of(e).is_some()
    }

    /// Emits a shared handle place with `.clone()` (a reference-count increment).
    pub(crate) fn shared_copy(&mut self, e: &Expr) {
        self.expr(e, P_POSTFIX);
        self.e.w(".clone()");
    }

    /// Fields that are cheap to copy (numbers, handles, optional handles) are read through the
    /// getter even inside a chain, so `node.next.map { .. }` works on a copy.
    fn cheap_field(&self, obj: &Expr, field: &str) -> bool {
        let Some(t) = self.assign_field_type(obj, field) else { return false };
        fn cheap(k: &crate::sema::Crate, t: &Type) -> bool {
            match t {
                Type::Option(inner) => cheap(k, inner),
                Type::Tuple(v) => v.iter().all(|x| cheap(k, x)),
                Type::Ref { mutable: false, .. } => true,
                Type::Path { segs, .. } if segs.len() == 1 => {
                    let n = segs[0].name.as_str();
                    matches!(
                        n,
                        "i8" | "i16" | "i32" | "i64" | "i128" | "isize" | "u8" | "u16" | "u32" | "u64" | "u128" | "usize" | "f32" | "f64" | "bool" | "char" | "Int" | "Long"
                            | "Double" | "Float" | "Boolean" | "Char"
                    ) || (n == "Option" && segs[0].generics.as_deref().is_some_and(|g| g.iter().all(|a| matches!(a, GenericArg::Type(t) if cheap(k, t)))))
                        || k.class(n).is_some_and(|c| c.is_shared)
                }
                _ => false,
            }
        }
        cheap(self.k, &t)
    }

    /// Lowers `e` into a string (for building `obj.0.read().field` bases).
    pub(crate) fn capture(&mut self, e: &Expr, prec: u8) -> String {
        let saved = std::mem::replace(&mut self.e, Emitter::new());
        self.expr(e, prec);
        let out = std::mem::replace(&mut self.e, saved);
        out.buf
    }

    /// `obj.field` with the object borrowed: `obj.0.read().field` / `obj.0.write().field`.
    fn borrowed_field(&mut self, obj: &Expr, field: &str, write: bool) -> Expr {
        let o = self.capture(obj, P_POSTFIX);
        if !write && self.cheap_field(obj, field) {
            return Expr::new(ExprKind::Raw(format!("{}.{}()", o, esc(field))), obj.span);
        }
        let access = if write { "write()" } else { "read()" };
        Expr::new(ExprKind::Raw(format!("{}.0.{}.{}", o, access, esc(field))), obj.span)
    }

    /// The `obj.field` at the bottom of a field/index/method chain, if `obj` is shared.
    fn shared_base(&self, e: &Expr) -> Option<(Expr, String)> {
        match &e.kind {
            ExprKind::Field { recv, name, safe: false } => match self.shared_field(recv, name) {
                Some(_) => Some(((**recv).clone(), name.clone())),
                None => self.shared_base(recv),
            },
            ExprKind::Index(r, _) | ExprKind::MethodCall { recv: r, safe: false, .. } => self.shared_base(r),
            _ => None,
        }
    }

    /// Replaces the shared `obj.field` at the bottom of a chain with a borrowed base.
    fn rebase(&self, e: &Expr, base: &Expr) -> Expr {
        let mut out = e.clone();
        match &mut out.kind {
            ExprKind::Field { recv, name, safe: false } if self.shared_field(recv, name).is_some() => return base.clone(),
            ExprKind::Field { recv, .. } | ExprKind::Index(recv, _) | ExprKind::MethodCall { recv, .. } => {
                **recv = self.rebase(recv, base);
            }
            _ => {}
        }
        out
    }

    /// Lowers reads and calls through a shared handle's fields. Returns true if `e` was handled.
    pub(crate) fn shared_access(&mut self, e: &Expr) -> bool {
        match &e.kind {
            // a plain read: the getter
            ExprKind::Field { recv, name, safe: false } => {
                if self.shared_field(recv, name).is_some() {
                    self.e.mark(e.span);
                    self.expr(recv, P_POSTFIX);
                    self.e.w(&format!(".{}()", esc(name)));
                    return true;
                }
                // `obj.pos.x`: read through one borrow instead of copying `pos`
                self.chain_through_borrow(e, false)
            }
            ExprKind::Index(..) => self.chain_through_borrow(e, false),
            ExprKind::MethodCall { recv, name, args, safe: false, .. } => {
                let ExprKind::Field { recv: obj, name: field, safe: false } = &recv.kind else {
                    return self.chain_through_borrow(e, false);
                };
                if self.shared_field(obj, field).is_none() {
                    return self.chain_through_borrow(e, false);
                }
                if !sema::MUTATING_METHODS.contains(&name.as_str()) {
                    return self.chain_through_borrow(e, false);
                }
                // `obj.items.push(x)`: evaluate the arguments first, then borrow mutably for the call
                self.e.mark(e.span);
                let mut hoisted = Vec::new();
                let mut new_args = Vec::new();
                for (i, a) in args.iter().enumerate() {
                    let simple = matches!(a.value.kind, ExprKind::Lambda { .. } | ExprKind::Int(_) | ExprKind::Float(_) | ExprKind::Str(_) | ExprKind::Char(_) | ExprKind::Bool(_))
                        || a.value.ident().is_some();
                    if simple {
                        new_args.push(a.clone());
                    } else {
                        let v = format!("__arg{}", i);
                        let text = self.capture(&a.value, P_LOWEST);
                        hoisted.push(format!("let {} = {};", v, text));
                        new_args.push(Arg { name: a.name.clone(), value: Expr::new(ExprKind::Raw(v), a.value.span), trailing: a.trailing });
                    }
                }
                let base = self.borrowed_field(obj, field, true);
                let call = Expr::new(ExprKind::MethodCall { recv: Box::new(base), name: name.clone(), generics: None, args: new_args, safe: false }, e.span);
                if hoisted.is_empty() {
                    self.expr(&call, P_POSTFIX);
                } else {
                    self.e.w(&format!("{{ {} ", hoisted.join(" ")));
                    self.expr(&call, P_LOWEST);
                    self.e.w(" }");
                }
                true
            }
            _ => false,
        }
    }

    /// `obj.items.len()`, `obj.pos.x`, `obj.items[0]`: one borrow for the whole chain.
    fn chain_through_borrow(&mut self, e: &Expr, write: bool) -> bool {
        // only when the shared field is below the top of the chain
        let Some((obj, field)) = self.shared_base(e) else { return false };
        if matches!(&e.kind, ExprKind::Field { recv, name, .. } if self.shared_field(recv, name).is_some()) {
            return false;
        }
        let base = self.borrowed_field(&obj, &field, write);
        let rebased = self.rebase(e, &base);
        self.e.mark(e.span);
        self.expr_inner(&rebased);
        true
    }

    /// Assignments to a shared handle's fields. Returns true if handled.
    pub(crate) fn shared_assign(&mut self, op: Option<BinOp>, target: &Expr, value: &Expr) -> bool {
        // `obj.field = v`: the setter
        if let (None, ExprKind::Field { recv, name, safe: false }) = (op, &target.kind) {
            if self.shared_field(recv, name).is_some() {
                let ty = self.assign_field_type(recv, name);
                self.expr(recv, P_POSTFIX);
                self.e.w(&format!(".set_{}(", name));
                match ty {
                    Some(t) => self.coerce(value, &t, TyPos::Param),
                    None => self.expr(value, P_LOWEST),
                }
                self.e.w(")");
                return true;
            }
        }
        // `obj.count += 1`, `obj.items[0] = v`: evaluate the value, then borrow mutably
        let Some((obj, field)) = self.shared_base(target) else { return false };
        let base = self.borrowed_field(&obj, &field, true);
        let place = self.rebase(target, &base);
        let v = self.capture(value, P_LOWEST);
        self.e.w(&format!("{{ let __value = {}; ", v));
        self.expr_inner(&place);
        let op_text = op.map(|o| format!("{}=", o.as_str())).unwrap_or_else(|| "=".into());
        self.e.w(&format!(" {} __value; }}", op_text));
        true
    }

    fn assign_field_type(&self, recv: &Expr, field: &str) -> Option<Type> {
        let t = self.infer_type(recv)?;
        let name = match &t {
            Type::Ref { inner, .. } => inner.single_name()?.to_string(),
            _ => t.single_name()?.to_string(),
        };
        self.class_info(&name)?.fields.iter().find(|f| f.name == field).map(|f| f.ty.clone())
    }
}
