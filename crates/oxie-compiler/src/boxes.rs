//! Recursive types. A field whose type holds its own type by value (`Neg(Expr)` in
//! `enum Expr`, `next: Node?` in `class Node`), directly or through other types of the crate,
//! would make the type infinitely large, so Oxie boxes it: `Box<Expr>`, `Option<Box<Node>>`.
//! The lowering boxes the values that go into such a field (`coerce`), and reads see through
//! the box. Shared classes are handles already, and lists and maps hold their elements on the
//! heap, so neither needs one.

use crate::ast::*;
use std::collections::{HashMap, HashSet};

/// Boxes the fields of the crate's types that contain their own type by value.
pub fn box_recursive(files: &mut [&mut File]) {
    // the types each type holds by value, by name
    let mut holds: HashMap<String, HashSet<String>> = HashMap::new();
    for f in files.iter() {
        each_type(&f.items, &mut |t| {
            holds.insert(t.name.clone(), HashSet::new());
        });
    }
    let names: HashSet<String> = holds.keys().cloned().collect();
    for f in files.iter() {
        each_type(&f.items, &mut |t| {
            let mut out = HashSet::new();
            for ty in field_types(t) {
                by_value(ty, &names, &mut out);
            }
            holds.insert(t.name.clone(), out);
        });
    }
    let reaches = |from: &str, to: &str| {
        let mut seen = HashSet::new();
        let mut todo = vec![from.to_string()];
        while let Some(n) = todo.pop() {
            if n == to {
                return true;
            }
            if seen.insert(n.clone()) {
                todo.extend(holds.get(&n).into_iter().flatten().cloned());
            }
        }
        false
    };
    for f in files.iter_mut() {
        each_type_mut(&mut f.items, &mut |t| {
            let name = t.name.clone();
            let recursive = |ty: &Type| {
                let mut out = HashSet::new();
                by_value(ty, &names, &mut out);
                out.iter().any(|n| reaches(n, &name))
            };
            for ty in field_types_mut(t) {
                if recursive(ty) {
                    *ty = boxed(std::mem::replace(ty, Type::Infer));
                }
            }
        });
    }
}

/// `T` as `Box<T>`, and `T?` as `Option<Box<T>>`.
fn boxed(t: Type) -> Type {
    match t {
        Type::Option(inner) => Type::Option(Box::new(Type::auto_box(*inner))),
        t => Type::auto_box(t),
    }
}

/// The names of the crate's types that a value of `t` holds by value (not behind a reference,
/// a box, a list or any other generic type but `Option`).
fn by_value(t: &Type, names: &HashSet<String>, out: &mut HashSet<String>) {
    match t {
        Type::Option(inner) | Type::Array(inner, _) => by_value(inner, names, out),
        Type::Tuple(ts) => ts.iter().for_each(|t| by_value(t, names, out)),
        Type::Path { segs, .. } => {
            let Some(last) = segs.last() else { return };
            match last.generics.as_deref() {
                Some([GenericArg::Type(inner)]) if last.name == "Option" => by_value(inner, names, out),
                // also a generic one (`List<T>` in `Cons(T, List<T>)`)
                _ if names.contains(&last.name) => {
                    out.insert(last.name.clone());
                }
                _ => {}
            }
        }
        _ => {}
    }
}

/// The types that can be recursive: classes, data classes and enums, but not shared classes.
fn boxable(t: &TypeDecl) -> bool {
    !t.shared && matches!(t.kind, TypeKind::Class | TypeKind::Data | TypeKind::Enum)
}

fn field_types(t: &TypeDecl) -> Vec<&Type> {
    let mut out: Vec<&Type> = t.fields.iter().map(|f| &f.ty).collect();
    for v in &t.variants {
        match &v.fields {
            VariantFields::Tuple(ts) => out.extend(ts.iter()),
            VariantFields::Named(fs) => out.extend(fs.iter().map(|f| &f.ty)),
            VariantFields::Unit => {}
        }
    }
    out
}

fn field_types_mut(t: &mut TypeDecl) -> Vec<&mut Type> {
    let mut out: Vec<&mut Type> = t.fields.iter_mut().map(|f| &mut f.ty).collect();
    for v in &mut t.variants {
        match &mut v.fields {
            VariantFields::Tuple(ts) => out.extend(ts.iter_mut()),
            VariantFields::Named(fs) => out.extend(fs.iter_mut().map(|f| &mut f.ty)),
            VariantFields::Unit => {}
        }
    }
    out
}

fn each_type(items: &[Item], f: &mut dyn FnMut(&TypeDecl)) {
    for it in items {
        match &it.kind {
            ItemKind::Type(t) if boxable(t) => f(t),
            ItemKind::Mod { items, .. } | ItemKind::Object { items, .. } => each_type(items, f),
            _ => {}
        }
    }
}

fn each_type_mut(items: &mut [Item], f: &mut dyn FnMut(&mut TypeDecl)) {
    for it in items {
        match &mut it.kind {
            ItemKind::Type(t) if boxable(t) => f(t),
            ItemKind::Mod { items, .. } | ItemKind::Object { items, .. } => each_type_mut(items, f),
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The field types of the first type in `src` after boxing, as Oxie would print them.
    fn fields_of(src: &str) -> Vec<String> {
        let (mut file, diags) = crate::parser::parse_file(src);
        assert!(diags.is_empty(), "{:?}", diags);
        box_recursive(&mut [&mut file]);
        let ItemKind::Type(t) = &file.items[0].kind else { panic!() };
        field_types(t).iter().map(|t| show(t)).collect()
    }

    fn show(t: &Type) -> String {
        match t {
            Type::Option(inner) => format!("{}?", show(inner)),
            _ => match t.auto_boxed() {
                Some(inner) => format!("Box<{}>", show(inner)),
                None => t.single_name().or_else(|| t.last_name()).unwrap_or("?").to_string(),
            },
        }
    }

    #[test]
    fn recursive_fields_are_boxed() {
        assert_eq!(fields_of("enum Expr { Num(f64), Neg(Expr), Bin(op: char, left: Expr, right: Expr) }"), ["f64", "Box<Expr>", "char", "Box<Expr>", "Box<Expr>"]);
        assert_eq!(fields_of("class Node(value: i32, next: Node?)"), ["i32", "Box<Node>?"]);
        // through another type: only the fields on the cycle
        assert_eq!(fields_of("enum Stmt { Block(body: Body), Print(String) }\nclass Body(first: Stmt, count: i32)"), ["Box<Body>", "String"]);
        // lists, boxes and shared classes are no cycle
        assert_eq!(fields_of("class Tree(children: [Tree], parent: Box<Tree>?)"), ["Vec", "Box?"]);
        assert_eq!(fields_of("enum A { X(i32) }"), ["i32"]);
    }
}
