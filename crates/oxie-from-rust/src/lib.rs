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

mod comments;
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
    let mut file = syn::parse_file(src).map_err(|e| {
        let s = e.span().start();
        format!("{}:{}: {}", s.line, s.column + 1, e)
    })?;
    oxie_aux::preprocess(&mut file);
    let info = collect_info(&file);
    let orig = facts::collect(&file);
    let mut fixes = Fixes::default();
    let mut warnings = Vec::new();
    let mut text = String::new();
    // a rejected method of an inherent impl is kept as Rust on its own, so this can take more
    // rounds than one per item
    for _round in 0..16 {
        let (t, ranges, methods, todos) = print(src, &file, &info, &fixes);
        text = t;
        let (rust, map, diags) = compile(&text);
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
        let mut changed = false;
        for (k, r) in &orig.receivers {
            if let Some(g) = got.receivers.get(k) {
                if g != r {
                    changed |= fixes.explicit_recv.insert(k.clone());
                }
            }
        }
        for (k, d) in &orig.derives {
            if let Some(g) = got.derives.get(k) {
                if g != d {
                    changed |= fixes.explicit_derives.insert(k.clone());
                }
            }
        }
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
    Ok(Translation { oxie: drop_unused_module_imports(&text), warnings })
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

/// Compiles Oxie text as a single-file script: generated Rust, its source map, diagnostics.
fn compile(text: &str) -> (String, Option<oxie_compiler::SourceMap>, Vec<oxie_compiler::Diag>) {
    let input = oxie_compiler::Input { rel_path: "main.oxi".into(), abs_path: "main.oxi".into(), text: text.to_string() };
    let opts = oxie_compiler::Options { crate_name: "main".into(), extern_crates: HashSet::new(), out_dir: "/oxie".into(), script: true };
    let out = oxie_compiler::compile(vec![input], &opts);
    let diags = out.diags.into_iter().next().map(|d| d.2).unwrap_or_default();
    match out.files.into_iter().next() {
        Some(f) => (f.text, f.map, diags),
        None => (String::new(), None, diags),
    }
}

fn print(src: &str, file: &syn::File, info: &Info, fixes: &Fixes) -> (String, Ranges, MethodRanges, Vec<String>) {
    let mut p = Printer::new(info.clone(), fixes, comments::scan(src));
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
    info
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
                let has_new = named && (new.is_some() || class);
                let new_params = new.unwrap_or_else(|| fields.iter().map(|(_, t)| t.clone()).collect());
                info.structs.insert(s.ident.to_string(), printer::StructInfo { fields, has_new, new_params });
            }
            syn::Item::Enum(e) => {
                let mut ei = printer::EnumInfo::default();
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
