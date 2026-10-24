//! Lowering from the Oxie AST to Rust source text.

use std::collections::{HashMap, HashSet};

use crate::ast::*;
use crate::diag::{Diag, Span};
use crate::emit::Emitter;
use crate::lexer::StrPart;
use crate::parser::is_type_like;
use crate::sema::{self, Crate, MethodInfo, Recv, VariantShape};
use crate::visit;

#[path = "shared.rs"]
mod shared;

// Rust output precedence levels.
pub(crate) const P_LOWEST: u8 = 0;
pub(crate) const P_RANGE: u8 = 1;
pub(crate) const P_CAST: u8 = 12;
pub(crate) const P_UNARY: u8 = 13;
pub(crate) const P_POSTFIX: u8 = 14;
pub(crate) const P_PRIMARY: u8 = 15;

pub const RT: &str = "crate::__oxie";

/// Rust keywords that are valid identifiers in Oxie and need `r#`.
const RUST_ONLY_KEYWORDS: &[&str] = &[
    "type", "box", "yield", "macro", "final", "priv", "typeof", "unsized", "virtual", "abstract", "become", "override",
    "gen", "await", "loop", "match", "struct", "trait", "impl", "where", "use", "fn", "let", "ref", "mod", "pub",
    "move", "dyn", "extern", "unsafe", "do", "try", "static", "const", "enum", "in", "as", "for", "while", "if",
    "else", "return", "break", "continue", "mut", "true", "false",
];

pub(crate) const FORMAT_MACROS: &[(&str, usize)] = &[
    ("println", 0), ("print", 0), ("eprintln", 0), ("eprint", 0), ("format", 0), ("panic", 0), ("todo", 0),
    ("unimplemented", 0), ("unreachable", 0), ("write", 1), ("writeln", 1), ("assert", 1), ("debug_assert", 1),
    ("assert_eq", 2), ("assert_ne", 2), ("debug_assert_eq", 2), ("debug_assert_ne", 2), ("format_args", 0),
];

pub(crate) const OTHER_MACROS: &[&str] = &[
    "dbg", "matches", "concat", "stringify", "env", "option_env", "include_str", "include_bytes", "line", "file",
    "column", "module_path", "cfg", "compile_error", "vec",
];

const STD_AUTO_DYN: &[&str] = &[
    "Display", "Debug", "Error", "Any", "Read", "Write", "BufRead", "Iterator", "DoubleEndedIterator",
    "ExactSizeIterator", "IntoIterator", "Future", "ToString", "Fn", "FnMut", "FnOnce",
];

const DERIVABLE: &[&str] = &["Debug", "Clone", "Copy", "PartialEq", "Eq", "Hash", "Default", "PartialOrd", "Ord"];

const CONTAINERS: &[&str] = &["Box", "Rc", "Arc", "RefCell", "Mutex", "RwLock", "Cell", "Pin", "Weak", "UnsafeCell"];

/// Std trait owning a method name (for assigning `override fun` to a supertype).
pub(crate) fn std_trait_of_method(m: &str) -> &'static [&'static str] {
    match m {
        "fmt" => &["Display", "Debug"],
        "default" => &["Default"],
        "clone" => &["Clone"],
        "eq" | "ne" => &["PartialEq"],
        "partial_cmp" | "lt" | "le" | "gt" | "ge" => &["PartialOrd"],
        "cmp" | "max" | "min" => &["Ord"],
        "hash" => &["Hash"],
        "next" | "size_hint" | "count" => &["Iterator"],
        "next_back" => &["DoubleEndedIterator"],
        "from" => &["From"],
        "try_from" => &["TryFrom"],
        "drop" => &["Drop"],
        "deref" => &["Deref"],
        "deref_mut" => &["DerefMut"],
        "from_str" => &["FromStr"],
        "as_ref" => &["AsRef"],
        "as_mut" => &["AsMut"],
        "borrow" => &["Borrow"],
        "index" => &["Index"],
        "index_mut" => &["IndexMut"],
        "into_iter" => &["IntoIterator"],
        "source" | "description" | "cause" => &["Error"],
        "poll" => &["Future"],
        "add" => &["Add"],
        "sub" => &["Sub"],
        "mul" => &["Mul"],
        "div" => &["Div"],
        "rem" => &["Rem"],
        "neg" => &["Neg"],
        "not" => &["Not"],
        "write_str" => &["Write"],
        _ => &[],
    }
}

fn std_trait_assoc(t: &str) -> &'static [&'static str] {
    match t {
        "Iterator" => &["Item"],
        "IntoIterator" => &["Item", "IntoIter"],
        "Deref" => &["Target"],
        "Add" | "Sub" | "Mul" | "Div" | "Rem" | "Neg" | "Not" | "Index" | "Future" | "Shl" | "Shr" | "BitAnd" | "BitOr" | "BitXor" => &["Output"],
        "FromStr" => &["Err"],
        "TryFrom" | "TryInto" => &["Error"],
        _ => &[],
    }
}

/// Kotlin operator function names -> (trait, method, is_assign, is_unary)
fn operator_trait(name: &str) -> Option<(&'static str, &'static str, bool, bool)> {
    Some(match name {
        "plus" | "add" => ("Add", "add", false, false),
        "minus" | "sub" => ("Sub", "sub", false, false),
        "times" | "mul" => ("Mul", "mul", false, false),
        "div" => ("Div", "div", false, false),
        "rem" | "mod" => ("Rem", "rem", false, false),
        "unaryMinus" | "neg" => ("Neg", "neg", false, true),
        "not" => ("Not", "not", false, true),
        "plusAssign" | "add_assign" => ("AddAssign", "add_assign", true, false),
        "minusAssign" | "sub_assign" => ("SubAssign", "sub_assign", true, false),
        "timesAssign" | "mul_assign" => ("MulAssign", "mul_assign", true, false),
        "divAssign" | "div_assign" => ("DivAssign", "div_assign", true, false),
        "remAssign" | "rem_assign" => ("RemAssign", "rem_assign", true, false),
        "shl" => ("Shl", "shl", false, false),
        "shr" => ("Shr", "shr", false, false),
        "and" | "bitand" => ("BitAnd", "bitand", false, false),
        "or" | "bitor" => ("BitOr", "bitor", false, false),
        "xor" | "bitxor" => ("BitXor", "bitxor", false, false),
        _ => return None,
    })
}

fn kotlin_alias(name: &str) -> Option<&'static str> {
    Some(match name {
        "Int" => "i32",
        "Long" => "i64",
        "Short" => "i16",
        "Byte" => "i8",
        "UInt" => "u32",
        "ULong" => "u64",
        "UShort" => "u16",
        "UByte" => "u8",
        "Double" => "f64",
        "Float" => "f32",
        "Boolean" => "bool",
        "Char" => "char",
        "Unit" => "()",
        _ => return None,
    })
}

pub(crate) const PRIMITIVES: &[&str] = &[
    "i8", "i16", "i32", "i64", "i128", "isize", "u8", "u16", "u32", "u64", "u128", "usize", "f32", "f64", "bool", "char", "str",
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TyPos {
    Param,
    Ret,
    Field,
    Generic,
    Container,
    Local,
    Plain,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum ItemCtx {
    Top,
    Local,
    Object,
}

#[derive(Clone)]
pub(crate) struct ClassCtx {
    pub(crate) name: String,
    pub(crate) fields: HashMap<String, Type>,
    pub(crate) methods: HashMap<String, MethodInfo>,
    pub(crate) self_name: String,
    pub(crate) is_static: bool,
    /// Type parameters of the class or impl block (methods get inferred bounds for them).
    pub(crate) generics: Vec<GenericParam>,
    /// Methods of implemented interfaces: their signatures follow the interface, so no bounds
    /// are inferred for them. `None` inside interfaces and trait impls (never inferred).
    pub(crate) trait_methods: Option<HashSet<String>>,
}

#[derive(Clone)]
pub(crate) struct FnCtx {
    pub(crate) throws: bool,
    /// Value type of the function (Ok type for throwing functions).
    pub(crate) ret: Option<Type>,
    pub(crate) is_lambda: bool,
    pub(crate) tries: Vec<String>,
    /// `Some` in a `toString` with a block body, whose text `Display` computes in a block:
    /// `return` breaks out of the block (labeled `TEXT_LABEL`), and this is `Some(true)` once
    /// one has, so the block gets its label.
    pub(crate) text_break: Option<bool>,
    /// The function returns `impl Interface`: its values are returned as they are, not boxed.
    pub(crate) unboxed_ret: bool,
}

/// The label of the block that computes the text of a `toString` with a block body.
pub(crate) const TEXT_LABEL: &str = "__oxie_text";

#[derive(Clone)]
pub(crate) struct LoopCtx {
    pub(crate) dowhile_label: Option<String>,
}

#[derive(Clone)]
pub(crate) struct Target {
    pub(crate) ty: Option<Type>,
    /// Wrap the tail value, e.g. `Ok` or `Ok::<_, E>`.
    pub(crate) wrap: Option<String>,
    /// The value is the function's result, so `return x` in tail position is just `x`.
    /// (Not for initializers: in `val x = when (..) { else -> return null }` the `return` leaves
    /// the function.)
    pub(crate) fn_result: bool,
}

impl Target {
    pub(crate) fn none() -> Target {
        Target { ty: None, wrap: None, fn_result: false }
    }
    pub(crate) fn ty(t: &Type) -> Target {
        Target { ty: Some(t.clone()), wrap: None, fn_result: false }
    }
}

pub(crate) enum PathKind {
    Module,
    Type,
    Value,
}

pub struct Lowerer<'a> {
    pub(crate) k: &'a Crate,
    pub e: Emitter,
    pub diags: Vec<Diag>,
    pub(crate) module: Vec<String>,
    pub(crate) is_root: bool,
    pub(crate) path_roots: HashSet<String>,
    pub(crate) scopes: Vec<HashMap<String, Option<Type>>>,
    pub(crate) class: Option<ClassCtx>,
    pub(crate) fns: Vec<FnCtx>,
    pub(crate) loops: Vec<LoopCtx>,
    pub(crate) tmp: usize,
    pub(crate) tests: Vec<(Vec<Attr>, TestDecl)>,
    pub(crate) in_test_mod: bool,
    pub(crate) pending_trait_ret: Option<Type>,
    /// Lowering the left side of `?:`: every `?.` is safe navigation.
    pub(crate) force_safe: bool,
    /// Inside a `pub` class/enum: members default to `pub`.
    pub(crate) members_pub: bool,
    /// Emitting a function signature, where `'_` may stand for elided lifetimes.
    pub(crate) in_signature: std::cell::Cell<bool>,
    /// Directory of the source file (for `mod name` declarations of hand-written Rust files).
    pub src_dir: Option<std::path::PathBuf>,
    /// Scripts put crate attributes in their generated main.rs; projects can't (it's the user's file).
    pub crate_attrs_allowed: bool,
    /// Lint attributes from `#![...]` in the crate root, applied to every item and module.
    pub root_lints: Vec<String>,
    /// Lambda parameters without a known type: (scope depth, name). Rust passes collection
    /// elements by reference, so comparisons of these with literals read the value first.
    pub(crate) lambda_params: Vec<(usize, String)>,
    /// The next lambda lowered is a predicate whose parameters are element references.
    pub(crate) next_lambda_refs: bool,
    /// Span starts of `for` iterables that are used again after the loop, with the start of
    /// that use (none when a loop around it runs again; see `visit::reused_loop_iters`).
    pub(crate) reused_iters: HashMap<u32, Option<u32>>,
    /// Variables a null check in the `if` being lowered unwraps, with their value types.
    pub(crate) narrowed_types: Vec<(String, Type)>,
    /// Methods of the shared class being lowered that use `this` as a value, or call one that
    /// does: their data methods take the handle as `__this` (see `shared.rs`).
    pub(crate) needs_this: HashSet<String>,
    /// While lowering such a method: the span starts of the `this` used as receivers, which
    /// stay `self`; any other `this` is `__this.clone()`.
    pub(crate) this_recv: Option<HashSet<u32>>,
    /// A parameter `write_params` adds after the declared ones.
    pub(crate) extra_param: Option<String>,
    /// The handle type (`Counter<T>`) of the shared class being lowered.
    pub(crate) shared_handle_ty: Option<String>,
    /// Lowering a method that takes `self` by value, so its fields can be moved out.
    pub(crate) self_by_value: bool,
    /// The next statement has attributes (`@cfg(test)`), which must cover all of it, so its
    /// arguments stay in place (see `move_args_changing_this`).
    pub(crate) keep_args: bool,
    /// What happened to the `String`/`Vec` parameters of this file's functions that could be
    /// lent, by (the function's name span, parameter index).
    pub param_notes: HashMap<(Span, usize), crate::params::ParamNote>,
    /// Functions declared in the blocks being lowered, with their parameters: calls go to them
    /// rather than to the crate's functions with their names.
    pub(crate) local_funs: Vec<(String, Vec<sema::ParamInfo>)>,
    /// The type aliases this file declares at its top level.
    pub(crate) aliases: HashSet<String>,
}

pub fn esc(name: &str) -> String {
    if name.starts_with("r#") {
        return name.to_string();
    }
    if RUST_ONLY_KEYWORDS.contains(&name) {
        format!("r#{}", name)
    } else {
        name.to_string()
    }
}

/// `"NotFound"` -> `"not found"`
fn humanize(name: &str) -> String {
    let mut out = String::new();
    for (i, c) in name.chars().enumerate() {
        if c.is_uppercase() && i > 0 {
            out.push(' ');
        }
        out.extend(c.to_lowercase());
    }
    out
}

fn slug(name: &str) -> String {
    let mut s: String = name.chars().map(|c| if c.is_alphanumeric() { c.to_ascii_lowercase() } else { '_' }).collect();
    while s.contains("__") {
        s = s.replace("__", "_");
    }
    let s = s.trim_matches('_').to_string();
    if s.is_empty() || s.chars().next().unwrap().is_ascii_digit() {
        format!("test_{}", s)
    } else {
        s
    }
}

impl<'a> Lowerer<'a> {
    pub fn new(k: &'a Crate, module: Vec<String>, is_root: bool) -> Lowerer<'a> {
        let mut path_roots: HashSet<String> = ["std", "core", "alloc", "crate", "super"].iter().map(|s| s.to_string()).collect();
        path_roots.extend(k.modules.iter().cloned());
        path_roots.extend(k.extern_crates.iter().cloned());
        Lowerer {
            k,
            e: Emitter::new(),
            diags: Vec::new(),
            module,
            is_root,
            path_roots,
            scopes: vec![HashMap::new()],
            class: None,
            fns: Vec::new(),
            loops: Vec::new(),
            tmp: 0,
            tests: Vec::new(),
            in_test_mod: false,
            pending_trait_ret: None,
            force_safe: false,
            members_pub: false,
            in_signature: std::cell::Cell::new(false),
            src_dir: None,
            crate_attrs_allowed: false,
            root_lints: Vec::new(),
            lambda_params: Vec::new(),
            next_lambda_refs: false,
            reused_iters: HashMap::new(),
            narrowed_types: Vec::new(),
            needs_this: HashSet::new(),
            this_recv: None,
            extra_param: None,
            shared_handle_ty: None,
            self_by_value: false,
            keep_args: false,
            param_notes: HashMap::new(),
            local_funs: Vec::new(),
            aliases: HashSet::new(),
        }
    }

    pub(crate) fn error(&mut self, span: Span, msg: impl Into<String>) {
        self.diags.push(Diag::error(span, msg));
    }

    pub(crate) fn fresh(&mut self, base: &str) -> String {
        self.tmp += 1;
        format!("__oxie_{}{}", base, self.tmp)
    }

    // ------------------------------------------------------------------ scopes

    pub(crate) fn push_scope(&mut self) {
        self.scopes.push(HashMap::new());
    }
    pub(crate) fn pop_scope(&mut self) {
        self.scopes.pop();
    }
    pub(crate) fn bind_name(&mut self, name: &str, ty: Option<Type>) {
        self.scopes.last_mut().unwrap().insert(name.to_string(), ty);
    }
    pub(crate) fn bind_pat(&mut self, p: &Pat, ty: Option<Type>) {
        if let Pat::Ident { name, sub: None, .. } = p {
            self.bind_name(name, ty);
            return;
        }
        let mut names = Vec::new();
        p.bindings(&mut names);
        for n in names {
            self.bind_name(&n, None);
        }
    }
    /// Is `name` an untyped lambda parameter (and not shadowed by a later binding)?
    pub(crate) fn is_untyped_lambda_param(&self, name: &str) -> bool {
        let Some(depth) = self.scopes.iter().rposition(|s| s.contains_key(name)) else { return false };
        self.lambda_params.iter().any(|(d, n)| *d == depth && n == name)
    }
    pub(crate) fn local(&self, name: &str) -> Option<&Option<Type>> {
        for s in self.scopes.iter().rev() {
            if let Some(t) = s.get(name) {
                return Some(t);
            }
        }
        None
    }
    pub(crate) fn is_local(&self, name: &str) -> bool {
        self.local(name).is_some()
    }

    pub(crate) fn implicit_field(&self, name: &str) -> Option<String> {
        if self.is_local(name) {
            return None;
        }
        let c = self.class.as_ref()?;
        if c.is_static {
            return None;
        }
        if c.fields.contains_key(name) {
            Some(format!("{}.{}", c.self_name, esc(name)))
        } else {
            None
        }
    }


    // ------------------------------------------------------------------ file

    pub fn lower_file(&mut self, file: &File) {
        for d in &file.inner_docs {
            self.e.w(&format!("//{}", d));
            self.e.nl();
        }
        // `#![...]`: module files take inner attributes directly. The crate root is `include!`d,
        // which can't, so its lint attributes are applied to each item and module instead.
        let is_lint = |a: &str| {
            let a = a.trim_start();
            ["allow", "warn", "deny", "forbid", "expect"].iter().any(|l| a.strip_prefix(l).is_some_and(|r| r.trim_start().starts_with('(')))
        };
        for a in &file.inner_attrs {
            if !self.is_root {
                self.e.w(&format!("#![{}]", a));
                self.e.nl();
            } else if is_lint(a) {
                self.root_lints.push(a.clone());
            } else if !self.crate_attrs_allowed {
                self.diags.push(
                    Diag::error(Span::default(), format!("`#![{}]` applies to the whole crate and must be written in src/main.rs (or src/lib.rs)", a))
                        .with_help("lint attributes like `#![allow(...)]` work in .oxi files; other crate attributes go in the Rust root file"),
                );
            }
        }
        self.e.w("#[allow(unused_imports)]\nuse crate::__oxie::prelude::*;");
        self.e.nl();
        self.ext_imports();
        // imports first so path roots are known
        for it in &file.items {
            if let ItemKind::Import(i) = &it.kind {
                self.note_import(&i.tree);
            }
        }
        // inline `mod name { }` declarations are path roots too (`math.double(2)`)
        fn mods(items: &[Item], out: &mut Vec<String>) {
            for it in items {
                if let ItemKind::Mod { name, items } = &it.kind {
                    out.push(name.clone());
                    mods(items, out);
                }
            }
        }
        self.aliases = file.items.iter().filter_map(|it| if let ItemKind::TypeAlias { name, .. } = &it.kind { Some(name.clone()) } else { None }).collect();
        let mut inline = Vec::new();
        mods(&file.items, &mut inline);
        self.path_roots.extend(inline);
        // the module's children in files (`lint/js.oxi` for `js.lint(..)` in `lint`)
        let n = self.module.len();
        let children = self.k.module_paths.iter().filter(|p| p.len() == n + 1 && p[..n] == self.module[..]).map(|p| p[n].clone());
        self.path_roots.extend(children.collect::<Vec<_>>());
        for it in &file.items {
            self.e.nl();
            self.item(it, ItemCtx::Top);
        }
        self.emit_tests();
        if !self.root_lints.is_empty() {
            let lines: Vec<String> = self.root_lints.iter().map(|a| format!("#[{}]", a)).collect();
            self.e.prefix_paragraphs(&lines);
        }
    }

    fn ext_imports(&mut self) {
        let mut seen = HashSet::new();
        let traits = self.k.ext_traits.clone();
        for (m, t) in traits {
            if m == self.module && !self.in_test_mod {
                continue;
            }
            if !seen.insert((m.clone(), t.clone())) {
                continue;
            }
            let mut path = String::from("crate");
            for s in &m {
                path.push_str("::");
                path.push_str(s);
            }
            self.e.w(&format!("#[allow(unused_imports)]\nuse {}::{} as _;", path, t));
            self.e.nl();
        }
    }

    fn note_import(&mut self, tree: &UseTree) {
        match tree {
            UseTree::Path { segments, alias } => {
                let name = alias.clone().unwrap_or_else(|| segments.last().cloned().unwrap_or_default());
                if !name.is_empty() && !is_type_like(&name) && name.chars().next().is_some_and(|c| c.is_lowercase()) {
                    self.path_roots.insert(name);
                }
            }
            UseTree::Group { segments, items } => {
                for i in items {
                    match i {
                        // `import std::fs::{self, File}` imports the module `fs`
                        UseTree::Path { segments: s, alias } if s.len() == 1 && s[0] == "self" => {
                            let name = alias.clone().or_else(|| segments.last().cloned()).unwrap_or_default();
                            if !name.is_empty() && name.chars().next().is_some_and(|c| c.is_lowercase()) {
                                self.path_roots.insert(name);
                            }
                        }
                        _ => self.note_import(i),
                    }
                }
            }
            UseTree::Glob { .. } => {}
        }
    }

    fn emit_tests(&mut self) {
        if self.tests.is_empty() {
            return;
        }
        let tests = std::mem::take(&mut self.tests);
        self.e.nl();
        self.e.w("#[cfg(test)]\nmod __oxie_tests {");
        self.e.indent();
        self.e.nl();
        self.e.w("#[allow(unused_imports)]\nuse super::*;");
        self.e.nl();
        self.in_test_mod = true;
        self.ext_imports();
        self.in_test_mod = false;
        let mut used = HashSet::new();
        for (attrs, t) in tests {
            self.e.nl();
            let base = slug(&t.name);
            let mut name = if base.starts_with("test") { base } else { format!("test_{}", base) };
            while self.k.is_fun_name(&name) || !used.insert(name.clone()) {
                name.push('_');
            }
            self.e.w(&format!("/// {}", t.name.replace('\n', " ")));
            self.e.nl();
            let custom = attrs.iter().any(|a| a.name().ends_with("test"));
            self.attrs(&attrs, &[]);
            if !custom {
                self.e.w("#[test]");
                self.e.nl();
            }
            let asyncs = if t.is_async { "async " } else { "" };
            if t.throws {
                self.e.w(&format!("{}fn {}() -> ::std::result::Result<(), {}::AnyError> ", asyncs, name, RT));
            } else {
                self.e.w(&format!("{}fn {}() ", asyncs, name));
            }
            self.fns.push(FnCtx { throws: t.throws, ret: None, is_lambda: false, tries: Vec::new(), text_break: None, unboxed_ret: false });
            self.push_scope();
            self.fn_block(&t.body, None, t.throws);
            self.pop_scope();
            self.fns.pop();
            self.e.nl();
        }
        self.e.dedent();
        self.e.w("}");
        self.e.nl();
    }

    // ------------------------------------------------------------------ attributes & docs

    pub(crate) fn docs(&mut self, docs: &[String]) {
        for d in docs {
            self.e.w(&format!("///{}", d));
            self.e.nl();
        }
    }

    /// Emits attributes, merging any derives with `extra_derives`.
    pub(crate) fn attrs(&mut self, attrs: &[Attr], extra_derives: &[String]) {
        let mut derives: Vec<String> = extra_derives.to_vec();
        let mut rest = Vec::new();
        for a in attrs {
            match a {
                Attr::Oxie { name, args, .. } => match name.as_str() {
                    "derive" => {
                        if let Some(args) = args {
                            for d in args.split(',') {
                                let d = d.trim().to_string();
                                if !d.is_empty() && !derives.contains(&d) {
                                    derives.push(d);
                                }
                            }
                        }
                    }
                    "no_auto_derive" | "from" | "source" => {}
                    _ => rest.push(match args {
                        Some(a) => format!("#[{}({})]", name, a),
                        None => format!("#[{}]", name),
                    }),
                },
                Attr::Raw(s) => {
                    let t = s.trim();
                    if let Some(inner) = t.strip_prefix("derive(").and_then(|x| x.strip_suffix(')')) {
                        for d in inner.split(',') {
                            let d = d.trim().to_string();
                            if !d.is_empty() && !derives.contains(&d) {
                                derives.push(d);
                            }
                        }
                    } else {
                        rest.push(format!("#[{}]", s));
                    }
                }
            }
        }
        if !derives.is_empty() {
            self.e.w(&format!("#[derive({})]", derives.join(", ")));
            self.e.nl();
        }
        for r in rest {
            self.e.w(&r);
            self.e.nl();
        }
    }

    /// Default visibility is crate-wide (`pub(crate)`, Kotlin's `internal`): visible across the
    /// whole project, but not exported from a library unless marked `pub`. Members of a `pub`
    /// class or enum default to `pub`. Keeping items unexported lets the optimizer inline them
    /// exactly like private Rust functions.
    pub(crate) fn vis(&self, v: &Vis, ctx: ItemCtx) -> String {
        match v {
            Vis::Default if ctx == ItemCtx::Local => String::new(),
            Vis::Default if self.members_pub => "pub ".into(),
            Vis::Default => "pub(crate) ".into(),
            Vis::Public => {
                if ctx == ItemCtx::Local {
                    String::new()
                } else {
                    "pub ".into()
                }
            }
            Vis::Private => String::new(),
            Vis::Internal => "pub(crate) ".into(),
            Vis::Raw(s) => format!("{} ", s),
        }
    }

    // ------------------------------------------------------------------ types

    pub(crate) fn is_trait_name(&self, n: &str) -> bool {
        // a class, enum or type alias the module has by that name is no trait here
        if self.class_info(n).is_some() || self.enum_info(n).is_some() || self.aliases.contains(n) {
            return false;
        }
        self.k.is_trait(n) || (STD_AUTO_DYN.contains(&n) && !self.k.is_user_type(n))
    }

    /// Rust text with the relative paths of its `include_str!`, `include_bytes!` and `include!`
    /// calls made absolute: they are relative to the .oxi file, not to the generated code.
    pub(crate) fn include_paths(&self, text: &str) -> String {
        let Some(dir) = &self.src_dir else { return text.to_string() };
        let mut out = String::with_capacity(text.len());
        let mut rest = text;
        while let Some(at) = ["include_str!", "include_bytes!", "include!"].iter().filter_map(|m| rest.find(m).map(|i| (i, m.len()))).min() {
            let (i, len) = at;
            let start = i + len;
            out.push_str(&rest[..start]);
            rest = &rest[start..];
            // `("path")` or `(concat!("path", ..))`
            let lead = rest.len() - rest.trim_start_matches(|c: char| c == '(' || c.is_whitespace()).len();
            let mut lit = lead;
            if rest[lead..].starts_with("concat!(") {
                lit = lead + "concat!(".len();
                lit += rest[lit..].len() - rest[lit..].trim_start().len();
            }
            if !rest[..lead].starts_with('(') || !rest[lit..].starts_with('"') {
                continue;
            }
            let Some(end) = rest[lit + 1..].find('"').map(|e| lit + 1 + e) else { continue };
            let path = &rest[lit + 1..end];
            if path.contains('\\') || std::path::Path::new(path).is_absolute() {
                continue;
            }
            out.push_str(&rest[..lit]);
            out.push_str(&format!("{:?}", dir.join(path).display().to_string().replace('\\', "/")));
            rest = &rest[end + 1..];
        }
        out.push_str(rest);
        out
    }

    /// The class `name` means in this module (of a name several modules declare, the module's
    /// own one or the one it imports).
    pub(crate) fn class_info(&self, name: &str) -> Option<&'a sema::ClassInfo> {
        self.k.class_in(name, &self.module)
    }

    pub(crate) fn enum_info(&self, name: &str) -> Option<&'a sema::EnumInfo> {
        self.k.enum_in(name, &self.module)
    }

    /// The class a path goes through (`Counter`, `a.Counter`).
    pub(crate) fn class_at(&self, path: &[String]) -> Option<&'a sema::ClassInfo> {
        self.k.class_at(path, &self.module)
    }

    pub(crate) fn enum_at(&self, path: &[String]) -> Option<&'a sema::EnumInfo> {
        self.k.enum_at(path, &self.module)
    }

    /// The function `name` means in this module (of a name several modules declare, the module's
    /// own one or the one it imports).
    pub(crate) fn fun_info(&self, name: &str) -> Option<&'a sema::FunInfo> {
        self.k.fun_in(name, &self.module)
    }

    /// The function a path names (`util.total`), if the module it leads to declares it.
    pub(crate) fn fun_at(&self, path: &[String]) -> Option<&'a sema::FunInfo> {
        self.k.fun_at(path, &self.module)
    }

    /// Automatic derives of the data class or enum `name` declared in this module.
    fn auto_derives(&self, name: &str) -> Vec<String> {
        self.k.derives.get(&self.k.decl_key(&self.module, name)).cloned().unwrap_or_default()
    }

    pub fn ty(&self, t: &Type, pos: TyPos) -> String {
        match t {
            Type::Path { global, segs, .. } => {
                // bare trait used as a type
                if segs.len() == 1 && segs[0].fn_sugar.is_none() && self.is_trait_name(&segs[0].name) && pos != TyPos::Plain {
                    let inner = self.path_ty(*global, segs, TyPos::Generic);
                    return self.trait_obj(&inner, pos);
                }
                if segs.len() == 1 && segs[0].fn_sugar.is_some() && pos != TyPos::Plain {
                    let inner = self.path_ty(*global, segs, TyPos::Generic);
                    return self.trait_obj(&inner, pos);
                }
                self.path_ty(*global, segs, pos)
            }
            Type::Qualified { ty, trait_, rest } => {
                let mut s = format!("<{}", self.ty(ty, TyPos::Plain));
                if let Some(t) = trait_ {
                    s.push_str(" as ");
                    s.push_str(&self.ty(t, TyPos::Plain));
                }
                s.push('>');
                for seg in rest {
                    s.push_str("::");
                    s.push_str(&self.seg(seg, TyPos::Generic));
                }
                s
            }
            Type::Ref { lifetime, mutable, inner } => {
                let mut s = String::from("&");
                if let Some(l) = lifetime {
                    s.push_str(&format!("'{} ", l));
                }
                if *mutable {
                    s.push_str("mut ");
                }
                s.push_str(&self.pointee(inner));
                s
            }
            Type::Ptr { mutable, inner } => format!("*{} {}", if *mutable { "mut" } else { "const" }, self.pointee(inner)),
            Type::Tuple(v) => {
                if v.len() == 1 {
                    format!("({},)", self.ty(&v[0], TyPos::Generic))
                } else {
                    format!("({})", v.iter().map(|t| self.ty(t, TyPos::Generic)).collect::<Vec<_>>().join(", "))
                }
            }
            Type::Array(inner, n) => format!("[{}; {}]", self.ty(inner, TyPos::Generic), n),
            Type::Slice(inner) => format!("[{}]", self.ty(inner, TyPos::Generic)),
            Type::Option(inner) => format!("Option<{}>", self.ty(inner, TyPos::Generic)),
            Type::Fn { params, ret } => {
                let ps = params.iter().map(|t| self.ty(t, TyPos::Generic)).collect::<Vec<_>>().join(", ");
                let r = if ret.is_unit() { String::new() } else { format!(" -> {}", self.ty(ret, TyPos::Ret)) };
                let base = match pos {
                    TyPos::Param => "FnMut",
                    _ => "Fn",
                };
                let sig = format!("{}({}){}", base, ps, r);
                self.trait_obj(&sig, pos)
            }
            Type::Dyn(b) => format!("dyn {}", self.bounds(b)),
            Type::Impl(b) => format!("impl {}", self.bounds(b)),
            Type::Bounds(b) => self.bounds(b),
            Type::Lifetime(l) => format!("'{}", l),
            Type::Maybe(t) => format!("?{}", self.ty(t, TyPos::Plain)),
            Type::Never => "!".into(),
            Type::Infer => "_".into(),
            Type::Raw(s) => s.clone(),
        }
    }

    fn trait_obj(&self, inner: &str, pos: TyPos) -> String {
        match pos {
            TyPos::Param | TyPos::Ret => format!("impl {}", inner),
            TyPos::Container => format!("dyn {}", inner),
            TyPos::Field | TyPos::Generic | TyPos::Local => format!("Box<dyn {}>", inner),
            TyPos::Plain => inner.to_string(),
        }
    }

    fn path_ty(&self, global: bool, segs: &[PathSeg], pos: TyPos) -> String {
        if segs.len() > 1 && self.k.nested.get(&segs[0].name).is_some_and(|inner| inner.contains(&segs[1].name)) {
            return self.path_ty(global, &segs[1..], pos);
        }
        let mut s = String::new();
        if global {
            s.push_str("::");
        }
        // crate-relative module paths
        if segs.len() > 1 && self.k.root_module(&segs[0].name, &self.module) && !self.is_local(&segs[0].name) {
            s.push_str("crate::");
        }
        for (i, seg) in segs.iter().enumerate() {
            if i > 0 {
                s.push_str("::");
            }
            if segs.len() == 1 && seg.generics.is_none() {
                if let Some(a) = kotlin_alias(&seg.name) {
                    if !self.k.is_user_type(&seg.name) {
                        s.push_str(a);
                        continue;
                    }
                }
                // `Token` for `data class Token<'a>`: spell the elided lifetime in signatures
                let n = self.k.type_lifetimes_in(&seg.name, &self.module);
                if self.in_signature.get() && n > 0 {
                    s.push_str(&esc(&seg.name));
                    s.push_str(&format!("<{}>", vec!["'_"; n].join(", ")));
                    continue;
                }
            }
            let _ = pos;
            s.push_str(&self.seg(seg, TyPos::Generic));
        }
        s
    }

    /// The type behind `&` or `*`: `dyn A + B` needs parentheses there (`&(dyn Error + 'static)`).
    fn pointee(&self, inner: &Type) -> String {
        let t = self.ty(inner, TyPos::Container);
        match inner {
            Type::Dyn(b) | Type::Impl(b) | Type::Bounds(b) if b.len() > 1 => format!("({})", t),
            _ => t,
        }
    }

    pub(crate) fn seg(&self, seg: &PathSeg, _pos: TyPos) -> String {
        // `impl Trait + use<'a, T>` says what the type captures; `use` is no name there
        let mut s = if seg.name == "use" && seg.generics.is_some() { seg.name.clone() } else { esc(&seg.name) };
        let container = CONTAINERS.contains(&seg.name.as_str());
        if let Some(g) = &seg.generics {
            s.push('<');
            let parts: Vec<String> = g
                .iter()
                .map(|a| match a {
                    GenericArg::Type(t) => self.ty(t, if container { TyPos::Container } else { TyPos::Generic }),
                    GenericArg::Lifetime(l) => format!("'{}", l),
                    GenericArg::Binding(n, t) => format!("{} = {}", n, self.ty(t, TyPos::Generic)),
                    GenericArg::Const(c) => c.clone(),
                })
                .collect();
            s.push_str(&parts.join(", "));
            s.push('>');
        }
        if let Some((params, ret)) = &seg.fn_sugar {
            s.push('(');
            s.push_str(&params.iter().map(|t| self.ty(t, TyPos::Generic)).collect::<Vec<_>>().join(", "));
            s.push(')');
            if let Some(r) = ret {
                if !r.is_unit() {
                    s.push_str(" -> ");
                    s.push_str(&self.ty(r, TyPos::Ret));
                }
            }
        }
        s
    }

    fn bounds(&self, b: &[Type]) -> String {
        b.iter()
            .map(|t| match t {
                Type::Fn { params, ret } => {
                    let ps = params.iter().map(|t| self.ty(t, TyPos::Generic)).collect::<Vec<_>>().join(", ");
                    if ret.is_unit() {
                        format!("FnMut({})", ps)
                    } else {
                        format!("FnMut({}) -> {}", ps, self.ty(ret, TyPos::Ret))
                    }
                }
                _ => self.ty(t, TyPos::Plain),
            })
            .collect::<Vec<_>>()
            .join(" + ")
    }

    fn generics_decl(&self, g: &Generics) -> String {
        if g.params.is_empty() {
            return String::new();
        }
        let parts: Vec<String> = g
            .params
            .iter()
            .map(|p| match p {
                GenericParam::Lifetime { name, bounds } => {
                    if bounds.is_empty() {
                        format!("'{}", name)
                    } else {
                        format!("'{}: {}", name, bounds.join(" + "))
                    }
                }
                GenericParam::Type { name, bounds, default } => {
                    let mut s = name.clone();
                    if !bounds.is_empty() {
                        s.push_str(": ");
                        s.push_str(&self.bounds(bounds));
                    }
                    if let Some(d) = default {
                        s.push_str(" = ");
                        s.push_str(&self.ty(d, TyPos::Generic));
                    }
                    s
                }
                GenericParam::Const { name, ty, default } => {
                    let mut s = format!("const {}: {}", name, self.ty(ty, TyPos::Plain));
                    if let Some(d) = default {
                        s.push_str(" = ");
                        s.push_str(d);
                    }
                    s
                }
            })
            .collect();
        format!("<{}>", parts.join(", "))
    }

    fn generics_names(&self, g: &Generics) -> String {
        if g.params.is_empty() {
            return String::new();
        }
        let parts: Vec<String> = g
            .params
            .iter()
            .map(|p| match p {
                GenericParam::Lifetime { name, .. } => format!("'{}", name),
                GenericParam::Type { name, .. } | GenericParam::Const { name, .. } => name.clone(),
            })
            .collect();
        format!("<{}>", parts.join(", "))
    }

    fn where_clause(&self, g: &Generics) -> String {
        if g.where_clause.is_empty() {
            return String::new();
        }
        let parts: Vec<String> = g
            .where_clause
            .iter()
            .map(|w| match w {
                WherePred::Type { ty, bounds } => format!("{}: {}", self.ty(ty, TyPos::Plain), self.bounds(bounds)),
                WherePred::Lifetime { name, bounds } => format!("'{}: {}", name, bounds.join(" + ")),
            })
            .collect();
        format!(" where {}", parts.join(", "))
    }

    // ------------------------------------------------------------------ patterns

    /// A pattern after `@` or `&`, where alternatives need parentheses (`q @ (b'"' | b'\'')`), and
    /// after `&` ranges too (`&(b'0'..=b'9')`).
    fn pat_operand(&mut self, p: &Pat, after_ref: bool) -> String {
        match p {
            Pat::Or(_) => format!("({})", self.pat(p)),
            Pat::Range { .. } if after_ref => format!("({})", self.pat(p)),
            _ => self.pat(p),
        }
    }

    pub(crate) fn pat(&mut self, p: &Pat) -> String {
        match p {
            Pat::Wild => "_".into(),
            Pat::Rest => "..".into(),
            Pat::Null => "None".into(),
            Pat::Ident { name, by_ref, mutable, sub, .. } => {
                let mut s = String::new();
                if *by_ref {
                    s.push_str("ref ");
                }
                if *mutable {
                    s.push_str("mut ");
                }
                s.push_str(&esc(name));
                if let Some(sub) = sub {
                    s.push_str(" @ ");
                    s.push_str(&self.pat_operand(sub, false));
                }
                s
            }
            Pat::Lit(e) => self.lit_text(e),
            Pat::Range { lo, hi, inclusive } => {
                let lo = lo.as_ref().map(|e| self.lit_text(e)).unwrap_or_default();
                let hi = hi.as_ref().map(|e| self.lit_text(e)).unwrap_or_default();
                format!("{}{}{}", lo, if *inclusive { "..=" } else { ".." }, hi)
            }
            Pat::Tuple(v) => {
                let parts: Vec<String> = v.iter().map(|p| self.pat(p)).collect();
                if parts.len() == 1 {
                    format!("({},)", parts[0])
                } else {
                    format!("({})", parts.join(", "))
                }
            }
            Pat::Slice(v) => format!("[{}]", v.iter().map(|p| self.pat(p)).collect::<Vec<_>>().join(", ")),
            Pat::Ref { mutable, pat } => format!("&{}{}", if *mutable { "mut " } else { "" }, self.pat_operand(pat, true)),
            Pat::Or(v) => v.iter().map(|p| self.pat(p)).collect::<Vec<_>>().join(" | "),
            Pat::Typed { pat, ty } => format!("{}: {}", self.pat(pat), self.ty(ty, TyPos::Local)),
            Pat::Raw(s) => s.clone(),
            Pat::Path(segs, _) => {
                let (path, shape) = self.resolve_variant_path(segs);
                match shape {
                    Some(VariantShape::Named(_)) => format!("{} {{ .. }}", path),
                    Some(VariantShape::Tuple(_)) => format!("{}(..)", path),
                    _ => path,
                }
            }
            Pat::TupleStruct { path, elems, .. } => {
                let (p, shape) = self.resolve_variant_path(path);
                if let Some(VariantShape::Named(fields)) = shape {
                    let mut parts = Vec::new();
                    let mut rest = false;
                    for (i, e) in elems.iter().enumerate() {
                        if matches!(e, Pat::Rest) {
                            rest = true;
                            continue;
                        }
                        if let Some((fname, _)) = fields.get(i) {
                            let sub = self.pat(e);
                            if sub == esc(fname) {
                                parts.push(sub);
                            } else {
                                parts.push(format!("{}: {}", esc(fname), sub));
                            }
                        }
                    }
                    if rest || elems.len() < fields.len() {
                        parts.push("..".into());
                    }
                    return format!("{} {{ {} }}", p, parts.join(", "));
                }
                format!("{}({})", p, elems.iter().map(|e| self.pat(e)).collect::<Vec<_>>().join(", "))
            }
            Pat::Struct { path, fields, rest, .. } => {
                let (p, _) = self.resolve_variant_path(path);
                let mut parts: Vec<String> = fields
                    .iter()
                    .map(|(n, sub)| match sub {
                        Some(sp) => format!("{}: {}", esc(n), self.pat(sp)),
                        None => esc(n),
                    })
                    .collect();
                if *rest {
                    parts.push("..".into());
                }
                if parts.is_empty() {
                    format!("{} {{}}", p)
                } else {
                    format!("{} {{ {} }}", p, parts.join(", "))
                }
            }
        }
    }

    /// Resolves `Circle`, `Shape.Circle`, `Shape::Circle` to a qualified variant path.
    pub(crate) fn resolve_variant_path(&self, segs: &[String]) -> (String, Option<VariantShape>) {
        if segs.len() == 1 {
            let n = &segs[0];
            if !self.k.is_class_name(n) && !self.is_local(n) {
                if let Some(en) = self.k.variant_enum_in(n, Some(&self.module)) {
                    let shape = en.variant(n).cloned();
                    return (format!("{}::{}", en.name, n), shape);
                }
            }
            return (esc(n), None);
        }
        let mut path = self.join_path(segs);
        let en_name = &segs[segs.len() - 2];
        let vname = &segs[segs.len() - 1];
        let shape = if en_name == "Self" {
            self.class.as_ref().and_then(|c| self.enum_info(&c.name)).and_then(|e| e.variant(vname).cloned())
        } else {
            self.enum_at(&segs[..segs.len() - 1]).and_then(|e| e.variant(vname).cloned())
        };
        if path.is_empty() {
            path = segs.join("::");
        }
        (path, shape)
    }

    pub(crate) fn join_path(&self, segs: &[String]) -> String {
        // `Outer.Inner` refers to a type declared inside a class, which Oxie hoists next to it
        if segs.len() > 1 && self.k.nested.get(&segs[0]).is_some_and(|inner| inner.contains(&segs[1])) {
            return self.join_path(&segs[1..]);
        }
        let mut s = String::new();
        if segs.len() > 1 && self.k.root_module(&segs[0], &self.module) && !self.is_local(&segs[0]) {
            if self.k.has_lib && self.is_root && self.module.is_empty() && self.is_main_file() {
                s.push_str(&self.k.crate_name.replace('-', "_"));
                s.push_str("::");
            } else {
                s.push_str("crate::");
            }
        }
        let parts: Vec<String> = segs.iter().map(|x| if x.is_empty() { String::new() } else { esc(x) }).collect();
        s.push_str(&parts.join("::"));
        s
    }

    fn is_main_file(&self) -> bool {
        false
    }

    /// Literal-ish expression text (used in patterns and const contexts).
    pub(crate) fn lit_text(&mut self, e: &Expr) -> String {
        match &e.kind {
            ExprKind::Int(s) | ExprKind::Float(s) | ExprKind::Char(s) | ExprKind::RawStr(s) => s.clone(),
            ExprKind::Bool(b) => b.to_string(),
            ExprKind::Str(parts) => format!("\"{}\"", lit_parts(parts)),
            ExprKind::Unary(UnOp::Neg, x) => format!("-{}", self.lit_text(x)),
            ExprKind::Path { segs, .. } => self.resolve_variant_path(segs).0,
            ExprKind::Null => "None".into(),
            _ => {
                self.error(e.span, "expected a literal");
                "_".into()
            }
        }
    }

    // ------------------------------------------------------------------ items

    pub(crate) fn item(&mut self, it: &Item, ctx: ItemCtx) {
        self.e.mark(it.span);
        match &it.kind {
            ItemKind::Fun(f) if f.receiver.is_some() => self.ext_fun(it, f, ctx),
            ItemKind::Fun(f) => self.free_fun(it, f, ctx),
            ItemKind::Type(t) => match t.kind {
                TypeKind::Enum => self.enum_decl(it, t, ctx),
                _ => self.class_decl(it, t, ctx),
            },
            ItemKind::Trait(t) => self.trait_decl(it, t, ctx),
            ItemKind::Impl(imp) => self.impl_decl(it, imp),
            ItemKind::Import(i) => {
                self.note_import(&i.tree);
                self.docs(&it.docs);
                self.attrs(&it.attrs, &[]);
                let vis = match it.vis {
                    Vis::Public => "pub ".to_string(),
                    Vis::Internal => "pub(crate) ".to_string(),
                    Vis::Raw(ref s) => format!("{} ", s),
                    _ => String::new(),
                };
                let tree = self.use_tree(&i.tree, true);
                self.e.w(&format!("{}use {};", vis, tree));
                self.e.nl();
            }
            ItemKind::Const(c) | ItemKind::Static(c) => {
                let is_static = matches!(it.kind, ItemKind::Static(_));
                self.docs(&it.docs);
                self.attrs(&it.attrs, &[]);
                let vis = self.vis(&it.vis, ctx);
                let ty = match &c.ty {
                    Some(t) => self.ty(t, TyPos::Plain),
                    None => match c.value.as_ref().and_then(infer_lit_type) {
                        Some(t) => t,
                        None => {
                            self.error(it.span, format!("constant `{}` needs a type annotation", c.name));
                            "_".into()
                        }
                    },
                };
                let kw = if is_static {
                    if c.mutable {
                        "static mut"
                    } else {
                        "static"
                    }
                } else {
                    "const"
                };
                self.e.w(&format!("{}{} {}: {}", vis, kw, c.name, ty));
                if let Some(v) = &c.value {
                    self.e.w(" = ");
                    self.expr(v, P_LOWEST);
                }
                self.e.w(";");
                self.e.nl();
            }
            ItemKind::TypeAlias { name, generics, ty } => {
                self.docs(&it.docs);
                self.attrs(&it.attrs, &[]);
                let vis = self.vis(&it.vis, ctx);
                let g = self.generics_decl(generics);
                let t = self.ty(ty, TyPos::Field);
                self.e.w(&format!("{}type {}{} = {};", vis, name, g, t));
                self.e.nl();
            }
            ItemKind::Mod { name, items } => {
                self.docs(&it.docs);
                self.attrs(&it.attrs, &[]);
                let vis = self.vis(&it.vis, ctx);
                self.e.w(&format!("{}mod {} {{", vis, name));
                self.e.indent();
                self.e.nl();
                self.e.w("#[allow(unused_imports)]\nuse crate::__oxie::prelude::*;");
                self.e.nl();
                self.module.push(name.clone());
                for i in items {
                    self.e.nl();
                    self.item(i, ItemCtx::Top);
                }
                self.module.pop();
                self.e.dedent();
                self.e.w("}");
                self.e.nl();
            }
            ItemKind::Object { name, items } => {
                self.docs(&it.docs);
                self.attrs(&it.attrs, &[]);
                let vis = self.vis(&it.vis, ctx);
                self.e.w(&format!("#[allow(non_snake_case)]\n{}mod {} {{", vis, name));
                self.e.indent();
                self.e.nl();
                self.e.w("#[allow(unused_imports)]\nuse super::*;");
                self.e.nl();
                for i in items {
                    self.e.nl();
                    self.item(i, ItemCtx::Object);
                }
                self.e.dedent();
                self.e.w("}");
                self.e.nl();
            }
            ItemKind::Test(t) => {
                self.tests.push((it.attrs.clone(), t.clone()));
            }
            ItemKind::Raw(s) => {
                // `mod name` refers to `name.rs` next to the .oxi file, not to the generated code;
                // with a `name.oxi`, Oxie declares the module itself
                let declared = s.strip_prefix("mod ").and_then(|r| r.strip_suffix(';')).map(|name| {
                    let mut path = self.module.clone();
                    path.push(name.to_string());
                    path
                });
                if declared.is_some_and(|path| self.k.module_paths.contains(&path)) {
                    return;
                }
                let text = match (s.strip_prefix("mod ").and_then(|r| r.strip_suffix(';')), &self.src_dir) {
                    (Some(name), Some(dir)) if name.chars().all(|c| c.is_alphanumeric() || c == '_') => {
                        let file = if dir.join(format!("{}.rs", name)).exists() { dir.join(format!("{}.rs", name)) } else { dir.join(name).join("mod.rs") };
                        format!("#[path = \"{}\"]\nmod {};", file.display().to_string().replace('\\', "/"), name)
                    }
                    _ => s.clone(),
                };
                let text = self.include_paths(&text);
                self.e.raw_lines(&text);
                self.e.nl();
            }
            ItemKind::Macro(s) => {
                self.docs(&it.docs);
                self.attrs(&it.attrs, &[]);
                let s = self.include_paths(s);
                self.e.raw_lines(&s);
                self.e.nl();
            }
        }
    }

    fn use_tree(&self, t: &UseTree, top: bool) -> String {
        let prefix = |segs: &[String]| -> String {
            if top && !segs.is_empty() && self.k.root_module(&segs[0], &self.module) {
                format!("crate::{}", segs.iter().map(|s| esc(s)).collect::<Vec<_>>().join("::"))
            } else {
                segs.iter().map(|s| if s.is_empty() { String::new() } else { esc(s) }).collect::<Vec<_>>().join("::")
            }
        };
        match t {
            UseTree::Path { segments, alias } => {
                let p = prefix(segments);
                match alias {
                    Some(a) => format!("{} as {}", p, a),
                    None => p,
                }
            }
            UseTree::Glob { segments } => {
                if segments.is_empty() {
                    "*".into()
                } else {
                    format!("{}::*", prefix(segments))
                }
            }
            UseTree::Group { segments, items } => {
                let inner: Vec<String> = items.iter().map(|i| self.use_tree(i, segments.is_empty() && top)).collect();
                if segments.is_empty() {
                    format!("{{{}}}", inner.join(", "))
                } else {
                    format!("{}::{{{}}}", prefix(segments), inner.join(", "))
                }
            }
        }
    }

    // ------------------------------------------------------------------ functions

    fn ret_type(&self, f: &FunDecl, pos: TyPos) -> Option<String> {
        self.in_signature.set(true);
        let r = self.ret_type_inner(f, pos);
        self.in_signature.set(false);
        r
    }

    fn ret_type_inner(&self, f: &FunDecl, pos: TyPos) -> Option<String> {
        let value = f.ret.as_ref().filter(|t| !t.is_unit());
        match &f.throws {
            // an interface returned from several branches as different classes is boxed
            Throws::No if pos == TyPos::Ret && self.iface_ret(f) == Some(true) => value.map(|t| self.ty(t, TyPos::Field)),
            Throws::No => value.map(|t| self.ty(t, pos)),
            Throws::Any => Some(format!(
                "::std::result::Result<{}, {}::AnyError>",
                value.map(|t| self.ty(t, TyPos::Generic)).unwrap_or_else(|| "()".into()),
                RT
            )),
            Throws::Ty(e) => Some(format!(
                "::std::result::Result<{}, {}>",
                value.map(|t| self.ty(t, TyPos::Generic)).unwrap_or_else(|| "()".into()),
                self.ty(e, TyPos::Generic)
            )),
        }
    }

    fn params_text(&mut self, f: &FunDecl, recv: Option<String>) -> String {
        self.params_parts(f, recv).join(", ")
    }

    /// The receiver (if any) and each parameter, as written in the signature.
    fn params_parts(&mut self, f: &FunDecl, recv: Option<String>) -> Vec<String> {
        self.in_signature.set(true);
        let parts = self.params_parts_inner(f, recv);
        self.in_signature.set(false);
        parts
    }

    /// Writes the parameter list `(..)`, each parameter followed by the decision about it: an
    /// inferred `&mut self` (`recv_why`), a `String`/`Vec` parameter lent or kept.
    fn write_params(&mut self, f: &FunDecl, parts: &[String], recv_why: Option<(String, Option<Span>)>) {
        let has_recv = parts.len() > f.params.len();
        self.e.w("(");
        for (i, part) in parts.iter().enumerate() {
            if i > 0 {
                self.e.w(", ");
            }
            self.e.w(part);
            if i == 0 && has_recv {
                if let Some((why, cause)) = &recv_why {
                    self.e.decide(f.name_span, part.clone(), why.clone(), *cause);
                }
                continue;
            }
            let index = i - has_recv as usize;
            if let Some(n) = self.param_notes.get(&(f.name_span, index)) {
                let chosen = if n.borrowed { part.split_once(": ").map_or(part.as_str(), |(_, t)| t).to_string() } else { "owned".to_string() };
                let (why, cause) = (n.why.clone(), n.cause);
                self.e.decide(f.params[index].span, chosen, why, cause);
            }
        }
        if let Some(extra) = self.extra_param.take() {
            if !parts.is_empty() {
                self.e.w(", ");
            }
            self.e.w(&extra);
        }
        self.e.w(")");
    }

    /// After the return type: why the signature got a lifetime that the source doesn't name
    /// (`orig_lifetimes` were written), tying the result to every reference parameter.
    fn lifetime_decision(&mut self, f: &FunDecl, orig_lifetimes: &[String]) {
        let mentions = |t: &str, lt: &str| t.match_indices(lt).any(|(i, _)| !t[i + lt.len()..].starts_with(|c: char| c.is_alphanumeric() || c == '_'));
        let ret = f.ret.as_ref().map(|r| self.ty(r, TyPos::Ret)).unwrap_or_default();
        // the one the result got (an `impl Trait` parameter's own isn't explained)
        let Some(lt) = f
            .generics
            .params
            .iter()
            .filter_map(|g| if let GenericParam::Lifetime { name, .. } = g { Some(format!("'{}", name)) } else { None })
            .find(|l| !orig_lifetimes.iter().any(|o| *l == format!("'{}", o)) && mentions(&ret, l))
        else {
            return;
        };
        // the parameters it was given to (not those with lifetimes of their own)
        let has_lt = |t: &str| mentions(t, &lt);
        let tied: Vec<String> = f.params.iter().filter(|p| has_lt(&self.ty(&p.ty, TyPos::Param))).filter_map(|p| p.name().map(|n| format!("`{}`", n))).collect();
        if !tied.is_empty() {
            self.e.decide(f.name_span, lt, format!("the result borrows from {}", tied.join(" or ")), None);
        }
    }

    fn params_parts_inner(&mut self, f: &FunDecl, recv: Option<String>) -> Vec<String> {
        let mut parts = Vec::new();
        if let Some(r) = recv {
            parts.push(r);
        }
        for p in &f.params {
            let fn_typed = matches!(p.ty, Type::Fn { .. });
            let mut pat = self.pat(&p.pat);
            let mut pos = TyPos::Param;
            if fn_typed {
                let captured = p.name().is_some_and(|n| f.body.as_ref().is_some_and(|b| visit::ident_in_lambda(b, n)));
                let called = p.name().is_some_and(|n| f.body.as_ref().is_some_and(|b| visit::calls_ident(b, n)));
                if captured {
                    // used from a closure: `impl Fn` keeps that closure callable without `mut`
                    pos = TyPos::Ret;
                } else if called && !pat.starts_with("mut ") {
                    pat = format!("mut {}", pat);
                }
            }
            let ty = self.ty(&p.ty, pos);
            parts.push(format!("{}: {}", pat, ty));
        }
        parts
    }

    fn fn_header(&self, f: &FunDecl) -> String {
        let mut s = String::new();
        if f.mods.is_const {
            s.push_str("const ");
        }
        if f.mods.is_async {
            s.push_str("async ");
        }
        if f.mods.is_unsafe {
            s.push_str("unsafe ");
        }
        if let Some(abi) = &f.mods.abi {
            s.push_str(&format!("extern \"{}\" ", abi));
        }
        s
    }

    fn bind_params(&mut self, f: &FunDecl) {
        for p in &f.params {
            self.bind_pat(&p.pat, Some(p.ty.clone()));
        }
    }

    fn free_fun(&mut self, it: &Item, f: &FunDecl, ctx: ItemCtx) {
        let lifetimes: Vec<String> = f.generics.params.iter().filter_map(|g| if let GenericParam::Lifetime { name, .. } = g { Some(name.clone()) } else { None }).collect();
        let f = &auto_lifetimes(&self.with_inferred_ret(f, None), false, &|n| self.k.type_lifetimes_in(n, &self.module));
        let f = &crate::bounds::with_inferred_bounds(f, &[], &[]);
        let is_main = f.name == "main" && ctx == ItemCtx::Top && self.is_root && self.module.is_empty();
        let wrap_main = is_main && !matches!(f.throws, Throws::No);
        self.docs(&it.docs);
        let (main_attrs, other_attrs): (Vec<Attr>, Vec<Attr>) = if wrap_main {
            it.attrs.iter().cloned().partition(|a| a.name().ends_with("main"))
        } else {
            (Vec::new(), it.attrs.clone())
        };
        self.attrs(&other_attrs, &[]);
        let vis = if is_main { String::new() } else { self.vis(&it.vis, ctx) };
        let name = if wrap_main { "__oxie_main".to_string() } else { esc(&f.name) };
        let header = self.fn_header(f);
        let g = self.generics_decl(&f.generics);
        self.push_scope();
        let params = self.params_parts(f, None);
        let ret = self.ret_type(f, TyPos::Ret);
        let wc = self.where_clause(&f.generics);
        self.e.w(&format!("{}{}fn {}{}", vis, header, name, g));
        self.write_params(f, &params, None);
        if let Some(r) = &ret {
            self.e.w(&format!(" -> {}", r));
        }
        self.lifetime_decision(f, &lifetimes);
        self.e.w(&wc);
        self.bind_params(f);
        match &f.body {
            Some(body) => {
                self.e.w(" ");
                self.fun_body(f, body);
            }
            None => self.e.w(";"),
        }
        self.pop_scope();
        self.e.nl();
        if wrap_main {
            self.e.nl();
            // the wrapper's frame in a backtrace points at `fun main`
            self.e.mark(it.span);
            self.attrs(&main_attrs, &[]);
            let asyncs = if f.mods.is_async { "async " } else { "" };
            let aw = if f.mods.is_async { ".await" } else { "" };
            self.e.w(&format!(
                "{}fn main() {{\n    if let Err(err) = __oxie_main(){} {{\n        {}::report(err);\n        ::std::process::exit(1);\n    }}\n}}",
                asyncs, aw, RT
            ));
            self.e.nl();
        }
    }

    fn fun_body(&mut self, f: &FunDecl, body: &FunBody) {
        let throws = !matches!(f.throws, Throws::No);
        let ret = f.ret.clone().filter(|t| !t.is_unit());
        let unboxed_ret = !throws && self.iface_ret(f).is_some_and(|boxed| !boxed);
        self.fns.push(FnCtx { throws, ret: ret.clone(), is_lambda: false, tries: Vec::new(), text_break: None, unboxed_ret });
        let by_value = std::mem::replace(&mut self.self_by_value, matches!(f.self_param, Some(SelfParam::Value { .. })));
        match body {
            FunBody::Block(b) => self.fn_block(b, ret.as_ref(), throws),
            FunBody::Expr(e) => {
                let b = Block { stmts: vec![Stmt::Expr { expr: e.clone(), semi: false }], span: e.span };
                // the value of `= expr` without a return type (`items.push(x)`) must be `()`:
                // anything else is an error asking for the type, never dropped
                // (`fun opt(x: i32) = Some(x)`)
                if ret.is_none() && !throws && !e.diverges() && !e.is_block_like() {
                    let unit = Target { ty: None, wrap: None, fn_result: true };
                    self.fn_block_with(&b, Some(&unit), false);
                } else {
                    self.fn_block(&b, ret.as_ref(), throws);
                }
            }
        }
        self.self_by_value = by_value;
        self.fns.pop();
    }

    /// Emits a function body block: tail value for non-unit functions, `Ok(())` for unit throwing ones.
    pub(crate) fn fn_block(&mut self, b: &Block, ret: Option<&Type>, throws: bool) {
        let target = ret.map(|t| Target { ty: Some(t.clone()), wrap: if throws { Some("Ok".into()) } else { None }, fn_result: true });
        self.fn_block_with(b, target.as_ref(), throws);
    }

    /// `fn_block` with the target of the last expression: `None` makes it a statement.
    fn fn_block_with(&mut self, b: &Block, target: Option<&Target>, throws: bool) {
        // generated names are numbered per function, so a change renumbers only its function
        let outer_tmp = std::mem::take(&mut self.tmp);
        let outer = self.scopes.iter().flat_map(|s| s.keys().cloned()).collect();
        self.reused_iters.extend(visit::reused_loop_iters(outer, b));
        self.e.w("{");
        self.e.indent();
        self.push_scope();
        match target {
            Some(target) => {
                self.stmts_with_tail(&b.stmts, Some(target));
            }
            None => {
                self.stmts_with_tail(&b.stmts, None);
                if throws && !ends_diverging(&b.stmts) {
                    self.e.nl();
                    self.e.w("Ok(())");
                }
            }
        }
        self.pop_scope();
        self.e.dedent();
        self.e.nl();
        self.e.w("}");
        self.tmp = outer_tmp;
    }

    fn ext_fun(&mut self, it: &Item, f: &FunDecl, ctx: ItemCtx) {
        let by_ref = !matches!(f.self_param, Some(SelfParam::Value { .. }) | Some(SelfParam::Typed(_)));
        let f = &auto_lifetimes(&self.with_inferred_ret(f, None), by_ref, &|n| self.k.type_lifetimes_in(n, &self.module));
        let f = &crate::bounds::with_inferred_bounds(f, &[], &[]);
        let recv_ty = f.receiver.as_ref().unwrap();
        let tname = sema::ext_trait_name(&self.module, &f.name);
        let recv = match &f.self_param {
            Some(SelfParam::Value { mutable }) => if *mutable { "mut self" } else { "self" }.to_string(),
            Some(SelfParam::RefMut) => "&mut self".into(),
            Some(SelfParam::Typed(t)) => format!("self: {}", self.ty(t, TyPos::Plain)),
            _ if f.mods.is_mut => "&mut self".into(),
            _ => "&self".into(),
        };
        let g = self.generics_decl(&f.generics);
        let gn = self.generics_names(&f.generics);
        let wc = self.where_clause(&f.generics);
        let header = self.fn_header(f);
        self.push_scope();
        let params = self.params_text(f, Some(recv));
        let ret = self.ret_type(f, TyPos::Ret);
        let sig = format!(
            "{}fn {}({}){}",
            header,
            esc(&f.name),
            params,
            ret.as_ref().map(|r| format!(" -> {}", r)).unwrap_or_default()
        );
        let vis = self.vis(&it.vis, ctx);
        self.docs(&it.docs);
        self.e.w(&format!("#[allow(non_camel_case_types)]\n{}trait {}{}{} {{\n    {};\n}}", vis, tname, g, wc, sig));
        self.e.nl();
        self.e.nl();
        self.e.w(&format!("impl{} {}{} for {}{} {{", g, tname, gn, self.ty(recv_ty, TyPos::Plain), wc));
        self.e.indent();
        self.e.nl();
        self.attrs(&it.attrs, &[]);
        self.e.w(&sig);
        self.bind_params(f);
        // inside an extension of an Oxie type, its fields and methods are in scope (like Kotlin)
        let saved_class = self.class.take();
        if let Some(owner) = recv_ty.single_name() {
            if self.class_info(owner).is_some() || self.enum_info(owner).is_some() {
                let mut ctx = self.class_ctx_for(owner, &[], &[]);
                ctx.is_static = matches!(f.self_param, Some(SelfParam::Typed(_)));
                self.class = Some(ctx);
            }
        }
        if let Some(body) = &f.body {
            self.e.w(" ");
            self.fun_body(f, body);
        } else {
            self.error(f.name_span, "extension functions need a body");
        }
        self.class = saved_class;
        self.pop_scope();
        self.e.dedent();
        self.e.nl();
        self.e.w("}");
        self.e.nl();
    }

    /// Emits a method inside an impl/trait block.
    pub(crate) fn method(&mut self, it: &Item, f: &FunDecl, recv: Recv, vis_allowed: bool, rename: Option<&str>) {
        let trait_ret = self.pending_trait_ret.take();
        let lifetimes: Vec<String> = f.generics.params.iter().filter_map(|g| if let GenericParam::Lifetime { name, .. } = g { Some(name.clone()) } else { None }).collect();
        // an inferred `&mut self` says why
        let recv_why = (recv == Recv::RefMut && f.self_param.is_none() && !f.mods.is_mut).then(|| self.class.as_ref()?.methods.get(&f.name)?.recv_why.clone()).flatten();
        let by_ref = match &f.self_param {
            Some(SelfParam::Ref) | Some(SelfParam::RefMut) => true,
            Some(_) => false,
            None => matches!(recv, Recv::Ref | Recv::RefMut),
        };
        let f = &auto_lifetimes(&self.with_inferred_ret(f, trait_ret), by_ref, &|n| self.k.type_lifetimes_in(n, &self.module));
        let f = &match self.class.as_ref() {
            Some(c) if !f.mods.is_override && c.trait_methods.as_ref().is_some_and(|t| !t.contains(&f.name)) => {
                let fields: Vec<(String, Type)> = c.fields.iter().map(|(n, t)| (n.clone(), t.clone())).collect();
                crate::bounds::with_inferred_bounds(f, &c.generics, &fields)
            }
            _ => f.clone(),
        };
        self.e.mark(it.span);
        self.docs(&it.docs);
        let attrs: Vec<Attr> = it.attrs.clone();
        self.attrs(&attrs, &[]);
        let vis = if vis_allowed { self.vis(&it.vis, ItemCtx::Top) } else { String::new() };
        let recv_text = match &f.self_param {
            Some(SelfParam::Value { mutable: true }) => Some("mut self".to_string()),
            Some(SelfParam::Typed(t)) => Some(format!("self: {}", self.ty(t, TyPos::Plain))),
            _ => match recv {
                Recv::Static => None,
                Recv::Ref => Some("&self".into()),
                Recv::RefMut => Some("&mut self".into()),
                Recv::Value => Some("self".into()),
            },
        };
        let header = self.fn_header(f);
        let g = self.generics_decl(&f.generics);
        self.push_scope();
        let params = self.params_parts(f, recv_text);
        let ret = self.ret_type(f, TyPos::Ret);
        let wc = self.where_clause(&f.generics);
        let name = rename.map(|s| s.to_string()).unwrap_or_else(|| esc(&f.name));
        // a shared class's method that passes `this` on gets the handle
        let this_handle = (recv != Recv::Static && !f.mods.is_override && self.needs_this.contains(&f.name)).then(|| self.shared_handle_ty.clone()).flatten();
        let saved_this = self.this_recv.take();
        if let (Some(h), Some(body)) = (&this_handle, &f.body) {
            self.extra_param = Some(format!("__this: &{}", h));
            self.this_recv = Some(visit::this_uses(body).1);
        }
        // an interface method's signature is the interface's, so it can't take the handle
        if let (Some(_), true, Some(body)) = (&self.shared_handle_ty, f.mods.is_override, &f.body) {
            if !visit::this_uses(body).0.is_empty() {
                self.error(f.name_span, format!("`{}` implements an interface method, so it can't pass `this` on: the interface's signature has no place for the object's handle. Let the caller pass the object in as a parameter instead", f.name));
            }
        }
        self.e.w(&format!("{}{}fn {}{}", vis, header, name, g));
        self.write_params(f, &params, recv_why);
        if let Some(r) = &ret {
            self.e.w(&format!(" -> {}", r));
        }
        self.lifetime_decision(f, &lifetimes);
        self.e.w(&wc);
        self.bind_params(f);
        let saved_static = self.class.as_ref().map(|c| c.is_static);
        if let Some(c) = self.class.as_mut() {
            c.is_static = recv == Recv::Static && f.self_param.is_none();
        }
        match &f.body {
            Some(b) => {
                self.e.w(" ");
                self.fun_body(f, b);
            }
            None => self.e.w(";"),
        }
        self.this_recv = saved_this;
        if let (Some(c), Some(s)) = (self.class.as_mut(), saved_static) {
            c.is_static = s;
        }
        self.pop_scope();
        self.e.nl();
    }

    // ------------------------------------------------------------------ classes

    fn class_ctx_for(&self, name: &str, decl_fields: &[Field], supertypes: &[Type]) -> ClassCtx {
        let info = self.class_info(name);
        let mut fields: HashMap<String, Type> = decl_fields.iter().map(|f| (f.name.clone(), f.ty.clone())).collect();
        if let Some(c) = info {
            for f in &c.fields {
                fields.entry(f.name.clone()).or_insert_with(|| f.ty.clone());
            }
        }
        let mut methods = info
            .map(|c| c.methods.clone())
            .or_else(|| self.enum_info(name).map(|e| e.methods.clone()))
            .unwrap_or_default();
        // methods inherited from interfaces (default methods) are callable without `this.`
        let mut stack: Vec<String> = supertypes.iter().filter_map(|t| t.last_name().map(|s| s.to_string())).collect();
        let mut seen = std::collections::HashSet::new();
        let mut trait_methods = HashSet::new();
        while let Some(tn) = stack.pop() {
            if !seen.insert(tn.clone()) {
                continue;
            }
            if let Some(ti) = self.k.traits.get(&tn) {
                for (n, m) in &ti.methods {
                    trait_methods.insert(n.clone());
                    methods.entry(n.clone()).or_insert_with(|| m.clone());
                }
            }
        }
        ClassCtx { name: name.to_string(), fields, methods, self_name: "self".into(), is_static: false, generics: Vec::new(), trait_methods: Some(trait_methods) }
    }

    fn field_type(&mut self, f: &Field) -> Type {
        if f.ty != Type::Infer {
            return f.ty.clone();
        }
        if let Some(d) = &f.default {
            if let Some(t) = infer_field_type(d) {
                return t;
            }
        }
        self.error(f.span, format!("property `{}` needs a type annotation", f.name));
        Type::Infer
    }

    fn class_decl(&mut self, it: &Item, t: &TypeDecl, ctx: ItemCtx) {
        let vis = self.vis(&it.vis, ctx);
        let saved_members_pub = self.members_pub;
        self.members_pub = it.vis == Vis::Public;
        let g = self.generics_decl(&t.generics);
        let gn = self.generics_names(&t.generics);
        let wc = self.where_clause(&t.generics);
        // a shared class is emitted as `{Name}Data` plus a handle named `Name`
        let data_name = if t.shared { Self::shared_data_name(&t.name) } else { t.name.clone() };
        let self_ty = format!("{}{}", data_name, gn);

        // which supertypes become derives
        let (derived, impl_traits, members_by_trait, inherent) = self.split_members(t);
        let (nested, inherent): (Vec<&Item>, Vec<&Item>) = inherent.into_iter().partition(|m| matches!(m.kind, ItemKind::Type(_) | ItemKind::Trait(_)));
        let mut derives: Vec<String> = self.auto_derives(&t.name);
        for d in derived {
            if !derives.contains(&d) {
                derives.push(d);
            }
        }
        if !t.shared {
            self.docs(&it.docs);
        }
        let attrs: Vec<Attr> = it.attrs.iter().filter(|a| !(t.shared && a.name() == "sync")).cloned().collect();
        self.attrs(&attrs, &derives);
        let fields: Vec<(Field, Type)> = t.fields.iter().map(|f| (f.clone(), self.field_type(f))).collect();
        match t.kind {
            TypeKind::TupleStruct => {
                if t.tuple_fields.is_empty() {
                    self.e.w(&format!("{}struct {}{}{};", vis, data_name, g, wc));
                } else {
                    let parts: Vec<String> = t
                        .tuple_fields
                        .iter()
                        .map(|(v, ty)| format!("{}{}", self.vis(v, ItemCtx::Top), self.ty(ty, TyPos::Field)))
                        .collect();
                    self.e.w(&format!("{}struct {}{}({}){};", vis, data_name, g, parts.join(", "), wc));
                }
                self.e.nl();
            }
            _ => {
                let kw = if t.kind == TypeKind::Union { "union" } else { "struct" };
                if fields.is_empty() {
                    self.e.w(&format!("{}{} {}{}{};", vis, kw, data_name, g, wc));
                    self.e.nl();
                } else {
                    self.e.w(&format!("{}{} {}{}{} {{", vis, kw, data_name, g, wc));
                    self.e.indent();
                    for (f, ty) in &fields {
                        self.e.nl();
                        self.e.mark(f.span);
                        self.docs(&f.docs);
                        self.attrs(&f.attrs, &[]);
                        let fv = self.vis(&f.vis, ItemCtx::Top);
                        let tt = self.ty(ty, TyPos::Field);
                        self.e.w(&format!("{}{}: {},", fv, esc(&f.name), tt));
                    }
                    self.e.dedent();
                    self.e.nl();
                    self.e.w("}");
                    self.e.nl();
                }
            }
        }

        let saved_class = self.class.take();
        let saved_this = (std::mem::take(&mut self.needs_this), self.shared_handle_ty.take());
        if t.shared {
            self.needs_this = needs_this(t);
            self.shared_handle_ty = Some(format!("{}{}", t.name, gn));
        }
        self.class = Some(self.class_ctx_for(&t.name, &t.fields, &t.supertypes));
        if let Some(c) = self.class.as_mut() {
            c.generics = t.generics.params.clone();
        }
        let info = self.class_info(&t.name).cloned();

        // inherent impl: constructor + methods
        let needs_new = !matches!(t.kind, TypeKind::TupleStruct | TypeKind::Union) && !info.as_ref().is_some_and(|c| c.user_new);
        let has_inherent = needs_new || !inherent.is_empty();
        let ops: Vec<&Item> = t.members.iter().filter(|m| matches!(&m.kind, ItemKind::Fun(f) if f.mods.is_operator)).collect();
        if has_inherent {
            self.e.nl();
            self.e.w(&format!("impl{} {}{} {{", g, self_ty, wc));
            self.e.indent();
            if needs_new {
                self.e.nl();
                self.constructor(t, &fields);
            }
            for m in &inherent {
                if let ItemKind::Fun(f) = &m.kind {
                    if f.mods.is_operator {
                        continue;
                    }
                    let recv = self.recv_of(&t.name, f);
                    if t.shared && recv == Recv::Static {
                        // static methods of a shared class live on the handle
                        continue;
                    }
                    self.e.nl();
                    self.method(m, f, recv, true, None);
                } else {
                    self.e.nl();
                    self.assoc_item(m, true);
                }
            }
            self.e.dedent();
            self.e.w("}");
            self.e.nl();
        }
        // operators
        for m in ops {
            if let ItemKind::Fun(f) = &m.kind {
                self.operator_impl(m, f, &g, &self_ty, &wc);
            }
        }
        // trait impls
        for (tr, members) in impl_traits.iter().zip(members_by_trait.iter()) {
            self.trait_impl_block(tr, members, &g, &self_ty, &wc);
        }
        // error class
        if t.is_error {
            self.error_class_impls(t, &g, &self_ty, &wc);
        }
        if t.shared {
            let statics: Vec<&Item> = inherent
                .iter()
                .copied()
                .filter(|m| matches!(&m.kind, ItemKind::Fun(f) if !f.mods.is_operator && self.recv_of(&t.name, f) == Recv::Static))
                .collect();
            let forwarded: Vec<(Type, Vec<&Item>)> = impl_traits
                .iter()
                .cloned()
                .zip(members_by_trait.iter().cloned())
                .filter(|(tr, _)| tr.last_name().is_some_and(|n| n != "__Display" && self.k.traits.contains_key(n)))
                .collect();
            self.shared_handle(it, t, &fields, &statics, &forwarded, &derives, &g, &gn, &wc);
        }
        self.class = saved_class;
        (self.needs_this, self.shared_handle_ty) = saved_this;
        self.members_pub = saved_members_pub;
        // nested classes, enums and interfaces live next to their outer class
        for m in nested {
            self.e.nl();
            self.item(m, ctx);
        }
    }

    pub(crate) fn recv_of(&self, owner: &str, f: &FunDecl) -> Recv {
        if let Some(c) = self.class_info(owner) {
            if let Some(m) = c.methods.get(&f.name) {
                return m.recv;
            }
        }
        if let Some(e) = self.enum_info(owner) {
            if let Some(m) = e.methods.get(&f.name) {
                return m.recv;
            }
        }
        match &f.self_param {
            Some(SelfParam::Ref) => Recv::Ref,
            Some(SelfParam::RefMut) => Recv::RefMut,
            Some(_) => Recv::Value,
            None if f.mods.is_static => Recv::Static,
            None if f.mods.is_mut => Recv::RefMut,
            None if sema::STATIC_METHOD_NAMES.contains(&f.name.as_str()) => Recv::Static,
            None => Recv::Ref,
        }
    }

    /// Splits class/enum members into derives, trait impls (with their members) and inherent members.
    fn split_members<'t>(&mut self, t: &'t TypeDecl) -> (Vec<String>, Vec<Type>, Vec<Vec<&'t Item>>, Vec<&'t Item>) {
        let mut derived = Vec::new();
        let mut impl_traits: Vec<Type> = Vec::new();
        for st in &t.supertypes {
            let name = st.last_name().unwrap_or("");
            let has_member = t.members.iter().any(|m| self.member_trait_candidates(m, t).iter().any(|c| c == name));
            if DERIVABLE.contains(&name) && !has_member && matches!(st, Type::Path { segs, .. } if segs.len() == 1 && segs[0].generics.is_none()) {
                derived.push(name.to_string());
            } else {
                impl_traits.push(st.clone());
            }
        }
        let mut by_trait: Vec<Vec<&Item>> = vec![Vec::new(); impl_traits.len()];
        let mut inherent = Vec::new();
        for m in &t.members {
            let is_override = matches!(&m.kind, ItemKind::Fun(f) if f.mods.is_override);
            let is_display = matches!(&m.kind, ItemKind::Fun(f) if (f.name == "to_string" || f.name == "toString") && f.mods.is_override);
            if is_display {
                // handled as a Display impl
                by_trait.push(vec![m]);
                impl_traits.push(Type::simple("__Display"));
                continue;
            }
            let cands = self.member_trait_candidates(m, t);
            let idx: Vec<usize> = impl_traits.iter().enumerate().filter(|(_, tr)| cands.iter().any(|c| Some(c.as_str()) == tr.last_name())).map(|(i, _)| i).collect();
            let assoc = matches!(m.kind, ItemKind::TypeAlias { .. });
            if idx.len() == 1 {
                by_trait[idx[0]].push(m);
            } else if (is_override || assoc) && impl_traits.iter().filter(|t| t.last_name() != Some("__Display")).count() == 1 {
                let i = impl_traits.iter().position(|t| t.last_name() != Some("__Display")).unwrap();
                by_trait[i].push(m);
            } else if is_override || (assoc && !impl_traits.is_empty()) {
                self.error(m.span, "cannot tell which supertype this member implements; use a separate `impl Trait for Type { }` block");
            } else {
                inherent.push(m);
            }
        }
        (derived, impl_traits, by_trait, inherent)
    }

    /// Names of supertypes (Oxie or std traits) that declare this member.
    fn member_trait_candidates(&self, m: &Item, t: &TypeDecl) -> Vec<String> {
        let mut out = Vec::new();
        let (name, is_type) = match &m.kind {
            ItemKind::Fun(f) => (f.name.as_str(), false),
            ItemKind::TypeAlias { name, .. } => (name.as_str(), true),
            _ => return out,
        };
        for st in &t.supertypes {
            let Some(sn) = st.last_name() else { continue };
            if let Some(ti) = self.k.traits.get(sn) {
                if (!is_type && ti.methods.contains_key(name)) || (is_type && ti.assoc_types.iter().any(|a| a == name)) {
                    out.push(sn.to_string());
                }
            } else if is_type {
                if std_trait_assoc(sn).contains(&name) {
                    out.push(sn.to_string());
                }
            } else if std_trait_of_method(name).contains(&sn) {
                out.push(sn.to_string());
            }
        }
        out
    }

    fn constructor(&mut self, t: &TypeDecl, fields: &[(Field, Type)]) {
        let params: Vec<String> = fields
            .iter()
            .filter(|(f, _)| f.in_ctor)
            .map(|(f, ty)| format!("{}: {}", esc(&f.name), self.ty(ty, TyPos::Field)))
            .collect();
        let vis = if self.members_pub { "pub " } else { "pub(crate) " };
        // generated, so an unused constructor is not the user's problem
        self.e.w(&format!("#[allow(dead_code)]\n{}fn new({}) -> Self {{", vis, params.join(", ")));
        self.e.indent();
        self.e.nl();
        let has_init = t.init.is_some();
        if has_init {
            self.e.w("#[allow(unused_mut)]\nlet mut __self = ");
        }
        if fields.is_empty() {
            self.e.w("Self");
        } else {
            self.e.w("Self {");
            self.e.indent();
            self.push_scope();
            for (f, _) in fields.iter().filter(|(f, _)| f.in_ctor) {
                self.bind_name(&f.name, Some(f.ty.clone()));
            }
            let saved = self.class.take();
            for (f, ty) in fields {
                self.e.nl();
                if f.in_ctor {
                    self.e.w(&format!("{},", esc(&f.name)));
                } else {
                    self.e.w(&format!("{}: ", esc(&f.name)));
                    match &f.default {
                        Some(d) => self.coerce(d, ty, TyPos::Field),
                        None => self.e.w("::std::default::Default::default()"),
                    }
                    self.e.w(",");
                }
            }
            self.class = saved;
            self.pop_scope();
            self.e.dedent();
            self.e.nl();
            self.e.w("}");
        }
        if has_init {
            self.e.w(";");
            self.e.nl();
            if let Some(c) = self.class.as_mut() {
                c.self_name = "__self".into();
            }
            self.push_scope();
            for (f, _) in fields.iter().filter(|(f, _)| f.in_ctor) {
                // constructor params shadow fields inside init
                let _ = f;
            }
            self.fns.push(FnCtx { throws: false, ret: None, is_lambda: false, tries: Vec::new(), text_break: None, unboxed_ret: false });
            let init = t.init.as_ref().unwrap();
            self.stmts_with_tail(&init.stmts, None);
            self.fns.pop();
            self.pop_scope();
            if let Some(c) = self.class.as_mut() {
                c.self_name = "self".into();
            }
            self.e.nl();
            self.e.w("__self");
        }
        self.e.dedent();
        self.e.nl();
        self.e.w("}");
        self.e.nl();
    }

    fn assoc_item(&mut self, m: &Item, vis_allowed: bool) {
        self.e.mark(m.span);
        match &m.kind {
            ItemKind::TypeAlias { name, generics, ty } => {
                let g = self.generics_decl(generics);
                let wc = self.where_clause(generics);
                match ty {
                    Type::Bounds(b) if b.is_empty() => self.e.w(&format!("type {}{}{};", name, g, wc)),
                    Type::Bounds(b) => {
                        let bs = self.bounds(b);
                        self.e.w(&format!("type {}{}: {}{};", name, g, bs, wc))
                    }
                    _ => {
                        let tt = self.ty(ty, TyPos::Generic);
                        self.e.w(&format!("type {}{} = {}{};", name, g, tt, wc))
                    }
                }
                self.e.nl();
            }
            ItemKind::Const(c) => {
                let vis = if vis_allowed { self.vis(&m.vis, ItemCtx::Top) } else { String::new() };
                let ty = match &c.ty {
                    Some(t) => self.ty(t, TyPos::Plain),
                    None => c.value.as_ref().and_then(infer_lit_type).unwrap_or_else(|| "_".into()),
                };
                self.e.w(&format!("{}const {}: {}", vis, c.name, ty));
                if let Some(v) = &c.value {
                    self.e.w(" = ");
                    self.expr(v, P_LOWEST);
                }
                self.e.w(";");
                self.e.nl();
            }
            _ => self.item(m, ItemCtx::Top),
        }
    }

    fn trait_impl_block(&mut self, tr: &Type, members: &[&Item], g: &str, self_ty: &str, wc: &str) {
        self.e.nl();
        if tr.last_name() == Some("__Display") {
            if let Some(m) = members.first() {
                if let ItemKind::Fun(f) = &m.kind {
                    self.display_impl_from_to_string(f, g, self_ty, wc);
                }
            }
            return;
        }
        let tname = self.ty(tr, TyPos::Plain);
        self.e.w(&format!("impl{} {} for {}{} {{", g, tname, self_ty, wc));
        self.e.indent();
        let trait_info = tr.last_name().and_then(|n| self.k.traits.get(n)).cloned();
        for m in members {
            self.e.nl();
            match &m.kind {
                ItemKind::Fun(f) => {
                    let owner = self.class.as_ref().map(|c| c.name.clone()).unwrap_or_default();
                    let recv = trait_info
                        .as_ref()
                        .and_then(|t| t.methods.get(&f.name))
                        .filter(|_| f.self_param.is_none() && !f.mods.is_mut && !f.mods.is_static)
                        .map(|m| m.recv)
                        .unwrap_or_else(|| self.recv_of(&owner, f));
                    self.pending_trait_ret = trait_info.as_ref().and_then(|t| t.methods.get(&f.name)).and_then(|m| m.ret.clone());
                    self.method(m, f, recv, false, None);
                }
                _ => self.assoc_item(m, false),
            }
        }
        self.e.dedent();
        self.e.w("}");
        self.e.nl();
    }

    fn display_impl_from_to_string(&mut self, f: &FunDecl, g: &str, self_ty: &str, wc: &str) {
        self.e.w(&format!("impl{} ::std::fmt::Display for {}{} {{", g, self_ty, wc));
        self.e.indent();
        self.e.nl();
        self.e.w("fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {");
        self.e.indent();
        self.e.nl();
        // the formatter `f` isn't bound: a field `f` of the class is still the field
        self.push_scope();
        let text_break = matches!(f.body, Some(FunBody::Block(_))).then_some(false);
        self.fns.push(FnCtx { throws: false, ret: Some(Type::simple("String")), is_lambda: false, tries: Vec::new(), text_break, unboxed_ret: false });
        let single = match &f.body {
            Some(FunBody::Expr(e)) => Some(e.clone()),
            Some(FunBody::Block(b)) if b.stmts.len() == 1 => match &b.stmts[0] {
                Stmt::Expr { expr: Expr { kind: ExprKind::Return(Some(v)), .. }, .. } => Some((**v).clone()),
                Stmt::Expr { expr, semi: false } => Some(expr.clone()),
                _ => None,
            },
            _ => None,
        };
        match single.as_ref() {
            Some(Expr { kind: ExprKind::Template(parts), .. }) => {
                self.e.w("write!(f, ");
                self.format_args(parts);
                self.e.w(")");
            }
            Some(Expr { kind: ExprKind::Str(parts), .. }) => {
                self.e.w(&format!("f.write_str(\"{}\")", lit_parts(parts)));
            }
            // `toString() = name` writes the field, which a `String` result would move out of `self`
            Some(e) if self.is_place(e) => {
                self.e.w("f.write_str(&");
                self.expr(e, P_UNARY);
                self.e.w(")");
            }
            _ => {
                self.e.w("let __oxie_s: String = ");
                match &f.body {
                    Some(FunBody::Expr(e)) => self.coerce(e, &Type::simple("String"), TyPos::Local),
                    Some(FunBody::Block(b)) => {
                        // `return x` at the end is just `x`; one before it breaks out of the block
                        let target = Target { ty: Some(Type::simple("String")), wrap: None, fn_result: true };
                        let at = self.e.pos();
                        self.e.w("{");
                        self.e.indent();
                        self.stmts_with_tail(&b.stmts, Some(&target));
                        self.e.dedent();
                        self.e.nl();
                        self.e.w("}");
                        if self.fns.last().and_then(|c| c.text_break) == Some(true) {
                            self.e.insert(at, &format!("'{}: ", TEXT_LABEL));
                        }
                    }
                    None => self.e.w("String::new()"),
                }
                self.e.w(";");
                self.e.nl();
                self.e.w("f.write_str(&__oxie_s)");
            }
        }
        self.fns.pop();
        self.pop_scope();
        self.e.dedent();
        self.e.nl();
        self.e.w("}");
        self.e.dedent();
        self.e.nl();
        self.e.w("}");
        self.e.nl();
    }

    fn operator_impl(&mut self, m: &Item, f: &FunDecl, g: &str, self_ty: &str, wc: &str) {
        let Some((tr, method, is_assign, is_unary)) = operator_trait(&f.name) else {
            self.error(f.name_span, format!("unknown operator function `{}`", f.name));
            return;
        };
        let rhs = f.params.first().map(|p| self.ty(&p.ty, TyPos::Generic));
        let trait_text = match (&rhs, is_unary) {
            (Some(r), false) if r != "Self" && r != self_ty => format!("::std::ops::{}<{}>", tr, r),
            _ => format!("::std::ops::{}", tr),
        };
        self.e.nl();
        self.e.w(&format!("impl{} {} for {}{} {{", g, trait_text, self_ty, wc));
        self.e.indent();
        self.e.nl();
        if !is_assign {
            let out = f.ret.as_ref().map(|t| self.ty(t, TyPos::Generic)).unwrap_or_else(|| "Self".into());
            self.e.w(&format!("type Output = {};", out));
            self.e.nl();
        }
        let mut f2 = f.clone();
        f2.mods.is_operator = false;
        if f2.ret.is_none() && !is_assign {
            f2.ret = Some(Type::simple("Self"));
        }
        let recv = if is_assign { Recv::RefMut } else { Recv::Value };
        let mut it2 = m.clone();
        it2.attrs.push(Attr::Oxie { name: "inline".into(), args: None, span: Span::default() });
        self.method(&it2, &f2, recv, false, Some(method));
        self.e.dedent();
        self.e.w("}");
        self.e.nl();
    }

    fn error_class_impls(&mut self, t: &TypeDecl, g: &str, self_ty: &str, wc: &str) {
        self.e.nl();
        self.e.w(&format!("impl{} ::std::fmt::Display for {}{} {{", g, self_ty, wc));
        self.e.indent();
        self.e.nl();
        self.e.w("fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {");
        self.e.indent();
        self.e.nl();
        self.push_scope();
        match &t.message {
            Some(msg) => {
                self.fns.push(FnCtx { throws: false, ret: None, is_lambda: false, tries: Vec::new(), text_break: None, unboxed_ret: false });
                self.write_message(msg);
                self.fns.pop();
            }
            None => {
                let text = humanize(&t.name);
                self.e.w(&format!("f.write_str(\"{}\")", text));
            }
        }
        self.pop_scope();
        self.e.dedent();
        self.e.nl();
        self.e.w("}");
        self.e.dedent();
        self.e.nl();
        self.e.w("}");
        self.e.nl();
        self.e.nl();
        self.e.w(&format!("impl{} ::std::error::Error for {}{} {{}}", g, self_ty, wc));
        self.e.nl();
    }

    /// `write!(f, ...)` for an error message template.
    pub(crate) fn write_message(&mut self, msg: &Expr) {
        match &msg.kind {
            ExprKind::Template(parts) => {
                self.e.w("write!(f, ");
                self.format_args(parts);
                self.e.w(")");
            }
            ExprKind::Str(parts) => self.e.w(&format!("f.write_str(\"{}\")", lit_parts(parts))),
            _ => {
                self.e.w("::std::fmt::Display::fmt(&");
                self.expr(msg, P_UNARY);
                self.e.w(", f)");
            }
        }
    }

    // ------------------------------------------------------------------ enums

    fn enum_decl(&mut self, it: &Item, t: &TypeDecl, ctx: ItemCtx) {
        let vis = self.vis(&it.vis, ctx);
        let saved_members_pub = self.members_pub;
        self.members_pub = it.vis == Vis::Public;
        let g = self.generics_decl(&t.generics);
        let gn = self.generics_names(&t.generics);
        let wc = self.where_clause(&t.generics);
        let self_ty = format!("{}{}", t.name, gn);
        let (derived, impl_traits, members_by_trait, inherent) = self.split_members(t);
        let mut derives: Vec<String> = self.auto_derives(&t.name);
        for d in derived {
            if !derives.contains(&d) {
                derives.push(d);
            }
        }
        self.docs(&it.docs);
        self.attrs(&it.attrs, &derives);
        self.e.w(&format!("{}enum {}{}{} {{", vis, t.name, g, wc));
        self.e.indent();
        for v in &t.variants {
            self.e.nl();
            self.e.mark(v.span);
            self.docs(&v.docs);
            let vattrs: Vec<Attr> = v.attrs.iter().filter(|a| a.name() != "from" && a.name() != "source").cloned().collect();
            self.attrs(&vattrs, &[]);
            let mut s = v.name.clone();
            match &v.fields {
                VariantFields::Unit => {}
                VariantFields::Tuple(ts) => {
                    s.push('(');
                    s.push_str(&ts.iter().map(|t| self.ty(t, TyPos::Field)).collect::<Vec<_>>().join(", "));
                    s.push(')');
                }
                VariantFields::Named(fs) => {
                    s.push_str(" { ");
                    s.push_str(&fs.iter().map(|f| format!("{}: {}", esc(&f.name), self.ty(&f.ty, TyPos::Field))).collect::<Vec<_>>().join(", "));
                    s.push_str(" }");
                }
            }
            self.e.w(&s);
            if let Some(d) = &v.discriminant {
                self.e.w(" = ");
                self.expr(d, P_LOWEST);
            }
            self.e.w(",");
        }
        self.e.dedent();
        self.e.nl();
        self.e.w("}");
        self.e.nl();

        let saved_class = self.class.take();
        self.class = Some(self.class_ctx_for(&t.name, &[], &t.supertypes));
        if let Some(c) = self.class.as_mut() {
            c.generics = t.generics.params.clone();
        }
        if !inherent.is_empty() {
            self.e.nl();
            self.e.w(&format!("impl{} {}{} {{", g, self_ty, wc));
            self.e.indent();
            for m in &inherent {
                self.e.nl();
                if let ItemKind::Fun(f) = &m.kind {
                    let recv = self.recv_of(&t.name, f);
                    self.method(m, f, recv, true, None);
                } else {
                    self.assoc_item(m, true);
                }
            }
            self.e.dedent();
            self.e.w("}");
            self.e.nl();
        }
        for m in t.members.iter().filter(|m| matches!(&m.kind, ItemKind::Fun(f) if f.mods.is_operator)) {
            if let ItemKind::Fun(f) = &m.kind {
                self.operator_impl(m, f, &g, &self_ty, &wc);
            }
        }
        for (tr, members) in impl_traits.iter().zip(members_by_trait.iter()) {
            self.trait_impl_block(tr, members, &g, &self_ty, &wc);
        }
        if t.is_error {
            self.error_enum_impls(t, &g, &self_ty, &wc);
        }
        self.class = saved_class;
        self.members_pub = saved_members_pub;
    }

    fn error_enum_impls(&mut self, t: &TypeDecl, g: &str, self_ty: &str, wc: &str) {
        // which variants wrap a source error
        let wraps_error = |ty: &Type| ty.last_name().is_some_and(|n| n.ends_with("Error") && n != "AnyError");
        let mut from_counts: HashMap<String, usize> = HashMap::new();
        for v in &t.variants {
            if let VariantFields::Tuple(ts) = &v.fields {
                if ts.len() == 1 && wraps_error(&ts[0]) {
                    *from_counts.entry(self.ty(&ts[0], TyPos::Plain)).or_default() += 1;
                }
            }
        }
        // Display
        self.e.nl();
        self.e.w(&format!("impl{} ::std::fmt::Display for {}{} {{", g, self_ty, wc));
        self.e.indent();
        self.e.nl();
        self.e.w("fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {");
        self.e.indent();
        self.e.nl();
        self.e.w("match self {");
        self.e.indent();
        for v in &t.variants {
            self.e.nl();
            self.e.mark(v.span);
            let mut used = Vec::new();
            if let Some(m) = &v.message {
                visit::idents_in(m, &mut used);
            }
            self.push_scope();
            let pat = match &v.fields {
                VariantFields::Unit => format!("Self::{}", v.name),
                VariantFields::Tuple(ts) => {
                    let parts: Vec<String> = (0..ts.len())
                        .map(|i| {
                            let n = format!("__{}", i);
                            if used.contains(&n) || (v.message.is_none() && ts.len() == 1) {
                                self.bind_name(&n, None);
                                n
                            } else {
                                "_".into()
                            }
                        })
                        .collect();
                    format!("Self::{}({})", v.name, parts.join(", "))
                }
                VariantFields::Named(fs) => {
                    let mut parts: Vec<String> = Vec::new();
                    for f in fs {
                        if used.contains(&f.name) {
                            // a field named `f` is bound as `__f`, so it doesn't hide the formatter
                            let name = if f.name == "f" { "__f" } else { &f.name };
                            self.bind_name(name, None);
                            parts.push(if name == f.name { esc(name) } else { format!("{}: {}", f.name, name) });
                        }
                    }
                    if parts.len() < fs.len() {
                        parts.push("..".into());
                    }
                    format!("Self::{} {{ {} }}", v.name, parts.join(", "))
                }
            };
            self.e.w(&format!("{} => ", pat));
            match &v.message {
                Some(m) => {
                    let mut m2 = rewrite_positional(m);
                    if matches!(&v.fields, VariantFields::Named(fs) if fs.iter().any(|f| f.name == "f")) {
                        rename_ident(&mut m2, "f", "__f");
                    }
                    self.fns.push(FnCtx { throws: false, ret: None, is_lambda: false, tries: Vec::new(), text_break: None, unboxed_ret: false });
                    self.write_message(&m2);
                    self.fns.pop();
                }
                None => match &v.fields {
                    VariantFields::Tuple(ts) if ts.len() == 1 => self.e.w("::std::fmt::Display::fmt(__0, f)"),
                    _ => self.e.w(&format!("f.write_str(\"{}\")", humanize(&v.name))),
                },
            }
            self.e.w(",");
            self.pop_scope();
        }
        self.e.dedent();
        self.e.nl();
        self.e.w("}");
        self.e.dedent();
        self.e.nl();
        self.e.w("}");
        self.e.dedent();
        self.e.nl();
        self.e.w("}");
        self.e.nl();
        // Error + source
        let sources: Vec<(String, String)> = t
            .variants
            .iter()
            .filter_map(|v| {
                let explicit = v.attrs.iter().any(|a| a.name() == "from" || a.name() == "source");
                match &v.fields {
                    VariantFields::Tuple(ts) if ts.len() == 1 && (explicit || wraps_error(&ts[0])) => Some((format!("Self::{}(e)", v.name), "e".to_string())),
                    VariantFields::Named(fs) => fs
                        .iter()
                        .find(|f| f.name == "source" || f.name == "cause")
                        .filter(|f| f.ty.last_name().is_some_and(|n| n != "AnyError" && n != "Box"))
                        .map(|f| (format!("Self::{} {{ {}: e, .. }}", v.name, esc(&f.name)), "e".to_string())),
                    _ => None,
                }
            })
            .collect();
        self.e.nl();
        if sources.is_empty() {
            self.e.w(&format!("impl{} ::std::error::Error for {}{} {{}}", g, self_ty, wc));
            self.e.nl();
        } else {
            self.e.w(&format!("impl{} ::std::error::Error for {}{} {{", g, self_ty, wc));
            self.e.indent();
            self.e.nl();
            self.e.w("fn source(&self) -> Option<&(dyn ::std::error::Error + 'static)> {");
            self.e.indent();
            self.e.nl();
            self.e.w("match self {");
            self.e.indent();
            for (p, b) in &sources {
                self.e.nl();
                self.e.w(&format!("{} => Some({}),", p, b));
            }
            self.e.nl();
            self.e.w("#[allow(unreachable_patterns)]\n_ => None,");
            self.e.dedent();
            self.e.nl();
            self.e.w("}");
            self.e.dedent();
            self.e.nl();
            self.e.w("}");
            self.e.dedent();
            self.e.nl();
            self.e.w("}");
            self.e.nl();
        }
        // From impls
        for v in &t.variants {
            if let VariantFields::Tuple(ts) = &v.fields {
                let explicit = v.attrs.iter().any(|a| a.name() == "from");
                if ts.len() == 1 {
                    let tt = self.ty(&ts[0], TyPos::Plain);
                    if explicit || (wraps_error(&ts[0]) && from_counts.get(&tt) == Some(&1)) {
                        self.e.nl();
                        self.e.w(&format!(
                            "impl{} ::std::convert::From<{}> for {}{} {{\n    fn from(e: {}) -> Self {{\n        Self::{}(e)\n    }}\n}}",
                            g, tt, self_ty, wc, tt, v.name
                        ));
                        self.e.nl();
                    }
                }
            }
        }
    }

    // ------------------------------------------------------------------ traits & impls

    fn trait_decl(&mut self, it: &Item, t: &TraitDecl, ctx: ItemCtx) {
        self.docs(&it.docs);
        self.attrs(&it.attrs, &[]);
        let vis = self.vis(&it.vis, ctx);
        let g = self.generics_decl(&t.generics);
        let wc = self.where_clause(&t.generics);
        let supers = if t.supertraits.is_empty() { String::new() } else { format!(": {}", self.bounds(&t.supertraits)) };
        let uns = if t.is_unsafe { "unsafe " } else { "" };
        self.e.w(&format!("{}{}trait {}{}{}{} {{", vis, uns, t.name, g, supers, wc));
        self.e.indent();
        let saved = self.class.take();
        let info = self.k.traits.get(&t.name).cloned();
        let mut tmethods = info.as_ref().map(|i| i.methods.clone()).unwrap_or_default();
        for st in &t.supertraits {
            if let Some(si) = st.last_name().and_then(|n| self.k.traits.get(n)) {
                for (n, m) in &si.methods {
                    tmethods.entry(n.clone()).or_insert_with(|| m.clone());
                }
            }
        }
        self.class = Some(ClassCtx {
            name: t.name.clone(),
            fields: HashMap::new(),
            methods: tmethods,
            self_name: "self".into(),
            is_static: false,
            generics: Vec::new(),
            trait_methods: None,
        });
        for m in &t.members {
            self.e.nl();
            match &m.kind {
                ItemKind::Fun(f) => {
                    let recv = info.as_ref().and_then(|i| i.methods.get(&f.name)).map(|m| m.recv).unwrap_or(Recv::Ref);
                    let recv = if f.self_param.is_none() && !f.mods.is_mut && !f.mods.is_static && !sema::STATIC_METHOD_NAMES.contains(&f.name.as_str()) && f.body.is_some() {
                        // default methods: infer &mut from the body
                        let facts = sema::analyze_fun(f, &HashSet::new(), &HashMap::new());
                        if facts.mutates_self {
                            Recv::RefMut
                        } else {
                            recv
                        }
                    } else {
                        recv
                    };
                    self.method(m, f, recv, false, None);
                }
                _ => self.assoc_item(m, false),
            }
        }
        self.class = saved;
        self.e.dedent();
        self.e.w("}");
        self.e.nl();
    }

    fn impl_decl(&mut self, it: &Item, imp: &ImplDecl) {
        self.docs(&it.docs);
        self.attrs(&it.attrs, &[]);
        let g = self.generics_decl(&imp.generics);
        let wc = self.where_clause(&imp.generics);
        let self_ty = self.ty(&imp.self_ty, TyPos::Plain);
        let uns = if imp.is_unsafe { "unsafe " } else { "" };
        match &imp.trait_ {
            Some(tr) => {
                let neg = if imp.negative { "!" } else { "" };
                let tt = self.ty(tr, TyPos::Plain);
                self.e.w(&format!("{}impl{} {}{} for {}{} {{", uns, g, neg, tt, self_ty, wc));
            }
            None => self.e.w(&format!("{}impl{} {}{} {{", uns, g, self_ty, wc)),
        }
        self.e.indent();
        let owner = imp.self_ty.last_name().unwrap_or("").to_string();
        let saved = self.class.take();
        let decl_fields: Vec<Field> = Vec::new();
        let supers: Vec<Type> = imp.trait_.iter().cloned().collect();
        self.class = Some(self.class_ctx_for(&owner, &decl_fields, &supers));
        if let Some(c) = self.class.as_mut() {
            c.generics = imp.generics.params.clone();
            if imp.trait_.is_some() {
                c.trait_methods = None;
            }
        }
        let tinfo = imp.trait_.as_ref().and_then(|t| t.last_name()).and_then(|n| self.k.traits.get(n)).cloned();
        for m in &imp.members {
            self.e.nl();
            match &m.kind {
                ItemKind::Fun(f) => {
                    let recv = tinfo
                        .as_ref()
                        .and_then(|t| t.methods.get(&f.name))
                        .filter(|_| f.self_param.is_none() && !f.mods.is_mut && !f.mods.is_static)
                        .map(|m| m.recv)
                        .unwrap_or_else(|| self.recv_of(&owner, f));
                    self.pending_trait_ret = tinfo.as_ref().and_then(|t| t.methods.get(&f.name)).and_then(|m| m.ret.clone());
                    self.method(m, f, recv, imp.trait_.is_none(), None);
                }
                _ => self.assoc_item(m, imp.trait_.is_none()),
            }
        }
        self.class = saved;
        self.e.dedent();
        self.e.w("}");
        self.e.nl();
    }
}

// ------------------------------------------------------------------ helpers

/// Text of literal string parts (already Rust-escaped).
pub fn lit_parts(parts: &[StrPart]) -> String {
    parts
        .iter()
        .map(|p| match p {
            StrPart::Lit(s) => s.clone(),
            StrPart::Interp { src, .. } if src.starts_with('$') => src.clone(),
            StrPart::Interp { src, .. } => format!("${{{}}}", src),
        })
        .collect()
}

pub(crate) fn infer_lit_type(e: &Expr) -> Option<String> {
    Some(match &e.kind {
        ExprKind::Int(s) => int_suffix(s).unwrap_or("i32").to_string(),
        ExprKind::Float(s) => if s.ends_with("f32") { "f32" } else { "f64" }.to_string(),
        ExprKind::Bool(_) => "bool".into(),
        ExprKind::Char(s) => if s.starts_with('b') { "u8" } else { "char" }.to_string(),
        ExprKind::Str(_) | ExprKind::RawStr(_) => "&str".into(),
        ExprKind::Unary(UnOp::Neg, x) => return infer_lit_type(x),
        _ => return None,
    })
}

pub(crate) fn int_suffix(s: &str) -> Option<&'static str> {
    ["i128", "u128", "isize", "usize", "i64", "u64", "i32", "u32", "i16", "u16", "i8", "u8", "f32", "f64"].into_iter().find(|&suf| s.ends_with(suf) && !s.starts_with("0x")).map(|v| v as _)
}

pub(crate) fn infer_field_type(e: &Expr) -> Option<Type> {
    match &e.kind {
        ExprKind::Str(_) | ExprKind::Template(_) => Some(Type::simple("String")),
        ExprKind::Call { callee, args } => {
            let n = callee.ident()?;
            if is_type_like(n) {
                return Some(Type::simple(n));
            }
            match n {
                "listOf" | "mutableListOf" | "arrayListOf" if args.is_empty() => None,
                _ => None,
            }
        }
        ExprKind::MethodCall { recv, name, .. } if name == "new" || name == "default" || name == "with_capacity" => {
            let n = recv.ident()?;
            if is_type_like(n) {
                Some(Type::simple(n))
            } else {
                None
            }
        }
        _ => infer_lit_type(e).map(|s| if s == "&str" { Type::simple("String") } else { Type::simple(&s) }),
    }
}

pub(crate) fn ends_diverging(stmts: &[Stmt]) -> bool {
    match stmts.last() {
        Some(Stmt::Expr { expr, .. }) => expr.diverges(),
        _ => false,
    }
}

/// The lifetimes Rust can't elide in a signature: those of a returned reference
/// ([`ret_lifetimes`]) and of references inside an `impl Trait` parameter
/// ([`impl_trait_lifetimes`]).
pub(crate) fn auto_lifetimes(f: &FunDecl, self_ref: bool, lt: &dyn Fn(&str) -> usize) -> FunDecl {
    impl_trait_lifetimes(&ret_lifetimes(f, self_ref, lt))
}

/// `patches: impl Iterator<Item = &Patch>`: Rust wants a name for a reference lifetime inside
/// an `impl Trait` parameter, so each such parameter gets a lifetime of its own.
fn impl_trait_lifetimes(f: &FunDecl) -> FunDecl {
    /// Names the elided reference lifetimes in `t` `l` (not in `Fn(&T)` sugar, where Rust
    /// accepts them); true if there were any.
    fn name(t: &mut Type, l: &str) -> bool {
        match t {
            Type::Ref { lifetime, inner, .. } => {
                let elided = lifetime.as_deref().is_none_or(|x| x == "_");
                if elided {
                    *lifetime = Some(l.to_string());
                }
                name(inner, l) | elided
            }
            Type::Ptr { inner, .. } | Type::Slice(inner) | Type::Array(inner, _) | Type::Option(inner) => name(inner, l),
            Type::Tuple(v) | Type::Impl(v) | Type::Dyn(v) | Type::Bounds(v) => v.iter_mut().fold(false, |any, t| name(t, l) | any),
            Type::Path { segs, .. } => {
                let mut any = false;
                for g in segs.iter_mut().filter(|s| s.fn_sugar.is_none()).filter_map(|s| s.generics.as_mut()).flatten() {
                    match g {
                        GenericArg::Type(t) | GenericArg::Binding(_, t) => any |= name(t, l),
                        GenericArg::Lifetime(x) if x == "_" => {
                            *x = l.to_string();
                            any = true;
                        }
                        _ => {}
                    }
                }
                any
            }
            _ => false,
        }
    }
    /// Names the lifetimes inside the `impl Trait`s of parameter type `t`.
    fn in_impls(t: &mut Type, l: &str) -> bool {
        match t {
            Type::Impl(bounds) => bounds.iter_mut().fold(false, |any, b| name(b, l) | any),
            Type::Ref { inner, .. } | Type::Ptr { inner, .. } | Type::Slice(inner) | Type::Array(inner, _) | Type::Option(inner) => in_impls(inner, l),
            Type::Tuple(v) => v.iter_mut().fold(false, |any, t| in_impls(t, l) | any),
            Type::Path { segs, .. } => segs.iter_mut().filter_map(|s| s.generics.as_mut()).flatten().fold(false, |any, g| match g {
                GenericArg::Type(t) | GenericArg::Binding(_, t) => in_impls(t, l) | any,
                _ => any,
            }),
            _ => false,
        }
    }
    let mut used: Vec<String> = f.generics.params.iter().filter_map(|g| if let GenericParam::Lifetime { name, .. } = g { Some(name.clone()) } else { None }).collect();
    let mut f2 = f.clone();
    let mut added = Vec::new();
    for p in &mut f2.params {
        let l = ('a'..='z').map(String::from).find(|c| !used.contains(c)).unwrap_or_else(|| format!("oxie{}", used.len()));
        if in_impls(&mut p.ty, &l) {
            used.push(l.clone());
            added.push(GenericParam::Lifetime { name: l, bounds: Vec::new() });
        }
    }
    if added.is_empty() {
        return f.clone();
    }
    // lifetimes come first in Rust's generic parameters
    let at = f2.generics.params.iter().take_while(|g| matches!(g, GenericParam::Lifetime { .. })).count();
    f2.generics.params.splice(at..at, added);
    f2
}

/// Rust can't elide the lifetime of a returned reference when there are several reference
/// parameters (or none). Oxie ties the result to all of them (or `'static` when there are none).
/// `lt` gives the lifetime parameters of a type written without them (`Token` for `Token<'a>`).
fn ret_lifetimes(f: &FunDecl, self_ref: bool, lt: &dyn Fn(&str) -> usize) -> FunDecl {
    fn count(t: &Type, n: &mut usize, lt: &dyn Fn(&str) -> usize) {
        match t {
            Type::Ref { lifetime: None, inner, .. } => {
                *n += 1;
                count(inner, n, lt);
            }
            Type::Ref { lifetime: Some(l), inner, .. } if l == "_" => {
                *n += 1;
                count(inner, n, lt);
            }
            Type::Ref { inner, .. } | Type::Ptr { inner, .. } | Type::Slice(inner) | Type::Array(inner, _) | Type::Option(inner) => count(inner, n, lt),
            Type::Tuple(v) | Type::Impl(v) | Type::Dyn(v) | Type::Bounds(v) => v.iter().for_each(|t| count(t, n, lt)),
            Type::Path { segs, .. } => {
                if let [s] = segs.as_slice() {
                    if s.generics.is_none() {
                        *n += lt(&s.name);
                    }
                }
                for s in segs {
                    if let Some(g) = &s.generics {
                        for a in g {
                            match a {
                                GenericArg::Type(t) | GenericArg::Binding(_, t) => count(t, n, lt),
                                GenericArg::Lifetime(l) if l == "_" => *n += 1,
                                _ => {}
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }
    fn set(t: &mut Type, l: &str, lt: &dyn Fn(&str) -> usize) {
        match t {
            Type::Ref { lifetime, inner, .. } => {
                if lifetime.as_deref().is_none_or(|x| x == "_") {
                    *lifetime = Some(l.to_string());
                }
                set(inner, l, lt);
            }
            Type::Ptr { inner, .. } | Type::Slice(inner) | Type::Array(inner, _) | Type::Option(inner) => set(inner, l, lt),
            Type::Tuple(v) | Type::Impl(v) | Type::Dyn(v) | Type::Bounds(v) => v.iter_mut().for_each(|t| set(t, l, lt)),
            Type::Path { segs, .. } => {
                if let [s] = segs.as_mut_slice() {
                    let n = lt(&s.name);
                    if s.generics.is_none() && n > 0 {
                        s.generics = Some(vec![GenericArg::Lifetime(l.to_string()); n]);
                    }
                }
                for s in segs {
                    if let Some(g) = &mut s.generics {
                        for a in g {
                            match a {
                                GenericArg::Type(t) | GenericArg::Binding(_, t) => set(t, l, lt),
                                GenericArg::Lifetime(x) if x == "_" => *x = l.to_string(),
                                _ => {}
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }
    let Some(ret) = &f.ret else { return f.clone() };
    let mut in_ret = 0;
    count(ret, &mut in_ret, lt);
    if in_ret == 0 || self_ref {
        return f.clone();
    }
    let mut in_params = 0;
    for p in &f.params {
        count(&p.ty, &mut in_params, lt);
    }
    // Rust elides a single one, but not inside an `impl Trait` parameter
    fn in_impl(t: &Type, lt: &dyn Fn(&str) -> usize) -> bool {
        match t {
            Type::Impl(_) => {
                let mut n = 0;
                count(t, &mut n, lt);
                n > 0
            }
            Type::Ref { inner, .. } | Type::Ptr { inner, .. } | Type::Slice(inner) | Type::Array(inner, _) | Type::Option(inner) => in_impl(inner, lt),
            Type::Tuple(v) => v.iter().any(|t| in_impl(t, lt)),
            Type::Path { segs, .. } => segs.iter().filter_map(|s| s.generics.as_ref()).flatten().any(|g| matches!(g, GenericArg::Type(t) | GenericArg::Binding(_, t) if in_impl(t, lt))),
            _ => false,
        }
    }
    if in_params == 1 && !f.params.iter().any(|p| in_impl(&p.ty, lt)) {
        return f.clone();
    }
    let mut f2 = f.clone();
    if in_params == 0 {
        set(f2.ret.as_mut().unwrap(), "static", lt);
        return f2;
    }
    let used: Vec<String> = f.generics.params.iter().filter_map(|g| if let GenericParam::Lifetime { name, .. } = g { Some(name.clone()) } else { None }).collect();
    let l = if used.iter().any(|u| u == "a") { "oxie".to_string() } else { "a".to_string() };
    f2.generics.params.insert(0, GenericParam::Lifetime { name: l.clone(), bounds: Vec::new() });
    for p in &mut f2.params {
        set(&mut p.ty, &l, lt);
    }
    set(f2.ret.as_mut().unwrap(), &l, lt);
    f2
}

/// Replaces the name `from` with `to` everywhere in `e`.
fn rename_ident(e: &mut Expr, from: &str, to: &str) {
    visit::walk_expr_mut(e, &mut |x| {
        if let ExprKind::Path { segs, generics: None, global: false } = &mut x.kind {
            if segs.len() == 1 && segs[0] == from {
                segs[0] = to.to_string();
            }
        }
        true
    });
}

/// In error message templates `$0` / `${0}` refers to tuple field 0.
fn rewrite_positional(e: &Expr) -> Expr {
    let mut e = e.clone();
    if let ExprKind::Template(parts) = &mut e.kind {
        for p in parts.iter_mut() {
            if let TemplatePart::Expr(x, _) = p {
                if let ExprKind::Int(n) = &x.kind {
                    x.kind = ExprKind::Path { segs: vec![format!("__{}", n)], generics: None, global: false };
                }
            }
        }
    }
    e
}


/// The methods of a shared class that use `this` as a value, or call (on `this`) one that does:
/// their data methods take the handle (`__this`), since the data alone isn't the object.
fn needs_this(t: &TypeDecl) -> HashSet<String> {
    let methods: Vec<&FunDecl> = t
        .members
        .iter()
        .filter_map(|m| if let ItemKind::Fun(f) = &m.kind { Some(f) } else { None })
        .filter(|f| !f.mods.is_static && !f.mods.is_override && !f.mods.is_operator && f.body.is_some())
        .collect();
    let mut out: HashSet<String> = methods.iter().filter(|f| !visit::this_uses(f.body.as_ref().unwrap()).0.is_empty()).map(|f| f.name.clone()).collect();
    loop {
        let before = out.len();
        for f in &methods {
            if !out.contains(&f.name) && visit::own_calls(f.body.as_ref().unwrap()).iter().any(|c| out.contains(c)) {
                out.insert(f.name.clone());
            }
        }
        if out.len() == before {
            return out;
        }
    }
}
