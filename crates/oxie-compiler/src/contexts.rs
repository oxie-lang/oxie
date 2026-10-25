//! Context parameters. `context(cx: &mut App) fun refresh()` takes `cx` like a parameter that
//! each call fills with its own `cx`: the variable of that name where the call is (a parameter,
//! a local, a lambda's parameter, or the caller's own context). On a class, `context(...)` gives
//! each method the contexts it uses, directly or through the methods and functions it calls (a
//! fixpoint over the crate, as for receivers), so code that passes `window` and `cx` everywhere
//! (GPUI) declares them once. A method used as a value (`cx.listener(Self.save)`) takes all of
//! them, since its caller passes every argument; it names those it doesn't use `_window`.
//!
//! Runs on the parsed crate before symbols are collected: each context becomes a parameter whose
//! default is the variable of its name, so calls fill it in like a default argument (`fill_args`
//! reports a call where there is none), and a call that names it (`refresh(cx = other)`) passes
//! another value.

use std::collections::{HashMap, HashSet};

use crate::ast::*;
use crate::diag::Diag;

/// A function or method declared in the crate.
struct Fun<'a> {
    file: usize,
    module: &'a [String],
    /// The class it is a method of (in the class body or an `impl` block).
    class: Option<usize>,
    decl: &'a FunDecl,
    /// Implements an interface method or an operator: its parameters are the interface's.
    fixed: bool,
}

/// An Oxie class: its contexts, its methods (by name, to `Decls::funs`) and its fields.
struct Class<'a> {
    name: &'a str,
    /// Has type parameters: the lowering doesn't know what its constructor or methods give.
    generic: bool,
    file: usize,
    module: &'a [String],
    contexts: &'a [Param],
    methods: HashMap<&'a str, usize>,
    fields: HashMap<&'a str, &'a Type>,
}

#[derive(Default)]
struct Decls<'a> {
    funs: Vec<Fun<'a>>,
    classes: Vec<Class<'a>>,
    /// Top-level functions by name (several modules may declare one).
    free: HashMap<&'a str, Vec<usize>>,
    /// The method names of each interface declared in the crate.
    traits: HashMap<&'a str, HashSet<&'a str>>,
    diags: Vec<(usize, Diag)>,
}

/// What holds a function's declaration.
#[derive(Clone, Copy)]
enum Owner<'a> {
    /// a module: the function is called by its name
    Top,
    /// a class's body (with the interfaces it implements) or one of its `impl` blocks
    Class { class: usize, trait_impl: bool, supers: &'a [Type] },
    /// an enum, or an `impl` block of a type Oxie doesn't declare
    Other,
}

/// What a call in a body calls, as far as the call's own text tells.
enum Callee {
    /// `f(..)`: a method of the caller's class (implicit `this`), a constructor or a function
    Bare(String),
    /// `this.f(..)`
    OnThis(String),
    /// `x.f(..)` on a value of a known class (by index)
    On(usize, String),
    /// `Self.f(..)`, `Point.origin(..)`, `util.f(..)`
    Path(Vec<String>),
}

/// The contexts to append to each function's parameters, by (file, start of its name).
type Adds = HashMap<(usize, u32), Vec<Param>>;

/// A call in a method whose contexts are inferred.
struct Site {
    callee: Callee,
    /// Contexts of the caller's class hidden where the call is (by a lambda's own `cx`, say).
    hidden: Vec<String>,
    /// Arguments given by name, and the number of positional ones.
    named: Vec<String>,
    positional: usize,
}

/// Turns `context(...)` clauses into parameters, inferring those of methods of classes that
/// declare contexts. Returns errors by file index.
pub fn add_contexts(files: &mut [(&[String], &mut File)]) -> Vec<(usize, Diag)> {
    let (adds, diags) = {
        let view: Vec<(&[String], &File)> = files.iter().map(|(m, f)| (*m, &**f)).collect();
        if !view.iter().any(|(_, f)| has_clause(&f.items)) {
            return Vec::new();
        }
        plan(&view)
    };
    for (i, (_, f)) in files.iter_mut().enumerate() {
        apply(&mut f.items, i, &adds);
    }
    diags
}

fn has_clause(items: &[Item]) -> bool {
    items.iter().any(|it| match &it.kind {
        ItemKind::Fun(f) => !f.contexts.is_empty(),
        ItemKind::Type(t) => !t.contexts.is_empty() || has_clause(&t.members),
        ItemKind::Impl(i) => has_clause(&i.members),
        ItemKind::Trait(t) => has_clause(&t.members),
        ItemKind::Mod { items, .. } | ItemKind::Object { items, .. } => has_clause(items),
        _ => false,
    })
}

/// The contexts each function takes, and the errors found.
fn plan(files: &[(&[String], &File)]) -> (Adds, Vec<(usize, Diag)>) {
    let mut d = Decls::default();
    for (i, (module, f)) in files.iter().enumerate() {
        d.gather_types(i, module, &f.items);
    }
    for (i, (module, f)) in files.iter().enumerate() {
        d.gather_funs(i, module, &f.items, Owner::Top);
    }
    d.check_clauses();
    let values = d.used_as_values(files);
    // the methods whose contexts come from their class, with what their bodies use
    let mut sites: HashMap<usize, Vec<Site>> = HashMap::new();
    let mut uses: HashMap<usize, HashSet<String>> = HashMap::new();
    let mut fixed_uses = Vec::new();
    for (i, f) in d.funs.iter().enumerate() {
        let Some(c) = f.class.map(|c| &d.classes[c]) else { continue };
        if c.contexts.is_empty() || !f.decl.contexts.is_empty() {
            continue;
        }
        let Some(body) = &f.decl.body else { continue };
        if f.fixed {
            // an override or operator can't take the class's contexts: say so where it uses one
            let mut s = Scan { d: &d, class: c, watch: c.contexts.iter().filter_map(|p| p.name()).collect(), scopes: vec![HashMap::new()], free: HashSet::new(), sites: Vec::new() };
            for p in &f.decl.params {
                s.bind_pat(&p.pat, Some(p.ty.clone()));
            }
            match body {
                FunBody::Block(b) => s.block(b),
                FunBody::Expr(e) => s.expr(e),
            }
            let mut used: Vec<String> = s.free.into_iter().collect();
            used.sort();
            if let Some(n) = used.first() {
                let what = if f.decl.mods.is_operator { "an operator" } else { "an interface method" };
                fixed_uses.push((f.file, f.decl.name_span, format!("`{}` is {}, so it takes only the parameters declared there and can't use the class's context `{}`: keep `{}` in a field, or pass it as an argument", f.decl.name, what, n, n)));
            }
            continue;
        }
        let mut s = Scan { d: &d, class: c, watch: c.contexts.iter().filter_map(|p| p.name()).collect(), scopes: vec![HashMap::new()], free: HashSet::new(), sites: Vec::new() };
        for p in &f.decl.params {
            s.bind_pat(&p.pat, Some(p.ty.clone()));
        }
        match body {
            FunBody::Block(b) => s.block(b),
            FunBody::Expr(e) => s.expr(e),
        }
        uses.insert(i, s.free);
        sites.insert(i, s.sites);
    }
    // a method uses a context when it calls something that takes it where its own is in scope
    loop {
        let mut grew = Vec::new();
        for (&i, here) in &sites {
            let f = &d.funs[i];
            let class = &d.classes[f.class.unwrap()];
            for site in here {
                let Some(g) = d.resolve(f, &site.callee) else { continue };
                let given = site.positional.saturating_sub(d.funs[g].decl.params.len());
                for (k, name) in d.contexts_of(g, &uses, &values).into_iter().enumerate() {
                    let passed = k < given || site.named.contains(&name) || site.hidden.contains(&name);
                    if !passed && class.contexts.iter().any(|p| p.name() == Some(name.as_str())) && !uses[&i].contains(&name) {
                        grew.push((i, name));
                    }
                }
            }
        }
        if grew.is_empty() {
            break;
        }
        for (i, name) in grew {
            uses.get_mut(&i).unwrap().insert(name);
        }
    }
    for (file, span, msg) in fixed_uses {
        d.error(file, span, msg);
    }
    let names: Vec<Names> = files.iter().map(|(module, f)| Names::of(module, f)).collect();
    let mut adds = HashMap::new();
    for (i, f) in d.funs.iter().enumerate() {
        let params: Vec<Param> = if !f.decl.contexts.is_empty() {
            f.decl.contexts.iter().map(|p| context_param(p, p.ty.clone(), false)).collect()
        } else if let Some(used) = uses.get(&i) {
            let all = values.contains(&i);
            let class = &d.classes[f.class.unwrap()];
            class
                .contexts
                .iter()
                .filter(|p| p.name().is_some_and(|n| all || used.contains(n)))
                .map(|p| {
                    // a method in another file (an `impl` block) may not import the type
                    let ty = if f.file == class.file { p.ty.clone() } else { qualify(&p.ty, &names[class.file], class.module, &names[f.file]) };
                    context_param(p, ty, !p.name().is_some_and(|n| used.contains(n)))
                })
                .collect()
        } else {
            continue;
        };
        if !params.is_empty() {
            adds.insert((f.file, f.decl.name_span.start), params);
        }
    }
    (adds, d.diags)
}

/// A context as the parameter it becomes: its default is the caller's variable of its name.
/// One the function doesn't use is named `_cx` (still filled from the caller's `cx`).
fn context_param(p: &Param, ty: Type, unused: bool) -> Param {
    let name = p.name().unwrap_or_default().to_string();
    let default = Expr::new(ExprKind::Path { segs: vec![name.clone()], generics: None, global: false }, p.span);
    let mut out = Param { pat: p.pat.clone(), ty, default: Some(default), context: true, lent: false, span: p.span };
    if let (true, Pat::Ident { name: n, .. }) = (unused, &mut out.pat) {
        *n = format!("_{}", name);
    }
    out
}

/// The names a file uses without a path: those it imports (with their paths from the crate
/// root) and the types it declares.
#[derive(Default)]
struct Names {
    imports: HashMap<String, Vec<String>>,
    declared: HashSet<String>,
}

impl Names {
    fn of(module: &[String], f: &File) -> Names {
        let mut n = Names::default();
        for it in &f.items {
            if let ItemKind::Import(i) = &it.kind {
                n.import(&i.tree, &[], module);
            }
        }
        n.declare(&f.items);
        n
    }

    fn import(&mut self, t: &UseTree, prefix: &[String], module: &[String]) {
        match t {
            UseTree::Path { segments, alias } => {
                let mut path: Vec<String> = prefix.iter().chain(segments).cloned().collect();
                // `{ self, Write }` names the module itself
                if path.len() > 1 && path.last().is_some_and(|s| s == "self") {
                    path.pop();
                }
                if let Some(name) = alias.clone().or_else(|| path.last().cloned()) {
                    self.imports.insert(name, from_root(path, module));
                }
            }
            UseTree::Group { segments, items } => {
                let prefix: Vec<String> = prefix.iter().chain(segments).cloned().collect();
                for i in items {
                    self.import(i, &prefix, module);
                }
            }
            UseTree::Glob { .. } => {}
        }
    }

    fn declare(&mut self, items: &[Item]) {
        for it in items {
            match &it.kind {
                ItemKind::Type(t) => {
                    self.declared.insert(t.name.clone());
                    self.declare(&t.members);
                }
                ItemKind::Trait(t) => {
                    self.declared.insert(t.name.clone());
                }
                ItemKind::TypeAlias { name, .. } => {
                    self.declared.insert(name.clone());
                }
                _ => {}
            }
        }
    }
}

/// An import path with `self::` and `super::` (relative to `module`) resolved from the crate root.
fn from_root(mut path: Vec<String>, module: &[String]) -> Vec<String> {
    let mut base = module.to_vec();
    let mut relative = false;
    while let Some(first) = path.first() {
        match first.as_str() {
            "self" => {}
            "super" => {
                base.pop();
            }
            _ => break,
        }
        path.remove(0);
        relative = true;
    }
    if !relative {
        return path;
    }
    let mut out = if base.is_empty() { vec!["crate".to_string()] } else { base };
    out.extend(path);
    out
}

/// Type `t` as written in a file whose names are `from` (in `module`), for a file whose names
/// are `to`: a type that file can't name gets the path it has where it was written
/// (`Log` -> `log::Log`, which the lowering starts with `crate::`).
fn qualify(t: &Type, from: &Names, module: &[String], to: &Names) -> Type {
    let q = |t: &Type| qualify(t, from, module, to);
    let arg = |a: &GenericArg| match a {
        GenericArg::Type(t) => GenericArg::Type(q(t)),
        GenericArg::Binding(n, t) => GenericArg::Binding(n.clone(), q(t)),
        a => a.clone(),
    };
    match t {
        Type::Path { global: false, segs, span } if !segs.is_empty() => {
            let mut segs: Vec<PathSeg> = segs.iter().map(|s| PathSeg { name: s.name.clone(), generics: s.generics.as_ref().map(|g| g.iter().map(arg).collect()), fn_sugar: s.fn_sugar.clone() }).collect();
            let first = segs[0].name.clone();
            let path = if to.imports.contains_key(&first) || to.declared.contains(&first) {
                None
            } else if let Some(p) = from.imports.get(&first) {
                Some(p.clone())
            } else if from.declared.contains(&first) {
                Some(if module.is_empty() { vec!["crate".to_string(), first] } else { module.iter().cloned().chain([first]).collect() })
            } else {
                None
            };
            let Some(mut path) = path else { return Type::Path { global: false, segs, span: *span } };
            // `import ::gpui::App`
            let global = path.first().is_some_and(|s| s.is_empty());
            if global {
                path.remove(0);
            }
            let head = segs.remove(0);
            let mut out: Vec<PathSeg> = path.into_iter().map(PathSeg::plain).collect();
            // the written segment's type arguments stay on it: `Context<Self>` -> `gpui::Context<Self>`
            if let Some(last) = out.last_mut() {
                last.generics = head.generics;
                last.fn_sugar = head.fn_sugar;
            }
            out.extend(segs);
            Type::Path { global, segs: out, span: *span }
        }
        Type::Ref { lifetime, mutable, inner } => Type::Ref { lifetime: lifetime.clone(), mutable: *mutable, inner: Box::new(q(inner)) },
        Type::Ptr { mutable, inner } => Type::Ptr { mutable: *mutable, inner: Box::new(q(inner)) },
        Type::Option(x) => Type::Option(Box::new(q(x))),
        Type::Slice(x) => Type::Slice(Box::new(q(x))),
        Type::Array(x, n) => Type::Array(Box::new(q(x)), n.clone()),
        Type::Tuple(v) => Type::Tuple(v.iter().map(q).collect()),
        Type::Fn { params, ret } => Type::Fn { params: params.iter().map(q).collect(), ret: Box::new(q(ret)) },
        Type::Dyn(v) => Type::Dyn(v.iter().map(q).collect()),
        Type::Impl(v) => Type::Impl(v.iter().map(q).collect()),
        Type::Bounds(v) => Type::Bounds(v.iter().map(q).collect()),
        t => t.clone(),
    }
}

/// Appends the planned contexts to the parameters of each function.
fn apply(items: &mut [Item], file: usize, adds: &Adds) {
    for it in items {
        match &mut it.kind {
            ItemKind::Fun(f) => {
                if let Some(ps) = adds.get(&(file, f.name_span.start)) {
                    f.params.extend(ps.iter().cloned());
                }
            }
            ItemKind::Type(t) => apply(&mut t.members, file, adds),
            ItemKind::Impl(i) => apply(&mut i.members, file, adds),
            ItemKind::Mod { items, .. } | ItemKind::Object { items, .. } => apply(items, file, adds),
            _ => {}
        }
    }
}

impl<'a> Decls<'a> {
    fn gather_types(&mut self, file: usize, module: &'a [String], items: &'a [Item]) {
        for it in items {
            match &it.kind {
                ItemKind::Type(t) if matches!(t.kind, TypeKind::Class | TypeKind::Data | TypeKind::TupleStruct) => {
                    let fields = t.fields.iter().map(|f| (f.name.as_str(), &f.ty)).collect();
                    let generic = t.generics.params.iter().any(|g| matches!(g, GenericParam::Type { .. } | GenericParam::Const { .. }));
                    self.classes.push(Class { name: &t.name, generic, file, module, contexts: &t.contexts, methods: HashMap::new(), fields });
                    self.gather_types(file, module, &t.members);
                }
                ItemKind::Type(t) => self.gather_types(file, module, &t.members),
                ItemKind::Trait(t) => {
                    let methods = t.members.iter().filter_map(|m| if let ItemKind::Fun(f) = &m.kind { Some(f.name.as_str()) } else { None }).collect();
                    self.traits.insert(&t.name, methods);
                }
                ItemKind::Mod { items, .. } | ItemKind::Object { items, .. } => self.gather_types(file, module, items),
                _ => {}
            }
        }
    }

    /// Functions and methods, each with what holds it.
    fn gather_funs(&mut self, file: usize, module: &'a [String], items: &'a [Item], owner: Owner<'a>) {
        for it in items {
            match &it.kind {
                ItemKind::Fun(f) => {
                    let i = self.funs.len();
                    let (class, fixed) = match owner {
                        // a method named like one of an interface the class implements goes to that
                        // interface's `impl` (`split_members`)
                        Owner::Class { class, trait_impl, supers } => {
                            let of_iface = supers.iter().filter_map(|t| t.last_name()).any(|n| {
                                self.traits.get(n).is_some_and(|ms| ms.contains(f.name.as_str())) || crate::lower::std_trait_of_method(&f.name).contains(&n)
                            });
                            (Some(class), trait_impl || of_iface)
                        }
                        _ => (None, false),
                    };
                    let fixed = fixed || f.mods.is_override || f.mods.is_operator;
                    self.funs.push(Fun { file, module, class, decl: f, fixed });
                    match owner {
                        Owner::Top => self.free.entry(&f.name).or_default().push(i),
                        Owner::Class { class, trait_impl: false, .. } => {
                            self.classes[class].methods.entry(&f.name).or_insert(i);
                        }
                        _ => {}
                    }
                }
                ItemKind::Type(t) => {
                    let owner = match t.kind {
                        TypeKind::Class | TypeKind::Data | TypeKind::TupleStruct => match self.class_at(&t.name, module) {
                            Some(class) => Owner::Class { class, trait_impl: false, supers: &t.supertypes },
                            None => Owner::Other,
                        },
                        _ => Owner::Other,
                    };
                    self.gather_funs(file, module, &t.members, owner);
                }
                ItemKind::Impl(i) => {
                    let owner = match i.self_ty.last_name().and_then(|n| self.class_at(n, module)) {
                        Some(class) => Owner::Class { class, trait_impl: i.trait_.is_some(), supers: &[] },
                        None => Owner::Other,
                    };
                    self.gather_funs(file, module, &i.members, owner);
                }
                ItemKind::Trait(t) => {
                    for m in &t.members {
                        if let ItemKind::Fun(f) = &m.kind {
                            if !f.contexts.is_empty() {
                                self.error(file, f.contexts[0].span, "an interface method can't take contexts: write them as parameters");
                            }
                        }
                    }
                }
                ItemKind::Mod { items, .. } | ItemKind::Object { items, .. } => self.gather_funs(file, module, items, owner),
                _ => {}
            }
        }
    }

    fn error(&mut self, file: usize, span: crate::diag::Span, msg: impl Into<String>) {
        self.diags.push((file, Diag::error(span, msg)));
    }

    /// Clauses Oxie can't honor: on `main`, overrides and operators, or naming a parameter.
    fn check_clauses(&mut self) {
        let mut errs = Vec::new();
        for f in &self.funs {
            let Some(first) = f.decl.contexts.first() else { continue };
            let name = &f.decl.name;
            if f.decl.mods.is_operator {
                errs.push((f.file, first.span, format!("the operator `{}` takes only its operand, so it can't take contexts", name)));
            } else if f.fixed {
                errs.push((f.file, first.span, format!("`{}` implements an interface method, so its parameters come from the interface: write the context as a parameter", name)));
            } else if name == "main" && f.class.is_none() {
                errs.push((f.file, first.span, "`main` can't take contexts: nothing calls it to pass them".to_string()));
            }
            for c in &f.decl.contexts {
                if let Some(n) = c.name().filter(|n| f.decl.params.iter().any(|p| p.name() == Some(*n))) {
                    errs.push((f.file, c.span, format!("`{}` is both a parameter and a context of `{}`", n, name)));
                }
            }
        }
        for (file, span, msg) in errs {
            self.error(file, span, msg);
        }
    }

    /// The class `name`, preferring the one declared in `module`.
    fn class_at(&self, name: &str, module: &[String]) -> Option<usize> {
        let mut found = None;
        for (i, c) in self.classes.iter().enumerate() {
            if c.name == name {
                if c.module == module {
                    return Some(i);
                }
                found.get_or_insert(i);
            }
        }
        found
    }

    /// The top-level function `name`, preferring the one in `module` (or one in `path`, when the
    /// call names its module).
    fn free_fun(&self, name: &str, module: &[String], path: Option<&[String]>) -> Option<usize> {
        let cands = self.free.get(name)?;
        if let Some(p) = path {
            return cands.iter().copied().find(|&i| self.funs[i].module.ends_with(p));
        }
        cands.iter().copied().find(|&i| self.funs[i].module == module).or_else(|| (cands.len() == 1).then(|| cands[0]))
    }

    /// The function a call goes to, if it is declared in the crate.
    fn resolve(&self, caller: &Fun, callee: &Callee) -> Option<usize> {
        let own = |n: &str| caller.class.and_then(|c| self.classes[c].methods.get(n).copied());
        match callee {
            Callee::Bare(n) => own(n)
                .or_else(|| self.class_at(n, caller.module).and_then(|c| self.classes[c].methods.get("new").copied()))
                .or_else(|| self.free_fun(n, caller.module, None)),
            Callee::OnThis(n) => own(n),
            Callee::On(c, n) => self.classes[*c].methods.get(n.as_str()).copied(),
            Callee::Path(segs) => {
                let (owner, last) = segs.split_at(segs.len() - 1);
                let last = last[0].as_str();
                if owner == ["Self"] {
                    return own(last);
                }
                if let Some(c) = owner.last().and_then(|o| self.class_at(o, caller.module)) {
                    return self.classes[c].methods.get(last).copied();
                }
                self.free_fun(last, caller.module, Some(owner))
            }
        }
    }

    /// The contexts function `g` takes so far, in order.
    fn contexts_of(&self, g: usize, uses: &HashMap<usize, HashSet<String>>, values: &HashSet<usize>) -> Vec<String> {
        let f = &self.funs[g];
        if !f.decl.contexts.is_empty() {
            return f.decl.contexts.iter().filter_map(|p| p.name().map(String::from)).collect();
        }
        let Some(used) = uses.get(&g) else { return Vec::new() };
        let all = values.contains(&g);
        let class = &self.classes[f.class.unwrap()];
        class.contexts.iter().filter_map(|p| p.name()).filter(|n| all || used.contains(*n)).map(String::from).collect()
    }

    /// Methods referred to without being called (`cx.listener(Self.save)`, `Point.dist`): their
    /// callers pass every argument, so they take every context of their class.
    fn used_as_values(&self, files: &[(&[String], &File)]) -> HashSet<usize> {
        let mut out = HashSet::new();
        for (module, f) in files {
            self.values_in(module, &f.items, None, &mut out);
        }
        out
    }

    fn values_in(&self, module: &[String], items: &[Item], class: Option<usize>, out: &mut HashSet<usize>) {
        for it in items {
            match &it.kind {
                ItemKind::Fun(f) => match &f.body {
                    Some(FunBody::Block(b)) => self.values_in_block(module, b, class, out),
                    Some(FunBody::Expr(e)) => self.values_in_block(module, &Block { stmts: vec![Stmt::Expr { expr: e.clone(), semi: false }], span: e.span }, class, out),
                    None => {}
                },
                ItemKind::Type(t) => {
                    let c = matches!(t.kind, TypeKind::Class | TypeKind::Data | TypeKind::TupleStruct).then(|| self.class_at(&t.name, module)).flatten();
                    self.values_in(module, &t.members, c, out);
                }
                ItemKind::Impl(i) => self.values_in(module, &i.members, i.self_ty.last_name().and_then(|n| self.class_at(n, module)), out),
                ItemKind::Mod { items, .. } | ItemKind::Object { items, .. } => self.values_in(module, items, None, out),
                ItemKind::Test(t) => self.values_in_block(module, &t.body, None, out),
                _ => {}
            }
        }
    }

    /// `Self.save`, `Self::save` or `Editor.save` in `b` without a call: the method as a value.
    fn values_in_block(&self, module: &[String], b: &Block, class: Option<usize>, out: &mut HashSet<usize>) {
        let method = |owner: &str, name: &str| {
            let c = if owner == "Self" { class } else { self.class_at(owner, module) }?;
            let c = &self.classes[c];
            (!c.fields.contains_key(name)).then(|| c.methods.get(name).copied()).flatten()
        };
        let mut callees = HashSet::new();
        crate::visit::walk_block(b, &mut |e| {
            match &e.kind {
                ExprKind::Call { callee, .. } => {
                    callees.insert((callee.span.start, callee.span.end));
                }
                _ if callees.contains(&(e.span.start, e.span.end)) => {}
                ExprKind::Path { segs, global: false, .. } if segs.len() >= 2 => out.extend(method(&segs[segs.len() - 2], &segs[segs.len() - 1])),
                ExprKind::Field { recv, name, .. } => out.extend(recv.ident().and_then(|o| method(o, name))),
                _ => {}
            }
            true
        });
    }
}

/// Walks a method body with its scopes: which contexts of its class it reads where no binding
/// of its own hides them, and the calls it makes.
struct Scan<'d, 'a> {
    d: &'d Decls<'a>,
    class: &'d Class<'a>,
    watch: Vec<&'a str>,
    /// Names bound in each scope, with their types when known.
    scopes: Vec<HashMap<String, Option<Type>>>,
    free: HashSet<String>,
    sites: Vec<Site>,
}

impl Scan<'_, '_> {
    fn lookup(&self, name: &str) -> Option<&Option<Type>> {
        self.scopes.iter().rev().find_map(|s| s.get(name))
    }

    fn bind(&mut self, name: &str, ty: Option<Type>) {
        self.scopes.last_mut().unwrap().insert(name.to_string(), ty);
    }

    fn bind_pat(&mut self, p: &Pat, ty: Option<Type>) {
        if let Pat::Ident { name, sub: None, .. } = p {
            self.bind(name, ty);
            return;
        }
        let mut names = Vec::new();
        p.bindings(&mut names);
        for n in names {
            self.bind(&n, None);
        }
    }

    fn read(&mut self, name: &str) {
        if self.watch.contains(&name) && self.lookup(name).is_none() {
            self.free.insert(name.to_string());
        }
    }

    /// The class of a value of type `t` (`Cart`, `&mut Cart`, or `Cart?`, which a null check
    /// unwraps), if it is one of the crate's.
    fn class_of_type(&self, t: &Type) -> Option<usize> {
        match t {
            Type::Ref { inner, .. } | Type::Option(inner) => self.class_of_type(inner),
            Type::Path { segs, .. } if segs.len() == 1 => self.d.class_at(&segs[0].name, self.class.module),
            _ => None,
        }
    }

    /// The class whose fields and methods `e` reaches: this one for `this`.
    fn class_of(&self, e: &Expr) -> Option<&Class<'_>> {
        match &e.kind {
            ExprKind::SelfValue => Some(self.class),
            _ => self.type_of(e).and_then(|t| self.class_of_type(&t)).map(|c| &self.d.classes[c]),
        }
    }

    /// The type of `e` where the lowering knows it too (`infer.rs`), so that both see the same
    /// calls: variables, fields, constructors, elements, and the results of the crate's functions
    /// and methods.
    fn type_of(&self, e: &Expr) -> Option<Type> {
        use ExprKind::*;
        let ret = |f: usize| {
            let decl = self.d.funs[f].decl;
            match decl.ret.clone()? {
                _ if !matches!(decl.throws, Throws::No) => None,
                t if t.single_name() == Some("Self") => self.d.funs[f].class.map(|c| Type::simple(self.d.classes[c].name)),
                t => Some(t),
            }
        };
        match &e.kind {
            Path { segs, global: false, .. } if segs.len() == 1 => match self.lookup(&segs[0]) {
                Some(t) => t.clone(),
                None => self.class.fields.get(segs[0].as_str()).map(|t| (*t).clone()),
            },
            Field { recv, name, .. } => self.class_of(recv)?.fields.get(name.as_str()).map(|t| (*t).clone()),
            Paren(x) | Unary(UnOp::Ref | UnOp::RefMut | UnOp::Deref, x) => self.type_of(x),
            Index(coll, i) if !matches!(i.kind, Range { .. }) => elem_type(self.type_of(coll)?),
            Call { callee, .. } => {
                let n = callee.ident()?;
                if self.lookup(n).is_some() {
                    return None;
                }
                if let Some(&m) = self.class.methods.get(n) {
                    return ret(m);
                }
                if let Some(c) = self.d.class_at(n, self.class.module) {
                    return (!self.d.classes[c].generic).then(|| Type::simple(n));
                }
                self.d.free_fun(n, self.class.module, None).and_then(ret)
            }
            MethodCall { recv, name, safe: false, .. } => {
                let c = self.class_of(recv).filter(|c| !c.generic || matches!(recv.kind, SelfValue))?;
                ret(*c.methods.get(name.as_str())?)
            }
            _ => None,
        }
    }

    fn site(&mut self, callee: Callee, args: &[Arg]) {
        let hidden = self.watch.iter().filter(|n| self.lookup(n).is_some()).map(|n| n.to_string()).collect();
        let named = args.iter().filter_map(|a| a.name.clone()).collect();
        let positional = args.iter().filter(|a| a.name.is_none()).count();
        self.sites.push(Site { callee, hidden, named, positional });
    }

    fn block(&mut self, b: &Block) {
        self.scopes.push(HashMap::new());
        for s in &b.stmts {
            self.stmt(s);
        }
        self.scopes.pop();
    }

    fn stmt(&mut self, s: &Stmt) {
        match s {
            Stmt::Let { pat, ty, init, else_, .. } => {
                if let Some(e) = init {
                    self.expr(e);
                }
                if let Some(b) = else_ {
                    self.block(b);
                }
                let ty = ty.clone().or_else(|| init.as_ref().and_then(|e| self.type_of(e)));
                self.bind_pat(pat, ty);
            }
            Stmt::Expr { expr, .. } => self.expr(expr),
            Stmt::Attributed { stmt, .. } => self.stmt(stmt),
            // a local function can't see the method's variables
            Stmt::Item(_) => {}
        }
    }

    /// A condition's bindings go into the current scope, for the branch it guards.
    fn cond(&mut self, c: &Cond) {
        match c {
            Cond::Expr(e) => self.expr(e),
            // the lowering doesn't type what a pattern binds
            Cond::Let { pat, expr, .. } => {
                self.expr(expr);
                self.bind_pat(pat, None);
            }
            Cond::Chain(parts) => {
                for p in parts {
                    self.cond(p);
                }
            }
        }
    }

    fn expr(&mut self, e: &Expr) {
        use ExprKind::*;
        match &e.kind {
            Path { segs, global: false, .. } if segs.len() == 1 => self.read(&segs[0]),
            Call { callee, args } => {
                match &callee.kind {
                    Path { segs, global: false, .. } if segs.len() == 1 => {
                        if self.lookup(&segs[0]).is_some() {
                            // a lambda in a variable
                            self.read(&segs[0]);
                        } else {
                            self.site(Callee::Bare(segs[0].clone()), args);
                        }
                    }
                    Path { segs, global: false, .. } => self.site(Callee::Path(segs.clone()), args),
                    _ => self.expr(callee),
                }
                for a in args {
                    self.expr(&a.value);
                }
            }
            MethodCall { recv, name, args, safe, .. } => {
                let callee = match &recv.kind {
                    // `a?.f()` takes its arguments as written
                    _ if *safe => None,
                    SelfValue => Some(Callee::OnThis(name.clone())),
                    // a type or a module: `Point.origin()`, `util.helper()`
                    Path { segs, global: false, .. } if segs.len() > 1 || (self.lookup(&segs[0]).is_none() && !self.class.fields.contains_key(segs[0].as_str())) => {
                        Some(Callee::Path(segs.iter().cloned().chain([name.clone()]).collect()))
                    }
                    _ => self.type_of(recv).and_then(|t| self.class_of_type(&t)).map(|c| Callee::On(c, name.clone())),
                };
                if let Some(c) = callee {
                    self.site(c, args);
                }
                self.expr(recv);
                for a in args {
                    self.expr(&a.value);
                }
            }
            If { cond, then, else_ } => {
                self.scopes.push(HashMap::new());
                self.cond(cond);
                self.block(then);
                self.scopes.pop();
                if let Some(x) = else_ {
                    self.expr(x);
                }
            }
            While { cond, body, .. } => {
                self.scopes.push(HashMap::new());
                self.cond(cond);
                self.block(body);
                self.scopes.pop();
            }
            When { subject, binding, arms, .. } => {
                if let Some(s) = subject {
                    self.expr(s);
                }
                self.scopes.push(HashMap::new());
                if let Some(p) = binding {
                    self.bind_pat(p, None);
                }
                for a in arms {
                    self.scopes.push(HashMap::new());
                    for c in &a.conds {
                        match c {
                            WhenCond::Pat(p) => self.bind_pat(p, None),
                            WhenCond::In { expr, .. } | WhenCond::Expr(expr) => self.expr(expr),
                            WhenCond::Else => {}
                        }
                    }
                    if let Some(g) = &a.guard {
                        self.expr(g);
                    }
                    self.expr(&a.body);
                    self.scopes.pop();
                }
                self.scopes.pop();
            }
            For { pat, iter, body, .. } => {
                self.expr(iter);
                let ty = self.type_of(iter).and_then(elem_type);
                self.scopes.push(HashMap::new());
                self.bind_pat(pat, ty);
                self.block(body);
                self.scopes.pop();
            }
            Lambda { params, body, implicit_it, .. } => {
                self.scopes.push(HashMap::new());
                for p in params {
                    self.bind_pat(&p.pat, p.ty.clone());
                }
                if *implicit_it {
                    self.bind("it", None);
                }
                self.expr(body);
                self.scopes.pop();
            }
            TryCatch { body, catch } => {
                self.block(body);
                if let Some(c) = catch {
                    self.scopes.push(HashMap::new());
                    self.bind_pat(&c.binding, None);
                    self.block(&c.body);
                    self.scopes.pop();
                }
            }
            StructLit { fields, base, .. } => {
                for (name, v) in fields {
                    match v {
                        Some(v) => self.expr(v),
                        // `Point { x }` reads `x`
                        None => self.read(name),
                    }
                }
                if let Some(b) = base {
                    self.expr(b);
                }
            }
            // raw Rust: a context it names is used
            Macro { text, .. } | Raw(text) => {
                for n in self.watch.clone() {
                    if mentions_word(text, n) {
                        self.read(n);
                    }
                }
            }
            Template(parts) => {
                for p in parts {
                    if let TemplatePart::Expr(x, _) = p {
                        self.expr(x);
                    }
                }
            }
            Block(b) | Unsafe(b) | ConstBlock(b) | LabeledBlock { block: b, .. } | Loop { body: b, .. } | Async { block: b, .. } => self.block(b),
            DoWhile { body, cond, .. } => {
                self.block(body);
                self.expr(cond);
            }
            Unary(_, x) | Try(x) | Unwrap(x) | Await(x) | Cast(x, _) | Paren(x) | Throw(x) | Field { recv: x, .. } | Is { expr: x, .. } => self.expr(x),
            Binary(_, a, b) | Assign(_, a, b) | Elvis(a, b) | Pair(a, b) | Index(a, b) | ArrayRepeat(a, b) | In { item: a, coll: b, .. } => {
                self.expr(a);
                self.expr(b);
            }
            Return(Some(x)) | Break { value: Some(x), .. } => self.expr(x),
            Tuple(v) | Array(v) | VecLit(v) | MapLit(v) | SetLit(v) => {
                for x in v {
                    self.expr(x);
                }
            }
            Range { start, end, .. } => {
                for x in [start, end].into_iter().flatten() {
                    self.expr(x);
                }
            }
            _ => {}
        }
    }
}

/// The element type of a list, array or slice type.
fn elem_type(mut t: Type) -> Option<Type> {
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

/// Does `text` contain `word` as a whole identifier?
fn mentions_word(text: &str, word: &str) -> bool {
    let ident = |c: char| c.is_alphanumeric() || c == '_';
    text.match_indices(word).any(|(i, _)| {
        !text[..i].chars().next_back().is_some_and(ident) && !text[i + word.len()..].chars().next().is_some_and(ident)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The context parameters each function of `files` ends up with (`name: [cx, _window]`), in
    /// the order of the files and their declarations.
    fn contexts(files: &[(&str, &str)]) -> Vec<String> {
        let mut parsed: Vec<(Vec<String>, File)> = files
            .iter()
            .map(|(module, src)| {
                let (file, diags) = crate::parser::parse_file(src);
                assert!(diags.is_empty(), "{:?}", diags);
                let module = if module.is_empty() { Vec::new() } else { vec![module.to_string()] };
                (module, file)
            })
            .collect();
        let diags = add_contexts(&mut parsed.iter_mut().map(|(m, f)| (m.as_slice(), f)).collect::<Vec<_>>());
        assert!(diags.is_empty(), "{:?}", diags);
        fn each(items: &[Item], out: &mut Vec<String>) {
            for it in items {
                match &it.kind {
                    ItemKind::Fun(f) => {
                        let ps: Vec<String> = f.params.iter().filter(|p| p.context).map(|p| format!("{}: {}", p.name().unwrap_or("_"), crate::symbols::type_text(&p.ty))).collect();
                        out.push(format!("{}: [{}]", f.name, ps.join(", ")));
                    }
                    ItemKind::Type(t) => each(&t.members, out),
                    ItemKind::Impl(i) => each(&i.members, out),
                    _ => {}
                }
            }
        }
        let mut out = Vec::new();
        for (_, f) in &parsed {
            each(&f.items, &mut out);
        }
        out
    }

    #[test]
    fn methods_take_the_contexts_they_use() {
        let src = "context(window: &mut Window, cx: &mut App)
class Editor(var text: String) {
    fun type_text(s: &str) { text.push_str(s); refresh() }
    fun refresh() { cx.notify() }
    fun title(): String = \"${text.len()}\"
    fun later() { run_later { cx -> refresh() } }
    fun paint() { window.paint(text) }
}";
        assert_eq!(
            contexts(&[("", src)]),
            ["type_text: [cx: &mut App]", "refresh: [cx: &mut App]", "title: []", "later: []", "paint: [window: &mut Window]"]
        );
    }

    #[test]
    fn handlers_take_every_context_and_pub_methods_those_they_use() {
        let src = "context(window: &mut Window, cx: &mut App)
class View(var n: i32) {
    pub fun reset() { n = 0 }
    pub fun refresh() { cx.notify() }
    fun on_click(e: &Click) { cx.notify() }
    fun render() { cx.listener(Self.on_click) }
}";
        assert_eq!(
            contexts(&[("", src)]),
            ["reset: []", "refresh: [cx: &mut App]", "on_click: [_window: &mut Window, cx: &mut App]", "render: [cx: &mut App]"]
        );
    }

    #[test]
    fn own_clauses_and_calls_through_values() {
        let src = "context(log: &mut Log)
fun record(s: &str) = log.add(s)

context(log: &mut Log)
class Tab(name: String) {
    fun close() = record(name.as_str())
    context(out: &mut Vec<String>)
    fun export() = out.push(name.clone())
}

context(log: &mut Log)
class Pane(var tabs: Vec<Tab>, var first: Tab) {
    fun close_all() { for (t in tabs) t.close() }
    fun close_first() = first.close()
    fun close_given(log: &mut Log) = first.close()
    fun named() = record(\"x\", log = &mut Log())
}";
        assert_eq!(
            contexts(&[("", src)]),
            [
                "record: [log: &mut Log]",
                "close: [log: &mut Log]",
                "export: [out: &mut Vec<String>]",
                "close_all: [log: &mut Log]",
                "close_first: [log: &mut Log]",
                "close_given: []",
                "named: []",
            ]
        );
    }

    #[test]
    fn methods_in_other_files_name_the_type_through_its_module() {
        let cart = "import log::Log

context(log: &mut Log)
class Cart(var items: Vec<String>) {
    fun add(s: &str) = log.add(s)
}";
        let totals = "impl Cart {
    fun add_twice(s: &str) { add(s); add(s) }
}";
        assert_eq!(contexts(&[("cart", cart), ("totals", totals)]), ["add: [log: &mut Log]", "add_twice: [log: &mut log::Log]"]);
    }
}
