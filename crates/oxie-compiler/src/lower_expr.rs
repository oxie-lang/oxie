//! Lowering of statements and expressions.

use crate::ast::*;
use crate::diag::{Diag, Span};
use crate::lower::*;
use crate::parser::is_type_like;
use crate::sema::{ParamInfo, Recv, VariantShape};
use crate::visit;

fn is_const_like(s: &str) -> bool {
    s.len() > 1 && s.chars().next().is_some_and(|c| c.is_uppercase()) && s.chars().all(|c| c.is_uppercase() || c == '_' || c.is_ascii_digit())
}

/// Methods whose return type is inferred from the expected type; `.into()` would break that.
const INFERENCE_DEPENDENT: &[&str] = &[
    "parse", "collect", "into", "try_into", "sum", "product", "unwrap_or_default", "default", "from", "ok", "err", "map", "and_then",
    "get", "first", "last", "find", "max", "min", "pop", "next", "cloned", "copied", "as_ref", "as_deref", "take", "filter",
];

/// Rust string literal text for a format string: braces doubled, except those of `\u{..}`.
fn escape_braces(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' => {
                out.push(c);
                let Some(n) = chars.next() else { break };
                out.push(n);
                if n == 'u' && chars.peek() == Some(&'{') {
                    for d in chars.by_ref() {
                        out.push(d);
                        if d == '}' {
                            break;
                        }
                    }
                }
            }
            '{' => out.push_str("{{"),
            '}' => out.push_str("}}"),
            _ => out.push(c),
        }
    }
    out
}

/// The type of an `if`/`when` value nothing declares: a `T?` when one of its branches is `null`
/// (`if (c) x else null`), so the other branches become `Some(x)`, and a `String` when some
/// branches are templates and others literals (`if (c) "none" else "$n items"`).
fn branch_target(e: &Expr) -> Target {
    #[derive(Default)]
    struct Tails {
        null: bool,
        literal: bool,
        template: bool,
    }
    fn walk(e: &Expr, t: &mut Tails) {
        let tail = |b: &Block, t: &mut Tails| {
            if let Some(Stmt::Expr { expr, semi: false }) = b.stmts.last() {
                walk(expr, t)
            }
        };
        match &e.kind {
            ExprKind::Null => t.null = true,
            ExprKind::Str(_) | ExprKind::RawStr(_) => t.literal = true,
            ExprKind::Template(_) => t.template = true,
            ExprKind::Paren(x) => walk(x, t),
            ExprKind::Block(b) => tail(b, t),
            ExprKind::If { then, else_: Some(el), .. } => {
                tail(then, t);
                walk(el, t);
            }
            ExprKind::When { arms, .. } => arms.iter().for_each(|a| walk(&a.body, t)),
            _ => {}
        }
    }
    let mut t = Tails::default();
    walk(e, &mut t);
    let text = t.literal && t.template;
    match (t.null, text) {
        (true, true) => Target::ty(&Type::Option(Box::new(Type::simple("String")))),
        (true, false) => Target::ty(&Type::Option(Box::new(Type::Infer))),
        (false, true) => Target::ty(&Type::simple("String")),
        (false, false) => Target::none(),
    }
}

fn is_ok_err_call(e: &Expr) -> bool {
    match &e.kind {
        ExprKind::Call { callee, .. } => matches!(callee.ident(), Some("Ok") | Some("Err")),
        ExprKind::Paren(x) => is_ok_err_call(x),
        _ => false,
    }
}

fn is_literal(e: &Expr) -> bool {
    matches!(e.kind, ExprKind::Int(_) | ExprKind::Float(_) | ExprKind::Bool(_) | ExprKind::Char(_) | ExprKind::Str(_) | ExprKind::RawStr(_))
        || matches!(&e.kind, ExprKind::Unary(UnOp::Neg, x) if is_literal(x))
}

/// Methods whose closure receives a reference to each element (`filter` on an iterator even
/// gets `&&T`). Comparisons and arithmetic with literals in these lambdas read the value.
const REF_PREDICATES: &[&str] = &[
    "filter", "filter_not", "find", "find_last", "position", "index_of_first", "any", "all", "none", "count", "take_while",
    "skip_while", "retain", "partition", "is_some_and",
];

/// A number, `char` or `bool` literal (possibly negated).
fn is_scalar_literal(e: &Expr) -> bool {
    match &e.kind {
        ExprKind::Int(_) | ExprKind::Float(_) | ExprKind::Char(_) | ExprKind::Bool(_) => true,
        ExprKind::Unary(UnOp::Neg, x) => matches!(x.kind, ExprKind::Int(_) | ExprKind::Float(_)),
        ExprKind::Paren(x) => is_scalar_literal(x),
        _ => false,
    }
}

/// What `x in coll` looks in: a map's keys, text, or another collection's elements.
#[derive(PartialEq)]
enum Contains {
    Key,
    Text,
    Value,
}

fn is_string_lit(e: &Expr) -> bool {
    matches!(e.kind, ExprKind::Str(_) | ExprKind::Template(_))
}

/// Postfix chain: base expression and links (innermost first).
fn chain_links(e: &Expr) -> (&Expr, Vec<&Expr>) {
    let mut links = Vec::new();
    let mut cur = e;
    loop {
        match &cur.kind {
            ExprKind::Field { recv, .. } | ExprKind::MethodCall { recv, .. } => {
                links.push(cur);
                cur = recv;
            }
            ExprKind::Index(recv, _) => {
                links.push(cur);
                cur = recv;
            }
            _ => break,
        }
    }
    links.reverse();
    (cur, links)
}

fn link_is_safe(l: &Expr) -> bool {
    matches!(l.kind, ExprKind::Field { safe: true, .. } | ExprKind::MethodCall { safe: true, .. })
}

fn has_safe_link(e: &Expr) -> bool {
    let (_, links) = chain_links(e);
    links.iter().any(|l| link_is_safe(l))
}

/// Std types that are never `Copy`: a method call on a field of one borrows the field (or moves
/// it out of `this`, which rustc rejects too).
const NEVER_COPY: &[&str] = &[
    "String", "Vec", "VecDeque", "LinkedList", "HashMap", "HashSet", "BTreeMap", "BTreeSet", "BinaryHeap", "Box", "Rc", "Arc",
    "Cell", "RefCell", "Mutex", "RwLock",
];

/// The method or function a call like `next_id()`, `this.next_id()` or `next_id()?` calls.
fn called_name(e: &Expr) -> Option<&str> {
    match &e.kind {
        ExprKind::Try(x) | ExprKind::Unwrap(x) | ExprKind::Await(x) => called_name(x),
        ExprKind::Call { callee, .. } => callee.ident(),
        ExprKind::MethodCall { name, .. } => Some(name),
        _ => None,
    }
}

/// Replaces the calls at the `moved` spans with the locals they were moved into, in the parts
/// of an expression that `calls_changing_this` looks at.
fn replace_moved(e: &mut Expr, moved: &[(Span, String)]) {
    if let Some((_, name)) = moved.iter().find(|(s, _)| *s == e.span).filter(|_| called_name(e).is_some()) {
        *e = Expr::new(ExprKind::Path { segs: vec![name.clone()], generics: None, global: false }, e.span);
        return;
    }
    match &mut e.kind {
        ExprKind::Call { args, .. } => args.iter_mut().for_each(|a| replace_moved(&mut a.value, moved)),
        ExprKind::MethodCall { recv, args, .. } => {
            replace_moved(recv, moved);
            args.iter_mut().for_each(|a| replace_moved(&mut a.value, moved));
        }
        ExprKind::Paren(x) | ExprKind::Cast(x, _) | ExprKind::Unary(_, x) | ExprKind::Field { recv: x, .. } | ExprKind::Try(x) | ExprKind::Unwrap(x) | ExprKind::Await(x) => replace_moved(x, moved),
        ExprKind::Binary(_, a, b) | ExprKind::Index(a, b) | ExprKind::Pair(a, b) => {
            replace_moved(a, moved);
            replace_moved(b, moved);
        }
        ExprKind::Template(parts) => {
            for p in parts {
                if let TemplatePart::Expr(x, _) = p {
                    replace_moved(x, moved);
                }
            }
        }
        ExprKind::Tuple(v) | ExprKind::Array(v) | ExprKind::VecLit(v) | ExprKind::SetLit(v) | ExprKind::MapLit(v) => v.iter_mut().for_each(|x| replace_moved(x, moved)),
        ExprKind::StructLit { fields, base, .. } => {
            fields.iter_mut().filter_map(|(_, v)| v.as_mut()).for_each(|v| replace_moved(v, moved));
            if let Some(b) = base {
                replace_moved(b, moved);
            }
        }
        _ => {}
    }
}

impl<'a> Lowerer<'a> {
    // ------------------------------------------------------------------ statements

    /// Emits statements on new lines. With `tail`, the last expression statement is the block value.
    pub(crate) fn stmts_with_tail(&mut self, stmts: &[Stmt], tail: Option<&Target>) {
        let tail_idx = match tail {
            Some(_) => match stmts.last() {
                Some(Stmt::Expr { semi: false, .. }) => Some(stmts.len() - 1),
                _ => None,
            },
            None => None,
        };
        // a function declared in the block can be called anywhere in it, and so can what it imports
        let outer_funs = self.local_funs.len();
        let outer_imports = self.local_imports.len();
        for s in stmts {
            if let Stmt::Item(b) = s {
                match &b.0.kind {
                    ItemKind::Fun(f) => self.local_funs.push((f.name.clone(), crate::sema::params_info(f))),
                    ItemKind::Import(d) => imported_names(&d.tree, &[], &mut self.local_imports),
                    _ => {}
                }
            }
        }
        for (i, s) in stmts.iter().enumerate() {
            self.e.nl();
            if Some(i) == tail_idx {
                if let Stmt::Expr { expr, .. } = s {
                    self.e.mark(expr.span);
                    self.tail_moving_args(expr, tail.unwrap());
                }
            } else {
                self.stmt(s);
            }
        }
        self.local_funs.truncate(outer_funs);
        self.local_imports.truncate(outer_imports);
    }

    fn stmt(&mut self, s: &Stmt) {
        match s {
            Stmt::Let { pat, mutable: _, ty, init, else_, span } => {
                if let Some(init) = init.as_ref().filter(|_| else_.is_none()) {
                    let mut taken = Vec::new();
                    pat.bindings(&mut taken);
                    if let Some((init, bound)) = self.move_args_changing_this(init, &taken) {
                        self.e.mark(*span);
                        self.let_stmt(pat, ty.as_ref(), Some(&init), None);
                        self.unbind_moved(bound);
                        return;
                    }
                }
                self.e.mark(*span);
                self.let_stmt(pat, ty.as_ref(), init.as_ref(), else_.as_ref());
            }
            Stmt::Expr { expr, semi } => {
                if let Some((e, bound)) = self.move_args_changing_this(expr, &[]) {
                    self.stmt(&Stmt::Expr { expr: e, semi: *semi });
                    self.unbind_moved(bound);
                    return;
                }
                self.e.mark(expr.span);
                if self.passes_errors_on() && (self.is_throwing_call(expr) || self.is_rust_result_call(expr)) {
                    self.propagate(expr);
                    self.e.w(";");
                    return;
                }
                // the arguments were moved out above, or stay (`keep_args`)
                let block_like = self.stmt_expr_here(expr);
                if !block_like || *semi {
                    self.e.w(";");
                }
            }
            Stmt::Item(b) => {
                self.item(&b.0, ItemCtx::Local);
            }
            Stmt::Attributed { attrs, stmt } => {
                for a in attrs {
                    self.e.w(&format!("#[{}]", a));
                    self.e.nl();
                }
                self.keep_args = matches!(**stmt, Stmt::Expr { .. } | Stmt::Let { init: Some(_), else_: None, .. });
                self.stmt(stmt);
                self.keep_args = false;
            }
        }
    }

    fn let_stmt(&mut self, pat: &Pat, ty: Option<&Type>, init: Option<&Expr>, else_: Option<&Block>) {
        let mut p = self.pat(pat);
        // closures that assign to captured variables are FnMut and must be `let mut` to be called
        if let (Some(Expr { kind: ExprKind::Lambda { body, .. }, .. }), Pat::Ident { mutable: false, .. }) = (init, pat) {
            if visit::has_assignment(body) {
                p = format!("mut {}", p);
            }
        }
        self.e.w(&format!("let {}", p));
        let fn_lambda = matches!(ty, Some(Type::Fn { .. })) && init.is_some_and(|e| matches!(e.kind, ExprKind::Lambda { .. } | ExprKind::Path { .. }));
        if let Some(t) = ty {
            if !fn_lambda {
                let tt = self.ty(t, TyPos::Local);
                self.e.w(&format!(": {}", tt));
            }
        }
        if let Some(e) = init {
            self.e.w(" = ");
            match ty {
                Some(Type::Fn { params, ret }) if fn_lambda => {
                    if matches!(e.kind, ExprKind::Lambda { .. }) {
                        self.lambda(e, Some(params), Some(ret));
                    } else {
                        self.expr(e, P_LOWEST);
                    }
                }
                Some(t) => self.coerce(e, t, TyPos::Local),
                None if self.is_shared_place(e) => self.shared_copy(e),
                // a lambda in a variable: Rust infers its parameters' types from its body
                None if matches!(e.kind, ExprKind::Lambda { .. }) => {
                    self.typed_by_body = true;
                    self.expr(e, P_LOWEST);
                    self.typed_by_body = false;
                }
                None => self.expr(e, P_LOWEST),
            }
        }
        if let Some(b) = else_ {
            self.e.w(" else ");
            self.block_stmt(b);
        }
        self.e.w(";");
        let known = ty.cloned().or_else(|| init.and_then(|e| self.infer_type(e))).or_else(|| init.and_then(|e| self.generic_ctor_type(e)));
        self.bind_pat(pat, known);
    }

    /// The calls to move into locals before a statement `e` that calls a method on a field of
    /// `this` (`items.push(next_id())`) whose arguments call methods of `this` taking `&mut self`.
    /// Rust borrows the field for the call before it runs the arguments, so rustc rejects the
    /// statement as written. Only when the call borrows the field (`borrowed_by`): a `Copy`
    /// field would be copied first and the statement compile. Also returns the field's name.
    fn args_changing_this<'e>(&self, e: &'e Expr) -> Option<(Vec<&'e Expr>, &'e str)> {
        let ExprKind::MethodCall { recv, name, args, safe: false, .. } = &e.kind else { return None };
        let c = self.class.as_ref()?;
        if c.is_static || self.self_by_value || args.iter().any(|a| a.name.is_some()) {
            return None;
        }
        let field = match &recv.kind {
            ExprKind::Field { recv: r, name: f, safe: false } if matches!(r.kind, ExprKind::SelfValue) => f.as_str(),
            _ => recv.ident().filter(|n| self.implicit_field(n).is_some())?,
        };
        let ty = match c.fields.get(field)? {
            // a property without a type has the type of its default (`var cart = Cart()`)
            Type::Infer => self.class_info(&c.name)?.fields.iter().find(|f| f.name == field)?.default.as_ref().and_then(infer_field_type)?,
            t => t.clone(),
        };
        if !self.borrowed_by(&ty, name) {
            return None;
        }
        let mut calls = Vec::new();
        let mut ran = false;
        for a in args {
            self.calls_changing_this(&a.value, &mut calls, &mut ran)?;
        }
        (!calls.is_empty()).then_some((calls, field))
    }

    /// Does calling `method` on a field of type `ty` borrow the field? Yes when the type is never
    /// `Copy` (`never_copy`), and for an Oxie type whose `method` takes `&self` or `&mut self`.
    fn borrowed_by(&self, ty: &Type, method: &str) -> bool {
        if self.never_copy(ty) {
            return true;
        }
        let Some(n) = ty.single_name() else { return false };
        let m = self.class_info(n).and_then(|c| c.methods.get(method)).or_else(|| self.enum_info(n).and_then(|e| e.methods.get(method)));
        m.is_some_and(|m| matches!(m.recv, Recv::Ref | Recv::RefMut))
    }

    /// Is `ty` never `Copy`: a std collection, `String`, `Box`, `&mut T`, a `shared class`
    /// handle, or an `Option` of one?
    fn never_copy(&self, ty: &Type) -> bool {
        match ty {
            Type::Ref { mutable, .. } => *mutable,
            Type::Option(inner) => self.never_copy(inner),
            Type::Path { segs, .. } if segs.len() == 1 => {
                let s = &segs[0];
                match s.name.as_str() {
                    "Option" => matches!(s.generics.as_deref(), Some([GenericArg::Type(t)]) if self.never_copy(t)),
                    n if self.k.is_user_type(n) => self.class_info(n).is_some_and(|c| c.is_shared),
                    n => NEVER_COPY.contains(&n),
                }
            }
            _ => false,
        }
    }

    /// Is `e` a call of a method of `this` that takes `&mut self` (`next_id()`, `this.next_id()`,
    /// with the `?`, `!!` or `.await` after it)?
    fn changes_this(&self, e: &Expr) -> bool {
        let name = match &e.kind {
            ExprKind::Try(x) | ExprKind::Unwrap(x) | ExprKind::Await(x) => return self.changes_this(x),
            ExprKind::Call { callee, .. } => match callee.ident() {
                Some(n) if !self.is_local(n) && self.implicit_field(n).is_none() => n,
                _ => return false,
            },
            ExprKind::MethodCall { recv, name, safe: false, .. } if matches!(recv.kind, ExprKind::SelfValue) => name.as_str(),
            _ => return false,
        };
        self.class.as_ref().and_then(|c| c.methods.get(name)).is_some_and(|m| m.recv == Recv::RefMut)
    }

    /// Walks an argument in the order Rust runs it and collects the calls that change `this`
    /// (`changes_this`), to move into locals. `ran` says that something already ran that could
    /// tell those calls ran earlier: a call, a read of a field, an operator. None leaves the
    /// statement as it is: such a call after something like that, or one that may not run at
    /// all (in a lambda, an `if`, after `&&` or `?.`).
    fn calls_changing_this<'e>(&self, e: &'e Expr, calls: &mut Vec<&'e Expr>, ran: &mut bool) -> Option<()> {
        if self.changes_this(e) {
            if *ran {
                return None;
            }
            calls.push(e);
            return Some(());
        }
        match &e.kind {
            ExprKind::Int(_) | ExprKind::Float(_) | ExprKind::Bool(_) | ExprKind::Char(_) | ExprKind::Str(_) | ExprKind::RawStr(_) | ExprKind::Null => {}
            // a local or a constant can't change, a field of `this` can
            ExprKind::Path { .. } => *ran |= e.ident().is_some_and(|n| self.implicit_field(n).is_some()),
            ExprKind::Paren(x) | ExprKind::Cast(x, _) | ExprKind::Unary(UnOp::Ref | UnOp::RefMut, x) => self.calls_changing_this(x, calls, ran)?,
            ExprKind::Pair(a, b) => {
                self.calls_changing_this(a, calls, ran)?;
                self.calls_changing_this(b, calls, ran)?;
            }
            ExprKind::Tuple(v) | ExprKind::Array(v) | ExprKind::VecLit(v) | ExprKind::SetLit(v) | ExprKind::MapLit(v) => {
                for x in v {
                    self.calls_changing_this(x, calls, ran)?;
                }
            }
            ExprKind::StructLit { fields, base, .. } => {
                for (n, v) in fields {
                    match v {
                        Some(v) => self.calls_changing_this(v, calls, ran)?,
                        None => *ran |= self.implicit_field(n).is_some(),
                    }
                }
                if let Some(b) = base {
                    self.calls_changing_this(b, calls, ran)?;
                }
            }
            // the operands, then the operation itself (which may be a user's `Add::add`)
            ExprKind::Binary(op, a, b) if !matches!(op, BinOp::And | BinOp::Or) => {
                self.calls_changing_this(a, calls, ran)?;
                self.calls_changing_this(b, calls, ran)?;
                *ran = true;
            }
            ExprKind::Index(a, b) => {
                self.calls_changing_this(a, calls, ran)?;
                self.calls_changing_this(b, calls, ran)?;
                *ran = true;
            }
            ExprKind::Unary(_, x) | ExprKind::Try(x) | ExprKind::Unwrap(x) | ExprKind::Await(x) | ExprKind::Field { recv: x, safe: false, .. } => {
                self.calls_changing_this(x, calls, ran)?;
                *ran = true;
            }
            ExprKind::Template(parts) => {
                for p in parts {
                    if let TemplatePart::Expr(x, _) = p {
                        self.calls_changing_this(x, calls, ran)?;
                    }
                }
                *ran = true;
            }
            ExprKind::Call { callee, args } if matches!(callee.kind, ExprKind::Path { .. }) && args.iter().all(|a| a.name.is_none()) => {
                for a in args {
                    self.calls_changing_this(&a.value, calls, ran)?;
                }
                *ran = true;
            }
            ExprKind::MethodCall { recv, args, safe: false, .. } if args.iter().all(|a| a.name.is_none()) => {
                self.calls_changing_this(recv, calls, ran)?;
                for a in args {
                    self.calls_changing_this(&a.value, calls, ran)?;
                }
                *ran = true;
            }
            _ => {
                let mut inside = false;
                visit::walk_expr(e, &mut |x| {
                    inside |= self.changes_this(x);
                    !inside
                });
                if inside {
                    return None;
                }
                *ran = true;
            }
        }
        Some(())
    }

    /// Moves the calls that change `this` out of the statement `e` (`args_changing_this`): emits
    /// a `let` for each, named after its method (`let next_id = self.next_id();`), and returns
    /// `e` reading the locals, which stay bound until `unbind_moved`. `taken`: names the
    /// statement itself binds.
    fn move_args_changing_this(&mut self, e: &Expr, taken: &[String]) -> Option<(Expr, Vec<(String, Option<Option<Type>>)>)> {
        if std::mem::take(&mut self.keep_args) {
            return None;
        }
        let (calls, field) = self.args_changing_this(e)?;
        let (calls, field): (Vec<Expr>, String) = (calls.into_iter().cloned().collect(), field.to_string());
        let method = called_name(e).unwrap_or_default().to_string();
        let mut moved: Vec<(Span, String)> = Vec::new();
        for c in &calls {
            let base = called_name(c).unwrap_or("value").to_string();
            // a later use of a variable or function of that name would get the local instead
            let taken_here = |n: &str| {
                self.is_local(n) || self.fun_info(n).is_some() || self.k.imports.contains_key(&(self.module.clone(), n.to_string())) || n == "drop" || taken.iter().any(|t| t == n) || moved.iter().any(|(_, m)| m == n)
            };
            let mut name = base.clone();
            let mut i = 2;
            while taken_here(&name) {
                name = format!("{}_{}", base, i);
                i += 1;
            }
            self.e.mark(c.span);
            self.e.w(&format!("let {} = ", esc(&name)));
            self.expr(c, P_LOWEST);
            self.e.w(";");
            let cause = self.class.as_ref().and_then(|cl| cl.methods.get(&base)).and_then(|m| m.recv_why.as_ref()).and_then(|(_, at)| *at);
            self.e.decide(c.span, "computed first", format!("`{}()` changes `this` while `{}` is borrowed for `.{}()`", base, field, method), cause);
            self.e.nl();
            moved.push((c.span, name));
        }
        self.e.mark(e.span);
        let mut out = e.clone();
        replace_moved(&mut out, &moved);
        let mut bound = Vec::new();
        for (c, (_, name)) in calls.iter().zip(&moved) {
            let ty = self.moved_type(c);
            let prev = self.scopes.last_mut().unwrap().insert(name.clone(), ty);
            bound.push((name.clone(), prev));
        }
        Some((out, bound))
    }

    /// The type of a call moved into a local, when it is clear: the method's result, also after
    /// `?` on a throwing method and `!!` on an optional result.
    fn moved_type(&self, c: &Expr) -> Option<Type> {
        let m = self.class.as_ref()?.methods.get(called_name(c)?)?;
        match &c.kind {
            ExprKind::Try(_) if m.throws => m.ret.clone(),
            ExprKind::Unwrap(_) => match m.ret.clone()? {
                Type::Option(t) => Some(*t),
                _ => None,
            },
            ExprKind::Call { .. } | ExprKind::MethodCall { .. } if !m.throws => m.ret.clone(),
            _ => None,
        }
    }

    /// Ends the bindings that `move_args_changing_this` made for its statement.
    fn unbind_moved(&mut self, bound: Vec<(String, Option<Option<Type>>)>) {
        let scope = self.scopes.last_mut().unwrap();
        for (name, prev) in bound.into_iter().rev() {
            match prev {
                Some(t) => scope.insert(name, t),
                None => scope.remove(&name),
            };
        }
    }

    /// Should `for (x in iter)` borrow `iter`? Yes for an owned collection in a local variable that
    /// is used after the loop, and for a collection field (moving it out of `&self` is an error).
    /// `Some(true)` when the elements are `Copy`, so the loop can still bind them by value.
    fn loop_borrow(&self, iter: &Expr) -> Option<bool> {
        let ty = match &iter.kind {
            ExprKind::Path { .. } => {
                let n = iter.ident()?;
                match self.local(n) {
                    Some(t) if self.reused_iters.contains_key(&iter.span.start) => t.clone()?,
                    Some(_) => return None,
                    None => {
                        self.implicit_field(n)?;
                        self.class.as_ref()?.fields.get(n)?.clone()
                    }
                }
            }
            ExprKind::Field { recv, name, safe: false } if matches!(recv.kind, ExprKind::SelfValue) => self.class.as_ref()?.fields.get(name)?.clone(),
            _ => return None,
        };
        if matches!(iter.kind, ExprKind::Field { .. }) || self.local(iter.ident()?).is_none() {
            if self.self_by_value || self.class.as_ref()?.is_static {
                return None;
            }
        }
        let Type::Path { segs, .. } = &ty else { return None };
        let [seg] = segs.as_slice() else { return None };
        match seg.name.as_str() {
            "Vec" | "VecDeque" | "LinkedList" | "HashSet" | "BTreeSet" | "BinaryHeap" => {}
            "HashMap" | "BTreeMap" => return Some(false),
            _ => return None,
        }
        let copy: std::collections::HashSet<String> = self.k.derives.iter().filter(|(_, d)| d.iter().any(|x| x == "Copy")).map(|(n, _)| n.clone()).collect();
        Some(match seg.generics.as_deref() {
            Some([GenericArg::Type(t)]) => crate::sema::is_copy_simple(t, &copy),
            _ => false,
        })
    }

    /// Why `for (x in iter)` borrows `iter` (just written): it is used after the loop, a loop
    /// around this one uses it again, or it is a field of `this`.
    fn loop_borrow_decision(&mut self, iter: &Expr) {
        let name = match &iter.kind {
            ExprKind::Field { name, .. } => name.clone(),
            _ => iter.ident().unwrap_or_default().to_string(),
        };
        let (why, cause) = match self.reused_iters.get(&iter.span.start) {
            Some(Some(at)) if self.local(&name).is_some() => (format!("`{}` is used after the loop", name), Some(Span::new(*at as usize, *at as usize + name.len()))),
            Some(None) if self.local(&name).is_some() => (format!("the loop around this one uses `{}` again", name), None),
            _ => (format!("`{}` belongs to `this`", name), None),
        };
        self.e.decide(iter.span, format!("&{}", name), why, cause);
    }

    /// `Box(5)` for a generic class `Box<T>`: `Box<_>`, so its methods are known on the variable.
    fn generic_ctor_type(&self, e: &Expr) -> Option<Type> {
        let ExprKind::Call { callee, .. } = &e.kind else { return None };
        let n = callee.ident()?;
        if self.is_local(n) {
            return None;
        }
        let c = self.class_info(n).filter(|c| c.has_generics)?;
        let count = c.type_params;
        if count == 0 {
            return None;
        }
        let mut seg = PathSeg::plain(n);
        seg.generics = Some(vec![GenericArg::Type(Type::Infer); count]);
        Some(Type::Path { global: false, segs: vec![seg], span: Span::default() })
    }

    /// Emits an expression in statement position: at the start of a statement in braces (a
    /// statement, a `when` arm, the body of `repeat { }`), so the calls that change `this` can be
    /// moved out first (`move_args_changing_this`). Returns true if it is block-like (no `;`
    /// needed).
    fn stmt_expr(&mut self, e: &Expr) -> bool {
        let moved = self.move_args_changing_this(e, &[]);
        let block_like = self.stmt_expr_here(moved.as_ref().map_or(e, |(m, _)| m));
        if let Some((_, bound)) = moved {
            self.unbind_moved(bound);
        }
        block_like
    }

    /// `tail` at the start of a statement in braces, with the calls that change `this` moved out
    /// first (`move_args_changing_this`).
    fn tail_moving_args(&mut self, e: &Expr, t: &Target) {
        let moved = self.move_args_changing_this(e, &[]);
        self.tail(moved.as_ref().map_or(e, |(m, _)| m), t);
        if let Some((_, bound)) = moved {
            self.unbind_moved(bound);
        }
    }

    fn stmt_expr_here(&mut self, e: &Expr) -> bool {
        if self.narrowing_exit(e) {
            return true;
        }
        match &e.kind {
            ExprKind::If { cond, then, else_ } => {
                self.if_expr(cond, then, else_.as_deref(), None);
                true
            }
            ExprKind::When { subject, binding, arms, rust_match } => {
                self.when_with_binding(subject.as_deref(), binding.as_ref(), arms, *rust_match, None, e.span);
                true
            }
            ExprKind::Block(b) => {
                self.block_stmt(b);
                true
            }
            ExprKind::LabeledBlock { label, block } => {
                self.e.w(&format!("'{}: ", label));
                self.block_stmt(block);
                true
            }
            ExprKind::Unsafe(b) => {
                self.e.w("unsafe ");
                self.block_stmt(b);
                true
            }
            ExprKind::Loop { .. } | ExprKind::While { .. } | ExprKind::For { .. } | ExprKind::DoWhile { .. } => {
                self.expr(e, P_LOWEST);
                true
            }
            ExprKind::TryCatch { body, catch } => {
                self.try_catch(body, catch.as_deref(), None);
                true
            }
            ExprKind::Field { .. } | ExprKind::MethodCall { .. } if has_safe_link(e) => {
                let r = self.resolve_question_dots(e);
                if has_safe_link(&r) && self.safe_chain_stmt(&r) {
                    return true;
                }
                if self.passes_errors_on() && self.is_throwing_call(&r) {
                    self.propagate(&r);
                    return false;
                }
                self.expr(&r, P_LOWEST);
                false
            }
            ExprKind::Paren(x) => self.stmt_expr_here(x),
            ExprKind::Assign(None, target, value) if self.map_entry_types(target).is_some() => {
                self.map_insert(target, value);
                false
            }
            ExprKind::Call { callee, args } if callee.ident() == Some("run") && self.is_builtin_call("run") && args.len() == 1 => {
                if let ExprKind::Lambda { body, .. } = &args[0].value.kind {
                    match &body.kind {
                        ExprKind::Block(b) => self.block_stmt(b),
                        _ => {
                            self.e.w("{");
                            self.e.indent();
                            self.e.nl();
                            let bl = self.stmt_expr(body);
                            if !bl {
                                self.e.w(";");
                            }
                            self.e.dedent();
                            self.e.nl();
                            self.e.w("}");
                        }
                    }
                    true
                } else {
                    self.expr(e, P_LOWEST);
                    false
                }
            }
            _ => {
                self.expr(e, P_LOWEST);
                false
            }
        }
    }

    /// `{ stmts }` in statement mode.
    pub(crate) fn block_stmt(&mut self, b: &Block) {
        self.e.w("{");
        self.e.indent();
        self.push_scope();
        self.stmts_with_tail(&b.stmts, None);
        self.pop_scope();
        self.e.dedent();
        if !b.stmts.is_empty() {
            self.e.nl();
        }
        self.e.w("}");
    }

    /// `{ stmts; tail }` in value mode.
    fn block_value(&mut self, b: &Block, t: &Target) {
        self.e.w("{");
        self.e.indent();
        self.push_scope();
        self.stmts_with_tail(&b.stmts, Some(t));
        let has_tail = matches!(b.stmts.last(), Some(Stmt::Expr { semi: false, .. }));
        if !has_tail {
            if let Some(w) = &t.wrap {
                if !ends_diverging(&b.stmts) {
                    self.e.nl();
                    self.e.w(&format!("{}(())", w));
                }
            }
        }
        self.pop_scope();
        self.e.dedent();
        if !b.stmts.is_empty() || t.wrap.is_some() {
            self.e.nl();
        }
        self.e.w("}");
    }

    /// Emits a value-producing expression: coercion to the target type and `Ok` wrapping.
    fn tail(&mut self, e: &Expr, t: &Target) {
        // `return x` as the last statement is just the value
        if let ExprKind::Return(Some(v)) = &e.kind {
            if t.fn_result && self.fns.last().is_some_and(|f| !f.is_lambda) && self.fns.last().is_some_and(|f| f.tries.is_empty()) && !v.diverges() {
                self.tail(v, t);
                return;
            }
        }
        if e.diverges() {
            self.expr(e, P_LOWEST);
            return;
        }
        match &e.kind {
            ExprKind::If { cond, then, else_ } if else_.is_some() => self.if_expr(cond, then, else_.as_deref(), Some(t)),
            ExprKind::When { subject, binding, arms, rust_match } => self.when_with_binding(subject.as_deref(), binding.as_ref(), arms, *rust_match, Some(t), e.span),
            ExprKind::Block(b) => self.block_value(b, t),
            ExprKind::LabeledBlock { label, block } if t.wrap.is_none() => {
                self.e.w(&format!("'{}: ", label));
                self.block_value(block, t);
            }
            ExprKind::Paren(x) => self.tail(x, t),
            _ => {
                let wrap = t.wrap.clone().filter(|_| !is_ok_err_call(e));
                // calling a throwing function propagates its error, like an exception would
                let auto_try = wrap.is_some() && self.passes_errors_on() && self.is_throwing_call(e);
                if let Some(w) = &wrap {
                    self.e.w(&format!("{}(", w));
                }
                if auto_try {
                    self.propagate(e);
                } else {
                    match &t.ty {
                        Some(_) if t.fn_result && self.returns_unboxed() => self.expr(e, P_LOWEST),
                        Some(ty) => self.coerce(e, ty, TyPos::Local),
                        None => self.expr(e, P_LOWEST),
                    }
                }
                if wrap.is_some() {
                    self.e.w(")");
                }
            }
        }
    }

    /// Emits `e` converted to `ty` where Oxie allows implicit conversion.
    pub(crate) fn coerce(&mut self, e: &Expr, ty: &Type, pos: TyPos) {
        // a block ending in a lambda (`{ [a] -> ... }`) for an `impl Fn` parameter stays unboxed
        let unboxed_fn = matches!(ty, Type::Fn { .. }) && !matches!(pos, TyPos::Field | TyPos::Generic | TyPos::Local);
        if matches!(e.kind, ExprKind::If { .. } | ExprKind::When { .. } | ExprKind::Block(_) | ExprKind::LabeledBlock { .. }) && !matches!(&e.kind, ExprKind::If { else_: None, .. }) && !unboxed_fn {
            self.tail(e, &Target::ty(ty));
            return;
        }
        if !matches!(ty, Type::Ref { .. }) && self.is_shared_place(e) {
            match ty {
                Type::Option(_) => {
                    self.e.w("Some(");
                    self.shared_copy(e);
                    self.e.w(")");
                }
                _ => self.shared_copy(e),
            }
            return;
        }
        // a boxed interface value for an `&Shape` parameter: `&*shapes[0]`, `&**s`
        if let Type::Ref { mutable, inner, .. } = ty {
            if pos == TyPos::Param && inner.single_name().is_some_and(|n| self.is_trait_name(n)) {
                if let Some(stars) = self.box_derefs(e) {
                    self.e.w(if *mutable { "&mut " } else { "&" });
                    self.e.w(stars);
                    self.expr(e, P_UNARY);
                    return;
                }
            }
        }
        match ty {
            Type::Ref { mutable, inner, .. } if pos == TyPos::Param && (self.auto_borrow(e, *mutable) || !mutable && self.lend_to_slice(e, inner)) => {
                self.e.mark(e.span);
                self.e.w(if *mutable { "&mut " } else { "&" });
                self.expr(e, P_UNARY);
                let param = self.ty(ty, TyPos::Param);
                self.e.decide(e.span, if *mutable { "&mut" } else { "&" }, format!("the parameter is `{}`", param), None);
            }
            Type::Option(inner) => match &e.kind {
                ExprKind::Null => self.e.w("None"),
                _ if self.is_value_like(e) || inner.is_string() && self.is_str_var(e) => {
                    self.e.w("Some(");
                    self.coerce(e, inner, TyPos::Generic);
                    self.e.w(")");
                }
                _ if self.is_place(e) && !self.place_is_option(e) => {
                    self.expr(e, P_POSTFIX);
                    self.e.w(".into()");
                }
                // a call whose type Oxie knows (and that isn't `T?`)
                ExprKind::Call { .. } | ExprKind::MethodCall { .. }
                    if !has_safe_link(e) && !self.is_option_expr(e) && self.infer_type(e).is_some_and(|t| !matches!(t, Type::Infer)) =>
                {
                    self.e.w("Some(");
                    self.coerce(e, inner, TyPos::Generic);
                    self.e.w(")");
                }
                // a call returning `T` converts to `T?` with `.into()` (identity when it already returns `T?`)
                ExprKind::MethodCall { name, .. } if !INFERENCE_DEPENDENT.contains(&name.as_str()) && !has_safe_link(e) && !self.is_option_expr(e) => {
                    self.expr(e, P_POSTFIX);
                    self.e.w(".into()");
                }
                // so does a function call, such as one into a Rust module whose type Oxie doesn't know
                ExprKind::Call { callee, .. }
                    if matches!(&callee.kind, ExprKind::Path { segs, .. } if segs.last().is_some_and(|n| n != "Some" && !INFERENCE_DEPENDENT.contains(&n.as_str())))
                        && !self.is_option_expr(e) =>
                {
                    self.expr(e, P_POSTFIX);
                    self.e.w(".into()");
                }
                _ => self.expr(e, P_LOWEST),
            },
            Type::Path { segs, .. } if segs.len() == 1 && segs[0].name == "String" && segs[0].generics.is_none() => match &e.kind {
                ExprKind::Str(parts) => self.e.w(&format!("String::from(\"{}\")", crate::lower::lit_parts(parts))),
                ExprKind::RawStr(s) => self.e.w(&format!("String::from({})", s)),
                // a `&str` where a `String` is expected is converted (identity when it already is one)
                ExprKind::MethodCall { name, .. }
                    if pos != TyPos::Local
                        && !INFERENCE_DEPENDENT.contains(&name.as_str())
                        && !matches!(name.as_str(), "to_string" | "to_owned" | "clone" | "to_uppercase" | "to_lowercase" | "repeat" | "join" | "replace" | "concat")
                        && !has_safe_link(e) =>
                {
                    self.expr(e, P_POSTFIX);
                    self.e.w(".into()");
                }
                ExprKind::Path { .. } if self.is_str_var(e) => {
                    self.expr(e, P_POSTFIX);
                    self.e.w(".to_string()");
                }
                _ => self.expr(e, P_LOWEST),
            },
            Type::Fn { params, ret } => {
                let boxed = matches!(pos, TyPos::Field | TyPos::Generic | TyPos::Local);
                if matches!(e.kind, ExprKind::Lambda { .. }) {
                    if boxed {
                        self.e.w("Box::new(");
                    }
                    self.lambda(e, Some(params), Some(ret));
                    if boxed {
                        self.e.w(")");
                    }
                } else if boxed && matches!(e.kind, ExprKind::Path { .. }) && !self.is_place(e) {
                    self.e.w("Box::new(");
                    self.expr(e, P_LOWEST);
                    self.e.w(")");
                } else {
                    self.expr(e, P_LOWEST);
                }
            }
            Type::Path { segs, .. } if segs.len() == 1 && segs[0].name == "Vec" && segs[0].generics.as_ref().is_some_and(|g| g.len() == 1) => {
                let elem = match &segs[0].generics.as_ref().unwrap()[0] {
                    GenericArg::Type(t) => Some(t.clone()),
                    _ => None,
                };
                match (&e.kind, elem) {
                    (ExprKind::VecLit(items), Some(elem)) if !items.iter().any(|x| matches!(x.kind, ExprKind::ArrayRepeat(..))) => {
                        self.e.w("vec![");
                        for (i, x) in items.iter().enumerate() {
                            if i > 0 {
                                self.e.w(", ");
                            }
                            self.coerce(x, &elem, TyPos::Generic);
                        }
                        self.e.w("]");
                    }
                    _ => self.expr(e, P_LOWEST),
                }
            }
            Type::Path { segs, .. }
                if segs.len() == 1 && matches!(pos, TyPos::Field | TyPos::Generic | TyPos::Local) && self.is_trait_name(&segs[0].name) && self.is_constructed_value(e) =>
            {
                self.e.w("Box::new(");
                self.expr(e, P_LOWEST);
                self.e.w(")");
            }
            _ => self.expr(e, P_LOWEST),
        }
    }

    /// Should an argument for a `&T` / `&mut T` parameter be borrowed automatically?
    /// Values are; references (`&x`, reference-typed variables, `self`) and string literals
    /// are passed as they are.
    fn auto_borrow(&self, e: &Expr, mutable: bool) -> bool {
        match &e.kind {
            ExprKind::Unary(UnOp::Ref | UnOp::RefMut, _) | ExprKind::SelfValue | ExprKind::Null | ExprKind::Lambda { .. } => false,
            ExprKind::Str(_) | ExprKind::RawStr(_) => false,
            ExprKind::Paren(x) => self.auto_borrow(x, mutable),
            _ if e.ident().is_some() => {
                let n = e.ident().unwrap();
                // a lambda parameter without a type gets it from this use: `&it` would make it `str`
                if self.is_untyped_lambda_param(n) || self.untyped_param(n) {
                    return false;
                }
                match self.local(n) {
                    Some(Some(t)) => !matches!(t, Type::Ref { .. } | Type::Ptr { .. }) && t.last_name() != Some("str"),
                    // unknown type: it may already be a `&mut` (`map.get_mut(k).unwrap()`)
                    Some(None) => !mutable,
                    // a field of this class, or a constant
                    None => self.class.as_ref().is_some_and(|c| c.fields.get(n).is_some_and(|t| !matches!(t, Type::Ref { .. }))) || (!mutable && is_const_like(n)),
                }
            }
            _ if mutable => self.is_place(e),
            ExprKind::Field { .. } | ExprKind::Index(..) => true,
            ExprKind::Call { .. } | ExprKind::StructLit { .. } | ExprKind::VecLit(_) | ExprKind::Array(_) | ExprKind::Template(_) | ExprKind::Int(_) | ExprKind::Float(_) | ExprKind::Tuple(_) => true,
            ExprKind::MethodCall { name, .. } => matches!(name.as_str(), "clone" | "to_string" | "to_owned" | "to_vec" | "collect" | "format"),
            _ => false,
        }
    }

    /// Is `n` a lambda parameter without a type (and not shadowed by a later binding)?
    fn untyped_param(&self, n: &str) -> bool {
        let depth = self.scopes.iter().rposition(|s| s.contains_key(n));
        self.untyped_params.iter().any(|(d, p)| Some(*d) == depth && p == n)
    }

    /// An argument for a `&str` or `&[T]` parameter is lent unless it already is a reference:
    /// lending a reference (`&s.trim()`) is harmless, since `&&str` coerces to `&str`.
    fn lend_to_slice(&self, e: &Expr, inner: &Type) -> bool {
        if !(matches!(inner, Type::Slice(_)) || inner.single_name() == Some("str")) {
            return false;
        }
        match &e.kind {
            ExprKind::Str(_) | ExprKind::RawStr(_) | ExprKind::Unary(UnOp::Ref | UnOp::RefMut, _) | ExprKind::Null | ExprKind::Lambda { .. } | ExprKind::SelfValue => false,
            ExprKind::Paren(x) => self.lend_to_slice(x, inner),
            // a lambda parameter without a type gets it from this use: `&it` would make it `str`
            _ if e.ident().is_some_and(|n| self.untyped_param(n)) => false,
            _ => match e.ident().and_then(|n| self.local(n)) {
                Some(Some(t)) => !matches!(t, Type::Ref { .. } | Type::Ptr { .. }) && t.last_name() != Some("str"),
                _ => true,
            },
        }
    }

    /// Values that can never be an `Option` themselves (safe to wrap in `Some`).
    /// Is this a call to an Oxie function or method declared with `throws`?
    pub(crate) fn is_throwing_call(&self, e: &Expr) -> bool {
        match &e.kind {
            ExprKind::Call { callee, .. } => {
                if let Some(n) = callee.ident() {
                    if self.is_local(n) {
                        return false;
                    }
                    if let Some(m) = self.class.as_ref().and_then(|c| c.methods.get(n)) {
                        return m.throws;
                    }
                    return self.fun_info(n).is_some_and(|f| f.throws);
                }
                // `util::load(x)`
                match &callee.kind {
                    ExprKind::Path { segs, .. } if segs.len() > 1 => self.fun_at(segs).is_some_and(|f| f.throws),
                    _ => false,
                }
            }
            ExprKind::MethodCall { recv, name, safe: false, .. } => {
                if matches!(recv.kind, ExprKind::SelfValue) {
                    return self.class.as_ref().and_then(|c| c.methods.get(name)).is_some_and(|m| m.throws);
                }
                if let Some((mut p, _)) = self.as_path(recv) {
                    if p.last().is_some_and(|o| o == "Self") {
                        p = vec![self.class.as_ref().map(|c| c.name.clone()).unwrap_or_default()];
                    }
                    if let Some(c) = self.class_at(&p) {
                        return c.methods.get(name).is_some_and(|m| m.throws);
                    }
                    // `util.load(x)`
                    p.push(name.clone());
                    return self.fun_at(&p).is_some_and(|f| f.throws);
                }
                let owner = self.infer_type(recv).and_then(|t| self.type_owner(&t));
                owner.and_then(|o| self.class_at(&o).and_then(|c| c.methods.get(name)).cloned()).is_some_and(|m| m.throws)
            }
            ExprKind::Await(x) => self.is_throwing_call(x),
            _ => false,
        }
    }

    /// A call of Rust code that returns a `Result` (`fs.write(path, text)`, `out.flush()`): as a
    /// statement in a throwing function it passes its error on, as a throwing Oxie function does,
    /// instead of dropping it. Methods of Oxie classes say themselves whether they throw, and an
    /// `.await` of unknown type may not be a `Result`.
    fn is_rust_result_call(&self, e: &Expr) -> bool {
        match &e.kind {
            ExprKind::MethodCall { recv, name, safe: false, .. } => {
                // `Config.open()`, `util.read(x)`
                if let Some((mut p, _)) = self.as_path(recv) {
                    if self.class_at(&p).is_some() || p.last().is_some_and(|s| s == "Self") {
                        return false;
                    }
                    p.push(name.clone());
                    if self.fun_at(&p).is_some() {
                        return false;
                    }
                }
                let oxie_class = matches!(recv.kind, ExprKind::SelfValue) || self.infer_type(recv).and_then(|t| self.type_owner(&t)).is_some_and(|o| self.class_at(&o).is_some());
                !oxie_class && self.is_result_expr(e)
            }
            ExprKind::Call { callee, .. } => match &callee.kind {
                ExprKind::Path { segs, .. } if self.fun_at(segs).is_some() => false,
                _ => self.is_result_expr(e),
            },
            _ => false,
        }
    }

    /// A throwing call's error passes on here: to the `catch` of an enclosing `try`, or out of a
    /// throwing function.
    fn passes_errors_on(&self) -> bool {
        self.fns.last().is_some_and(|f| !f.tries.is_empty()) || self.in_throwing_fn()
    }

    /// `call?`, or inside a `try`, a jump to its `catch` with the error, as an explicit `?` does.
    fn propagate(&mut self, e: &Expr) {
        let t = Expr::new(ExprKind::Try(Box::new(e.clone())), e.span);
        self.expr(&t, P_LOWEST);
    }

    fn in_throwing_fn(&self) -> bool {
        self.fns.last().is_some_and(|f| f.throws && !f.is_lambda)
    }

    /// A variable known to be `&str`, which converts to a `String` with `.to_string()`.
    fn is_str_var(&self, e: &Expr) -> bool {
        matches!(e.kind, ExprKind::Path { .. })
            && self.infer_type(e).is_some_and(|t| matches!(&t, Type::Ref { mutable: false, inner, .. } if inner.single_name() == Some("str")))
    }

    fn is_value_like(&self, e: &Expr) -> bool {
        match &e.kind {
            ExprKind::Int(_) | ExprKind::Float(_) | ExprKind::Bool(_) | ExprKind::Char(_) | ExprKind::Str(_) | ExprKind::RawStr(_)
            | ExprKind::Template(_) | ExprKind::StructLit { .. } | ExprKind::Tuple(_) | ExprKind::Array(_) | ExprKind::VecLit(_)
            | ExprKind::MapLit(_) | ExprKind::SetLit(_) | ExprKind::Cast(..) | ExprKind::Range { .. } | ExprKind::Lambda { .. }
            | ExprKind::Pair(..) | ExprKind::Is { .. } | ExprKind::In { .. } => true,
            ExprKind::Binary(..) | ExprKind::Unary(UnOp::Neg, _) | ExprKind::Unary(UnOp::Not, _) => true,
            ExprKind::Paren(x) => self.is_value_like(x),
            _ => self.is_constructed_value(e),
        }
    }

    /// Constructor calls: `Point(1, 2)`, `Shape.Circle(1.0)`, `Point { .. }`, and variants and
    /// classes without fields: `Status.Done`, `Marker`.
    fn is_constructed_value(&self, e: &Expr) -> bool {
        match &e.kind {
            ExprKind::StructLit { .. } => true,
            ExprKind::Call { callee, .. } => match &callee.kind {
                ExprKind::Path { segs, .. } => {
                    let last = segs.last().map(|s| s.as_str()).unwrap_or("");
                    is_type_like(last) && !matches!(last, "Some" | "Ok" | "Err" | "None" | "Box" | "Rc" | "Arc")
                }
                _ => false,
            },
            ExprKind::MethodCall { recv, name, .. } => {
                is_type_like(name) && matches!(self.as_path(recv), Some((_, PathKind::Type)))
            }
            ExprKind::Path { .. } | ExprKind::Field { .. } => self.is_unit_variant(e) || self.is_unit_class_value(e),
            _ => false,
        }
    }

    /// For a function that returns an interface (`fun pick(): Animal`), whether it returns a box
    /// (`Box<dyn Animal>`): when it returns newly built values of different classes. Otherwise
    /// it returns `impl Animal`, unboxed, as an `override` does (the interface declares
    /// `impl Animal`). `None` for other functions.
    pub(crate) fn iface_ret(&self, f: &FunDecl) -> Option<bool> {
        let Some(Type::Path { segs, .. }) = &f.ret else { return None };
        if segs.len() != 1 || segs[0].generics.is_some() || segs[0].fn_sugar.is_some() || !self.is_trait_name(&segs[0].name) {
            return None;
        }
        let Some(body) = f.body.as_ref().filter(|_| !f.mods.is_override) else { return Some(false) };
        let mut classes: Vec<String> = Vec::new();
        for v in visit::return_values(body) {
            if !self.is_constructed_value(&v) {
                continue;
            }
            let class = match &v.kind {
                ExprKind::Call { callee, .. } => match &callee.kind {
                    ExprKind::Path { segs, .. } => segs.join("."),
                    _ => continue,
                },
                ExprKind::StructLit { path, .. } => path.join("."),
                ExprKind::MethodCall { recv, .. } => match self.as_path(recv) {
                    Some((p, _)) => p.join("."),
                    None => continue,
                },
                _ => match self.as_path(&v) {
                    Some((p, _)) => p.join("."),
                    None => continue,
                },
            };
            if !classes.contains(&class) {
                classes.push(class);
            }
        }
        Some(classes.len() > 1)
    }

    /// Is the innermost function one whose values are returned as they are (`impl Interface`)?
    fn returns_unboxed(&self) -> bool {
        self.fns.last().is_some_and(|f| f.unboxed_ret)
    }

    /// A class without fields, which is its own value: `Marker`, `shapes.Marker`.
    fn is_unit_class_value(&self, e: &Expr) -> bool {
        let path = match &e.kind {
            ExprKind::Path { segs, generics: None, global: false } if segs.len() == 1 && (self.is_local(&segs[0]) || self.implicit_field(&segs[0]).is_some()) => return false,
            ExprKind::Path { segs, generics: None, .. } => segs.clone(),
            ExprKind::Field { .. } => match self.as_path(e) {
                Some((p, PathKind::Type)) => p,
                _ => return false,
            },
            _ => return false,
        };
        self.class_at(&path).is_some_and(|c| c.is_unit)
    }

    /// A variant without fields: `Status.Done`, `Self.Done`, `shapes.Status.Done`, or `Done`
    /// alone when only one enum has it.
    fn is_unit_variant(&self, e: &Expr) -> bool {
        let path = match &e.kind {
            // resolved as in `path_expr`
            ExprKind::Path { segs, generics: None, global: false } if segs.len() == 1 => {
                let n = &segs[0];
                if self.is_local(n) || self.implicit_field(n).is_some() || self.k.is_class_name(n) || self.k.is_fun_name(n) {
                    return false;
                }
                return self.variant_enum(n).and_then(|en| en.variant(n)).is_some_and(|v| matches!(v, VariantShape::Unit));
            }
            ExprKind::Path { segs, generics: None, .. } => segs.clone(),
            ExprKind::Field { .. } => match self.as_path(e) {
                Some((p, PathKind::Value)) => p,
                _ => return false,
            },
            _ => return false,
        };
        let Some((name, owner)) = path.split_last() else { return false };
        let owner = match (owner, &self.class) {
            ([s], Some(c)) if s == "Self" => vec![c.name.clone()],
            _ => owner.to_vec(),
        };
        self.enum_at(&owner).and_then(|en| en.variant(name)).is_some_and(|v| matches!(v, VariantShape::Unit))
    }

    pub(crate) fn is_place(&self, e: &Expr) -> bool {
        match &e.kind {
            ExprKind::Path { segs, generics: None, .. } => {
                segs.len() == 1 && !is_type_like(&segs[0]) && !self.k.is_fun_name(&segs[0]) || is_const_like(segs.last().unwrap())
            }
            ExprKind::SelfValue => true,
            ExprKind::Field { recv, safe: false, .. } => self.as_path(e).is_none() && self.is_place(recv),
            ExprKind::Index(r, _) => self.is_place(r),
            ExprKind::Paren(x) => self.is_place(x),
            ExprKind::Unary(UnOp::Deref, x) => self.is_place(x),
            _ => false,
        }
    }

    fn place_is_option(&self, e: &Expr) -> bool {
        if let Some(n) = e.ident() {
            if let Some(Some(t)) = self.local(n) {
                return matches!(t, Type::Option(_)) || t.single_name() == Some("Option");
            }
            if let Some(c) = self.class.as_ref() {
                if let Some(t) = c.fields.get(n) {
                    return matches!(t, Type::Option(_)) || t.single_name() == Some("Option");
                }
            }
        }
        false
    }

    // ------------------------------------------------------------------ expressions

    fn prec_of(&self, e: &Expr) -> u8 {
        use ExprKind::*;
        match &e.kind {
            Assign(..) | Return(_) | Break { .. } | Continue(_) | Throw(_) | Lambda { .. } => P_LOWEST,
            Range { .. } => P_RANGE,
            Binary(op, ..) => {
                if *op == BinOp::Add && self.is_string_concat(e) {
                    P_PRIMARY
                } else {
                    op.prec()
                }
            }
            Cast(..) => P_CAST,
            Unary(..) => P_UNARY,
            Is { negated: true, .. } | In { negated: true, .. } => P_UNARY,
            Try(_) if self.in_try_block() => P_PRIMARY,
            Try(_) | Unwrap(_) | Await(_) | Field { .. } | MethodCall { .. } | Call { .. } | Index(..) | In { .. } => P_POSTFIX,
            Paren(x) => self.prec_of(x),
            // Rust text: an operator or a space in it may bind looser than what is around it
            // (`(rust { &raw mut info }).cast()`)
            Raw(s) => {
                let t = s.trim();
                if t.starts_with(['&', '*', '-', '!', '|']) || t.contains(char::is_whitespace) && !t.ends_with([')', ']', '}']) {
                    P_LOWEST
                } else {
                    P_PRIMARY
                }
            }
            _ => P_PRIMARY,
        }
    }

    fn emits_block_like(&self, e: &Expr) -> bool {
        use ExprKind::*;
        match &e.kind {
            If { .. } | When { .. } | Block(_) | Unsafe(_) | Loop { .. } | While { .. } | For { .. } | DoWhile { .. } | TryCatch { .. } | Async { .. }
            | LabeledBlock { .. } | ConstBlock(_) => true,
            Elvis(_, r) => !is_literal(r),
            Try(_) => self.in_try_block(),
            MethodCall { name, args, .. } => (name == "copy" && !args.is_empty() && args.iter().all(|a| a.name.is_some())) || name == "let",
            Paren(x) => self.emits_block_like(x),
            _ => false,
        }
    }

    /// Whether `expr(e, min)` writes `e` ending with a cast that isn't in parentheses.
    fn ends_with_cast(&self, e: &Expr, min: u8) -> bool {
        use ExprKind::*;
        if self.prec_of(e) < min {
            return false;
        }
        match &e.kind {
            Cast(..) => true,
            Paren(x) => self.ends_with_cast(x, P_LOWEST),
            Binary(op, _, r) => !(*op == BinOp::Add && self.is_string_concat(e)) && self.ends_with_cast(r, op.prec() + 1),
            _ => false,
        }
    }

    fn in_try_block(&self) -> bool {
        self.fns.last().is_some_and(|f| !f.tries.is_empty())
    }

    pub(crate) fn expr(&mut self, e: &Expr, min: u8) {
        let parens = self.prec_of(e) < min || (min > P_LOWEST && self.emits_block_like(e));
        if parens {
            self.e.w("(");
        }
        self.e.mark(e.span);
        self.expr_inner(e);
        if parens {
            self.e.w(")");
        }
    }

    pub(crate) fn expr_inner(&mut self, e: &Expr) {
        use ExprKind::*;
        // fields of shared-class handles
        match &e.kind {
            Field { safe: false, .. } | Index(..) | MethodCall { safe: false, .. } if self.shared_access(e) => return,
            Assign(op, target, value) if self.shared_assign(*op, target, value) => return,
            _ => {}
        }
        match &e.kind {
            Int(s) | Float(s) | Char(s) | RawStr(s) => self.e.w(s),
            Bool(b) => self.e.w(if *b { "true" } else { "false" }),
            Str(parts) => self.e.w(&format!("\"{}\"", crate::lower::lit_parts(parts))),
            Template(parts) => {
                self.e.w("format!(");
                self.format_args(parts);
                self.e.w(")");
            }
            Null => self.e.w("None"),
            Path { segs, generics, global } => self.path_expr(segs, generics.as_ref(), *global, e.span),
            // `Vec::<i32>::new` (parsed as `<Vec<i32>>::new`)
            QualifiedPath(Type::Qualified { ty, trait_: None, rest }) if matches!(**ty, Type::Path { .. }) => {
                let turbofish = |s: String| match s.find('<') {
                    Some(i) => format!("{}::{}", &s[..i], &s[i..]),
                    None => s,
                };
                let mut s = turbofish(self.ty(ty, TyPos::Plain));
                for seg in rest {
                    s.push_str("::");
                    s.push_str(&turbofish(self.seg(seg, TyPos::Generic)));
                }
                self.e.w(&s);
            }
            QualifiedPath(t) => {
                let s = self.ty(t, TyPos::Plain);
                self.e.w(&s);
            }
            // in a shared class's method that passes `this` on, `this` as a value is the handle
            SelfValue if self.this_recv.as_ref().is_some_and(|r| !r.contains(&e.span.start)) => self.e.w("__this.clone()"),
            SelfValue => {
                let n = self.class.as_ref().map(|c| c.self_name.clone()).unwrap_or_else(|| "self".into());
                self.e.w(&n);
            }
            Unary(op, x) => {
                let s = match op {
                    UnOp::Neg => "-",
                    UnOp::Not => "!",
                    UnOp::Deref => "*",
                    UnOp::Ref => "&",
                    UnOp::RefMut => "&mut ",
                };
                self.e.w(s);
                self.expr(x, P_UNARY);
            }
            // `x == null` is `x.is_none()`: `x == None` would need `PartialEq` on the value
            Binary(op @ (BinOp::Eq | BinOp::Ne), l, r) if matches!(r.kind, Null) != matches!(l.kind, Null) => {
                let x = if matches!(r.kind, Null) { l } else { r };
                self.expr(x, P_POSTFIX);
                self.e.w(if *op == BinOp::Eq { ".is_none()" } else { ".is_some()" });
            }
            Binary(op, l, r) => {
                if *op == BinOp::Add && self.is_string_concat(e) {
                    self.string_concat(e);
                    return;
                }
                self.binary(*op, l, r, false);
            }
            // `m[k] = v` puts a value in a map; as an expression it is `()`, like an assignment
            Assign(None, target, value) if self.map_entry_types(target).is_some() => {
                self.e.w("{ ");
                self.map_insert(target, value);
                self.e.w("; }");
            }
            // `m[k] += 1`: the key may be missing, and only the program knows what that means
            Assign(Some(op), target, _) if self.map_entry_types(target).is_some() => {
                let (Some((key_ty, _)), ExprKind::Index(m, k)) = (self.map_entry_types(target), &target.kind) else { unreachable!() };
                let simple = |x: &Expr| x.ident().is_some() || matches!(x.kind, Int(_) | Str(_) | Char(_));
                let (m, owned, borrowed) = if m.ident().is_some() && simple(k) {
                    let key = self.capture(k, P_LOWEST);
                    // `entry` takes the key itself, `get_mut` a reference (text: a `&str`)
                    match &k.kind {
                        Str(_) if key_ty.single_name() == Some("String") => (self.capture(m, P_POSTFIX), format!("{}.to_string()", key), key),
                        Str(_) => (self.capture(m, P_POSTFIX), key.clone(), key),
                        _ => (self.capture(m, P_POSTFIX), key.clone(), format!("&{}", key)),
                    }
                } else {
                    ("map".into(), "key".into(), "&key".into())
                };
                let o = op.as_str();
                self.diags.push(
                    Diag::error(target.span, format!("a map's value can't be changed in place with `{}=`: the key may not be in the map", o)).with_help(format!(
                        "write `*{m}.entry({owned}).or_default() {o}= ...` to start a missing key at its default value (0, \"\", an empty list), or `*{m}.get_mut({borrowed}).unwrap() {o}= ...` to stop the program when it is missing"
                    )),
                );
                self.expr(target, P_LOWEST);
            }
            Assign(op, target, value) => {
                let target_ty = self.assign_target_type(target);
                self.expr(target, P_LOWEST + 1);
                match op {
                    Some(o) => self.e.w(&format!(" {}= ", o.as_str())),
                    None => self.e.w(" = "),
                }
                match (op, target_ty) {
                    (None, Some(t)) => self.coerce(value, &t, TyPos::Local),
                    // String += "...$x": append a borrowed str
                    (Some(BinOp::Add), _)
                        if matches!(value.kind, Template(_)) || (matches!(value.kind, Binary(BinOp::Add, ..)) && self.is_string_concat(value)) || (self.is_owned_string(value) && self.is_owned_string(target)) =>
                    {
                        self.e.w("&");
                        self.expr(value, P_UNARY);
                    }
                    (None, None) if self.is_shared_place(value) => self.shared_copy(value),
                    _ => self.expr(value, P_LOWEST),
                }
            }
            Call { callee, args } => self.call(callee, args, e.span),
            MethodCall { .. } | Field { .. } | Index(..) if has_safe_link(e) => {
                let r = self.resolve_question_dots(e);
                if has_safe_link(&r) {
                    self.safe_chain(&r);
                } else {
                    self.expr_inner(&r);
                }
            }
            MethodCall { recv, name, generics, args, .. } => self.method_call(recv, name, generics.as_ref(), args, e.span),
            Field { recv, name, .. } => {
                if let Some((p, _)) = self.field_path(e) {
                    self.e.w(&p);
                    return;
                }
                self.expr(recv, P_POSTFIX);
                // `this` is written `self`: the rest is marked, as after an implicit field's `self`
                if matches!(recv.kind, SelfValue) {
                    self.e.mark(Span::new(recv.span.end as usize, recv.span.end as usize));
                }
                self.e.w(".");
                self.e.w(&member(name));
            }
            Index(r, i) => {
                self.expr(r, P_POSTFIX);
                self.e.w("[");
                // a list indexed with another integer type: `as usize` costs nothing, and a
                // negative index fails the bounds check like one past the end
                if self.int_index(r, i) {
                    self.expr(i, P_CAST);
                    self.e.w(" as usize");
                    self.e.decide(i.span, "as usize", "indexes are `usize`", None);
                } else {
                    self.expr(i, P_LOWEST);
                }
                self.e.w("]");
            }
            Try(x) => {
                if let Some(label) = self.fns.last().and_then(|f| f.tries.last().cloned()) {
                    let v = self.fresh("v");
                    let err = self.fresh("e");
                    self.e.w("match ");
                    self.expr(x, P_LOWEST);
                    self.e.w(" {");
                    self.e.indent();
                    self.e.w(&format!("\nOk({v}) => {v},\nErr({err}) => break '{label} Err(::std::convert::From::from({err})),"));
                    self.e.dedent();
                    self.e.w("\n}");
                } else {
                    self.expr(x, P_POSTFIX);
                    self.e.w("?");
                }
            }
            Unwrap(x) => {
                self.expr(x, P_POSTFIX);
                self.e.w(".unwrap()");
            }
            Await(x) => {
                self.expr(x, P_POSTFIX);
                self.e.w(".await");
            }
            Cast(x, t) => {
                self.expr(x, P_CAST);
                let tt = self.ty(t, TyPos::Plain);
                self.e.w(&format!(" as {}", tt));
            }
            Elvis(l, r) => self.elvis(l, r),
            In { item, coll, negated } if matches!(strip_paren(coll).kind, Range { .. }) => {
                let Range { start, end, inclusive } = &strip_paren(coll).kind else { unreachable!() };
                self.in_range(item, start.as_deref(), end.as_deref(), *inclusive, *negated);
            }
            In { item, coll, negated } => {
                if *negated {
                    self.e.w("!");
                }
                self.expr(coll, P_POSTFIX);
                let kind = self.contains_kind(coll);
                self.e.w(if kind == Contains::Key { ".contains_key(" } else { ".contains(" });
                // a key that already is a reference (`"ada"`, a `&str`) is looked up as it is, so it
                // works for `String` keys too
                let lend = match kind {
                    Contains::Key => self.auto_borrow(item, false),
                    Contains::Text => !is_string_lit(coll) && !self.is_char(item),
                    Contains::Value => true,
                };
                if lend {
                    self.e.w("&");
                    self.expr(item, P_UNARY);
                } else {
                    self.expr(item, P_LOWEST);
                }
                self.e.w(")");
            }
            Is { expr, pat, negated } => {
                if *negated {
                    self.e.w("!");
                }
                self.e.w("matches!(");
                self.expr(expr, P_LOWEST);
                let p = self.pat(pat);
                self.e.w(&format!(", {})", p));
            }
            Pair(a, b) => {
                self.e.w("(");
                self.expr(a, P_LOWEST);
                self.e.w(", ");
                self.expr(b, P_LOWEST);
                self.e.w(")");
            }
            If { cond, then, else_ } => {
                if else_.is_some() {
                    self.if_expr(cond, then, else_.as_deref(), Some(&branch_target(e)));
                } else {
                    self.if_expr(cond, then, None, None);
                }
            }
            When { subject, binding, arms, rust_match } => {
                self.when_with_binding(subject.as_deref(), binding.as_ref(), arms, *rust_match, Some(&branch_target(e)), e.span);
            }
            Block(b) => self.block_value(b, &Target::none()),
            LabeledBlock { label, block } => {
                self.e.w(&format!("'{}: ", label));
                self.block_value(block, &Target::none());
            }
            ConstBlock(b) => {
                self.e.w("const ");
                self.block_value(b, &Target::none());
            }
            Unsafe(b) => {
                self.e.w("unsafe ");
                self.block_value(b, &Target::none());
            }
            Async { is_move, block } => {
                self.e.w(if *is_move { "async move " } else { "async " });
                self.fns.push(FnCtx { throws: false, ret: None, is_lambda: true, tries: Vec::new(), text_break: None, unboxed_ret: false });
                let saved = std::mem::take(&mut self.loops);
                self.block_value(block, &Target::none());
                self.loops = saved;
                self.fns.pop();
            }
            Loop { label, body } => {
                if let Some(l) = label {
                    self.e.w(&format!("'{}: ", l));
                }
                self.e.w("loop ");
                self.loops.push(LoopCtx { dowhile_label: None });
                self.block_stmt(body);
                self.loops.pop();
            }
            While { label, cond, body } if matches!(cond.as_ref(), Cond::Chain(_)) => {
                // `while (val a = x && a > 0)` -> loop { match/if ... else break }
                let Cond::Chain(parts) = cond.as_ref() else { unreachable!() };
                if let Some(l) = label {
                    self.e.w(&format!("'{}: ", l));
                }
                self.e.w("loop {");
                self.e.indent();
                self.e.nl();
                self.loops.push(LoopCtx { dowhile_label: None });
                let brk = Expr::new(ExprKind::Break { label: None, value: None }, e.span);
                self.let_chain(parts, body, Some(&brk), None);
                self.loops.pop();
                self.e.dedent();
                self.e.nl();
                self.e.w("}");
            }
            While { label, cond, body } => {
                if let Some(l) = label {
                    self.e.w(&format!("'{}: ", l));
                }
                self.e.w("while ");
                self.push_scope();
                self.cond(cond);
                self.e.w(" ");
                self.loops.push(LoopCtx { dowhile_label: None });
                self.block_stmt(body);
                self.loops.pop();
                self.pop_scope();
            }
            DoWhile { label, body, cond } => {
                if let Some(l) = label {
                    self.e.w(&format!("'{}: ", l));
                }
                let needs_label = visit::has_own_continue(body);
                let lbl = if needs_label { Some(self.fresh("body")) } else { None };
                self.e.w("loop {");
                self.e.indent();
                self.e.nl();
                self.loops.push(LoopCtx { dowhile_label: lbl.clone() });
                if let Some(l) = &lbl {
                    self.e.w(&format!("'{}: ", l));
                }
                self.block_stmt(body);
                self.loops.pop();
                self.e.nl();
                self.e.w("if !(");
                self.expr(cond, P_LOWEST);
                self.e.w(") {");
                self.e.indent();
                self.e.nl();
                self.e.w("break;");
                self.e.dedent();
                self.e.nl();
                self.e.w("}");
                self.e.dedent();
                self.e.nl();
                self.e.w("}");
            }
            For { label, pat, iter, body } => {
                if let Some(l) = label {
                    self.e.w(&format!("'{}: ", l));
                }
                let p = self.pat(pat);
                let borrows = self.loop_borrow(iter);
                // the body changes the elements: borrowed mutably (for a field, sema makes the
                // method `&mut self` by the same rule)
                let writes_elements = borrows.is_some()
                    && matches!(pat, Pat::Ident { name, by_ref: false, mutable: false, sub: None, .. } if visit::writes_through(body, name));
                // an element of a list of interface values is a box: `Box<dyn Shape>`, or a
                // reference to one when the loop borrows the list
                let boxed = match pat {
                    Pat::Ident { name, by_ref: false, sub: None, .. } => {
                        let (coll, by_ref) = match &strip_paren(iter).kind {
                            Unary(UnOp::Ref, x) => (&**x, true),
                            MethodCall { recv, name, args, .. } if args.is_empty() && matches!(name.as_str(), "iter" | "iter_mut") => (&**recv, true),
                            _ => (iter.as_ref(), borrows.is_some()),
                        };
                        self.elem_type(coll).and_then(|t| self.boxed_iface(&t)).map(|b| (name.clone(), if by_ref { Type::Ref { lifetime: None, mutable: writes_elements, inner: Box::new(b) } } else { b }))
                    }
                    _ => None,
                };
                match borrows {
                    // a collection used again after the loop is borrowed, not consumed; mutably
                    // when the body changes the elements (`for (x in xs) x.qty += 1`)
                    Some(copy) => {
                        let plain = |p: &Pat| matches!(p, Pat::Ident { by_ref: false, mutable: false, sub: None, .. } | Pat::Wild);
                        let by_value = copy && (plain(pat) || matches!(pat, Pat::Tuple(ps) if ps.iter().all(plain)));
                        if writes_elements {
                            self.e.w(&format!("for {} in &mut ", p));
                        } else if by_value {
                            self.e.w(&format!("for &{} in &", p));
                        } else {
                            self.e.w(&format!("for {} in &", p));
                        }
                        self.expr(iter, P_POSTFIX);
                        self.loop_borrow_decision(iter);
                    }
                    None => {
                        self.e.w(&format!("for {} in ", p));
                        let needs_parens = contains_struct_lit(iter);
                        if needs_parens {
                            self.e.w("(");
                        }
                        self.expr(iter, P_LOWEST);
                        if needs_parens {
                            self.e.w(")");
                        }
                    }
                }
                self.e.w(" ");
                self.push_scope();
                // the loop variable has the element type: a reference when the loop borrows
                let elem = match (pat, borrows) {
                    (Pat::Ident { by_ref: false, sub: None, .. }, Some(copy)) => {
                        self.elem_type(iter).map(|t| if copy && !writes_elements { t } else { Type::Ref { lifetime: None, mutable: writes_elements, inner: Box::new(t) } })
                    }
                    (Pat::Ident { by_ref: false, sub: None, .. }, None) if iter.ident().is_some_and(|n| self.is_local(n)) => self.elem_type(iter),
                    _ => None,
                };
                self.bind_pat(pat, elem);
                if let Some((name, t)) = boxed {
                    self.bind_name(&name, Some(t));
                }
                self.loops.push(LoopCtx { dowhile_label: None });
                self.block_stmt(body);
                self.loops.pop();
                self.pop_scope();
            }
            Lambda { .. } => self.lambda(e, None, None),
            Return(v) => self.return_expr(v.as_deref()),
            Break { label, value } => {
                self.e.w("break");
                if let Some(l) = label {
                    self.e.w(&format!(" '{}", l));
                }
                if let Some(v) = value {
                    self.e.w(" ");
                    self.expr(v, P_LOWEST);
                }
            }
            Continue(label) => match label {
                Some(l) => self.e.w(&format!("continue '{}", l)),
                None => match self.loops.last().and_then(|l| l.dowhile_label.clone()) {
                    Some(l) => self.e.w(&format!("break '{}", l)),
                    None => self.e.w("continue"),
                },
            },
            Throw(x) => {
                if let Some(label) = self.fns.last().and_then(|f| f.tries.last().cloned()) {
                    self.e.w(&format!("break '{} Err(", label));
                    self.expr(x, P_POSTFIX);
                    self.e.w(".into())");
                } else {
                    self.e.w("return Err(");
                    self.expr(x, P_POSTFIX);
                    self.e.w(".into())");
                }
            }
            Tuple(v) => {
                self.e.w("(");
                for (i, x) in v.iter().enumerate() {
                    if i > 0 {
                        self.e.w(", ");
                    }
                    self.expr(x, P_LOWEST);
                }
                if v.len() == 1 {
                    self.e.w(",");
                }
                self.e.w(")");
            }
            Array(v) => {
                self.e.w("[");
                self.comma_list(v);
                self.e.w("]");
            }
            ArrayRepeat(x, n) => {
                self.e.w("[");
                self.expr(x, P_LOWEST);
                self.e.w("; ");
                self.expr(n, P_LOWEST);
                self.e.w("]");
            }
            VecLit(v) => {
                if v.len() == 1 {
                    if let ArrayRepeat(x, n) = &v[0].kind {
                        self.e.w("vec![");
                        self.expr(x, P_LOWEST);
                        self.e.w("; ");
                        self.expr(n, P_LOWEST);
                        self.e.w("]");
                        return;
                    }
                }
                self.e.w("vec![");
                self.comma_list(v);
                self.e.w("]");
            }
            MapLit(v) => {
                if v.is_empty() {
                    self.e.w("::std::collections::HashMap::new()");
                } else {
                    self.e.w("::std::collections::HashMap::from([");
                    self.comma_list(v);
                    self.e.w("])");
                }
            }
            SetLit(v) => {
                if v.is_empty() {
                    self.e.w("::std::collections::HashSet::new()");
                } else {
                    self.e.w("::std::collections::HashSet::from([");
                    self.comma_list(v);
                    self.e.w("])");
                }
            }
            Range { start, end, inclusive } => {
                if let Some(s) = start {
                    self.expr(s, P_RANGE + 1);
                }
                self.e.w(if *inclusive { "..=" } else { ".." });
                if let Some(x) = end {
                    self.expr(x, P_RANGE + 1);
                }
            }
            StructLit { path, generics, fields, base } => self.struct_lit(path, generics.as_ref(), fields, base.as_deref()),
            Macro { path, text } => {
                let text = self.include_paths(&format!("{}!{}", path, text));
                self.e.raw_lines(&text);
            }
            TryCatch { body, catch } => {
                let t = Target::none();
                self.try_catch(body, catch.as_deref(), Some(&t));
            }
            Paren(x) => self.expr(x, P_LOWEST),
            Raw(s) => {
                let s = self.include_paths(s.trim());
                self.e.raw_lines(&s);
            }
            Error => self.e.w("()"),
        }
    }

    /// `l op r`. With `before_lt` a `<` or `<<` follows, and a cast at the end of `r` needs parentheses.
    fn binary(&mut self, op: BinOp, l: &Expr, r: &Expr, before_lt: bool) {
        let p = op.prec();
        let cmp = p == 5;
        let lp = if cmp { p + 1 } else { p };
        let by_value = matches!(op, BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge | BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Rem);
        if by_value && is_scalar_literal(r) && self.is_ref_lambda_param(l) {
            self.read_value(l);
        } else {
            self.operand(l, lp, matches!(op, BinOp::Lt | BinOp::Shl));
        }
        self.e.w(&format!(" {} ", op.as_str()));
        if by_value && is_scalar_literal(l) && self.is_ref_lambda_param(r) {
            self.read_value(r);
        } else {
            self.operand(r, p + 1, before_lt);
        }
    }

    /// `expr(e, min)`. With `before_lt` a `<` or `<<` follows, and a cast at the end of `e` gets
    /// parentheses: Rust reads the `<` in `n as usize < 3` as the start of generic arguments.
    fn operand(&mut self, e: &Expr, min: u8, before_lt: bool) {
        if !before_lt || !self.ends_with_cast(e, min) {
            self.expr(e, min);
            return;
        }
        match &e.kind {
            ExprKind::Paren(x) => {
                self.e.mark(e.span);
                self.operand(x, P_LOWEST, true);
            }
            ExprKind::Binary(op, l, r) => {
                self.e.mark(e.span);
                self.binary(*op, l, r, true);
            }
            _ => {
                self.e.w("(");
                self.expr(e, P_LOWEST);
                self.e.w(")");
            }
        }
    }

    /// `x in a..b` -> `a <= x && x < b` (x is evaluated once)
    fn in_range(&mut self, item: &Expr, start: Option<&Expr>, end: Option<&Expr>, inclusive: bool, negated: bool) {
        let simple = self.is_place(item) || is_literal(item);
        let var = if simple { None } else { Some(self.fresh("x")) };
        if negated {
            self.e.w("!");
        }
        self.e.w("(");
        if let Some(v) = &var {
            self.e.w(&format!("{{ let {} = ", v));
            self.expr(item, P_LOWEST);
            self.e.w("; ");
        }
        let emit_item = |l: &mut Self| match &var {
            Some(v) => l.e.w(v),
            None => l.expr(item, 6),
        };
        let mut first = true;
        if let Some(s) = start {
            self.expr(s, 6);
            self.e.w(" <= ");
            emit_item(self);
            first = false;
        }
        if let Some(x) = end {
            if !first {
                self.e.w(" && ");
            }
            emit_item(self);
            self.e.w(if inclusive { " <= " } else { " < " });
            self.expr(x, 6);
            first = false;
        }
        if first {
            self.e.w("true");
        }
        if var.is_some() {
            self.e.w(" }");
        }
        self.e.w(")");
    }

    fn comma_list(&mut self, v: &[Expr]) {
        for (i, x) in v.iter().enumerate() {
            if i > 0 {
                self.e.w(", ");
            }
            if self.is_shared_place(x) {
                self.shared_copy(x);
            } else {
                self.expr(x, P_LOWEST);
            }
        }
    }

    /// For `m[k]` on a `HashMap` or `BTreeMap`: its key and value types.
    fn map_entry_types(&self, target: &Expr) -> Option<(Type, Type)> {
        let ExprKind::Index(m, _) = &target.kind else { return None };
        let mut t = self.infer_type(m)?;
        while let Type::Ref { inner, .. } = t {
            t = *inner;
        }
        let Type::Path { segs, .. } = &t else { return None };
        let [seg] = segs.as_slice() else { return None };
        if !matches!(seg.name.as_str(), "HashMap" | "BTreeMap") {
            return None;
        }
        match seg.generics.as_deref() {
            Some([GenericArg::Type(k), GenericArg::Type(v)]) => Some((k.clone(), v.clone())),
            _ => Some((Type::Infer, Type::Infer)),
        }
    }

    /// `m[k] = v` on a map: `m.insert(k, v)`, which adds the key or replaces its value.
    fn map_insert(&mut self, target: &Expr, value: &Expr) {
        let (Some((key_ty, value_ty)), ExprKind::Index(m, key)) = (self.map_entry_types(target), &target.kind) else { return };
        self.expr(m, P_POSTFIX);
        self.e.w(".insert(");
        for (i, (x, t)) in [(&**key, key_ty), (value, value_ty)].into_iter().enumerate() {
            if i > 0 {
                self.e.w(", ");
            }
            match t {
                Type::Infer => self.expr(x, P_LOWEST),
                t => self.coerce(x, &t, TyPos::Generic),
            }
        }
        self.e.w(")");
    }

    fn assign_target_type(&self, target: &Expr) -> Option<Type> {
        match &target.kind {
            ExprKind::Path { .. } => {
                let n = target.ident()?;
                if let Some(t) = self.local(n) {
                    return t.clone();
                }
                self.class.as_ref().and_then(|c| if c.is_static { None } else { c.fields.get(n).cloned() })
            }
            ExprKind::Field { recv, name, .. } if matches!(recv.kind, ExprKind::SelfValue) => self.class.as_ref().and_then(|c| c.fields.get(name).cloned()),
            _ => None,
        }
        .filter(|t| *t != Type::Infer)
    }

    /// Let chains (`val a = x && a > 0 && val b = f(a)`), lowered to nested `if`/`match` so they
    /// work in every Rust edition. The `else` branch is repeated where a part can fail.
    fn let_chain(&mut self, parts: &[Cond], then: &Block, else_: Option<&Expr>, mode: Option<&Target>) {
        if parts.is_empty() {
            match mode {
                Some(t) => self.block_value(then, t),
                None => self.block_stmt(then),
            }
            return;
        }
        let guards_end = |from: usize| parts[from..].iter().position(|c| !matches!(c, Cond::Expr(_))).map_or(parts.len(), |p| from + p);
        match &parts[0] {
            Cond::Expr(_) => {
                let end = guards_end(0);
                self.e.w("if ");
                for (i, c) in parts[..end].iter().enumerate() {
                    if i > 0 {
                        self.e.w(" && ");
                    }
                    if let Cond::Expr(x) = c {
                        self.expr(x, 5);
                    }
                }
                self.e.w(" ");
                self.chain_rest(&parts[end..], then, else_, mode);
                self.e.w(" else ");
                self.chain_else(else_, mode);
            }
            Cond::Let { pat, expr, implicit_some } => {
                let end = guards_end(1);
                self.e.w("match ");
                self.expr(expr, P_LOWEST + 1);
                self.e.w(" {");
                self.e.indent();
                self.e.nl();
                self.push_scope();
                let p = self.pat(pat);
                if *implicit_some {
                    self.e.w(&format!("Some({})", p));
                } else {
                    self.e.w(&p);
                }
                self.bind_pat(pat, None);
                // a variable a null check unwrapped keeps its type (`narrow_cond`)
                if let Pat::Ident { name, .. } = pat {
                    if let Some(i) = self.narrowed_types.iter().position(|(n, _)| n == name) {
                        let (n, t) = self.narrowed_types.remove(i);
                        self.bind_name(&n, Some(t));
                    }
                }
                for (i, c) in parts[1..end].iter().enumerate() {
                    self.e.w(if i == 0 { " if " } else { " && " });
                    if let Cond::Expr(x) = c {
                        self.expr(x, 5);
                    }
                }
                self.e.w(" => ");
                self.chain_rest(&parts[end..], then, else_, mode);
                self.pop_scope();
                self.e.nl();
                self.e.w("#[allow(unreachable_patterns)]");
                self.e.nl();
                self.e.w("_ => ");
                self.chain_else(else_, mode);
                self.e.dedent();
                self.e.nl();
                self.e.w("}");
            }
            Cond::Chain(inner) => {
                let mut all = inner.clone();
                all.extend(parts[1..].iter().cloned());
                self.let_chain(&all, then, else_, mode);
            }
        }
    }

    fn chain_rest(&mut self, rest: &[Cond], then: &Block, else_: Option<&Expr>, mode: Option<&Target>) {
        if rest.is_empty() {
            match mode {
                Some(t) => self.block_value(then, t),
                None => self.block_stmt(then),
            }
        } else {
            self.e.w("{");
            self.e.indent();
            self.e.nl();
            self.let_chain(rest, then, else_, mode);
            self.e.dedent();
            self.e.nl();
            self.e.w("}");
        }
    }

    fn chain_else(&mut self, else_: Option<&Expr>, mode: Option<&Target>) {
        match else_ {
            None => self.e.w("{}"),
            Some(el) => match (&el.kind, mode) {
                (ExprKind::Block(b), Some(t)) => self.block_value(b, t),
                (ExprKind::Block(b), None) => self.block_stmt(b),
                (_, Some(t)) => {
                    self.e.w("{");
                    self.e.indent();
                    self.e.nl();
                    self.tail(el, t);
                    self.e.dedent();
                    self.e.nl();
                    self.e.w("}");
                }
                (_, None) => {
                    self.e.w("{");
                    self.e.indent();
                    self.e.nl();
                    let bl = self.stmt_expr(el);
                    if !bl {
                        self.e.w(";");
                    }
                    self.e.dedent();
                    self.e.nl();
                    self.e.w("}");
                }
            },
        }
    }

    /// `if (x != null)` (alone or in an `&&` chain) on a local `T?` that the branch doesn't
    /// assign: `if let Some(x) = &x`, so `x` is the value in the branch. A `Copy` value is
    /// bound as it is, one the branch changes through `&mut x`. Returns the new condition and
    /// the types of the unwrapped names.
    fn narrow_cond(&self, cond: &Cond, then: &Block) -> (Option<Cond>, Vec<(String, Type)>) {
        let Cond::Expr(e) = cond else { return (None, Vec::new()) };
        let mut parts = Vec::new();
        and_operands(e, &mut parts);
        let mut typed = Vec::new();
        let mut out = Vec::new();
        for p in parts {
            match self.null_check(p, BinOp::Ne).and_then(|x| self.narrowable(x, then).map(|t| (x, t))) {
                Some((x, inner)) => {
                    let copy: std::collections::HashSet<String> = self.k.derives.iter().filter(|(_, d)| d.iter().any(|x| x == "Copy")).map(|(n, _)| n.clone()).collect();
                    let by_value = crate::sema::is_copy_simple(&inner, &copy);
                    let mutable = !by_value && visit::writes_through(then, x);
                    let var = Expr::new(ExprKind::Path { segs: vec![x.to_string()], generics: None, global: false }, p.span);
                    let expr = if by_value { var } else { Expr::new(ExprKind::Unary(if mutable { UnOp::RefMut } else { UnOp::Ref }, Box::new(var)), p.span) };
                    let ty = if by_value { inner } else { Type::Ref { lifetime: None, mutable, inner: Box::new(inner) } };
                    typed.push((x.to_string(), ty));
                    out.push(Cond::Let { pat: Pat::Ident { name: x.to_string(), by_ref: false, mutable: false, sub: None, span: p.span }, expr, implicit_some: true });
                }
                None => out.push(Cond::Expr(p.clone())),
            }
        }
        if typed.is_empty() {
            return (None, Vec::new());
        }
        let cond = if out.len() == 1 { out.pop().unwrap() } else { Cond::Chain(out) };
        (Some(cond), typed)
    }

    /// `x == null` / `null == x` (or `!=`), for a variable `x`: its name.
    fn null_check<'e>(&self, e: &'e Expr, op: BinOp) -> Option<&'e str> {
        let ExprKind::Binary(o, l, r) = &strip_paren(e).kind else { return None };
        if *o != op {
            return None;
        }
        match (&l.kind, &r.kind) {
            (_, ExprKind::Null) => l.ident(),
            (ExprKind::Null, _) => r.ident(),
            _ => None,
        }
    }

    /// The value type of a local `T?` named `x` that `body` doesn't assign, so a null check can
    /// unwrap it.
    fn narrowable(&self, x: &str, body: &Block) -> Option<Type> {
        let t = self.local(x)?.clone()?;
        let inner = match t {
            Type::Option(inner) => *inner,
            Type::Path { ref segs, .. } if segs.len() == 1 && segs[0].name == "Option" => match segs[0].generics.as_deref() {
                Some([GenericArg::Type(inner)]) => inner.clone(),
                _ => return None,
            },
            _ => return None,
        };
        let mut assigned = false;
        visit::walk_block(body, &mut |e| {
            if let ExprKind::Assign(_, place, _) = &e.kind {
                assigned |= place.ident() == Some(x);
            }
            !assigned
        });
        (!assigned).then_some(inner)
    }

    /// `if (x == null) return` (or `throw`, `continue`, `break`) on a local `T?`: `x` is the
    /// value after it, `let Some(x) = x else { return };`. Returns false if `e` isn't one.
    fn narrowing_exit(&mut self, e: &Expr) -> bool {
        let ExprKind::If { cond, then, else_: None } = &e.kind else { return false };
        let Cond::Expr(c) = &**cond else { return false };
        let Some(x) = self.null_check(c, BinOp::Eq) else { return false };
        let exits = matches!(then.stmts.last(), Some(Stmt::Expr { expr, .. }) if expr.diverges());
        let empty = Block { stmts: Vec::new(), span: then.span };
        let Some(inner) = self.narrowable(x, &empty).filter(|_| exits) else { return false };
        let x = x.to_string();
        self.e.w(&format!("let Some({}) = {} else ", esc(&x), esc(&x)));
        self.block_stmt(then);
        self.e.w(";");
        self.e.decide(c.span, format!("let Some({}) = {} else", x, x), format!("`{}` is not null after this line", x), None);
        self.bind_name(&x, Some(inner));
        true
    }

    fn cond(&mut self, c: &Cond) {
        match c {
            Cond::Chain(parts) => {
                // only reached for conditions outside `if`/`while`; join the parts
                for (i, p) in parts.iter().enumerate() {
                    if i > 0 {
                        self.e.w(" && ");
                    }
                    self.cond(p);
                }
            }
            Cond::Expr(e) => {
                let parens = contains_struct_lit(e);
                if parens {
                    self.e.w("(");
                }
                self.expr(e, P_LOWEST);
                if parens {
                    self.e.w(")");
                }
            }
            Cond::Let { pat, expr, implicit_some } => {
                let p = self.pat(pat);
                if *implicit_some {
                    self.e.w(&format!("let Some({}) = ", p));
                } else {
                    self.e.w(&format!("let {} = ", p));
                }
                self.expr(expr, P_LOWEST + 1);
                self.bind_pat(pat, None);
            }
        }
    }

    // ------------------------------------------------------------------ paths

    fn path_expr(&mut self, segs: &[String], generics: Option<&Vec<GenericArg>>, global: bool, span: Span) {
        let mut s = String::new();
        if global {
            s.push_str("::");
        }
        if segs.len() == 1 {
            let n = &segs[0];
            if let Some(f) = self.implicit_field(n) {
                // the name after `self.` is marked too, so a position after `self.name` maps
                // exactly after `name` (where rustc's `.clone()` goes)
                let (recv, field) = f.split_at(f.rfind('.').map_or(0, |i| i + 1));
                self.e.w(recv);
                self.e.mark(span);
                self.e.w(field);
                return;
            }
            if !self.is_local(n) && !self.k.is_class_name(n) && !self.k.is_fun_name(n) {
                if let Some(en) = self.variant_enum(n) {
                    s.push_str(&format!("{}::{}", en.name, n));
                    self.e.w(&s);
                    return;
                }
            }
            s.push_str(&esc(n));
        } else {
            s.push_str(&self.join_path(segs));
        }
        if let Some(g) = generics {
            s.push_str("::");
            s.push_str(&self.generic_args_text(g));
        }
        self.e.w(&s);
    }

    pub(crate) fn generic_args_text(&self, g: &[GenericArg]) -> String {
        let parts: Vec<String> = g
            .iter()
            .map(|a| match a {
                GenericArg::Type(t) => self.ty(t, TyPos::Generic),
                GenericArg::Lifetime(l) => format!("'{}", l),
                GenericArg::Binding(n, t) => format!("{} = {}", n, self.ty(t, TyPos::Generic)),
                GenericArg::Const(c) => c.clone(),
            })
            .collect();
        format!("<{}>", parts.join(", "))
    }

    /// Is `Type.method()` a call on the value of a fieldless struct (rather than a static call)?
    fn unit_value_method(&self, ty: &str, method: &str) -> bool {
        let Some(c) = self.class_info(ty) else { return false };
        if !c.is_unit {
            return false;
        }
        if let Some(m) = c.methods.get(method) {
            return m.recv != Recv::Static;
        }
        // a default method of one of the interfaces brs knows
        self.k.traits.values().any(|t| t.methods.get(method).is_some_and(|m| m.recv != Recv::Static))
    }

    /// Interprets `Type.member` / `module.item` chains as Rust paths.
    pub(crate) fn as_path(&self, e: &Expr) -> Option<(Vec<String>, PathKind)> {
        match &e.kind {
            ExprKind::Path { segs, generics: None, global: false } if segs.len() == 1 => {
                let n = &segs[0];
                if self.is_local(n) || self.implicit_field(n).is_some() {
                    return None;
                }
                if n == "Self" || crate::lower::PRIMITIVES.contains(&n.as_str()) {
                    return Some((segs.clone(), PathKind::Type));
                }
                if is_type_like(n) {
                    // a unit variant is a value with members; `Extension.instantiate(..)` with a
                    // tuple variant `Extension(..)` goes through a type of that name
                    let unit = self.variant_enum(n).and_then(|en| en.variant(n)).is_some_and(|v| matches!(v, VariantShape::Unit));
                    if unit && !self.k.is_user_type(n) {
                        return None;
                    }
                    return Some((segs.clone(), PathKind::Type));
                }
                if self.path_roots.contains(n) {
                    return Some((segs.clone(), PathKind::Module));
                }
                None
            }
            // `Stack::<i32>.new()`: a type with explicit generic arguments
            ExprKind::Path { segs, generics: Some(g), global: false } if is_type_like(segs.last().unwrap()) && !self.is_local(&segs[0]) => {
                let mut p = segs.clone();
                let last = p.pop().unwrap();
                p.push(format!("{}::{}", last, self.generic_args_text(g)));
                Some((p, PathKind::Type))
            }
            ExprKind::Path { segs, generics: None, .. } => {
                let last = segs.last().unwrap();
                let kind = if is_type_like(last) {
                    PathKind::Type
                } else if is_const_like(last) {
                    PathKind::Value
                } else {
                    PathKind::Module
                };
                Some((segs.clone(), kind))
            }
            ExprKind::Field { recv, name, safe: false } => {
                let (mut p, k) = self.as_path(recv)?;
                match k {
                    PathKind::Module => {
                        let kind = if is_type_like(name) {
                            PathKind::Type
                        } else if is_const_like(name) {
                            PathKind::Value
                        } else {
                            PathKind::Module
                        };
                        p.push(name.clone());
                        Some((p, kind))
                    }
                    PathKind::Type => {
                        p.push(name.clone());
                        Some((p, PathKind::Value))
                    }
                    PathKind::Value => None,
                }
            }
            _ => None,
        }
    }

    /// Path text for a field access that is really a path (`Color.Red`, `std.f64.consts.PI`).
    fn field_path(&self, e: &Expr) -> Option<(String, PathKind)> {
        if let ExprKind::Field { recv, .. } = &e.kind {
            if matches!(self.as_path(recv), Some((_, PathKind::Module)) | Some((_, PathKind::Type))) {
                let (p, k) = self.as_path(e)?;
                let mut segs = p;
                if segs[0] == "Self" {
                    // keep
                } else if segs.len() == 2 && self.enum_info(&segs[0]).is_none() {
                    // fine
                }
                if segs.first().map(|s| s.as_str()) == Some("this") {
                    segs[0] = "self".into();
                }
                return Some((self.join_path(&segs), k));
            }
        }
        None
    }

    // ------------------------------------------------------------------ calls

    /// Whether the field `name` of the class being lowered may hold something callable: a
    /// function type, `Box<dyn Fn(..)>`, a type parameter, or a type Oxie can't see into.
    fn field_callable(&self, name: &str) -> bool {
        fn callable(t: &Type, generics: &[GenericParam]) -> bool {
            match t {
                Type::Ref { inner, .. } | Type::Ptr { inner, .. } => callable(inner, generics),
                Type::Path { segs, .. } => {
                    let last = segs.last().unwrap();
                    match last.name.as_str() {
                        _ if last.fn_sugar.is_some() => true,
                        "Fn" | "FnMut" | "FnOnce" => true,
                        "Box" | "Rc" | "Arc" => match last.generics.as_deref() {
                            Some([GenericArg::Type(inner)]) => callable(inner, generics),
                            _ => true,
                        },
                        n => segs.len() == 1 && generics.iter().any(|g| matches!(g, GenericParam::Type { name, .. } if name == n)),
                    }
                }
                Type::Option(_) | Type::Tuple(_) | Type::Array(..) | Type::Slice(_) | Type::Never => false,
                _ => true,
            }
        }
        self.class.as_ref().and_then(|c| c.fields.get(name).map(|t| callable(t, &c.generics))).unwrap_or(true)
    }

    pub(crate) fn is_builtin_call(&self, name: &str) -> bool {
        !self.is_local(name) && !self.k.fun_in_scope(name, &self.module) && self.class.as_ref().is_none_or(|c| !c.methods.contains_key(name))
    }

    /// The element type of a list, array or slice expression.
    /// `xs[i]` on a list, array or slice with `i` of an integer type other than `usize`.
    fn int_index(&self, coll: &Expr, i: &Expr) -> bool {
        let Some(t) = self.infer_type(i) else { return false };
        let int = matches!(t.single_name(), Some("i8" | "i16" | "i32" | "i64" | "i128" | "isize" | "u8" | "u16" | "u32" | "u64" | "u128" | "Int" | "Long" | "Short" | "Byte"));
        int && !matches!(i.kind, ExprKind::Int(_)) && self.elem_type(coll).is_some()
    }

    fn elem_type(&self, coll: &Expr) -> Option<Type> {
        let mut t = self.infer_type(coll)?;
        while let Type::Ref { inner, .. } = t {
            t = *inner;
        }
        match t {
            Type::Slice(inner) | Type::Array(inner, _) => Some(*inner),
            Type::Path { segs, .. } if segs.len() == 1 && matches!(segs[0].name.as_str(), "Vec" | "VecDeque") => match segs[0].generics.as_deref() {
                Some([GenericArg::Type(inner)]) => Some(inner.clone()),
                _ => None,
            },
            _ => None,
        }
    }

    /// `Box<Shape>` for an interface value stored in a field or collection (`Shape`, which is
    /// boxed there, or `Box<Shape>`).
    fn boxed_iface(&self, t: &Type) -> Option<Type> {
        let iface = |t: &Type| t.single_name().is_some_and(|n| self.is_trait_name(n)) && matches!(t, Type::Path { segs, .. } if segs[0].generics.is_none());
        if iface(t) {
            return Some(Type::Path { global: false, segs: vec![PathSeg { name: "Box".into(), generics: Some(vec![GenericArg::Type(t.clone())]), fn_sugar: None }], span: Default::default() });
        }
        match t {
            Type::Path { segs, .. } if segs.len() == 1 && segs[0].name == "Box" => match segs[0].generics.as_deref() {
                Some([GenericArg::Type(inner)]) if iface(inner) => Some(t.clone()),
                _ => None,
            },
            _ => None,
        }
    }

    /// The `*`s that reach the interface value inside a box, for an argument of an `&Shape`
    /// parameter: Rust doesn't turn `&Box<dyn Shape>` into `&dyn Shape` by itself.
    fn box_derefs(&self, e: &Expr) -> Option<&'static str> {
        match &e.kind {
            ExprKind::Paren(x) => self.box_derefs(x),
            _ if e.ident().is_some() => {
                let n = e.ident().unwrap();
                match self.local(n) {
                    Some(Some(Type::Ref { inner, .. })) => self.boxed_iface(inner).filter(|_| inner.single_name() == Some("Box")).map(|_| "**"),
                    Some(Some(t)) => self.boxed_iface(t).filter(|_| t.single_name() == Some("Box")).map(|_| "*"),
                    Some(None) => None,
                    // a field of this class: an interface value is boxed there
                    None => self.class.as_ref().and_then(|c| c.fields.get(n)).and_then(|t| self.boxed_iface(t)).map(|_| "*"),
                }
            }
            ExprKind::Index(..) | ExprKind::Field { .. } => self.infer_type(e).and_then(|t| self.boxed_iface(&t)).map(|_| "*"),
            _ => None,
        }
    }

    fn contains_kind(&self, coll: &Expr) -> Contains {
        if is_string_lit(coll) {
            return Contains::Text;
        }
        let t = self.infer_type(coll);
        let mut t = t.as_ref();
        while let Some(Type::Ref { inner, .. }) = t {
            t = Some(inner);
        }
        match t.and_then(|t| t.last_name()) {
            Some("HashMap" | "BTreeMap") => Contains::Key,
            Some("str" | "String") => Contains::Text,
            _ => Contains::Value,
        }
    }

    /// A `char`, which text's `contains` takes by value (`'e' in s` → `s.contains('e')`).
    fn is_char(&self, e: &Expr) -> bool {
        matches!(&e.kind, ExprKind::Char(c) if !c.starts_with('b')) || self.infer_type(e).is_some_and(|t| t.single_name() == Some("char"))
    }

    fn call(&mut self, callee: &Expr, args: &[Arg], span: Span) {
        // `(self.on_click)(x)` calls the function stored in a field; without the parentheses
        // Rust would look for a method
        if let ExprKind::Paren(inner) = &callee.kind {
            if matches!(inner.kind, ExprKind::Field { .. }) {
                self.e.w("(");
                self.expr(inner, P_LOWEST);
                self.e.w(")");
                self.arg_list(args, None, TyPos::Param, span);
                return;
            }
        }
        if let ExprKind::Path { segs, generics, global: false } = &callee.kind {
            if segs.len() == 1 {
                let name = segs[0].clone();
                if self.is_local(&name) {
                    self.e.w(&esc(&name));
                    self.arg_list(args, None, TyPos::Param, span);
                    return;
                }
                if let Some((_, params)) = self.local_funs.iter().rev().find(|(n, _)| *n == name).cloned() {
                    self.e.w(&esc(&name));
                    if let Some(g) = generics {
                        let gt = self.generic_args_text(g);
                        self.e.w(&format!("::{}", gt));
                    }
                    self.arg_list(args, Some(&params), TyPos::Param, span);
                    return;
                }
                // calling a function-typed property (a field of another type leaves the name to a
                // function: `list(..)` with a `list: ListState` field)
                if let Some(f) = self.implicit_field(&name).filter(|_| self.field_callable(&name)) {
                    self.e.w(&format!("({})", f));
                    self.arg_list(args, None, TyPos::Param, span);
                    return;
                }
                if let Some(c) = self.class.as_ref().cloned() {
                    // `drop(x)` is std's: Rust never lets code call `Drop::drop`
                    let std_drop = |m: &&crate::sema::MethodInfo| name == "drop" && m.params.is_empty() && args.len() == 1;
                    if let Some(m) = c.methods.get(&name).filter(|m| !std_drop(m)) {
                        let static_ctx = c.is_static;
                        if m.recv == Recv::Static {
                            self.e.w(&format!("Self::{}", esc(&name)));
                        } else if !static_ctx {
                            self.e.w(&format!("{}.{}", c.self_name, esc(&name)));
                        } else {
                            self.e.w(&esc(&name));
                        }
                        let (args, params) = self.with_this(&name, args, &m.params, span);
                        self.arg_list(&args, Some(&params), TyPos::Param, span);
                        return;
                    }
                }
                if self.is_builtin_call(&name) {
                    if crate::lower::FORMAT_MACROS.iter().any(|(n, _)| *n == name) || crate::lower::OTHER_MACROS.contains(&name.as_str()) {
                        self.macro_call(&name, args, span);
                        return;
                    }
                    // `mutableListOf<T>()`, `mapOf<K, V>()`: empty collections with explicit types
                    if let Some(g) = generics.as_ref().filter(|_| args.is_empty()) {
                        let ty = match name.as_str() {
                            "listOf" | "mutableListOf" | "arrayListOf" | "vecOf" => Some("Vec"),
                            "mapOf" | "mutableMapOf" | "hashMapOf" => Some("::std::collections::HashMap"),
                            "setOf" | "mutableSetOf" | "hashSetOf" => Some("::std::collections::HashSet"),
                            _ => None,
                        };
                        if let Some(ty) = ty {
                            let gt = self.generic_args_text(g);
                            self.e.w(&format!("{}::{}::new()", ty, gt));
                            return;
                        }
                    }
                    if self.builtin_fn(&name, args, span) {
                        return;
                    }
                }
                if let Some(cls) = self.class_info(&name).cloned() {
                    if !cls.is_tuple && !cls.is_union {
                        let g = generics.as_ref().map(|g| format!("::{}", self.generic_args_text(g))).unwrap_or_default();
                        self.e.w(&format!("{}{}::new", esc(&name), g));
                        let params = cls.ctor_params();
                        let pos = if cls.user_new { TyPos::Param } else { TyPos::Field };
                        self.arg_list(args, Some(&params), pos, span);
                        return;
                    }
                }
                if let Some(fi) = self.fun_info(&name).cloned() {
                    self.e.w(&esc(&name));
                    if let Some(g) = generics {
                        let gt = self.generic_args_text(g);
                        self.e.w(&format!("::{}", gt));
                    }
                    self.arg_list(args, Some(&fi.params), TyPos::Param, span);
                    return;
                }
                if !self.k.is_user_type(&name) {
                    if let Some(en) = self.variant_enum(&name).cloned() {
                        let shape = en.variant(&name).cloned().unwrap();
                        self.variant_ctor(&format!("{}::{}", en.name, name), &shape, args, span);
                        return;
                    }
                }
                if is_type_like(&name) && args.is_empty() && !matches!(name.as_str(), "Some" | "None" | "Ok" | "Err") {
                    let g = generics.as_ref().map(|g| format!("::{}", self.generic_args_text(g))).unwrap_or_default();
                    self.e.w(&format!("{}{}::new()", esc(&name), g));
                    return;
                }
                // a function from elsewhere (`gpui::list`) beside a field that can't be called
                if self.implicit_field(&name).is_some() {
                    self.e.w(&esc(&name));
                    if let Some(g) = generics {
                        let gt = self.generic_args_text(g);
                        self.e.w(&format!("::{}", gt));
                    }
                    self.arg_list(args, None, TyPos::Param, span);
                    return;
                }
            } else {
                self.path_call(segs, generics.as_ref(), args, span);
                return;
            }
        }
        self.expr(callee, P_POSTFIX);
        self.arg_list(args, None, TyPos::Param, span);
    }

    /// Call through a path: `Shape::Circle(1.0)`, `Point.origin()`, `std.mem.swap(a, b)`.
    fn path_call(&mut self, segs: &[String], generics: Option<&Vec<GenericArg>>, args: &[Arg], span: Span) {
        // `Outer.Inner(...)`: a type declared inside a class (hoisted next to it)
        if segs.len() > 1 && self.k.nested.get(&segs[0]).is_some_and(|inner| inner.contains(&segs[1])) {
            let rest = segs[1..].to_vec();
            if rest.len() == 1 {
                let callee = Expr::new(ExprKind::Path { segs: rest, generics: generics.cloned(), global: false }, span);
                self.call(&callee, args, span);
            } else {
                self.path_call(&rest, generics, args, span);
            }
            return;
        }
        let n = segs.len();
        if n >= 2 {
            // the type the call goes through: `Self`, `Color` or `shapes.Color`
            let owner = if segs[n - 2] == "Self" {
                vec![self.class.as_ref().map(|c| c.name.clone()).unwrap_or_else(|| "Self".into())]
            } else {
                segs[..n - 1].to_vec()
            };
            let last = &segs[n - 1];
            let path = self.join_path(segs);
            if let Some(en) = self.enum_at(&owner).cloned() {
                if let Some(shape) = en.variant(last).cloned() {
                    self.variant_ctor(&path, &shape, args, span);
                    return;
                }
                if let Some(m) = en.methods.get(last).cloned() {
                    self.e.w(&path);
                    self.arg_list(args, Some(&m.params), TyPos::Param, span);
                    return;
                }
            }
            if let Some(c) = self.class_at(&owner).cloned() {
                if last == "new" && !c.user_new {
                    self.e.w(&path);
                    if let Some(g) = generics {
                        let gt = self.generic_args_text(g);
                        self.e.w(&format!("::{}", gt));
                    }
                    let params = c.ctor_params();
                    self.arg_list(args, Some(&params), TyPos::Field, span);
                    return;
                }
                if let Some(m) = c.methods.get(last).cloned() {
                    self.e.w(&path);
                    self.arg_list(args, Some(&m.params), TyPos::Param, span);
                    return;
                }
            }
            // `shapes.Circle(2.0)`: a class constructed through the module that declares it
            if let Some(c) = self.k.type_key_at(segs, &self.module).and_then(|key| self.k.classes.get(&key)).cloned() {
                if !c.is_tuple && !c.is_union {
                    let g = generics.map(|g| format!("::{}", self.generic_args_text(g))).unwrap_or_default();
                    self.e.w(&format!("{}{}::new", path, g));
                    let params = c.ctor_params();
                    let pos = if c.user_new { TyPos::Param } else { TyPos::Field };
                    self.arg_list(args, Some(&params), pos, span);
                    return;
                }
            }
            // `util.total(v)`: a function called through the module that declares it
            if let Some(fi) = self.fun_at(segs).cloned() {
                let g = generics.map(|g| format!("::{}", self.generic_args_text(g))).unwrap_or_default();
                self.e.w(&format!("{}{}", path, g));
                self.arg_list(args, Some(&fi.params), TyPos::Param, span);
                return;
            }
        }
        let mut path = self.join_path(segs);
        if let Some(g) = generics {
            path.push_str("::");
            path.push_str(&self.generic_args_text(g));
        }
        self.e.w(&path);
        self.arg_list(args, None, TyPos::Param, span);
    }

    fn variant_ctor(&mut self, path: &str, shape: &VariantShape, args: &[Arg], span: Span) {
        match shape {
            VariantShape::Unit => {
                self.e.w(path);
                if !args.is_empty() {
                    self.error(span, "this enum variant has no fields");
                }
            }
            VariantShape::Tuple(types) => {
                let params: Vec<ParamInfo> = types.iter().map(|t| ParamInfo { name: None, ty: t.clone(), default: None, context: None, generics: Vec::new() }).collect();
                self.e.w(path);
                self.arg_list(args, Some(&params), TyPos::Field, span);
            }
            VariantShape::Named(fields) => {
                let params: Vec<ParamInfo> = fields.iter().map(|(n, t)| ParamInfo { name: Some(n.clone()), ty: t.clone(), default: None, context: None, generics: Vec::new() }).collect();
                let slots = self.fill_args(args, &params, span);
                self.e.w(&format!("{} {{ ", path));
                for (i, (p, a)) in params.iter().zip(slots.iter()).enumerate() {
                    if i > 0 {
                        self.e.w(", ");
                    }
                    let name = p.name.clone().unwrap();
                    self.e.w(&format!("{}: ", esc(&name)));
                    match a {
                        Some(x) => self.coerce(x, &p.ty, TyPos::Field),
                        None => self.e.w("::std::default::Default::default()"),
                    }
                }
                self.e.w(" }");
            }
        }
    }

    /// Maps positional and named arguments onto parameters, filling defaults.
    fn fill_args(&mut self, args: &[Arg], params: &[ParamInfo], span: Span) -> Vec<Option<Expr>> {
        let mut slots: Vec<Option<Expr>> = vec![None; params.len()];
        let mut next = 0;
        // a context the callee doesn't use is named `_cx`, and still passed as `cx`
        let named = |p: &ParamInfo, n: &str| p.name.as_deref() == Some(n) || p.context.is_some() && p.name.as_deref().and_then(|x| x.strip_prefix('_')) == Some(n);
        for a in args {
            match &a.name {
                Some(n) => match params.iter().position(|p| named(p, n)) {
                    Some(i) => slots[i] = Some(a.value.clone()),
                    None => self.error(a.value.span, format!("no parameter named `{}`", n)),
                },
                None => {
                    while next < slots.len() && slots[next].is_some() {
                        next += 1;
                    }
                    if next < slots.len() {
                        slots[next] = Some(a.value.clone());
                        next += 1;
                    } else {
                        self.error(a.value.span, format!("too many arguments: expected {}", params.len()));
                    }
                }
            }
        }
        for (i, p) in params.iter().enumerate() {
            if slots[i].is_none() {
                if let (Some(fun), Some(d)) = (&p.context, &p.default) {
                    // a context: the variable of its name where the call is
                    let n = d.ident().unwrap_or_default().to_string();
                    if !self.is_local(&n) && self.implicit_field(&n).is_none() {
                        let ty = crate::symbols::type_text(&p.ty);
                        // a method of a class with the context, whose call here Oxie couldn't follow
                        // when it chose the method's contexts
                        let in_class = self.class.as_ref().and_then(|c| self.class_info(&c.name)).is_some_and(|c| c.contexts.contains(&n));
                        let d = if in_class {
                            Diag::error(span, format!("`{}` needs the context `{}`, and Oxie didn't see that this method passes it on", fun, n)).with_help(format!(
                                "give the method a `context(...)` of its own with `{}: {}` and the other contexts it uses, or pass one: `{}({} = ...)`",
                                n, ty, fun, n
                            ))
                        } else {
                            Diag::error(span, format!("`{}` needs the context `{}`, and there is no `{}` here", fun, n, n))
                                .with_help(format!("declare it on the function that makes this call, `context({}: {}) fun ...`, or pass one: `{}({} = ...)`", n, ty, fun, n))
                        };
                        self.diags.push(d);
                    }
                    slots[i] = Some(Expr::new(d.kind.clone(), span));
                } else if let Some(d) = &p.default {
                    slots[i] = Some(d.clone());
                } else {
                    let n = p.name.clone().unwrap_or_else(|| format!("#{}", i + 1));
                    self.error(span, format!("missing argument `{}`", n));
                }
            }
        }
        slots
    }

    /// The arguments and parameters of a call to a method of this shared class that takes the
    /// handle (`needs_this`): `__this` passed on. Only such methods have it to pass.
    fn with_this(&mut self, name: &str, args: &[Arg], params: &[ParamInfo], span: Span) -> (Vec<Arg>, Vec<ParamInfo>) {
        let (mut args, mut params) = (args.to_vec(), params.to_vec());
        if self.needs_this.contains(name) {
            if self.this_recv.is_none() {
                self.error(span, format!("`{}()` passes `this` on, so only a method of the object can call it, not a constructor, an interface method or an operator", name));
            }
            params.push(ParamInfo { name: Some("__this".into()), ty: Type::Infer, default: None, context: None, generics: Vec::new() });
            args.push(Arg { name: Some("__this".into()), value: Expr::new(ExprKind::Raw("__this".into()), span) });
        }
        (args, params)
    }

    fn arg_list(&mut self, args: &[Arg], params: Option<&[ParamInfo]>, pos: TyPos, span: Span) {
        self.e.w("(");
        match params {
            Some(params) => {
                let slots = self.fill_args(args, params, span);
                let mut first = true;
                for (p, a) in params.iter().zip(slots.iter()) {
                    if let Some(a) = a {
                        if !first {
                            self.e.w(", ");
                        }
                        first = false;
                        // a lambda can't be annotated with the callee's type parameters
                        if matches!(a.kind, ExprKind::Lambda { .. }) && type_mentions(&p.ty, &p.generics) {
                            self.expr(a, P_LOWEST);
                        } else {
                            self.coerce(a, &p.ty, pos);
                        }
                    }
                }
            }
            None => {
                for (i, a) in args.iter().enumerate() {
                    if i > 0 {
                        self.e.w(", ");
                    }
                    if let Some(n) = &a.name {
                        self.error(a.value.span, format!("named argument `{}` can only be used with functions declared in Oxie", n));
                    }
                    if self.is_shared_place(&a.value) {
                        self.shared_copy(&a.value);
                    } else {
                        self.expr(&a.value, P_LOWEST);
                    }
                }
            }
        }
        self.e.w(")");
    }

    /// Kotlin-style helper functions. Returns false if `name` is not one.
    fn builtin_fn(&mut self, name: &str, args: &[Arg], _span: Span) -> bool {
        let values: Vec<Expr> = args.iter().map(|a| a.value.clone()).collect();
        match name {
            "listOf" | "mutableListOf" | "arrayListOf" | "vecOf" => {
                if values.is_empty() {
                    self.e.w("Vec::new()");
                } else {
                    self.e.w("vec![");
                    self.comma_list(&values);
                    self.e.w("]");
                }
                true
            }
            "arrayOf" => {
                self.e.w("[");
                self.comma_list(&values);
                self.e.w("]");
                true
            }
            "mapOf" | "mutableMapOf" | "hashMapOf" => {
                let e = Expr::new(ExprKind::MapLit(values), Span::default());
                self.expr_inner(&e);
                true
            }
            "setOf" | "mutableSetOf" | "hashSetOf" => {
                let e = Expr::new(ExprKind::SetLit(values), Span::default());
                self.expr_inner(&e);
                true
            }
            "run" if values.len() == 1 => {
                if let ExprKind::Lambda { body, .. } = &values[0].kind {
                    match &body.kind {
                        ExprKind::Block(b) => self.block_value(b, &Target::none()),
                        _ => {
                            self.e.w("{ ");
                            self.expr(body, P_LOWEST);
                            self.e.w(" }");
                        }
                    }
                    true
                } else {
                    false
                }
            }
            "repeat" if values.len() == 2 => {
                if let ExprKind::Lambda { params, body, .. } = &values[1].kind {
                    let var = match params.first() {
                        Some(p) => self.pat(&p.pat),
                        None => {
                            if visit::uses_it(body) {
                                "it".into()
                            } else {
                                "_".into()
                            }
                        }
                    };
                    self.e.w(&format!("for {} in 0..", var));
                    self.expr(&values[0], P_RANGE + 1);
                    self.e.w(" ");
                    self.push_scope();
                    self.bind_name(&var, None);
                    self.loops.push(LoopCtx { dowhile_label: None });
                    match &body.kind {
                        ExprKind::Block(b) => self.block_stmt(b),
                        _ => {
                            self.e.w("{");
                            self.e.indent();
                            self.e.nl();
                            let bl = self.stmt_expr(body);
                            if !bl {
                                self.e.w(";");
                            }
                            self.e.dedent();
                            self.e.nl();
                            self.e.w("}");
                        }
                    }
                    self.loops.pop();
                    self.pop_scope();
                    true
                } else {
                    false
                }
            }
            _ => false,
        }
    }

    /// `println(...)` and friends.
    fn macro_call(&mut self, name: &str, args: &[Arg], span: Span) {
        let fmt_pos = crate::lower::FORMAT_MACROS.iter().find(|(n, _)| *n == name).map(|(_, p)| *p);
        if name == "vec" {
            self.e.w("vec![");
            let v: Vec<Expr> = args.iter().map(|a| a.value.clone()).collect();
            self.comma_list(&v);
            self.e.w("]");
            return;
        }
        if let ([a], "include_str" | "include_bytes" | "include") = (args, name) {
            if let ExprKind::Str(parts) = &a.value.kind {
                if let [crate::lexer::StrPart::Lit(path)] = parts.as_slice() {
                    let text = self.include_paths(&format!("{}!(\"{}\")", name, path));
                    self.e.w(&text);
                    return;
                }
            }
        }
        self.e.w(&format!("{}!(", name));
        let Some(pos) = fmt_pos else {
            for (i, a) in args.iter().enumerate() {
                if i > 0 {
                    self.e.w(", ");
                }
                self.expr(&a.value, P_LOWEST);
            }
            self.e.w(")");
            return;
        };
        let lead = pos.min(args.len());
        for (i, a) in args[..lead].iter().enumerate() {
            if i > 0 {
                self.e.w(", ");
            }
            self.expr(&a.value, P_LOWEST);
        }
        let rest = &args[lead..];
        if !rest.is_empty() {
            if lead > 0 {
                self.e.w(", ");
            }
            match &rest[0].value.kind {
                ExprKind::Template(parts) => {
                    self.format_args(parts);
                    for a in &rest[1..] {
                        self.e.w(", ");
                        self.expr(&a.value, P_LOWEST);
                    }
                }
                ExprKind::Str(parts) if rest.len() == 1 => {
                    self.e.w(&format!("\"{}\"", escape_braces(&crate::lower::lit_parts(parts))));
                }
                ExprKind::Str(_) | ExprKind::RawStr(_) => {
                    for (i, a) in rest.iter().enumerate() {
                        if i > 0 {
                            self.e.w(", ");
                        }
                        self.expr(&a.value, P_LOWEST);
                    }
                }
                _ => {
                    let sep = if matches!(name, "println" | "print" | "eprintln" | "eprint") { " " } else { "" };
                    let fmt = vec!["{}"; rest.len()].join(sep);
                    self.e.w(&format!("\"{}\"", fmt));
                    for a in rest {
                        self.e.w(", ");
                        self.expr(&a.value, P_LOWEST);
                    }
                }
            }
        }
        let _ = span;
        self.e.w(")");
    }

    /// `"text {} {name}", args` for a template.
    pub(crate) fn format_args(&mut self, parts: &[TemplatePart]) {
        // the format string in pieces, each name read inline (`{low}`) with the span of its
        // name: rustc points inside the string for errors about it (`low` has no `Display`)
        let mut pieces: Vec<(String, Option<Span>)> = Vec::new();
        let mut args: Vec<&Expr> = Vec::new();
        for p in parts {
            match p {
                TemplatePart::Lit(s) => pieces.push((escape_braces(s), None)),
                TemplatePart::Expr(e, spec) => {
                    let spec_s = spec.as_ref().map(|s| format!(":{}", s)).unwrap_or_default();
                    if let Some(n) = e.ident() {
                        if self.is_local(n) && !n.starts_with("r#") && esc(n) == n {
                            pieces.push((format!("{{{}{}}}", n, spec_s), Some(e.span)));
                            continue;
                        }
                    }
                    pieces.push((format!("{{{}}}", spec_s), None));
                    args.push(e);
                }
            }
        }
        self.e.w("\"");
        for (text, span) in pieces {
            if let Some(s) = span {
                // the `{` maps to the `$` of `$low` (the `{` of `${low}`), right before the name
                self.e.mark(Span::empty_at(s.start.saturating_sub(1)));
            }
            self.e.w(&text);
        }
        self.e.w("\"");
        for a in args {
            self.e.w(", ");
            self.expr(a, P_LOWEST);
        }
    }

    fn method_call(&mut self, recv: &Expr, name: &str, generics: Option<&Vec<GenericArg>>, args: &[Arg], span: Span) {
        // Kotlin scope function: x.let { ... }
        if name == "let" && args.len() == 1 {
            if let ExprKind::Lambda { params, body, .. } = &args[0].value.kind {
                let var = match params.first() {
                    Some(p) => self.pat(&p.pat),
                    None => "it".to_string(),
                };
                self.e.w(&format!("{{ let {} = ", var));
                self.expr(recv, P_LOWEST);
                self.e.w("; ");
                self.push_scope();
                match params.first() {
                    Some(p) => self.bind_pat(&p.pat, None),
                    None => self.bind_name("it", None),
                }
                match &body.kind {
                    ExprKind::Block(b) => {
                        self.e.indent();
                        self.stmts_with_tail(&b.stmts, Some(&Target::none()));
                        self.e.dedent();
                        self.e.nl();
                        self.e.w("}");
                    }
                    _ => {
                        self.expr(body, P_LOWEST);
                        self.e.w(" }");
                    }
                }
                self.pop_scope();
                return;
            }
        }
        // `Square.name()` on a fieldless struct: the type name is also its value
        if let Some(n) = recv.ident() {
            if !self.is_local(n) && self.unit_value_method(n, name) {
                self.e.w(&format!("{}.{}", esc(n), member(name)));
                if let Some(g) = generics {
                    let gt = self.generic_args_text(g);
                    self.e.w(&format!("::{}", gt));
                }
                let params = self.class_info(n).and_then(|c| c.methods.get(name)).map(|m| m.params.clone());
                self.arg_list(args, params.as_deref(), TyPos::Param, span);
                return;
            }
        }
        if let Some((mut p, kind)) = self.as_path(recv) {
            if matches!(kind, PathKind::Module | PathKind::Type) {
                p.push(name.to_string());
                self.path_call(&p, generics, args, span);
                return;
            }
        }
        // data class copy(field = value)
        if name == "copy" && !args.is_empty() && args.iter().all(|a| a.name.is_some()) {
            let v = self.fresh("copy");
            self.e.w(&format!("{{ let mut {} = ", v));
            self.expr(recv, P_POSTFIX);
            self.e.w(".clone();");
            for a in args {
                self.e.w(&format!(" {}.{} = ", v, esc(a.name.as_ref().unwrap())));
                self.expr(&a.value, P_LOWEST);
                self.e.w(";");
            }
            self.e.w(&format!(" {} }}", v));
            return;
        }
        // method of an Oxie class when the receiver's type is known
        if !matches!(recv.kind, ExprKind::SelfValue) {
            let owner = self.infer_type(recv).and_then(|t| self.type_owner(&t));
            let m = owner.as_ref().and_then(|o| self.class_at(o).and_then(|c| c.methods.get(name)).or_else(|| self.enum_at(o).and_then(|e| e.methods.get(name)))).cloned();
            if let Some(m) = m {
                self.expr(recv, P_POSTFIX);
                self.e.w(&format!(".{}", esc(name)));
                if let Some(g) = generics {
                    let gt = self.generic_args_text(g);
                    self.e.w(&format!("::{}", gt));
                }
                self.arg_list(args, Some(&m.params), TyPos::Param, span);
                return;
            }
        }
        // self.method(...) with known parameters / calling a function-typed field
        if matches!(recv.kind, ExprKind::SelfValue) {
            if let Some(c) = self.class.as_ref().cloned() {
                if let Some(m) = c.methods.get(name) {
                    self.expr(recv, P_POSTFIX);
                    self.e.w(&format!(".{}", esc(name)));
                    let (args, params) = self.with_this(name, args, &m.params, span);
                    self.arg_list(&args, Some(&params), TyPos::Param, span);
                    return;
                }
                if matches!(c.fields.get(name), Some(Type::Fn { .. })) {
                    self.e.w("(");
                    self.expr(recv, P_POSTFIX);
                    self.e.w(&format!(".{})", esc(name)));
                    self.arg_list(args, None, TyPos::Param, span);
                    return;
                }
            }
        }
        let args = self.fix_lambda_arity(recv, name, args);
        self.expr(recv, P_POSTFIX);
        self.e.w(".");
        self.e.w(&member(name));
        if let Some(g) = generics {
            let gt = self.generic_args_text(g);
            self.e.w(&format!("::{}", gt));
        }
        if REF_PREDICATES.contains(&name) && args.len() == 1 && matches!(args[0].value.kind, ExprKind::Lambda { .. }) {
            self.e.w("(");
            self.next_lambda_refs = true;
            self.lambda(&args[0].value, None, None);
            self.e.w(")");
            return;
        }
        self.arg_list(&args, None, TyPos::Param, span);
    }

    /// Kotlin lambdas may ignore their argument (`map_err { MyError() }`); Rust closures must
    /// declare it. Adds `_` for std methods whose closure always receives a value.
    fn fix_lambda_arity(&self, recv: &Expr, name: &str, args: &[Arg]) -> Vec<Arg> {
        const ONE_ARG: &[&str] = &[
            "map", "map_err", "and_then", "filter", "for_each", "any", "all", "find", "position", "inspect", "inspect_err", "retain",
            "sort_by_key", "sort_unstable_by_key", "max_by_key", "min_by_key", "flat_map", "filter_map", "take_while", "skip_while",
            "map_while", "find_map", "partition", "is_some_and", "is_ok_and", "is_err_and", "map_or", "or_insert_with_key", "group_by",
            "dedup_by_key", "chunk_by", "split", "rsplit", "splitn", "matches", "trim_matches", "trim_start_matches", "trim_end_matches",
            "filter_not", "find_last", "index_of_first", "none", "count", "sum_of", "max_of", "min_of", "sorted_by", "sorted_by_descending",
            "distinct_by", "associate_by",
        ];
        let result_like = matches!(name, "unwrap_or_else" | "or_else" | "map_or_else") && self.is_result_expr(recv);
        if !ONE_ARG.contains(&name) && !result_like {
            return args.to_vec();
        }
        args.iter()
            .map(|a| {
                let mut a = a.clone();
                if let ExprKind::Lambda { params, body, .. } = &mut a.value.kind {
                    if params.is_empty() && !visit::uses_it(body) {
                        params.push(LambdaParam { pat: Pat::Wild, ty: None });
                    }
                    // `sum_of { it }` sums the elements, not references to them
                    if matches!(name, "sum_of" | "max_of" | "min_of") {
                        let param = match params.first() {
                            None => Some("it".to_string()),
                            Some(LambdaParam { pat: Pat::Ident { name, .. }, .. }) => Some(name.clone()),
                            _ => None,
                        };
                        let tail = match &mut body.kind {
                            ExprKind::Block(b) if b.stmts.len() == 1 => match &mut b.stmts[0] {
                                Stmt::Expr { expr, semi: false } => Some(expr),
                                _ => None,
                            },
                            _ => Some(&mut **body),
                        };
                        if let (Some(p), Some(tail)) = (param, tail) {
                            if tail.ident() == Some(p.as_str()) {
                                let span = tail.span;
                                *tail = Expr::new(ExprKind::Unary(UnOp::Deref, Box::new(tail.clone())), span);
                            }
                        }
                    }
                }
                a
            })
            .collect()
    }

    // ------------------------------------------------------------------ safe navigation

    fn link_suffix(&mut self, link: &Expr) {
        match &link.kind {
            ExprKind::Field { name, .. } => {
                self.e.w(".");
                self.e.w(&member(name));
            }
            ExprKind::MethodCall { name, generics, args, .. } => {
                self.e.w(".");
                self.e.w(&member(name));
                if let Some(g) = generics {
                    let gt = self.generic_args_text(g);
                    self.e.w(&format!("::{}", gt));
                }
                self.arg_list(args, None, TyPos::Param, link.span);
            }
            ExprKind::Index(_, idx) => {
                self.e.w("[");
                self.expr(idx, P_LOWEST);
                self.e.w("]");
            }
            _ => {}
        }
    }

    fn split_segments<'e>(links: &[&'e Expr]) -> (Vec<&'e Expr>, Vec<Vec<&'e Expr>>) {
        let first = links.iter().position(|l| link_is_safe(l)).unwrap_or(links.len());
        let prefix = links[..first].to_vec();
        let mut segs: Vec<Vec<&Expr>> = Vec::new();
        for l in &links[first..] {
            if link_is_safe(l) {
                segs.push(vec![*l]);
            } else {
                segs.last_mut().unwrap().push(*l);
            }
        }
        (prefix, segs)
    }

    /// Does this expression produce an `Option` of a reference (so `?.field` must borrow)?
    fn yields_borrowed_option(&self, e: &Expr) -> bool {
        match &e.kind {
            ExprKind::Call { callee, .. } => {
                let ret = callee.ident().and_then(|n| self.fun_info(n)).and_then(|f| f.ret.clone());
                matches!(ret, Some(Type::Option(inner)) if matches!(*inner, Type::Ref { .. }))
            }
            ExprKind::MethodCall { name, .. } => matches!(
                name.as_str(),
                "get" | "first" | "last" | "find" | "iter" | "max_by_key" | "min_by_key" | "max_by" | "min_by" | "max" | "min" | "get_mut" | "peek" | "next" | "as_ref" | "as_deref" | "last_mut" | "first_mut"
            ),
            ExprKind::Paren(x) => self.yields_borrowed_option(x),
            _ => false,
        }
    }

    fn emit_chain_base(&mut self, base: &Expr, prefix: &[&Expr]) -> bool {
        let place = self.is_place(base) && prefix.iter().all(|l| matches!(l.kind, ExprKind::Field { .. } | ExprKind::Index(..)));
        match prefix.last() {
            Some(l) => self.expr(l, P_POSTFIX),
            None => self.expr(base, P_POSTFIX),
        }
        place
    }

    fn let_lambda(seg: &[&Expr]) -> Option<Expr> {
        if seg.len() == 1 {
            if let ExprKind::MethodCall { name, args, .. } = &seg[0].kind {
                if name == "let" && args.len() == 1 && matches!(args[0].value.kind, ExprKind::Lambda { .. }) {
                    return Some(args[0].value.clone());
                }
            }
        }
        None
    }

    /// `a?.b.c?.d()` -> `a.as_ref().and_then(|v| v.b.c.as_ref()).map(|v| v.d())`
    fn safe_chain(&mut self, e: &Expr) {
        let (base, links) = chain_links(e);
        let (prefix, segs) = Self::split_segments(&links);
        let base_expr_borrowed = prefix.last().map_or_else(|| self.yields_borrowed_option(base), |l| self.yields_borrowed_option(l));
        let place = self.emit_chain_base(base, &prefix);
        if place {
            self.e.w(".as_ref()");
        }
        let mut borrowed = place || base_expr_borrowed;
        let n = segs.len();
        for (i, seg) in segs.iter().enumerate() {
            let last = i == n - 1;
            let flatten = !last || self.segment_yields_option(seg);
            let method = if flatten { "and_then" } else { "map" };
            if let Some(lam) = Self::let_lambda(seg) {
                self.e.w(&format!(".{}(", method));
                self.lambda(&lam, None, None);
                self.e.w(")");
                continue;
            }
            let v = self.fresh("v");
            // a shared object's field is read through its getter (a copy) at the end
            let getter = matches!(&seg.last().unwrap().kind, ExprKind::Field { recv, name, .. } if self.optional_shared_field(recv, name).is_some());
            let ends_in_field = matches!(seg.last().unwrap().kind, ExprKind::Field { .. } | ExprKind::Index(..)) && !getter;
            // a field read through a reference is borrowed (`&v.name`), like Kotlin's reference semantics
            let amp = borrowed && !flatten && ends_in_field;
            self.e.w(&format!(".{}(|{}| {}{}", method, v, if amp { "&" } else { "" }, v));
            self.segment_links(seg);
            if flatten && ends_in_field {
                self.e.w(".as_ref()");
            }
            self.e.w(")");
            borrowed = flatten && ends_in_field;
        }
    }

    /// Emits the links of a `?.` segment after its value. A shared object's field is read as
    /// outside `?.`: through the getter, a copy (`v.name()`), at the end, and through one borrow
    /// inside (`v.0.read().items.len()`).
    fn segment_links(&mut self, seg: &[&Expr]) {
        for (i, l) in seg.iter().enumerate() {
            let shared = match &l.kind {
                ExprKind::Field { recv, name, .. } => self.optional_shared_field(recv, name).map(|sync| (name, sync)),
                _ => None,
            };
            match shared {
                Some((name, _)) if i == seg.len() - 1 => self.e.w(&format!(".{}()", member(name))),
                Some((name, _)) => {
                    let write = matches!(&seg[i + 1].kind, ExprKind::MethodCall { name, .. } if crate::sema::MUTATING_METHODS.contains(&name.as_str()));
                    let access = if write { "write()" } else { "read()" };
                    self.e.w(&format!(".0.{}.{}", access, member(name)));
                }
                None => self.link_suffix(l),
            }
        }
    }

    /// The type of a value in a `?.` chain: `infer_type`, or the declared type of a class field
    /// reached through `?.` (`a.parent?.parent`).
    fn chain_value_type(&self, e: &Expr) -> Option<Type> {
        if let Some(t) = self.infer_type(e) {
            return Some(t);
        }
        let ExprKind::Field { recv, name, .. } = &e.kind else { return None };
        let mut t = self.chain_value_type(recv)?;
        while let Type::Option(inner) | Type::Ref { inner, .. } = t {
            t = *inner;
        }
        self.class_info(t.single_name()?)?.fields.iter().find(|f| &f.name == name).map(|f| f.ty.clone())
    }

    /// Is `recv.field` (in a `?.` chain) a field of a shared class, reached through a `T?` or
    /// a reference? Returns whether the class is `@sync`.
    fn optional_shared_field(&self, recv: &Expr, field: &str) -> Option<bool> {
        let mut t = self.chain_value_type(recv);
        while let Some(Type::Option(inner) | Type::Ref { inner, .. }) = t {
            t = Some(*inner);
        }
        let c = self.class_info(t.as_ref()?.single_name()?)?;
        (c.is_shared && c.fields.iter().any(|f| f.name == field)).then_some(c.is_sync)
    }

    /// `?.` means Kotlin safe navigation on optional values and Rust's `?` + `.` on results.
    /// Rewrites the chain so that links decided as error propagation become `Try` + member access.
    pub(crate) fn resolve_question_dots(&self, e: &Expr) -> Expr {
        let (base, links) = chain_links(e);
        let force = self.force_safe
            || links.iter().any(|l| link_is_safe(l) && matches!(&l.kind, ExprKind::MethodCall { name, .. } if name == "let"));
        let mut cur = base.clone();
        let mut cur_is_option = false;
        for l in links {
            let mut node = l.clone();
            let mut safe = link_is_safe(l);
            // `?.` after a property declared non-nullable is just `.` (as in Kotlin)
            if safe && cur_is_option {
                if let ExprKind::Field { name, .. } = &cur.kind {
                    if self.field_known_non_option(name) {
                        safe = false;
                        if let ExprKind::Field { safe: s, .. } | ExprKind::MethodCall { safe: s, .. } = &mut node.kind {
                            *s = false;
                        }
                    }
                }
            }
            let mut make_try = false;
            if safe && !force && !cur_is_option {
                make_try = if self.is_result_expr(&cur) {
                    true
                } else if self.is_option_expr(&cur) {
                    false
                } else {
                    self.can_propagate()
                };
            }
            let new_recv = if make_try {
                let sp = cur.span;
                Expr::new(ExprKind::Try(Box::new(cur)), sp)
            } else {
                cur
            };
            match &mut node.kind {
                ExprKind::Field { recv, safe: s, .. } | ExprKind::MethodCall { recv, safe: s, .. } => {
                    **recv = new_recv;
                    if make_try {
                        *s = false;
                    }
                }
                ExprKind::Index(recv, _) => **recv = new_recv,
                _ => {}
            }
            if safe && !make_try {
                cur_is_option = true;
            }
            cur = node;
        }
        cur
    }

    /// Every Oxie class field with this name is declared with a non-optional type.
    fn field_known_non_option(&self, name: &str) -> bool {
        let mut any = false;
        for c in self.k.classes.values() {
            if let Some(f) = c.fields.iter().find(|f| f.name == name) {
                if matches!(f.ty, Type::Option(_)) || f.ty.single_name() == Some("Option") || f.ty == Type::Infer {
                    return false;
                }
                any = true;
            }
        }
        any
    }

    fn can_propagate(&self) -> bool {
        match self.fns.last() {
            Some(f) if !f.tries.is_empty() => true,
            Some(f) if !f.is_lambda => f.throws || f.ret.as_ref().is_some_and(|t| t.last_name() == Some("Result")),
            _ => false,
        }
    }

    fn is_result_expr(&self, e: &Expr) -> bool {
        const RESULT_METHODS: &[&str] = &[
            "parse", "read_to_string", "read_line", "read_to_end", "read_exact", "write_all", "flush", "open", "create", "read_dir",
            "metadata", "canonicalize", "from_str", "from_utf8", "try_into", "try_from", "lock", "send", "recv", "accept", "connect",
            "bind", "json", "text", "bytes", "context", "with_context", "map_err", "ok_or", "ok_or_else", "send_timeout", "recv_timeout",
            "try_recv", "elapsed", "duration_since", "read_line_to", "remove_file", "create_dir_all", "copy", "rename",
        ];
        const RESULT_FNS: &[&str] = &[
            "read_to_string", "read", "write", "open", "create", "from_str", "from_slice", "from_reader", "to_string_pretty", "var", "read_dir", "metadata",
            "remove_file", "remove_dir", "remove_dir_all", "create_dir", "create_dir_all", "copy", "rename", "canonicalize", "read_link", "hard_link",
            "set_permissions",
        ];
        if self.is_throwing_call(e) {
            return true;
        }
        match &e.kind {
            ExprKind::Await(_) => true,
            ExprKind::MethodCall { recv, name, .. } => {
                if let Some((p, _)) = self.as_path(recv) {
                    let _ = p;
                    return RESULT_FNS.contains(&name.as_str()) || RESULT_METHODS.contains(&name.as_str());
                }
                RESULT_METHODS.contains(&name.as_str())
            }
            ExprKind::Call { callee, .. } => match &callee.kind {
                ExprKind::Path { segs, .. } if segs.len() > 1 => RESULT_FNS.contains(&segs.last().unwrap().as_str()),
                _ => false,
            },
            ExprKind::Paren(x) => self.is_result_expr(x),
            _ => matches!(self.infer_type(e), Some(t) if t.last_name() == Some("Result")),
        }
    }

    fn is_option_expr(&self, e: &Expr) -> bool {
        const OPTION_METHODS: &[&str] = &[
            "get", "get_mut", "first", "last", "find", "find_map", "pop", "next", "next_back", "peek", "max", "min", "max_by_key",
            "min_by_key", "max_by", "min_by", "position", "rposition", "nth", "last_mut", "first_mut", "parent", "file_name",
            "extension", "file_stem", "checked_add", "checked_sub", "checked_mul", "checked_div", "strip_prefix", "strip_suffix",
            "split_once", "rsplit_once", "to_str", "ok", "err", "pop_front", "pop_back", "front", "back", "get_key_value", "char_indices_next",
            "and_then", "filter", "or_else", "remove_entry", "find_char", "chars_next",
        ];
        if has_safe_link(e) || self.place_is_option(e) {
            return true;
        }
        match &e.kind {
            ExprKind::MethodCall { name, .. } => {
                if OPTION_METHODS.contains(&name.as_str()) {
                    return true;
                }
            }
            ExprKind::Field { name, .. } => {
                let mut segs: Vec<&Expr> = Vec::new();
                segs.push(e);
                if self.segment_yields_option(&segs) {
                    return true;
                }
                let _ = name;
            }
            ExprKind::Paren(x) => return self.is_option_expr(x),
            _ => {}
        }
        matches!(self.infer_type(e), Some(Type::Option(_))) || matches!(self.infer_type(e), Some(t) if t.last_name() == Some("Option"))
    }

    /// Does a `?.` segment produce an `Option` (so the chain must be flattened like Kotlin)?
    /// Decided from Oxie declarations: `T?` fields and methods returning `T?`.
    fn segment_yields_option(&self, seg: &[&Expr]) -> bool {
        let is_opt = |t: &Type| matches!(t, Type::Option(_)) || t.single_name() == Some("Option");
        let field_opt = |name: &str| {
            let mut any = false;
            for c in self.k.classes.values() {
                if let Some(f) = c.fields.iter().find(|f| f.name == name) {
                    if !is_opt(&f.ty) {
                        return false;
                    }
                    any = true;
                }
            }
            any
        };
        let method_opt = |name: &str| {
            let mut any = false;
            for c in self.k.classes.values() {
                if let Some(m) = c.methods.get(name) {
                    match &m.ret {
                        Some(t) if is_opt(t) => any = true,
                        _ => return false,
                    }
                }
            }
            any
        };
        let mut opt = false;
        for (_i, l) in seg.iter().enumerate() {
            match &l.kind {
                ExprKind::Field { name, .. } => opt = field_opt(name),
                ExprKind::MethodCall { name, .. } => {
                    if opt {
                        // these keep an Option an Option
                        opt = matches!(name.as_str(), "clone" | "as_ref" | "as_deref" | "cloned" | "copied" | "as_mut" | "take" | "filter" | "or" | "xor");
                    } else {
                        opt = method_opt(name);
                    }
                }
                _ => opt = false,
            }
        }
        opt
    }

    /// Statement form: `if let Some(v) = &a { v.b(); }`. Returns false if not applicable.
    fn safe_chain_stmt(&mut self, e: &Expr) -> bool {
        let (base, links) = chain_links(e);
        let (prefix, segs) = Self::split_segments(&links);
        if segs.len() != 1 {
            return false;
        }
        let seg = &segs[0];
        let lam = Self::let_lambda(seg);
        let var = match &lam {
            Some(l) => match &l.kind {
                ExprKind::Lambda { params, body, .. } => match params.first() {
                    Some(p) => self.pat(&p.pat),
                    None if visit::uses_it(body) => "it".into(),
                    None => "_".into(),
                },
                _ => unreachable!(),
            },
            None => self.fresh("v"),
        };
        let place = self.is_place(base) && prefix.iter().all(|l| matches!(l.kind, ExprKind::Field { .. } | ExprKind::Index(..)));
        self.e.w(&format!("if let Some({}) = ", var));
        if place {
            self.e.w("&");
        }
        self.emit_chain_base(base, &prefix);
        self.e.w(" {");
        self.e.indent();
        self.push_scope();
        self.bind_name(&var, None);
        match lam {
            Some(l) => {
                if let ExprKind::Lambda { body, .. } = &l.kind {
                    match &body.kind {
                        ExprKind::Block(b) => self.stmts_with_tail(&b.stmts, None),
                        _ => {
                            self.e.nl();
                            let bl = self.stmt_expr(body);
                            if !bl {
                                self.e.w(";");
                            }
                        }
                    }
                }
            }
            None => {
                self.e.nl();
                self.e.w(&var);
                self.segment_links(seg);
                self.e.w(";");
            }
        }
        self.pop_scope();
        self.e.dedent();
        self.e.nl();
        self.e.w("}");
        true
    }

    // ------------------------------------------------------------------ operators

    /// Is `e` a `+` of text that becomes `format!`: with a literal or template, or one Rust
    /// rejects (an owned `String` on the right, borrowed text on the left). Neither side moves.
    fn is_string_concat(&self, e: &Expr) -> bool {
        let mut ops = Vec::new();
        concat_operands(e, &mut ops);
        if ops.len() < 2 {
            return false;
        }
        let borrowed_text = |o: &Expr| matches!(self.infer_type(o), Some(Type::Ref { inner, .. }) if matches!(inner.single_name(), Some("str" | "String")));
        ops.iter().any(|o| is_string_lit(o)) || borrowed_text(ops[0]) || ops[1..].iter().any(|o| self.is_owned_string(o))
    }

    /// Is `e` known to be an owned `String`?
    fn is_owned_string(&self, e: &Expr) -> bool {
        matches!(self.infer_type(e), Some(t) if t.single_name() == Some("String"))
    }

    fn string_concat(&mut self, e: &Expr) {
        let mut nodes: Vec<&Expr> = Vec::new();
        let mut ops: Vec<&Expr> = Vec::new();
        spine(e, &mut ops, &mut nodes);
        let first = ops.iter().position(|o| is_string_lit(o)).unwrap_or(0);
        let mut fmt = String::new();
        let mut args: Vec<&Expr> = Vec::new();
        if first == 1 {
            fmt.push_str("{}");
            args.push(ops[0]);
        } else if first > 1 {
            fmt.push_str("{}");
            args.push(nodes[first - 2]);
        }
        let mut inline_args: Vec<(usize, Vec<TemplatePart>)> = Vec::new();
        let _ = &mut inline_args;
        for o in &ops[first..] {
            match &o.kind {
                ExprKind::Str(parts) => fmt.push_str(&escape_braces(&crate::lower::lit_parts(parts))),
                ExprKind::Template(parts) => {
                    for p in parts {
                        match p {
                            TemplatePart::Lit(s) => fmt.push_str(&escape_braces(s)),
                            TemplatePart::Expr(x, spec) => {
                                fmt.push_str(&format!("{{{}}}", spec.as_ref().map(|s| format!(":{}", s)).unwrap_or_default()));
                                args.push(x);
                            }
                        }
                    }
                }
                _ => {
                    fmt.push_str("{}");
                    args.push(o);
                }
            }
        }
        self.e.w(&format!("format!(\"{}\"", fmt));
        for a in args {
            self.e.w(", ");
            self.expr(a, P_LOWEST);
        }
        self.e.w(")");
    }

    /// A default for `?:` that costs nothing to compute (a literal, a constant, a variable, a
    /// field), so it can be passed to `or_val`, which reads the value behind a `?.` reference.
    fn is_plain_default(&self, r: &Expr) -> bool {
        match &r.kind {
            ExprKind::Path { .. } => true,
            ExprKind::Field { safe: false, .. } => self.as_path(r).is_some() || self.is_place(r),
            ExprKind::Paren(x) => self.is_plain_default(x),
            _ => is_literal(r),
        }
    }

    fn elvis(&mut self, l: &Expr, r: &Expr) {
        let saved = self.force_safe;
        self.force_safe = true;
        if self.is_plain_default(r) {
            self.e.w(&format!("{}::ElvisOr::or_val(", RT));
            self.expr(l, P_LOWEST);
            self.force_safe = saved;
            self.e.w(", ");
            self.expr(r, P_LOWEST);
            self.e.w(")");
            return;
        }
        let v = self.fresh("v");
        self.e.w(&format!("match {}::Elvis::opt(", RT));
        self.expr(l, P_LOWEST);
        self.force_safe = saved;
        self.e.w(") {");
        self.e.indent();
        self.e.w(&format!("\nSome({v}) => {v},\nNone => "));
        self.expr(r, P_LOWEST);
        self.e.w(",");
        self.e.dedent();
        self.e.w("\n}");
    }

    fn return_expr(&mut self, v: Option<&Expr>) {
        let ctx = self.fns.last().cloned();
        match self.fns.last_mut() {
            // in a `toString` body, `return` leaves the block that computes the text
            Some(f) if f.text_break.is_some() => {
                f.text_break = Some(true);
                self.e.w(&format!("break '{}", TEXT_LABEL));
            }
            _ => self.e.w("return"),
        }
        match ctx {
            Some(f) if !f.is_lambda => {
                let wrap = f.throws && !v.is_some_and(is_ok_err_call) && !v.is_some_and(|x| x.diverges());
                match v {
                    Some(x) if wrap && self.is_throwing_call(x) => {
                        self.e.w(" Ok(");
                        self.expr(x, P_POSTFIX);
                        self.e.w("?)");
                    }
                    Some(x) => {
                        self.e.w(" ");
                        if wrap {
                            self.e.w("Ok(");
                        }
                        match &f.ret {
                            Some(_) if f.unboxed_ret => self.expr(x, P_LOWEST),
                            Some(t) => self.coerce(x, t, TyPos::Local),
                            None => self.expr(x, P_LOWEST),
                        }
                        if wrap {
                            self.e.w(")");
                        }
                    }
                    None => {
                        if f.throws {
                            self.e.w(" Ok(())");
                        }
                    }
                }
            }
            _ => {
                if let Some(x) = v {
                    self.e.w(" ");
                    match self.fns.last().and_then(|f| f.ret.clone()) {
                        Some(t) => self.coerce(x, &t, TyPos::Local),
                        None => self.expr(x, P_LOWEST),
                    }
                }
            }
        }
    }

    // ------------------------------------------------------------------ control flow

    /// `mode`: None = statement, Some(target) = value.
    fn if_expr(&mut self, cond: &Cond, then: &Block, else_: Option<&Expr>, mode: Option<&Target>) {
        // `x != null` unwraps `x` in the branch
        let (narrowed, typed) = self.narrow_cond(cond, then);
        let cond = narrowed.as_ref().unwrap_or(cond);
        if let Cond::Chain(parts) = cond {
            let saved = std::mem::replace(&mut self.narrowed_types, typed);
            self.let_chain(parts, then, else_, mode);
            self.narrowed_types = saved;
            return;
        }
        self.e.w("if ");
        self.push_scope();
        self.cond(cond);
        for (name, t) in typed {
            self.bind_name(&name, Some(t));
        }
        self.e.w(" ");
        match mode {
            Some(t) => self.block_value(then, t),
            None => self.block_stmt(then),
        }
        self.pop_scope();
        if let Some(el) = else_ {
            self.e.w(" else ");
            match &el.kind {
                // a let chain lowers to `match`, which needs braces after `else`
                ExprKind::If { cond, then, else_ } if matches!(**cond, Cond::Chain(_)) || matches!(self.narrow_cond(cond, then).0, Some(Cond::Chain(_))) => {
                    self.e.w("{");
                    self.e.indent();
                    self.e.nl();
                    self.if_expr(cond, then, else_.as_deref(), mode);
                    self.e.dedent();
                    self.e.nl();
                    self.e.w("}");
                }
                ExprKind::If { cond, then, else_ } => self.if_expr(cond, then, else_.as_deref(), mode),
                ExprKind::Block(b) => match mode {
                    Some(t) => self.block_value(b, t),
                    None => self.block_stmt(b),
                },
                _ => {
                    self.e.w("{");
                    self.e.indent();
                    self.e.nl();
                    self.e.mark(el.span);
                    match mode {
                        Some(t) => self.tail_moving_args(el, t),
                        // as a statement, so a throwing call passes its error on
                        None => self.stmt(&Stmt::Expr { expr: el.clone(), semi: false }),
                    }
                    self.e.dedent();
                    self.e.nl();
                    self.e.w("}");
                }
            }
        }
    }

    /// `when (val x = e) { ... }` -> `{ let x = e; match x { ... } }`
    fn when_with_binding(&mut self, subject: Option<&Expr>, binding: Option<&Pat>, arms: &[WhenArm], rust_match: bool, mode: Option<&Target>, span: Span) {
        let Some(b) = binding else {
            self.when_expr(subject, arms, rust_match, mode, span);
            return;
        };
        let p = self.pat(b);
        let ty = subject.and_then(|s| self.infer_type(s));
        self.e.w("{");
        self.e.indent();
        self.e.nl();
        self.e.w(&format!("let {} = ", p));
        if let Some(s) = subject {
            self.expr(s, P_LOWEST);
        }
        self.e.w(";");
        self.e.nl();
        self.push_scope();
        self.bind_pat(b, ty);
        let var = Expr::new(ExprKind::Path { segs: vec![match b { Pat::Ident { name, .. } => name.clone(), _ => "_".into() }], generics: None, global: false }, span);
        self.when_expr(Some(&var), arms, rust_match, mode, span);
        self.pop_scope();
        self.e.dedent();
        self.e.nl();
        self.e.w("}");
    }

    /// A `when` on a `String` (`Some(false)`) or `&String` (`Some(true)`) with a string literal arm.
    fn text_when(&self, subject: &Expr, arms: &[WhenArm]) -> Option<bool> {
        fn text(p: &Pat) -> bool {
            match p {
                Pat::Lit(e) => matches!(e.kind, ExprKind::Str(_) | ExprKind::RawStr(_)),
                Pat::Or(ps) => ps.iter().any(text),
                _ => false,
            }
        }
        if !arms.iter().any(|a| a.conds.iter().any(|c| matches!(c, WhenCond::Pat(p) if text(p)))) {
            return None;
        }
        let is_string = |t: &Type| t.single_name() == Some("String");
        match self.infer_type(subject)? {
            Type::Ref { inner, .. } => is_string(&inner).then_some(true),
            t => is_string(&t).then_some(false),
        }
    }

    fn when_expr(&mut self, subject: Option<&Expr>, arms: &[WhenArm], rust_match: bool, mode: Option<&Target>, span: Span) {
        // `in 1..=9` with literal bounds is just a range pattern
        let arms: Vec<WhenArm> = arms
            .iter()
            .map(|a| {
                let mut a = a.clone();
                for c in a.conds.iter_mut() {
                    if let WhenCond::In { expr, negated: false } = c {
                        if let ExprKind::Range { start: Some(lo), end: Some(hi), inclusive } = &strip_paren(expr).kind {
                            if is_literal(lo) && is_literal(hi) {
                                *c = WhenCond::Pat(Pat::Range { lo: Some(lo.clone()), hi: Some(hi.clone()), inclusive: *inclusive });
                            }
                        }
                    }
                }
                a
            })
            .collect();
        let arms = &arms[..];
        match subject {
            Some(s) => {
                self.e.w("match ");
                // text arms on a `String` compare it as `&str`, the type of the literals: `&*s`
                // (which a `&str` the inference took for a `String` gives too), `s.as_str()` for
                // a `&String`
                match self.text_when(s, arms) {
                    Some(true) => {
                        self.expr(s, P_POSTFIX);
                        self.e.w(".as_str()");
                        self.e.decide(s.span, ".as_str()", "the arms compare text", None);
                    }
                    Some(false) => {
                        self.e.w("&*");
                        self.expr(s, P_UNARY);
                        self.e.decide(s.span, "&*", "the arms compare text", None);
                    }
                    None => self.expr(s, P_LOWEST),
                }
                self.e.w(" {");
                self.e.indent();
                let mut has_catch_all = false;
                let mut has_literal = false;
                for arm in arms {
                    self.e.nl();
                    self.e.mark(arm.span);
                    for a in &arm.attrs {
                        self.e.w(&format!("#[{}]", a));
                        self.e.nl();
                    }
                    self.push_scope();
                    let has_in = arm.conds.iter().any(|c| matches!(c, WhenCond::In { .. }));
                    if has_in {
                        has_literal = true;
                        let w = self.fresh("w");
                        let mut checks = Vec::new();
                        for c in &arm.conds {
                            match c {
                                WhenCond::In { .. } => checks.push(c.clone()),
                                WhenCond::Pat(_) => checks.push(c.clone()),
                                _ => {}
                            }
                        }
                        self.e.w(&format!("ref {} if ", w));
                        for (i, c) in checks.iter().enumerate() {
                            if i > 0 {
                                self.e.w(" || ");
                            }
                            match c {
                                WhenCond::In { expr, negated } => {
                                    if let ExprKind::Range { start, end, inclusive } = &strip_paren(expr).kind {
                                        let item = Expr::new(ExprKind::Unary(UnOp::Deref, Box::new(Expr::new(ExprKind::Path { segs: vec![w.clone()], generics: None, global: false }, expr.span))), expr.span);
                                        self.push_scope();
                                        self.bind_name(&w, None);
                                        self.in_range(&item, start.as_deref(), end.as_deref(), *inclusive, *negated);
                                        self.pop_scope();
                                    } else {
                                        if *negated {
                                            self.e.w("!");
                                        }
                                        self.expr(expr, P_POSTFIX);
                                        // `w` is a reference to the subject
                                        match self.contains_kind(expr) {
                                            Contains::Key => self.e.w(&format!(".contains_key({})", w)),
                                            Contains::Text if subject.is_some_and(|s| self.is_char(s)) => self.e.w(&format!(".contains(*{})", w)),
                                            _ => self.e.w(&format!(".contains({})", w)),
                                        }
                                    }
                                }
                                WhenCond::Pat(p) => {
                                    let ps = self.pat(p);
                                    self.e.w(&format!("matches!({}, {})", w, ps));
                                }
                                _ => {}
                            }
                        }
                        if let Some(g) = &arm.guard {
                            self.e.w(" && (");
                            self.expr(g, P_LOWEST);
                            self.e.w(")");
                        }
                    } else {
                        let pats: Vec<String> = arm
                            .conds
                            .iter()
                            .map(|c| match c {
                                WhenCond::Pat(p) => {
                                    if matches!(p, Pat::Lit(_) | Pat::Range { .. }) {
                                        has_literal = true;
                                    }
                                    if arm.guard.is_none() && matches!(p, Pat::Wild | Pat::Ident { sub: None, .. }) {
                                        has_catch_all = true;
                                    }
                                    self.pat(p)
                                }
                                WhenCond::Else => {
                                    has_catch_all = arm.guard.is_none() || has_catch_all;
                                    "_".into()
                                }
                                WhenCond::Expr(e) => {
                                    self.error(e.span, "expected a pattern");
                                    "_".into()
                                }
                                WhenCond::In { .. } => unreachable!(),
                            })
                            .collect();
                        self.e.w(&pats.join(" | "));
                        for c in &arm.conds {
                            if let WhenCond::Pat(p) = c {
                                self.bind_pat(p, None);
                            }
                        }
                        if let Some(g) = &arm.guard {
                            self.e.w(" if ");
                            self.expr(g, P_LOWEST);
                        }
                    }
                    self.e.w(" => ");
                    self.arm_body(&arm.body, mode);
                    self.pop_scope();
                }
                if mode.is_none() && !rust_match && has_literal && !has_catch_all {
                    self.e.nl();
                    self.e.w("_ => {}");
                }
                self.e.dedent();
                self.e.nl();
                self.e.w("}");
            }
            None => {
                // subject-less when: if / else if chain
                let mut first = true;
                let mut has_else = false;
                for arm in arms {
                    let is_else = arm.conds.iter().any(|c| matches!(c, WhenCond::Else));
                    if is_else {
                        has_else = true;
                        if first {
                            self.e.w("{");
                        } else {
                            self.e.w(" else {");
                        }
                    } else {
                        self.e.w(if first { "if " } else { " else if " });
                        for (i, c) in arm.conds.iter().enumerate() {
                            if i > 0 {
                                self.e.w(" || ");
                            }
                            match c {
                                WhenCond::Expr(e) => self.expr(e, if arm.conds.len() > 1 { 4 } else { P_LOWEST }),
                                WhenCond::Pat(p) => {
                                    let s = self.pat(p);
                                    self.e.w(&s);
                                }
                                _ => {}
                            }
                        }
                        self.e.w(" {");
                    }
                    first = false;
                    self.e.indent();
                    self.push_scope();
                    match &arm.body.kind {
                        ExprKind::Block(b) => match mode {
                            Some(t) => {
                                self.stmts_with_tail(&b.stmts, Some(t));
                            }
                            None => self.stmts_with_tail(&b.stmts, None),
                        },
                        _ => {
                            self.e.nl();
                            self.e.mark(arm.body.span);
                            match mode {
                                Some(t) => self.tail_moving_args(&arm.body, t),
                                // as a statement, so a throwing call passes its error on
                                None => self.stmt(&Stmt::Expr { expr: arm.body.clone(), semi: false }),
                            }
                        }
                    }
                    self.pop_scope();
                    self.e.dedent();
                    self.e.nl();
                    self.e.w("}");
                    if is_else {
                        break;
                    }
                }
                if !has_else && mode.is_some() {
                    self.e.w(" else {\n    unreachable!(\"no `when` branch matched\")\n}");
                }
                let _ = span;
            }
        }
    }

    fn arm_body(&mut self, body: &Expr, mode: Option<&Target>) {
        match (&body.kind, mode) {
            (ExprKind::Block(b), Some(t)) => self.block_value(b, t),
            (ExprKind::Block(b), None) => self.block_stmt(b),
            // in a block, so the calls that change `this` can go first (`move_args_changing_this`)
            (_, Some(t)) if self.args_changing_this(body).is_some() => {
                self.block_value(&Block { stmts: vec![Stmt::Expr { expr: body.clone(), semi: false }], span: body.span }, t);
                self.e.w(",");
            }
            (_, Some(t)) => {
                self.tail(body, t);
                self.e.w(",");
            }
            (_, None) => {
                if body.diverges() {
                    self.expr(body, P_LOWEST);
                    self.e.w(",");
                } else if body.is_block_like() {
                    self.stmt_expr(body);
                    self.e.w(",");
                } else {
                    self.e.w("{");
                    self.e.indent();
                    self.e.nl();
                    // as a statement, so a throwing call passes its error on
                    self.stmt(&Stmt::Expr { expr: body.clone(), semi: true });
                    self.e.dedent();
                    self.e.nl();
                    self.e.w("}");
                }
            }
        }
    }

    fn try_catch(&mut self, body: &Block, catch: Option<&Catch>, mode: Option<&Target>) {
        let label = self.fresh("try");
        let err_ty = match catch.and_then(|c| c.ty.as_ref()) {
            Some(t) => self.ty(t, TyPos::Generic),
            None => format!("{}::AnyError", RT),
        };
        let wrap = format!("Ok::<_, {}>", err_ty);
        let emit_block = |l: &mut Self, value: bool| {
            l.e.w(&format!("'{}: ", label));
            if let Some(f) = l.fns.last_mut() {
                f.tries.push(label.clone());
            }
            let t = Target { ty: None, wrap: Some(wrap.clone()), fn_result: false };
            if value {
                l.block_value(body, &t);
            } else {
                l.e.w("{");
                l.e.indent();
                l.push_scope();
                l.stmts_with_tail(&body.stmts, None);
                if !ends_diverging(&body.stmts) {
                    l.e.nl();
                    l.e.w(&format!("{}(())", wrap));
                }
                l.pop_scope();
                l.e.dedent();
                l.e.nl();
                l.e.w("}");
            }
            if let Some(f) = l.fns.last_mut() {
                f.tries.pop();
            }
        };
        match catch {
            None => {
                if mode.is_none() {
                    self.e.w("let _ = ");
                    emit_block(self, false);
                    self.e.w(";");
                } else {
                    emit_block(self, true);
                }
            }
            Some(c) => {
                self.e.w("match ");
                emit_block(self, mode.is_some());
                self.e.w(" {");
                self.e.indent();
                self.e.nl();
                let v = self.fresh("v");
                if mode.is_some() {
                    self.e.w(&format!("Ok({v}) => {v},"));
                } else {
                    self.e.w("Ok(()) => {}");
                }
                self.e.nl();
                self.push_scope();
                let bp = self.pat(&c.binding);
                self.bind_pat(&c.binding, c.ty.clone());
                self.e.w(&format!("Err({}) => ", bp));
                match mode {
                    Some(t) => self.block_value(&c.body, t),
                    None => self.block_stmt(&c.body),
                }
                self.pop_scope();
                self.e.dedent();
                self.e.nl();
                self.e.w("}");
            }
        }
    }

    // ------------------------------------------------------------------ lambdas & literals

    /// An untyped lambda parameter, which may be a reference to the value (`filter { it > 2 }`).
    fn is_ref_lambda_param(&self, e: &Expr) -> bool {
        e.ident().is_some_and(|n| self.is_untyped_lambda_param(n))
    }

    /// Reads a scalar through any number of references: `crate::__oxie::val(it)`.
    fn read_value(&mut self, e: &Expr) {
        self.e.mark(e.span);
        self.e.w("crate::__oxie::val(");
        self.expr(e, P_LOWEST);
        self.e.w(")");
    }

    pub(crate) fn lambda(&mut self, e: &Expr, param_tys: Option<&[Type]>, ret_ty: Option<&Type>) {
        let ExprKind::Lambda { params, ret, body, is_move, is_async, implicit_it: braces, .. } = &e.kind else {
            self.expr(e, P_LOWEST);
            return;
        };
        let implicit_it = *braces && params.is_empty() && visit::uses_it(body);
        let typed_by_body = std::mem::take(&mut self.typed_by_body);
        if *is_async {
            self.e.w("async ");
        }
        if *is_move {
            self.e.w("move ");
        }
        // predicates of `filter`, `find`, ... receive references to the elements
        let ref_params = std::mem::take(&mut self.next_lambda_refs);
        self.push_scope();
        let depth = self.scopes.len() - 1;
        let mut ps = Vec::new();
        if implicit_it {
            let t = param_tys.and_then(|p| p.first()).map(|t| format!(": {}", self.ty(t, TyPos::Local))).unwrap_or_default();
            ps.push(format!("it{}", t));
            let ty = param_tys.and_then(|p| p.first()).cloned();
            if ty.is_none() && ref_params {
                self.lambda_params.push((depth, "it".into()));
            }
            if ty.is_none() && typed_by_body {
                self.untyped_params.push((depth, "it".into()));
            }
            self.bind_name("it", ty);
        }
        for (i, p) in params.iter().enumerate() {
            let pt = self.pat(&p.pat);
            let ty = p.ty.clone().or_else(|| param_tys.and_then(|v| v.get(i)).cloned());
            match &ty {
                Some(t) => {
                    let tt = self.ty(t, TyPos::Local);
                    ps.push(format!("{}: {}", pt, tt));
                }
                None => {
                    ps.push(pt);
                    if let (true, Pat::Ident { name, sub: None, .. }) = (typed_by_body, &p.pat) {
                        self.untyped_params.push((depth, name.clone()));
                    }
                    if let (true, Pat::Ident { name, sub: None, .. }) = (ref_params, &p.pat) {
                        self.lambda_params.push((depth, name.clone()));
                    }
                }
            }
            self.bind_pat(&p.pat, ty);
        }
        self.e.w(&format!("|{}|", ps.join(", ")));
        let ret = ret.clone().or_else(|| ret_ty.cloned().filter(|t| !t.is_unit()));
        let explicit_ret = matches!(&e.kind, ExprKind::Lambda { ret: Some(_), .. });
        self.fns.push(FnCtx { throws: false, ret: ret.clone(), is_lambda: true, tries: Vec::new(), text_break: None, unboxed_ret: false });
        let saved_loops = std::mem::take(&mut self.loops);
        let target = Target { ty: ret.clone(), wrap: None, fn_result: false };
        // `{ items.push(next_id()) }`: in a block, so the calls that change `this` can go first
        let moving = !matches!(body.kind, ExprKind::Block(_)) && self.args_changing_this(body).is_some();
        let as_block = Block { stmts: vec![Stmt::Expr { expr: (**body).clone(), semi: false }], span: body.span };
        if explicit_ret {
            let rt = self.ty(ret.as_ref().unwrap(), TyPos::Ret);
            self.e.w(&format!(" -> {} ", rt));
            match &body.kind {
                ExprKind::Block(b) => self.block_value(b, &target),
                _ if moving => self.block_value(&as_block, &target),
                _ => {
                    self.e.w("{ ");
                    self.tail(body, &target);
                    self.e.w(" }");
                }
            }
        } else {
            self.e.w(" ");
            match &body.kind {
                ExprKind::Block(b) => self.block_value(b, &target),
                _ if moving => self.block_value(&as_block, &target),
                _ => {
                    if ret.is_some() {
                        self.tail(body, &target);
                    } else {
                        self.expr(body, P_LOWEST);
                    }
                }
            }
        }
        self.loops = saved_loops;
        self.fns.pop();
        self.lambda_params.retain(|(d, _)| *d != depth);
        self.untyped_params.retain(|(d, _)| *d != depth);
        self.pop_scope();
    }

    fn struct_lit(&mut self, path: &[String], generics: Option<&Vec<GenericArg>>, fields: &[(String, Option<Expr>)], base: Option<&Expr>) {
        let owner = if path.len() == 1 && path[0] == "Self" { self.class.as_ref().map(|c| c.name.clone()) } else { path.last().cloned() };
        let (p, shape) = self.resolve_variant_path(path);
        let field_types: Vec<(String, Type)> = match shape {
            Some(VariantShape::Named(f)) => f,
            _ => owner
                .as_ref()
                .and_then(|o| self.class_info(o))
                .map(|c| c.fields.iter().map(|f| (f.name.clone(), f.ty.clone())).collect())
                .unwrap_or_default(),
        };
        let mut text = p;
        if let Some(g) = generics {
            text.push_str("::");
            text.push_str(&self.generic_args_text(g));
        }
        self.e.w(&format!("{} {{ ", text));
        for (i, (n, v)) in fields.iter().enumerate() {
            if i > 0 {
                self.e.w(", ");
            }
            let fty = field_types.iter().find(|(fname, _)| fname == n).map(|(_, t)| t.clone());
            match v {
                Some(v) => {
                    self.e.w(&format!("{}: ", esc(n)));
                    match fty {
                        Some(t) => self.coerce(v, &t, TyPos::Field),
                        None => self.expr(v, P_LOWEST),
                    }
                }
                None => match self.implicit_field(n) {
                    Some(f) => self.e.w(&format!("{}: {}", esc(n), f)),
                    None => self.e.w(&esc(n)),
                },
            }
        }
        if let Some(b) = base {
            if !fields.is_empty() {
                self.e.w(", ");
            }
            self.e.w("..");
            self.expr(b, P_LOWEST);
        }
        self.e.w(" }");
    }
}

fn type_mentions(t: &Type, names: &[String]) -> bool {
    if names.is_empty() {
        return false;
    }
    let text = crate::symbols::type_text(t);
    text.split(|c: char| !(c.is_alphanumeric() || c == '_')).any(|w| names.iter().any(|n| n == w))
}

fn strip_paren(e: &Expr) -> &Expr {
    match &e.kind {
        ExprKind::Paren(x) => strip_paren(x),
        _ => e,
    }
}

fn member(name: &str) -> String {
    if name.chars().all(|c| c.is_ascii_digit()) {
        name.to_string()
    } else {
        esc(name)
    }
}

fn contains_struct_lit(e: &Expr) -> bool {
    let mut found = false;
    visit::walk_expr(e, &mut |x| {
        match &x.kind {
            ExprKind::StructLit { .. } => found = true,
            ExprKind::Call { .. } | ExprKind::MethodCall { .. } | ExprKind::Lambda { .. } | ExprKind::Paren(_) | ExprKind::Index(..) => {
                return matches!(x.kind, ExprKind::MethodCall { .. }) && !found;
            }
            _ => {}
        }
        !found
    });
    found
}

/// The operands of an `&&` chain, left to right.
fn and_operands<'e>(e: &'e Expr, out: &mut Vec<&'e Expr>) {
    match &e.kind {
        ExprKind::Binary(BinOp::And, l, r) => {
            and_operands(l, out);
            and_operands(r, out);
        }
        _ => out.push(e),
    }
}

fn concat_operands<'e>(e: &'e Expr, out: &mut Vec<&'e Expr>) {
    match &e.kind {
        ExprKind::Binary(BinOp::Add, l, r) => {
            concat_operands(l, out);
            out.push(r);
        }
        _ => out.push(e),
    }
}

/// Left spine of a `+` chain: operands, and the Add nodes covering operands[0..=k+1].
fn spine<'e>(e: &'e Expr, ops: &mut Vec<&'e Expr>, nodes: &mut Vec<&'e Expr>) {
    match &e.kind {
        ExprKind::Binary(BinOp::Add, l, r) => {
            spine(l, ops, nodes);
            ops.push(r);
            nodes.push(e);
        }
        _ => ops.push(e),
    }
}

/// The names an import brings in, with the paths they stand for (`Config` for
/// `nucleo_matcher::{Config, Matcher}`).
fn imported_names(tree: &UseTree, prefix: &[String], out: &mut Vec<(String, Vec<String>)>) {
    match tree {
        UseTree::Path { segments, alias } => {
            let path: Vec<String> = prefix.iter().chain(segments).cloned().collect();
            if let Some(last) = path.last().filter(|l| *l != "self") {
                out.push((alias.clone().unwrap_or_else(|| last.clone()), path.clone()));
            }
        }
        UseTree::Group { segments, items } => {
            let prefix: Vec<String> = prefix.iter().chain(segments).cloned().collect();
            for i in items {
                imported_names(i, &prefix, out);
            }
        }
        // a glob is kept as `*` with the path it imports from
        UseTree::Glob { segments } => out.push(("*".into(), prefix.iter().chain(segments).cloned().collect())),
    }
}
