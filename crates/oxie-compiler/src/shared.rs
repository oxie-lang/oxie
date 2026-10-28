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
use crate::lower_expr::strip_paren;

const COPY_TYPES: &[&str] = &[
    "i8", "i16", "i32", "i64", "i128", "isize", "u8", "u16", "u32", "u64", "u128", "usize", "f32", "f64", "bool", "char", "Int", "Long", "Double", "Float", "Boolean", "Char",
];

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
        let (ptr, inner) = if sync { (self.std_name("std::sync::Arc"), self.std_name("std::sync::Mutex")) } else { (self.std_name("std::rc::Rc"), self.std_name("std::cell::RefCell")) };
        let (cell, new_cell) = (format!("{}<{}<{}>>", ptr, inner, data), format!("{}::new({}::new(", ptr, inner));
        let (clone, partial_eq, eq, hash, hasher) = (
            self.std_name("std::clone::Clone"),
            self.std_name("std::cmp::PartialEq"),
            self.std_name("std::cmp::Eq"),
            self.std_name("std::hash::Hash"),
            self.std_name("std::hash::Hasher"),
        );
        // `read()` / `write()` (`__oxie::SharedCell`, `SharedLock`) explain a conflicting use
        let (read, write) = ("read()", "write()");

        self.e.nl();
        self.docs(&it.docs);
        self.e.w(&format!("{}struct {}{}(pub(crate) {}){};", vis, t.name, g, cell, wc));
        self.e.nl();

        // Clone shares the object; equality and hashing use its identity
        self.e.nl();
        self.e.w(&format!("impl{} {} for {}{} {{\n    fn clone(&self) -> Self {{\n        Self(self.0.clone())\n    }}\n}}", g, clone, handle, wc));
        self.e.nl();
        self.e.w(&format!("impl{} {} for {}{} {{\n    fn eq(&self, other: &Self) -> bool {{\n        {}::ptr_eq(&self.0, &other.0)\n    }}\n}}", g, partial_eq, handle, wc, ptr));
        self.e.nl();
        self.e.w(&format!("impl{} {} for {}{} {{}}", g, eq, handle, wc));
        self.e.nl();
        self.e.w(&format!(
            "impl{} {} for {}{} {{\n    fn hash<H: {}>(&self, state: &mut H) {{\n        ({}::as_ptr(&self.0) as *const () as usize).hash(state)\n    }}\n}}",
            g, hash, handle, wc, hasher, ptr
        ));
        self.e.nl();
        let has_display = t.members.iter().any(|m| matches!(&m.kind, ItemKind::Fun(f) if (f.name == "toString" || f.name == "to_string") && f.mods.is_override));
        let debug = derives.iter().any(|d| d == "Debug");
        for (tr, needed) in [("Debug", debug), ("Display", has_display)] {
            if needed {
                let (tr, formatter, result) = (self.std_name(&format!("std::fmt::{}", tr)), self.std_name("std::fmt::Formatter"), self.std_name("std::fmt::Result"));
                self.e.w(&format!("impl{} {} for {}{} {{\n    fn fmt(&self, f: &mut {}<'_>) -> {} {{\n        {}::fmt(&*self.0.{}, f)\n    }}\n}}", g, tr, handle, wc, formatter, result, tr, read));
                self.e.nl();
            }
        }
        // without its own `Debug`, the handle prints as `Node(..)`, so a data class or list that
        // holds it can still be printed (and objects that refer to each other don't print forever)
        let own_debug = self.class_info(&t.name).is_some_and(|c| c.interfaces.iter().any(|i| i == "Debug"));
        if !debug && !own_debug {
            let (tr, formatter, result) = (self.std_name("std::fmt::Debug"), self.std_name("std::fmt::Formatter"), self.std_name("std::fmt::Result"));
            self.e.w(&format!("impl{} {} for {}{} {{\n    fn fmt(&self, f: &mut {}<'_>) -> {} {{\n        f.write_str(\"{}(..)\")\n    }}\n}}", g, tr, handle, wc, formatter, result, t.name));
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
            "{}{}fn new({}) -> Self {{\n    Self({}{}::new({}))))\n}}",
            self.unused_allowed(&format!("{}::new", t.name)),
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
            // another crate may call those of a library's public fields
            let external = self.k.has_lib && vis.starts_with("pub ") && fvis.starts_with("pub ");
            // kept where something may call it, without `#[allow(dead_code)]` where this class's is called
            let keep = |l: &Self, name: &str| l.crate_calls.is_none() || external || l.crate_calls(name);
            let weak = self.weak_parts(ty).filter(|_| f.is_weak());
            if !method_names.contains(&f.name.as_str()) && keep(self, &f.name) {
                let allow = self.unused_allowed(&format!("{}::{}", t.name, f.name));
                self.e.nl();
                if weak.is_some() {
                    let place = format!("self.0.{}.{}", read, name);
                    self.e.w(&format!("{}{}fn {}(&self) -> {} {{\n    {}\n}}", allow, fvis, name, fty, self.weak_upgrade(&place, ty)));
                } else if self.is_cloneable(ty, &tparams) {
                    let bound = match ty.single_name() {
                        Some(n) if tparams.iter().any(|p| p == n) => format!(" where {}: {}", n, clone),
                        _ => String::new(),
                    };
                    // numbers, references and the like are copied without `.clone()`
                    let copy = matches!(ty, Type::Ref { mutable: false, .. }) || ty.single_name().is_some_and(|n| COPY_TYPES.contains(&n));
                    let clone_call = if copy { "" } else { ".clone()" };
                    self.e.w(&format!("{}{}fn {}(&self) -> {}{} {{\n    self.0.{}.{}{}\n}}", allow, fvis, name, fty, bound, read, name, clone_call));
                } else if !sync {
                    let r = self.std_name("std::cell::Ref");
                    self.e.w(&format!("{}{}fn {}(&self) -> {}<'_, {}> {{\n    {}::map(self.0.read(), |d| &d.{})\n}}", allow, fvis, name, r, fty, r, name));
                }
                self.e.nl();
            }
            let setter = format!("set_{}", f.name);
            if !method_names.contains(&setter.as_str()) && keep(self, &setter) {
                self.e.nl();
                let value = weak.as_ref().map_or("value".to_string(), |w| Self::weak_from("value", w));
                self.e.w(&format!("{}{}fn {}(&self, value: {}) {{\n    self.0.{}.{} = {};\n}}", self.unused_allowed(&format!("{}::{}", t.name, setter)), fvis, esc(&setter), self.ty(ty, TyPos::Field), write, name, value));
                self.e.nl();
            }
        }
        // forwarding methods
        let inherent: Vec<&Item> = t.members.iter().filter(|m| !forwarded.iter().any(|(_, ms)| ms.iter().any(|x| std::ptr::eq(*x, *m)))).collect();
        for m in inherent {
            let ItemKind::Fun(f) = &m.kind else { continue };
            // `Drop` is the data's: the handle has nothing to forward
            let drop = f.name == "drop" && f.params.is_empty() && t.supertypes.iter().any(|s| s.last_name() == Some("Drop"));
            if f.mods.is_operator || (f.mods.is_override && (f.name == "toString" || f.name == "to_string")) || drop {
                continue;
            }
            let recv = self.recv_of(&t.name, f);
            if matches!(recv, Recv::Static | Recv::Value) {
                continue;
            }
            self.e.nl();
            self.forwarder(m, f, recv, recv, mvis, read, write);
        }
        // static methods live on the handle
        for m in statics {
            if let ItemKind::Fun(f) = &m.kind {
                self.e.nl();
                self.method(m, f, Recv::Static, true, None);
            }
        }
        self.e.dedent();
        self.e.line_break();
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
                let data_recv = self.recv_of(&t.name, f);
                let recv = trait_info.as_ref().and_then(|ti| ti.methods.get(&f.name)).map(|mi| mi.recv).unwrap_or(data_recv);
                if matches!(recv, Recv::Static | Recv::Value) {
                    continue;
                }
                self.e.nl();
                self.pending_trait_ret = crate::lower::trait_ret(tr, trait_info.as_ref(), &f.name);
                self.forwarder(m, f, recv, data_recv, "", read, write);
            }
            self.e.dedent();
            self.e.line_break();
            self.e.w("}");
            self.e.nl();
        }
    }

    /// `fn name(&self, a: A) -> R { self.0.write().name(a) }`
    /// A method of the handle calling the data's: `recv` is the handle method's receiver,
    /// `data_recv` the data method's (which decides whether it locks for writing).
    fn forwarder(&mut self, it: &Item, f: &FunDecl, recv: Recv, data_recv: Recv, vis: &str, read: &str, write: &str) {
        let trait_ret = self.pending_trait_ret.take();
        let f = &auto_lifetimes(&self.with_inferred_ret(f, trait_ret), true, &|n| self.k.type_lifetimes_in(n, &self.module));
        let f = &match self.class.as_ref() {
            Some(c) if !f.mods.is_override => {
                let fields: Vec<(String, Type)> = c.fields.iter().map(|(n, t)| (n.clone(), t.clone())).collect();
                crate::bounds::with_inferred_bounds(f, &c.generics, &fields, &[])
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
        let access = if data_recv == Recv::RefMut { write } else { read };
        // methods of trait impls are never unused
        let owner = self.class.as_ref().map(|c| c.name.clone()).unwrap_or_default();
        let allow = if vis.is_empty() { String::new() } else { self.unused_allowed(&format!("{}::{}", owner, f.name)) };
        self.e.w(&format!("{}{}{}fn {}{}({})", allow, vis, header, esc(&f.name), g, params));
        if let Some(r) = ret {
            self.e.w(&format!(" -> {}", r));
        }
        self.e.w(&format!("{} {{\n    self.0.{}.{}({})\n}}", wc, access, esc(&f.name), args.join(", ")));
        self.e.nl();
    }

    /// `#[allow(dead_code)]` for a generated item named `name`, unless the crate calls it.
    pub(crate) fn unused_allowed(&self, name: &str) -> String {
        if self.crate_calls(name) { String::new() } else { "#[allow(dead_code)]\n".into() }
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

    /// The shared class whose handle `recv` is or refers to.
    pub(crate) fn shared_class_name(&self, recv: &Expr) -> Option<String> {
        let t = self.infer_type(recv)?;
        let name = match &t {
            Type::Ref { inner, .. } => inner.single_name()?,
            _ => t.single_name()?,
        };
        self.class_info(name).filter(|c| c.is_shared).map(|_| name.to_string())
    }

    /// Records a call of `member` (a method, getter or setter) on the handle `recv`: by name, and
    /// as `Class::member` where Oxie knows the class.
    pub(crate) fn record_member(&mut self, recv: &Expr, member: &str) {
        self.calls.insert(member.to_string());
        if let Some(c) = self.shared_class_name(recv) {
            self.calls.insert(format!("{}::{}", c, member));
        }
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
        if self.weak_read(e).is_some() {
            return false; // upgraded, so already a copy of the handle
        }
        let optional_handle = || matches!(self.infer_type(e), Some(Type::Option(t)) if t.single_name().and_then(|n| self.class_info(n)).is_some_and(|c| c.is_shared));
        self.is_place(e) && !matches!(e.kind, ExprKind::SelfValue) && (self.shared_class_of(e).is_some() || optional_handle())
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
    pub(crate) fn borrowed_field(&mut self, obj: &Expr, field: &str, write: bool) -> Expr {
        let o = self.capture(obj, P_POSTFIX);
        if !write && self.cheap_field(obj, field) {
            self.record_member(obj, field);
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

    /// Does `e` read through a shared handle's field in one chain (`m.data.len()`), whose borrow
    /// Rust keeps until the end of the statement? A plain field read goes through the getter,
    /// which ends its borrow inside.
    pub(crate) fn holds_shared_borrow(&self, e: &Expr) -> bool {
        let mut e = e;
        while let ExprKind::Paren(x) = &e.kind {
            e = x;
        }
        let getter = matches!(&e.kind, ExprKind::Field { recv, name, safe: false } if self.shared_field(recv, name).is_some());
        !getter && self.shared_base(e).is_some()
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
                    self.record_member(recv, name);
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
                // the value put into a list or map field gets the element's type (`"two"` into a
                // `[String]`), as it would on a plain object
                let elem = match (self.assign_field_type(obj, field), name.as_str(), args.len()) {
                    (Some(Type::Path { segs, .. }), m, n) => match (segs.last().map(|s| s.name.as_str()), segs.last().and_then(|s| s.generics.clone()).as_deref(), m, n) {
                        (Some("Vec" | "VecDeque"), Some([GenericArg::Type(t)]), "push" | "push_back" | "push_front", 1) => Some((0, t.clone())),
                        (Some("Vec" | "VecDeque"), Some([GenericArg::Type(t)]), "insert", 2) => Some((1, t.clone())),
                        (Some("HashMap" | "BTreeMap"), Some([GenericArg::Type(k), GenericArg::Type(v)]), "insert", 2) => Some((usize::MAX, Type::Tuple(vec![k.clone(), v.clone()]))),
                        (Some("HashSet" | "BTreeSet"), Some([GenericArg::Type(t)]), "insert", 1) => Some((0, t.clone())),
                        _ => None,
                    },
                    _ => None,
                };
                let elem_ty = |i: usize| match &elem {
                    Some((usize::MAX, Type::Tuple(kv))) => kv.get(i).cloned(),
                    Some((j, t)) if *j == i => Some(t.clone()),
                    _ => None,
                };
                let mut hoisted = Vec::new();
                let mut new_args = Vec::new();
                for (i, a) in args.iter().enumerate() {
                    let simple = matches!(a.value.kind, ExprKind::Lambda { .. } | ExprKind::Int(_) | ExprKind::Float(_) | ExprKind::Str(_) | ExprKind::Char(_) | ExprKind::Bool(_))
                        || a.value.ident().is_some();
                    let coerced = |l: &mut Self, t: &Type| {
                        let saved = std::mem::replace(&mut l.e, crate::emit::Emitter::new());
                        l.coerce(&a.value, t, TyPos::Generic);
                        std::mem::replace(&mut l.e, saved).buf
                    };
                    match elem_ty(i) {
                        Some(t) if simple => {
                            let text = coerced(self, &t);
                            new_args.push(Arg { name: a.name.clone(), value: Expr::new(ExprKind::Raw(text), a.value.span), trailing: a.trailing });
                        }
                        _ if simple => new_args.push(a.clone()),
                        t => {
                            let v = format!("__arg{}", i);
                            let text = match t {
                                Some(t) => coerced(self, &t),
                                None => self.capture(&a.value, P_LOWEST),
                            };
                            hoisted.push(format!("let {} = {};", v, text));
                            new_args.push(Arg { name: a.name.clone(), value: Expr::new(ExprKind::Raw(v), a.value.span), trailing: a.trailing });
                        }
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

    /// `obj.items.len()`, `obj.pos.x`, `obj.items[0]`: one borrow for the whole chain; a
    /// mutable one when a call in it changes what it is called on
    /// (`obj.counts.entry(k).or_insert(0)`, `obj.items.iter_mut()`).
    fn chain_through_borrow(&mut self, e: &Expr, write: bool) -> bool {
        // only when the shared field is below the top of the chain
        let Some((obj, field)) = self.shared_base(e) else { return false };
        let write = write || chain_changes(e);
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
                self.record_member(recv, &format!("set_{}", name));
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
        let op_text = op.map(|o| format!("{}=", o.as_str())).unwrap_or_else(|| "=".into());
        // a literal or a variable can't reach the object while it is borrowed
        let simple = match &value.kind {
            ExprKind::Int(_) | ExprKind::Float(_) | ExprKind::Char(_) | ExprKind::Bool(_) | ExprKind::RawStr(_) => true,
            ExprKind::Str(parts) => parts.iter().all(|p| matches!(p, crate::lexer::StrPart::Lit(_))),
            _ => value.ident().is_some_and(|n| self.is_local(n)),
        };
        if simple {
            self.expr_inner(&place);
            self.e.w(&format!(" {} ", op_text));
            self.expr(value, P_LOWEST);
            return true;
        }
        let v = self.capture(value, P_LOWEST);
        self.e.w(&format!("{{ let __value = {}; ", v));
        self.expr_inner(&place);
        self.e.w(&format!(" {} __value; }}", op_text));
        true
    }

    // ------------------------------------------------------------------ @weak fields

    /// The Rust of a `@weak` field of type `ty` (`Node?`), or None when `ty` isn't an optional
    /// shared class.
    pub(crate) fn weak_parts(&self, ty: &Type) -> Option<WeakParts> {
        let Type::Option(inner) = ty else { return None };
        let Type::Path { global, segs, span } = &**inner else { return None };
        let c = self.class_info(&segs.last()?.name).filter(|c| c.is_shared)?;
        let (ptr, weak, cell) = if c.is_sync { ("std::sync::Arc", "std::sync::Weak", "std::sync::Mutex") } else { ("std::rc::Rc", "std::rc::Weak", "std::cell::RefCell") };
        let mut data = segs.clone();
        data.last_mut()?.name = Self::shared_data_name(&c.name);
        let data = self.ty(&Type::Path { global: *global, segs: data, span: *span }, TyPos::Field);
        let mut handle = segs.clone();
        handle.last_mut()?.generics = None;
        let handle = self.ty(&Type::Path { global: *global, segs: handle, span: *span }, TyPos::Plain);
        let weak = self.std_name(weak);
        Some(WeakParts { ty: format!("{}<{}<{}>>", weak, self.std_name(cell), data), new: format!("{}::new", weak), downgrade: format!("{}::downgrade", self.std_name(ptr)), handle })
    }

    /// The type of the `@weak` field `name` of the class being lowered (seen from its methods).
    pub(crate) fn own_weak_field(&self, name: &str) -> Option<Type> {
        let c = self.class.as_ref()?;
        if c.is_static || self.shared_handle_ty.is_none() || self.is_local(name) {
            return None;
        }
        let info = self.class_info(&c.name)?;
        info.fields.iter().find(|f| f.name == name && f.weak).map(|f| f.ty.clone())
    }

    /// A read of a `@weak` field inside its class's methods (`parent`, `this.parent`): its type.
    pub(crate) fn weak_read(&self, e: &Expr) -> Option<Type> {
        match &strip_paren(e).kind {
            ExprKind::Path { segs, generics: None, global: false } if segs.len() == 1 && self.implicit_field(&segs[0]).is_some() => self.own_weak_field(&segs[0]),
            ExprKind::Field { recv, name, safe: false } if matches!(recv.kind, ExprKind::SelfValue) => self.own_weak_field(name),
            _ => None,
        }
    }

    /// `self.parent.upgrade().map(Node)`: the field `place` read as an optional handle.
    pub(crate) fn weak_upgrade(&self, place: &str, ty: &Type) -> String {
        match self.weak_parts(ty) {
            Some(w) => format!("{}.upgrade().map({})", place, w.handle),
            None => place.to_string(),
        }
    }

    /// `this.parent` inside the class: the upgraded handle. Returns true if `e` was handled.
    pub(crate) fn weak_field_read(&mut self, e: &Expr) -> bool {
        let ExprKind::Field { name, .. } = &e.kind else { return false };
        let (Some(ty), Some(c)) = (self.weak_read(e), self.class.as_ref()) else { return false };
        let place = format!("{}.{}", c.self_name, esc(name));
        self.e.mark(e.span);
        self.e.w(&self.weak_upgrade(&place, &ty));
        true
    }

    /// The value `e` of type `ty` (`Node?`) as the `Weak` a `@weak` field holds: `Weak::new()`
    /// for `null`, `Rc::downgrade(&node.0)` for a handle.
    pub(crate) fn weak_value(&mut self, e: &Expr, ty: &Type) -> String {
        let Some(w) = self.weak_parts(ty) else { return self.capture(e, P_LOWEST) };
        if matches!(strip_paren(e).kind, ExprKind::Null) {
            return format!("{}()", w.new);
        }
        if self.shared_class_of(e).is_some() {
            // only borrowed for the downgrade, so no copy of the handle
            let h = self.capture(e, P_POSTFIX);
            let h = h.strip_suffix(".clone()").unwrap_or(&h);
            return format!("{}(&{}.0)", w.downgrade, h);
        }
        let saved = std::mem::replace(&mut self.e, Emitter::new());
        self.coerce(e, ty, TyPos::Field);
        let v = std::mem::replace(&mut self.e, saved).buf;
        Self::weak_from(&v, &w)
    }

    /// `value.map_or_else(Weak::new, |n| Rc::downgrade(&n.0))` for an optional handle `value`.
    pub(crate) fn weak_from(value: &str, w: &WeakParts) -> String {
        format!("{}.map_or_else({}, |n| {}(&n.0))", value, w.new, w.downgrade)
    }

    /// `parent = v` / `this.parent = v` inside the class. Returns true if handled.
    pub(crate) fn weak_assign(&mut self, op: Option<BinOp>, target: &Expr, value: &Expr) -> bool {
        let Some(ty) = self.weak_read(target) else { return false };
        let name = match &strip_paren(target).kind {
            ExprKind::Path { segs, .. } => segs[0].clone(),
            ExprKind::Field { name, .. } => name.clone(),
            _ => return false,
        };
        if op.is_some() {
            return false;
        }
        let self_name = self.class.as_ref().map_or("self".to_string(), |c| c.self_name.clone());
        let v = self.weak_value(value, &ty);
        self.e.mark(target.span);
        self.e.w(&format!("{}.{} = {}", self_name, esc(&name), v));
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

/// What a `@weak` field of a shared class is written with.
pub(crate) struct WeakParts {
    /// `Weak<RefCell<NodeData>>`
    pub(crate) ty: String,
    /// `Weak::new`
    pub(crate) new: String,
    /// `Rc::downgrade`
    pub(crate) downgrade: String,
    /// `Node`, the handle's constructor
    pub(crate) handle: String,
}

/// Does a call in the chain `e` change what it is called on (`entry`, `iter_mut`, `push`)?
fn chain_changes(e: &Expr) -> bool {
    match &e.kind {
        ExprKind::MethodCall { recv, name, .. } => sema::MUTATING_METHODS.contains(&name.as_str()) || chain_changes(recv),
        ExprKind::Field { recv, .. } | ExprKind::Index(recv, _) => chain_changes(recv),
        _ => false,
    }
}
