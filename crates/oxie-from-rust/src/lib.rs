//! Translates Rust source code into idiomatic Oxie.
//!
//! The translation inverts what the Oxie compiler does: `let mut` → `var`, structs with their
//! `impl` blocks → classes, `Result` returns → `throws`, `format!` → string templates, `match`
//! → `when`, closures → lambdas, and so on. Constructs without an Oxie equivalent are kept as
//! Rust, which Oxie accepts directly or inside `rust { }` blocks.
//!
//! Every translation is checked by compiling it back to Rust with the Oxie compiler: method
//! receivers and derives that Oxie would infer differently are written out explicitly, and
//! items Oxie rejects are kept as Rust.
//!
//! Code that passes the same values down most calls (GPUI's `window` and `cx`) gets context
//! parameters; [`translate_crate`] decides them for all the files of a crate at once, since a
//! class's methods may be spread over many files (see `contexts`).

mod comments;
mod contexts;
mod expr;
mod facts;
mod fmtstr;
mod oxie_aux;
mod printer;
mod types;

use std::collections::{HashMap, HashSet};

use printer::{Fixes, Info, Printer};

/// The result of [`translate`].
#[derive(Debug, Clone)]
pub struct Translation {
    /// Oxie source text.
    pub oxie: String,
    /// Things the user should look at (parts kept as Rust, Oxie errors).
    pub warnings: Vec<String>,
}

/// Translates a Rust source file into Oxie.
pub fn translate(src: &str) -> Result<Translation, String> {
    translate_crate(&[src]).pop().unwrap()
}

/// Translates the Rust files of one crate into Oxie, one translation per file. The classes
/// get their contexts from all the files: a method in another file's `impl` block drops the
/// same parameters as one declared next to the class.
pub fn translate_crate(srcs: &[&str]) -> Vec<Result<Translation, String>> {
    translate_crate_as(srcs, false)
}

/// `translate_crate` for a library (`library`), whose public methods that change their object
/// say `mut fun`, or a program.
pub fn translate_crate_as(srcs: &[&str], library: bool) -> Vec<Result<Translation, String>> {
    let parsed: Vec<Result<syn::File, String>> = srcs
        .iter()
        .map(|src| {
            let mut file = syn::parse_file(src).map_err(|e| {
                let s = e.span().start();
                format!("{}:{}: {}", s.line, s.column + 1, e)
            })?;
            oxie_aux::preprocess(&mut file);
            Ok(file)
        })
        .collect();
    let empty = syn::File { shebang: None, attrs: Vec::new(), items: Vec::new() };
    let files: Vec<&syn::File> = parsed.iter().map(|p| p.as_ref().unwrap_or(&empty)).collect();
    let mut krate = contexts::scan(&files);
    krate.library = library;
    let mut out = Vec::new();
    // a method kept as Rust takes its contexts as written, which the files that call it must
    // know: translate again until nothing more is kept
    for _pass in 0..8 {
        out.clear();
        let mut kept = Vec::new();
        for (i, (src, file)) in srcs.iter().zip(&parsed).enumerate() {
            match file {
                Ok(file) => {
                    let (t, k) = translate_file(src, file, &krate, i);
                    out.push(Ok(t));
                    kept.extend(k);
                }
                Err(e) => out.push(Err(e.clone())),
            }
        }
        let mut changed = false;
        for k in &kept {
            changed |= k.apply(&mut krate);
        }
        if !changed {
            break;
        }
    }
    out
}

/// A part of the crate's classes that a file kept as Rust.
#[derive(Clone, Debug)]
enum Kept {
    Class(String),
    Method(String, String),
}

impl Kept {
    /// Whether this changes what `krate` gives contexts to.
    fn apply(&self, krate: &mut contexts::Crate) -> bool {
        match self {
            Kept::Class(c) => krate.drop_class(c),
            Kept::Method(c, m) => krate.keep_method(c, m),
        }
    }
}

/// The classes and methods of `file` that `fixes` keep as Rust.
fn kept_as_rust(file: &syn::File, fixes: &Fixes) -> Vec<Kept> {
    let methods = |imp: &syn::ItemImpl, only: Option<usize>| -> Vec<Kept> {
        if imp.trait_.is_some() {
            return Vec::new();
        }
        let ty = facts::type_name(&imp.self_ty);
        imp.items
            .iter()
            .enumerate()
            .filter(|(j, _)| only.is_none_or(|o| o == *j))
            .filter_map(|(_, ii)| if let syn::ImplItem::Fn(f) = ii { Some(Kept::Method(ty.clone(), f.sig.ident.to_string())) } else { None })
            .collect()
    };
    let mut out = Vec::new();
    for &i in fixes.raw_items.keys() {
        match file.items.get(i) {
            Some(syn::Item::Struct(s)) => out.push(Kept::Class(s.ident.to_string())),
            Some(syn::Item::Impl(imp)) => out.extend(methods(imp, None)),
            _ => {}
        }
    }
    for &(i, j) in fixes.raw_methods.keys() {
        if let Some(syn::Item::Impl(imp)) = file.items.get(i) {
            out.extend(methods(imp, Some(j)));
        }
    }
    out
}

/// Translates file `idx` of a crate; also returns what it kept as Rust of the crate's classes
/// with contexts.
fn translate_file(src: &str, file: &syn::File, krate: &contexts::Crate, idx: usize) -> (Translation, Vec<Kept>) {
    let info = collect_info(file);
    let orig = facts::collect(file);
    let mut fixes = Fixes::default();
    let mut warnings = Vec::new();
    let mut text = String::new();
    let mut kept = Vec::new();
    let mentioned = idents(src);
    // a rejected method of an inherent impl is kept as Rust on its own, so this can take more
    // rounds than one per item
    for _round in 0..24 {
        // what this file keeps as Rust takes its contexts as written
        let mut local = krate.clone();
        kept = kept_as_rust(file, &fixes).into_iter().filter(|k| k.apply(&mut local)).collect();
        let (t, ranges, methods, todos) = print(src, file, &info, &fixes, &local);
        text = t;
        let stub = local.stub(idx, &mentioned, &Printer::new(info.clone(), &fixes, &local, Vec::new()));
        let (rust, map, diags) = compile(&text, &stub);
        let errors: Vec<&oxie_compiler::Diag> = diags.iter().filter(|d| d.severity == oxie_compiler::Severity::Error).collect();
        if !errors.is_empty() {
            if std::env::var_os("OXIE_FROM_RUST_DEBUG").is_some() {
                let idx = oxie_compiler::LineIndex::new(&text);
                eprintln!("--- translation\n{}", text);
                for d in &errors {
                    let (l, c) = idx.line_col(&text, d.span.start as usize);
                    eprintln!("oxie error {}:{}: {}", l + 1, c + 1, d.message);
                }
            }
            // a type that lost a derive Oxie didn't infer (`Copy`, which decides whether its
            // parameters are lent) writes its derives out first: that may be what the errors are
            if syn::parse_file(&rust).is_ok_and(|gen| lost_derives(&orig, &facts::collect(&gen), &mut fixes)) {
                continue;
            }
            let mut changed = false;
            for d in &errors {
                changed |= keep_as_rust(&mut fixes, &ranges, &methods, d.span.start as usize, &format!("Oxie rejected the translation ({})", d.message));
            }
            if changed {
                continue;
            }
            let idx = oxie_compiler::LineIndex::new(&text);
            for d in errors {
                let (l, c) = idx.line_col(&text, d.span.start as usize);
                warnings.push(format!("the translation does not compile with Oxie: {}:{}: {}", l + 1, c + 1, d.message));
            }
            break;
        }
        let gen = match syn::parse_file(&rust) {
            Ok(g) => g,
            Err(e) => {
                if std::env::var_os("OXIE_FROM_RUST_DEBUG").is_some() {
                    eprintln!("--- generated Rust does not parse: {}\n{}", e, rust);
                }
                // the Oxie compiler produced invalid Rust for some item: keep that item as Rust
                let at = e.span().start();
                let src_pos = map.as_ref().and_then(|m| m.lookup(at.line.saturating_sub(1) as u32, at.column as u32));
                if let Some((l, c)) = src_pos {
                    let off = oxie_compiler::LineIndex::new(&text).offset(&text, l as usize, c as usize);
                    if keep_as_rust(&mut fixes, &ranges, &methods, off, "the Oxie compiler produced invalid Rust for the translation") {
                        continue;
                    }
                }
                warnings.push(format!("the translation compiles to invalid Rust: {}", e));
                break;
            }
        };
        let got = facts::collect(&gen);
        let mut changed = compare_contexts(&orig, &got, &local, &mut fixes);
        for (k, r) in &orig.receivers {
            if let Some(g) = got.receivers.get(k) {
                if g != r {
                    changed |= fixes.explicit_recv.insert(k.clone());
                }
            }
        }
        changed |= lost_derives(&orig, &got, &mut fixes);
        if !changed {
            if !todos.is_empty() {
                let mut seen = HashSet::new();
                for t in todos {
                    if seen.insert(t.clone()) {
                        warnings.push(t);
                    }
                }
            }
            break;
        }
    }
    (Translation { oxie: drop_unused_module_imports(&text), warnings }, kept)
}

/// Marks the types whose derives come out of Oxie unlike the Rust's to write theirs out; true if
/// there are new ones.
fn lost_derives(orig: &facts::Facts, got: &facts::Facts, fixes: &mut Fixes) -> bool {
    let mut changed = false;
    for (k, d) in &orig.derives {
        if got.derives.get(k).is_some_and(|g| g != d) {
            changed |= fixes.explicit_derives.insert(k.clone());
        }
    }
    changed
}

/// Methods whose parameters and calls whose arguments come out of Oxie unlike the Rust's,
/// because Oxie gave a method other contexts than planned or didn't follow a call: the method
/// gets a `context(...)` of its own, and the call passes its contexts as written.
fn compare_contexts(orig: &facts::Facts, got: &facts::Facts, krate: &contexts::Crate, fixes: &mut Fixes) -> bool {
    let mut changed = false;
    let class_of = |ty: &str| ty.split('<').next().unwrap_or("").rsplit("::").next().unwrap_or("").to_string();
    for (k, ps) in &orig.params {
        if got.params.get(k).is_some_and(|g| !same_params(g, ps)) && k.1.is_empty() && krate.takes(&class_of(&k.0), &k.2).is_some() {
            changed |= fixes.ctx_clause.insert(k.clone());
        }
    }
    let watched: HashSet<&str> = krate.classes.values().filter(|c| !c.contexts.is_empty()).flat_map(|c| c.methods.iter()).filter(|(_, m)| m.takes.as_ref().is_some_and(|t| !t.is_empty())).map(|(n, _)| n.as_str()).collect();
    for (f, calls) in &orig.calls {
        for (name, counts) in calls {
            if watched.contains(name.as_str()) && got.calls.get(f).and_then(|c| c.get(name)) != Some(counts) {
                changed |= fixes.ctx_calls.insert((f.clone(), name.clone()));
            }
        }
    }
    changed
}

/// Are the parameter types Oxie gave a method (`got`, as `facts` writes them) the Rust's
/// (`orig`), up to the borrowing of those its body only reads (`String` as `&str`, `Vec<T>` as
/// `&[T]`, `PathBuf` as `&Path`, `Box<T>` and other values as `&T`)?
fn same_params(got: &[String], orig: &[String]) -> bool {
    let lent = |g: &str, o: &str| {
        let Some(inner) = g.strip_prefix('&') else { return false };
        inner == o
            || (o == "String" && inner == "str")
            || (o == "PathBuf" && inner == "Path")
            || o.strip_prefix("Vec<").and_then(|t| t.strip_suffix('>')).is_some_and(|t| inner == format!("[{}]", t))
            || o.strip_prefix("Box<").and_then(|t| t.strip_suffix('>')).is_some_and(|t| inner == t)
    };
    got.len() == orig.len() && got.iter().zip(orig).all(|(g, o)| g == o || lent(g, o))
}

/// The identifiers in a source text.
fn idents(src: &str) -> HashSet<String> {
    src.split(|c: char| !(c.is_alphanumeric() || c == '_')).filter(|w| w.chars().next().is_some_and(|c| c.is_alphabetic() || c == '_')).map(String::from).collect()
}

/// Removes `import std::fmt` and similar lines whose (lowercase, so not a trait) name is no
/// longer used, e.g. after `impl fmt::Display` became `toString`.
fn drop_unused_module_imports(text: &str) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let mut keep = vec![true; lines.len()];
    for (i, l) in lines.iter().enumerate() {
        let Some(path) = l.strip_prefix("import ") else { continue };
        if path.contains(['{', '*', ' ']) {
            continue;
        }
        let name = path.rsplit("::").next().unwrap_or(path);
        if !name.chars().next().is_some_and(|c| c.is_lowercase()) {
            continue;
        }
        let used = lines.iter().enumerate().any(|(j, other)| j != i && contains_word(other, name));
        if !used {
            keep[i] = false;
        }
    }
    let mut out: Vec<&str> = Vec::new();
    for (i, l) in lines.iter().enumerate() {
        if keep[i] {
            // no double blank line where an import was removed
            if l.is_empty() && out.last().is_some_and(|p| p.is_empty()) {
                continue;
            }
            out.push(l);
        }
    }
    while out.first().is_some_and(|l| l.is_empty()) {
        out.remove(0);
    }
    let mut s = out.join("\n");
    s.push('\n');
    s
}

fn contains_word(hay: &str, w: &str) -> bool {
    let b = hay.as_bytes();
    let mut start = 0;
    while let Some(i) = hay[start..].find(w) {
        let at = start + i;
        let before = at == 0 || !(b[at - 1].is_ascii_alphanumeric() || b[at - 1] == b'_');
        let end = at + w.len();
        let after = end >= b.len() || !(b[end].is_ascii_alphanumeric() || b[end] == b'_');
        if before && after {
            return true;
        }
        start = at + w.len();
    }
    false
}

type Ranges = Vec<(usize, std::ops::Range<usize>)>;
type MethodRanges = Vec<((usize, usize), std::ops::Range<usize>)>;

/// Keeps the code at byte `pos` of the translation as Rust from now on: the method of an
/// inherent impl it is in, or else the top-level item. False when that is Rust already.
fn keep_as_rust(fixes: &mut Fixes, items: &Ranges, methods: &MethodRanges, pos: usize, reason: &str) -> bool {
    if let Some((k, _)) = methods.iter().find(|(_, r)| r.contains(&pos)) {
        // another error of a method this round already keeps as Rust
        if fixes.raw_methods.contains_key(k) {
            return false;
        }
        fixes.raw_methods.insert(*k, reason.to_string());
        return true;
    }
    match items.iter().find(|(_, r)| r.contains(&pos)) {
        Some((i, _)) if !fixes.raw_items.contains_key(i) => {
            fixes.raw_items.insert(*i, reason.to_string());
            true
        }
        _ => false,
    }
}

/// Compiles Oxie text as a single-file script, with `stub` (the contexts of the crate's other
/// files, see `contexts`) as a second file: the generated Rust, its source map, diagnostics.
fn compile(text: &str, stub: &str) -> (String, Option<oxie_compiler::SourceMap>, Vec<oxie_compiler::Diag>) {
    let main = std::path::PathBuf::from("main.oxi");
    let mut inputs = vec![oxie_compiler::Input { rel_path: main.clone(), abs_path: main.clone(), text: text.to_string() }];
    if !stub.is_empty() {
        let path = std::path::PathBuf::from("__contexts.oxi");
        inputs.push(oxie_compiler::Input { rel_path: path.clone(), abs_path: path, text: stub.to_string() });
    }
    let opts = oxie_compiler::Options { crate_name: "main".into(), extern_crates: HashSet::new(), out_dir: "/oxie".into(), script: true, root_attrs: Vec::new() };
    let out = oxie_compiler::compile(inputs, &opts);
    let diags = out.diags.into_iter().find(|d| d.0 == main).map(|d| d.2).unwrap_or_default();
    match out.files.into_iter().find(|f| f.rel_path.file_stem() == main.file_stem()) {
        Some(f) => (f.text, f.map, diags),
        None => (String::new(), None, diags),
    }
}

fn print(src: &str, file: &syn::File, info: &Info, fixes: &Fixes, krate: &contexts::Crate) -> (String, Ranges, MethodRanges, Vec<String>) {
    let mut p = Printer::new(info.clone(), fixes, krate, comments::scan(src));
    let text = p.file(file);
    (text, p.item_ranges, p.method_ranges, p.todos)
}

fn collect_info(file: &syn::File) -> Info {
    let mut info = Info::default();
    let mut fn_counts: HashMap<String, usize> = HashMap::new();
    collect_items(&file.items, &mut info, &mut fn_counts);
    for (n, c) in fn_counts {
        if c > 1 {
            info.fns.remove(&n);
        }
    }
    info.std_collections = std_collections(&file.items);
    outside_imports(&file.items, "", &mut info.outside_imports);
    info
}

/// The names each module imports from another crate (see `Info::outside_imports`).
fn outside_imports(items: &[syn::Item], prefix: &str, out: &mut HashSet<(String, String)>) {
    fn walk(t: &syn::UseTree, first: Option<&str>, prefix: &str, out: &mut HashSet<(String, String)>) {
        let outside = |f: Option<&str>| f.is_some_and(|f| !matches!(f, "crate" | "self" | "super"));
        match t {
            syn::UseTree::Path(p) => {
                let f = first.or(Some(&*p.ident.to_string())).map(str::to_string);
                walk(&p.tree, f.as_deref(), prefix, out);
            }
            syn::UseTree::Name(n) if outside(first) => {
                out.insert((prefix.to_string(), n.ident.to_string()));
            }
            syn::UseTree::Rename(r) if outside(first) => {
                out.insert((prefix.to_string(), r.rename.to_string()));
            }
            syn::UseTree::Group(g) => {
                for t in &g.items {
                    walk(t, first, prefix, out);
                }
            }
            _ => {}
        }
    }
    for it in items {
        match it {
            syn::Item::Use(u) => walk(&u.tree, None, prefix, out),
            syn::Item::Mod(m) => {
                if let Some((_, inner)) = &m.content {
                    outside_imports(inner, &format!("{}{}::", prefix, m.ident), out);
                }
            }
            _ => {}
        }
    }
}

/// The std collections the items import by their own names: `use std::collections::HashMap`,
/// `use std::collections::{HashMap, HashSet}`, `use std::collections::*`.
fn std_collections(items: &[syn::Item]) -> HashSet<String> {
    fn walk(t: &syn::UseTree, path: &mut Vec<String>, out: &mut HashSet<String>) {
        let in_std = |p: &[String]| p.len() >= 2 && p[0] == "std" && p[1] == "collections";
        match t {
            syn::UseTree::Path(p) => {
                path.push(p.ident.to_string());
                walk(&p.tree, path, out);
                path.pop();
            }
            syn::UseTree::Name(n) if in_std(path) => {
                out.insert(n.ident.to_string());
            }
            syn::UseTree::Glob(_) if path.len() == 2 && in_std(path) => {
                out.extend(["HashMap", "HashSet", "BTreeMap", "BTreeSet", "VecDeque", "BinaryHeap", "LinkedList"].map(String::from));
            }
            syn::UseTree::Group(g) => {
                for t in &g.items {
                    walk(t, path, out);
                }
            }
            _ => {}
        }
    }
    let mut out = HashSet::new();
    for it in items {
        if let syn::Item::Use(u) = it {
            walk(&u.tree, &mut Vec::new(), &mut out);
        }
    }
    out
}

fn collect_items(items: &[syn::Item], info: &mut Info, fn_counts: &mut HashMap<String, usize>) {
    // `new` functions of each type
    let mut news: HashMap<String, Vec<syn::Type>> = HashMap::new();
    for it in items {
        if let syn::Item::Impl(imp) = it {
            let fns = info.type_fns.entry(facts::type_name(&imp.self_ty)).or_default();
            fns.extend(imp.items.iter().filter_map(|ii| if let syn::ImplItem::Fn(f) = ii { Some(f.sig.ident.to_string()) } else { None }));
            if imp.trait_.is_some() {
                for ii in &imp.items {
                    if let syn::ImplItem::Fn(f) = ii {
                        if printer::result_parts_of(&f.sig.output) {
                            info.throwing.insert(f.sig.ident.to_string());
                        }
                        if f.sig.ident == "new" && f.sig.receiver().is_none() {
                            info.trait_new.insert(facts::type_name(&imp.self_ty));
                        }
                    }
                }
                continue;
            }
            let syn::Type::Path(tp) = &*imp.self_ty else { continue };
            let Some(last) = tp.path.segments.last() else { continue };
            for ii in &imp.items {
                if let syn::ImplItem::Fn(f) = ii {
                    if printer::result_parts_of(&f.sig.output) {
                        info.throwing.insert(f.sig.ident.to_string());
                    }
                    if f.sig.ident == "new" && f.sig.receiver().is_none() {
                        let ps: Vec<syn::Type> = f
                            .sig
                            .inputs
                            .iter()
                            .filter_map(|a| match a {
                                syn::FnArg::Typed(pt) => Some((*pt.ty).clone()),
                                _ => None,
                            })
                            .collect();
                        news.insert(last.ident.to_string(), ps);
                    }
                }
            }
        }
    }
    for it in items {
        match it {
            syn::Item::Struct(s) => {
                let fields: Vec<(String, syn::Type)> = match &s.fields {
                    syn::Fields::Named(n) => n.named.iter().map(|f| (f.ident.as_ref().unwrap().to_string(), f.ty.clone())).collect(),
                    _ => Vec::new(),
                };
                let class = matches!(&s.fields, syn::Fields::Named(n) if !n.named.is_empty() && n.named.iter().all(|f| printer::only_docs(&f.attrs)));
                let named = matches!(&s.fields, syn::Fields::Named(_));
                let new = news.get(&s.ident.to_string()).cloned();
                let has_new = named && (new.is_some() || class) && !info.trait_new.contains(&s.ident.to_string());
                let new_params = new.unwrap_or_else(|| fields.iter().map(|(_, t)| t.clone()).collect());
                let copy = facts::derives_of(&s.attrs).contains("Copy");
                info.structs.insert(s.ident.to_string(), printer::StructInfo { fields, has_new, new_params, copy });
            }
            syn::Item::Enum(e) => {
                let mut ei = printer::EnumInfo { copy: facts::derives_of(&e.attrs).contains("Copy"), ..Default::default() };
                for v in &e.variants {
                    if let syn::Fields::Named(n) = &v.fields {
                        ei.named.insert(v.ident.to_string(), n.named.iter().map(|f| f.ident.as_ref().unwrap().to_string()).collect());
                    }
                }
                info.enums.insert(e.ident.to_string(), ei);
            }
            syn::Item::Trait(t) => {
                info.traits.insert(t.ident.to_string());
                let fns = info.type_fns.entry(format!("trait {}", t.ident)).or_default();
                fns.extend(t.items.iter().filter_map(|ti| if let syn::TraitItem::Fn(f) = ti { Some(f.sig.ident.to_string()) } else { None }));
                for ti in &t.items {
                    if let syn::TraitItem::Fn(f) = ti {
                        if printer::result_parts_of(&f.sig.output) {
                            info.throwing.insert(f.sig.ident.to_string());
                        }
                    }
                }
            }
            syn::Item::Fn(f) => {
                let ps: Vec<syn::Type> = f
                    .sig
                    .inputs
                    .iter()
                    .filter_map(|a| match a {
                        syn::FnArg::Typed(pt) => Some((*pt.ty).clone()),
                        _ => None,
                    })
                    .collect();
                *fn_counts.entry(f.sig.ident.to_string()).or_default() += 1;
                info.fns.insert(f.sig.ident.to_string(), ps);
                info.free_fns.insert(f.sig.ident.to_string());
                if let syn::ReturnType::Type(_, t) = &f.sig.output {
                    if matches!(&**t, syn::Type::Path(p) if p.path.segments.last().is_some_and(|s| s.ident == "Option")) {
                        info.option_fns.insert(f.sig.ident.to_string());
                    }
                }
                if f.sig.ident == "run" {
                    info.run_shadowed = true;
                }
                if printer::result_parts_of(&f.sig.output) {
                    info.throwing.insert(f.sig.ident.to_string());
                }
            }
            syn::Item::Mod(m) => {
                info.modules.insert(m.ident.to_string());
                if let Some((_, items)) = &m.content {
                    collect_items(items, info, fn_counts);
                }
            }
            syn::Item::Use(u) => collect_use(&u.tree, info),
            _ => {}
        }
    }
}

fn collect_use(t: &syn::UseTree, info: &mut Info) {
    match t {
        // `use std::fs::{self, File}` imports `fs` too, but Oxie resolves `fs.f()` only for
        // modules imported by name, so those keep `fs::f()`
        syn::UseTree::Path(p) => collect_use(&p.tree, info),
        syn::UseTree::Name(n) => {
            let s = n.ident.to_string();
            if s == "self" {
                return;
            }
            if s.chars().next().is_some_and(|c| c.is_lowercase()) {
                info.modules.insert(s);
            }
        }
        syn::UseTree::Rename(r) => {
            let s = r.rename.to_string();
            if s.chars().next().is_some_and(|c| c.is_lowercase()) {
                info.modules.insert(s);
            }
        }
        syn::UseTree::Group(g) => {
            for i in &g.items {
                collect_use(i, info);
            }
        }
        syn::UseTree::Glob(_) => {}
    }
}
