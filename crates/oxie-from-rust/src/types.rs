//! Types, generics, attributes and raw token printing.

use proc_macro2::{Delimiter, Spacing, TokenStream, TokenTree};
use quote::ToTokens;

use crate::printer::Printer;

/// Words that are reserved in Oxie but not in Rust; Rust identifiers spelled like them need `r#`.
const OXIE_RESERVED: &[&str] = &["fun", "val", "var", "when", "throw", "catch", "class", "interface", "import", "typealias", "null", "this", "is"];

pub fn ident(s: &str) -> String {
    if OXIE_RESERVED.contains(&s) {
        format!("r#{}", s)
    } else {
        s.to_string()
    }
}

pub fn id(i: &syn::Ident) -> String {
    let s = i.to_string();
    if s.starts_with("r#") {
        s
    } else {
        ident(&s)
    }
}

/// Prints Rust tokens with conventional spacing.
pub fn tokens(ts: &TokenStream) -> String {
    let mut out = String::new();
    let tts: Vec<TokenTree> = ts.clone().into_iter().collect();
    let mut prev: Option<TokenTree> = None;
    let mut prev_joint = false;
    let mut prev_colon2 = false;
    for (i, tt) in tts.iter().enumerate() {
        let s = match tt {
            TokenTree::Group(g) => {
                let inner = tokens(&g.stream());
                match g.delimiter() {
                    Delimiter::Parenthesis => format!("({})", inner),
                    Delimiter::Bracket => format!("[{}]", inner),
                    Delimiter::Brace => {
                        if inner.is_empty() {
                            "{}".to_string()
                        } else {
                            format!("{{ {} }}", inner)
                        }
                    }
                    Delimiter::None => inner,
                }
            }
            other => other.to_string(),
        };
        if let Some(p) = &prev {
            if !prev_colon2 && needs_space(p, tt, prev_joint, tts.get(i + 1)) {
                out.push(' ');
            }
        }
        prev_colon2 = prev_joint && matches!((&prev, tt), (Some(TokenTree::Punct(a)), TokenTree::Punct(b)) if a.as_char() == ':' && b.as_char() == ':');
        prev_joint = matches!(tt, TokenTree::Punct(p) if p.spacing() == Spacing::Joint);
        out.push_str(&s);
        prev = Some(tt.clone());
    }
    out
}

fn needs_space(prev: &TokenTree, cur: &TokenTree, prev_joint: bool, _next: Option<&TokenTree>) -> bool {
    if prev_joint {
        // `::`, `->`, `'a` (lifetime quote is a joint punct), `..=`
        return false;
    }
    match (prev, cur) {
        (_, TokenTree::Punct(p)) if matches!(p.as_char(), ',' | ';' | '.' | '?') => false,
        (_, TokenTree::Punct(p)) if p.as_char() == ':' && p.spacing() == Spacing::Joint => false,
        (TokenTree::Punct(p), _) if matches!(p.as_char(), '.' | '#' | '&' | '$') => false,
        (TokenTree::Punct(p), TokenTree::Literal(_)) if p.as_char() == '-' => false,
        (TokenTree::Punct(p), _) if p.as_char() == ':' => true,
        (TokenTree::Punct(p), TokenTree::Group(_)) if p.as_char() == '!' => false,
        (TokenTree::Ident(_), TokenTree::Group(g)) if g.delimiter() != Delimiter::Brace => false,
        (TokenTree::Ident(_), TokenTree::Punct(p)) if p.as_char() == '!' => false,
        (TokenTree::Punct(p), TokenTree::Ident(_)) if p.as_char() == '!' => false,
        (TokenTree::Punct(p), _) if p.as_char() == '<' => false,
        (_, TokenTree::Punct(p)) if matches!(p.as_char(), '<' | '>') => false,
        (TokenTree::Punct(p), _) if p.as_char() == '>' => !matches!(cur, TokenTree::Group(g) if g.delimiter() == Delimiter::Parenthesis),
        (TokenTree::Group(_), TokenTree::Group(g)) if g.delimiter() != Delimiter::Brace => false,
        _ => true,
    }
}

pub fn tok<T: ToTokens>(t: &T) -> String {
    tokens(&t.to_token_stream())
}

impl Printer<'_> {
    pub fn ty(&self, t: &syn::Type) -> String {
        // where Oxie reads `[T]` as a slice, a `Vec<T>` keeps its name
        let slice = self.slice_here.take();
        match t {
            syn::Type::Path(p) => {
                if p.qself.is_none() {
                    if let Some(inner) = option_inner(t) {
                        if optionable(inner) {
                            return format!("{}?", self.ty(inner));
                        }
                    }
                    if is_any_error(t) {
                        return "AnyError".into();
                    }
                    // `[T]` for a list, `[K: V]` for std's map
                    if let Some(elem) = self.list_elem(&p.path).filter(|_| !slice) {
                        return format!("[{}]", self.ty(elem));
                    }
                    if let Some((k, v)) = self.std_map(&p.path) {
                        return format!("[{}: {}]", self.ty(k), self.ty(v));
                    }
                }
                self.type_path(p.qself.as_ref(), &p.path)
            }
            syn::Type::Reference(r) => {
                let mut s = String::from("&");
                if let Some(l) = &r.lifetime {
                    s.push_str(&l.to_string());
                    s.push(' ');
                }
                if r.mutability.is_some() {
                    s.push_str("mut ");
                }
                // `&Option<T>` must stay as written: `&T?` means `Option<&T>` in Oxie.
                if option_inner(&r.elem).is_some() {
                    if let syn::Type::Path(p) = &*r.elem {
                        s.push_str(&self.type_path(None, &p.path));
                        return s;
                    }
                }
                self.slice_here.set(true);
                s.push_str(&self.ty(&r.elem));
                s
            }
            // where Oxie reads `[T]` as a list, Rust's slice is `slice<T>`
            syn::Type::Slice(s) if !slice => format!("slice<{}>", self.ty(&s.elem)),
            syn::Type::Slice(s) => format!("[{}]", self.ty(&s.elem)),
            syn::Type::Array(a) => format!("[{}; {}]", self.ty(&a.elem), self.rust_expr_inline(&a.len)),
            syn::Type::Tuple(t) => {
                let v: Vec<String> = t.elems.iter().map(|e| self.ty(e)).collect();
                if v.len() == 1 {
                    format!("({},)", v[0])
                } else {
                    format!("({})", v.join(", "))
                }
            }
            syn::Type::Ptr(p) => {
                self.slice_here.set(true);
                format!("*{} {}", if p.mutability.is_some() { "mut" } else { "const" }, self.ty(&p.elem))
            }
            syn::Type::ImplTrait(i) => format!("impl {}", self.bounds(&i.bounds)),
            syn::Type::TraitObject(o) => format!("dyn {}", self.bounds(&o.bounds)),
            syn::Type::Paren(p) => {
                self.slice_here.set(slice);
                format!("({})", self.ty(&p.elem))
            }
            syn::Type::Group(g) => {
                self.slice_here.set(slice);
                self.ty(&g.elem)
            }
            syn::Type::Never(_) => "!".into(),
            syn::Type::Infer(_) => "_".into(),
            syn::Type::BareFn(f) => {
                let mut s = String::new();
                if let Some(l) = &f.lifetimes {
                    s.push_str(&tok(l));
                    s.push(' ');
                }
                if f.unsafety.is_some() {
                    s.push_str("unsafe ");
                }
                if let Some(abi) = &f.abi {
                    s.push_str(&tok(abi));
                    s.push(' ');
                }
                let args: Vec<String> = f
                    .inputs
                    .iter()
                    .map(|a| match &a.name {
                        Some((n, _)) => format!("{}: {}", n, self.ty(&a.ty)),
                        None => self.ty(&a.ty),
                    })
                    .collect();
                s.push_str(&format!("fn({})", args.join(", ")));
                if let syn::ReturnType::Type(_, r) = &f.output {
                    s.push_str(&format!(" -> {}", self.rust_ty(r)));
                }
                s
            }
            other => tok(other),
        }
    }

    /// A type in a position where Oxie's sugar must not apply (inside `fn(..) -> T`).
    pub fn rust_ty(&self, t: &syn::Type) -> String {
        tok(t)
    }

    /// The element type of `Vec<T>` (`std::vec::Vec<T>`), unless the file declares a `Vec`.
    fn list_elem<'p>(&self, path: &'p syn::Path) -> Option<&'p syn::Type> {
        let names: Vec<String> = path.segments.iter().map(|s| s.ident.to_string()).collect();
        let is_vec = match names.as_slice() {
            [v] => v == "Vec" && !self.info.structs.contains_key("Vec") && path.leading_colon.is_none(),
            [k, m, v] => (k == "std" || k == "alloc") && m == "vec" && v == "Vec",
            _ => false,
        };
        match &path.segments.last()?.arguments {
            syn::PathArguments::AngleBracketed(a) if is_vec && a.args.len() == 1 => match &a.args[0] {
                syn::GenericArgument::Type(t) => Some(t),
                _ => None,
            },
            _ => None,
        }
    }

    /// The key and value types of std's `HashMap<K, V>` (imported, or by its whole path), which
    /// Oxie writes `[K: V]`.
    fn std_map<'p>(&self, path: &'p syn::Path) -> Option<(&'p syn::Type, &'p syn::Type)> {
        let names: Vec<String> = path.segments.iter().map(|s| s.ident.to_string()).collect();
        let is_std = match names.as_slice() {
            [m] => m == "HashMap" && self.info.std_collections.contains("HashMap") && path.leading_colon.is_none(),
            [s, c, m] => s == "std" && c == "collections" && m == "HashMap",
            [s, c, h, m] => s == "std" && c == "collections" && h == "hash_map" && m == "HashMap",
            _ => false,
        };
        match &path.segments.last()?.arguments {
            syn::PathArguments::AngleBracketed(a) if is_std && a.args.len() == 2 => match (&a.args[0], &a.args[1]) {
                (syn::GenericArgument::Type(k), syn::GenericArgument::Type(v)) => Some((k, v)),
                _ => None,
            },
            _ => None,
        }
    }

    pub fn type_path(&self, qself: Option<&syn::QSelf>, path: &syn::Path) -> String {
        // a trait's type arguments may be slices (`AsRef<[u8]>`)
        let is_trait = self.trait_args.take();
        if qself.is_none() {
            if let Some(s) = crate::oxie_aux::strip_runtime_path(path) {
                return s;
            }
        }
        let mut s = String::new();
        let segs: Vec<&syn::PathSegment> = path.segments.iter().collect();
        let mut start = 0;
        if let Some(q) = qself {
            s.push('<');
            self.slice_here.set(true);
            s.push_str(&self.ty(&q.ty));
            if q.position > 0 {
                s.push_str(" as ");
                if path.leading_colon.is_some() {
                    s.push_str("::");
                }
                let tr: Vec<String> = segs[..q.position].iter().map(|seg| self.path_segment_of(seg, true)).collect();
                s.push_str(&tr.join("::"));
            }
            s.push('>');
            start = q.position;
            for seg in &segs[start..] {
                s.push_str("::");
                s.push_str(&self.path_segment(seg));
            }
            return s;
        }
        if path.leading_colon.is_some() && !segs.first().is_some_and(|f| f.ident == "std" || f.ident == "core" || f.ident == "alloc") {
            s.push_str("::");
        }
        let v: Vec<String> = segs[start..].iter().map(|seg| self.path_segment_of(seg, is_trait)).collect();
        s.push_str(&v.join("::"));
        s
    }

    pub fn path_segment(&self, seg: &syn::PathSegment) -> String {
        self.path_segment_of(seg, false)
    }

    /// `is_trait`: the segment is a trait's, whose type arguments may be slices.
    fn path_segment_of(&self, seg: &syn::PathSegment, is_trait: bool) -> String {
        let mut s = id(&seg.ident);
        match &seg.arguments {
            syn::PathArguments::None => {}
            syn::PathArguments::AngleBracketed(a) => {
                // the pointers that hold slices: `Box<[T]>`, `Rc<[T]>`
                let unsized_args = is_trait || matches!(seg.ident.to_string().as_str(), "Box" | "Rc" | "Arc" | "Cow" | "Weak");
                s.push_str(&self.generic_args_of(a, unsized_args));
            }
            syn::PathArguments::Parenthesized(p) => {
                let ins: Vec<String> = p.inputs.iter().map(|t| self.ty(t)).collect();
                s.push_str(&format!("({})", ins.join(", ")));
                if let syn::ReturnType::Type(_, r) = &p.output {
                    s.push_str(&format!(" -> {}", self.ty(r)));
                }
            }
        }
        s
    }

    pub fn generic_args(&self, a: &syn::AngleBracketedGenericArguments) -> String {
        self.generic_args_of(a, false)
    }

    /// `unsized_args`: the arguments are a pointer's or a trait's, where Oxie reads `[T]` as a
    /// slice.
    fn generic_args_of(&self, a: &syn::AngleBracketedGenericArguments, unsized_args: bool) -> String {
        let v: Vec<String> = a
            .args
            .iter()
            .map(|g| match g {
                syn::GenericArgument::Type(t) => {
                    self.slice_here.set(unsized_args);
                    self.ty(t)
                }
                syn::GenericArgument::Lifetime(l) => l.to_string(),
                syn::GenericArgument::Const(e) => {
                    let s = self.rust_expr_inline(e);
                    if matches!(e, syn::Expr::Lit(_) | syn::Expr::Path(_) | syn::Expr::Block(_)) {
                        s
                    } else {
                        format!("{{ {} }}", s)
                    }
                }
                syn::GenericArgument::AssocType(at) => {
                    let g = at.generics.as_ref().map(|g| self.generic_args(g)).unwrap_or_default();
                    // `Deref<Target = [T]>`
                    self.slice_here.set(unsized_args && at.ident == "Target");
                    format!("{}{} = {}", at.ident, g, self.ty(&at.ty))
                }
                syn::GenericArgument::Constraint(c) => format!("{}: {}", c.ident, self.bounds(&c.bounds)),
                other => tok(other),
            })
            .collect();
        format!("{}<{}>", if a.colon2_token.is_some() { "::" } else { "" }, v.join(", "))
    }

    pub fn bounds<'b>(&self, bounds: impl IntoIterator<Item = &'b syn::TypeParamBound>) -> String {
        let v: Vec<String> = bounds.into_iter().map(|b| self.bound(b)).collect();
        v.join(" + ")
    }

    pub fn bound(&self, b: &syn::TypeParamBound) -> String {
        match b {
            syn::TypeParamBound::Trait(t) => {
                let mut s = String::new();
                if t.paren_token.is_some() {
                    s.push('(');
                }
                if let Some(l) = &t.lifetimes {
                    s.push_str(&tok(l));
                    s.push(' ');
                }
                if let syn::TraitBoundModifier::Maybe(_) = t.modifier {
                    s.push('?');
                }
                self.trait_args.set(true);
                s.push_str(&self.type_path(None, &t.path));
                if t.paren_token.is_some() {
                    s.push(')');
                }
                s
            }
            syn::TypeParamBound::Lifetime(l) => l.to_string(),
            other => tok(other),
        }
    }

    pub fn generics(&self, g: &syn::Generics) -> String {
        if g.params.is_empty() {
            return String::new();
        }
        let v: Vec<String> = g
            .params
            .iter()
            .map(|p| match p {
                syn::GenericParam::Lifetime(l) => {
                    let mut s = l.lifetime.to_string();
                    if !l.bounds.is_empty() {
                        let b: Vec<String> = l.bounds.iter().map(|b| b.to_string()).collect();
                        s.push_str(&format!(": {}", b.join(" + ")));
                    }
                    s
                }
                syn::GenericParam::Type(t) => {
                    let mut s = id(&t.ident);
                    if !t.bounds.is_empty() {
                        s.push_str(&format!(": {}", self.bounds(&t.bounds)));
                    }
                    if let Some(d) = &t.default {
                        s.push_str(&format!(" = {}", self.ty(d)));
                    }
                    s
                }
                syn::GenericParam::Const(c) => {
                    let mut s = format!("const {}: {}", c.ident, self.ty(&c.ty));
                    if let Some(d) = &c.default {
                        s.push_str(&format!(" = {}", self.rust_expr_inline(d)));
                    }
                    s
                }
            })
            .collect();
        format!("<{}>", v.join(", "))
    }

    pub fn where_clause(&self, g: &syn::Generics) -> String {
        let Some(w) = &g.where_clause else { return String::new() };
        if w.predicates.is_empty() {
            return String::new();
        }
        let v: Vec<String> = w
            .predicates
            .iter()
            .map(|p| match p {
                syn::WherePredicate::Type(t) => {
                    let mut s = String::new();
                    if let Some(l) = &t.lifetimes {
                        s.push_str(&tok(l));
                        s.push(' ');
                    }
                    // `where [T]: Ord`
                    self.slice_here.set(true);
                    s.push_str(&format!("{}: {}", self.ty(&t.bounded_ty), self.bounds(&t.bounds)));
                    s
                }
                syn::WherePredicate::Lifetime(l) => {
                    let b: Vec<String> = l.bounds.iter().map(|b| b.to_string()).collect();
                    format!("{}: {}", l.lifetime, b.join(" + "))
                }
                other => tok(other),
            })
            .collect();
        format!(" where {}", v.join(", "))
    }

    /// Attributes as Oxie lines (`@derive(Debug)`, `/// doc`), skipping `derive` when
    /// `skip_derive` is set.
    pub fn attrs(&self, attrs: &[syn::Attribute], skip_derive: bool) -> Vec<String> {
        let mut out = Vec::new();
        for a in attrs {
            if skip_derive && a.path().is_ident("derive") {
                continue;
            }
            out.extend(self.attr(a));
        }
        out
    }

    pub fn attr(&self, a: &syn::Attribute) -> Vec<String> {
        let inner = matches!(a.style, syn::AttrStyle::Inner(_));
        if a.path().is_ident("doc") {
            if let syn::Meta::NameValue(nv) = &a.meta {
                if let syn::Expr::Lit(syn::ExprLit { lit: syn::Lit::Str(s), .. }) = &nv.value {
                    let marker = if inner { "//!" } else { "///" };
                    return s.value().split('\n').map(|l| format!("{}{}", marker, l.trim_end())).collect();
                }
            }
        }
        if inner {
            return vec![format!("#![{}]", self.meta(&a.meta))];
        }
        // `@name` and `@name(args)`; Oxie has no `@name = value` or `@name[args]`
        match &a.meta {
            syn::Meta::Path(_) | syn::Meta::List(syn::MetaList { delimiter: syn::MacroDelimiter::Paren(_), .. }) => vec![format!("@{}", self.meta(&a.meta))],
            _ => vec![format!("#[{}]", self.meta(&a.meta))],
        }
    }

    pub fn meta(&self, m: &syn::Meta) -> String {
        match m {
            syn::Meta::Path(p) => tok(p),
            syn::Meta::List(l) => {
                let inner = tokens(&l.tokens);
                match l.delimiter {
                    syn::MacroDelimiter::Paren(_) => format!("{}({})", tok(&l.path), inner),
                    syn::MacroDelimiter::Bracket(_) => format!("{}[{}]", tok(&l.path), inner),
                    syn::MacroDelimiter::Brace(_) => format!("{}{{{}}}", tok(&l.path), inner),
                }
            }
            syn::Meta::NameValue(nv) => format!("{} = {}", tok(&nv.path), self.rust_expr_inline(&nv.value)),
        }
    }

    pub fn vis(&self, v: &syn::Visibility) -> String {
        match v {
            syn::Visibility::Public(_) => "pub ".into(),
            syn::Visibility::Restricted(r) => {
                let p = tok(&r.path);
                if p == "crate" {
                    String::new()
                } else if r.in_token.is_some() {
                    format!("pub(in {}) ", p)
                } else {
                    format!("pub({}) ", p)
                }
            }
            syn::Visibility::Inherited => String::new(),
        }
    }
}

/// `Option<T>` → `Some(T)`.
pub fn option_inner(t: &syn::Type) -> Option<&syn::Type> {
    let syn::Type::Path(p) = t else { return None };
    if p.qself.is_some() {
        return None;
    }
    let names: Vec<String> = p.path.segments.iter().map(|s| s.ident.to_string()).collect();
    let ok = match names.len() {
        1 => names[0] == "Option",
        3 => (names[0] == "std" || names[0] == "core") && names[1] == "option" && names[2] == "Option",
        _ => false,
    };
    if !ok {
        return None;
    }
    let syn::PathArguments::AngleBracketed(a) = &p.path.segments.last()?.arguments else { return None };
    if a.args.len() != 1 {
        return None;
    }
    match a.args.first()? {
        syn::GenericArgument::Type(t) => Some(t),
        _ => None,
    }
}

/// Types that can be written `T?`.
fn optionable(t: &syn::Type) -> bool {
    match t {
        syn::Type::Path(p) => option_inner(t).is_none() && p.qself.is_none(),
        syn::Type::Reference(r) => matches!(&*r.elem, syn::Type::Path(_) | syn::Type::Slice(_)) && option_inner(&r.elem).is_none(),
        syn::Type::Tuple(t) => !t.elems.is_empty(),
        syn::Type::Array(_) | syn::Type::Slice(_) => true,
        _ => false,
    }
}

/// `Box<dyn Error>`, `Box<dyn std::error::Error + Send + Sync + 'static>`, ...
pub fn is_any_error(t: &syn::Type) -> bool {
    let syn::Type::Path(p) = t else { return false };
    let Some(last) = p.path.segments.last() else { return false };
    if last.ident != "Box" {
        return false;
    }
    let syn::PathArguments::AngleBracketed(a) = &last.arguments else { return false };
    if a.args.len() != 1 {
        return false;
    }
    let Some(syn::GenericArgument::Type(syn::Type::TraitObject(o))) = a.args.first() else { return false };
    let mut has_error = false;
    for b in &o.bounds {
        match b {
            syn::TypeParamBound::Trait(t) => {
                let names: Vec<String> = t.path.segments.iter().map(|s| s.ident.to_string()).collect();
                let last = names.last().map(|s| s.as_str()).unwrap_or("");
                match last {
                    "Error" if names.len() == 1 || names == ["std", "error", "Error"] || names == ["core", "error", "Error"] => has_error = true,
                    "Send" | "Sync" => {}
                    _ => return false,
                }
            }
            syn::TypeParamBound::Lifetime(l) => {
                if l.ident != "static" {
                    return false;
                }
            }
            _ => return false,
        }
    }
    has_error
}
