//! Lists from array literals. A `var` initialized with an array literal (`var xs = [1, 2]`) that
//! the code after it grows or shrinks (`xs.push(3)`) is a list, `vec![1, 2]`: an array can't
//! change its length, so Rust would reject the call. Other array literals stay arrays, and one
//! with a declared type takes that type in the lowering (`val xs: [i32] = [1, 2]`).
//!
//! Likewise a `&mut [T]` parameter that its function grows (`items.push(x)`) is a
//! `&mut Vec<T>`: a slice can't change its length either.

use crate::ast::*;
use crate::visit::{walk_block_mut, walk_expr, walk_expr_mut, walk_stmt};

/// Methods of `Vec` that change its length, which arrays don't have.
const GROWS: &[&str] = &[
    "push", "pop", "insert", "remove", "clear", "truncate", "extend", "extend_from_slice", "append", "retain", "retain_mut",
    "drain", "dedup", "dedup_by", "dedup_by_key", "resize", "resize_with", "split_off", "swap_remove", "reserve",
    "shrink_to_fit",
];

/// Makes the array literals of the items' `var`s that grow lists.
pub fn grow_lists(items: &mut [Item]) {
    for it in items {
        match &mut it.kind {
            ItemKind::Fun(f) => {
                grown_params(f);
                match &mut f.body {
                    Some(FunBody::Block(b)) => in_block(b),
                    Some(FunBody::Expr(e)) => in_expr(e),
                    None => {}
                }
            }
            ItemKind::Type(t) => {
                if let Some(b) = &mut t.init {
                    in_block(b);
                }
                grow_lists(&mut t.members);
            }
            ItemKind::Trait(t) => grow_lists(&mut t.members),
            ItemKind::Impl(i) => grow_lists(&mut i.members),
            ItemKind::Mod { items, .. } | ItemKind::Object { items, .. } => grow_lists(items),
            ItemKind::Test(t) => in_block(&mut t.body),
            _ => {}
        }
    }
}

/// The block and every block inside it.
fn in_block(b: &mut Block) {
    lists(b);
    walk_block_mut(b, &mut nested);
}

fn in_expr(e: &mut Expr) {
    walk_expr_mut(e, &mut nested);
}

/// The blocks an expression holds itself (deeper ones are reached by the walk).
fn nested(e: &mut Expr) -> bool {
    use ExprKind::*;
    match &mut e.kind {
        Block(b) | Unsafe(b) | ConstBlock(b) | LabeledBlock { block: b, .. } | Loop { body: b, .. } | Async { block: b, .. } | While { body: b, .. } | DoWhile { body: b, .. } | For { body: b, .. } | If { then: b, .. } => lists(b),
        TryCatch { body, catch } => {
            lists(body);
            if let Some(c) = catch {
                lists(&mut c.body);
            }
        }
        _ => {}
    }
    true
}

/// Turns the array literal of each `var` of the block that a later statement grows into a list.
fn lists(b: &mut Block) {
    for i in 0..b.stmts.len() {
        let (head, rest) = b.stmts.split_at_mut(i + 1);
        let Stmt::Let { pat: Pat::Ident { name, mutable: true, .. }, ty: None, init: Some(init), .. } = &mut head[i] else { continue };
        if !matches!(init.kind, ExprKind::Array(_) | ExprKind::ArrayRepeat(..)) || !rest.iter().any(|s| grows(s, name)) {
            continue;
        }
        let span = init.span;
        let items = match std::mem::replace(&mut init.kind, ExprKind::Error) {
            ExprKind::Array(items) => items,
            repeat => vec![Expr::new(repeat, span)],
        };
        init.kind = ExprKind::VecLit(items);
    }
}

/// Makes each `&mut [T]` parameter of the function that its body grows a `&mut Vec<T>`.
fn grown_params(f: &mut FunDecl) {
    let Some(body) = &f.body else { return };
    for p in &mut f.params {
        let Some(name) = p.name().map(str::to_string) else { continue };
        let Type::Ref { mutable: true, inner, .. } = &mut p.ty else { continue };
        let Type::Slice(elem) = &**inner else { continue };
        let grown = match body {
            FunBody::Block(b) => b.stmts.iter().any(|s| grows(s, &name)),
            FunBody::Expr(e) => grows_expr(e, &name),
        };
        if grown {
            **inner = Type::list((**elem).clone(), p.span);
        }
    }
}

/// Does the statement call a method that changes the length of the variable `name`?
fn grows(s: &Stmt, name: &str) -> bool {
    let mut found = false;
    walk_stmt(s, &mut |e| {
        found |= grows_here(e, name);
        !found
    });
    found
}

fn grows_expr(e: &Expr, name: &str) -> bool {
    let mut found = false;
    walk_expr(e, &mut |e| {
        found |= grows_here(e, name);
        !found
    });
    found
}

fn grows_here(e: &Expr, name: &str) -> bool {
    matches!(&e.kind, ExprKind::MethodCall { recv, name: m, .. } if recv.ident() == Some(name) && GROWS.contains(&m.as_str()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn init_of(src: &str) -> ExprKind {
        let (mut file, diags) = crate::parser::parse_file(src);
        assert!(diags.is_empty(), "{:?}", diags);
        grow_lists(&mut file.items);
        let ItemKind::Fun(f) = &file.items[0].kind else { panic!() };
        let Some(FunBody::Block(b)) = &f.body else { panic!() };
        let Stmt::Let { init: Some(e), .. } = &b.stmts[0] else { panic!() };
        e.kind.clone()
    }

    #[test]
    fn grown_arrays_become_lists() {
        assert!(matches!(init_of("fun f() {\n    var xs = [1, 2]\n    xs.push(3)\n}\n"), ExprKind::VecLit(v) if v.len() == 2));
        assert!(matches!(init_of("fun f() {\n    var xs = [0; 4]\n    if (true) { xs.clear() }\n}\n"), ExprKind::VecLit(v) if v.len() == 1));
        // read, sorted (arrays sort too) or declared: an array
        assert!(matches!(init_of("fun f() {\n    var xs = [1, 2]\n    xs.sort()\n}\n"), ExprKind::Array(_)));
        assert!(matches!(init_of("fun f() {\n    val xs = [1, 2]\n    println(xs.len())\n}\n"), ExprKind::Array(_)));
        assert!(matches!(init_of("fun f() {\n    var xs: [i32; 2] = [1, 2]\n    xs[0] = 3\n}\n"), ExprKind::Array(_)));
    }

    fn param_of(src: &str) -> Type {
        let (mut file, diags) = crate::parser::parse_file(src);
        assert!(diags.is_empty(), "{:?}", diags);
        grow_lists(&mut file.items);
        let ItemKind::Fun(f) = &file.items[0].kind else { panic!() };
        let Type::Ref { inner, .. } = &f.params[0].ty else { panic!() };
        (**inner).clone()
    }

    #[test]
    fn grown_slice_params_become_lists() {
        assert!(param_of("fun f(xs: &mut [i32]) {\n    xs.push(1)\n}\n").single_name() == Some("Vec"));
        assert!(param_of("fun f(xs: &mut [i32]) = xs.clear()\n").single_name() == Some("Vec"));
        // changing elements, or a read-only slice: still a slice
        assert!(matches!(param_of("fun f(xs: &mut [i32]) {\n    xs[0] = 1\n}\n"), Type::Slice(_)));
        assert!(matches!(param_of("fun f(xs: &[i32]) {\n    println(xs.len())\n}\n"), Type::Slice(_)));
    }
}
