//! The `.oxi` files of the crate a rustc error comes from, for explanations that look beyond
//! the error's own file: the declaration of the function it calls, of a type it names.

use std::cell::OnceCell;
use std::collections::HashSet;
use std::path::{Path, PathBuf};

use oxie_compiler::ast::{File, Item, ItemKind};
use oxie_compiler::compile::canonicalize;
use oxie_compiler::diag::Span;
use oxie_compiler::emit::Decision;
use oxie_compiler::{Input, Options};

use crate::project::Project;

pub struct SourceFile {
    pub path: PathBuf,
    /// The path relative to the crate's `src/` (`main.oxi` for a script).
    pub rel: PathBuf,
    pub text: String,
    ast: OnceCell<File>,
}

impl SourceFile {
    pub fn new(path: PathBuf, rel: PathBuf, text: String) -> SourceFile {
        SourceFile { path, rel, text, ast: OnceCell::new() }
    }

    pub fn lines(&self) -> Vec<&str> {
        self.text.lines().collect()
    }

    /// The parsed file, parsed on first use.
    pub fn ast(&self) -> &File {
        self.ast.get_or_init(|| oxie_compiler::parser::parse_file(&self.text).0)
    }
}

pub struct Sources {
    pub files: Vec<SourceFile>,
    /// A single-file script rather than a project.
    script: bool,
    /// What compiling the crate decided, by file (see `decisions`).
    decisions: OnceCell<Vec<(PathBuf, Vec<Decision>)>>,
}

impl Sources {
    /// Every `.oxi` file of the project `file` belongs to, or only `file` for a script.
    pub fn of(file: &Path) -> Sources {
        let file = canonicalize(file).unwrap_or_else(|_| file.to_path_buf());
        let project = file.parent().and_then(Project::find).filter(|p| file.starts_with(canonicalize(p.src_dir()).unwrap_or_else(|_| p.src_dir())));
        let script = project.is_none();
        let mut files: Vec<SourceFile> = project
            .and_then(|p| oxie_compiler::discover(&p.src_dir()).ok())
            .unwrap_or_default()
            .into_iter()
            .map(|i| SourceFile::new(canonicalize(&i.abs_path).unwrap_or(i.abs_path), i.rel_path, i.text))
            .collect();
        if !files.iter().any(|f| f.path == file) {
            files.push(SourceFile::new(file.clone(), PathBuf::from("main.oxi"), std::fs::read_to_string(&file).unwrap_or_default()));
        }
        Sources { files, script, decisions: OnceCell::new() }
    }

    /// A crate of in-memory files (for tests).
    #[cfg(test)]
    pub fn from_texts(files: &[(&str, &str)]) -> Sources {
        let files = files.iter().map(|(p, t)| SourceFile::new(PathBuf::from(p), PathBuf::from(Path::new(p).file_name().unwrap()), t.to_string())).collect();
        Sources { files, script: false, decisions: OnceCell::new() }
    }

    /// What the compiler decided for each file that the source doesn't spell out (an inferred
    /// `&mut self`, a lent parameter): the crate is compiled again, in memory, on first use.
    pub fn decisions(&self) -> &[(PathBuf, Vec<Decision>)] {
        self.decisions.get_or_init(|| {
            let inputs = self.files.iter().map(|f| Input { rel_path: f.rel.clone(), abs_path: f.path.clone(), text: f.text.clone() }).collect();
            let opts = Options { crate_name: "main".into(), extern_crates: HashSet::new(), out_dir: PathBuf::from("/oxie"), script: self.script, root_attrs: Vec::new() };
            let out = oxie_compiler::compile(inputs, &opts);
            self.files.iter().zip(out.files).map(|(f, g)| (f.path.clone(), g.decisions)).collect()
        })
    }

    /// Why the method `name` of `class` takes `&mut self` when Oxie inferred it: its file, the use
    /// that decided it and the reason.
    pub fn mut_self(&self, name: &str, class: &str) -> Option<(&SourceFile, Option<Span>, String)> {
        let mut found: Vec<(&SourceFile, Span, Option<Span>, String)> = Vec::new();
        for (path, ds) in self.decisions() {
            let Some(file) = self.file(path) else { continue };
            let is_method = |s: Span| file.text.get(s.start as usize..s.end as usize) == Some(name) && owner_at(&file.ast().items, s.start).as_deref() == Some(class);
            for d in ds.iter().filter(|d| d.chosen == "&mut self" && is_method(d.span)) {
                if !found.iter().any(|(f, s, ..)| f.path == file.path && *s == d.span) {
                    found.push((file, d.span, d.cause, d.why.clone()));
                }
            }
        }
        let [(file, _, cause, why)] = <[_; 1]>::try_from(found).ok()?;
        Some((file, cause, why))
    }

    pub fn file(&self, path: &Path) -> Option<&SourceFile> {
        self.files.iter().find(|f| f.path == path).or_else(|| {
            let path = canonicalize(path).ok()?;
            self.files.iter().find(|f| f.path == path)
        })
    }

    /// The file declaring something, by a test on each file's lines: `current` when it declares
    /// it, else the only other file that does (with two, the name is ambiguous).
    pub fn declaring<'a>(&'a self, current: &Path, declares: impl Fn(&[&str]) -> bool) -> Option<&'a SourceFile> {
        let here = self.file(current);
        if let Some(f) = here.filter(|f| declares(&f.lines())) {
            return Some(f);
        }
        let mut found = self.files.iter().filter(|f| here.is_none_or(|h| h.path != f.path) && declares(&f.lines()));
        let first = found.next()?;
        found.next().is_none().then_some(first)
    }

    /// `path` when it isn't the error's file `current` (where edits and labels go by default).
    pub fn elsewhere(&self, path: &Path, current: &Path) -> Option<PathBuf> {
        let here = self.file(current).map(|f| f.path.as_path()).unwrap_or(current);
        (path != here).then(|| path.to_path_buf())
    }
}

/// Does the file declare a function `name` outside classes and `impl` blocks?
pub fn declares_top_level_fun(items: &[Item], name: &str) -> bool {
    items.iter().any(|it| match &it.kind {
        ItemKind::Fun(f) => f.name == name,
        ItemKind::Mod { items, .. } => declares_top_level_fun(items, name),
        _ => false,
    })
}

/// The class, enum or `impl` block whose body holds `offset`.
pub fn owner_at(items: &[Item], offset: u32) -> Option<String> {
    for it in items.iter().filter(|it| it.span.start <= offset && offset <= it.span.end) {
        match &it.kind {
            ItemKind::Type(t) => return Some(t.name.clone()),
            ItemKind::Impl(i) => return i.self_ty.last_name().map(str::to_string),
            ItemKind::Mod { items, .. } | ItemKind::Object { items, .. } => return owner_at(items, offset),
            _ => {}
        }
    }
    None
}
