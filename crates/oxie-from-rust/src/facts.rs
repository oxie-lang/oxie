//! Facts about a Rust file that the translation must preserve: how each method receives
//! `self`, and which traits each type derives. The translated Oxie is compiled back to Rust
//! and its facts compared with the original's; differences become explicit receivers and
//! derives in the Oxie output.

use std::collections::{BTreeMap, BTreeSet};

use quote::ToTokens;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Recv {
    None,
    Ref,
    RefMut,
    Value,
    /// `self: Box<Self>` and other typed receivers.
    Typed,
}

/// (self type, trait or "", method name)
pub type MethodKey = (String, String, String);

#[derive(Default, Debug)]
pub struct Facts {
    pub receivers: BTreeMap<MethodKey, Recv>,
    pub derives: BTreeMap<String, BTreeSet<String>>,
}

pub fn recv_of(sig: &syn::Signature) -> Recv {
    match sig.inputs.first() {
        Some(syn::FnArg::Receiver(r)) => {
            if r.colon_token.is_some() {
                Recv::Typed
            } else if r.reference.is_some() {
                if r.mutability.is_some() {
                    Recv::RefMut
                } else {
                    Recv::Ref
                }
            } else {
                Recv::Value
            }
        }
        _ => Recv::None,
    }
}

/// Last path segment of a type, with its generic arguments, without spaces: `Wrapper<T>`.
/// The name of a type without its path or arguments: `Counter` for `crate::a::Counter<T>`.
pub fn type_name(t: &syn::Type) -> String {
    match t {
        syn::Type::Path(p) if p.qself.is_none() => p.path.segments.last().map(|s| s.ident.to_string()).unwrap_or_default(),
        _ => String::new(),
    }
}

pub fn type_key(t: &syn::Type) -> String {
    match t {
        syn::Type::Path(p) if p.qself.is_none() => match p.path.segments.last() {
            Some(s) => {
                let mut k = s.ident.to_string();
                if !s.arguments.is_empty() {
                    k.push_str(&s.arguments.to_token_stream().to_string());
                }
                k.replace(' ', "")
            }
            None => String::new(),
        },
        _ => t.to_token_stream().to_string().replace(' ', ""),
    }
}

/// The trait's name, without path or generic arguments (`Mul<f64>` → `Mul`).
pub fn trait_key(p: &syn::Path) -> String {
    p.segments.last().map(|s| s.ident.to_string()).unwrap_or_default()
}

pub fn derives_of(attrs: &[syn::Attribute]) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for a in attrs {
        if a.path().is_ident("derive") {
            let _ = a.parse_nested_meta(|m| {
                if let Some(s) = m.path.segments.last() {
                    out.insert(s.ident.to_string());
                }
                Ok(())
            });
        }
    }
    out
}

/// Items declared inside a function body, at any depth (not inside those items).
fn nested_items(b: &syn::Block) -> Vec<syn::Item> {
    use syn::visit::Visit;
    struct V(Vec<syn::Item>);
    impl<'ast> Visit<'ast> for V {
        fn visit_item(&mut self, i: &'ast syn::Item) {
            self.0.push(i.clone());
        }
    }
    let mut v = V(Vec::new());
    v.visit_block(b);
    v.0
}

pub fn collect(file: &syn::File) -> Facts {
    let mut f = Facts::default();
    collect_items(&file.items, "", &mut f);
    f
}

fn collect_items(items: &[syn::Item], prefix: &str, f: &mut Facts) {
    for it in items {
        match it {
            syn::Item::Struct(s) => {
                f.derives.insert(format!("{}{}", prefix, s.ident), derives_of(&s.attrs));
            }
            syn::Item::Enum(e) => {
                f.derives.insert(format!("{}{}", prefix, e.ident), derives_of(&e.attrs));
            }
            syn::Item::Impl(i) => {
                let ty = format!("{}{}", prefix, type_key(&i.self_ty));
                let tr = i.trait_.as_ref().map(|(_, p, _)| trait_key(p)).unwrap_or_default();
                for ii in &i.items {
                    if let syn::ImplItem::Fn(m) = ii {
                        f.receivers.insert((ty.clone(), tr.clone(), m.sig.ident.to_string()), recv_of(&m.sig));
                        collect_items(&nested_items(&m.block), &format!("{}::{}()::", ty, m.sig.ident), f);
                    }
                }
            }
            syn::Item::Trait(t) => {
                let ty = format!("{}trait {}", prefix, t.ident);
                for ti in &t.items {
                    if let syn::TraitItem::Fn(m) = ti {
                        f.receivers.insert((ty.clone(), String::new(), m.sig.ident.to_string()), recv_of(&m.sig));
                        if let Some(b) = &m.default {
                            collect_items(&nested_items(b), &format!("{}::{}()::", ty, m.sig.ident), f);
                        }
                    }
                }
            }
            syn::Item::Fn(func) => collect_items(&nested_items(&func.block), &format!("{}{}()::", prefix, func.sig.ident), f),
            syn::Item::Mod(m) => {
                if let Some((_, items)) = &m.content {
                    if m.ident != "__oxie" {
                        collect_items(items, &format!("{}{}::", prefix, m.ident), f);
                    }
                }
            }
            _ => {}
        }
    }
}
