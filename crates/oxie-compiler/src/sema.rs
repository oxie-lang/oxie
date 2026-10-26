//! Crate-wide symbol table and method receiver inference.

use std::collections::{HashMap, HashSet};

use crate::ast::*;
use crate::diag::Span;
use crate::parser::is_type_like;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Recv {
    Static,
    Ref,
    RefMut,
    Value,
}

#[derive(Clone, Debug)]
pub struct ParamInfo {
    pub name: Option<String>,
    pub ty: Type,
    pub default: Option<Expr>,
    /// A context parameter: the name of its function, for the error when a call has none to pass.
    pub context: Option<String>,
    /// Generic parameter names of the declaring function (lambda arguments can't be annotated with them).
    pub generics: Vec<String>,
    /// An interface value passed in a box: a parameter of an interface method typed as an Oxie
    /// interface is a `Box<dyn I>`, so the interface stays usable as `dyn`.
    pub boxed: bool,
    /// Lent by Oxie (`Param::lent`): every argument that isn't a reference is lent.
    pub lent: bool,
    /// Exists only in some builds (`@cfg(feature = "x") p: T`): a call may leave it out.
    pub cfg: bool,
}

#[derive(Clone, Debug)]
pub struct MethodInfo {
    pub recv: Recv,
    pub params: Vec<ParamInfo>,
    pub throws: bool,
    pub ret: Option<Type>,
    /// The receiver was written in the source (`self`, `mut fun`, `static fun`, ...).
    pub explicit: bool,
    /// Why an inferred receiver is `&mut self` ("it assigns `hits`"), and the use that decided it.
    pub recv_why: Option<(String, Option<Span>)>,
    /// An interface's method or an implementation of one.
    pub iface: bool,
}

#[derive(Clone, Debug)]
pub struct FieldInfo {
    pub name: String,
    pub ty: Type,
    pub default: Option<Expr>,
    pub in_ctor: bool,
}

#[derive(Clone, Debug)]
pub struct ClassInfo {
    pub name: String,
    pub module: Vec<String>,
    pub fields: Vec<FieldInfo>,
    pub methods: HashMap<String, MethodInfo>,
    pub user_new: bool,
    pub is_data: bool,
    pub is_tuple: bool,
    /// `union` (constructed with a struct literal, no generated constructor)
    pub is_union: bool,
    /// No fields at all: the type name is also its only value (`struct Marker`, `class Dog : Animal`)
    pub is_unit: bool,
    /// Type or const parameters (a class with only lifetimes is used without arguments).
    pub has_generics: bool,
    /// Number of type parameters (lifetimes and consts excluded).
    pub type_params: usize,
    /// `shared class`: a handle to `{Name}Data` (`sync`: `Arc<Mutex<..>>` instead of `Rc<RefCell<..>>`).
    pub is_shared: bool,
    pub is_sync: bool,
    /// Number of lifetime parameters (`data class Token<'a>`).
    pub lifetimes: usize,
    /// The names in its `context(...)` clause.
    pub contexts: Vec<String>,
    /// The last names of the interfaces it implements (after `:` or in `impl X for` blocks).
    pub interfaces: Vec<String>,
    /// Associated constants (in the body, a companion object or an `impl` block).
    pub consts: Vec<String>,
}

impl ClassInfo {
    /// Constructor parameters in order.
    pub fn ctor_params(&self) -> Vec<ParamInfo> {
        if self.user_new {
            if let Some(m) = self.methods.get("new") {
                return m.params.clone();
            }
        }
        self.fields
            .iter()
            .filter(|f| f.in_ctor)
            .map(|f| ParamInfo { name: Some(f.name.clone()), ty: f.ty.clone(), default: f.default.clone(), context: None, generics: Vec::new(), boxed: false, lent: false, cfg: false })
            .collect()
    }
}

#[derive(Clone, Debug)]
pub enum VariantShape {
    Unit,
    Tuple(Vec<Type>),
    Named(Vec<(String, Type)>),
}

#[derive(Clone, Debug)]
pub struct EnumInfo {
    pub name: String,
    pub module: Vec<String>,
    pub variants: Vec<(String, VariantShape)>,
    pub methods: HashMap<String, MethodInfo>,
    /// Number of lifetime parameters (`enum Tok<'a>`).
    pub lifetimes: usize,
    pub has_type_params: bool,
    /// The last names of the interfaces it implements (after `:` or in `impl X for` blocks).
    pub interfaces: Vec<String>,
    /// Associated constants, as in `ClassInfo`.
    pub consts: Vec<String>,
}

impl EnumInfo {
    pub fn variant(&self, name: &str) -> Option<&VariantShape> {
        self.variants.iter().find(|(n, _)| n == name).map(|(_, s)| s)
    }
}

#[derive(Clone, Debug)]
pub struct TraitInfo {
    pub name: String,
    pub module: Vec<String>,
    pub methods: HashMap<String, MethodInfo>,
    pub assoc_types: Vec<String>,
    /// The last names of its supertraits (`'static` for a lifetime bound).
    pub supertraits: Vec<String>,
    /// Its own methods can be forwarded to the value inside a `Box<T>` (`(**self).m(..)`): it
    /// has no generics, associated items or generic methods, and every method takes `&self`
    /// or `&mut self` and doesn't mention `Self`. `Some(true)` when they all take `&self`, so
    /// they can be forwarded from a `&T` too.
    pub forward: Option<bool>,
    /// Names of its type parameters (`interface Repo<T>`).
    pub generics: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct FunInfo {
    pub module: Vec<String>,
    pub params: Vec<ParamInfo>,
    pub throws: bool,
    pub ret: Option<Type>,
    /// Written in Rust (in a `rust { }` block): Oxie doesn't know its signature, only that it
    /// hides functions of other modules with its name.
    pub raw: bool,
    /// The result type names a type parameter (`fun with<T>(..): T`), so a call's type depends
    /// on its arguments.
    pub generic_ret: bool,
}

#[derive(Clone, Debug, Default)]
pub struct Crate {
    pub crate_name: String,
    /// Classes and enums by name; a name declared in several modules is keyed by `scoped_key`
    /// for each declaration (see `same_named`), so look types up with `class_in`/`enum_in`.
    pub classes: HashMap<String, ClassInfo>,
    pub enums: HashMap<String, EnumInfo>,
    pub traits: HashMap<String, TraitInfo>,
    pub funs: HashMap<String, FunInfo>,
    pub ambiguous: HashSet<String>,
    /// Class and enum names declared in several modules (`a::Counter` and `b::Counter`).
    pub same_named: HashSet<String>,
    /// Function names declared in several modules; `funs` keys these like `classes` does.
    pub same_named_funs: HashSet<String>,
    /// What a name imported into a module stands for: (module, name) -> path (`a::Counter`).
    pub imports: HashMap<(Vec<String>, String), Vec<String>>,
    /// The paths each module imports with `*`.
    pub globs: HashMap<Vec<String>, Vec<Vec<String>>>,
    /// Top-level module names (children of the crate root).
    pub modules: HashSet<String>,
    /// The names of the modules declared in a file (`mod b { .. }`), at any depth.
    pub inline_modules: HashSet<String>,
    /// The path of every module that has an .oxi file.
    pub module_paths: HashSet<Vec<String>>,
    /// External crate names from Cargo.toml.
    pub extern_crates: HashSet<String>,
    /// (module path, trait name) for every extension-function trait.
    pub ext_traits: Vec<(Vec<String>, String)>,
    /// Type names modules declare in Rust, which Oxie can't see into: in `rust { }` blocks
    /// (`struct Newline;`) and item macros (`actions!(editor, [Newline])`).
    pub rust_types: HashSet<(Vec<String>, String)>,
    /// Variant name -> the (module, name) of each enum that has it.
    pub variant_owner: HashMap<String, Vec<(Vec<String>, String)>>,
    /// Whether a `lib.oxi` exists (main.oxi then imports modules through the crate name).
    pub has_lib: bool,
    /// Automatic derives per type (data classes and enums), keyed like `classes`.
    pub derives: HashMap<String, Vec<String>>,
    /// Types declared inside a class body (hoisted to the module): outer name -> inner names.
    pub nested: HashMap<String, Vec<String>>,
    /// Declared type names in all capitals (`class DB`, `enum HTTP`), which don't look like types.
    pub caps_types: HashSet<String>,
    /// Statics and constants whose names look like types (`static N`, also declared in a
    /// `thread_local!` or `lazy_static!` call): `N.with { }` reads the value, not a type's member.
    pub value_names: HashSet<String>,
}

/// What `Crate` keys by module when several modules declare a name.
#[derive(Clone, Copy)]
enum Space {
    /// classes and enums
    Types,
    Funs,
}

/// The key of a class, enum or function whose name is declared in several modules (`a::Counter`,
/// and `crate::Counter` for the crate root).
pub fn scoped_key(module: &[String], name: &str) -> String {
    if module.is_empty() {
        format!("crate::{}", name)
    } else {
        format!("{}::{}", module.join("::"), name)
    }
}

pub const STATIC_METHOD_NAMES: &[&str] = &["new", "default", "from", "from_str", "try_from", "from_iter"];

/// Methods that take `&mut self` on common std types (used to infer `mut fun`).
pub const MUTATING_METHODS: &[&str] = &[
    "push", "push_str", "push_back", "push_front", "pop", "pop_back", "pop_front", "insert", "remove", "clear", "extend",
    "extend_from_slice", "append", "truncate", "retain", "retain_mut", "drain", "sort", "sort_by", "sort_by_key",
    "sort_unstable", "sort_unstable_by", "sort_unstable_by_key", "dedup", "dedup_by_key", "reverse", "swap", "resize",
    "entry", "get_mut", "iter_mut", "values_mut", "last_mut", "first_mut", "as_mut", "as_mut_slice", "take", "replace",
    "set", "fill", "rotate_left", "rotate_right", "split_off", "shrink_to_fit", "reserve", "get_or_insert",
    "get_or_insert_with", "write", "write_all", "write_str", "write_fmt", "flush", "read", "read_line",
    "read_to_string", "read_to_end", "read_exact", "next", "next_back", "or_insert", "swap_remove", "push_all",
    "borrow_mut", "chunks_mut", "split_at_mut", "nth", "advance_by", "make_ascii_uppercase", "make_ascii_lowercase",
];

/// Methods returning a new value (an iterator, a copy) rather than a reference into their receiver:
/// changing that value doesn't change the receiver.
const NEW_VALUE_METHODS: &[&str] = &[
    "iter", "into_iter", "chars", "char_indices", "bytes", "lines", "split", "splitn", "rsplit", "rsplitn",
    "split_whitespace", "split_terminator", "keys", "values", "windows", "chunks", "map", "filter", "filter_map",
    "enumerate", "zip", "rev", "skip", "step_by", "peekable", "cloned", "copied", "clone", "to_vec", "to_string",
    "to_owned", "collect",
];

impl Crate {
    pub fn class(&self, name: &str) -> Option<&ClassInfo> {
        if self.ambiguous.contains(name) {
            return None;
        }
        self.classes.get(name)
    }
    pub fn enum_(&self, name: &str) -> Option<&EnumInfo> {
        if self.ambiguous.contains(name) {
            return None;
        }
        self.enums.get(name)
    }
    /// The class `name` means when written in `module` (see `type_key_in`).
    pub fn class_in(&self, name: &str, module: &[String]) -> Option<&ClassInfo> {
        if self.imported_from_outside(name, module) {
            return None;
        }
        if !self.same_named.contains(name) {
            return self.class(name);
        }
        self.classes.get(&self.type_key_in(name, module)?)
    }
    pub fn enum_in(&self, name: &str, module: &[String]) -> Option<&EnumInfo> {
        if self.imported_from_outside(name, module) {
            return None;
        }
        if !self.same_named.contains(name) {
            return self.enum_(name);
        }
        self.enums.get(&self.type_key_in(name, module)?)
    }
    /// The class a path written in `module` goes through (`Counter`, `a.Counter`): by its last
    /// name, or for a name declared in several modules, the one the path leads to.
    pub fn class_at(&self, path: &[String], module: &[String]) -> Option<&ClassInfo> {
        let name = path.last()?;
        if self.leaves_crate(path, module) || path.len() == 1 && self.imported_from_outside(name, module) {
            return None;
        }
        // `crate.ser.ValueSerializer` isn't the `ValueSerializer` of another module
        if !self.same_named.contains(name) && (path.len() == 1 || self.type_key_at(path, module).is_some()) {
            return self.class(name);
        }
        self.classes.get(&self.type_key_at(path, module)?)
    }
    pub fn enum_at(&self, path: &[String], module: &[String]) -> Option<&EnumInfo> {
        let name = path.last()?;
        if self.leaves_crate(path, module) || path.len() == 1 && self.imported_from_outside(name, module) {
            return None;
        }
        if !self.same_named.contains(name) && (path.len() == 1 || self.type_key_at(path, module).is_some()) {
            return self.enum_(name);
        }
        self.enums.get(&self.type_key_at(path, module)?)
    }
    /// Whether a path written in `module` starts at another crate (`std.thread.Builder`), so it
    /// names none of this crate's types, whatever their names.
    pub fn leaves_crate(&self, path: &[String], module: &[String]) -> bool {
        let Some(first) = path.first().filter(|_| path.len() > 1) else { return false };
        // `ser.SerializeMap` after `import serde::ser`
        if let Some(p) = self.imports.get(&(module.to_vec(), first.clone())) {
            let inside = p.first().is_some_and(|f| matches!(f.as_str(), "crate" | "self" | "super") || *f == self.crate_name || self.modules.contains(f));
            return !inside;
        }
        let other_crate = matches!(first.as_str(), "std" | "core" | "alloc") || self.extern_crates.contains(first);
        other_crate && !self.modules.contains(first)
    }
    /// Whether `module` imports `name` from another crate (`import alacritty_terminal::Selection`),
    /// so that the name means that one, not a type or function of this crate.
    fn imported_from_outside(&self, name: &str, module: &[String]) -> bool {
        self.imports.get(&(module.to_vec(), name.to_string())).is_some_and(|p| {
            // `import proc_macro2.Group`: a path that starts at none of this crate's modules,
            // types or imports starts at another crate
            let first = p.first().map(String::as_str).unwrap_or_default();
            let other = p.len() > 1
                && !matches!(first, "crate" | "self" | "super" | "Self")
                && first != self.crate_name
                && !self.modules.contains(first)
                && !self.inline_modules.contains(first)
                && !self.module_paths.contains(&[module, &[first.to_string()]].concat())
                && !self.is_user_type(first)
                && !self.imports.contains_key(&(module.to_vec(), first.to_string()));
            other || self.leaves_crate(p, module)
        })
    }
    /// The key of the class or enum `name` declared in `module`.
    pub fn decl_key(&self, module: &[String], name: &str) -> String {
        if self.same_named.contains(name) {
            scoped_key(module, name)
        } else {
            name.to_string()
        }
    }
    /// The key of the class or enum `name` means when written in `module`. A name declared in several
    /// modules means the one declared in `module` or imported into it (by name or with `*`).
    pub fn type_key_in(&self, name: &str, module: &[String]) -> Option<String> {
        self.key_in(Space::Types, name, module)
    }
    /// The key of the class or enum a path written in `module` names (`a::Counter`, `crate::Counter`,
    /// `super::b::Counter`), if the module the path leads to declares it.
    pub fn type_key_at(&self, path: &[String], module: &[String]) -> Option<String> {
        self.key_at(Space::Types, path, module)
    }
    /// The function `name` means when written in `module` (as for types, see `type_key_in`).
    pub fn fun_in(&self, name: &str, module: &[String]) -> Option<&FunInfo> {
        if !self.same_named_funs.contains(name) && self.ambiguous.contains(name) || self.imported_from_outside(name, module) {
            return None;
        }
        self.funs.get(&self.key_in(Space::Funs, name, module)?).filter(|f| !f.raw)
    }
    /// The function a path written in `module` names (`util.total`), if the module the path
    /// leads to declares it.
    pub fn fun_at(&self, path: &[String], module: &[String]) -> Option<&FunInfo> {
        if !self.same_named_funs.contains(path.last()?) && self.ambiguous.contains(path.last()?) {
            return None;
        }
        self.funs.get(&self.key_at(Space::Funs, path, module)?).filter(|f| !f.raw)
    }
    /// Whether some module declares a function `name`.
    pub fn is_fun_name(&self, name: &str) -> bool {
        self.funs.contains_key(name) || self.same_named_funs.contains(name)
    }
    /// Whether `name` written in `module` is a top-level module: one with that name, unless
    /// `module` has a child module of the name (`lsp` in `editor`, with `editor/lsp.oxi`).
    pub fn root_module(&self, name: &str, module: &[String]) -> bool {
        self.modules.contains(name) && !(!module.is_empty() && self.module_paths.contains(&[module, &[name.to_string()]].concat()))
    }
    /// Whether a function `name` of the crate can be called by that name in `module`: declared
    /// there, or imported by name or with `*`.
    pub fn fun_in_scope(&self, name: &str, module: &[String]) -> bool {
        if let Some(path) = self.imports.get(&(module.to_vec(), name.to_string())) {
            return self.key_at(Space::Funs, path, module).is_some();
        }
        let Some(key) = self.key_in(Space::Funs, name, module) else { return false };
        let Some(home) = self.module_of(Space::Funs, &key) else { return false };
        home.as_slice() == module || self.globs.get(module).into_iter().flatten().any(|g| self.module_path(g, module, true).as_ref() == Some(home))
    }
    /// The module declaring the type (or function) under `key`.
    fn module_of(&self, space: Space, key: &str) -> Option<&Vec<String>> {
        match space {
            Space::Types => self.classes.get(key).map(|c| &c.module).or_else(|| self.enums.get(key).map(|e| &e.module)),
            Space::Funs => self.funs.get(key).map(|f| &f.module),
        }
    }
    fn key_in(&self, space: Space, name: &str, module: &[String]) -> Option<String> {
        let same = match space {
            Space::Types => &self.same_named,
            Space::Funs => &self.same_named_funs,
        };
        if !same.contains(name) {
            // `import crate::rngs::thread::rng` names that function, not another `rng` of the crate
            if let Some(path) = self.imports.get(&(module.to_vec(), name.to_string())).filter(|p| p.len() > 1) {
                return self.key_at(space, path, module);
            }
            return Some(name.to_string());
        }
        let key = scoped_key(module, name);
        if self.module_of(space, &key).is_some() {
            return Some(key);
        }
        if let Some(path) = self.imports.get(&(module.to_vec(), name.to_string())).filter(|p| p.len() > 1) {
            return self.key_at(space, path, module);
        }
        self.globs.get(module).into_iter().flatten().filter(|g| !g.is_empty()).find_map(|glob| {
            let path: Vec<String> = glob.iter().cloned().chain([name.to_string()]).collect();
            self.key_at(space, &path, module)
        })
    }
    fn key_at(&self, space: Space, path: &[String], module: &[String]) -> Option<String> {
        self.key_at_depth(space, path, module, 0)
    }
    /// `key_at`, following at most a few re-exports (`pub use`), which may go in circles.
    fn key_at_depth(&self, space: Space, path: &[String], module: &[String], depth: usize) -> Option<String> {
        let (name, qual) = path.split_last()?;
        if qual.is_empty() {
            return self.key_in(space, name, module);
        }
        let target = self.module_path(qual, module, true)?;
        let same = match space {
            Space::Types => &self.same_named,
            Space::Funs => &self.same_named_funs,
        };
        let key = if same.contains(name) { scoped_key(&target, name) } else { name.clone() };
        if self.module_of(space, &key).is_some_and(|m| *m == target) {
            return Some(key);
        }
        // `crate.ser.ValueSerializer` after `pub use value::ValueSerializer` (or `value::*`) in `ser`
        if target.as_slice() == module || depth > 4 {
            return None;
        }
        if let Some(via) = self.imports.get(&(target.clone(), name.clone())).filter(|p| p.len() > 1) {
            return self.key_at_depth(space, via, &target, depth + 1);
        }
        self.globs.get(&target).into_iter().flatten().filter(|g| !g.is_empty()).find_map(|glob| {
            let path: Vec<String> = glob.iter().cloned().chain([name.clone()]).collect();
            self.key_at_depth(space, &path, &target, depth + 1)
        })
    }
    /// The module a path written in `module` leads to, as the generated Rust resolves it: from
    /// `crate`, `self` or `super`, from the crate root for a top-level module (`join_path` adds
    /// `crate::`), through a module imported by name (`import super::a`), else a child of `module`.
    fn module_path(&self, qual: &[String], module: &[String], follow_imports: bool) -> Option<Vec<String>> {
        let first = qual.first()?.as_str();
        let (mut m, mut rest) = match first {
            "crate" => (Vec::new(), &qual[1..]),
            _ if first == self.crate_name => (Vec::new(), &qual[1..]),
            "self" => (module.to_vec(), &qual[1..]),
            "super" => (module.to_vec(), qual),
            _ if self.root_module(first, module) => (Vec::new(), qual),
            _ => match self.imports.get(&(module.to_vec(), first.to_string())).filter(|_| follow_imports) {
                Some(p) => (self.module_path(p, module, false)?, &qual[1..]),
                None => (module.to_vec(), qual),
            },
        };
        while rest.first().is_some_and(|s| s == "super") {
            m.pop()?;
            rest = &rest[1..];
        }
        m.extend(rest.iter().cloned());
        Some(m)
    }
    /// Lifetime parameters of an Oxie class or enum whose only generics are lifetimes (`Token<'a>`),
    /// so that a bare `Token` in a signature can be completed.
    pub fn type_lifetimes(&self, name: &str) -> usize {
        self.type_lifetimes_in(name, &[])
    }
    /// Like `type_lifetimes`, for `name` written in `module`.
    pub fn type_lifetimes_in(&self, name: &str, module: &[String]) -> usize {
        if let Some(c) = self.class_in(name, module) {
            return if c.type_params == 0 { c.lifetimes } else { 0 };
        }
        match self.enum_in(name, module) {
            Some(e) if !e.has_type_params => e.lifetimes,
            _ => 0,
        }
    }
    /// The enum owning a variant, if the variant name is unique among Oxie enums.
    pub fn variant_enum(&self, variant: &str) -> Option<&EnumInfo> {
        self.variant_enum_in(variant, None)
    }

    /// Like `variant_enum`, but when several enums have the variant, prefers the one declared in `module`.
    pub fn variant_enum_in(&self, variant: &str, module: Option<&[String]>) -> Option<&EnumInfo> {
        if matches!(variant, "Some" | "None" | "Ok" | "Err") {
            return None;
        }
        let owners = self.variant_owner.get(variant)?;
        // a name the module imports from elsewhere (`import super::Redo`, a unit struct) is that one
        let imported_other = |at: &[String]| {
            self.imports.get(&(at.to_vec(), variant.to_string())).is_some_and(|path| {
                let owner = path.len().checked_sub(2).map(|i| &path[i]);
                !owners.iter().any(|(_, n)| Some(n) == owner)
            })
        };
        if module.is_some_and(imported_other) {
            return None;
        }
        // and so is a type the module has from Rust code, its own or through `import editor.*`
        // (as are the names the glob's module imports, for `mod tests { import super::* }`)
        if let Some(m) = module {
            let has = |at: &[String]| self.rust_types.contains(&(at.to_vec(), variant.to_string())) || at != m && imported_other(at);
            if has(m) || self.globs.get(m).into_iter().flatten().filter_map(|g| self.module_path(g, m, true)).any(|g| has(&g)) {
                return None;
            }
        }
        let enum_of = |(m, n): &(Vec<String>, String)| if self.same_named.contains(n) { self.enums.get(&scoped_key(m, n)) } else { self.enum_(n) };
        if owners.len() == 1 {
            return enum_of(&owners[0]);
        }
        let m = module?;
        let local: Vec<&EnumInfo> = owners.iter().filter(|(om, _)| om == m).filter_map(enum_of).collect();
        if local.len() == 1 {
            Some(local[0])
        } else {
            None
        }
    }
    pub fn is_trait(&self, name: &str) -> bool {
        self.traits.contains_key(name)
    }
    /// Whether `name` names a type: it looks like one (`Point`) or the crate declares it (`DB`).
    pub fn is_type_name(&self, name: &str) -> bool {
        is_type_like(name) || self.caps_types.contains(name)
    }
    pub fn is_user_type(&self, name: &str) -> bool {
        self.classes.contains_key(name) || self.enums.contains_key(name) || self.traits.contains_key(name) || self.same_named.contains(name)
    }
    /// Whether some module declares a class `name`.
    pub fn is_class_name(&self, name: &str) -> bool {
        self.classes.contains_key(name) || self.same_named.contains(name) && self.classes.values().any(|c| c.name == name)
    }
}

// -------------------------------------------------------------------------------------------
// Collection

pub struct SourceUnit<'a> {
    pub module: Vec<String>,
    pub file: &'a File,
}

pub fn collect(units: &[SourceUnit], crate_name: &str, extern_crates: HashSet<String>, has_lib: bool) -> Crate {
    // classes and enums declared in function bodies count as types of their module, unless their
    // name is declared elsewhere too
    let mut top = HashSet::new();
    for u in units {
        declared_names(&u.file.items, &mut top);
    }
    let mut local: Vec<(Vec<String>, Vec<Item>)> = units
        .iter()
        .map(|u| {
            let mut out = Vec::new();
            local_types(&u.file.items, &mut out);
            (u.module.clone(), out)
        })
        .collect();
    let mut seen: HashMap<String, usize> = HashMap::new();
    for (_, items) in &local {
        let mut names = HashSet::new();
        declared_names(items, &mut names);
        for n in names {
            *seen.entry(n).or_default() += 1;
        }
    }
    for (_, items) in &mut local {
        let own = |n: &str| seen.get(n) == Some(&1) && !top.contains(n);
        items.retain(|it| match &it.kind {
            ItemKind::Type(t) => own(&t.name),
            ItemKind::Impl(i) => i.self_ty.last_name().is_some_and(own),
            _ => false,
        });
    }
    let files: Vec<(Vec<String>, File)> = local.into_iter().filter(|(_, items)| !items.is_empty()).map(|(m, items)| (m, File { inner_docs: Vec::new(), inner_attrs: Vec::new(), items })).collect();
    let all: Vec<SourceUnit> = units.iter().map(|u| SourceUnit { module: u.module.clone(), file: u.file }).chain(files.iter().map(|(m, f)| SourceUnit { module: m.clone(), file: f })).collect();
    let units = &all[..];
    let mut k = Crate { crate_name: crate_name.to_string(), extern_crates, has_lib, ..Default::default() };
    for u in units {
        if let Some(first) = u.module.first() {
            k.modules.insert(first.clone());
        }
        k.module_paths.insert(u.module.clone());
    }
    for u in units {
        collect_caps_types(&mut k, &u.file.items);
    }
    // pass 1: traits (needed for impl receivers)
    for u in units {
        collect_traits(&mut k, &u.module, &u.file.items);
    }
    // pass 2: types and functions
    for u in units {
        collect_items(&mut k, &u.module, &u.file.items);
    }
    // pass 3: inherent impl blocks add methods to classes
    for u in units {
        collect_impls(&mut k, &u.module, &u.file.items);
    }
    box_interface_params(&mut k);
    // pass 4: receiver inference (fixpoint over mutating methods)
    infer_receivers(&mut k, units);
    compute_derives(&mut k, units);
    k
}

fn collect_caps_types(k: &mut Crate, items: &[Item]) {
    for it in items {
        let name = match &it.kind {
            ItemKind::Type(t) => {
                collect_caps_types(k, &t.members);
                &t.name
            }
            ItemKind::Trait(t) => &t.name,
            ItemKind::TypeAlias { name, .. } => name,
            ItemKind::Mod { items, .. } | ItemKind::Object { items, .. } => {
                collect_caps_types(k, items);
                continue;
            }
            ItemKind::Const(c) | ItemKind::Static(c) => {
                if is_type_like(&c.name) {
                    k.value_names.insert(c.name.clone());
                }
                continue;
            }
            ItemKind::Macro(text) => {
                k.value_names.extend(macro_statics(text).into_iter().filter(|n| is_type_like(n)));
                continue;
            }
            _ => continue,
        };
        if !is_type_like(name) && name.starts_with(|c: char| c.is_uppercase()) {
            k.caps_types.insert(name.clone());
        }
    }
}

/// The statics an item macro declares: `thread_local! { static N: Cell<i32> = .. }`,
/// `lazy_static! { static ref CONFIG: Config = .. }`.
pub fn macro_statics(text: &str) -> Vec<String> {
    let head = text.trim_start().split('!').next().unwrap_or("").trim();
    if !matches!(head.rsplit("::").next(), Some("thread_local" | "lazy_static")) {
        return Vec::new();
    }
    let words: Vec<&str> = text.split(|c: char| !(c.is_alphanumeric() || c == '_')).filter(|w| !w.is_empty()).collect();
    let mut out = Vec::new();
    for (i, w) in words.iter().enumerate() {
        if *w == "static" {
            let name = if words.get(i + 1) == Some(&"ref") { words.get(i + 2) } else { words.get(i + 1) };
            if let Some(n) = name {
                out.push(n.to_string());
            }
        }
    }
    out
}

/// The names of the types, interfaces and functions `items` declare, in modules too.
fn declared_names(items: &[Item], out: &mut HashSet<String>) {
    for it in items {
        match &it.kind {
            ItemKind::Type(t) => {
                out.insert(t.name.clone());
                declared_names(&t.members, out);
            }
            ItemKind::Trait(t) => {
                out.insert(t.name.clone());
            }
            ItemKind::Mod { items, .. } => declared_names(items, out),
            _ => {}
        }
    }
}

/// The classes, enums and `impl` blocks declared inside function bodies of `items`.
fn local_types(items: &[Item], out: &mut Vec<Item>) {
    fn block(b: &Block, out: &mut Vec<Item>) {
        for s in &b.stmts {
            if let Stmt::Item(i) = s {
                if matches!(i.0.kind, ItemKind::Type(_) | ItemKind::Impl(_)) {
                    out.push(i.0.clone());
                }
                local_types(std::slice::from_ref(&i.0), out);
            }
        }
    }
    for it in items {
        match &it.kind {
            ItemKind::Fun(FunDecl { body: Some(FunBody::Block(b)), .. }) => block(b, out),
            ItemKind::Test(t) => block(&t.body, out),
            ItemKind::Type(t) => local_types(&t.members, out),
            ItemKind::Trait(t) => local_types(&t.members, out),
            ItemKind::Impl(i) => local_types(&i.members, out),
            ItemKind::Mod { items, .. } | ItemKind::Object { items, .. } => local_types(items, out),
            _ => {}
        }
    }
    crate::visit::walk_items(items, &mut |e| {
        for it in crate::fields::local_items(e) {
            if matches!(it.kind, ItemKind::Type(_) | ItemKind::Impl(_)) {
                out.push(it.clone());
            }
            local_types(std::slice::from_ref(it), out);
        }
        true
    });
}

/// Generics made only of lifetimes (or none), which don't stand in the way of derives.
fn only_lifetimes(g: &Generics) -> bool {
    g.params.iter().all(|p| matches!(p, GenericParam::Lifetime { .. }))
}

/// A path segment without type arguments (`Token` or `Token<'a>`).
fn no_type_args(seg: &PathSeg) -> bool {
    seg.generics.as_ref().is_none_or(|g| g.iter().all(|a| matches!(a, GenericArg::Lifetime(_))))
}

/// Methods that return `this` itself as their class (`fun url(u: &str): B { url = u; this }`, a
/// builder) take `self` by value, or `mut self` when they change it: a reference can't be
/// returned as the value. Not interface methods, whose receivers the interface decides, nor
/// methods of shared classes, whose `this` is a handle. Runs after receiver inference (`k`)
/// tells which methods change `this`; returns whether it changed any.
pub fn by_value_receivers(files: &mut [(&[String], &mut File)], k: &Crate) -> bool {
    fn go(items: &mut [Item], module: &[String], k: &Crate) -> bool {
        let mut changed = false;
        for it in items {
            match &mut it.kind {
                ItemKind::Type(t) if !t.shared => {
                    let key = k.decl_key(module, &t.name);
                    let iface = |n: &str| t.supertypes.iter().filter_map(|s| s.last_name()).any(|st| k.traits.get(st).is_some_and(|ti| ti.methods.contains_key(n)));
                    // `this`, or what a method found here returns for it (`reset().times(2)`)
                    fn this(e: &Expr, by_value: &[(String, bool)]) -> bool {
                        let found = |n: &str| by_value.iter().any(|(m, _)| m == n);
                        match &e.kind {
                            ExprKind::SelfValue => true,
                            ExprKind::Paren(x) => this(x, by_value),
                            ExprKind::Call { callee, .. } => callee.ident().is_some_and(found),
                            ExprKind::MethodCall { recv, name, safe: false, .. } => found(name) && this(recv, by_value),
                            _ => false,
                        }
                    }
                    // does the method change `this` (so it takes `mut self`), other than by passing it
                    // on to a method that takes `self`?
                    let class = k.classes.get(&key);
                    let changes = |f: &FunDecl, by_value: &[(String, bool)]| {
                        let Some((why, _)) = class.and_then(|c| c.methods.get(&f.name)).filter(|mi| mi.recv == Recv::RefMut).and_then(|mi| mi.recv_why.as_ref()) else { return false };
                        if !by_value.iter().any(|(n, _)| why.starts_with(&format!("it calls `{}()`", n))) {
                            return true;
                        }
                        let fields: HashSet<String> = class.map(|c| c.fields.iter().map(|f| f.name.clone()).collect()).unwrap_or_default();
                        let mut recvs: HashMap<String, Recv> = class.map(|c| c.methods.iter().map(|(n, m)| (n.clone(), m.recv)).collect()).unwrap_or_default();
                        recvs.extend(by_value.iter().map(|(n, _)| (n.clone(), Recv::Value)));
                        analyze_fun(f, &fields, &recvs).mutates_self
                    };
                    let mut by_value: Vec<(String, bool)> = Vec::new();
                    loop {
                        let before = by_value.len();
                        for m in &t.members {
                            let ItemKind::Fun(f) = &m.kind else { continue };
                            let own = f.ret.as_ref().and_then(|r| r.single_name()).is_some_and(|n| n == "Self" || n == t.name);
                            if f.self_param.is_some() || f.mods.is_static || f.mods.is_override || !own || iface(&f.name) || by_value.iter().any(|(n, _)| *n == f.name) {
                                continue;
                            }
                            let Some(body) = &f.body else { continue };
                            if crate::visit::return_values(body).iter().any(|e| this(e, &by_value)) {
                                by_value.push((f.name.clone(), f.mods.is_mut || changes(f, &by_value)));
                            }
                        }
                        if by_value.len() == before {
                            break;
                        }
                    }
                    for m in &mut t.members {
                        if let ItemKind::Fun(f) = &mut m.kind {
                            if let Some((_, mutable)) = by_value.iter().find(|(n, _)| *n == f.name) {
                                f.self_param = Some(SelfParam::Value { mutable: *mutable });
                                f.mods.is_mut = false;
                                changed = true;
                            }
                        }
                    }
                    changed |= go(&mut t.members, module, k);
                }
                ItemKind::Mod { name, items } => {
                    let inner: Vec<String> = module.iter().cloned().chain([name.clone()]).collect();
                    changed |= go(items, &inner, k);
                }
                _ => {}
            }
        }
        changed
    }
    let mut changed = false;
    for (module, f) in files.iter_mut() {
        changed |= go(&mut f.items, module, k);
    }
    changed
}

/// Extension functions take `&mut self` when their body changes the receiver, like methods:
/// assigns its fields, calls a method of it that does (`&mut self` methods of an Oxie class, a
/// mutating std method like `push` on any other type), or takes `&mut` of a field. Runs after
/// receiver inference (`k`), which tells what the class's methods take.
pub fn ext_receivers(files: &mut [(&[String], &mut File)], k: &Crate) {
    fn go(items: &mut [Item], module: &[String], k: &Crate) {
        for it in items {
            match &mut it.kind {
                ItemKind::Fun(f) if f.receiver.is_some() && f.self_param.is_none() && !f.mods.is_mut && !f.mods.is_static => {
                    let class = f.receiver.as_ref().and_then(|r| r.single_name()).and_then(|n| k.class_in(n, module)).filter(|c| !c.is_shared);
                    let fields: HashSet<String> = class.map(|c| c.fields.iter().map(|f| f.name.clone()).collect()).unwrap_or_default();
                    let methods: HashMap<String, Recv> = match class {
                        Some(c) => c.methods.iter().map(|(n, m)| (n.clone(), m.recv)).collect(),
                        // not the names that also read on common types (`str::replace`, `Iterator::take`)
                        None => MUTATING_METHODS.iter().filter(|m| !matches!(**m, "replace" | "take" | "set" | "read" | "write" | "next" | "nth")).map(|m| (m.to_string(), Recv::RefMut)).collect(),
                    };
                    if analyze_fun(f, &fields, &methods).mutates_self {
                        f.mods.is_mut = true;
                    }
                }
                ItemKind::Mod { name, items } => {
                    let inner: Vec<String> = module.iter().cloned().chain([name.clone()]).collect();
                    go(items, &inner, k);
                }
                _ => {}
            }
        }
    }
    for (module, f) in files.iter_mut() {
        go(&mut f.items, module, k);
    }
}

/// Classes and enums whose fields hold references get a lifetime parameter: `class Parser(input: &str)`
/// becomes `struct Parser<'a> { input: &'a str }`. A field naming such a type (`Vec<Token>`) needs
/// one too, so this runs over the whole crate before symbols are collected.
pub fn auto_type_lifetimes(files: &mut [&mut File]) {
    fn gather<'a>(items: &'a [Item], out: &mut Vec<&'a TypeDecl>) {
        for it in items {
            match &it.kind {
                ItemKind::Type(t) => {
                    out.push(t);
                    gather(&t.members, out);
                }
                ItemKind::Mod { items, .. } => gather(items, out),
                _ => {}
            }
        }
    }
    fn needs(t: &Type, lt: &HashMap<String, usize>) -> bool {
        match t {
            Type::Ref { lifetime: None, .. } => true,
            Type::Ref { inner, .. } | Type::Slice(inner) | Type::Array(inner, _) | Type::Option(inner) => needs(inner, lt),
            Type::Tuple(v) => v.iter().any(|t| needs(t, lt)),
            Type::Path { segs, .. } => segs.iter().any(|s| match &s.generics {
                None => segs.len() == 1 && lt.contains_key(&s.name),
                Some(g) => g.iter().any(|a| matches!(a, GenericArg::Type(t) if needs(t, lt))),
            }),
            _ => false,
        }
    }
    fn fill(t: &mut Type, lt: &HashMap<String, usize>) {
        match t {
            Type::Ref { lifetime, inner, .. } => {
                if lifetime.is_none() {
                    *lifetime = Some("a".into());
                }
                fill(inner, lt);
            }
            Type::Slice(inner) | Type::Array(inner, _) | Type::Option(inner) => fill(inner, lt),
            Type::Tuple(v) => v.iter_mut().for_each(|t| fill(t, lt)),
            Type::Path { segs, .. } => {
                let single = segs.len() == 1;
                for s in segs {
                    match &mut s.generics {
                        None if single => {
                            if let Some(&n) = lt.get(&s.name) {
                                s.generics = Some(vec![GenericArg::Lifetime("a".into()); n]);
                            }
                        }
                        Some(g) => {
                            for a in g {
                                if let GenericArg::Type(t) = a {
                                    fill(t, lt);
                                }
                            }
                        }
                        None => {}
                    }
                }
            }
            _ => {}
        }
    }
    fn field_types(t: &mut TypeDecl) -> Vec<&mut Type> {
        let mut v: Vec<&mut Type> = t.fields.iter_mut().map(|f| &mut f.ty).collect();
        v.extend(t.tuple_fields.iter_mut().map(|(_, ty)| ty));
        for var in &mut t.variants {
            match &mut var.fields {
                VariantFields::Unit => {}
                VariantFields::Tuple(ts) => v.extend(ts.iter_mut()),
                VariantFields::Named(fs) => v.extend(fs.iter_mut().map(|f| &mut f.ty)),
            }
        }
        v
    }
    fn apply(items: &mut [Item], auto: &HashSet<String>, lt: &HashMap<String, usize>) {
        for it in items {
            match &mut it.kind {
                ItemKind::Type(t) => {
                    if t.generics.is_empty() && auto.contains(&t.name) {
                        t.generics.params.push(GenericParam::Lifetime { name: "a".into(), bounds: Vec::new() });
                        for ty in field_types(t) {
                            fill(ty, lt);
                        }
                    }
                    apply(&mut t.members, auto, lt);
                }
                ItemKind::Mod { items, .. } => apply(items, auto, lt),
                _ => {}
            }
        }
    }

    let mut lt: HashMap<String, usize> = HashMap::new();
    let mut auto: HashSet<String> = HashSet::new();
    {
        let mut decls = Vec::new();
        for f in files.iter() {
            gather(&f.items, &mut decls);
        }
        for t in &decls {
            let n = t.generics.params.iter().filter(|g| matches!(g, GenericParam::Lifetime { .. })).count();
            if n > 0 && only_lifetimes(&t.generics) {
                lt.insert(t.name.clone(), n);
            }
        }
        let has_ref = |t: &TypeDecl, lt: &HashMap<String, usize>| {
            t.fields.iter().map(|f| &f.ty).chain(t.tuple_fields.iter().map(|(_, ty)| ty)).any(|ty| needs(ty, lt))
                || t.variants.iter().any(|v| match &v.fields {
                    VariantFields::Unit => false,
                    VariantFields::Tuple(ts) => ts.iter().any(|ty| needs(ty, lt)),
                    VariantFields::Named(fs) => fs.iter().any(|f| needs(&f.ty, lt)),
                })
        };
        loop {
            let mut changed = false;
            for t in &decls {
                if t.generics.is_empty() && !auto.contains(&t.name) && has_ref(t, &lt) {
                    auto.insert(t.name.clone());
                    lt.insert(t.name.clone(), 1);
                    changed = true;
                }
            }
            if !changed {
                break;
            }
        }
    }
    if auto.is_empty() {
        return;
    }
    for f in files.iter_mut() {
        apply(&mut f.items, &auto, &lt);
    }
}

fn has_attr(attrs: &[Attr], name: &str) -> bool {
    attrs.iter().any(|a| a.name() == name)
}

/// Kotlin-like value semantics: data classes and enums get Debug/Clone/PartialEq,
/// plus Copy/Eq/Hash when every field type is known to support them.
fn compute_derives(k: &mut Crate, units: &[SourceUnit]) {
    fn gather<'a>(k: &Crate, module: &[String], items: &'a [Item], out: &mut Vec<(String, &'a TypeDecl, &'a [Attr])>) {
        for it in items {
            match &it.kind {
                ItemKind::Type(t) => {
                    out.push((k.decl_key(module, &t.name), t, &it.attrs));
                    gather(k, module, &t.members, out);
                }
                ItemKind::Mod { name, items } => {
                    let mut m = module.to_vec();
                    m.push(name.clone());
                    gather(k, &m, items, out);
                }
                _ => {}
            }
        }
    }
    // keyed like `classes`: a type whose name several modules declare doesn't count as `Copy` or
    // `Hash` in field types, which only name it
    let mut decls = Vec::new();
    for u in units {
        gather(k, &u.module, &u.file.items, &mut decls);
    }
    let mut copy: HashSet<String> = HashSet::new();
    let mut hash: HashSet<String> = HashSet::new();
    let field_types = |t: &TypeDecl| -> Vec<Type> {
        let mut v: Vec<Type> = t.fields.iter().map(|f| f.ty.clone()).collect();
        for var in &t.variants {
            match &var.fields {
                VariantFields::Unit => {}
                VariantFields::Tuple(ts) => v.extend(ts.iter().cloned()),
                VariantFields::Named(fs) => v.extend(fs.iter().map(|f| f.ty.clone())),
            }
        }
        v
    };
    let eligible = |t: &TypeDecl, attrs: &[Attr]| {
        !t.is_error && !has_attr(attrs, "no_auto_derive") && matches!(t.kind, TypeKind::Data | TypeKind::Enum) && only_lifetimes(&t.generics)
    };
    for _ in 0..6 {
        let mut changed = false;
        for (key, t, attrs) in &decls {
            if !eligible(t, attrs) {
                continue;
            }
            let fts = field_types(t);
            if !copy.contains(key) && fts.iter().all(|ft| is_copy_simple(ft, &copy)) {
                copy.insert(key.clone());
                changed = true;
            }
            if !hash.contains(key) && fts.iter().all(|ft| is_hash_simple(ft, &hash)) {
                hash.insert(key.clone());
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    for (key, t, attrs) in &decls {
        let mut d: Vec<String> = Vec::new();
        if t.is_error {
            d.push("Debug".into());
        } else if has_attr(attrs, "no_auto_derive") {
        } else if matches!(t.kind, TypeKind::Data | TypeKind::Enum) {
            d.extend(["Debug", "Clone"].iter().map(|s| s.to_string()));
            if only_lifetimes(&t.generics) && copy.contains(key) {
                d.push("Copy".into());
            }
            d.push("PartialEq".into());
            if only_lifetimes(&t.generics) && hash.contains(key) {
                d.push("Eq".into());
                d.push("Hash".into());
            }
        }
        k.derives.insert(key.clone(), d);
    }
}

fn collect_traits(k: &mut Crate, module: &[String], items: &[Item]) {
    for it in items {
        match &it.kind {
            ItemKind::Trait(t) => {
                let mut methods = HashMap::new();
                let mut assoc = Vec::new();
                let generics = t.generics.params.iter().filter_map(|g| if let GenericParam::Type { name, .. } = g { Some(name.clone()) } else { None }).collect();
                for m in &t.members {
                    match &m.kind {
                        ItemKind::Fun(f) => {
                            let mut mi = method_info_explicit(f, None).unwrap_or_else(|| default_mi(f));
                            mi.iface = true;
                            add_method(&mut methods, &f.name, mi);
                        }
                        ItemKind::TypeAlias { name, .. } => assoc.push(name.clone()),
                        _ => {}
                    }
                }
                let supertraits = t
                    .supertraits
                    .iter()
                    .filter_map(|s| match s {
                        Type::Lifetime(l) => Some(format!("'{}", l)),
                        s => s.last_name().map(String::from),
                    })
                    .collect();
                let forward = forwardable(t);
                insert_unique(&mut k.traits, &mut k.ambiguous, t.name.clone(), TraitInfo { name: t.name.clone(), module: module.to_vec(), methods, assoc_types: assoc, supertraits, forward, generics });
            }
            ItemKind::Mod { name, items } => {
                let mut m = module.to_vec();
                m.push(name.clone());
                collect_traits(k, &m, items);
            }
            _ => {}
        }
    }
}

/// See `TraitInfo::forward`.
fn forwardable(t: &TraitDecl) -> Option<bool> {
    if t.is_unsafe || !t.generics.params.is_empty() {
        return None;
    }
    let mentions_self = |ty: &Type| crate::symbols::type_text(ty).split(|c: char| !(c.is_alphanumeric() || c == '_')).any(|w| w == "Self");
    let mut all_ref = true;
    for m in &t.members {
        let ItemKind::Fun(f) = &m.kind else { return None };
        let simple = f.contexts.is_empty()
            && !(f.mods.is_async || f.mods.is_unsafe || f.mods.is_const || f.mods.abi.is_some())
            && f.generics.params.iter().all(|g| matches!(g, GenericParam::Lifetime { .. }))
            && f.params.iter().all(|p| matches!(p.pat, Pat::Ident { sub: None, .. }) && !mentions_self(&p.ty) && !matches!(p.ty, Type::Fn { .. } | Type::Impl(_)))
            && !f.ret.as_ref().is_some_and(mentions_self)
            && f.receiver.is_none();
        if !simple {
            return None;
        }
        let mutable = match &f.self_param {
            Some(SelfParam::Ref) => false,
            Some(SelfParam::RefMut) => true,
            None if f.mods.is_static || STATIC_METHOD_NAMES.contains(&f.name.as_str()) || f.mods.is_operator => return None,
            None => f.mods.is_mut || f.body.is_some() && analyze_fun(f, &HashSet::new(), &HashMap::new()).mutates_self,
            Some(_) => return None,
        };
        all_ref &= !mutable;
    }
    Some(all_ref)
}

/// Does the method implement a method of one of the interfaces `supertypes` names (`override`,
/// or a method named like one of theirs)?
fn implements_method(k: &Crate, f: &FunDecl, supertypes: &[Type]) -> bool {
    f.mods.is_override || supertypes.iter().filter_map(|s| s.last_name()).any(|n| k.traits.get(n).is_some_and(|t| t.methods.contains_key(&f.name)))
}

/// Marks the parameters of interface methods typed as an Oxie interface `boxed`.
fn box_interface_params(k: &mut Crate) {
    let ifaces: HashSet<String> = k.traits.keys().filter(|n| !k.classes.contains_key(*n) && !k.enums.contains_key(*n)).cloned().collect();
    let mark = |m: &mut MethodInfo| {
        if m.iface {
            for p in &mut m.params {
                p.boxed = is_iface_type(&ifaces, &p.ty);
            }
        }
    };
    k.traits.values_mut().flat_map(|t| t.methods.values_mut()).for_each(mark);
    k.classes.values_mut().flat_map(|c| c.methods.values_mut()).for_each(mark);
    k.enums.values_mut().flat_map(|e| e.methods.values_mut()).for_each(mark);
}

/// A bare interface name among `ifaces` (`Shape`, not `Box<Shape>` or `&Shape`).
pub(crate) fn is_iface_type(ifaces: &HashSet<String>, t: &Type) -> bool {
    matches!(t, Type::Path { segs, .. } if segs.len() == 1 && segs[0].generics.is_none() && segs[0].fn_sugar.is_none() && ifaces.contains(&segs[0].name))
}

fn insert_unique<V>(map: &mut HashMap<String, V>, amb: &mut HashSet<String>, key: String, v: V) {
    if map.contains_key(&key) {
        amb.insert(key);
    } else {
        map.insert(key, v);
    }
}

/// The key for a new class (`is_class`) or enum `name` declared in `module`: its name, or once
/// another module declares the name too, `scoped_key` for every declaration of it. `None` for a
/// second one in the same module, which is left out (the name is ambiguous there).
fn type_key_for(k: &mut Crate, name: &str, module: &[String], is_class: bool) -> Option<String> {
    if !k.same_named.contains(name) {
        let class_mod = k.classes.get(name).map(|c| c.module.clone());
        let enum_mod = k.enums.get(name).map(|e| e.module.clone());
        if class_mod.iter().chain(&enum_mod).all(|m| m == module) {
            if if is_class { class_mod.is_some() } else { enum_mod.is_some() } {
                k.ambiguous.insert(name.to_string());
                return None;
            }
            return Some(name.to_string());
        }
        k.same_named.insert(name.to_string());
        if let Some(c) = k.classes.remove(name) {
            k.classes.insert(scoped_key(&c.module, name), c);
        }
        if let Some(e) = k.enums.remove(name) {
            k.enums.insert(scoped_key(&e.module, name), e);
        }
    }
    let key = scoped_key(module, name);
    let taken = if is_class { k.classes.contains_key(&key) } else { k.enums.contains_key(&key) };
    (!taken).then_some(key)
}

/// The functions a `rust { }` block declares at its top level (`fn total(..)`, `pub(crate) fn ..`).
fn raw_fn_names(text: &str) -> Vec<String> {
    raw_items(text).into_iter().filter(|(kw, _)| *kw == "fn").map(|(_, n)| n).collect()
}

/// The CamelCase names in an item macro, like the unit structs `actions!(editor, [Newline])`
/// declares.
fn camel_words(text: &str) -> Vec<String> {
    let words = text.split(|c: char| !(c.is_alphanumeric() || c == '_'));
    words.filter(|w| w.starts_with(|c: char| c.is_ascii_uppercase()) && w.contains(|c: char| c.is_ascii_lowercase())).map(str::to_string).collect()
}

/// The macro calls at the top level of a `rust { }` block (`actions!(editor, [Newline])`).
fn raw_macro_calls(text: &str) -> Vec<&str> {
    let b = text.as_bytes();
    let mut out = Vec::new();
    let (mut i, mut depth) = (0, 0usize);
    while i < b.len() {
        match b[i] {
            b'{' => depth += 1,
            b'}' => depth = depth.saturating_sub(1),
            b'!' if depth == 0 && i > 0 && (b[i - 1].is_ascii_alphanumeric() || b[i - 1] == b'_') => {
                let open = i + 1 + text[i + 1..].len() - text[i + 1..].trim_start().len();
                let close = match b.get(open) {
                    Some(b'(') => b')',
                    Some(b'[') => b']',
                    Some(b'{') => b'}',
                    _ => {
                        i += 1;
                        continue;
                    }
                };
                let (mut j, mut d) = (open, 0usize);
                while j < b.len() {
                    if b[j] == b[open] {
                        d += 1;
                    } else if b[j] == close {
                        d -= 1;
                        if d == 0 {
                            break;
                        }
                    }
                    j += 1;
                }
                out.push(&text[open..j.min(b.len())]);
                i = j;
            }
            _ => {}
        }
        i += 1;
    }
    out
}

/// The items a `rust { }` block declares at its top level, with their keywords: `("fn", "total")`,
/// `("struct", "Newline")`.
fn raw_items(text: &str) -> Vec<(&'static str, String)> {
    let b = text.as_bytes();
    let ident = |i: usize| b[i].is_ascii_alphanumeric() || b[i] == b'_';
    let mut out = Vec::new();
    let (mut i, mut depth) = (0, 0usize);
    while i < b.len() {
        match b[i] {
            b'/' if b.get(i + 1) == Some(&b'/') => i = text[i..].find('\n').map_or(b.len(), |n| i + n),
            b'/' if b.get(i + 1) == Some(&b'*') => i = text[i + 2..].find("*/").map_or(b.len(), |n| i + n + 4),
            b'"' => {
                i += 1;
                while i < b.len() && b[i] != b'"' {
                    i += if b[i] == b'\\' { 2 } else { 1 };
                }
                i += 1;
            }
            b'r' if (i == 0 || !ident(i - 1)) && b[i + 1..].iter().find(|&&c| c != b'#') == Some(&b'"') => {
                let hashes = b[i + 1..].iter().take_while(|&&c| c == b'#').count();
                let close = format!("\"{}", "#".repeat(hashes));
                let open = i + 1 + hashes + 1;
                i = text.get(open..).and_then(|t| t.find(&close)).map_or(b.len(), |n| open + n + close.len());
            }
            // a char literal (not a lifetime): `'{'`, `'\''`
            b'\'' if b.get(i + 2) == Some(&b'\'') && b.get(i + 1) != Some(&b'\\') => i += 3,
            b'\'' if b.get(i + 1) == Some(&b'\\') => i = text[i + 2..].find('\'').map_or(b.len(), |n| i + n + 3),
            b'{' => {
                depth += 1;
                i += 1;
            }
            b'}' => {
                depth = depth.saturating_sub(1);
                i += 1;
            }
            c if c.is_ascii_alphabetic() || c == b'_' => {
                let start = i;
                while i < b.len() && ident(i) {
                    i += 1;
                }
                let kw = ["fn", "struct", "enum", "union", "type", "trait"].into_iter().find(|kw| *kw == &text[start..i]);
                if let Some(kw) = kw.filter(|_| depth == 0) {
                    let rest = text[i..].trim_start();
                    let name: String = rest.chars().take_while(|c| c.is_alphanumeric() || *c == '_').collect();
                    if !name.is_empty() {
                        out.push((kw, name));
                    }
                }
            }
            _ => i += 1,
        }
    }
    out
}

/// The key for a new function `name` declared in `module`, as `type_key_for` does for types.
fn fun_key_for(k: &mut Crate, name: &str, module: &[String]) -> Option<String> {
    if !k.same_named_funs.contains(name) {
        match k.funs.get(name).map(|f| f.module == module) {
            None => return Some(name.to_string()),
            Some(true) => {
                k.ambiguous.insert(name.to_string());
                return None;
            }
            Some(false) => {
                k.same_named_funs.insert(name.to_string());
                if let Some(f) = k.funs.remove(name) {
                    k.funs.insert(scoped_key(&f.module, name), f);
                }
            }
        }
    }
    let key = scoped_key(module, name);
    (!k.funs.contains_key(&key)).then_some(key)
}

/// Records what `module` imports: `import a::Counter` (or `a::{b::Counter, Point as P}`) by name,
/// `import a::*` as a glob.
fn collect_import(k: &mut Crate, module: &[String], prefix: &[String], tree: &UseTree) {
    match tree {
        UseTree::Path { segments, alias } => {
            let path: Vec<String> = prefix.iter().chain(segments).cloned().collect();
            if let Some(last) = path.last().filter(|l| path.len() > 1 && *l != "self") {
                let name = alias.clone().unwrap_or_else(|| last.clone());
                k.imports.insert((module.to_vec(), name), path);
            }
        }
        UseTree::Glob { segments } => {
            let path: Vec<String> = prefix.iter().chain(segments).cloned().collect();
            k.globs.entry(module.to_vec()).or_default().push(path);
        }
        UseTree::Group { segments, items } => {
            let path: Vec<String> = prefix.iter().chain(segments).cloned().collect();
            for i in items {
                collect_import(k, module, &path, i);
            }
        }
    }
}

pub fn params_info(f: &FunDecl) -> Vec<ParamInfo> {
    let generics: Vec<String> = f
        .generics
        .params
        .iter()
        .filter_map(|g| match g {
            GenericParam::Type { name, .. } | GenericParam::Const { name, .. } => Some(name.clone()),
            GenericParam::Lifetime { .. } => None,
        })
        .collect();
    f.params
        .iter()
        .map(|p| ParamInfo { name: p.name().map(|s| s.to_string()), ty: p.ty.clone(), default: p.default.clone(), context: p.context.then(|| f.name.clone()), generics: generics.clone(), boxed: false, lent: p.lent, cfg: p.attrs.iter().any(|a| a.starts_with("cfg(")) })
        .collect()
}

/// Adds a method; one declared again (`@cfg(windows) fun new(c, console)` beside
/// `@cfg(not(windows)) fun new(c)`) keeps the first, taking the parameters only some versions
/// have as ones a call may leave out.
fn add_method(methods: &mut HashMap<String, MethodInfo>, name: &str, mi: MethodInfo) {
    let Some(old) = methods.get_mut(name) else {
        methods.insert(name.to_string(), mi);
        return;
    };
    let common = old.params.len().min(mi.params.len());
    if mi.params.len() > common {
        old.params.extend(mi.params[common..].iter().cloned());
    }
    for p in &mut old.params[common..] {
        p.cfg = true;
    }
}

fn default_mi(f: &FunDecl) -> MethodInfo {
    MethodInfo { recv: Recv::Ref, params: params_info(f), throws: !matches!(f.throws, Throws::No), ret: f.ret.clone(), explicit: false, recv_why: None, iface: false }
}

/// Receiver determined purely from syntax, or None if it must be inferred.
fn method_info_explicit(f: &FunDecl, trait_recv: Option<Recv>) -> Option<MethodInfo> {
    let params = params_info(f);
    let throws = !matches!(f.throws, Throws::No);
    let recv = match &f.self_param {
        Some(SelfParam::Ref) => Some(Recv::Ref),
        Some(SelfParam::RefMut) => Some(Recv::RefMut),
        Some(SelfParam::Typed(Type::Ref { mutable: true, .. })) => Some(Recv::RefMut),
        Some(SelfParam::Typed(Type::Ref { mutable: false, .. })) => Some(Recv::Ref),
        Some(SelfParam::Value { .. }) | Some(SelfParam::Typed(_)) | Some(SelfParam::TypedMut(_)) => Some(Recv::Value),
        None if f.mods.is_static => Some(Recv::Static),
        None if f.mods.is_mut => Some(Recv::RefMut),
        None if trait_recv.is_some() => trait_recv,
        None if STATIC_METHOD_NAMES.contains(&f.name.as_str()) => Some(Recv::Static),
        None if f.mods.is_operator => Some(Recv::Value),
        None => None,
    };
    recv.map(|recv| MethodInfo { recv, params, throws, ret: f.ret.clone(), explicit: trait_recv.is_none() || f.self_param.is_some() || f.mods.is_static || f.mods.is_mut, recv_why: None, iface: false })
}

fn collect_items(k: &mut Crate, module: &[String], items: &[Item]) {
    for it in items {
        if let ItemKind::Type(t) = &it.kind {
            let inner: Vec<&Item> = t.members.iter().filter(|m| matches!(m.kind, ItemKind::Type(_) | ItemKind::Trait(_))).collect();
            if !inner.is_empty() {
                let names: Vec<String> = inner
                    .iter()
                    .filter_map(|m| match &m.kind {
                        ItemKind::Type(x) => Some(x.name.clone()),
                        ItemKind::Trait(x) => Some(x.name.clone()),
                        _ => None,
                    })
                    .collect();
                k.nested.insert(t.name.clone(), names);
                let owned: Vec<Item> = inner.into_iter().cloned().collect();
                collect_items(k, module, &owned);
            }
        }
        match &it.kind {
            ItemKind::Fun(f) if f.receiver.is_none() => {
                let generic_ret = f.ret.as_ref().is_some_and(|r| {
                    let text = crate::symbols::type_text(r);
                    let names: Vec<&str> = text.split(|c: char| !(c.is_alphanumeric() || c == '_')).collect();
                    f.generics.params.iter().any(|g| matches!(g, GenericParam::Type { name, .. } if names.contains(&name.as_str())))
                });
                let info = FunInfo { module: module.to_vec(), params: params_info(f), throws: !matches!(f.throws, Throws::No), ret: f.ret.clone(), raw: false, generic_ret };
                if let Some(key) = fun_key_for(k, &f.name, module) {
                    k.funs.insert(key, info);
                }
            }
            ItemKind::Raw(text) => {
                for name in raw_fn_names(text) {
                    let info = FunInfo { module: module.to_vec(), params: Vec::new(), throws: false, ret: None, raw: true, generic_ret: false };
                    if let Some(key) = fun_key_for(k, &name, module) {
                        k.funs.insert(key, info);
                    }
                }
                for (_, name) in raw_items(text).into_iter().filter(|(kw, _)| *kw != "fn") {
                    k.rust_types.insert((module.to_vec(), name));
                }
                for name in raw_macro_calls(text).into_iter().flat_map(camel_words) {
                    k.rust_types.insert((module.to_vec(), name));
                }
            }
            ItemKind::Macro(text) => {
                for name in camel_words(text) {
                    k.rust_types.insert((module.to_vec(), name));
                }
            }
            ItemKind::Fun(f) => {
                k.ext_traits.push((module.to_vec(), ext_trait_name(module, &f.name)));
            }
            ItemKind::Type(t) => match t.kind {
                TypeKind::Class | TypeKind::Data | TypeKind::TupleStruct | TypeKind::Union => {
                    let fields = t
                        .fields
                        .iter()
                        .map(|f| FieldInfo { name: f.name.clone(), ty: f.ty.clone(), default: f.default.clone(), in_ctor: f.in_ctor })
                        .collect();
                    let mut methods = HashMap::new();
                    let mut user_new = false;
                    for m in &t.members {
                        if let ItemKind::Fun(f) = &m.kind {
                            if f.name == "new" {
                                user_new = true;
                            }
                            let mut mi = method_info_explicit(f, None).unwrap_or_else(|| default_mi(f));
                            mi.iface = implements_method(k, f, &t.supertypes);
                            add_method(&mut methods, &f.name, mi);
                        }
                    }
                    let info = ClassInfo {
                        name: t.name.clone(),
                        module: module.to_vec(),
                        fields,
                        methods,
                        user_new,
                        is_data: t.kind == TypeKind::Data,
                        is_tuple: t.kind == TypeKind::TupleStruct,
                        is_union: t.kind == TypeKind::Union,
                        is_unit: t.kind != TypeKind::Union && !t.shared && t.fields.is_empty() && t.tuple_fields.is_empty(),
                        is_shared: t.shared,
                        is_sync: t.shared && it.attrs.iter().any(|a| a.name() == "sync"),
                        has_generics: !only_lifetimes(&t.generics),
                        type_params: t.generics.params.iter().filter(|g| matches!(g, GenericParam::Type { .. })).count(),
                        lifetimes: t.generics.params.iter().filter(|g| matches!(g, GenericParam::Lifetime { .. })).count(),
                        contexts: t.contexts.iter().filter_map(|p| p.name().map(String::from)).collect(),
                        interfaces: t.supertypes.iter().filter_map(|s| s.last_name().map(String::from)).collect(),
                        consts: member_consts(&t.members),
                    };
                    if let Some(key) = type_key_for(k, &t.name, module, true) {
                        k.classes.insert(key, info);
                    }
                }
                TypeKind::Enum => {
                    let variants: Vec<(String, VariantShape)> = t
                        .variants
                        .iter()
                        .map(|v| {
                            let shape = match &v.fields {
                                VariantFields::Unit => VariantShape::Unit,
                                VariantFields::Tuple(ts) => VariantShape::Tuple(ts.clone()),
                                VariantFields::Named(fs) => VariantShape::Named(fs.iter().map(|f| (f.name.clone(), f.ty.clone())).collect()),
                            };
                            (v.name.clone(), shape)
                        })
                        .collect();
                    let mut methods = HashMap::new();
                    for m in &t.members {
                        if let ItemKind::Fun(f) = &m.kind {
                            let mut mi = method_info_explicit(f, None).unwrap_or_else(|| default_mi(f));
                            mi.iface = implements_method(k, f, &t.supertypes);
                            add_method(&mut methods, &f.name, mi);
                        }
                    }
                    let info = EnumInfo {
                        name: t.name.clone(),
                        module: module.to_vec(),
                        variants,
                        methods,
                        lifetimes: t.generics.params.iter().filter(|g| matches!(g, GenericParam::Lifetime { .. })).count(),
                        has_type_params: !only_lifetimes(&t.generics),
                        interfaces: t.supertypes.iter().filter_map(|s| s.last_name().map(String::from)).collect(),
                        consts: member_consts(&t.members),
                    };
                    if let Some(key) = type_key_for(k, &t.name, module, false) {
                        for (vn, _) in &info.variants {
                            k.variant_owner.entry(vn.clone()).or_default().push((module.to_vec(), t.name.clone()));
                        }
                        k.enums.insert(key, info);
                    }
                }
            },
            ItemKind::Import(decl) => collect_import(k, module, &[], &decl.tree),
            ItemKind::Mod { name, items } => {
                k.inline_modules.insert(name.clone());
                let mut m = module.to_vec();
                m.push(name.clone());
                collect_items(k, &m, items);
            }
            _ => {}
        }
    }
}

/// The key of the class or enum an impl block written in `module` is for (`impl Counter`,
/// `impl Display for a.Counter`), or the type's last name when it isn't an Oxie type.
fn impl_owner(k: &Crate, imp: &ImplDecl, module: &[String]) -> Option<String> {
    let name = imp.self_ty.last_name()?;
    let path: Vec<String> = match &imp.self_ty {
        Type::Path { segs, .. } => segs.iter().map(|s| s.name.clone()).collect(),
        _ => vec![name.to_string()],
    };
    Some(k.type_key_at(&path, module).unwrap_or_else(|| name.to_string()))
}

/// The names of the constants among a type's members.
fn member_consts(members: &[Item]) -> Vec<String> {
    members.iter().filter_map(|m| if let ItemKind::Const(c) = &m.kind { Some(c.name.clone()) } else { None }).collect()
}

fn collect_impls(k: &mut Crate, module: &[String], items: &[Item]) {
    for it in items {
        match &it.kind {
            ItemKind::Impl(imp) => {
                let Some(owner) = impl_owner(k, imp, module) else { continue };
                let trait_info = imp.trait_.as_ref().and_then(|t| t.last_name()).and_then(|n| k.traits.get(n)).cloned();
                // an implementation for every `T`, or for boxes or references, leaves no room for
                // the forwarding ones Oxie would add
                let blanket = match &imp.self_ty {
                    Type::Ref { .. } => true,
                    t => t.last_name().is_some_and(|n| n == "Box" || imp.generics.params.iter().any(|g| matches!(g, GenericParam::Type { name, .. } if name == n))),
                };
                if let (true, Some(t)) = (blanket, imp.trait_.as_ref().and_then(|t| t.last_name())) {
                    if let Some(ti) = k.traits.get_mut(t) {
                        ti.forward = None;
                    }
                }
                if let Some(t) = imp.trait_.as_ref().and_then(|t| t.last_name()) {
                    if let Some(c) = k.classes.get_mut(&owner) {
                        c.interfaces.push(t.to_string());
                    } else if let Some(e) = k.enums.get_mut(&owner) {
                        e.interfaces.push(t.to_string());
                    }
                }
                if imp.trait_.is_none() {
                    let consts = member_consts(&imp.members);
                    if let Some(c) = k.classes.get_mut(&owner) {
                        c.consts.extend(consts);
                    } else if let Some(e) = k.enums.get_mut(&owner) {
                        e.consts.extend(consts);
                    }
                }
                for m in &imp.members {
                    if let ItemKind::Fun(f) = &m.kind {
                        let trecv = trait_info.as_ref().and_then(|t| t.methods.get(&f.name)).map(|m| m.recv);
                        let mut mi = method_info_explicit(f, trecv).unwrap_or_else(|| default_mi(f));
                        mi.iface = imp.trait_.is_some();
                        if let Some(c) = k.classes.get_mut(&owner) {
                            // an interface's `new` (`impl Watcher for FsEventWatcher { fun new(..) }`)
                            // too: a generated constructor would hide it
                            if f.name == "new" && (imp.trait_.is_none() || mi.recv == Recv::Static) {
                                c.user_new = true;
                            }
                            add_method(&mut c.methods, &f.name, mi);
                        } else if let Some(e) = k.enums.get_mut(&owner) {
                            add_method(&mut e.methods, &f.name, mi);
                        }
                    }
                }
            }
            ItemKind::Mod { name, items } => {
                let mut m = module.to_vec();
                m.push(name.clone());
                collect_impls(k, &m, items);
            }
            _ => {}
        }
    }
}

pub fn ext_trait_name(module: &[String], fun: &str) -> String {
    let mut s = String::from("__OxieExt");
    for m in module {
        s.push('_');
        s.push_str(m);
    }
    s.push('_');
    s.push_str(fun);
    s
}

// -------------------------------------------------------------------------------------------
// Receiver inference

/// Information about a method body relevant to receiver inference.
#[derive(Default, Debug)]
pub struct BodyFacts {
    pub uses_self: bool,
    pub mutates_self: bool,
    /// The first use that changes `self`, and what it does ("it assigns `hits`").
    pub mut_cause: Option<(Span, String)>,
}

struct MethodRef<'a> {
    /// The type the method belongs to; the interface for its own methods (required and default).
    owner: String,
    fun: &'a FunDecl,
    trait_recv: Option<Recv>,
    /// Is or implements a method of this Oxie interface whose receiver is inferred (from its
    /// default body and its implementations).
    implements: Option<String>,
    /// The module declaring it, where the functions it calls are looked up.
    module: Vec<String>,
}

fn infer_receivers(k: &mut Crate, units: &[SourceUnit]) {
    // gather method bodies per owner type, and the Oxie interfaces each type implements or extends
    let mut methods: Vec<MethodRef> = Vec::new();
    let mut supers: HashMap<String, Vec<String>> = HashMap::new();
    fn gather<'a>(k: &Crate, module: &[String], items: &'a [Item], out: &mut Vec<MethodRef<'a>>, supers: &mut HashMap<String, Vec<String>>) {
        for it in items {
            match &it.kind {
                ItemKind::Type(t) => {
                    gather(k, module, &t.members, out, supers);
                    let owner = k.decl_key(module, &t.name);
                    let traits = t.supertypes.iter().filter_map(|st| st.last_name()).filter(|n| k.traits.contains_key(*n));
                    supers.entry(owner.clone()).or_default().extend(traits.map(String::from));
                    for m in &t.members {
                        if let ItemKind::Fun(f) = &m.kind {
                            // methods implementing an Oxie interface take its receiver
                            let mut trecv = None;
                            let mut implements = None;
                            for st in &t.supertypes {
                                if let Some(ti) = st.last_name().and_then(|n| k.traits.get(n)) {
                                    if let Some(mi) = ti.methods.get(&f.name) {
                                        if mi.explicit {
                                            trecv = Some(mi.recv);
                                        } else {
                                            implements = Some(ti.name.clone());
                                        }
                                    }
                                }
                            }
                            out.push(MethodRef { owner: owner.clone(), fun: f, trait_recv: trecv, implements, module: module.to_vec() });
                        }
                    }
                }
                ItemKind::Trait(t) => {
                    // default bodies are inferred like other methods (calling a `&mut self` method makes them `&mut self`)
                    let traits = t.supertraits.iter().filter_map(|st| st.last_name()).filter(|n| k.traits.contains_key(*n));
                    supers.entry(t.name.clone()).or_default().extend(traits.map(String::from));
                    for m in &t.members {
                        if let ItemKind::Fun(f) = &m.kind {
                            let implements = k.traits.get(&t.name).and_then(|ti| ti.methods.get(&f.name)).filter(|mi| !mi.explicit).map(|_| t.name.clone());
                            out.push(MethodRef { owner: t.name.clone(), fun: f, trait_recv: None, implements, module: module.to_vec() });
                        }
                    }
                }
                ItemKind::Impl(imp) => {
                    let Some(owner) = impl_owner(k, imp, module) else { continue };
                    let tinfo = imp.trait_.as_ref().and_then(|t| t.last_name()).and_then(|n| k.traits.get(n));
                    if let Some(ti) = tinfo {
                        supers.entry(owner.clone()).or_default().push(ti.name.clone());
                    }
                    for m in &imp.members {
                        if let ItemKind::Fun(f) = &m.kind {
                            let mi = tinfo.and_then(|t| t.methods.get(&f.name));
                            let trecv = mi.filter(|m| m.explicit).map(|m| m.recv);
                            let implements = mi.filter(|m| !m.explicit).and(tinfo.map(|t| t.name.clone()));
                            out.push(MethodRef { owner: owner.clone(), fun: f, trait_recv: trecv, implements, module: module.to_vec() });
                        }
                    }
                }
                ItemKind::Mod { name, items } => {
                    let mut m = module.to_vec();
                    m.push(name.clone());
                    gather(k, &m, items, out, supers);
                }
                _ => {}
            }
        }
    }
    for u in units {
        gather(k, &u.module, &u.file.items, &mut methods, &mut supers);
    }

    // initial: explicit receivers fixed, others start as Ref/Static
    let mut decided: HashMap<(String, String), Recv> = HashMap::new();
    let mut pending: Vec<usize> = Vec::new();
    for (i, m) in methods.iter().enumerate() {
        if let Some(mi) = method_info_explicit(m.fun, m.trait_recv) {
            decided.insert((m.owner.clone(), m.fun.name.clone()), mi.recv);
        } else {
            pending.push(i);
            decided.insert((m.owner.clone(), m.fun.name.clone()), Recv::Ref);
        }
    }
    // `&mut` parameters of crate functions and methods
    let mut mut_params: HashMap<String, Vec<usize>> = HashMap::new();
    let is_mut_ref = |t: &Type| matches!(t, Type::Ref { mutable: true, .. });
    let mut_idx = |params: &[ParamInfo]| -> Vec<usize> { params.iter().enumerate().filter(|(_, p)| is_mut_ref(&p.ty)).map(|(i, _)| i).collect() };
    // a function name several modules declare (keyed `a::fill`) is added per module below
    for (name, f) in k.funs.iter().filter(|(key, _)| !key.contains("::")) {
        let idx = mut_idx(&f.params);
        if !idx.is_empty() {
            mut_params.insert(name.clone(), idx);
        }
    }
    for m in &methods {
        let idx: Vec<usize> = m.fun.params.iter().enumerate().filter(|(_, p)| is_mut_ref(&p.ty)).map(|(i, _)| i).collect();
        if !idx.is_empty() {
            mut_params.entry(m.fun.name.clone()).or_insert(idx);
        }
    }
    let mut mut_params_in: HashMap<Vec<String>, HashMap<String, Vec<usize>>> = HashMap::new();
    // why an inferred receiver is `&mut self`, from the last round
    let mut whys: HashMap<(String, String), (String, Option<Span>)> = HashMap::new();
    // An interface method is `&mut self` if its default body or any implementation needs it; then all
    // implementations use it. Kept across rounds (interface, method) -> the type that needed it, so
    // that an implementation which changes nothing stays `&mut self` for the methods calling it.
    let mut trait_mut: HashMap<(String, String), String> = HashMap::new();
    let trait_why = |t: &str, name: &str, by: &str| {
        if by == t {
            format!("the default method `{}.{}` changes its object, so every implementation takes `&mut self`", t, name)
        } else {
            format!("the interface method `{}.{}` changes its object in `{}`, so every implementation takes `&mut self`", t, name, by)
        }
    };
    // Methods sharing an owner and name (`bump` in `impl Wrap<i32>` and in `impl Wrap<String>`) would
    // undo each other's receiver every round, so a receiver is never lowered: `&self` < static < `&mut self`.
    let rank = |r: Recv| match r {
        Recv::RefMut => 2,
        Recv::Static => 1,
        _ => 0,
    };
    // fixpoint: receivers only rise, so it ends, however long a chain of calls on `this` is
    loop {
        let mut changed = false;
        for &i in &pending {
            let m = &methods[i];
            let fields: HashSet<String> = k.classes.get(&m.owner).map(|c| c.fields.iter().map(|f| f.name.clone()).collect()).unwrap_or_default();
            let recvs = callable_recvs(&m.owner, &decided, &supers);
            // the methods callable on fields whose type is an Oxie class (`c.add(1)` for `var c: Counter`);
            // a shared class's are called through its handle, which `&self` reaches
            let field_recvs: HashMap<String, HashMap<String, Recv>> = k
                .classes
                .get(&m.owner)
                .into_iter()
                .flat_map(|c| c.fields.iter().map(move |f| (c, f)))
                .filter_map(|(c, f)| {
                    let Type::Path { segs, .. } = &f.ty else { return None };
                    let path: Vec<String> = segs.iter().map(|s| s.name.clone()).collect();
                    let key = k.type_key_at(&path, &c.module).filter(|key| k.classes.get(key).is_some_and(|c| !c.is_shared))?;
                    Some((f.name.clone(), callable_recvs(&key, &decided, &supers)))
                })
                .collect();
            // the methods callable on the elements of fields that are collections of Oxie classes
            // or interfaces (`for (c in counters) c.bump()`)
            let elem_recvs: HashMap<String, HashMap<String, Recv>> = k
                .classes
                .get(&m.owner)
                .into_iter()
                .flat_map(|c| c.fields.iter().map(move |f| (c, f)))
                .filter_map(|(c, f)| {
                    let Type::Path { segs, .. } = &f.ty else { return None };
                    let [seg] = segs.as_slice() else { return None };
                    if !matches!(seg.name.as_str(), "Vec" | "VecDeque" | "LinkedList") {
                        return None;
                    }
                    let Some([GenericArg::Type(Type::Path { segs, .. })]) = seg.generics.as_deref() else { return None };
                    let path: Vec<String> = segs.iter().map(|s| s.name.clone()).collect();
                    let key = match k.type_key_at(&path, &c.module).filter(|key| k.classes.get(key).is_some_and(|c| !c.is_shared)) {
                        Some(key) => key,
                        None if path.len() == 1 && k.traits.contains_key(&path[0]) => path[0].clone(),
                        None => return None,
                    };
                    Some((f.name.clone(), callable_recvs(&key, &decided, &supers)))
                })
                .collect();
            let mut_params = if k.same_named_funs.is_empty() {
                &mut_params
            } else {
                // the same-named functions this method's module sees
                mut_params_in.entry(m.module.clone()).or_insert_with(|| {
                    let mut here = mut_params.clone();
                    for name in &k.same_named_funs {
                        match k.fun_in(name, &m.module).map(|f| mut_idx(&f.params)) {
                            Some(idx) if !idx.is_empty() => here.insert(name.clone(), idx),
                            _ => here.remove(name),
                        };
                    }
                    here
                })
            };
            let facts = analyze_fun_with(m.fun, &fields, &recvs, mut_params, &field_recvs, &elem_recvs);
            let name = m.owner.rsplit("::").next().unwrap_or(&m.owner);
            let returns_self = m.fun.ret.as_ref().is_some_and(|t| t.single_name() == Some("Self") || t.single_name() == Some(name));
            // implementations of an interface method that is `&mut self` stay `&mut self`
            let by = m.implements.as_ref().and_then(|t| trait_mut.get(&(t.clone(), m.fun.name.clone())).map(|by| (t, by)));
            let recv = if facts.mutates_self || by.is_some() {
                Recv::RefMut
            } else if !facts.uses_self && returns_self && m.trait_recv.is_none() && m.implements.is_none() {
                Recv::Static
            } else {
                Recv::Ref
            };
            let key = (m.owner.clone(), m.fun.name.clone());
            let recv = if rank(decided[&key]) > rank(recv) { decided[&key] } else { recv };
            match (facts.mut_cause, by) {
                (Some((at, why)), _) => whys.insert(key.clone(), (why, Some(at))),
                (None, Some((t, by))) => whys.insert(key.clone(), (trait_why(t, &m.fun.name, by), None)),
                // kept `&mut self` from a method with the same owner and name, and its reason
                (None, None) if recv == Recv::RefMut => None,
                (None, None) => whys.remove(&key),
            };
            if decided.get(&key) != Some(&recv) {
                decided.insert(key, recv);
                changed = true;
            }
        }
        // the first `&mut self` implementation (or default body) of an interface method spreads to the
        // others; not a shared class's, whose handle changes the data behind `&self`
        for m in &methods {
            if let Some(t) = &m.implements {
                if decided[&(m.owner.clone(), m.fun.name.clone())] == Recv::RefMut && !k.classes.get(&m.owner).is_some_and(|c| c.is_shared) {
                    trait_mut.entry((t.clone(), m.fun.name.clone())).or_insert_with(|| m.owner.clone());
                }
            }
        }
        for m in &methods {
            if let Some(t) = &m.implements {
                let Some(by) = trait_mut.get(&(t.clone(), m.fun.name.clone())) else { continue };
                let key = (m.owner.clone(), m.fun.name.clone());
                if decided.get(&key) != Some(&Recv::RefMut) {
                    whys.insert(key.clone(), (trait_why(t, &m.fun.name, by), None));
                    decided.insert(key, Recv::RefMut);
                    changed = true;
                }
            }
        }
        if !changed {
            break;
        }
    }
    for ((owner, name), recv) in decided {
        let why = (recv == Recv::RefMut).then(|| whys.remove(&(owner.clone(), name.clone()))).flatten();
        let mi = if let Some(c) = k.classes.get_mut(&owner) {
            c.methods.get_mut(&name)
        } else if let Some(e) = k.enums.get_mut(&owner) {
            e.methods.get_mut(&name)
        } else {
            k.traits.get_mut(&owner).and_then(|t| t.methods.get_mut(&name))
        };
        if let Some(mi) = mi {
            mi.recv = recv;
            mi.recv_why = why;
        }
    }
}

/// The receivers of the methods `owner` can call on `this`: its own, then those of the interfaces
/// it implements or extends (default methods are callable without `this.`).
fn callable_recvs(owner: &str, decided: &HashMap<(String, String), Recv>, supers: &HashMap<String, Vec<String>>) -> HashMap<String, Recv> {
    let mut recvs = HashMap::new();
    let mut todo = vec![owner];
    let mut seen = HashSet::new();
    while let Some(o) = todo.pop() {
        if seen.insert(o) {
            for ((_, n), r) in decided.iter().filter(|((x, _), _)| x == o) {
                recvs.entry(n.clone()).or_insert(*r);
            }
            todo.extend(supers.get(o).into_iter().flatten().map(|s| s.as_str()));
        }
    }
    recvs
}

pub fn analyze_fun(f: &FunDecl, fields: &HashSet<String>, methods: &HashMap<String, Recv>) -> BodyFacts {
    analyze_fun_with(f, fields, methods, &HashMap::new(), &HashMap::new(), &HashMap::new())
}

/// Like `analyze_fun`, also knowing which parameters of callable functions are `&mut`
/// (passing a field there borrows it mutably: `fill(items)` becomes `fill(&mut self.items)`)
/// and the receivers of methods on fields of Oxie class types and on the elements of fields that
/// hold them.
pub fn analyze_fun_with(
    f: &FunDecl,
    fields: &HashSet<String>,
    methods: &HashMap<String, Recv>,
    mut_params: &HashMap<String, Vec<usize>>,
    field_recvs: &HashMap<String, HashMap<String, Recv>>,
    elem_recvs: &HashMap<String, HashMap<String, Recv>>,
) -> BodyFacts {
    let mut w = Walker { fields, methods, mut_params, field_recvs, elem_recvs, scopes: vec![HashSet::new()], facts: BodyFacts::default() };
    for p in &f.params {
        w.bind(&p.pat);
    }
    match &f.body {
        Some(FunBody::Block(b)) => w.block(b),
        Some(FunBody::Expr(e)) => w.expr(e),
        None => {}
    }
    w.facts
}

pub fn analyze_block(b: &Block, fields: &HashSet<String>, methods: &HashMap<String, Recv>) -> BodyFacts {
    let none = HashMap::new();
    let mut w = Walker { fields, methods, mut_params: &none, field_recvs: &HashMap::new(), elem_recvs: &HashMap::new(), scopes: vec![HashSet::new()], facts: BodyFacts::default() };
    w.block(b);
    w.facts
}

struct Walker<'a> {
    fields: &'a HashSet<String>,
    methods: &'a HashMap<String, Recv>,
    /// Function and method name -> indices of its `&mut` parameters.
    mut_params: &'a HashMap<String, Vec<usize>>,
    /// Field name -> the receivers of the methods of its Oxie class.
    field_recvs: &'a HashMap<String, HashMap<String, Recv>>,
    /// Field name -> the receivers of the methods of the Oxie class or interface of its elements.
    elem_recvs: &'a HashMap<String, HashMap<String, Recv>>,
    scopes: Vec<HashSet<String>>,
    facts: BodyFacts,
}

impl<'a> Walker<'a> {
    fn is_local(&self, n: &str) -> bool {
        self.scopes.iter().any(|s| s.contains(n))
    }
    fn bind(&mut self, p: &Pat) {
        let mut names = Vec::new();
        p.bindings(&mut names);
        let top = self.scopes.last_mut().unwrap();
        for n in names {
            top.insert(n);
        }
    }
    fn push(&mut self) {
        self.scopes.push(HashSet::new());
    }
    fn pop(&mut self) {
        self.scopes.pop();
    }

    /// The body changes `self` at `at`; the first such use is the reason for `&mut self`.
    fn mutates(&mut self, at: Span, why: String) {
        self.facts.mutates_self = true;
        if self.facts.mut_cause.is_none() {
            self.facts.mut_cause = Some((at, why));
        }
    }

    /// A field passed to a `&mut` parameter is borrowed mutably.
    fn mut_args(&mut self, fun: &str, args: &[Arg]) {
        let Some(idx) = self.mut_params.get(fun) else { return };
        for (i, a) in args.iter().enumerate() {
            if a.name.is_none() && idx.contains(&i) && self.rooted_in_self(&a.value) {
                let why = format!("it passes `{}` to `{}`, which takes `&mut`", self.self_field(&a.value), fun);
                self.mutates(a.value.span, why);
            }
        }
    }

    /// The field of `self` a place starts from (`hits` for `hits[0]` or `self.hits.len`), or
    /// `this` for `self` itself.
    fn self_field(&self, e: &Expr) -> String {
        let mut cur = e;
        let mut field = None;
        loop {
            match &cur.kind {
                ExprKind::Field { recv, name, .. } => {
                    field = Some(name.clone());
                    cur = recv;
                }
                ExprKind::Index(r, _) | ExprKind::MethodCall { recv: r, .. } | ExprKind::Unary(UnOp::Deref, r) | ExprKind::Paren(r) => cur = r,
                ExprKind::Path { .. } => return cur.ident().unwrap_or("this").to_string(),
                _ => return field.unwrap_or_else(|| "this".into()),
            }
        }
    }

    /// The field `c` or `this.c` names, if it isn't shadowed by a local.
    fn direct_field<'e>(&self, e: &'e Expr) -> Option<&'e str> {
        match &e.kind {
            ExprKind::Path { .. } => e.ident().filter(|n| !self.is_local(n) && self.fields.contains(*n)),
            ExprKind::Field { recv, name, .. } if matches!(recv.kind, ExprKind::SelfValue) => Some(name),
            ExprKind::Paren(x) => self.direct_field(x),
            _ => None,
        }
    }

    /// Is the root of this place expression `self` or an implicit field?
    fn rooted_in_self(&self, e: &Expr) -> bool {
        let mut cur = e;
        loop {
            match &cur.kind {
                ExprKind::Field { recv, .. } => cur = recv,
                ExprKind::Index(r, _) => cur = r,
                // `chars().next()` advances a new iterator, not the field
                ExprKind::MethodCall { name, .. } if NEW_VALUE_METHODS.contains(&name.as_str()) => return false,
                ExprKind::MethodCall { recv, .. } => cur = recv,
                ExprKind::Unary(UnOp::Deref, r) => cur = r,
                ExprKind::Paren(r) => cur = r,
                ExprKind::SelfValue => return true,
                ExprKind::Path { .. } => {
                    return cur.ident().is_some_and(|n| !self.is_local(n) && self.fields.contains(n));
                }
                _ => return false,
            }
        }
    }

    fn block(&mut self, b: &Block) {
        self.push();
        for s in &b.stmts {
            self.stmt(s);
        }
        self.pop();
    }

    fn stmt(&mut self, s: &Stmt) {
        match s {
            Stmt::Let { pat, init, else_, .. } => {
                if let Some(e) = init {
                    self.expr(e);
                }
                if let Some(b) = else_ {
                    self.block(b);
                }
                self.bind(pat);
            }
            Stmt::Expr { expr, .. } => self.expr(expr),
            Stmt::Attributed { stmt, .. } => self.stmt(stmt),
            Stmt::Item(_) => {}
        }
    }

    fn cond(&mut self, c: &Cond) {
        match c {
            Cond::Expr(e) => self.expr(e),
            Cond::Let { pat, expr, .. } => {
                self.expr(expr);
                self.bind(pat);
            }
            Cond::Chain(parts) => parts.iter().for_each(|c| self.cond(c)),
        }
    }

    fn expr(&mut self, e: &Expr) {
        use ExprKind::*;
        match &e.kind {
            Path { .. } => {
                if let Some(n) = e.ident() {
                    if !self.is_local(n) && self.fields.contains(n) {
                        self.facts.uses_self = true;
                    }
                }
            }
            SelfValue => self.facts.uses_self = true,
            Template(parts) => {
                for p in parts {
                    if let TemplatePart::Expr(x, _) = p {
                        self.expr(x);
                    }
                }
            }
            Unary(op, x) => {
                if *op == UnOp::RefMut && self.rooted_in_self(x) {
                    let why = format!("it takes `&mut {}`", self.self_field(x));
                    self.mutates(x.span, why);
                }
                self.expr(x);
            }
            Binary(_, a, b) | Elvis(a, b) | Pair(a, b) | Index(a, b) | ArrayRepeat(a, b) => {
                self.expr(a);
                self.expr(b);
            }
            In { item, coll, .. } => {
                self.expr(item);
                self.expr(coll);
            }
            Is { expr, .. } => self.expr(expr),
            Assign(op, t, v) => {
                if self.rooted_in_self(t) {
                    let why = format!("it {} `{}`", if op.is_some() { "changes" } else { "assigns" }, self.self_field(t));
                    self.mutates(t.span, why);
                }
                self.expr(t);
                self.expr(v);
            }
            Call { callee, args } => {
                if let Some(n) = callee.ident() {
                    if !self.is_local(n) {
                        if let Some(r) = self.methods.get(n).copied() {
                            if r != Recv::Static {
                                self.facts.uses_self = true;
                            }
                            if r == Recv::RefMut {
                                self.mutates(e.span, format!("it calls `{}()`, which changes `this`", n));
                            }
                        }
                    }
                }
                if let Some(n) = callee.ident().filter(|n| !self.is_local(n)) {
                    self.mut_args(n, args);
                }
                self.expr(callee);
                for a in args {
                    self.expr(&a.value);
                }
            }
            MethodCall { recv, name, args, .. } => {
                if matches!(recv.kind, SelfValue) {
                    if let Some(Recv::RefMut) = self.methods.get(name) {
                        self.mutates(e.span, format!("it calls `{}()`, which changes `this`", name));
                    }
                    self.mut_args(name, args);
                } else if let Some(field) = self.direct_field(recv).filter(|f| self.field_recvs.get(*f).and_then(|r| r.get(name)) == Some(&Recv::RefMut)) {
                    let why = format!("it calls `{}.{}()`, which changes `{}`", field, name, field);
                    self.mutates(e.span, why);
                } else if MUTATING_METHODS.contains(&name.as_str()) && self.rooted_in_self(recv) {
                    let why = format!("it calls `{}.{}()`", self.self_field(recv), name);
                    self.mutates(e.span, why);
                }
                self.expr(recv);
                for a in args {
                    self.expr(&a.value);
                }
            }
            Field { recv, .. } | Try(recv) | Unwrap(recv) | Await(recv) | Cast(recv, _) | Paren(recv) | Throw(recv) => self.expr(recv),
            If { cond, then, else_ } => {
                self.push();
                self.cond(cond);
                self.block(then);
                self.pop();
                if let Some(e) = else_ {
                    self.expr(e);
                }
            }
            When { subject, binding, arms, .. } => {
                if let Some(s) = subject {
                    self.expr(s);
                }
                if let Some(b) = binding {
                    self.bind(b);
                }
                for a in arms {
                    self.push();
                    for c in &a.conds {
                        match c {
                            WhenCond::Pat(p) => self.bind(p),
                            WhenCond::In { expr, .. } | WhenCond::Expr(expr) => self.expr(expr),
                            WhenCond::Else => {}
                        }
                    }
                    if let Some(g) = &a.guard {
                        self.expr(g);
                    }
                    self.expr(&a.body);
                    self.pop();
                }
            }
            Block(b) | Unsafe(b) | ConstBlock(b) | LabeledBlock { block: b, .. } | Loop { body: b, .. } => self.block(b),
            Async { block, .. } => self.block(block),
            While { cond, body, .. } => {
                self.push();
                self.cond(cond);
                self.block(body);
                self.pop();
            }
            DoWhile { body, cond, .. } => {
                self.block(body);
                self.expr(cond);
            }
            For { pat, iter, body, .. } => {
                // `for (x in items) x.qty += 1` over a field borrows it mutably (see `lower_expr`)
                let field = matches!(&iter.kind, Path { .. }) || matches!(&iter.kind, Field { recv, .. } if matches!(recv.kind, SelfValue));
                if field && self.rooted_in_self(iter) {
                    let elems = match &iter.kind {
                        Field { name, .. } => Some(name.as_str()),
                        _ => iter.ident(),
                    }
                    .and_then(|f| self.elem_recvs.get(f));
                    let changes = |m: &str| elems.and_then(|r| r.get(m)) == Some(&Recv::RefMut);
                    if let Pat::Ident { name, by_ref: false, mutable: false, sub: None, .. } = pat {
                        if crate::visit::writes_through(body, name, &changes) {
                            let why = format!("it changes the elements of `{}`", self.self_field(iter));
                            self.mutates(iter.span, why);
                        }
                    }
                }
                self.expr(iter);
                self.push();
                self.bind(pat);
                self.block(body);
                self.pop();
            }
            Lambda { params, body, .. } => {
                self.push();
                for p in params {
                    self.bind(&p.pat);
                }
                self.scopes.last_mut().unwrap().insert("it".into());
                self.expr(body);
                self.pop();
            }
            Return(Some(x)) => self.expr(x),
            Break { value: Some(x), .. } => self.expr(x),
            Tuple(v) | Array(v) | VecLit(v) | MapLit(v) | SetLit(v) => {
                for x in v {
                    self.expr(x);
                }
            }
            Range { start, end, .. } => {
                if let Some(s) = start {
                    self.expr(s);
                }
                if let Some(s) = end {
                    self.expr(s);
                }
            }
            StructLit { fields, base, .. } => {
                for (n, v) in fields {
                    match v {
                        Some(v) => self.expr(v),
                        None => {
                            if !self.is_local(n) && self.fields.contains(n) {
                                self.facts.uses_self = true;
                            }
                        }
                    }
                }
                if let Some(b) = base {
                    self.expr(b);
                }
            }
            Macro { text, .. } | Raw(text) => {
                if contains_word(text, "self") {
                    self.facts.uses_self = true;
                }
            }
            TryCatch { body, catch } => {
                self.block(body);
                if let Some(c) = catch {
                    self.push();
                    self.bind(&c.binding);
                    self.block(&c.body);
                    self.pop();
                }
            }
            _ => {}
        }
    }
}

pub fn contains_word(text: &str, word: &str) -> bool {
    let bytes = text.as_bytes();
    let mut i = 0;
    while let Some(pos) = text[i..].find(word) {
        let s = i + pos;
        let e = s + word.len();
        let before_ok = s == 0 || !(bytes[s - 1].is_ascii_alphanumeric() || bytes[s - 1] == b'_');
        let after_ok = e >= bytes.len() || !(bytes[e].is_ascii_alphanumeric() || bytes[e] == b'_');
        if before_ok && after_ok {
            return true;
        }
        i = e;
    }
    false
}

// -------------------------------------------------------------------------------------------
// Type facts used for automatic derives

const INTS: &[&str] = &["i8", "i16", "i32", "i64", "i128", "isize", "u8", "u16", "u32", "u64", "u128", "usize", "Int", "Long", "Short", "Byte", "UInt", "ULong", "UByte", "UShort"];

/// Copy-able without knowing user types.
pub fn is_copy_simple(t: &Type, copy_types: &HashSet<String>) -> bool {
    match t {
        Type::Path { segs, .. } if segs.len() == 1 => {
            let n = segs[0].name.as_str();
            if segs[0].generics.is_none() && (INTS.contains(&n) || matches!(n, "f32" | "f64" | "bool" | "char" | "Double" | "Float" | "Boolean" | "Char")) {
                return true;
            }
            if n == "Option" {
                if let Some(g) = &segs[0].generics {
                    return g.iter().all(|a| matches!(a, GenericArg::Type(t) if is_copy_simple(t, copy_types)));
                }
            }
            no_type_args(&segs[0]) && copy_types.contains(n)
        }
        Type::Option(inner) => is_copy_simple(inner, copy_types),
        Type::Tuple(v) => v.iter().all(|t| is_copy_simple(t, copy_types)),
        Type::Array(inner, _) => is_copy_simple(inner, copy_types),
        Type::Ref { mutable: false, .. } => true,
        _ => false,
    }
}

/// Eq + Hash-able without knowing user types.
pub fn is_hash_simple(t: &Type, hash_types: &HashSet<String>) -> bool {
    match t {
        Type::Path { segs, .. } if segs.len() == 1 => {
            let n = segs[0].name.as_str();
            let args_ok = |seg: &PathSeg| seg.generics.as_ref().is_none_or(|g| g.iter().all(|a| matches!(a, GenericArg::Type(t) if is_hash_simple(t, hash_types))));
            if INTS.contains(&n) || matches!(n, "bool" | "char" | "String" | "Boolean" | "Char") {
                return segs[0].generics.is_none();
            }
            if matches!(n, "Vec" | "Option" | "Box" | "BTreeMap" | "BTreeSet" | "Rc" | "Arc") {
                return args_ok(&segs[0]);
            }
            no_type_args(&segs[0]) && hash_types.contains(n)
        }
        Type::Option(inner) => is_hash_simple(inner, hash_types),
        Type::Tuple(v) => v.iter().all(|t| is_hash_simple(t, hash_types)),
        Type::Array(inner, _) | Type::Slice(inner) => is_hash_simple(inner, hash_types),
        Type::Ref { mutable: false, inner, .. } => matches!(inner.single_name(), Some("str")) || is_hash_simple(inner, hash_types),
        _ => false,
    }
}

pub fn is_type_name_like(s: &str) -> bool {
    is_type_like(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The receiver of each method of `owner` (a class, enum or interface), with the reason and the
    /// source it points at.
    fn receivers(src: &str, owner: &str) -> HashMap<String, (Recv, Option<(String, Option<String>)>)> {
        let (file, diags) = crate::parser::parse_file(src);
        assert!(diags.is_empty(), "{:?}", diags);
        let k = collect(&[SourceUnit { module: Vec::new(), file: &file }], "main", HashSet::new(), false);
        let text = |s: Span| src[s.start as usize..s.end as usize].to_string();
        let methods = k
            .classes
            .get(owner)
            .map(|c| c.methods.clone())
            .or_else(|| k.enums.get(owner).map(|e| e.methods.clone()))
            .or_else(|| k.traits.get(owner).map(|t| t.methods.clone()))
            .unwrap();
        methods.into_iter().map(|(n, m)| (n, (m.recv, m.recv_why.map(|(w, s)| (w, s.map(text)))))).collect()
    }

    #[test]
    fn mut_self_says_why() {
        let src = r#"
fun fill(v: &mut Vec<i32>) { v.push(1) }
class Counter(var n: i32 = 0, var hits: Vec<i32> = vec[]) {
    fun reset() { n = 0 }
    fun bump() { n += 1 }
    fun hit() { hits.push(1) }
    fun refill() { fill(hits) }
    fun again() { reset() }
    fun peek(): i32 = n
}
"#;
        let r = receivers(src, "Counter");
        assert_eq!(r["reset"], (Recv::RefMut, Some(("it assigns `n`".into(), Some("n".into())))));
        assert_eq!(r["bump"].1, Some(("it changes `n`".into(), Some("n".into()))));
        assert_eq!(r["hit"].1, Some(("it calls `hits.push()`".into(), Some("hits.push(1)".into()))));
        assert_eq!(r["refill"].1, Some(("it passes `hits` to `fill`, which takes `&mut`".into(), Some("hits".into()))));
        assert_eq!(r["again"].1, Some(("it calls `reset()`, which changes `this`".into(), Some("reset()".into()))));
        assert_eq!(r["peek"], (Recv::Ref, None));
    }

    #[test]
    fn interface_methods_say_which_implementation_changes_its_object() {
        let src = r#"
interface Shape { fun grow() }
class Circle(var r: i32) : Shape { override fun grow() { r += 1 } }
class Dot() : Shape { override fun grow() {} }
"#;
        assert_eq!(receivers(src, "Circle")["grow"].1, Some(("it changes `r`".into(), Some("r".into()))));
        let (recv, why) = receivers(src, "Dot")["grow"].clone();
        assert_eq!(recv, Recv::RefMut);
        assert_eq!(why, Some(("the interface method `Shape.grow` changes its object in `Circle`, so every implementation takes `&mut self`".into(), None)));
    }

    #[test]
    fn calling_an_interface_method_that_changes_its_object_changes_this() {
        // `Dot.grow` and `Point.grow` change nothing, but are `&mut self` like `Circle.grow`
        let src = r#"
interface Shape {
    fun grow()
    fun grow_twice() { grow(); grow() }
}
class Dot() : Shape {
    override fun grow() {}
    fun twice() { grow() }
    fun thrice() { twice(); this.grow() }
}
class Circle(var r: i32) : Shape { override fun grow() { r += 1 } }
class Point()
impl Shape for Point { fun grow() {} }
impl Point { fun twice() { this.grow() } }
"#;
        let calls = |name: &str, call: &str| Some((format!("it calls `{}()`, which changes `this`", name), Some(call.to_string())));
        let dot = receivers(src, "Dot");
        assert_eq!(dot["grow"].1, Some(("the interface method `Shape.grow` changes its object in `Circle`, so every implementation takes `&mut self`".into(), None)));
        assert_eq!(dot["twice"], (Recv::RefMut, calls("grow", "grow()")));
        assert_eq!(dot["thrice"], (Recv::RefMut, calls("twice", "twice()")));
        assert_eq!(receivers(src, "Point")["twice"], (Recv::RefMut, calls("grow", "this.grow()")));
        let shape = receivers(src, "Shape");
        assert_eq!(shape["grow"].0, Recv::RefMut);
        assert_eq!(shape["grow_twice"], (Recv::RefMut, calls("grow", "grow()")));
    }

    #[test]
    fn default_methods_take_the_receiver_of_the_methods_they_call() {
        let src = r#"
interface Counter {
    mut fun bump()
    fun bump_twice() { bump(); bump() }
    fun peek() {}
}
class Clicks(var n: i32) : Counter {
    override fun bump() { n += 1 }
    fun again() { bump_twice() }
}
class Quiet() : Counter {
    override fun bump() {}
    override fun bump_twice() {}
}
"#;
        let counter = receivers(src, "Counter");
        assert_eq!(counter["bump_twice"], (Recv::RefMut, Some(("it calls `bump()`, which changes `this`".into(), Some("bump()".into())))));
        assert_eq!(counter["peek"], (Recv::Ref, None));
        // calling the inherited default method
        assert_eq!(receivers(src, "Clicks")["again"], (Recv::RefMut, Some(("it calls `bump_twice()`, which changes `this`".into(), Some("bump_twice()".into())))));
        // overriding it
        let why = "the default method `Counter.bump_twice` changes its object, so every implementation takes `&mut self`";
        assert_eq!(receivers(src, "Quiet")["bump_twice"], (Recv::RefMut, Some((why.into(), None))));
    }

    #[test]
    fn long_call_chains_reach_the_first_caller() {
        // callers come first, so each round reaches one more of them (inference used to stop after 8)
        let src = r#"
class Chain(var n: i32) {
    fun a() { b() }
    fun b() { c() }
    fun c() { d() }
    fun d() { e() }
    fun e() { f() }
    fun f() { g() }
    fun g() { h() }
    fun h() { i() }
    fun i() { n += 1 }
}
"#;
        let r = receivers(src, "Chain");
        assert!(r.values().all(|(recv, _)| *recv == Recv::RefMut), "{:?}", r);
        assert_eq!(r["a"].1, Some(("it calls `b()`, which changes `this`".into(), Some("b()".into()))));
    }

    #[test]
    fn methods_sharing_an_owner_and_name_dont_undo_each_other() {
        // one `bump` (and one `make`) would undo the other's receiver every round, forever
        let src = r#"
class Wrap<T>(var n: i32, val t: T)
impl Wrap<i32> {
    fun bump() { n += 1 }
    fun make(): Self = Wrap(0, 0)
}
impl Wrap<String> {
    fun bump() {}
    fun make(): Self = Wrap(n, t.clone())
}
"#;
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || tx.send(receivers(src, "Wrap")).unwrap());
        let r = rx.recv_timeout(std::time::Duration::from_secs(10)).expect("receiver inference doesn't end");
        assert_eq!(r["bump"], (Recv::RefMut, Some(("it changes `n`".into(), Some("n".into())))));
    }

    #[test]
    fn same_named_classes_keep_their_own_receivers() {
        let src = r#"
mod a {
    class Counter(var n: i32) {
        fun bump() { n += 1 }
    }
}
mod b {
    class Counter(var log: Vec<String>) {
        fun bump() {}
    }
    impl Counter {
        fun reset() { log.clear() }
    }
}
"#;
        assert_eq!(receivers(src, "a::Counter")["bump"].0, Recv::RefMut);
        let b = receivers(src, "b::Counter");
        assert_eq!(b["bump"], (Recv::Ref, None));
        assert_eq!(b["reset"].0, Recv::RefMut);
    }

    #[test]
    fn a_name_several_modules_declare_means_the_one_in_scope() {
        let src = r#"
class Point(x: i32)
mod a {
    data class Point(x: i32, y: i32)
    enum Shape { Dot }
}
mod b {
    data class Point(label: String)
    enum Shape { Dot }
}
mod c {
    import super::b::Point
    import super::a::*
}
mod d {
    class Cache()
}
"#;
        let (file, diags) = crate::parser::parse_file(src);
        assert!(diags.is_empty(), "{:?}", diags);
        let k = collect(&[SourceUnit { module: Vec::new(), file: &file }], "main", HashSet::new(), false);
        let path = |s: &str| s.split("::").filter(|x| !x.is_empty()).map(String::from).collect::<Vec<_>>();
        let point = |module: &str| k.class_in("Point", &path(module)).map(|c| c.module.clone());
        assert_eq!(point(""), Some(path("")));
        assert_eq!(point("a"), Some(path("a")));
        assert_eq!(point("c"), Some(path("b")), "imported by name");
        assert_eq!(k.enum_in("Shape", &path("c")).map(|e| e.module.clone()), Some(path("a")), "imported with `*`");
        assert_eq!(k.class_at(&path("a::Point"), &[]).map(|c| c.fields.len()), Some(2));
        assert_eq!(k.class_at(&path("super::b::Point"), &path("c")).map(|c| c.fields[0].name.clone()), Some("label".into()));
        // a path names a class only through the module that declares it
        assert_eq!(k.type_key_at(&path("d::Cache"), &[]), Some("Cache".into()));
        assert_eq!(k.type_key_at(&path("std::Cache"), &[]), None);
        // derives are decided per declaration
        assert!(k.derives["a::Point"].iter().any(|d| d == "Copy"));
        assert!(!k.derives["b::Point"].iter().any(|d| d == "Copy"));
    }

    #[test]
    fn same_named_functions_resolve_by_module() {
        let src = r#"
fun max(a: i32, b: i32): i32 = a
mod a {
    fun fill(xs: &mut Vec<i32>) { xs.push(0) }
    class Bag(var items: Vec<i32>) {
        fun refill() { fill(items) }
    }
}
mod b {
    fun fill(xs: &Vec<i32>): usize = xs.len()
    class Bag(var items: Vec<i32>) {
        fun count(): usize = fill(items)
    }
}
mod c {
    import super::b::fill
}
"#;
        let (file, diags) = crate::parser::parse_file(src);
        assert!(diags.is_empty(), "{:?}", diags);
        let k = collect(&[SourceUnit { module: Vec::new(), file: &file }], "main", HashSet::new(), false);
        let path = |s: &str| s.split("::").filter(|x| !x.is_empty()).map(String::from).collect::<Vec<_>>();
        let fills_mut = |module: &str| k.fun_in("fill", &path(module)).map(|f| matches!(f.params[0].ty, Type::Ref { mutable: true, .. }));
        assert_eq!(fills_mut("a"), Some(true));
        assert_eq!(fills_mut("b"), Some(false));
        assert_eq!(fills_mut("c"), Some(false), "imported");
        assert_eq!(fills_mut(""), None);
        assert!(k.fun_at(&path("a::fill"), &[]).is_some());
        // a path names a function only through the module that declares it
        assert!(k.fun_at(&path("max"), &[]).is_some());
        assert!(k.fun_at(&path("std::cmp::max"), &[]).is_none());
        // each method passes `items` to its own module's `fill`
        assert_eq!(k.classes["a::Bag"].methods["refill"].recv, Recv::RefMut);
        assert_eq!(k.classes["b::Bag"].methods["count"].recv, Recv::Ref);
    }
}
