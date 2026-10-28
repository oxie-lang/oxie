//! Context parameters for code that passes the same values down most calls, like GPUI's
//! `window: &mut Window, cx: &mut Context<Self>`.
//!
//! A scan of the whole crate (every file given to [`crate::translate_crate`], or the one file
//! given to [`crate::translate`]) picks for each class the trailing parameters most of its
//! methods take, which become the class's `context(...)`, and what each method takes of them:
//! its own trailing parameters, or nothing when it keeps them as written. The decision depends
//! only on signatures and visibilities, so every file of the crate makes the same one: a class
//! declared in `workspace.rs` gets the clause there, and its methods in the `impl` blocks of
//! the other files drop the same parameters. The printer then chooses how a method gets its
//! contexts back (inferred by Oxie, or a `context(...)` of its own), and calls the compiler
//! resolves drop the trailing arguments that name them. Compiling the translation back checks that each method's parameters and
//! each call's arguments come out as in the Rust.

use std::collections::{HashMap, HashSet};

use syn::visit::Visit;

use crate::printer::Printer;
use crate::types::tok;

/// Fewest methods of a class that must end their parameters with a value for it to become one
/// of the class's contexts, and the fewest per hundred methods.
const MIN_METHODS: usize = 3;
const MIN_PERCENT: usize = 25;

/// What the crate's classes do with their contexts.
#[derive(Default, Debug, Clone)]
pub struct Crate {
    pub classes: HashMap<String, Class>,
    /// The field names of every struct with named fields, by struct name (of all structs of a
    /// name, together).
    pub struct_fields: HashMap<String, HashSet<String>>,
    /// Every struct and enum name, and whether all those of the name derive `Copy` (Oxie lends a
    /// parameter of one that doesn't, unless it says `own`).
    pub value_types: HashMap<String, bool>,
    /// A library's: its public methods that change their object say `mut fun`, which Oxie
    /// requires of a library's API.
    pub library: bool,
}

#[derive(Debug, Clone)]
pub struct Class {
    /// The `context(...)`: names and types, in order.
    pub contexts: Vec<(String, syn::Type)>,
    /// The file that declares the class.
    pub file: usize,
    pub decl: syn::ItemStruct,
    /// Inherent methods (in any file) by name.
    pub methods: HashMap<String, Method>,
}

#[derive(Debug, Clone)]
pub struct Method {
    pub file: usize,
    pub sig: syn::Signature,
    /// Referred to without a call (`cx.listener(Self::save)`): Oxie gives it every context too.
    pub value: bool,
    /// The contexts it takes, which are its last parameters: `Some(vec![])` takes none, and
    /// `None` keeps its parameters as written (so calls pass them too).
    pub takes: Option<Vec<String>>,
}

impl Crate {
    /// The class `name` if it has contexts.
    pub fn class(&self, name: &str) -> Option<&Class> {
        self.classes.get(name).filter(|c| !c.contexts.is_empty())
    }

    /// The contexts the method `name` of class `class` takes, if any.
    pub fn takes(&self, class: &str, name: &str) -> Option<&[String]> {
        self.class(class)?.methods.get(name)?.takes.as_deref().filter(|t| !t.is_empty())
    }

    /// Stops giving class `name` contexts (its declaration was kept as Rust); false if it had none.
    pub fn drop_class(&mut self, name: &str) -> bool {
        match self.classes.get_mut(name) {
            Some(c) if !c.contexts.is_empty() => {
                c.contexts.clear();
                true
            }
            _ => false,
        }
    }

    /// What file `file` needs to see of the other files to translate like the whole crate does
    /// (an Oxie file compiled next to it): the classes with contexts that it names but doesn't
    /// declare, and their methods in the other files, without bodies, so that Oxie resolves
    /// calls to them as it does in the crate.
    pub fn stub(&self, file: usize, names: &HashSet<String>, p: &Printer) -> String {
        let mut classes: Vec<(&String, &Class)> = self.classes.iter().filter(|(n, c)| !c.contexts.is_empty() && (c.file == file || names.contains(*n))).collect();
        classes.sort_by_key(|(n, _)| *n);
        let mut out = Vec::new();
        for (name, c) in classes {
            if c.file != file {
                out.push(format!("{}\nclass {} {{}}", p.context_clause(&c.contexts), name));
            }
            let mut methods: Vec<(&String, &Method)> = c.methods.iter().filter(|(_, m)| m.file != file).collect();
            methods.sort_by_key(|(n, _)| *n);
            let mut funs = Vec::new();
            for (n, m) in methods {
                let takes = m.takes.as_deref().unwrap_or_default();
                let own: Vec<(String, syn::Type)> = c.contexts.iter().filter(|(n, _)| takes.contains(n)).cloned().collect();
                let clause = if own.is_empty() { String::new() } else { format!("{} ", p.context_clause(&own)) };
                let params: Vec<String> = m
                    .sig
                    .inputs
                    .iter()
                    .filter_map(|a| if let syn::FnArg::Typed(pt) = a { Some(p.ty(&pt.ty)) } else { None })
                    .enumerate()
                    .take(m.sig.inputs.iter().filter(|a| matches!(a, syn::FnArg::Typed(_))).count() - takes.len())
                    .map(|(i, t)| format!("p{}: {}", i, t))
                    .collect();
                let kw = if m.sig.receiver().is_some() { "fun" } else { "static fun" };
                funs.push(format!("    {}{} {}{}({}) {{}}", clause, kw, crate::types::ident(n), p.generics(&m.sig.generics), params.join(", ")));
            }
            if !funs.is_empty() {
                out.push(format!("impl {} {{\n{}\n}}", name, funs.join("\n")));
            }
        }
        out.join("\n\n")
    }

    /// Keeps method `name` of `class` with its parameters as written (it was kept as Rust).
    /// One used as a value would get every context of its class, so the class gets none.
    pub fn keep_method(&mut self, class: &str, name: &str) -> bool {
        let Some(c) = self.classes.get_mut(class) else { return false };
        let Some(m) = c.methods.get_mut(name) else { return false };
        if m.takes.as_ref().is_none_or(|t| t.is_empty()) {
            return false;
        }
        m.takes = None;
        if m.value {
            c.contexts.clear();
        }
        true
    }
}

/// A parameter as (name without a leading `_`, whether it is unused, type); `_` has no name.
fn param(a: &syn::FnArg) -> Option<(Option<String>, bool, &syn::Type)> {
    let syn::FnArg::Typed(pt) = a else { return None };
    let pi = match &*pt.pat {
        syn::Pat::Ident(pi) if pi.by_ref.is_none() && pi.mutability.is_none() && pi.subpat.is_none() => pi,
        syn::Pat::Wild(_) => return Some((None, true, &*pt.ty)),
        _ => return None,
    };
    let n = pi.ident.to_string();
    Some(match n.strip_prefix('_') {
        Some(rest) if !rest.is_empty() => (Some(rest.to_string()), true, &*pt.ty),
        _ => (Some(n), false, &*pt.ty),
    })
}

/// The last parameters of `sig` that are contexts in `contexts` (in their order): their names,
/// and whether any is unused (`_cx` or `_`).
pub fn trailing(sig: &syn::Signature, contexts: &[(String, syn::Type)]) -> (Vec<String>, bool) {
    let mut names = Vec::new();
    let mut unused = false;
    // contexts left that may come before the ones found
    let mut left = contexts.len();
    for a in sig.inputs.iter().rev() {
        let Some((n, u, t)) = param(a) else { break };
        let Some(k) = contexts[..left].iter().rposition(|(cn, ct)| n.as_ref().is_none_or(|n| n == cn) && tok(ct) == tok(t)) else { break };
        names.insert(0, contexts[k].0.clone());
        unused |= u;
        left = k;
    }
    (names, unused)
}

/// Scans the crate's files.
pub fn scan(files: &[&syn::File]) -> Crate {
    let mut s = Scan::default();
    for (i, f) in files.iter().enumerate() {
        s.types(i, &f.items);
    }
    for (i, f) in files.iter().enumerate() {
        s.impls(i, &f.items);
    }
    let mut krate = Crate { struct_fields: std::mem::take(&mut s.struct_fields), value_types: std::mem::take(&mut s.value_types), ..Crate::default() };
    for (name, (file, decl)) in s.structs {
        // the stubs `stub` writes have no type parameters
        if s.ambiguous.contains(&name) || !decl.generics.params.is_empty() {
            continue;
        }
        let methods = s.methods.remove(&name).unwrap_or_default();
        let mut class = Class { contexts: Vec::new(), file, decl, methods: HashMap::new() };
        let contexts = pick_contexts(&methods);
        for (file, sig) in methods {
            let name = sig.ident.to_string();
            class.methods.insert(name, Method { file, sig, value: false, takes: None });
        }
        for (owner, m) in &s.values {
            if *owner == class.decl.ident {
                if let Some(m) = class.methods.get_mut(m) {
                    m.value = true;
                }
            }
        }
        class.contexts = contexts;
        plan_methods(&mut class);
        krate.classes.insert(name, class);
    }
    krate
}

/// The parameters most methods end with: the most common last one, then the most common one
/// before those, as long as enough methods take it. Only `&mut` references qualify (`&mut
/// Window`): what is passed down every call to be changed, not any shared value.
fn pick_contexts(methods: &[(usize, syn::Signature)]) -> Vec<(String, syn::Type)> {
    let mut out: Vec<(String, syn::Type)> = Vec::new();
    let enough = |n: usize| n >= MIN_METHODS && n * 100 >= methods.len() * MIN_PERCENT;
    loop {
        let mut counts: HashMap<(String, String), (usize, syn::Type)> = HashMap::new();
        for (_, m) in methods {
            // the parameter before the contexts it ends with
            let mut rest: Vec<&syn::FnArg> = m.inputs.iter().collect();
            let (found, _) = trailing(m, &out);
            rest.truncate(rest.len() - found.len());
            if found.len() != out.len() && !found.is_empty() {
                // it takes only some: the ones before can't be added in front
                continue;
            }
            let Some((Some(n), _, t)) = rest.last().and_then(|a| param(a)) else { continue };
            if !matches!(t, syn::Type::Reference(r) if r.mutability.is_some()) || out.iter().any(|(o, _)| *o == n) {
                continue;
            }
            counts.entry((n, tok(t))).or_insert((0, t.clone())).0 += 1;
        }
        let best = counts.into_iter().filter(|(_, (c, _))| enough(*c)).max_by(|a, b| a.1 .0.cmp(&b.1 .0).then_with(|| b.0.cmp(&a.0)));
        match best {
            Some(((n, _), (_, t))) => out.insert(0, (n, t)),
            None => return out,
        }
    }
}

/// What each method takes. A method used as a value takes every context of its class, so a
/// class with one that takes none of them can't have contexts at all.
fn plan_methods(c: &mut Class) {
    if c.contexts.is_empty() {
        return;
    }
    let all: Vec<String> = c.contexts.iter().map(|(n, _)| n.clone()).collect();
    let mut blocked = false;
    for m in c.methods.values_mut() {
        let (names, unused) = trailing(&m.sig, &c.contexts);
        m.takes = if names.is_empty() {
            Some(Vec::new())
        } else if !unused || (m.value && names == all) {
            Some(names)
        } else {
            None
        };
        if m.value && m.takes.as_ref().is_none_or(|t| t.is_empty()) {
            blocked = true;
        }
    }
    if blocked {
        if std::env::var_os("OXIE_FROM_RUST_DEBUG").is_some() {
            let names: Vec<String> = c.methods.iter().filter(|(_, m)| m.value && m.takes.as_ref().is_none_or(|t| t.is_empty())).map(|(n, _)| n.clone()).collect();
            eprintln!("no contexts for `{}`: {} would take them all as values", c.decl.ident, names.join(", "));
        }
        c.contexts.clear();
        for m in c.methods.values_mut() {
            m.takes = None;
        }
    }
}

#[derive(Default)]
struct Scan {
    /// Named-field structs (which become classes) by name, with their file.
    structs: HashMap<String, (usize, syn::ItemStruct)>,
    /// Names declared more than once: Oxie may pick another declaration than Rust.
    ambiguous: HashSet<String>,
    methods: HashMap<String, Vec<(usize, syn::Signature)>>,
    /// (type, method) referred to without a call.
    values: HashSet<(syn::Ident, String)>,
    struct_fields: HashMap<String, HashSet<String>>,
    value_types: HashMap<String, bool>,
}

impl Scan {
    fn types(&mut self, file: usize, items: &[syn::Item]) {
        for it in items {
            if let syn::Item::Struct(syn::ItemStruct { ident, attrs, .. }) | syn::Item::Enum(syn::ItemEnum { ident, attrs, .. }) = it {
                let copy = crate::facts::derives_of(attrs).contains("Copy");
                *self.value_types.entry(ident.to_string()).or_insert(copy) &= copy;
            }
            let name = match it {
                syn::Item::Struct(s) => {
                    if let syn::Fields::Named(n) = &s.fields {
                        let fields = self.struct_fields.entry(s.ident.to_string()).or_default();
                        fields.extend(n.named.iter().filter_map(|f| f.ident.as_ref().map(|i| i.to_string())));
                    }
                    let class = matches!(&s.fields, syn::Fields::Named(n) if !n.named.is_empty() && n.named.iter().all(|f| crate::printer::only_docs(&f.attrs)));
                    if class && !self.structs.contains_key(&s.ident.to_string()) {
                        self.structs.insert(s.ident.to_string(), (file, s.clone()));
                        continue;
                    }
                    s.ident.to_string()
                }
                syn::Item::Enum(e) => e.ident.to_string(),
                syn::Item::Trait(t) => t.ident.to_string(),
                syn::Item::Type(t) => t.ident.to_string(),
                syn::Item::Mod(m) => {
                    if let Some((_, items)) = &m.content {
                        self.types(file, items);
                    }
                    continue;
                }
                _ => continue,
            };
            self.ambiguous.insert(name);
        }
    }

    fn impls(&mut self, file: usize, items: &[syn::Item]) {
        for it in items {
            match it {
                syn::Item::Impl(imp) => {
                    let syn::Type::Path(tp) = &*imp.self_ty else { continue };
                    let Some(last) = tp.path.segments.last() else { continue };
                    let ty = last.ident.clone();
                    for ii in &imp.items {
                        if let syn::ImplItem::Fn(f) = ii {
                            let mut v = Values { owner: &ty, called: HashSet::new(), out: &mut self.values };
                            v.visit_block(&f.block);
                            if imp.trait_.is_none() {
                                self.methods.entry(ty.to_string()).or_default().push((file, f.sig.clone()));
                            }
                        }
                    }
                }
                syn::Item::Fn(f) => {
                    let owner = syn::Ident::new("Self", proc_macro2::Span::call_site());
                    let mut v = Values { owner: &owner, called: HashSet::new(), out: &mut self.values };
                    v.visit_block(&f.block);
                }
                syn::Item::Mod(m) => {
                    if let Some((_, items)) = &m.content {
                        self.impls(file, items);
                    }
                }
                _ => {}
            }
        }
    }
}

/// Collects `Self::name` and `Type::name` used as values.
struct Values<'a> {
    owner: &'a syn::Ident,
    /// Paths in call position (by address).
    called: HashSet<*const syn::ExprPath>,
    out: &'a mut HashSet<(syn::Ident, String)>,
}

impl<'ast> Visit<'ast> for Values<'_> {
    fn visit_expr_call(&mut self, c: &'ast syn::ExprCall) {
        if let syn::Expr::Path(p) = &*c.func {
            self.called.insert(p as *const _);
        }
        syn::visit::visit_expr_call(self, c);
    }

    fn visit_expr_path(&mut self, p: &'ast syn::ExprPath) {
        let segs = &p.path.segments;
        if segs.len() >= 2 && p.qself.is_none() && !self.called.contains(&(p as *const _)) {
            let owner = &segs[segs.len() - 2].ident;
            let owner = if owner == "Self" { self.owner.clone() } else { owner.clone() };
            self.out.insert((owner, segs[segs.len() - 1].ident.to_string()));
        }
    }

    fn visit_item(&mut self, _: &'ast syn::Item) {}
}

/// The names in `names` that a function body reads where nothing of its own binds them, as
/// Oxie decides which contexts a method uses. Closures that take `cx` hide the method's.
pub fn uses(block: &syn::Block, names: &[String]) -> HashSet<String> {
    let mut u = Uses { names, scopes: vec![HashSet::new()], found: HashSet::new() };
    u.visit_block(block);
    u.found
}

struct Uses<'n> {
    names: &'n [String],
    scopes: Vec<HashSet<String>>,
    found: HashSet<String>,
}

impl Uses<'_> {
    fn bind(&mut self, p: &syn::Pat) {
        struct B<'a>(&'a mut HashSet<String>);
        impl<'ast> Visit<'ast> for B<'_> {
            fn visit_pat_ident(&mut self, i: &'ast syn::PatIdent) {
                self.0.insert(i.ident.to_string());
                syn::visit::visit_pat_ident(self, i);
            }
        }
        B(self.scopes.last_mut().unwrap()).visit_pat(p);
    }

    fn read(&mut self, name: &str) {
        if self.names.iter().any(|n| n == name) && !self.scopes.iter().any(|s| s.contains(name)) {
            self.found.insert(name.to_string());
        }
    }

    fn scoped(&mut self, f: impl FnOnce(&mut Self)) {
        self.scopes.push(HashSet::new());
        f(self);
        self.scopes.pop();
    }
}

impl<'ast> Visit<'ast> for Uses<'_> {
    fn visit_block(&mut self, b: &'ast syn::Block) {
        self.scoped(|u| syn::visit::visit_block(u, b));
    }

    fn visit_local(&mut self, l: &'ast syn::Local) {
        if let Some(init) = &l.init {
            self.visit_expr(&init.expr);
            if let Some((_, e)) = &init.diverge {
                self.visit_expr(e);
            }
        }
        self.bind(&l.pat);
    }

    fn visit_expr_closure(&mut self, c: &'ast syn::ExprClosure) {
        self.scoped(|u| {
            for p in &c.inputs {
                u.bind(p);
            }
            u.visit_expr(&c.body);
        });
    }

    fn visit_arm(&mut self, a: &'ast syn::Arm) {
        self.scoped(|u| {
            u.bind(&a.pat);
            if let Some((_, g)) = &a.guard {
                u.visit_expr(g);
            }
            u.visit_expr(&a.body);
        });
    }

    fn visit_expr_for_loop(&mut self, f: &'ast syn::ExprForLoop) {
        self.visit_expr(&f.expr);
        self.scoped(|u| {
            u.bind(&f.pat);
            u.visit_block(&f.body);
        });
    }

    fn visit_expr_if(&mut self, i: &'ast syn::ExprIf) {
        self.scoped(|u| {
            u.visit_expr(&i.cond);
            u.visit_block(&i.then_branch);
        });
        if let Some((_, e)) = &i.else_branch {
            self.visit_expr(e);
        }
    }

    fn visit_expr_while(&mut self, w: &'ast syn::ExprWhile) {
        self.scoped(|u| {
            u.visit_expr(&w.cond);
            u.visit_block(&w.body);
        });
    }

    fn visit_expr_let(&mut self, l: &'ast syn::ExprLet) {
        self.visit_expr(&l.expr);
        self.bind(&l.pat);
    }

    fn visit_expr_path(&mut self, p: &'ast syn::ExprPath) {
        if p.qself.is_none() && p.path.segments.len() == 1 {
            self.read(&p.path.segments[0].ident.to_string());
        }
    }

    fn visit_macro(&mut self, m: &'ast syn::Macro) {
        fn idents(ts: proc_macro2::TokenStream, out: &mut Vec<String>) {
            for t in ts {
                match t {
                    proc_macro2::TokenTree::Ident(i) => out.push(i.to_string()),
                    proc_macro2::TokenTree::Group(g) => idents(g.stream(), out),
                    _ => {}
                }
            }
        }
        let mut v = Vec::new();
        idents(m.tokens.clone(), &mut v);
        for n in v {
            self.read(&n);
        }
    }

    // a local `fn` can't see the method's variables
    fn visit_item(&mut self, _: &'ast syn::Item) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    fn class(src: &str, name: &str) -> Class {
        let f = syn::parse_file(src).unwrap();
        scan(&[&f]).classes.remove(name).unwrap()
    }

    #[test]
    fn picks_the_parameters_most_methods_end_with() {
        let c = class(
            "struct W { n: i32 }
impl W {
    fn a(&mut self, x: i32, window: &mut Window, cx: &mut Context<Self>) {}
    fn b(&mut self, window: &mut Window, cx: &mut Context<Self>) {}
    fn c(&mut self, cx: &mut Context<Self>) {}
    fn d(&self, cx: &mut App) {}
    fn e(&self, _window: &mut Window, cx: &mut Context<Self>) {}
    fn f(&self) -> i32 { self.n }
}",
            "W",
        );
        let names: Vec<&str> = c.contexts.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, ["window", "cx"]);
        let takes = |m: &str| c.methods[m].takes.clone();
        assert_eq!(takes("a"), Some(vec!["window".to_string(), "cx".to_string()]));
        assert_eq!(takes("c"), Some(vec!["cx".to_string()]));
        assert_eq!(takes("d"), Some(vec![]));
        assert_eq!(takes("e"), None);
        assert_eq!(takes("f"), Some(vec![]));
    }

    #[test]
    fn a_handler_without_them_keeps_the_class_without_contexts() {
        let src = "struct W { n: i32 }
impl W {
    fn a(&mut self, cx: &mut App) {}
    fn b(&mut self, cx: &mut App) {}
    fn c(&mut self, cx: &mut App) { cx.on_click(Self::click) }
    pub fn n(&self) -> i32 { self.n }
    fn click(&mut self, e: &Click) {}
}";
        assert!(class(src, "W").contexts.is_empty());
        // without the handler, `pub fn n` takes none, as Oxie infers
        let c = class(&src.replace("cx.on_click(Self::click)", ""), "W");
        assert_eq!(c.contexts.len(), 1);
        assert_eq!(c.methods["n"].takes, Some(vec![]));
    }

    #[test]
    fn closures_hide_the_contexts_they_take() {
        let f: syn::ItemFn = syn::parse_str("fn f() { a(cx); x.update(|this, cx| b(cx)); let window = w(); c(window); format!(\"{}\", y); }").unwrap();
        let names = ["window".to_string(), "cx".to_string(), "y".to_string()];
        let mut got: Vec<String> = uses(&f.block, &names).into_iter().collect();
        got.sort();
        assert_eq!(got, ["cx", "y"]);
    }
}
