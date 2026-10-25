//! Syntax tree for Oxie source files.

use crate::diag::Span;
use crate::lexer::StrPart;

#[derive(Clone, Debug)]
pub struct File {
    pub inner_docs: Vec<String>,
    /// `#![...]` attribute contents (crate- or module-level).
    pub inner_attrs: Vec<String>,
    pub items: Vec<Item>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Vis {
    /// No modifier: public (Kotlin-style default).
    Default,
    Public,
    Private,
    /// `internal` -> `pub(crate)`
    Internal,
    /// Raw Rust restricted visibility like `pub(super)`.
    Raw(String),
}

#[derive(Clone, Debug)]
pub enum Attr {
    /// `@name(args)` - stored as Rust attribute content `name(args)`.
    Oxie { name: String, args: Option<String>, span: Span },
    /// `#[...]` raw Rust attribute content.
    Raw(String),
}

impl Attr {
    pub fn name(&self) -> &str {
        match self {
            Attr::Oxie { name, .. } => name,
            Attr::Raw(s) => s.split(['(', ' ', '=']).next().unwrap_or(""),
        }
    }
}

#[derive(Clone, Debug)]
pub struct Item {
    pub docs: Vec<String>,
    pub attrs: Vec<Attr>,
    pub vis: Vis,
    pub kind: ItemKind,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub enum ItemKind {
    Fun(FunDecl),
    Type(TypeDecl),
    Trait(TraitDecl),
    Impl(ImplDecl),
    Import(ImportDecl),
    Const(ConstDecl),
    Static(ConstDecl),
    TypeAlias { name: String, generics: Generics, ty: Type },
    Mod { name: String, items: Vec<Item> },
    Object { name: String, items: Vec<Item> },
    Test(TestDecl),
    Raw(String),
    /// Item-position macro call like `macro_rules! x { ... }` or `thread_local! { ... }`.
    Macro(String),
}

#[derive(Clone, Debug, Default)]
pub struct Generics {
    pub params: Vec<GenericParam>,
    pub where_clause: Vec<WherePred>,
}

impl Generics {
    pub fn is_empty(&self) -> bool {
        self.params.is_empty()
    }
}

#[derive(Clone, Debug)]
pub enum GenericParam {
    Lifetime { name: String, bounds: Vec<String> },
    Type { name: String, bounds: Vec<Type>, default: Option<Type> },
    Const { name: String, ty: Type, default: Option<String> },
}

#[derive(Clone, Debug)]
pub enum WherePred {
    Type { ty: Type, bounds: Vec<Type> },
    Lifetime { name: String, bounds: Vec<String> },
}

#[derive(Clone, Debug, Default)]
pub struct FunModifiers {
    pub is_async: bool,
    pub is_unsafe: bool,
    pub is_const: bool,
    pub is_static: bool,
    pub is_mut: bool,
    pub is_override: bool,
    pub is_operator: bool,
    pub abi: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum SelfParam {
    /// `self`
    Value { mutable: bool },
    /// `&self`
    Ref,
    /// `&mut self`
    RefMut,
    /// `self: Type`
    Typed(Type),
}

#[derive(Clone, Debug)]
pub struct Param {
    pub pat: Pat,
    pub ty: Type,
    pub default: Option<Expr>,
    /// A context parameter (`context(cx: &mut App)`): its default is the caller's own `cx`.
    pub context: bool,
    /// Declared as a value that the body only reads, so Oxie lends it (`params.rs`): its type is
    /// now the reference, and every argument for it is lent.
    pub lent: bool,
    pub span: Span,
}

impl Param {
    pub fn name(&self) -> Option<&str> {
        match &self.pat {
            Pat::Ident { name, .. } => Some(name),
            _ => None,
        }
    }
}

#[derive(Clone, Debug)]
pub enum Throws {
    No,
    /// `throws` without a type: the dynamic `AnyError`.
    Any,
    Ty(Type),
}

#[derive(Clone, Debug)]
pub enum FunBody {
    Block(Block),
    Expr(Expr),
}

#[derive(Clone, Debug)]
pub struct FunDecl {
    pub name: String,
    pub name_span: Span,
    pub mods: FunModifiers,
    pub generics: Generics,
    /// Extension receiver: `fun String.shout()`.
    pub receiver: Option<Type>,
    pub self_param: Option<SelfParam>,
    pub params: Vec<Param>,
    /// `context(cx: &mut App) fun ...`: parameters that calls fill with their own variable of the
    /// name (`contexts.rs` appends them to `params`).
    pub contexts: Vec<Param>,
    pub ret: Option<Type>,
    pub throws: Throws,
    pub body: Option<FunBody>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum TypeKind {
    /// `class` / `data class` / `struct` with named fields.
    Class,
    Data,
    /// Rust tuple struct: `struct Meters(f64)`.
    TupleStruct,
    Enum,
    /// Rust `union`
    Union,
}

#[derive(Clone, Debug)]
pub struct Field {
    pub docs: Vec<String>,
    pub attrs: Vec<Attr>,
    pub vis: Vis,
    pub name: String,
    pub ty: Type,
    pub default: Option<Expr>,
    /// Declared in the primary constructor (vs. a body property).
    pub in_ctor: bool,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub enum VariantFields {
    Unit,
    Tuple(Vec<Type>),
    Named(Vec<Field>),
}

#[derive(Clone, Debug)]
pub struct Variant {
    pub docs: Vec<String>,
    pub attrs: Vec<Attr>,
    pub name: String,
    pub fields: VariantFields,
    pub discriminant: Option<Expr>,
    /// `error enum` message template.
    pub message: Option<Expr>,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct TypeDecl {
    pub kind: TypeKind,
    pub is_error: bool,
    pub name: String,
    pub name_span: Span,
    pub generics: Generics,
    pub fields: Vec<Field>,
    pub tuple_fields: Vec<(Vis, Type)>,
    pub variants: Vec<Variant>,
    pub supertypes: Vec<Type>,
    /// Methods, associated types and consts declared in the body.
    pub members: Vec<Item>,
    pub init: Option<Block>,
    /// `error class X(...) = "message"`
    pub message: Option<Expr>,
    /// `shared class`: values are shared handles (`Rc<RefCell<..>>`, or `Arc<Mutex<..>>` with `@sync`).
    pub shared: bool,
    /// `context(cx: &mut App) class ...`: the contexts its methods may take; each takes those it uses.
    pub contexts: Vec<Param>,
}

#[derive(Clone, Debug)]
pub struct TraitDecl {
    pub name: String,
    pub name_span: Span,
    pub generics: Generics,
    pub supertraits: Vec<Type>,
    pub members: Vec<Item>,
    pub is_unsafe: bool,
}

#[derive(Clone, Debug)]
pub struct ImplDecl {
    pub generics: Generics,
    pub trait_: Option<Type>,
    pub negative: bool,
    pub self_ty: Type,
    pub members: Vec<Item>,
    pub is_unsafe: bool,
}

#[derive(Clone, Debug)]
pub enum UseTree {
    Path { segments: Vec<String>, alias: Option<String> },
    Glob { segments: Vec<String> },
    Group { segments: Vec<String>, items: Vec<UseTree> },
}

#[derive(Clone, Debug)]
pub struct ImportDecl {
    pub tree: UseTree,
}

#[derive(Clone, Debug)]
pub struct ConstDecl {
    pub name: String,
    pub mutable: bool,
    pub ty: Option<Type>,
    pub value: Option<Expr>,
}

#[derive(Clone, Debug)]
pub struct TestDecl {
    pub name: String,
    pub throws: bool,
    pub is_async: bool,
    pub body: Block,
}

// ---------------------------------------------------------------------------------------------
// Types

#[derive(Clone, Debug, PartialEq)]
pub struct PathSeg {
    pub name: String,
    pub generics: Option<Vec<GenericArg>>,
    /// `Fn(A, B) -> C` sugar
    pub fn_sugar: Option<(Vec<Type>, Option<Box<Type>>)>,
}

impl PathSeg {
    pub fn plain(name: impl Into<String>) -> PathSeg {
        PathSeg { name: name.into(), generics: None, fn_sugar: None }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum GenericArg {
    Type(Type),
    Lifetime(String),
    Binding(String, Type),
    Const(String),
}

#[derive(Clone, Debug, PartialEq)]
pub enum Type {
    Path { global: bool, segs: Vec<PathSeg>, span: Span },
    /// `<T as Trait>::Name`
    Qualified { ty: Box<Type>, trait_: Option<Box<Type>>, rest: Vec<PathSeg> },
    Ref { lifetime: Option<String>, mutable: bool, inner: Box<Type> },
    Ptr { mutable: bool, inner: Box<Type> },
    Tuple(Vec<Type>),
    Array(Box<Type>, String),
    Slice(Box<Type>),
    /// `T?`
    Option(Box<Type>),
    /// `(A, B) -> C`
    Fn { params: Vec<Type>, ret: Box<Type> },
    Dyn(Vec<Type>),
    Impl(Vec<Type>),
    /// `A + B + 'a` (bounds list in type position)
    Bounds(Vec<Type>),
    Lifetime(String),
    /// `?Sized`
    Maybe(Box<Type>),
    Never,
    Infer,
    /// Raw Rust text (e.g. `fn(i32) -> i32` pointer types, macro types).
    Raw(String),
}

impl Type {
    pub fn simple(name: &str) -> Type {
        Type::Path { global: false, segs: vec![PathSeg::plain(name)], span: Span::default() }
    }
    /// `[T]` where a value is: a list, `Vec<T>`.
    pub fn list(elem: Type, span: Span) -> Type {
        Type::Path { global: false, segs: vec![PathSeg { name: "Vec".into(), generics: Some(vec![GenericArg::Type(elem)]), fn_sugar: None }], span }
    }
    /// `[K: V]`, std's `HashMap<K, V>`. Its path starts at the crate root (`::HashMap`), which no
    /// Rust type can (it would name a crate), so the lowering names std's map whatever the
    /// module imports.
    pub fn map(key: Type, value: Type, span: Span) -> Type {
        let generics = Some(vec![GenericArg::Type(key), GenericArg::Type(value)]);
        Type::Path { global: true, segs: vec![PathSeg { name: "HashMap".into(), generics, fn_sugar: None }], span }
    }
    /// Was the type written `[K: V]`?
    pub fn is_map(&self) -> bool {
        matches!(self, Type::Path { global: true, segs, .. } if segs.len() == 1 && segs[0].name == "HashMap")
    }
    /// The last path segment name, if a plain path type.
    pub fn last_name(&self) -> Option<&str> {
        match self {
            Type::Path { segs, .. } => segs.last().map(|s| s.name.as_str()),
            _ => None,
        }
    }
    pub fn single_name(&self) -> Option<&str> {
        match self {
            Type::Path { segs, .. } if segs.len() == 1 => Some(&segs[0].name),
            _ => None,
        }
    }
    pub fn is_unit(&self) -> bool {
        matches!(self, Type::Tuple(v) if v.is_empty()) || self.single_name() == Some("Unit")
    }
    pub fn is_string(&self) -> bool {
        self.single_name() == Some("String")
    }
}

// ---------------------------------------------------------------------------------------------
// Patterns

#[derive(Clone, Debug, PartialEq)]
pub enum Pat {
    Wild,
    Rest,
    Ident { name: String, by_ref: bool, mutable: bool, sub: Option<Box<Pat>>, span: Span },
    Lit(Box<Expr>),
    Range { lo: Option<Box<Expr>>, hi: Option<Box<Expr>>, inclusive: bool },
    Tuple(Vec<Pat>),
    Path(Vec<String>, Span),
    TupleStruct { path: Vec<String>, elems: Vec<Pat>, span: Span },
    Struct { path: Vec<String>, fields: Vec<(String, Option<Pat>)>, rest: bool, span: Span },
    Ref { mutable: bool, pat: Box<Pat> },
    Or(Vec<Pat>),
    Slice(Vec<Pat>),
    /// Kotlin `null` -> `None`
    Null,
    Typed { pat: Box<Pat>, ty: Type },
    /// Raw Rust pattern text (macro patterns etc.)
    Raw(String),
}

impl Pat {
    pub fn bindings(&self, out: &mut Vec<String>) {
        match self {
            Pat::Ident { name, sub, .. } => {
                out.push(name.clone());
                if let Some(s) = sub {
                    s.bindings(out);
                }
            }
            Pat::Tuple(v) | Pat::Or(v) | Pat::Slice(v) => {
                for p in v {
                    p.bindings(out)
                }
            }
            Pat::TupleStruct { elems, .. } => {
                for p in elems {
                    p.bindings(out)
                }
            }
            Pat::Struct { fields, .. } => {
                for (name, p) in fields {
                    match p {
                        Some(p) => p.bindings(out),
                        None => out.push(name.clone()),
                    }
                }
            }
            Pat::Ref { pat, .. } => pat.bindings(out),
            Pat::Typed { pat, .. } => pat.bindings(out),
            _ => {}
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Expressions

#[derive(Clone, Debug, PartialEq)]
pub struct Expr {
    pub kind: ExprKind,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Arg {
    pub name: Option<String>,
    pub value: Expr,
}

#[derive(Clone, Debug, PartialEq)]
pub struct LambdaParam {
    pub pat: Pat,
    pub ty: Option<Type>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Cond {
    Expr(Expr),
    /// `val pat = expr` -> `let pat = expr` (bare identifier means `Some(ident)`)
    Let { pat: Pat, expr: Expr, implicit_some: bool },
    /// Let chain: `val a = x && a > 0 && val b = y` (each part is `Expr` or `Let`)
    Chain(Vec<Cond>),
}

#[derive(Clone, Debug, PartialEq)]
pub enum WhenCond {
    Pat(Pat),
    /// `in range`, `!in range`
    In { expr: Expr, negated: bool },
    /// condition in a subject-less `when`
    Expr(Expr),
    Else,
}

#[derive(Clone, Debug, PartialEq)]
pub struct WhenArm {
    /// Attributes on the arm (`@allow(unreachable_patterns) _ -> ...`), as Rust attribute contents.
    pub attrs: Vec<String>,
    pub conds: Vec<WhenCond>,
    pub guard: Option<Expr>,
    pub body: Expr,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Catch {
    pub binding: Pat,
    pub ty: Option<Type>,
    pub body: Block,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Rem,
    And,
    Or,
    BitAnd,
    BitOr,
    BitXor,
    Shl,
    Shr,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
}

impl BinOp {
    pub fn as_str(self) -> &'static str {
        use BinOp::*;
        match self {
            Add => "+",
            Sub => "-",
            Mul => "*",
            Div => "/",
            Rem => "%",
            And => "&&",
            Or => "||",
            BitAnd => "&",
            BitOr => "|",
            BitXor => "^",
            Shl => "<<",
            Shr => ">>",
            Eq => "==",
            Ne => "!=",
            Lt => "<",
            Le => "<=",
            Gt => ">",
            Ge => ">=",
        }
    }
    /// Rust precedence (higher binds tighter).
    pub fn prec(self) -> u8 {
        use BinOp::*;
        match self {
            Mul | Div | Rem => 11,
            Add | Sub => 10,
            Shl | Shr => 9,
            BitAnd => 8,
            BitXor => 7,
            BitOr => 6,
            Eq | Ne | Lt | Le | Gt | Ge => 5,
            And => 4,
            Or => 3,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnOp {
    Neg,
    Not,
    Deref,
    Ref,
    RefMut,
}

#[derive(Clone, Debug, PartialEq)]
pub enum ExprKind {
    Int(String),
    Float(String),
    Bool(bool),
    Char(String),
    Str(Vec<StrPart>),
    /// Interpolated parts after parsing: (literal text) or (expr, spec)
    Template(Vec<TemplatePart>),
    RawStr(String),
    Null,
    /// Identifier or `a::b::c` path, with optional turbofish generics on the last segment.
    Path { segs: Vec<String>, generics: Option<Vec<GenericArg>>, global: bool },
    /// `<T as Trait>::item`
    QualifiedPath(Type),
    SelfValue,
    Unary(UnOp, Box<Expr>),
    Binary(BinOp, Box<Expr>, Box<Expr>),
    /// `a = b`, `a += b` (op is None for plain assignment)
    Assign(Option<BinOp>, Box<Expr>, Box<Expr>),
    Call { callee: Box<Expr>, args: Vec<Arg> },
    MethodCall { recv: Box<Expr>, name: String, generics: Option<Vec<GenericArg>>, args: Vec<Arg>, safe: bool },
    Field { recv: Box<Expr>, name: String, safe: bool },
    Index(Box<Expr>, Box<Expr>),
    /// `expr?`
    Try(Box<Expr>),
    /// `expr!!`
    Unwrap(Box<Expr>),
    Await(Box<Expr>),
    Cast(Box<Expr>, Type),
    Elvis(Box<Expr>, Box<Expr>),
    In { item: Box<Expr>, coll: Box<Expr>, negated: bool },
    /// `x is Pattern` -> `matches!(x, Pattern)`
    Is { expr: Box<Expr>, pat: Pat, negated: bool },
    /// `a to b` -> `(a, b)`
    Pair(Box<Expr>, Box<Expr>),
    If { cond: Box<Cond>, then: Block, else_: Option<Box<Expr>> },
    When { subject: Option<Box<Expr>>, binding: Option<Pat>, arms: Vec<WhenArm>, rust_match: bool },
    Block(Block),
    Unsafe(Block),
    Async { is_move: bool, block: Block },
    Loop { label: Option<String>, body: Block },
    While { label: Option<String>, cond: Box<Cond>, body: Block },
    DoWhile { label: Option<String>, body: Block, cond: Box<Expr> },
    For { label: Option<String>, pat: Pat, iter: Box<Expr>, body: Block },
    /// `implicit_it`: a brace lambda written without parameters (`{ it * 2 }`), whose `it` is its
    /// parameter. Closures with an explicit empty list (`|| it + 1`, `() => it`) capture an outer
    /// `it` instead.
    Lambda { params: Vec<LambdaParam>, ret: Option<Type>, body: Box<Expr>, is_move: bool, implicit_it: bool, is_async: bool },
    Return(Option<Box<Expr>>),
    Break { label: Option<String>, value: Option<Box<Expr>> },
    Continue(Option<String>),
    Throw(Box<Expr>),
    Tuple(Vec<Expr>),
    Array(Vec<Expr>),
    ArrayRepeat(Box<Expr>, Box<Expr>),
    /// `vec[...]` / `listOf(...)`
    VecLit(Vec<Expr>),
    MapLit(Vec<Expr>),
    SetLit(Vec<Expr>),
    Range { start: Option<Box<Expr>>, end: Option<Box<Expr>>, inclusive: bool },
    StructLit { path: Vec<String>, generics: Option<Vec<GenericArg>>, fields: Vec<(String, Option<Expr>)>, base: Option<Box<Expr>> },
    /// Raw macro call `name!(...)` - text passed through.
    Macro { path: String, text: String },
    TryCatch { body: Block, catch: Option<Box<Catch>> },
    /// `'label: { ... }` (a block that `break 'label value` can leave)
    LabeledBlock { label: String, block: Block },
    /// `const { ... }` evaluated at compile time
    ConstBlock(Block),
    Paren(Box<Expr>),
    Raw(String),
    /// Placeholder after a parse error.
    Error,
}

#[derive(Clone, Debug, PartialEq)]
pub enum TemplatePart {
    Lit(String),
    Expr(Expr, Option<String>),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Block {
    pub stmts: Vec<Stmt>,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Stmt {
    Let { pat: Pat, mutable: bool, ty: Option<Type>, init: Option<Expr>, else_: Option<Block>, span: Span },
    Expr { expr: Expr, semi: bool },
    Item(Box<ItemBox>),
    /// Statement with outer attributes (`@cfg(test) println(...)`); attribute contents as Rust text.
    Attributed { attrs: Vec<String>, stmt: Box<Stmt> },
}

/// Wrapper so `Stmt` can derive PartialEq without requiring it on items.
#[derive(Clone, Debug)]
pub struct ItemBox(pub Item);

impl PartialEq for ItemBox {
    fn eq(&self, _: &Self) -> bool {
        false
    }
}

impl Expr {
    pub fn new(kind: ExprKind, span: Span) -> Expr {
        Expr { kind, span }
    }
    pub fn ident(&self) -> Option<&str> {
        match &self.kind {
            ExprKind::Path { segs, generics: None, global: false } if segs.len() == 1 => Some(&segs[0]),
            _ => None,
        }
    }
    /// Expressions that end with a block and need no `;` as statements.
    pub fn is_block_like(&self) -> bool {
        matches!(
            self.kind,
            ExprKind::If { .. }
                | ExprKind::When { .. }
                | ExprKind::Block(_)
                | ExprKind::Unsafe(_)
                | ExprKind::Loop { .. }
                | ExprKind::While { .. }
                | ExprKind::DoWhile { .. }
                | ExprKind::For { .. }
                | ExprKind::TryCatch { .. }
                | ExprKind::LabeledBlock { .. }
                | ExprKind::ConstBlock(_)
        )
    }
    /// Expressions that never produce a value.
    pub fn diverges(&self) -> bool {
        match &self.kind {
            ExprKind::Return(_) | ExprKind::Break { .. } | ExprKind::Continue(_) | ExprKind::Throw(_) => true,
            ExprKind::Macro { path, .. } => matches!(path.as_str(), "panic" | "todo" | "unimplemented" | "unreachable"),
            ExprKind::Call { callee, .. } => matches!(callee.ident(), Some("exit") | Some("panic") | Some("todo") | Some("unimplemented") | Some("unreachable")),
            _ => false,
        }
    }
}
