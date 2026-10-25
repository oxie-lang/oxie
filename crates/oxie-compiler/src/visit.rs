//! Generic traversal over expressions.

use crate::ast::*;

/// Calls `f` on every expression in pre-order. If `f` returns false, children are skipped.
pub fn walk_expr(e: &Expr, f: &mut dyn FnMut(&Expr) -> bool) {
    if !f(e) {
        return;
    }
    use ExprKind::*;
    match &e.kind {
        Template(parts) => {
            for p in parts {
                if let TemplatePart::Expr(x, _) = p {
                    walk_expr(x, f);
                }
            }
        }
        Unary(_, x) | Try(x) | Unwrap(x) | Await(x) | Cast(x, _) | Paren(x) | Throw(x) => walk_expr(x, f),
        Field { recv, .. } => walk_expr(recv, f),
        Binary(_, a, b) | Assign(_, a, b) | Elvis(a, b) | Pair(a, b) | Index(a, b) | ArrayRepeat(a, b) => {
            walk_expr(a, f);
            walk_expr(b, f);
        }
        In { item, coll, .. } => {
            walk_expr(item, f);
            walk_expr(coll, f);
        }
        Is { expr, .. } => walk_expr(expr, f),
        Call { callee, args } => {
            walk_expr(callee, f);
            for a in args {
                walk_expr(&a.value, f);
            }
        }
        MethodCall { recv, args, .. } => {
            walk_expr(recv, f);
            for a in args {
                walk_expr(&a.value, f);
            }
        }
        If { cond, then, else_ } => {
            walk_cond(cond, f);
            walk_block(then, f);
            if let Some(e) = else_ {
                walk_expr(e, f);
            }
        }
        When { subject, arms, .. } => {
            if let Some(s) = subject {
                walk_expr(s, f);
            }
            for a in arms {
                for c in &a.conds {
                    match c {
                        WhenCond::In { expr, .. } | WhenCond::Expr(expr) => walk_expr(expr, f),
                        _ => {}
                    }
                }
                if let Some(g) = &a.guard {
                    walk_expr(g, f);
                }
                walk_expr(&a.body, f);
            }
        }
        Block(b) | Unsafe(b) | ConstBlock(b) | LabeledBlock { block: b, .. } | Loop { body: b, .. } | Async { block: b, .. } => walk_block(b, f),
        While { cond, body, .. } => {
            walk_cond(cond, f);
            walk_block(body, f);
        }
        DoWhile { body, cond, .. } => {
            walk_block(body, f);
            walk_expr(cond, f);
        }
        For { iter, body, .. } => {
            walk_expr(iter, f);
            walk_block(body, f);
        }
        Lambda { body, .. } => walk_expr(body, f),
        Return(Some(x)) => walk_expr(x, f),
        Break { value: Some(x), .. } => walk_expr(x, f),
        Tuple(v) | Array(v) | VecLit(v) | MapLit(v) | SetLit(v) => {
            for x in v {
                walk_expr(x, f);
            }
        }
        Range { start, end, .. } => {
            if let Some(x) = start {
                walk_expr(x, f);
            }
            if let Some(x) = end {
                walk_expr(x, f);
            }
        }
        StructLit { fields, base, .. } => {
            for (_, v) in fields {
                if let Some(v) = v {
                    walk_expr(v, f);
                }
            }
            if let Some(b) = base {
                walk_expr(b, f);
            }
        }
        TryCatch { body, catch } => {
            walk_block(body, f);
            if let Some(c) = catch {
                walk_block(&c.body, f);
            }
        }
        _ => {}
    }
}

pub fn walk_cond(c: &Cond, f: &mut dyn FnMut(&Expr) -> bool) {
    match c {
        Cond::Expr(e) => walk_expr(e, f),
        Cond::Let { expr, .. } => walk_expr(expr, f),
        Cond::Chain(parts) => parts.iter().for_each(|c| walk_cond(c, f)),
    }
}

pub fn walk_block(b: &Block, f: &mut dyn FnMut(&Expr) -> bool) {
    for s in &b.stmts {
        walk_stmt(s, f);
    }
}

pub fn walk_stmt(s: &Stmt, f: &mut dyn FnMut(&Expr) -> bool) {
    match s {
        Stmt::Let { init, else_, .. } => {
            if let Some(e) = init {
                walk_expr(e, f);
            }
            if let Some(b) = else_ {
                walk_block(b, f);
            }
        }
        Stmt::Expr { expr, .. } => walk_expr(expr, f),
        Stmt::Attributed { stmt, .. } => walk_stmt(stmt, f),
        Stmt::Item(_) => {}
    }
}

/// `walk_expr` that can change the expressions: calls `f` on every expression in pre-order, and
/// skips the children when it returns false (after replacing the expression, say).
pub fn walk_expr_mut(e: &mut Expr, f: &mut dyn FnMut(&mut Expr) -> bool) {
    if !f(e) {
        return;
    }
    use ExprKind::*;
    match &mut e.kind {
        Template(parts) => {
            for p in parts {
                if let TemplatePart::Expr(x, _) = p {
                    walk_expr_mut(x, f);
                }
            }
        }
        Unary(_, x) | Try(x) | Unwrap(x) | Await(x) | Cast(x, _) | Paren(x) | Throw(x) => walk_expr_mut(x, f),
        Field { recv, .. } => walk_expr_mut(recv, f),
        Binary(_, a, b) | Assign(_, a, b) | Elvis(a, b) | Pair(a, b) | Index(a, b) | ArrayRepeat(a, b) => {
            walk_expr_mut(a, f);
            walk_expr_mut(b, f);
        }
        In { item, coll, .. } => {
            walk_expr_mut(item, f);
            walk_expr_mut(coll, f);
        }
        Is { expr, .. } => walk_expr_mut(expr, f),
        Call { callee, args } => {
            walk_expr_mut(callee, f);
            for a in args {
                walk_expr_mut(&mut a.value, f);
            }
        }
        MethodCall { recv, args, .. } => {
            walk_expr_mut(recv, f);
            for a in args {
                walk_expr_mut(&mut a.value, f);
            }
        }
        If { cond, then, else_ } => {
            walk_cond_mut(cond, f);
            walk_block_mut(then, f);
            if let Some(e) = else_ {
                walk_expr_mut(e, f);
            }
        }
        When { subject, arms, .. } => {
            if let Some(s) = subject {
                walk_expr_mut(s, f);
            }
            for a in arms {
                for c in &mut a.conds {
                    match c {
                        WhenCond::In { expr, .. } | WhenCond::Expr(expr) => walk_expr_mut(expr, f),
                        _ => {}
                    }
                }
                if let Some(g) = &mut a.guard {
                    walk_expr_mut(g, f);
                }
                walk_expr_mut(&mut a.body, f);
            }
        }
        Block(b) | Unsafe(b) | ConstBlock(b) | LabeledBlock { block: b, .. } | Loop { body: b, .. } | Async { block: b, .. } => walk_block_mut(b, f),
        While { cond, body, .. } => {
            walk_cond_mut(cond, f);
            walk_block_mut(body, f);
        }
        DoWhile { body, cond, .. } => {
            walk_block_mut(body, f);
            walk_expr_mut(cond, f);
        }
        For { iter, body, .. } => {
            walk_expr_mut(iter, f);
            walk_block_mut(body, f);
        }
        Lambda { body, .. } => walk_expr_mut(body, f),
        Return(Some(x)) => walk_expr_mut(x, f),
        Break { value: Some(x), .. } => walk_expr_mut(x, f),
        Tuple(v) | Array(v) | VecLit(v) | MapLit(v) | SetLit(v) => {
            for x in v {
                walk_expr_mut(x, f);
            }
        }
        Range { start, end, .. } => {
            if let Some(x) = start {
                walk_expr_mut(x, f);
            }
            if let Some(x) = end {
                walk_expr_mut(x, f);
            }
        }
        StructLit { fields, base, .. } => {
            for (_, v) in fields {
                if let Some(v) = v {
                    walk_expr_mut(v, f);
                }
            }
            if let Some(b) = base {
                walk_expr_mut(b, f);
            }
        }
        TryCatch { body, catch } => {
            walk_block_mut(body, f);
            if let Some(c) = catch {
                walk_block_mut(&mut c.body, f);
            }
        }
        _ => {}
    }
}

pub fn walk_cond_mut(c: &mut Cond, f: &mut dyn FnMut(&mut Expr) -> bool) {
    match c {
        Cond::Expr(e) => walk_expr_mut(e, f),
        Cond::Let { expr, .. } => walk_expr_mut(expr, f),
        Cond::Chain(parts) => parts.iter_mut().for_each(|c| walk_cond_mut(c, f)),
    }
}

pub fn walk_block_mut(b: &mut Block, f: &mut dyn FnMut(&mut Expr) -> bool) {
    for s in &mut b.stmts {
        walk_stmt_mut(s, f);
    }
}

pub fn walk_stmt_mut(s: &mut Stmt, f: &mut dyn FnMut(&mut Expr) -> bool) {
    match s {
        Stmt::Let { init, else_, .. } => {
            if let Some(e) = init {
                walk_expr_mut(e, f);
            }
            if let Some(b) = else_ {
                walk_block_mut(b, f);
            }
        }
        Stmt::Expr { expr, .. } => walk_expr_mut(expr, f),
        Stmt::Attributed { stmt, .. } => walk_stmt_mut(stmt, f),
        Stmt::Item(_) => {}
    }
}

/// Calls `f` on every expression of the items: function and method bodies, default values,
/// `init` blocks, enum discriminants and messages, tests and constants (pre-order, like
/// `walk_expr`).
pub fn walk_items(items: &[Item], f: &mut dyn FnMut(&Expr) -> bool) {
    for it in items {
        match &it.kind {
            ItemKind::Fun(d) => {
                for p in &d.params {
                    if let Some(x) = &p.default {
                        walk_expr(x, f);
                    }
                }
                match &d.body {
                    Some(FunBody::Block(b)) => walk_block(b, f),
                    Some(FunBody::Expr(e)) => walk_expr(e, f),
                    None => {}
                }
            }
            ItemKind::Type(t) => {
                let variants = t.variants.iter().flat_map(|v| v.discriminant.iter().chain(v.message.iter()));
                for x in t.fields.iter().filter_map(|fd| fd.default.as_ref()).chain(t.message.as_ref()).chain(variants) {
                    walk_expr(x, f);
                }
                if let Some(b) = &t.init {
                    walk_block(b, f);
                }
                walk_items(&t.members, f);
            }
            ItemKind::Trait(t) => walk_items(&t.members, f),
            ItemKind::Impl(i) => walk_items(&i.members, f),
            ItemKind::Mod { items, .. } | ItemKind::Object { items, .. } => walk_items(items, f),
            ItemKind::Test(t) => walk_block(&t.body, f),
            ItemKind::Const(c) | ItemKind::Static(c) => {
                if let Some(e) = &c.value {
                    walk_expr(e, f);
                }
            }
            _ => {}
        }
    }
}

/// Does `body` reference the implicit lambda parameter `it`?
pub fn uses_it(body: &Expr) -> bool {
    let mut found = false;
    walk_expr(body, &mut |e| {
        if found {
            return false;
        }
        match &e.kind {
            ExprKind::Path { segs, generics: None, global: false } if segs.len() == 1 && segs[0] == "it" => {
                found = true;
                false
            }
            // nested brace lambdas without params have their own `it`
            ExprKind::Lambda { params, implicit_it: true, .. } if params.is_empty() => false,
            ExprKind::Lambda { params, .. } => !params.iter().any(|p| matches!(&p.pat, Pat::Ident { name, .. } if name == "it")),
            ExprKind::Template(parts) => {
                for p in parts {
                    if let TemplatePart::Expr(x, _) = p {
                        if uses_it(x) {
                            found = true;
                        }
                    }
                }
                false
            }
            ExprKind::Macro { text, .. } | ExprKind::Raw(text) => {
                if crate::sema::contains_word(text, "it") {
                    found = true;
                }
                false
            }
            _ => true,
        }
    });
    found
}

/// Does the block contain an unlabeled `continue` that targets the enclosing loop?
pub fn has_own_continue(b: &Block) -> bool {
    let mut found = false;
    walk_block(b, &mut |e| match &e.kind {
        ExprKind::Continue(None) => {
            found = true;
            false
        }
        ExprKind::Loop { .. } | ExprKind::While { .. } | ExprKind::For { .. } | ExprKind::DoWhile { .. } | ExprKind::Lambda { .. } => false,
        _ => !found,
    });
    found
}

/// Does the function body call the parameter `name` directly (`name(...)`)?
pub fn calls_ident(b: &FunBody, name: &str) -> bool {
    let mut found = false;
    let mut f = |e: &Expr| {
        if let ExprKind::Call { callee, .. } = &e.kind {
            if callee.ident() == Some(name) {
                found = true;
            }
        }
        !found
    };
    match b {
        FunBody::Block(b) => walk_block(b, &mut f),
        FunBody::Expr(e) => walk_expr(e, &mut f),
    }
    found
}

/// Does the function body store the parameter `name` somewhere it outlives the call: assign it
/// (`listener = f`), put it into a collection (`shapes.push(s)`), box it (`Box.new(f)`) or pass
/// it to a constructor (`Holder(s)`)?
pub fn stores_ident(b: &FunBody, name: &str) -> bool {
    let is_it = |e: &Expr| {
        let mut e = e;
        while let ExprKind::Paren(x) = &e.kind {
            e = x;
        }
        e.ident() == Some(name)
    };
    let mut found = false;
    let mut f = |e: &Expr| {
        found |= match &e.kind {
            ExprKind::Assign(None, _, v) => is_it(v),
            ExprKind::MethodCall { recv, name: m, args, .. } => {
                matches!(m.as_str(), "push" | "push_back" | "push_front" | "insert" | "add") && args.iter().any(|a| is_it(&a.value))
                    || m == "new" && recv.ident() == Some("Box") && args.iter().any(|a| is_it(&a.value))
            }
            ExprKind::Call { callee, args } => {
                let stores = match &callee.kind {
                    ExprKind::Path { segs, .. } => segs.last().is_some_and(|n| crate::parser::is_type_like(n) || n == "Some") || segs.len() == 2 && segs[0] == "Box",
                    _ => false,
                };
                stores && args.iter().any(|a| is_it(&a.value))
            }
            ExprKind::VecLit(items) | ExprKind::Array(items) => items.iter().any(is_it),
            _ => false,
        };
        !found
    };
    match b {
        FunBody::Block(b) => walk_block(b, &mut f),
        FunBody::Expr(e) => walk_expr(e, &mut f),
    }
    found
}

/// The values a function body returns: the leaves of its tail expression (through `if`, `when`
/// and blocks) and every `return x` outside lambdas.
pub fn return_values(b: &FunBody) -> Vec<Expr> {
    fn tail(e: &Expr, out: &mut Vec<Expr>) {
        match &e.kind {
            ExprKind::If { then, else_: Some(x), .. } => {
                block_tail(then, out);
                tail(x, out);
            }
            ExprKind::When { arms, .. } => arms.iter().for_each(|a| tail(&a.body, out)),
            ExprKind::Block(b) => block_tail(b, out),
            ExprKind::Paren(x) => tail(x, out),
            ExprKind::Return(_) => {}
            _ if e.diverges() => {}
            _ => out.push(e.clone()),
        }
    }
    fn block_tail(b: &Block, out: &mut Vec<Expr>) {
        if let Some(Stmt::Expr { expr, semi: false }) = b.stmts.last() {
            tail(expr, out);
        }
    }
    let mut out = Vec::new();
    let mut returns = |e: &Expr| {
        if let ExprKind::Return(Some(v)) = &e.kind {
            tail(v, &mut out);
        }
        !matches!(e.kind, ExprKind::Lambda { .. })
    };
    match b {
        FunBody::Block(b) => {
            walk_block(b, &mut returns);
            block_tail(b, &mut out);
        }
        FunBody::Expr(e) => {
            walk_expr(e, &mut returns);
            tail(e, &mut out);
        }
    }
    out
}

/// Does `body` change what the loop variable `name` refers to: assign to a place inside it
/// (`x.qty += 1`, `x[0] = 1`), take `&mut` of one, call a mutating method on it
/// (`x.tags.push(t)`, or one `changes` names, like an Oxie class's `&mut self` methods), or
/// change the elements in a loop over it?
pub fn writes_through(body: &Block, name: &str, changes: &dyn Fn(&str) -> bool) -> bool {
    fn root(e: &Expr) -> Option<&str> {
        match &e.kind {
            ExprKind::Field { recv, safe: false, .. } | ExprKind::Index(recv, _) | ExprKind::Paren(recv) | ExprKind::Unary(UnOp::Deref, recv) => root(recv),
            _ => e.ident(),
        }
    }
    let mut found = false;
    walk_block(body, &mut |e| {
        match &e.kind {
            ExprKind::Assign(_, place, _) if place.ident().is_none() && root(place) == Some(name) => found = true,
            ExprKind::MethodCall { recv, name: m, safe: false, .. } if crate::sema::MUTATING_METHODS.contains(&m.as_str()) && root(recv) == Some(name) => found = true,
            ExprKind::MethodCall { recv, name: m, safe: false, .. } if recv.ident() == Some(name) && changes(m) => found = true,
            ExprKind::Unary(UnOp::RefMut, place) if root(place) == Some(name) => found = true,
            // a loop over it that changes its elements (`for (x in row) *x += 1`)
            ExprKind::For { pat, iter, body, .. } if root(iter) == Some(name) => {
                let mut names = Vec::new();
                pat.bindings(&mut names);
                found = names.iter().any(|n| writes_through(body, n, &|_| false));
            }
            _ => {}
        }
        !found
    });
    found
}

/// The `this` of a method body used as a value (`listeners.push(this)`, `l.saw(this)`), not as
/// the receiver of a field, method or index: their span starts, and those of the receivers.
pub fn this_uses(b: &FunBody) -> (Vec<u32>, std::collections::HashSet<u32>) {
    let mut all = Vec::new();
    let mut recv = std::collections::HashSet::new();
    let mut f = |e: &Expr| {
        match &e.kind {
            ExprKind::SelfValue => all.push(e.span.start),
            ExprKind::Field { recv: r, .. } | ExprKind::MethodCall { recv: r, .. } | ExprKind::Index(r, _) if matches!(r.kind, ExprKind::SelfValue) => {
                recv.insert(r.span.start);
            }
            _ => {}
        }
        true
    };
    match b {
        FunBody::Block(b) => walk_block(b, &mut f),
        FunBody::Expr(e) => walk_expr(e, &mut f),
    }
    let values = all.into_iter().filter(|s| !recv.contains(s)).collect();
    (values, recv)
}

/// The names of the methods a body calls on `this`: `name(..)` and `this.name(..)`.
pub fn own_calls(b: &FunBody) -> Vec<String> {
    let mut out = Vec::new();
    let mut f = |e: &Expr| {
        match &e.kind {
            ExprKind::Call { callee, .. } => out.extend(callee.ident().map(str::to_string)),
            ExprKind::MethodCall { recv, name, .. } if matches!(recv.kind, ExprKind::SelfValue) => out.push(name.clone()),
            _ => {}
        }
        true
    };
    match b {
        FunBody::Block(b) => walk_block(b, &mut f),
        FunBody::Expr(e) => walk_expr(e, &mut f),
    }
    out
}

/// Identifiers referenced anywhere in an expression (for error message templates).
pub fn idents_in(e: &Expr, out: &mut Vec<String>) {
    walk_expr(e, &mut |x| {
        if let Some(n) = x.ident() {
            out.push(n.to_string());
        }
        if let ExprKind::Int(n) = &x.kind {
            out.push(format!("__{}", n));
        }
        true
    });
}

/// Do these statements mention the name `name`?
pub fn stmts_use(stmts: &[Stmt], name: &str) -> bool {
    let mut found = false;
    for s in stmts {
        walk_stmt(s, &mut |x| {
            found |= x.ident() == Some(name);
            !found
        });
    }
    found
}

/// Is `name` referenced inside any lambda within the function body?
pub fn ident_in_lambda(b: &FunBody, name: &str) -> bool {
    let mut found = false;
    let mut f = |e: &Expr| {
        if let ExprKind::Lambda { body, .. } = &e.kind {
            walk_expr(body, &mut |x| {
                if x.ident() == Some(name) {
                    found = true;
                }
                !found
            });
            return false;
        }
        !found
    };
    match b {
        FunBody::Block(b) => walk_block(b, &mut f),
        FunBody::Expr(e) => walk_expr(e, &mut f),
    }
    found
}

/// Does the expression contain an assignment (used to make closures `mut`)?
pub fn has_assignment(e: &Expr) -> bool {
    let mut found = false;
    walk_expr(e, &mut |x| {
        if matches!(x.kind, ExprKind::Assign(..)) {
            found = true;
        }
        !found
    });
    found
}

/// `for` loops over a local variable that is used again after the loop: later in the scope that
/// declares it, or by the next round of a loop that encloses the `for` but not the declaration.
/// Such loops borrow the collection instead of consuming it. Returns the span starts of their
/// iterables, each with the start of the first use after the loop (none when only an enclosing
/// loop uses it again). `outer` are the names already in scope (parameters).
pub fn reused_loop_iters(outer: Vec<String>, body: &Block) -> std::collections::HashMap<u32, Option<u32>> {
    struct Scope {
        names: Vec<String>,
        end: u32,
        is_loop: bool,
    }
    struct Walk {
        scopes: Vec<Scope>,
        uses: Vec<(String, u32)>,
        /// The field read at each use of a variable that is a field's receiver (`t` in `t.name`).
        fields: std::collections::HashMap<u32, String>,
        found: std::collections::HashMap<u32, Option<u32>>,
    }
    impl Walk {
        fn block(&mut self, b: &Block, is_loop: bool, names: Vec<String>) {
            self.scopes.push(Scope { names, end: b.span.end, is_loop });
            for s in &b.stmts {
                self.stmt(s);
            }
            self.scopes.pop();
        }
        fn stmt(&mut self, s: &Stmt) {
            match s {
                Stmt::Let { pat, init, else_, .. } => {
                    if let Some(e) = init {
                        self.expr(e);
                    }
                    if let Some(b) = else_ {
                        self.block(b, false, Vec::new());
                    }
                    let mut names = Vec::new();
                    pat.bindings(&mut names);
                    self.scopes.last_mut().unwrap().names.extend(names);
                }
                Stmt::Expr { expr, .. } => self.expr(expr),
                Stmt::Attributed { stmt, .. } => self.stmt(stmt),
                Stmt::Item(_) => {}
            }
        }
        /// Walks a condition and returns the names its `val` patterns bind.
        fn cond(&mut self, c: &Cond) -> Vec<String> {
            let mut names = Vec::new();
            match c {
                Cond::Expr(e) => self.expr(e),
                Cond::Let { pat, expr, .. } => {
                    self.expr(expr);
                    pat.bindings(&mut names);
                }
                Cond::Chain(parts) => parts.iter().for_each(|p| names.extend(self.cond(p))),
            }
            names
        }
        fn scoped(&mut self, names: Vec<String>, end: u32, f: impl FnOnce(&mut Self)) {
            self.scopes.push(Scope { names, end, is_loop: false });
            f(self);
            self.scopes.pop();
        }
        fn expr(&mut self, e: &Expr) {
            walk_expr(e, &mut |x| self.visit(x));
        }
        fn visit(&mut self, x: &Expr) -> bool {
            use ExprKind::*;
            match &x.kind {
                Block(b) | Unsafe(b) | ConstBlock(b) | LabeledBlock { block: b, .. } | Async { block: b, .. } => self.block(b, false, Vec::new()),
                Loop { body, .. } => self.block(body, true, Vec::new()),
                While { cond, body, .. } => {
                    let names = self.cond(cond);
                    self.block(body, true, names);
                }
                DoWhile { body, cond, .. } => {
                    self.block(body, true, Vec::new());
                    self.expr(cond);
                }
                For { pat, iter, body, .. } => {
                    self.expr(iter);
                    if let Some(n) = iter.ident() {
                        self.check(n, None, iter.span.start, x.span.end);
                    }
                    // a field of a variable (`t.members`): a later use of another field doesn't count
                    if let Field { recv, name, safe: false } = &iter.kind {
                        if let Some(n) = recv.ident() {
                            self.check(n, Some(name), iter.span.start, x.span.end);
                        }
                    }
                    let mut names = Vec::new();
                    pat.bindings(&mut names);
                    self.block(body, true, names);
                }
                Lambda { params, body, .. } => {
                    let mut names = Vec::new();
                    params.iter().for_each(|p| p.pat.bindings(&mut names));
                    self.scoped(names, x.span.end, |w| w.expr(body));
                }
                If { cond, then, else_ } => {
                    let names = self.cond(cond);
                    self.block(then, false, names);
                    if let Some(e) = else_ {
                        self.expr(e);
                    }
                }
                When { subject, binding, arms, .. } => {
                    if let Some(s) = subject {
                        self.expr(s);
                    }
                    let mut outer = Vec::new();
                    if let Some(p) = binding {
                        p.bindings(&mut outer);
                    }
                    self.scoped(outer, x.span.end, |w| {
                        for a in arms {
                            let mut names = Vec::new();
                            for c in &a.conds {
                                match c {
                                    WhenCond::Pat(p) => p.bindings(&mut names),
                                    WhenCond::In { expr, .. } | WhenCond::Expr(expr) => w.expr(expr),
                                    WhenCond::Else => {}
                                }
                            }
                            w.scoped(names, a.span.end, |w| {
                                if let Some(g) = &a.guard {
                                    w.expr(g);
                                }
                                w.expr(&a.body);
                            });
                        }
                    });
                }
                TryCatch { body, catch } => {
                    self.block(body, false, Vec::new());
                    if let Some(c) = catch {
                        let mut names = Vec::new();
                        c.binding.bindings(&mut names);
                        self.block(&c.body, false, names);
                    }
                }
                _ => return true,
            }
            false
        }
        /// A loop at `[_, end)` iterating over `name`: is the variable used again?
        fn check(&mut self, name: &str, field: Option<&str>, start: u32, end: u32) {
            let Some(d) = self.scopes.iter().rposition(|s| s.names.iter().any(|n| n == name)) else { return };
            let in_loop = self.scopes[d + 1..].iter().any(|s| s.is_loop);
            let scope_end = self.scopes[d].end;
            let other_field = |p: &u32| field.is_some_and(|f| self.fields.get(p).is_some_and(|g| g != f));
            let later = self.uses.iter().filter(|(n, p)| n == name && *p >= end && *p < scope_end && !other_field(p)).map(|(_, p)| *p).min();
            if in_loop || later.is_some() {
                self.found.insert(start, later);
            }
        }
    }
    let mut uses = Vec::new();
    let mut fields = std::collections::HashMap::new();
    walk_block(body, &mut |x| {
        if let Some(n) = x.ident() {
            uses.push((n.to_string(), x.span.start));
        }
        if let ExprKind::Field { recv, name, .. } = &x.kind {
            if recv.ident().is_some() {
                fields.insert(recv.span.start, name.clone());
            }
        }
        true
    });
    let mut w = Walk { scopes: vec![Scope { names: outer, end: u32::MAX, is_loop: false }], uses, fields, found: Default::default() };
    w.block(body, false, Vec::new());
    w.found
}
