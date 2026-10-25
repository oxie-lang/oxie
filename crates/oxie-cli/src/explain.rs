//! Plain-language explanations and quick fixes for the rustc errors Oxie programmers hit most.
//!
//! rustc reports errors against the generated Rust. After they are mapped back to .oxi lines,
//! this module rewrites the common ones in Oxie terms (`val`/`var`, lambdas, `throws`), relabels
//! the related spans ("moved here", "borrowed here") and computes fixes as edits to the .oxi
//! source. Fixes come from two places: rustc's own suggestions whose spans map exactly onto the
//! source, and Oxie-specific edits found by looking at the source text. A declaration the error
//! depends on (the function a value moved into, a type) may be in another file of the crate.

use std::path::{Path, PathBuf};

use crate::cargo::{Edit, Fix, Label, MappedDiag, Suggestion};
use crate::sources::Sources;
use oxie_compiler::ast::{Block, Cond, Expr, ExprKind, File, FunBody, FunDecl, Item, ItemKind, Pat, Stmt, Type, TypeDecl, UnOp};
use oxie_compiler::diag::Span;

pub fn explain(d: &mut MappedDiag, sources: &Sources) {
    let Some(here) = sources.file(&d.file) else { return };
    let src = here.text.as_str();
    let lines = here.lines();
    if not_optional(d, src) {
        generated_names(d);
        return;
    }
    let names = ticks(&d.message);
    let first = names.first().cloned().unwrap_or_default();
    let code = d.code.clone().unwrap_or_default();
    derive_in_oxie_terms(d);
    match code.as_str() {
        "E0382" => moved_value(d, sources, &lines, &first),
        "E0384" => assign_twice(d, &lines, &first),
        "E0596" => borrow_mut(d, sources, &lines, &first),
        "E0499" | "E0502" | "E0505" | "E0506" => borrow_conflict(d, sources, &lines, &first, &code),
        "E0507" => {
            let at = byte_offset(src, d.line, d.col) as u32;
            // `items[0]` (rustc: "cannot move out of index of `Vec<String>`") stays in the list,
            // whether the list is borrowed or not
            let reason = if d.message.starts_with("cannot move out of index of") {
                let (element, list) = match indexed_at(&here.ast().items, src, at) {
                    Some((element, list)) => (format!("`{}`", element), format!("`{}`", list)),
                    None => ("an element".to_string(), describe(&first)),
                };
                rewrite(d, format!("can't take {} out of {}", element, list));
                format!("{} keeps its elements, and indexing only reaches one where it is, so it can't be moved out.", list)
            } else {
                rewrite(d, format!("can't take {} out of a value that is only borrowed", describe(&first)));
                "the value belongs to something you only borrowed, so it can't be moved out.".to_string()
            };
            d.notes.retain(|n| !n.contains("`mut "));
            // rustc offers `&` because `Display` puts the text in a `String` variable, as for a
            // `val`, but the text of a `toString` can't be a reference
            if is_to_string_text(&here.ast().items, at) {
                d.notes.retain(|n| !n.starts_with("help: consider borrowing here"));
                d.suggestions.retain(|s| s.replacement != "&");
            }
            d.notes.push(help(&format!("{} Take a copy with `.clone()`, or borrow it with `&`.", reason)));
        }
        "E0594" if d.message.contains("not declared as mutable") => borrow_mut(d, sources, &lines, &first),
        "E0594" if d.message.contains("behind a `&` reference") => {
            let name = first.trim_start_matches('*').split(['.', '[']).next().unwrap_or("").to_string();
            rewrite(d, format!("`{}` is a read-only reference (`&`), so it can't be changed through it", name));
            d.notes.retain(|n| !n.contains("`mut "));
            loop_over_mut(d, &lines, &name);
        }
        "E0373" => lambda_outlives(d, &lines, &names),
        "E0597" => {
            rewrite(d, format!("`{}` is dropped while something still refers to it", first));
            d.notes.push(help(&format!("a reference to `{}` is used after `{}` goes out of scope (the end of its block). Declare `{}` in an outer block, or store a copy instead of a reference.", first, first, first)));
            lifetime_hint(d, sources, &lines);
        }
        "E0716" => {
            rewrite(d, "a temporary value is dropped while something still refers to it".to_string());
            d.notes.retain(|n| !n.contains("`let` binding"));
            d.notes.push(help("store the value in a `val` first, then borrow from that variable."));
            lifetime_hint(d, sources, &lines);
        }
        "E0106" => {
            d.notes.push(help("Oxie fills in lifetimes for functions and for classes that hold references, but not here; write them yourself (`fun f<'a>(x: &'a str): &'a str`), or hold owned values (`String` instead of `&str`) to avoid lifetimes altogether."));
        }
        "E0599" | "E0624" if is_optional_receiver(d) => optional_receiver(d, &lines, &first),
        "E0599" => no_method(d, sources, &lines, &first),
        "E0433" | "E0412" | "E0425" | "E0422" | "E0423" | "E0531" | "E0532" => {
            if d.message.contains("undeclared") || d.message.starts_with("cannot find") {
                rewrite(d, format!("`{}` isn't defined or imported here", last_segment(&first)));
            }
            imports(d, &lines);
        }
        "E0277" => trait_missing(d, sources, &lines, &names),
        "E0308" => mismatched(d, src, &lines),
        "E0615" => getter_call(d, &lines, &first),
        // `filter { seen.insert(it) }`: the lambda's parameter is a reference into the iteration
        "E0521" if d.label.as_deref().is_some_and(|l| l.ends_with("escapes the closure body here")) => {
            let name = d.label.as_deref().and_then(|l| between(l, "`", "`")).unwrap_or_else(|| "it".to_string());
            rewrite(d, format!("`{}` is a reference, and is stored where it would outlive the lambda", name));
            d.notes.retain(|n| !n.contains("invariant") && !n.contains("nomicon") && !n.contains("requirement occurs"));
            d.notes.push(help(&format!("lambdas like `filter`, `find` and `any` get a reference to each element, which lives only for that call. Store a copy instead: `*{}` for a number or `char`, `{}.clone()` otherwise.", name, name)));
        }
        "E0283" if d.notes.iter().any(|n| n.contains("Option<_>: From<Option<")) => optional_already(d, &lines),
        // reported without a code
        _ if d.message.starts_with("future cannot be sent between threads safely") => thread_safety(d, sources, ""),
        _ => {}
    }
    match d.code.as_deref() {
        Some("unused_mut") => unused_mut(d, &lines),
        Some("unused_variables") => unused_variable(d, &lines, &first),
        Some("unused_must_use") if d.message.contains("unused `Result`") => unused_result(d, &lines),
        _ => {}
    }
    rustc_fixes(d, &lines);
    d.secondary.retain(|s| s.exact && !s.text.is_empty());
    d.notes.retain(|n| !n.contains("in format strings you may be able to use"));
    generated_names(d);
    field_names(d, sources);
}

/// rustc names `this` and its fields as Rust does (`self.items`, `*self`); messages, labels,
/// notes and fix titles name them as the code does at their position (see `oxie_place`).
fn field_names(d: &mut MappedDiag, sources: &Sources) {
    let written = |file: &Path, line: u32, col: u32| -> String { sources.file(file).and_then(|f| f.lines().get(line as usize).map(|l| l.chars().skip(col as usize).collect())).unwrap_or_default() };
    let here = written(&d.file, d.line, d.col);
    d.message = oxie_places(&d.message, &here);
    d.label = d.label.take().map(|l| oxie_places(&l, &here));
    for n in d.notes.iter_mut().filter(|n| !n.starts_with("rustc: ")) {
        *n = oxie_places(n, &here);
    }
    for f in &mut d.fixes {
        f.title = oxie_places(&f.title, &here);
    }
    for s in &mut d.secondary {
        s.text = oxie_places(&s.text, &written(s.file.as_deref().unwrap_or(&d.file), s.line, s.col));
    }
}

/// `text` with each place of `this` between backticks in Oxie's words.
fn oxie_places(text: &str, written: &str) -> String {
    text.split('`').enumerate().map(|(i, s)| if i % 2 == 1 { oxie_place(s, written).unwrap_or_else(|| s.to_string()) } else { s.to_string() }).collect::<Vec<_>>().join("`")
}

/// `main() throws` is generated as `__oxie_main` (called from a plain `main`); messages call
/// it by its Oxie name.
fn generated_names(d: &mut MappedDiag) {
    let fix = |s: &mut String| {
        if s.contains("__oxie_main") {
            *s = s.replace("__oxie_main", "main");
        }
    };
    fix(&mut d.message);
    d.label.iter_mut().for_each(fix);
    d.notes.iter_mut().for_each(fix);
    d.secondary.iter_mut().for_each(|s| fix(&mut s.text));
    d.rust_message.iter_mut().for_each(fix);
}

// ---------------------------------------------------------------------------------------------
// individual errors

fn moved_value(d: &mut MappedDiag, sources: &Sources, lines: &[&str], name: &str) {
    if moved_by_unwrap(d, lines, name) {
        return;
    }
    // a `move` lambda takes what it uses; rustc's "value moved into closure here" is at its start
    let into = |t: &str| t.contains("into closure");
    let at = if d.label.as_deref().is_some_and(into) { Some((d.line, d.col)) } else { d.secondary.iter().find(|s| into(&s.text)).map(|s| (s.line, s.col)) };
    if let Some(lambda) = at.and_then(|(l, c)| move_lambda(sources, &d.file, lines, l, c, name)) {
        moved_into_lambda(d, lines, name, lambda);
        return;
    }
    refine(d, lines, name);
    // rustc suggests a copy (`.clone()`, `<T as Clone>::clone(..)`, or one taken before a lambda)
    // only when the type implements `Clone`; a plain `.clone()` is its suggestion at the move
    let copyable = d.raw_suggestions.iter().any(|s| s.contains("clone("));
    let clone_here = d.raw_suggestions.iter().any(|s| s == ".clone()");
    let in_loop = d.label.as_deref().is_some_and(|l| l.contains("previous iteration")) || d.secondary.iter().any(|s| s.text.contains("previous iteration"));
    let mut move_site: Option<(u32, u32)> = None;
    let mut callee: Option<(String, usize)> = None;
    if in_loop && d.label.as_deref().is_some_and(|l| l.contains("previous iteration")) {
        move_site = Some((d.line, d.col));
        callee = call_around(lines.get(d.line as usize).copied().unwrap_or(""), d.col as usize);
        d.label = Some(format!("`{}` is moved here in every iteration of the loop", name));
    }
    for s in d.secondary.iter_mut() {
        // a lambda without `move` takes a value its body moves: the move is at that use
        if s.text.contains("moved here") || s.text.contains("moved due to use in closure") || (s.text.contains("value moved") && !s.text.contains("into closure")) {
            let line = lines.get(s.line as usize).copied().unwrap_or("");
            if let Some(c) = find_word(line, name, s.col as usize) {
                s.col = c as u32;
                s.end_col = (c + name.chars().count()) as u32;
            }
            callee = call_around(line, s.col as usize);
            s.text = match &callee {
                Some((f, _)) => format!("`{}` moved into `{}()` here", name, f),
                None if line.contains(" in ") && line.trim_start().starts_with("for") => format!("the loop takes `{}` here", name),
                None => format!("`{}` moved here", name),
            };
            move_site = Some((s.line, s.col));
            s.exact = true;
        } else if s.text.contains("move occurs because") || s.text.contains("inside of this loop") || s.text.contains("into closure") {
            s.text = String::new();
        }
    }
    d.secondary.retain(|s| !s.text.is_empty());
    if !in_loop {
        d.label = Some("used here after the move".into());
    }
    rewrite(
        d,
        if in_loop { format!("`{}` was already moved in an earlier loop iteration", name) } else { format!("`{}` was moved, so it can't be used anymore", name) },
    );
    d.notes.retain(|n| !n.starts_with("note: move occurs") && !n.contains("consider changing this parameter type") && !n.contains("consider cloning"));
    let callee_text = callee.as_ref().map(|(f, _)| format!("`{}`", f)).unwrap_or_else(|| "the new owner".into());
    // the callee's parameter, declared in this file or another one: (file, line, type start,
    // type end, type, declaration line)
    let param = callee.as_ref().and_then(|(f, index)| {
        let file = sources.declaring(&d.file, |ls| fun_param(ls, f, *index).is_some())?;
        let lines = file.lines();
        let (pl, pc, pe, ty) = fun_param(&lines, f, *index)?;
        Some((sources.elsewhere(&file.path, &d.file), pl, pc, pe, ty, lines[pl as usize].to_string()))
    });
    // Oxie already lends `String` and `Vec` parameters that a top-level function only reads; one
    // that stayed owned is kept by the function (stored or returned), so `&` wouldn't compile.
    // Nor would it for a parameter of any function that stores or returns it.
    let stored = callee.as_ref().filter(|_| param.is_some()).and_then(|(f, index)| stored_reason(sources, &d.file, f, *index));
    let lent_kind = param.as_ref().is_some_and(|(.., ty, decl)| ["fun ", "private fun ", "internal fun "].iter().any(|p| decl.starts_with(p)) && (ty == "String" || ty.starts_with("Vec<")));
    let kept = lent_kind || stored.is_some();
    if kept {
        // say where and why the function keeps it, from the same rules that decided it
        let why = callee.as_ref().and_then(|(f, index)| kept_reason(sources, &d.file, f, *index)).or(stored);
        if let Some((file, l, c, e, why)) = &why {
            d.secondary.push(Label { file: file.clone(), line: *l, col: *c, end_col: *e, text: format!("{} keeps it: {}", callee_text, why), exact: true });
        }
        let because = why.map(|(.., w)| format!(" ({})", w)).unwrap_or_default();
        let instead = match (copyable, in_loop) {
            (true, _) => format!(" Pass a copy with `{}.clone()`.", name),
            (false, false) => format!(" Finish using `{}` before the call.", name),
            (false, true) => String::new(),
        };
        d.notes.push(help(&format!(
            "giving a value to a function moves it: {} keeps it{}, and `{}` can't be used afterwards.{}",
            callee_text, because, name, instead
        )));
    } else {
        let instead = if copyable { format!("Pass a copy with `{}.clone()`, or let {} borrow the value instead", name, callee_text) } else { format!("Let {} borrow the value instead", callee_text) };
        d.notes.push(help(&format!(
            "giving a value to a function or another variable moves it: {} owns it now, and `{}` can't be used afterwards. {}.",
            callee_text, name, instead
        )));
    }
    if let Some((l, c)) = move_site.filter(|_| clone_here) {
        let line = lines.get(l as usize).copied().unwrap_or("");
        if word_at(line, c as usize) == Some(name) {
            let end = c + name.chars().count() as u32;
            d.fixes.push(Fix { title: format!("pass a copy of `{}`", name), edits: vec![edit(l, end, end, ".clone()")] });
        }
    }
    if let (Some((f, _)), Some((file, pl, pc, pe, ty, _))) = (callee, param) {
        if !ty.starts_with('&') && !is_copy_type(&ty) && !kept {
            let borrowed = borrowed_type(&ty);
            d.fixes.push(Fix { title: format!("let `{}` borrow it (parameter type `{}`)", f, borrowed), edits: vec![Edit { file, ..edit(pl, pc, pe, &borrowed) }] });
        }
    }
}

/// `if (val x = opt)` (or `while`) takes the value out of `opt`; rustc may not even name it
/// ("use of moved value"). Unwrapping a reference, `if (val x = &opt)`, keeps `opt`.
fn moved_by_unwrap(d: &mut MappedDiag, lines: &[&str], name: &str) -> bool {
    let unwrapped = |l: u32| -> Option<(u32, String)> {
        let line = lines.get(l as usize).copied().unwrap_or("");
        let mut from = 0;
        while let Some(i) = line[from..].find("(val ") {
            let rest = &line[from + i + 5..];
            from += i + 5;
            let Some((_, src)) = rest.split_once(" = ") else { continue };
            let opt: String = src.chars().take_while(|c| is_ident_char(*c) || *c == '.').collect();
            if !opt.is_empty() && (name.is_empty() || opt == name) && (src[opt.len()..].starts_with(')') || src[opt.len()..].starts_with(" &&")) {
                let col = line[..line.len() - src.len()].chars().count() as u32;
                return Some((col, opt));
            }
        }
        None
    };
    let moved = d.secondary.iter().position(|s| s.text.contains("moved here") || s.text.contains("value moved"));
    let Some((col, opt)) = moved.and_then(|i| unwrapped(d.secondary[i].line)) else { return false };
    let site = moved.unwrap();
    let line = d.secondary[site].line;
    if name.is_empty() {
        // the use is the same kind of unwrap, of the same value
        if let Some((c, _)) = unwrapped(d.line) {
            d.col = c;
            d.end_col = c + opt.chars().count() as u32;
        }
    } else {
        refine(d, lines, name);
    }
    let s = &mut d.secondary[site];
    s.col = col;
    s.end_col = col + opt.chars().count() as u32;
    s.text = format!("`{}` moved here: its value is taken out", opt);
    s.exact = true;
    d.secondary.retain(|s| s.text.contains("moved here"));
    d.label = Some("used here after the move".into());
    rewrite(d, format!("`{}` was moved, so it can't be used anymore", opt));
    d.notes.retain(|n| !n.contains("`ref `") && !n.starts_with("note: "));
    d.notes.push(help(&format!("`val x = {}` in a condition moves the value out of `{}`. To keep `{}`, unwrap a reference: `val x = &{}`.", opt, opt, opt, opt)));
    d.fixes.push(Fix { title: format!("unwrap a reference (`&{}`)", opt), edits: vec![edit(line, col, col, "&")] });
    true
}

/// A lambda that took a value because it is `move` (written so, or with a capture list).
struct MoveLambda {
    line: u32,
    /// `move {` or the `{` of a capture list lambda
    col: u32,
    end_col: u32,
    /// lists its captures, `{ [a] -> ... }`
    captures: bool,
    /// gives it its own copy of the value in a capture list
    fix: Option<Vec<Edit>>,
}

/// The `move` lambda starting at (line, col), and the edit that adds `name` to its capture list,
/// for `move { ... }` and `{ [a] -> ... }` written on one line with the `{`.
fn move_lambda(sources: &Sources, file: &Path, lines: &[&str], line: u32, col: u32, name: &str) -> Option<MoveLambda> {
    let here = sources.file(file)?;
    let at = byte_offset(&here.text, line, col) as u32;
    let mut found = None;
    oxie_compiler::visit::walk_items(&here.ast().items, &mut |e| {
        if let ExprKind::Lambda { is_move: true, params, body, .. } = &e.kind {
            if e.span.start == at {
                found = Some((params.is_empty(), body.span));
            }
        }
        true
    });
    let (no_params, body) = found?;
    let text = lines.get(line as usize).copied().unwrap_or("");
    let chars: Vec<char> = text.chars().collect();
    let c = col as usize;
    let before: String = chars[..c.min(chars.len())].iter().collect();
    let written_move = before.trim_end().strip_suffix("move").filter(|b| !b.chars().next_back().is_some_and(is_ident_char)).map(|b| b.chars().count() as u32);
    if chars.get(c) != Some(&'{') {
        // `move |x| ...` or `move x => ...`, which starts at `move`
        let word = chars[c.min(chars.len())..].iter().take_while(|ch| is_ident_char(**ch)).count().max(1);
        return Some(MoveLambda { line, col, end_col: col + word as u32, captures: false, fix: None });
    }
    let after: String = chars[c + 1..].iter().collect();
    let captures = written_move.is_none() && after.trim_start().starts_with('[');
    let fix = if let Some(m) = written_move {
        // `{ [name] -> ... }` has no `it`
        let uses_it = here.text.get(body.start as usize..body.end as usize).is_some_and(|b| b.lines().any(|l| find_word(l, "it", 0).is_some()));
        let list = if !no_params { Some(format!(" [{}]", name)) } else if !uses_it { Some(format!(" [{}] ->", name)) } else { None };
        list.map(|l| vec![edit(line, m, col, ""), edit(line, col + 1, col + 1, &l)])
    } else if captures {
        // before the `]` that closes the list
        let open = c + 1 + (after.chars().count() - after.trim_start().chars().count());
        let mut depth = 0;
        let close = chars.iter().enumerate().skip(open).find_map(|(i, ch)| {
            match ch {
                '[' | '(' | '{' => depth += 1,
                ']' | ')' | '}' => depth -= 1,
                _ => {}
            }
            (depth == 0).then_some(i)
        });
        close.filter(|&i| chars[i] == ']').map(|i| vec![edit(line, i as u32, i as u32, &format!(", {}", name))])
    } else {
        None
    };
    Some(MoveLambda { line, col: written_move.unwrap_or(col), end_col: col + 1, captures, fix })
}

fn moved_into_lambda(d: &mut MappedDiag, lines: &[&str], name: &str, lambda: MoveLambda) {
    let copyable = d.raw_suggestions.iter().any(|s| s.contains("clone("));
    let in_loop = d.label.as_deref().is_some_and(|l| l.contains("previous iteration"));
    let takes = format!("this lambda takes `{}`", name);
    for s in d.secondary.iter_mut() {
        if s.text.contains("into closure") {
            (s.line, s.col, s.end_col, s.text, s.exact) = (lambda.line, lambda.col, lambda.end_col, takes.clone(), true);
        } else if s.text.contains("moved due to use in closure") {
            refine_label(s, lines, name);
            s.text = format!("`{}` is used here, so the lambda takes it", name);
        } else {
            // "move occurs because", "inside of this loop", and the use of a lambda that
            // repeats the primary span
            s.text = String::new();
        }
    }
    d.secondary.retain(|s| !s.text.is_empty());
    if in_loop {
        (d.line, d.end_line, d.col, d.end_col) = (lambda.line, lambda.line, lambda.col, lambda.end_col);
        d.label = Some(format!("{} in every iteration of the loop", takes));
    } else {
        refine(d, lines, name);
        d.label = Some("used here after the move".into());
    }
    rewrite(
        d,
        if in_loop { format!("`{}` was already moved in an earlier loop iteration", name) } else { format!("`{}` was moved, so it can't be used anymore", name) },
    );
    d.notes.retain(|n| !n.starts_with("note: move occurs") && !n.contains("consider cloning"));
    let (owner, copy) = if in_loop {
        (format!("the lambda made in the first iteration owns `{}`, and later iterations can't use it", name), "each lambda")
    } else {
        (format!("the lambda owns `{}` now, and `{}` can't be used afterwards", name, name), "the lambda")
    };
    let instead = match (copyable, lambda.captures) {
        (true, false) => format!("Give {} its own copy with a capture list, `{{ [{}] -> ... }}`, or with a `{}.clone()` made before it.", copy, name, name),
        (true, true) => format!("Give {} its own copy by adding `{}` to its capture list.", copy, name),
        (false, _) if in_loop => format!("Create `{}` inside the loop, so that each iteration has its own.", name),
        (false, _) => format!("Finish using `{}` before the lambda.", name),
    };
    let rule = if lambda.captures { "a lambda with a capture list takes the other values it uses too" } else { "a `move` lambda takes the values it uses" };
    d.notes.push(help(&format!("{}, so {}. {}", rule, owner, instead)));
    // rustc's copy renames the value inside the lambda (`let value = name.clone()`)
    d.suggestions.clear();
    if let Some(edits) = lambda.fix.filter(|_| copyable) {
        d.fixes.push(Fix { title: format!("give {} its own copy of `{}` (`[{}]`)", copy, name, name), edits });
    }
}

fn assign_twice(d: &mut MappedDiag, lines: &[&str], name: &str) {
    refine(d, lines, name);
    if d.message.contains("immutable argument") {
        rewrite(d, format!("the parameter `{}` can't be changed", name));
        d.notes.retain(|n| !n.contains("`mut "));
        d.notes.push(help(&format!("parameters are read-only. Copy it into a variable first: `var {} = {}`.", name, name)));
        return;
    }
    rewrite(d, format!("`{}` is a `val`, so it can't be changed", name));
    d.label = Some("changed here".into());
    make_var(d, lines, name);
}

fn borrow_mut(d: &mut MappedDiag, sources: &Sources, lines: &[&str], first: &str) {
    let name = first.trim_start_matches('*').split(['.', '[']).next().unwrap_or("").to_string();
    refine(d, lines, &name);
    if d.message.contains("behind a `&` reference") {
        rewrite(d, format!("`{}` is a read-only reference (`&`), so it can't be changed through it", name));
        d.notes.retain(|n| !n.contains("`mut") && !n.contains("mutable reference"));
        if loop_over_mut(d, lines, &name) {
            return;
        }
        if let Some((pl, pc, fname, index)) = param_decl(lines, &name, d.line) {
            let line = lines[pl as usize];
            let rest: String = line.chars().skip(pc as usize).collect();
            if rest.starts_with('&') && !rest.starts_with("&mut ") && !rest.starts_with("&'") {
                let mut edits = vec![edit(pl, pc, pc + 1, "&mut ")];
                // callers that pass `&x` now need `&mut x`. For a top-level function, its calls
                // `f(..)` outside class bodies (where `f` may be a method), in every file of the
                // crate unless another file declares a top-level `f` too; for a method, the calls
                // `x.f(..)` in this file, which may also be of another type's method
                use crate::sources::{declares_top_level_fun, owner_at};
                let here = sources.file(&d.file);
                let is_method = here.is_some_and(|f| owner_at(&f.ast().items, byte_offset(&f.text, pl, pc) as u32).is_some());
                let unique = !is_method && sources.files.iter().filter(|f| declares_top_level_fun(&f.ast().items, &fname)).count() <= 1;
                for file in sources.files.iter().filter(|f| unique || sources.elsewhere(&f.path, &d.file).is_none()) {
                    let other = sources.elsewhere(&file.path, &d.file);
                    for (cl, cline) in file.lines().iter().enumerate() {
                        for (start, args) in calls_of(cline, &fname, is_method) {
                            let in_class = || owner_at(&file.ast().items, byte_offset(&file.text, cl as u32, start as u32) as u32).is_some();
                            if let Some((ac, atext)) = args.get(index).filter(|_| is_method || !in_class()) {
                                if atext.starts_with('&') && !atext.starts_with("&mut ") && (other.is_some() || cl as u32 != pl) {
                                    let c = (start + ac) as u32;
                                    edits.push(Edit { file: other.clone(), ..edit(cl as u32, c, c + 1, "&mut ") });
                                }
                            }
                        }
                    }
                }
                d.notes.push(help(&format!("to change `{}`, `{}` must ask for a mutable reference: `{}: &mut ...`.", name, fname, name)));
                d.fixes.push(Fix { title: format!("let `{}` change `{}` (`&mut`)", fname, name), edits });
            }
        }
        return;
    }
    if d.message.contains("not declared as mutable") {
        if find_decl(lines, &name, d.line, "val").is_none() && param_mut(d, lines, &name) {
            mut_self_call(d, sources, d.line, d.col, &name);
            return;
        }
        rewrite(d, format!("`{}` is a `val`, so it can't be changed", name));
        mut_self_call(d, sources, d.line, d.col, &name);
        make_var(d, lines, &name);
    }
}

/// `name` is a parameter (not a reference) that its function changes: to change the caller's
/// value, it asks for `&mut`, which callers lend automatically.
fn param_mut(d: &mut MappedDiag, lines: &[&str], name: &str) -> bool {
    let Some((pl, pc, fname, index)) = param_decl(lines, name, d.line) else { return false };
    let Some((_, _, _, ty)) = fun_param(lines, &fname, index).filter(|(l, c, ..)| (*l, *c) == (pl, pc)) else { return false };
    if ty.starts_with('&') || ty.starts_with('*') || ty.is_empty() {
        return false;
    }
    rewrite(d, format!("`{}` is a parameter, so `{}` can't change it", name, fname));
    d.notes.retain(|n| !n.contains("`mut ") && !n.contains("make this binding mutable"));
    for s in d.secondary.iter_mut() {
        if s.text.contains("help: consider") {
            s.text = String::new();
        }
    }
    d.notes.push(help(&format!(
        "to change the caller's value, `{}` must ask for a mutable reference: `{}: &mut {}`. Callers then lend theirs: `{}(x)` passes `&mut x`.",
        fname, name, ty, fname
    )));
    d.fixes.push(Fix { title: format!("let `{}` change `{}` (`&mut`)", fname, name), edits: vec![edit(pl, pc, pc, "&mut ")] });
    true
}

/// `name` is the variable of a loop that reads its list (`for (x in items)`): to change the
/// elements, the loop goes over `&mut items`. Oxie does that itself when the body assigns to
/// `x.field` or calls a list method like `push` on it; not for the methods of a class.
fn loop_over_mut(d: &mut MappedDiag, lines: &[&str], name: &str) -> bool {
    let header = format!("for ({} in ", name);
    for l in (0..=d.line).rev() {
        let line = lines.get(l as usize).copied().unwrap_or("");
        let Some(at) = line.find(&header) else {
            // the loop must be in the same function
            if find_word(line, "fun", 0).is_some() {
                return false;
            }
            continue;
        };
        let start = at + header.len();
        let list: String = line[start..].chars().take_while(|c| is_ident_char(*c) || *c == '.').collect();
        // `for (x in items.iter())`: `iter_mut()` lends the elements for changing
        if let Some(recv) = list.strip_suffix(".iter").filter(|_| line[start + list.len()..].starts_with("())")) {
            let col = line[..start + list.len()].chars().count() as u32;
            d.notes.retain(|n| !n.contains("iter_mut()"));
            d.notes.push(help(&format!("`iter()` reads the elements of `{}`; `iter_mut()` lets the loop change them.", recv)));
            d.fixes.push(Fix { title: "use `iter_mut()`".into(), edits: vec![edit(l, col, col, "_mut")] });
            return true;
        }
        if list.is_empty() || !line[start + list.len()..].starts_with(')') || list.ends_with('.') {
            return false;
        }
        // `for (row in grid) { for (x in row) ... }`: `row` is an element the outer loop lends,
        // so that loop must lend it for changing
        if !list.contains('.') && list != name {
            let mut outer = MappedDiag { line: l, ..d.clone() };
            outer.notes.clear();
            outer.fixes.clear();
            if loop_over_mut(&mut outer, lines, &list) && !outer.fixes.is_empty() {
                d.notes.extend(outer.notes);
                d.fixes.extend(outer.fixes);
                return true;
            }
        }
        let col = line[..start].chars().count() as u32;
        d.notes.push(help(&format!("the loop reads the elements of `{}`; to change them, loop over `&mut {}`.", list, list)));
        d.fixes.push(Fix { title: format!("loop over `&mut {}`", list), edits: vec![edit(l, col, col, "&mut ")] });
        return true;
    }
    false
}

/// The class of the variable `name` used at `line`: the type its `val`/`var` declares or the
/// class its initializer constructs (`val c = Counter()`), or the type of a parameter `name`.
fn local_class(lines: &[&str], name: &str, line: u32) -> Option<String> {
    let decl = declaration(lines, name, line)?;
    let after = decl.strip_prefix(':').or_else(|| decl.strip_prefix('='))?;
    let ty: String = after.trim_start().trim_start_matches('&').trim_start_matches("mut ").chars().take_while(|c| is_ident_char(*c)).collect();
    ty.chars().next().is_some_and(char::is_uppercase).then_some(ty)
}

/// The element type of the list `name` used at `line`: from the `Vec<T>` or `[T]` its `val`/`var`
/// or parameter declares, or `i32`/`f64` for a `vec[...]` of number literals.
fn element_type(lines: &[&str], name: &str, line: u32) -> Option<String> {
    let decl = declaration(lines, name, line)?;
    if let Some(ty) = decl.strip_prefix(':') {
        let ty = ty.trim_start().trim_start_matches('&').trim_start_matches("mut ").trim_start();
        let inner = ty.strip_prefix("Vec<").or_else(|| ty.strip_prefix('['))?;
        return Some(inner.chars().take_while(|c| is_ident_char(*c)).collect());
    }
    let items: Vec<&str> = decl.strip_prefix('=')?.trim_start().strip_prefix("vec[")?.split(']').next()?.split(',').map(str::trim).collect();
    let digits = |s: &str| !s.is_empty() && s.chars().all(|c| c.is_ascii_digit() || c == '_');
    let int = |s: &str| digits(s.strip_prefix('-').unwrap_or(s));
    let float = |s: &str| s.strip_prefix('-').unwrap_or(s).split_once('.').is_some_and(|(a, b)| digits(a) && digits(b));
    if items.iter().all(|s| int(s)) {
        Some("i32".into())
    } else {
        items.iter().all(|s| float(s)).then(|| "f64".into())
    }
}

/// What follows the name in the declaration of the variable or parameter `name` used at `line`:
/// `: Type = ...` or `= value` for a `val`/`var`, `: Type...` for a parameter.
fn declaration(lines: &[&str], name: &str, line: u32) -> Option<String> {
    match [find_decl(lines, name, line, "val"), find_decl(lines, name, line, "var")].into_iter().flatten().max() {
        Some((l, c)) => {
            let rest: String = lines[l as usize].chars().skip(c as usize + 3).collect();
            Some(rest.trim_start().strip_prefix(name)?.trim_start().to_string())
        }
        None => {
            let (pl, pc, ..) = param_decl(lines, name, line)?;
            Some(format!(": {}", lines[pl as usize].chars().skip(pc as usize).collect::<String>()))
        }
    }
}

/// `name.method(` at `col` of the line: the method's name.
fn method_called_on(line: &str, col: u32, name: &str) -> Option<String> {
    let rest: String = line.chars().skip(col as usize).collect();
    let after = rest.strip_prefix(name)?.strip_prefix('.')?;
    let m: String = after.chars().take_while(|c| is_ident_char(*c)).collect();
    (!m.is_empty() && after[m.len()..].starts_with('(')).then_some(m)
}

/// When `name.method()` at this position calls a method that Oxie made `&mut self`, says why
/// the call changes `name`: a label where the method changes its object, and a note. Only when
/// the class of `name` is known (a constructor call or a declared type), so a method of another
/// type with the same name isn't blamed.
fn mut_self_call(d: &mut MappedDiag, sources: &Sources, line: u32, col: u32, name: &str) -> bool {
    let Some(here) = sources.file(&d.file) else { return false };
    let lines = here.lines();
    let Some(m) = method_called_on(lines.get(line as usize).copied().unwrap_or(""), col, name) else { return false };
    let class = if name == "this" || name == "self" { crate::sources::owner_at(&here.ast().items, byte_offset(&here.text, line, col) as u32) } else { local_class(&lines, name, line) };
    let Some((file, cause, why)) = class.and_then(|c| sources.mut_self(&m, &c)) else { return false };
    if let Some(c) = cause {
        let idx = oxie_compiler::LineIndex::new(&file.text);
        let (l, cc) = idx.line_col(&file.text, c.start as usize);
        let (el, ec) = idx.line_col(&file.text, c.end as usize);
        let end_col = if el == l { ec as u32 } else { cc as u32 + 1 };
        d.secondary.push(Label { file: sources.elsewhere(&file.path, &d.file), line: l as u32, col: cc as u32, end_col, text: format!("`{}` changes its object here, so it takes `&mut self`", m), exact: true });
    }
    d.notes.push(help(&format!("calling `{}.{}()` changes `{}`: Oxie made `{}` take `&mut self` because {}.", name, m, name, m, why)));
    true
}

fn make_var(d: &mut MappedDiag, lines: &[&str], name: &str) {
    d.notes.retain(|n| !n.contains("`mut ") && !n.contains("make this binding mutable"));
    for s in d.secondary.iter_mut() {
        if s.text.contains("first assignment") || s.text.contains("help: consider") {
            s.text = "declared with `val` here".into();
            s.exact = true;
        }
    }
    if let Some((l, c)) = find_decl(lines, name, d.line, "val") {
        if !d.secondary.iter().any(|s| s.line == l) {
            d.secondary.push(Label { file: None, line: l, col: c, end_col: c + 3, text: "declared with `val` here".into(), exact: true });
        }
        d.notes.push(help(&format!("declare it with `var` to allow changes: `var {} = ...`.", name)));
        d.fixes.push(Fix { title: format!("declare `{}` with `var`", name), edits: vec![edit(l, c, c + 3, "var")] });
    }
}

fn borrow_conflict(d: &mut MappedDiag, sources: &Sources, lines: &[&str], name: &str, code: &str) {
    refine(d, lines, name);
    // `*self` in a method is `this`
    let is_this = name == "*self" || name == "self";
    let name = if is_this { "this" } else { name };
    // changed (borrowed for changing) while a shared borrow is still in use
    let changed_while_read = code == "E0502" && d.message.contains("as mutable because it is also borrowed as immutable");
    let relabel = |t: &str| -> String {
        let t = t.to_string();
        if t.contains("immutable borrow later used") || t.contains("borrow later used") || t.contains("first borrow later used") {
            "...and is still used here".into()
        } else if t.contains("immutable borrow occurs") {
            format!("`{}` is borrowed here...", name)
        } else if t.contains("first mutable borrow occurs") {
            format!("`{}` is borrowed for changing here...", name)
        } else if t.contains("second mutable borrow occurs") {
            "...and borrowed for changing again here".into()
        } else if t.contains("mutable borrow occurs") {
            format!("`{}` is changed here", name)
        } else if t.contains("is borrowed here") || t.contains("borrow of") {
            format!("`{}` is borrowed here...", name)
        } else if t.contains("is assigned to here") {
            format!("`{}` is changed here", name)
        } else if t.contains("move out of") {
            format!("`{}` is moved here", name)
        } else {
            t
        }
    };
    if let Some(l) = d.label.clone() {
        d.label = Some(relabel(&l));
    }
    for s in d.secondary.iter_mut() {
        let t = relabel(&s.text);
        if t != s.text {
            s.text = t;
            s.exact = true;
            if s.text.contains(&format!("`{}`", name)) {
                refine_label(s, lines, name);
            }
        }
    }
    let headline = match code {
        "E0502" if changed_while_read => format!("`{}` can't be changed while it is borrowed", name),
        "E0502" => format!("`{}` can't be read while it is being changed", name),
        "E0499" => format!("`{}` is already borrowed for changing", name),
        "E0505" => format!("`{}` can't be moved while it is borrowed", name),
        _ => format!("`{}` can't be changed while it is borrowed", name),
    };
    rewrite(d, headline);
    // one span borrowed and still used: one label
    let used: Vec<(u32, u32)> = d.secondary.iter().filter(|s| s.text.contains("borrowed here...")).map(|s| (s.line, s.col)).collect();
    d.secondary.retain(|s| !(s.text == "...and is still used here" && used.contains(&(s.line, s.col))));
    for s in d.secondary.iter_mut().filter(|s| s.text.ends_with("borrowed here...") && used.contains(&(s.line, s.col))) {
        s.text = format!("`{}` is borrowed here, and still used", name);
    }
    // a loop over a field of `this` whose body changes `this`: loop over a copy
    if is_this {
        let found = d.secondary.iter().filter(|s| s.text.contains("borrowed")).find_map(|s| loop_field(lines.get(s.line as usize).copied().unwrap_or("")).map(|f| (s.line, f)));
        if let Some((l, (end, field))) = found {
            d.notes.retain(|n| !n.contains("index-based loop"));
            d.notes.push(help(&format!(
                "the loop goes through `{}`, which belongs to `this`, while its body changes `this` (and so maybe `{}`). Loop over a copy, or collect the changes and make them after the loop.",
                field, field
            )));
            d.fixes.push(Fix { title: format!("loop over a copy of `{}`", field), edits: vec![edit(l, end, end, ".clone()")] });
            return;
        }
    }
    // shapes with a change that costs nothing: a map's entry API, a loop over `iter_mut()`
    if changed_while_read && (entry_instead(d, sources) || iter_mut_instead(d, sources, lines)) {
        return;
    }
    // a change made by calling a method that Oxie made `&mut self`: say why it changes the value
    let changes = |t: &str| t.contains("is changed here") || t.contains("for changing");
    let mut at: Vec<(u32, u32)> = d.label.as_deref().filter(|l| changes(l)).map(|_| (d.line, d.col)).into_iter().collect();
    at.extend(d.secondary.iter().filter(|s| s.file.is_none() && changes(&s.text)).map(|s| (s.line, s.col)));
    let receivers: &[&str] = if is_this { &["this", "self"] } else { &[name] };
    'calls: for (l, c) in at {
        for r in receivers {
            if mut_self_call(d, sources, l, c, r) {
                break 'calls;
            }
        }
    }
    d.notes.push(help(&format!(
        "a reference to `{}` is still in use. Rust doesn't let a value change (or move) while references to it exist, because the change could invalidate them. Finish using the reference first, or copy the data out (`.clone()`) instead of borrowing it.",
        name
    )));
    // `val x = &expr` -> `val x = expr.clone()`
    for s in d.secondary.iter() {
        if !s.text.contains("borrowed here") {
            continue;
        }
        let line = lines.get(s.line as usize).copied().unwrap_or("");
        let trimmed = line.trim_start();
        if !(trimmed.starts_with("val ") || trimmed.starts_with("var ")) {
            continue;
        }
        let Some(eq) = line.find(" = &") else { continue };
        let amp = line[..eq + 3].chars().count() as u32;
        let expr = line[eq + 4..].split("//").next().unwrap_or("").trim_end();
        if expr.is_empty() || expr.starts_with("mut ") {
            continue;
        }
        let end = amp + 1 + expr.chars().count() as u32;
        d.fixes.push(Fix { title: "copy the value instead of borrowing it".into(), edits: vec![edit(s.line, amp, amp + 1, ""), edit(s.line, end, end, ".clone()")] });
        break;
    }
}

/// `if (val v = m.get(&k)) return v` right before `m.insert(k, value)`, on a `HashMap` or
/// `BTreeMap` parameter or field: returning the reference keeps `m` borrowed after the `if` too,
/// since rustc doesn't tell the path that returned from the one that goes on. The entry API looks
/// the key up once, but takes an owned key: a key the insert converts (`k.to_string()`) would be
/// made on every call, which the explanation says. When the key isn't converted and the next
/// statement is `return m.get(&k)!!`, the fix replaces the three.
fn entry_instead(d: &mut MappedDiag, sources: &Sources) -> bool {
    let Some(here) = sources.file(&d.file) else { return false };
    let src = here.text.as_str();
    let Some(borrow) = d.secondary.iter().find(|s| s.text.contains("is borrowed here")) else { return false };
    if !d.secondary.iter().any(|s| s.text.starts_with("returning this value requires")) {
        return false;
    }
    let (get_at, insert_at) = (byte_offset(src, borrow.line, borrow.col) as u32, byte_offset(src, d.line, d.col) as u32);
    let text = |s: Span| src.get(s.start as usize..s.end as usize).unwrap_or("").to_string();
    // the key a lookup or an insert names: `id` for `&id` and for `id.clone()`
    let key = |s: Span| {
        let k = text(s);
        let k = k.trim_start_matches('&');
        [".clone()", ".to_string()", ".to_owned()"].iter().fold(k, |k, m| k.strip_suffix(m).unwrap_or(k)).to_string()
    };
    // `m.get(k)`, alone or unwrapped (`!!`, `.unwrap()`): (map, key)
    let lookup = |e: &Expr| match &e.kind {
        ExprKind::MethodCall { recv, name, args, .. } if name == "get" && args.len() == 1 => Some((text(recv.span), key(args[0].value.span))),
        _ => None,
    };
    let unwrapped = |e: &Expr| match &e.kind {
        ExprKind::Unwrap(g) => lookup(g),
        ExprKind::MethodCall { recv, name, args, .. } if name == "unwrap" && args.is_empty() => lookup(recv),
        _ => None,
    };
    // the `if` and its lookup; the insert, its map, key, key text, value text and whether the value
    // can run in a lambda; the unwrapped lookups that may follow (their span, whether they return)
    let (mut first, mut insert, mut after) = (None, None, Vec::new());
    oxie_compiler::visit::walk_items(&here.ast().items, &mut |e| {
        match &e.kind {
            ExprKind::If { cond, then, else_: None } => {
                if let Cond::Let { pat: Pat::Ident { name: v, .. }, expr, implicit_some: true } = &**cond {
                    let returns_v = matches!(&then.stmts[..], [Stmt::Expr { expr: Expr { kind: ExprKind::Return(Some(r)), .. }, .. }] if r.ident() == Some(v.as_str()));
                    if expr.span.start == get_at && returns_v {
                        first = lookup(expr).map(|l| (e.span, l));
                    }
                }
            }
            ExprKind::MethodCall { recv, name, args, .. } if name == "insert" && args.len() == 2 && e.span.start == insert_at => {
                let (k, value) = (&args[0].value, &args[1].value);
                insert = Some((e.span, text(recv.span), key(k.span), text(k.span), text(value.span), lambda_body_ok(value, &text(recv.span))));
            }
            ExprKind::Return(Some(r)) => after.extend(unwrapped(r).map(|l| (e.span, true, l))),
            _ => after.extend(unwrapped(e).map(|l| (e.span, false, l))),
        }
        true
    });
    let (Some((if_span, (map, k))), Some((call, insert_map, insert_key, key_text, value, value_ok))) = (first, insert) else { return false };
    // nothing but blank space, `;` and (unless `strict`) comments between two statements
    let gap = |a: u32, b: u32, strict: bool| src.get(a as usize..b as usize).is_some_and(|t| t.lines().all(|l| (if strict { l } else { code(l) }).chars().all(|c| c.is_whitespace() || c == ';')));
    let Some(is_param) = map_declared(here.ast(), insert_at, &map) else { return false };
    if map != insert_map || k != insert_key || !gap(if_span.end, call.start, false) {
        return false;
    }
    let entry = format!("{}.entry({}).or_insert_with {{ {} }}", map, key_text, value);
    for s in d.secondary.iter_mut().filter(|s| s.text.starts_with("returning this value requires")) {
        s.text = format!("returned here, so `{}` stays borrowed for the rest of the function", map);
    }
    // the insert makes the map's key from the one looked up (`key.to_string()`, `key.clone()`)
    let converted = key_text != k;
    let advice = if converted {
        format!(
            "The entry API looks the key up once and inserts only when it is missing: `{}`. It needs an owned key for every lookup, though, so `{}` then runs on every call, even when the key is already there: a trade-off, not a change that costs nothing.",
            entry, key_text
        )
    } else {
        format!("Look the key up once with the entry API, which inserts only when the key is missing: `{}`.", entry)
    };
    d.notes.push(help(&format!(
        "returning the reference from `{}.get()` keeps `{}` borrowed until the function returns, even on the path where the key is missing, so `{}.insert()` can't change it. {}",
        map, map, map, advice
    )));
    // the lookup that ends the function, after the insert (the fix keeps comments, so there are
    // none in between); the value runs in a lambda while the map parameter is borrowed (a field's
    // lambda could need `this`)
    let tail = |s: Span, ret: bool| ret || src[s.end as usize..].trim_start().starts_with('}');
    let last = after.into_iter().find(|(s, ret, l)| gap(call.end, s.start, true) && tail(*s, *ret) && *l == (map.clone(), k.clone()));
    if let Some((s, ret, _)) = last.filter(|_| !converted && is_param && value_ok && !value.contains('\n') && gap(if_span.end, call.start, true)) {
        let edit = span_edit(src, Span { start: if_span.start, end: s.end }, &format!("{}{}", if ret { "return " } else { "" }, entry));
        d.fixes.push(Fix { title: format!("look `{}` up once with `{}.entry()`", k, map), edits: vec![edit] });
    }
    true
}

/// Can `e` be the body of a lambda that runs while `map` is borrowed: it doesn't name the map,
/// use `it` or leave the function.
fn lambda_body_ok(e: &Expr, map: &str) -> bool {
    let mut ok = true;
    oxie_compiler::visit::walk_expr(e, &mut |x| {
        let leaves = matches!(x.kind, ExprKind::Return(_) | ExprKind::Throw(_) | ExprKind::Try(_) | ExprKind::Await(_) | ExprKind::Break { .. } | ExprKind::Continue(_));
        ok &= !leaves && !matches!(x.ident(), Some(n) if n == map || n == "it");
        ok
    });
    ok
}

/// Whether `recv` (`m` or `this.m`, read at `offset`) is declared as a `HashMap` or `BTreeMap`:
/// `Some(true)` for a parameter of the function, `Some(false)` for a field of its class.
fn map_declared(file: &File, offset: u32, recv: &str) -> Option<bool> {
    let (f, class) = fun_at(&file.items, offset)?;
    let field = recv.strip_prefix("this.");
    let param = f.params.iter().find(|p| field.is_none() && p.name() == Some(recv)).map(|p| &p.ty);
    let ty = param.or_else(|| class?.fields.iter().find(|x| x.name == field.unwrap_or(recv)).map(|x| &x.ty))?;
    let ty = match ty {
        Type::Ref { inner, .. } => inner,
        t => t,
    };
    matches!(ty.last_name(), Some("HashMap" | "BTreeMap")).then_some(param.is_some())
}

/// The function or method whose declaration holds `offset`, and its class.
fn fun_at(items: &[Item], offset: u32) -> Option<(&FunDecl, Option<&TypeDecl>)> {
    let it = items.iter().find(|it| it.span.start <= offset && offset < it.span.end)?;
    match &it.kind {
        ItemKind::Fun(f) => Some((f, None)),
        ItemKind::Type(t) => fun_at(&t.members, offset).map(|(f, _)| (f, Some(t))),
        ItemKind::Impl(i) => fun_at(&i.members, offset),
        ItemKind::Mod { items, .. } | ItemKind::Object { items, .. } => fun_at(items, offset),
        _ => None,
    }
}

/// `v[i] = ...` in a loop over `v.iter().enumerate()` whose body uses `v` only as `v[i]`: the loop
/// reads `v` while the body changes it. Changing each element through `v.iter_mut()` is allowed
/// and costs nothing. The fix rewrites a body of one statement when the elements are numbers.
fn iter_mut_instead(d: &mut MappedDiag, sources: &Sources, lines: &[&str]) -> bool {
    let Some(here) = sources.file(&d.file) else { return false };
    let src = here.text.as_str();
    let Some(borrow) = d.secondary.iter().find(|s| s.text.contains("is borrowed here")) else { return false };
    let (iter_at, change_at) = (byte_offset(src, borrow.line, borrow.col) as u32, byte_offset(src, d.line, d.col) as u32);
    let within = |s: Span, at: u32| s.start <= at && at < s.end;
    let mut found = None;
    oxie_compiler::visit::walk_items(&here.ast().items, &mut |e| {
        if let ExprKind::For { pat: Pat::Tuple(ps), iter, body, .. } = &e.kind {
            if within(iter.span, iter_at) && within(body.span, change_at) {
                found = indexed_loop(src, ps, iter, body);
            }
        }
        found.is_none()
    });
    let Some(l) = found else { return false };
    d.notes.retain(|n| !n.contains("index-based loop"));
    let example = match &l.stmt {
        Some(s) => format!("for ({}) {{ {} }}", l.head, s),
        None => format!("for ({}) {{ *{} = ... }}", l.head, l.elem),
    };
    d.notes.push(help(&format!(
        "`{}.iter()` borrows `{}` for the whole loop, so the body can't change `{}[{}]`, even though it is the element the loop is at. Loop over `{}.iter_mut()` instead, which lets the body change each element through `{}`: `{}`.",
        l.list, l.list, l.list, l.index, l.list, l.elem, example
    )));
    let numbers = l.list.chars().all(is_ident_char) && element_type(lines, &l.list, d.line).is_some_and(|t| is_copy_type(&t));
    if l.stmt.is_some() && numbers {
        let edits = l.edits.iter().map(|(s, t)| span_edit(src, *s, t)).collect();
        d.fixes.push(Fix { title: format!("change the elements through `{}.iter_mut()`", l.list), edits });
    }
    true
}

/// A loop `for ((i, x) in v.iter().enumerate())` whose body changes `v[i]` (see `iter_mut_instead`).
struct IndexedLoop {
    list: String,
    index: String,
    elem: String,
    /// The loop's head over `v.iter_mut()`, enumerated when the body uses `i` apart from `v[i]`.
    head: String,
    /// The edits that make the loop change the elements through `x`.
    edits: Vec<(Span, String)>,
    /// The body's one statement after the edits, when each read of `x` became `*x` or stayed the
    /// receiver of a method call.
    stmt: Option<String>,
}

/// The loop over `iter` when it is `v.iter().enumerate()` into `(i, x)` and the body uses `v`
/// only as `v[i]`, and assigns only to `v[i]`.
fn indexed_loop(src: &str, ps: &[Pat], iter: &Expr, body: &Block) -> Option<IndexedLoop> {
    let [Pat::Ident { name: i, span: i_span, .. }, Pat::Ident { name: x, span: x_span, .. }] = ps else { return None };
    let ExprKind::MethodCall { recv: iter_call, name: enumerate, args, .. } = &iter.kind else { return None };
    let ExprKind::MethodCall { recv: list, name: iter_name, args: iter_args, .. } = &iter_call.kind else { return None };
    if enumerate != "enumerate" || iter_name != "iter" || !args.is_empty() || !iter_args.is_empty() {
        return None;
    }
    let text = |s: Span| src.get(s.start as usize..s.end as usize).unwrap_or("");
    let v = text(list.span);
    let is_elem = |e: &Expr| matches!(&e.kind, ExprKind::Index(l, ix) if text(l.span) == v && ix.ident() == Some(i.as_str()));
    let is_x = |e: &Expr| e.ident() == Some(x.as_str());
    // what becomes `*x`: each `v[i]`, and `x` read as an operand; the reads of `x` handled so
    // (or left as they are), and all of them
    let (mut derefs, mut handled, mut reads, mut writes) = (Vec::new(), 0, 0, 0);
    let (mut index_used, mut other) = (false, false);
    for s in &body.stmts {
        oxie_compiler::visit::walk_stmt(s, &mut |e| {
            match &e.kind {
                _ if is_elem(e) => {
                    derefs.push(e.span);
                    return false;
                }
                ExprKind::Assign(_, target, value) => {
                    if is_elem(target) {
                        writes += 1;
                    } else {
                        other = true;
                    }
                    if is_x(value) {
                        derefs.push(value.span);
                        handled += 1;
                    }
                }
                ExprKind::Binary(_, a, b) => {
                    for o in [a, b].into_iter().filter(|o| is_x(o)) {
                        derefs.push(o.span);
                        handled += 1;
                    }
                }
                ExprKind::Unary(UnOp::Neg, a) if is_x(a) => {
                    derefs.push(a.span);
                    handled += 1;
                }
                ExprKind::Unary(UnOp::Deref, a) | ExprKind::MethodCall { recv: a, .. } if is_x(a) => handled += 1,
                _ if is_x(e) => reads += 1,
                _ if e.ident() == Some(i.as_str()) => index_used = true,
                _ if text(e.span) == v => other = true,
                _ => {}
            }
            true
        });
    }
    // the pattern `(i, x) in ` before the iterable
    let open = src[..i_span.start as usize].rfind('(')?;
    let close = x_span.end as usize + src[x_span.end as usize..].find(')')?;
    let blank = |a: usize, b: usize, t: &str| src.get(a..b).is_some_and(|s| s.trim() == t);
    let pattern = blank(open + 1, i_span.start as usize, "") && blank(i_span.end as usize, x_span.start as usize, ",") && blank(x_span.end as usize, close, "") && blank(close + 1, iter.span.start as usize, "in");
    if other || writes == 0 || !pattern {
        return None;
    }
    let head = if index_used { format!("({}, {}) in {}.iter_mut().enumerate()", i, x, v) } else { format!("{} in {}.iter_mut()", x, v) };
    let stmt = match &body.stmts[..] {
        [Stmt::Expr { expr, .. }] if handled == reads => {
            let mut t = text(expr.span).to_string();
            let mut ds: Vec<&Span> = derefs.iter().collect();
            ds.sort_by_key(|s| std::cmp::Reverse(s.start));
            for s in ds {
                t.replace_range((s.start - expr.span.start) as usize..(s.end - expr.span.start) as usize, &format!("*{}", x));
            }
            (!t.contains('\n')).then_some(t)
        }
        _ => None,
    };
    let mut edits = vec![(Span::new(open, iter.span.end as usize), head.clone())];
    edits.extend(derefs.into_iter().map(|s| (s, format!("*{}", x))));
    Some(IndexedLoop { list: v.to_string(), index: i.clone(), elem: x.clone(), head, edits, stmt })
}

/// A value that isn't `Send`/`Sync` used from another thread: usually a `shared class` handle.
fn thread_safety(d: &mut MappedDiag, sources: &Sources, ty: &str) {
    let future = d.message.starts_with("future cannot be");
    // for a future, the type is in a note: "the trait `Send` is not implemented for `Rc<..>`"
    let noted = d.notes.iter().find_map(|n| n.split("is not implemented for `").nth(1).and_then(|t| t.rsplit_once('`')).map(|(t, _)| t.to_string()));
    let ty = if ty.is_empty() { noted.as_deref().unwrap_or("") } else { ty };
    d.notes.retain(|n| !n.contains("full name for the type") && !n.contains("--verbose") && !n.contains("{closure@") && !n.contains("required because it's used within"));
    let class = ty.strip_prefix("Rc<RefCell<").and_then(|t| t.strip_suffix("Data>>"));
    // `shared class C` in some file of the crate: (the file unless it's this one, line, column)
    let decl_in = |lines: &[&str], c: &str| -> Option<(u32, u32)> {
        let pat = format!("shared class {}", c);
        lines.iter().enumerate().find_map(|(l, t)| t.find(&pat).filter(|i| t[i + pat.len()..].chars().next().is_none_or(|c| !is_ident_char(c))).map(|i| (l as u32, t[..i].chars().count() as u32)))
    };
    let decl = class.and_then(|c| {
        let file = sources.declaring(&d.file, |ls| decl_in(ls, c).is_some())?;
        let (l, col) = decl_in(&file.lines(), c)?;
        Some((sources.elsewhere(&file.path, &d.file), l, col))
    });
    match (class, decl) {
        (Some(c), Some((file, l, col))) => {
            if future {
                rewrite(d, format!("this task can't move between threads: it holds a `{}` across an `await`", c));
                d.notes.retain(|n| !n.contains("is not implemented for"));
            } else {
                rewrite(d, format!("`{}` can't be used from another thread", c));
                d.label = Some(format!("a `{}` handle goes to another thread here", c));
            }
            d.notes.push(help(&format!(
                "a `shared class` handle (`Rc<RefCell<..>>`) belongs to one thread. Declare it `@sync shared class {}`, which uses `Arc<Mutex<..>>` so threads can share it.",
                c
            )));
            d.fixes.push(Fix { title: format!("make `{}` usable from several threads (`@sync`)", c), edits: vec![Edit { file, ..edit(l, col, col, "@sync ") }] });
        }
        _ if future => {
            rewrite(d, "this task can't move between threads: it holds a value across an `await` that belongs to one thread".into());
            d.notes.push(help("a value that belongs to one thread (an `Rc`, a `RefCell`, or a `shared class` without `@sync`) is still alive at an `await`, and the task may continue on another thread. End its use before the `await` (put it in a block), or use `Arc`, `Mutex` or a `@sync shared class`."));
        }
        _ if ty.starts_with("Rc<") => {
            d.notes.push(help("`Rc` counts its references without synchronization, so it can't cross threads. Use `Arc` (with a `Mutex` to change the value), or a `@sync shared class`."));
        }
        _ => {}
    }
}

/// `for (x in items)` / `for (x in this.items)`: the column after the iterable and its name.
fn loop_field(line: &str) -> Option<(u32, String)> {
    let start = line.find("for (")?;
    let rest = &line[start..];
    let in_at = start + rest.find(" in ")? + 4;
    let iter = &line[in_at..];
    let name_at = in_at + if iter.starts_with("this.") { 5 } else { 0 };
    let name: String = line[name_at..].chars().take_while(|c| is_ident_char(*c)).collect();
    if name.is_empty() || !line[name_at + name.len()..].starts_with(')') {
        return None;
    }
    Some((line[..name_at + name.len()].chars().count() as u32, name))
}

fn lambda_outlives(d: &mut MappedDiag, lines: &[&str], names: &[String]) {
    let name = names.iter().find(|n| *n != "'static").cloned().unwrap_or_default();
    rewrite(d, format!("the lambda may run after `{}` is gone", name));
    d.label = Some(format!("this lambda borrows `{}`", name));
    for s in d.secondary.iter_mut() {
        if s.text.contains("is borrowed here") {
            s.text = format!("`{}` is used here", name);
            s.exact = true;
            refine_label(s, lines, &name);
        }
    }
    d.notes.retain(|n| !n.contains("`move ") && !n.contains("move` keyword") && !n.contains("outlive `'static`"));
    d.notes.push(help(&format!(
        "the lambda can run later (for example on another thread), after this function has returned and `{}` is gone. A `move` lambda takes `{}` with it.",
        name, name
    )));
    let line = lines.get(d.line as usize).copied().unwrap_or("");
    let rest: String = line.chars().skip(d.col as usize).collect();
    if rest.starts_with('{') || rest.starts_with('|') || rest.starts_with('(') {
        // `move { }` can't follow a call as a trailing lambda (`spawn { .. }`, `f(x) { .. }`),
        // so it goes inside the parentheses
        let before: String = line.chars().take(d.col as usize).collect();
        let before = before.trim_end();
        let close = if rest.starts_with('{') { matching_brace(lines, d.line, d.col) } else { None };
        let edits = match (close, before.chars().last()) {
            (Some((cl, cc)), Some(')')) => {
                let paren = before.chars().count() as u32 - 1;
                let empty = before[..before.len() - 1].trim_end().ends_with('(');
                vec![edit(d.line, paren, d.col, if empty { "move " } else { ", move " }), edit(cl, cc + 1, cc + 1, ")")]
            }
            (Some((cl, cc)), Some(c)) if is_ident_char(c) || c == '>' => vec![edit(d.line, before.chars().count() as u32, d.col, "(move "), edit(cl, cc + 1, cc + 1, ")")],
            _ => vec![edit(d.line, d.col, d.col, "move ")],
        };
        d.fixes.push(Fix { title: "make it a `move` lambda".into(), edits });
    }
}

/// The `}` that closes the `{` at (line, col), skipping string literals: (line, column).
fn matching_brace(lines: &[&str], line: u32, col: u32) -> Option<(u32, u32)> {
    let (mut depth, mut in_str, mut escaped) = (0, false, false);
    for (l, text) in lines.iter().enumerate().skip(line as usize) {
        let skip = if l == line as usize { col as usize } else { 0 };
        for (c, ch) in text.chars().enumerate().skip(skip) {
            match ch {
                _ if escaped => escaped = false,
                '\\' if in_str => escaped = true,
                '"' => in_str = !in_str,
                '{' if !in_str => depth += 1,
                '}' if !in_str => {
                    depth -= 1;
                    if depth == 0 {
                        return Some((l as u32, c as u32));
                    }
                }
                _ => {}
            }
        }
    }
    None
}

/// Kotlin/Java method names and their Rust equivalents.
const KOTLIN_METHODS: &[(&str, &str)] = &[
    ("size", "len"),
    ("length", "len"),
    ("count", "len"),
    ("isEmpty", "is_empty"),
    ("isNotEmpty", "!is_empty"),
    ("isBlank", "trim().is_empty"),
    ("add", "push"),
    ("addAll", "extend"),
    ("put", "insert"),
    ("containsKey", "contains_key"),
    ("forEach", "for_each"),
    ("toUpperCase", "to_uppercase"),
    ("toLowerCase", "to_lowercase"),
    ("uppercase", "to_uppercase"),
    ("lowercase", "to_lowercase"),
    ("startsWith", "starts_with"),
    ("endsWith", "ends_with"),
    ("removeAt", "remove"),
    ("indexOf", "find"),
    ("toList", "to_vec"),
    ("toMutableList", "to_vec"),
    ("toString", "to_string"),
    ("equals", "eq"),
    ("getOrDefault", "get(..).unwrap_or"),
    ("keySet", "keys"),
    ("toInt", "parse::<i32>"),
    ("toLong", "parse::<i64>"),
    ("toDouble", "parse::<f64>"),
    ("charAt", "chars().nth"),
    ("substring", "get(a..b)"),
    ("joinToString", "join"),
    ("reversed", "rev"),
    ("first", "first"),
];

fn no_method(d: &mut MappedDiag, sources: &Sources, lines: &[&str], method: &str) {
    if !d.message.starts_with("no method named") && !d.message.starts_with("no function or associated item named") {
        return;
    }
    refine(d, lines, method);
    // one of Oxie's list operations on something else: rustc suggests implementing the runtime's
    // trait for it
    if d.notes.iter().any(|n| n.contains("`ListOps` defines an item")) {
        let ty = ticks(&d.message).get(1).cloned().unwrap_or_default();
        d.notes.retain(|n| !n.contains("`ListOps` defines an item") && !n.contains("items from traits can only be used if"));
        if is_iterator(&ty) {
            list_op_on_iterator(d, sources, method, &ty);
        } else {
            d.notes.push(help(&format!("`{}()` is one of Oxie's list operations: lists, arrays and slices have it, but `{}` doesn't.", method, ty)));
        }
    }
    opposite_name(d, sources, method);
    let rust = KOTLIN_METHODS.iter().find(|(k, _)| *k == method).map(|(_, r)| r.to_string()).or_else(|| {
        // only when rustc knows a method by the snake_case name
        let snake = snake_case(method);
        (snake != method && d.notes.iter().any(|n| n.contains(&format!("`{}`", snake)))).then_some(snake)
    });
    if let Some(r) = rust.filter(|r| r != method) {
        let plain = r.chars().all(|c| c.is_alphanumeric() || c == '_');
        d.notes.retain(|n| !n.contains("similar name") && !n.contains("you might have meant"));
        d.notes.push(help(&format!("Rust names this `{}`; standard library methods use Rust's snake_case names.", r)));
        let line = lines.get(d.line as usize).copied().unwrap_or("");
        if plain && word_at(line, d.col as usize) == Some(method) {
            let end = d.col + method.chars().count() as u32;
            d.fixes.push(Fix { title: format!("use `{}`", r), edits: vec![edit(d.line, d.col, end, &r)] });
        }
    }
    imports(d, lines);
}

/// A method of `T` called on a `T?` (rustc: "no method named `trim` found for enum `Option<T>`",
/// or "method `len` is private", which `Option` has too).
fn is_optional_receiver(d: &MappedDiag) -> bool {
    d.message.contains("`Option<") || d.notes.iter().any(|n| n.contains("Option::expect"))
}

/// Explains a method of `T` called on a `T?`. A null check unwraps a local variable or
/// parameter whose type Oxie knows, not a field: such a value checked with `!= null` (or
/// `== null` and `return`) is still optional, so bind it with `if (val x = &x)` or
/// `val x = x ?: return`, or call through `?.`. rustc's `.expect("REASON")` would panic, and take
/// the value.
fn optional_receiver(d: &mut MappedDiag, lines: &[&str], method: &str) {
    refine(d, lines, method);
    let line = lines.get(d.line as usize).copied().unwrap_or("");
    let before: String = line.chars().take(d.col as usize).collect();
    let Some(before) = before.strip_suffix('.') else { return };
    let recv: String = before.chars().rev().take_while(|c| is_ident_char(*c) || *c == '.').collect::<Vec<_>>().into_iter().rev().collect();
    let recv = recv.trim_start_matches('.').to_string();
    if recv.is_empty() {
        return;
    }
    let ty = d.notes.iter().find_map(|n| between(n, "exists on the type `", "`")).map(|t| format!("`{}?`", t)).unwrap_or_else(|| "optional".into());
    rewrite(d, format!("`{}` is {}, so it has no `{}()` until it is unwrapped", recv, if ty == "optional" { ty.clone() } else { format!("optional ({})", ty) }, method));
    d.notes.retain(|n| !n.contains("Option::expect") && !n.contains("exists on the type"));
    d.suggestions.retain(|s| !s.replacement.starts_with(".expect("));
    let plain = recv.chars().all(is_ident_char);
    let short = recv.rsplit('.').next().unwrap_or(&recv).to_string();
    for l in (0..=d.line).rev().take(40) {
        let text = lines.get(l as usize).copied().unwrap_or("");
        let checked = format!("{} != null", recv);
        if let Some(at) = text.find(&format!("if ({})", checked)) {
            // borrowed when the value is used after the check, so it stays usable
            let later = lines.iter().skip(l as usize + 1).take_while(|t| find_word(t, "fun", 0).is_none()).any(|t| find_word(t, &short, 0).is_some() || t.contains(&recv));
            let bound = format!("val {} = {}{}", short, if later || !plain { "&" } else { "" }, recv);
            let why = if plain { format!("Oxie couldn't tell the type of `{}` here, so `{}` didn't unwrap it", recv, checked) } else { format!("`{}` checks a field, and a null check unwraps only local variables and parameters", checked) };
            d.notes.push(help(&format!("{}. Unwrap it in the condition instead: `if ({})`{}.", why, bound, if plain { String::new() } else { format!(", and use `{}`", short) })));
            let col = text[..at + 4].chars().count() as u32;
            let mut edits = vec![edit(l, col, col + checked.chars().count() as u32, &bound)];
            // a field is renamed in the uses on the same line (`u.email` becomes `email`)
            if !plain {
                if l != d.line {
                    return;
                }
                let code = &text[..text[at..].find("//").map_or(text.len(), |c| at + c)];
                let mut from = at + 4 + checked.len();
                while let Some(i) = code[from..].find(&recv) {
                    let c = text[..from + i].chars().count() as u32;
                    edits.push(edit(l, c, c + recv.chars().count() as u32, &short));
                    from += i + recv.len();
                }
            }
            d.fixes.push(Fix { title: format!("unwrap `{}` (`{}`)", recv, bound), edits });
            return;
        }
        let early = format!("if ({} == null) ", recv);
        if let Some(at) = text.find(&early) {
            let exit = text[at + early.len()..].trim_end();
            if ["return", "throw", "continue", "break"].iter().any(|k| exit.starts_with(k)) && !exit.contains("//") {
                let why = if plain { format!("Oxie couldn't tell the type of `{}` here", recv) } else { "a null check unwraps only local variables and parameters".to_string() };
                d.notes.push(help(&format!("after `if ({} == null) {}`, `{}` is still optional ({}). Unwrap it instead: `val {} = {} ?: {}`.", recv, exit, recv, why, short, recv, exit)));
                if plain {
                    let col = text[..at].chars().count() as u32;
                    let end = text.trim_end().chars().count() as u32;
                    d.fixes.push(Fix { title: format!("unwrap `{}` (`?: {}`)", recv, exit.split_whitespace().next().unwrap_or("return")), edits: vec![edit(l, col, end, &format!("val {} = {} ?: {}", recv, recv, exit))] });
                }
                return;
            }
        }
        // the check must be in the same function
        if find_word(text, "fun", 0).is_some() {
            break;
        }
    }
    d.notes.push(help(&format!("`{}?.{}()` calls it when there is a value (and gives an optional result); `if (val {} = {})` unwraps it for a block.", recv, method, short, recv)));
}

/// Iterator types of std, by the name rustc gives them in messages (`std::str::Split<'a, P>`,
/// `Map<I, F>`).
const ITERATORS: &[&str] = &[
    "Iter", "IterMut", "IntoIter", "Drain", "Keys", "Values", "ValuesMut", "IntoKeys", "IntoValues", "Chars", "CharIndices", "Bytes", "Lines",
    "Split", "SplitN", "RSplit", "RSplitN", "SplitTerminator", "SplitInclusive", "SplitWhitespace", "SplitAsciiWhitespace", "Matches",
    "MatchIndices", "Windows", "Chunks", "ChunksExact", "Range", "RangeInclusive", "RangeFrom", "Map", "Filter", "FilterMap", "FlatMap",
    "Flatten", "Enumerate", "Rev", "Zip", "Chain", "Take", "Skip", "TakeWhile", "SkipWhile", "MapWhile", "StepBy", "Peekable", "Cloned",
    "Copied", "Inspect", "Scan", "Fuse", "Cycle", "Args",
];

/// Is a type that rustc names in a message an iterator: one of std's (`Chars<'a>`), or
/// `impl Iterator<Item = i32>` or `Box<dyn Iterator<Item = i32>>`?
fn is_iterator(ty: &str) -> bool {
    let path = ty.split('<').next().unwrap_or(ty);
    let std = !path.contains("::") || ["std::", "core::", "alloc::"].iter().any(|p| path.starts_with(p));
    ty.contains("Iterator<") || std && ITERATORS.contains(&last_segment(path))
}

/// Iterators of std whose items aren't references into a list (`chars()`, `split(' ')`, ranges,
/// `into_iter()`): collected into a list, its elements are what the iterator's items are.
fn values_iterator(ty: &str) -> bool {
    let path = ty.split('<').next().unwrap_or(ty);
    path.starts_with("std::str::") || ["std::ops::Range", "std::ops::RangeInclusive", "std::vec::IntoIter", "Chars", "CharIndices", "SplitWhitespace", "SplitAsciiWhitespace"].contains(&path)
}

/// One of Oxie's list operations called on an iterator (`xs.iter().map_indexed { }`,
/// `s.chars().join_to_string("")`): lists have it, so it is called on the list, or on the
/// iterator collected into one. The similar names rustc suggests do something else (`is_sorted`
/// for `sorted`).
fn list_op_on_iterator(d: &mut MappedDiag, sources: &Sources, method: &str, ty: &str) {
    if let Some(other) = d.notes.iter().find(|n| n.contains(" with a similar name")).and_then(|n| ticks(n).first().cloned()) {
        d.notes.retain(|n| !n.contains(" with a similar name"));
        d.suggestions.retain(|s| s.replacement != other);
    }
    // rustc's "method `sorted` is available on `std::slice::Iter<'_, i32>`" for an earlier
    // receiver of the chain, an iterator too
    d.secondary.retain(|s| !s.text.starts_with(&format!("method `{}` is available on", method)));
    d.notes.push(help(&format!(
        "`{m}()` is one of Oxie's list operations: lists, arrays and slices have it, but `{ty}` is an iterator. Call it on the list (`xs.{m}()` rather than `xs.iter().{m}()`), or collect the iterator into a list first (`.collect::<Vec<_>>().{m}()`).",
        m = method,
        ty = ty
    )));
    let Some(here) = sources.file(&d.file) else { return };
    let src = here.text.as_str();
    let at = byte_offset(src, d.line, d.col);
    if !src[at..].starts_with(method) {
        return;
    }
    // where the call's receiver ends, and where the list ends when the receiver is `list.iter()`
    let mut found = None;
    oxie_compiler::visit::walk_items(&here.ast().items, &mut |e| {
        if let ExprKind::MethodCall { recv, name, safe: false, .. } = &e.kind {
            if name == method && src.get(recv.span.end as usize..at).is_some_and(|t| t.trim() == ".") {
                let list = match &recv.kind {
                    ExprKind::MethodCall { recv: list, name, generics: None, args, safe: false } if name == "iter" && args.is_empty() => Some(list.span.end),
                    _ => None,
                };
                found = Some((recv.span.end, list));
            }
        }
        found.is_none()
    });
    let Some((end, list)) = found else { return };
    match list {
        // `iter()` of a list, array or slice: the operation's lambdas get the same `&T` on the list
        Some(list) if ty.starts_with("std::slice::Iter<") => {
            d.fixes.push(Fix { title: format!("call `{}()` on the list", method), edits: vec![span_edit(src, Span::new(list as usize, end as usize), "")] });
        }
        // a list collected from it is a temporary, which the operations that return a reference
        // into the list would outlive
        _ if values_iterator(ty) && !["find", "find_last", "max_by_key", "min_by_key"].contains(&method) => {
            d.fixes.push(Fix { title: "collect the iterator into a list first".into(), edits: vec![span_edit(src, Span::new(end as usize, end as usize), ".collect::<Vec<_>>()")] });
        }
        _ => {}
    }
}

/// Words of method names that mean the opposite of each other.
const OPPOSITE_WORDS: &[(&str, &str)] = &[
    ("some", "none"),
    ("ok", "err"),
    ("min", "max"),
    ("first", "last"),
    ("front", "back"),
    ("start", "end"),
    ("starts", "ends"),
    ("prefix", "suffix"),
    ("upper", "lower"),
    ("uppercase", "lowercase"),
    ("push", "pop"),
    ("encode", "decode"),
    ("increment", "decrement"),
    ("ascending", "descending"),
    ("enable", "disable"),
    ("enabled", "disabled"),
];

/// rustc's "there is a method `is_empty` with a similar name" for `is_not_empty`: a name that
/// means the opposite isn't a fix. When it is the call negated, the fix is `!value.is_empty()`.
fn opposite_name(d: &mut MappedDiag, sources: &Sources, method: &str) {
    let Some(other) = d.notes.iter().find(|n| n.contains(" with a similar name")).and_then(|n| ticks(n).first().cloned()) else { return };
    let (a, b) = (words(method), words(&other));
    if !d.raw_suggestions.contains(&other) || !opposite(&a, &b) {
        return;
    }
    d.notes.retain(|n| !n.contains(" with a similar name"));
    d.suggestions.retain(|s| s.replacement != other);
    let negation = negates(&a, &b) && is_predicate(&a) && is_predicate(&b);
    let fix = if negation { negated_call(d, sources, method, &other) } else { None };
    d.notes.push(help(&match &fix {
        Some((text, _)) => format!("`{}` means the opposite of `{}`: write `{}`.", other, method, text),
        None if negation => format!("`{}` means the opposite of `{}`: negate it with `!`.", other, method),
        None => format!("`{}` exists, but it means the opposite of `{}`.", other, method),
    }));
    if let Some((text, edits)) = fix {
        d.fixes.push(Fix { title: format!("use `{}`", text), edits });
    }
}

/// The call of `method` at the error, negated, with `other` in its place: `!x.other()`, in
/// parentheses when a `.` or `?` follows, or `x.other()` for `!x.method()`. Its text and the
/// edits, when it is on one line.
fn negated_call(d: &MappedDiag, sources: &Sources, method: &str, other: &str) -> Option<(String, Vec<Edit>)> {
    let here = sources.file(&d.file)?;
    let src = here.text.as_str();
    let at = byte_offset(src, d.line, d.col);
    if !src[at..].starts_with(method) {
        return None;
    }
    let is_call = |e: &Expr| matches!(&e.kind, ExprKind::MethodCall { recv, name, safe: false, .. } if name == method && src.get(recv.span.end as usize..at).is_some_and(|t| t.trim() == "."));
    // the call, the `!` in front of it, and whether it is the operand of a postfix operator
    let (mut call, mut not, mut postfix) = (None, None, false);
    oxie_compiler::visit::walk_items(&here.ast().items, &mut |e| {
        match &e.kind {
            _ if is_call(e) => call = Some(e.span),
            ExprKind::Unary(UnOp::Not, x) if is_call(x) => not = Some(e.span),
            ExprKind::MethodCall { recv: x, .. } | ExprKind::Field { recv: x, .. } | ExprKind::Index(x, _) | ExprKind::Try(x) | ExprKind::Unwrap(x) | ExprKind::Await(x) | ExprKind::Cast(x, _) if is_call(x) => postfix = true,
            _ => {}
        }
        true
    });
    let (start, end) = call.map(|s| (s.start as usize, s.end as usize))?;
    let (open, close) = match not {
        Some(_) => ("", ""),
        None if postfix => ("(!", ")"),
        None => ("!", ""),
    };
    let text = format!("{}{}{}{}{}", open, &src[start..at], other, &src[at + method.len()..end], close);
    if text.contains('\n') {
        return None;
    }
    let mut edits = vec![span_edit(src, Span::new(not.map_or(start, |n| n.start as usize), start), open), span_edit(src, Span::new(at, at + method.len()), other)];
    if !close.is_empty() {
        edits.push(span_edit(src, Span::new(end, end), close));
    }
    Some((text, edits))
}

/// The words of a method name: `is_not_empty` and `isNotEmpty` are `is`, `not`, `empty`.
fn words(name: &str) -> Vec<String> {
    snake_case(name).split('_').filter(|w| !w.is_empty()).map(str::to_string).collect()
}

/// Do two method names, as words, mean the opposite of each other: one negates the other, or
/// they differ in one word of the opposite meaning (`is_some`, `is_none`)?
fn opposite(a: &[String], b: &[String]) -> bool {
    negates(a, b) || one_word_apart(a, b).is_some_and(|(x, y)| OPPOSITE_WORDS.iter().any(|&(p, q)| (x, y) == (p, q) || (x, y) == (q, p)))
}

/// Is one name the other with a negating word (`is_not_empty`, `has_no_items`) or prefix
/// (`is_invalid`, `is_unsorted`)?
fn negates(a: &[String], b: &[String]) -> bool {
    let (long, short) = if a.len() > b.len() { (a, b) } else { (b, a) };
    let word = long.len() == short.len() + 1 && (0..long.len()).any(|i| matches!(long[i].as_str(), "not" | "no" | "non") && long[..i] == short[..i] && long[i + 1..] == short[i..]);
    word || one_word_apart(a, b).is_some_and(|(x, y)| ["un", "in", "im", "ir", "il", "dis", "non"].iter().any(|p| x.strip_prefix(p) == Some(y) || y.strip_prefix(p) == Some(x)))
}

/// The words in which two names differ, when they have as many words and differ in one.
fn one_word_apart<'a>(a: &'a [String], b: &'a [String]) -> Option<(&'a str, &'a str)> {
    let mut diff = a.iter().zip(b).filter(|(x, y)| x != y).map(|(x, y)| (x.as_str(), y.as_str()));
    let first = diff.next()?;
    (a.len() == b.len() && diff.next().is_none()).then_some(first)
}

/// Does a method name ask a yes-or-no question (`is_empty`, `has_items`), so that `!` negates it?
fn is_predicate(words: &[String]) -> bool {
    matches!(words.first().map(String::as_str), Some("is" | "has" | "are" | "can" | "does" | "should"))
}

/// `use a::b::C;` suggestions become `import a::b::C` fixes.
fn imports(d: &mut MappedDiag, lines: &[&str]) {
    let mut paths: Vec<String> = Vec::new();
    for s in &d.raw_suggestions {
        for l in s.lines() {
            let l = l.trim();
            if let Some(p) = l.strip_prefix("use ").and_then(|p| p.strip_suffix(';')) {
                let p = p.trim_start_matches("crate::").to_string();
                if !p.contains("__oxie") && !paths.contains(&p) {
                    paths.push(p);
                }
            }
        }
    }
    if paths.is_empty() {
        return;
    }
    d.notes.retain(|n| !n.contains("consider importing") && !n.contains("perhaps you want to import") && !n.contains("similar name"));
    let at = import_line(lines);
    let blank = if at == 0 && lines.first().is_some_and(|l| !l.trim().is_empty() && !l.starts_with("import ")) { "\n" } else { "" };
    for p in paths.iter().take(3) {
        d.fixes.push(Fix { title: format!("import `{}`", p), edits: vec![edit(at, 0, 0, &format!("import {}\n{}", p, blank))] });
    }
}

/// The line where a new import goes: after the last top-level import, or at the top.
fn import_line(lines: &[&str]) -> u32 {
    lines.iter().rposition(|l| l.starts_with("import ")).map(|i| i as u32 + 1).unwrap_or(0)
}

fn trait_missing(d: &mut MappedDiag, sources: &Sources, lines: &[&str], names: &[String]) {
    let msg = d.message.clone();
    if msg.contains("cannot be sent between threads safely") || msg.contains("cannot be shared between threads safely") {
        thread_safety(d, sources, names.first().map(String::as_str).unwrap_or(""));
        return;
    }
    // `tokio.spawn { ... }`: a lambda where a future is expected
    if msg.starts_with("`{closure@") && msg.ends_with("` is not a future") {
        rewrite(d, "a lambda is not a future".into());
        d.label = Some("this lambda is passed where a future is expected".into());
        d.notes.retain(|n| !n.contains("use parentheses to call this closure") && !n.contains("is not implemented for closure") && !n.contains("full name for the type") && !n.contains("--verbose"));
        d.notes.push(help("pass an async block instead of a lambda: `tokio.spawn(async move { ... })`. To give it its own copy of a shared handle, clone the handle into a `val` before the block."));
        return;
    }
    if msg.contains("the `?` operator can only be used") {
        d.notes.retain(|n| !n.contains("FromResidual") && !n.contains("consider adding return type"));
        let text = lines.get(d.line as usize).copied().unwrap_or("");
        // the `?` at or after the reported column (which can point at the start of the call)
        let from = text.char_indices().nth(d.col as usize).map(|(i, _)| i).unwrap_or(text.len());
        let q = text[from..].find('?').map(|i| from + i);
        let question_dot = q.is_some_and(|i| text[i..].starts_with("?."));
        let q_col = q.map(|i| text[..i].chars().count() as u32);
        if let Some(c) = q_col.filter(|_| msg.contains("not `Option`s") || msg.contains("not `Result`s")) {
            (d.col, d.end_line, d.end_col) = (c, d.line, c + 1);
        }
        // an optional value in a function that `throws`: there is no error to pass on
        if msg.contains("not `Option`s") {
            rewrite(d, "this value is optional (`T?`), so `?` has no error to pass on".into());
            d.label = Some("`?` on an optional value".into());
            if question_dot {
                d.notes.push(help("Oxie can't tell the type of the value before `?.`, and in a function that `throws` it reads `?.` on such a value as passing an error on. To get an optional result instead, declare the value's type first (`val v: String? = ...`, then `v?.len()`); to fail when it is missing, turn it into an error."));
            } else {
                d.notes.push(help("turn a missing value into an error that `?` can pass on: `.ok_or(\"...\")?`."));
            }
            if let Some(c) = q_col {
                d.fixes.push(Fix { title: "fail with an error when the value is missing".into(), edits: vec![edit(d.line, c, c + 1, ".ok_or(\"missing value\")?")] });
            }
            return;
        }
        // a `Result` in a function that returns an optional value
        if msg.contains("not `Result`s") {
            rewrite(d, "`?` passes an error on, but this function returns an optional value (`T?`), which can't hold it".into());
            d.label = Some("`?` on a value that can fail".into());
            d.notes.push(help("drop the error and give `null` instead with `.ok()?`, or declare the function `throws`."));
            if let Some(c) = q_col {
                d.fixes.push(Fix { title: "give `null` when it fails".into(), edits: vec![edit(d.line, c, c + 1, ".ok()?")] });
            }
            return;
        }
        rewrite(d, "`?` passes errors on, but this function doesn't declare `throws`".into());
        let name = enclosing_fun(lines, d.line).and_then(|(_, t)| t.split_once("fun ")).map(|(_, rest)| rest.chars().take_while(|c| is_ident_char(*c)).collect::<String>());
        match name.filter(|n| !n.is_empty()) {
            Some(n) => d.notes.push(help(&format!("add `throws` to `{}` so errors can be passed on: `fun {}(...) throws {{ ... }}`.", n, n))),
            None => d.notes.push(help("add `throws` to the function so errors can be passed on.")),
        }
        if let Some((l, c)) = enclosing_fun_brace(lines, d.line) {
            d.fixes.push(Fix { title: "add `throws` to the function".into(), edits: vec![edit(l, c, c, " throws")] });
        }
        return;
    }
    if msg.contains("`?` couldn't convert the error") {
        d.notes.push(help("the error type doesn't convert into the one this function throws. Declare plain `throws` to accept any error, or add a case for it to your `error enum` marked `@from`."));
        return;
    }
    let Some(ty) = names.first() else { return };
    let display = msg.ends_with("doesn't implement `std::fmt::Display`");
    let debug = msg.ends_with("doesn't implement `std::fmt::Debug`") || msg.ends_with("doesn't implement `Debug`");
    if !display && !debug {
        bound_fix(d, lines);
        return;
    }
    // a name read inline maps to its `$`: underline the whole `$low` / `${low}`
    if let Some(len) = placeholder_len(lines.get(d.line as usize).copied().unwrap_or(""), d.col) {
        (d.end_line, d.end_col) = (d.line, d.col + len);
    }
    if is_type_param(ty) {
        bound_fix(d, lines);
        return;
    }
    let base = ty.split('<').next().unwrap_or(ty).trim_start_matches('&').to_string();
    let is_std_collection = matches!(base.as_str(), "Vec" | "Option" | "HashMap" | "HashSet" | "BTreeMap" | "BTreeSet" | "VecDeque" | "Result") || base.starts_with('(') || base.starts_with('[');
    let user_kind = sources.declaring(&d.file, |ls| user_type_kind(ls, &base).is_some()).and_then(|f| user_type_kind(&f.lines(), &base));
    d.notes.retain(|n| !n.contains("in format strings you may be able to use") && !n.contains("is not implemented for"));
    if display {
        rewrite(d, format!("`{}` has no text form for `$` templates", ty));
        let printable_debug = is_std_collection || matches!(user_kind, Some("data class") | Some("enum"));
        if printable_debug {
            d.notes.push(help(&format!("print its debug form with a `:?` spec: `${{value:?}}`.{}", if user_kind.is_some() { format!(" To choose the text yourself, add `override fun toString() = \"...\"` to `{}`.", base) } else { String::new() })));
            if let Some(f) = debug_spec_fix(d, lines) {
                d.fixes.push(f);
            }
        } else if user_kind.is_some() {
            d.notes.push(help(&format!("add `override fun toString() = \"...\"` to `{}`, or make it a `data class` and print it with `${{value:?}}`.", base)));
        }
    } else if user_kind.is_some() {
        d.notes.push(help(&format!("`data class` and `enum` get a debug form automatically; for a plain `class`, add `@derive(Debug)` to `{}`.", base)));
    }
}

/// rustc's "consider annotating `Stats` with `#[derive(Default)]`" as the Oxie spelling.
fn derive_in_oxie_terms(d: &mut MappedDiag) {
    for n in d.notes.iter_mut() {
        let Some(rest) = n.strip_prefix("help: consider annotating `") else { continue };
        let Some((ty, rest)) = rest.split_once("` with `#[derive(") else { continue };
        let Some((traits, _)) = rest.split_once(")]`") else { continue };
        let ty = ty.split('<').next().unwrap_or(ty);
        *n = help(&format!("make `{}` implement {}: `class {}(...) : {}`.", ty, tick_list(traits), ty, traits));
    }
}

fn tick_list(traits: &str) -> String {
    traits.split(", ").map(|t| format!("`{}`", t)).collect::<Vec<_>>().join(" and ")
}

/// The length of the template placeholder (`$name`, `${...}`) starting at `col`.
fn placeholder_len(line: &str, col: u32) -> Option<u32> {
    let rest: Vec<char> = line.chars().skip(col as usize).collect();
    match rest.get(1)? {
        '{' if rest[0] == '$' => {
            let mut depth = 0;
            for (i, c) in rest.iter().enumerate().skip(1) {
                match c {
                    '{' => depth += 1,
                    '}' => {
                        depth -= 1;
                        if depth == 0 {
                            return Some(i as u32 + 1);
                        }
                    }
                    _ => {}
                }
            }
            None
        }
        c if rest[0] == '$' && (c.is_alphabetic() || *c == '_') => Some(1 + rest[1..].iter().take_while(|c| is_ident_char(**c)).count() as u32),
        _ => None,
    }
}

/// Adds `:?` to the template placeholder that rustc pointed at.
fn debug_spec_fix(d: &MappedDiag, lines: &[&str]) -> Option<Fix> {
    let line = lines.get(d.line as usize).copied()?;
    let chars: Vec<char> = line.chars().collect();
    // `{name}` inline arguments in the generated format string
    if let Some(gen) = d.gen_text.as_deref().and_then(|g| g.strip_prefix('{')).and_then(|g| g.strip_suffix('}')) {
        if !gen.is_empty() && gen.chars().all(|c| c.is_alphanumeric() || c == '_') {
            let simple = format!("${}", gen);
            let braced = format!("${{{}}}", gen);
            let s: String = chars.iter().collect();
            if let Some(i) = s.find(&braced) {
                let c = s[..i].chars().count() as u32 + braced.chars().count() as u32 - 1;
                return Some(Fix { title: "print the debug form (`:?`)".into(), edits: vec![edit(d.line, c, c, ":?")] });
            }
            let mut from = 0;
            while let Some(i) = s[from..].find(&simple) {
                let i = from + i;
                let after = s[i + simple.len()..].chars().next();
                if !after.is_some_and(|c| c.is_alphanumeric() || c == '_') {
                    let c = s[..i].chars().count() as u32;
                    let end = c + simple.chars().count() as u32;
                    return Some(Fix { title: "print the debug form (`:?`)".into(), edits: vec![edit(d.line, c, end, &format!("${{{}:?}}", gen))] });
                }
                from = i + simple.len();
            }
            return None;
        }
    }
    // an expression inside `${...}`: find the closing brace after the span
    let start = d.col as usize;
    let open = (0..start.min(chars.len())).rev().find(|&i| chars[i] == '{' && i > 0 && chars[i - 1] == '$')?;
    let mut depth = 0;
    for (i, &ch) in chars.iter().enumerate().skip(open) {
        match ch {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    let inner: String = chars[open + 1..i].iter().collect();
                    if inner.contains(':') {
                        return None;
                    }
                    return Some(Fix { title: "print the debug form (`:?`)".into(), edits: vec![edit(d.line, i as u32, i as u32, ":?")] });
                }
            }
            _ => {}
        }
    }
    None
}

/// "consider restricting type parameter `T` with trait `X`: `: path::X`" -> add the bound.
fn bound_fix(d: &mut MappedDiag, lines: &[&str]) {
    let Some(n) = d.notes.iter().find(|n| n.contains("consider restricting type parameter")).cloned() else { return };
    let param = ticks(&n).first().cloned().unwrap_or_default();
    let Some(bound) = d.raw_suggestions.iter().find(|s| s.starts_with(": ") || s.starts_with(" + ")).cloned() else { return };
    let Some((l, line)) = enclosing_fun(lines, d.line) else { return };
    let Some(lt) = line.find('<') else { return };
    let Some(gt) = line[lt..].find('>').map(|g| lt + g) else { return };
    let generics = &line[lt + 1..gt];
    let mut offset = lt + 1;
    for part in generics.split(',') {
        let p = part.trim();
        let lead = part.len() - part.trim_start().len();
        if p == param {
            let c = line[..offset + lead + p.len()].chars().count() as u32;
            let bound = bound.trim_start_matches(" + ").trim_start_matches(": ");
            d.notes.retain(|x| !x.contains("consider restricting type parameter"));
            d.notes.push(help(&format!("generic code can only use what the type parameter promises; `{}: {}` promises it.", param, bound)));
            d.fixes.push(Fix { title: format!("require `{}: {}`", param, bound), edits: vec![edit(l, c, c, &format!(": {}", bound))] });
            return;
        }
        offset += part.len() + 1;
    }
}

/// `node.next?.value` on a shared class: fields of shared objects are read through getters.
/// `val v = if (c) x else null` where `x` comes from Rust and is optional already: Oxie makes
/// it `x.into()`, which could be `Some(x)` or `x` itself. Declaring the type settles it.
fn optional_already(d: &mut MappedDiag, lines: &[&str]) {
    let ty = d.notes.iter().find_map(|n| between(n, "Option<_>: From<Option<", ">>`")).map(|t| format!("{}?", t));
    rewrite(d, "Oxie can't tell whether this value is optional already".to_string());
    d.notes.clear();
    d.suggestions.clear();
    d.notes.push(help(
        "next to `null` (`if (c) x else null`) a value becomes optional, but this one comes from Rust, where Oxie doesn't see its type, and may be optional already. Declare the variable's type.",
    ));
    let line = lines.get(d.line as usize).copied().unwrap_or("");
    let (Some(ty), Some(at)) = (ty, line.find("val ")) else { return };
    let name: String = line[at + 4..].chars().take_while(|c| c.is_alphanumeric() || *c == '_').collect();
    if name.is_empty() || line[at + 4 + name.len()..].trim_start().starts_with(':') {
        return;
    }
    let col = line[..at + 4 + name.len()].chars().count() as u32;
    d.fixes.push(Fix { title: format!("declare `{}` as `{}`", name, ty), edits: vec![edit(d.line, col, col, &format!(": {}", ty))] });
}

fn getter_call(d: &mut MappedDiag, lines: &[&str], name: &str) {
    refine(d, lines, name);
    let ty = ticks(&d.message).get(1).cloned().unwrap_or_default();
    let owner = ty.trim_start_matches('&').to_string();
    rewrite(d, format!("`{}` is read with `{}()` here", name, name));
    d.label = Some(format!("`{}` is a shared class; its fields are read through methods", owner));
    d.notes.retain(|n| !n.contains("use parentheses"));
    d.notes.push(help(&format!(
        "Oxie reads `x.{}` as `x.{}()` when it knows `x` is a `{}`; here it couldn't tell (for example after `?.`), so write the call.",
        name, name, owner
    )));
    d.notes.retain(|n| !n.contains("methods are immutable"));
    let line = lines.get(d.line as usize).copied().unwrap_or("");
    if word_at(line, d.col as usize) == Some(name) {
        let end = d.col + name.chars().count() as u32;
        let after: String = line.chars().skip(end as usize).collect();
        let after = after.trim_start();
        let assigned = after.starts_with('=') && !after.starts_with("==") || ["+=", "-=", "*=", "/=", "%="].iter().any(|op| after.starts_with(op));
        if assigned {
            d.notes.push(help(&format!("to change it, call the setter: `x.set_{}(value)`.", name)));
        } else {
            d.fixes.push(Fix { title: format!("call `{}()`", name), edits: vec![edit(d.line, end, end, "()")] });
        }
    }
}

fn mismatched(d: &mut MappedDiag, src: &str, lines: &[&str]) {
    let borrowed_text = d.label.as_deref().is_some_and(|l| l.contains("expected `String`, found `&str`"));
    if let Some(l) = d.label.clone() {
        let l = l.replace("found `&str`", "found `&str` (borrowed text)").replace("found `String`", "found `String` (owned text)");
        d.label = Some(oxie_types(&l));
    }
    if borrowed_text {
        elvis_borrowed_text(d, lines);
    }
    expr_body_value(d, lines);
    // `n ?: "none"` with an `i32?`: the default goes to a runtime helper, which rustc mentions
    if d.rust_message.is_none() {
        if let Some((value, _)) = elvis_default_at(src, d.line, d.col) {
            rewrite(d, format!("the default after `?:` doesn't have the type of the value in `{}`", value));
            d.notes.retain(|n| !n.contains("the return type of this call") && !n.contains("method defined here") && !n.contains("arguments to this"));
            d.notes.push(help(&format!("`{} ?: default` gives the value in `{}`, or the default when there is none, so both need the same type.", value, value)));
        }
    }
}

/// `fun opt(x: i32) = Some(x)`: an expression body without a return type must be `()`, and Oxie
/// couldn't tell this one's type. The fix declares it: `fun opt(x: i32): i32? = Some(x)`.
fn expr_body_value(d: &mut MappedDiag, lines: &[&str]) {
    let Some(found) = d.label.as_deref().and_then(|l| between(l, "expected `()`, found `", "`")) else { return };
    let line = lines.get(d.line as usize).copied().unwrap_or("");
    let Some(fun_at) = line.find("fun ") else { return };
    // the `)` that closes the parameters, followed by ` = ` and the body the error is in
    let open = match line[fun_at..].find('(') {
        Some(i) => fun_at + i,
        None => return,
    };
    let mut depth = 0;
    let Some(close) = line[open..].char_indices().find(|&(_, c)| {
        depth += match c {
            '(' => 1,
            ')' => -1,
            _ => 0,
        };
        depth == 0
    }).map(|(i, _)| open + i) else { return };
    let after = &line[close + 1..];
    let eq = after.len() - after.trim_start().len();
    if !after[eq..].starts_with('=') || after[eq..].starts_with("==") || line[..close + 1 + eq].chars().count() as u32 > d.col {
        return;
    }
    let name: String = line[fun_at + 4..open].chars().filter(|c| !c.is_whitespace()).collect();
    let name = name.rsplit('.').next().unwrap_or(&name).split('<').next().unwrap_or("").to_string();
    rewrite(d, format!("`{}` declares no return type, but its body gives a value of type `{}`", name, found));
    d.label = Some(format!("of type `{}`", found));
    d.notes.retain(|n| !n.contains("try adding a return type") && !n.contains("consider using a semicolon") && !n.contains("expected unit type") && !n.contains("found enum") && !n.contains("found struct"));
    d.notes.push(help("a function written `fun f() = expr` without a return type returns nothing, and Oxie couldn't tell the type of this body. Declare the return type, or write the body as a block (`{ ... }`) to throw the value away."));
    if !found.contains(['{', '}']) && !found.contains("closure") && !found.contains("impl ") {
        let col = line[..close + 1].chars().count() as u32;
        d.fixes.push(Fix { title: format!("declare the return type `{}`", found), edits: vec![edit(d.line, col, col, &format!(": {}", found))] });
    }
}

/// `u.address?.city ?: "unknown"` where a `String` is expected: the chain reads `city` through a
/// reference, so the text default makes the result a `&str`. Oxie doesn't copy the field
/// silently; the fix is `u.address?.city.clone() ?: "unknown"`.
fn elvis_borrowed_text(d: &mut MappedDiag, lines: &[&str]) {
    if d.line != d.end_line {
        return;
    }
    let line = lines.get(d.line as usize).copied().unwrap_or("");
    let span: Vec<char> = line.chars().skip(d.col as usize).take(d.end_col.saturating_sub(d.col) as usize).collect();
    let Some(op) = top_level_elvis(&span) else { return };
    let left: String = span[..op].iter().collect::<String>().trim_end().to_string();
    let right: String = span[op + 2..].iter().collect::<String>().trim().to_string();
    if !left.contains("?.") || !right.starts_with('"') || left.ends_with(".clone()") {
        return;
    }
    rewrite(d, format!("`{} ?: {}` is borrowed text (`&str`), but a `String` is expected", left, right));
    d.notes.retain(|n| !n.contains("conversion method"));
    d.notes.push(help(&format!(
        "`{}` reads the field through a reference, so `?:` with a text default gives a `&str` borrowed from it. Oxie doesn't copy the field silently: take a copy with `.clone()` before `?:`, or declare the result as `&str`.",
        left
    )));
    let end = d.col + left.chars().count() as u32;
    d.fixes.push(Fix { title: "use a copy (`.clone()`)".to_string(), edits: vec![edit(d.line, end, end, ".clone()")] });
}

/// Index of the first `?:` outside brackets and string literals.
fn top_level_elvis(s: &[char]) -> Option<usize> {
    let (mut depth, mut in_str, mut escaped) = (0i32, false, false);
    for i in 0..s.len() {
        let c = s[i];
        if in_str {
            match c {
                _ if escaped => escaped = false,
                '\\' => escaped = true,
                '"' => in_str = false,
                _ => {}
            }
            continue;
        }
        match c {
            '"' => in_str = true,
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => depth -= 1,
            '?' if depth == 0 && s.get(i + 1) == Some(&':') => return Some(i),
            _ => {}
        }
    }
    None
}

/// `name ?: "x"` or `name?.len()` where `name` can't be null (a `String`, not a `String?`).
/// rustc complains about the runtime helpers they lower to (`ElvisOr`, `as_ref`, `map`), which
/// the program never mentions; the error is replaced by one about the operand, with a fix.
/// Both copies rustc reports for one `?:` end up the same, so only one is shown.
fn not_optional(d: &mut MappedDiag, src: &str) -> bool {
    let msg = d.message.clone();
    // the type rustc names, and whether the error is about `?:` (else about `?.`)
    let (ty, elvis) = match d.code.as_deref().unwrap_or("") {
        "E0277" if msg.contains(": ElvisOr<") || msg.contains(": Elvis`") => (between(&msg, "the trait bound `", ": Elvis"), true),
        "E0277" if msg.contains("the `?` operator can only be applied to values that implement `Try`") => (d.label.as_deref().and_then(|l| between(l, "cannot be applied to type `", "`")), false),
        "E0599" if ["as_ref", "map", "and_then"].iter().any(|m| msg.starts_with(&format!("no method named `{}` found", m))) => (msg.split_once(" found for ").and_then(|(_, r)| ticks(r).first().cloned()), false),
        "E0599" if msg.ends_with("is not an iterator") => (ticks(&msg).first().cloned(), false),
        // rustc suggests `<String as AsRef<T>>::as_ref(&` and `)`
        "E0282" | "E0283" if d.raw_suggestions.iter().any(|s| s.contains(" as AsRef<")) => (d.raw_suggestions.iter().find_map(|s| between(s, "<", " as AsRef<")), false),
        _ => return false,
    };
    let (file, _) = oxie_compiler::parser::parse_file(src);
    let at = byte_offset(src, d.line, d.col) as u32;
    // (the value, the text the fix removes or replaces, its replacement)
    let mut found: Option<(Span, Span, &str)> = None;
    // `a ?: b ?: c` is `(a ?: b) ?: c`: both start at `a`, and rustc's error doesn't tell which
    // one has the value that isn't optional
    let (mut elvises, mut chain) = (0, None);
    oxie_compiler::visit::walk_items(&file.items, &mut |e| {
        match &e.kind {
            ExprKind::Elvis(l, r) if elvis && l.span.start == at => {
                // the outermost comes first: its left side is the whole chain before the last `?:`
                chain = chain.or(Some(l.span));
                elvises += 1;
                found = Some((l.span, Span { start: l.span.end, end: r.span.end }, ""));
            }
            // in `a?.b?.c` every link starts at `a`; the first `?.` is the one after the value
            ExprKind::Field { recv, safe: true, .. } | ExprKind::MethodCall { recv, safe: true, .. } if !elvis && recv.span.start == at && found.is_none_or(|(v, ..)| recv.span.end < v.end) => {
                if let Some(q) = src.get(recv.span.end as usize..e.span.end as usize).and_then(|t| t.find("?.")) {
                    let q = recv.span.end + q as u32;
                    found = Some((recv.span, Span { start: q, end: q + 2 }, "."));
                }
            }
            _ => {}
        }
        true
    });
    let Some((value, removed, replacement)) = found else { return false };
    let value = if elvises > 1 { chain.unwrap_or(value) } else { value };
    let shown = source_text(src, value).filter(|t| t.chars().count() <= 40).map(|t| format!("`{}`", t)).unwrap_or_else(|| "the value".into());
    let ty = ty.filter(|t| t != "_");
    if elvises > 1 {
        // a chain: say what's wrong without naming the operand
        rewrite(d, "a `?:` here has a left side that can't be null".to_string());
        d.label = Some(match ty.as_deref() {
            Some(t) if !t.starts_with('{') => format!("one of these is a `{}`, not a `{}?`", t, t),
            _ => "one of these is never null".into(),
        });
        d.notes = vec![help("`a ?: b ?: c` is `(a ?: b) ?: c`. When `a`, or `a ?: b`, always has a value, the `?:` after it is never used: remove that default, or make the value optional.")];
        d.fixes.clear();
    } else if let Some(inner) = ty.as_deref().and_then(|t| t.trim_start_matches('&').strip_prefix("Option<").or_else(|| t.trim_start_matches('&').strip_prefix("Result<"))) {
        // `opt ?: 0` with a `String?`: the default has another type than the value
        let inner = first_generic_arg(inner);
        rewrite(d, format!("the default after `?:` doesn't have the type of the value in {}", shown));
        d.label = Some(format!("holds a `{}`", inner));
        d.notes = vec![help(&format!("`a ?: default` gives the value in `a`, or the default when there is none, so the default must be a `{}` too.", inner))];
    } else {
        let what = match ty.as_deref() {
            Some("{integer}") => format!("{} is an integer", shown),
            Some("{float}") => format!("{} is a number", shown),
            Some(t) => format!("{} has type `{}`", shown, t),
            None => format!("{} is never null", shown),
        };
        let (op, does) = if elvis { ("?:", "`a ?: b` gives `b` when `a` is null (a `T?`) or an error (a `Result`)") } else { ("?.", "`a?.b` reads `b` only when `a` isn't null (a `T?`), and passes an error on (a `Result`)") };
        rewrite(d, format!("`{}` needs an optional value, but {}", op, what));
        d.label = Some(match ty.as_deref() {
            Some(t) if !t.starts_with('{') => format!("type `{}`, not `{}?`", t, t),
            _ => "never null".into(),
        });
        let removed_text = source_text(src, removed).unwrap_or_default();
        let (advice, title) = if elvis {
            let short = !removed_text.trim().is_empty() && removed_text.trim().chars().count() <= 30;
            let title = if short { format!("remove `{}`", removed_text.trim()) } else { "remove the default (`?: ...`)".to_string() };
            // a variable or field can be declared optional; a call's result can't
            let declared = source_text(src, value).filter(|t| t.chars().all(|c| is_ident_char(c) || c == '.')).zip(ty.as_deref().filter(|t| !t.starts_with('{')));
            let or = declared.map(|(v, t)| format!(", or declare `{}` as `{}?` if it can be missing", v, t)).unwrap_or_default();
            (format!("{} always has a value, so the default is never used: remove it{}.", shown, or), title)
        } else {
            (format!("{} always has a value: use `.` instead.", shown), "use `.` instead of `?.`".to_string())
        };
        d.notes = vec![help(&format!("{}. {}", does, advice))];
        let idx = oxie_compiler::LineIndex::new(src);
        let (l, c) = idx.line_col(src, removed.start as usize);
        let (el, ec) = idx.line_col(src, removed.end as usize);
        d.fixes = vec![Fix { title, edits: vec![Edit { file: None, line: l as u32, col: c as u32, end_line: el as u32, end_col: ec as u32, text: replacement.to_string() }] }];
    }
    let idx = oxie_compiler::LineIndex::new(src);
    let (l, c) = idx.line_col(src, value.start as usize);
    let (el, ec) = idx.line_col(src, value.end as usize);
    (d.line, d.col, d.end_line, d.end_col) = (l as u32, c as u32, el as u32, ec as u32);
    d.secondary.clear();
    d.suggestions.clear();
    d.multipart.clear();
    d.raw_suggestions.clear();
    // in a `throws` function `?.` passes errors on, and rustc's follow-up errors about that
    // `?` come at the same position
    d.covers_position = true;
    true
}

/// The `?:` whose default starts at this position: (the value's text, the default's text).
fn elvis_default_at(src: &str, line: u32, col: u32) -> Option<(String, String)> {
    let (file, _) = oxie_compiler::parser::parse_file(src);
    let at = byte_offset(src, line, col) as u32;
    let mut found = None;
    oxie_compiler::visit::walk_items(&file.items, &mut |e| {
        if let ExprKind::Elvis(l, r) = &e.kind {
            if r.span.start == at {
                found = source_text(src, l.span).zip(source_text(src, r.span));
            }
        }
        true
    });
    found
}

/// Is the expression at `offset` the text of a `toString`: a value its body is or ends with, or
/// one that a `return` gives?
fn is_to_string_text(items: &[Item], offset: u32) -> bool {
    for it in items.iter().filter(|it| it.span.start <= offset && offset <= it.span.end) {
        match &it.kind {
            ItemKind::Fun(f) if (f.name == "toString" || f.name == "to_string") && f.mods.is_override => {
                let mut starts = Vec::new();
                match &f.body {
                    Some(FunBody::Expr(e)) => values(e, &mut starts),
                    Some(FunBody::Block(b)) => {
                        block_values(b, &mut starts);
                        oxie_compiler::visit::walk_block(b, &mut |e| match &e.kind {
                            // a `return` in a lambda gives the lambda's value
                            ExprKind::Lambda { .. } => false,
                            ExprKind::Return(Some(v)) => {
                                values(v, &mut starts);
                                true
                            }
                            _ => true,
                        });
                    }
                    None => {}
                }
                return starts.contains(&offset);
            }
            ItemKind::Type(t) => return is_to_string_text(&t.members, offset),
            ItemKind::Impl(i) => return is_to_string_text(&i.members, offset),
            ItemKind::Mod { items, .. } | ItemKind::Object { items, .. } => return is_to_string_text(items, offset),
            _ => {}
        }
    }
    false
}

/// Where the values a block ends with start (both branches of an `if` and every arm of a `when`).
fn block_values(b: &Block, out: &mut Vec<u32>) {
    if let Some(Stmt::Expr { expr, semi: false }) = b.stmts.last() {
        values(expr, out);
    }
}

fn values(e: &Expr, out: &mut Vec<u32>) {
    match &e.kind {
        ExprKind::If { then, else_: Some(el), .. } => {
            block_values(then, out);
            values(el, out);
        }
        ExprKind::When { arms, .. } => arms.iter().for_each(|a| values(&a.body, out)),
        ExprKind::Block(b) => block_values(b, out),
        ExprKind::Paren(x) => values(x, out),
        _ => out.push(e.span.start),
    }
}

/// The byte offset of a zero-based line and character column.
fn byte_offset(src: &str, line: u32, col: u32) -> usize {
    let start: usize = src.split_inclusive('\n').take(line as usize).map(str::len).sum();
    let rest = src.get(start..).unwrap_or("");
    start + rest.char_indices().nth(col as usize).map_or(rest.len(), |(i, _)| i)
}

/// The text of the index expression starting at `offset` and of its list (`items[0]`, `items`):
/// the outermost one, for `rows[0][1]`.
fn indexed_at(items: &[Item], src: &str, offset: u32) -> Option<(String, String)> {
    let mut found = None;
    oxie_compiler::visit::walk_items(items, &mut |e| {
        if let ExprKind::Index(list, _) = &e.kind {
            if found.is_none() && e.span.start == offset {
                found = Some((e.span, list.span));
            }
        }
        true
    });
    let (element, list) = found?;
    Some((source_text(src, element)?, source_text(src, list)?))
}

/// The source text of a span on one line.
fn source_text(src: &str, span: Span) -> Option<String> {
    let t = src.get(span.start as usize..span.end as usize)?;
    (!t.contains('\n')).then(|| t.to_string())
}

/// The first type argument of the arguments that follow `Option<` or `Result<` (`Vec<i32>` in
/// `Vec<i32>, E>`).
fn first_generic_arg(args: &str) -> &str {
    let mut depth = 0;
    for (i, c) in args.char_indices() {
        match c {
            '<' | '(' | '[' => depth += 1,
            '>' | ')' | ']' if depth > 0 => depth -= 1,
            ',' | '>' if depth == 0 => return args[..i].trim(),
            _ => {}
        }
    }
    args.trim()
}

/// The text between the first `start` and the next `end` after it.
fn between(s: &str, start: &str, end: &str) -> Option<String> {
    let (_, rest) = s.split_once(start)?;
    rest.split_once(end).map(|(t, _)| t.to_string())
}

fn unused_mut(d: &mut MappedDiag, lines: &[&str]) {
    let line = lines.get(d.line as usize).copied().unwrap_or("");
    let rest: String = line.chars().skip(d.col as usize).collect();
    let col = if rest.starts_with("var ") { Some(d.col) } else { find_word(line, "var", d.col as usize).map(|c| c as u32) };
    let Some(c) = col else { return };
    let after: String = line.chars().skip(c as usize + 4).collect();
    let name: String = after.trim_start().chars().take_while(|c| c.is_alphanumeric() || *c == '_').collect();
    rewrite(d, format!("`{}` never changes, so it can be a `val`", name));
    d.col = c;
    d.end_col = c + 3;
    d.label = None;
    d.notes.retain(|n| !n.contains("remove this `mut`") && !n.contains("#[warn(unused_mut)]"));
    d.fixes.push(Fix { title: format!("declare `{}` with `val`", name), edits: vec![edit(d.line, c, c + 3, "val")] });
}

/// A call that can fail, used as a statement in a function that doesn't `throw`: its error is
/// dropped, and the program goes on.
fn unused_result(d: &mut MappedDiag, lines: &[&str]) {
    rewrite(d, "this call can fail, and its error is ignored".into());
    d.label = Some("the error this returns is dropped".into());
    d.notes.retain(|n| !n.contains("#[warn(") && !n.contains("let _ =") && !n.contains("should be handled"));
    let name = enclosing_fun(lines, d.line).and_then(|(_, t)| t.split_once("fun ")).map(|(_, rest)| rest.chars().take_while(|c| is_ident_char(*c)).collect::<String>()).filter(|n| !n.is_empty());
    let pass_on = match &name {
        Some(n) => format!("declare `fun {}() throws` to pass it on", n),
        None => "declare the function `throws` to pass it on".into(),
    };
    d.notes.push(help(&format!("{}, add `.unwrap()` to stop the program when it fails, or write `val _ = ...` to ignore it on purpose.", pass_on)));
    if let Some((l, c)) = enclosing_fun_brace(lines, d.line) {
        d.fixes.push(Fix { title: "add `throws` to the function".into(), edits: vec![edit(l, c, c, " throws")] });
    }
    if d.line == d.end_line {
        d.fixes.push(Fix { title: "stop the program when it fails".into(), edits: vec![edit(d.end_line, d.end_col, d.end_col, ".unwrap()")] });
    }
}

fn unused_variable(d: &mut MappedDiag, lines: &[&str], name: &str) {
    refine(d, lines, name);
    let line = lines.get(d.line as usize).copied().unwrap_or("");
    d.notes.retain(|n| !n.contains("#[warn("));
    if word_at(line, d.col as usize) == Some(name) {
        let end = d.col + name.chars().count() as u32;
        d.notes.retain(|n| !n.contains("prefix it with an underscore"));
        d.notes.push(help(&format!("if that's intended, name it `_{}`.", name)));
        d.fixes.push(Fix { title: format!("rename to `_{}`", name), edits: vec![edit(d.line, d.col, end, &format!("_{}", name))] });
    }
}

/// rustc suggestions that map exactly onto the source and read the same in Oxie.
fn rustc_fixes(d: &mut MappedDiag, lines: &[&str]) {
    let suggestions = d.suggestions.clone();
    for s in suggestions {
        let r = s.replacement.as_str();
        let title = match r {
            ".clone()" => "use a copy (`.clone()`)".to_string(),
            "&" => "pass a reference (`&`)".to_string(),
            "&mut " => "pass a mutable reference (`&mut`)".to_string(),
            "move " => "make it a `move` lambda".to_string(),
            "*" => "use the value (`*`)".to_string(),
            "?" => "pass the error on (`?`)".to_string(),
            _ if r.starts_with('.') && r.ends_with(')') && !r.contains("::") && !r.contains('|') => format!("use `{}`", r),
            _ if !r.is_empty() && r.chars().all(|c| c.is_alphanumeric() || c == '_') && s.line == s.end_line && d.fixes.is_empty() => {
                let line = lines.get(s.line as usize).copied().unwrap_or("");
                let old: String = line.chars().skip(s.col as usize).take((s.end_col - s.col) as usize).collect();
                if old.is_empty() || !old.chars().all(|c| c.is_alphanumeric() || c == '_') {
                    continue;
                }
                format!("use `{}`", r)
            }
            _ => continue,
        };
        let e = Edit { file: None, line: s.line, col: s.col, end_line: s.end_line, end_col: s.end_col, text: r.to_string() };
        // an explanation's own fix of the same kind wins (`move` goes inside a call's parentheses)
        if d.fixes.iter().any(|f| f.title == title || f.edits.iter().any(|x| x.line == e.line && x.col == e.col)) {
            continue;
        }
        d.fixes.push(Fix { title, edits: vec![e] });
    }
    // one that edits several places (`Box::new(` and `)`), when each part reads as Oxie: its note
    // can't show the code in one piece, so it becomes the fix's title
    for m in d.multipart.clone() {
        let same_place = |d: &MappedDiag, edits: &[Edit]| d.fixes.iter().any(|f| f.edits.iter().any(|x| edits.iter().any(|e| (x.line, x.col) == (e.line, e.col))));
        if let Some(edits) = clone_call(&m.parts, lines) {
            if !same_place(d, &edits) {
                d.fixes.push(Fix { title: "use a copy (`.clone()`)".into(), edits });
            }
            continue;
        }
        let edits: Option<Vec<Edit>> = m.parts.iter().map(|p| Some(Edit { file: None, line: p.line, col: p.col, end_line: p.end_line, end_col: p.end_col, text: oxie_code(&p.replacement)? })).collect();
        let Some(edits) = edits else { continue };
        if same_place(d, &edits) {
            continue;
        }
        d.notes.retain(|n| n.strip_prefix("help: ") != Some(m.message.as_str()));
        d.fixes.push(Fix { title: oxie_terms(&m.message), edits });
    }
}

/// rustc's `<String as Clone>::clone(&` and `)` around a value, in Oxie: `.clone()` after the
/// value, which is put in parentheses unless the call applies to all of it (`items[0]`).
fn clone_call(parts: &[Suggestion], lines: &[&str]) -> Option<Vec<Edit>> {
    let [open, close] = parts else { return None };
    if !(open.replacement.starts_with('<') && open.replacement.ends_with(" as Clone>::clone(&") && close.replacement == ")" && open.line == close.line) {
        return None;
    }
    let line = lines.get(open.line as usize)?;
    let value: String = line.chars().skip(open.end_col as usize).take(close.col.checked_sub(open.end_col)? as usize).collect();
    if value.is_empty() {
        return None;
    }
    Some(if postfix_ok(&value) {
        vec![edit(close.line, close.col, close.end_col, ".clone()")]
    } else {
        vec![edit(open.line, open.col, open.end_col, "("), edit(close.line, close.col, close.end_col, ").clone()")]
    })
}

/// Does a method call after this code apply to all of it: a name, field, index, call or
/// parenthesized value (`this.items[i + 1]`, `(a + b)`), not `*x` or `a + b`?
fn postfix_ok(code: &str) -> bool {
    let mut depth = 0;
    for c in code.chars() {
        match c {
            '(' | '[' => depth += 1,
            ')' | ']' => depth -= 1,
            _ if depth > 0 => {}
            c if is_ident_char(c) || c == '.' => {}
            _ => return false,
        }
    }
    depth == 0
}

/// A part of a rustc suggestion as Oxie, when it reads the same apart from paths through a type
/// (`Box::new(` is `Box.new(`): parentheses, `&`, `*`, `?`, names and calls.
fn oxie_code(part: &str) -> Option<String> {
    let code = type_paths(part);
    let rust_only = |w: &str| matches!(w, "self" | "Self" | "dyn" | "impl" | "fn" | "let");
    let ok = code.chars().all(|c| is_ident_char(c) || " ().,&*?".contains(c)) && !code.split(|c: char| !is_ident_char(c)).any(rust_only);
    ok.then_some(code)
}

/// rustc's words in Oxie terms: a path through a type in its code takes `.` (`Box.new`), and a
/// field that holds a function isn't a "trait object".
fn oxie_terms(message: &str) -> String {
    let message = message.replace("the trait object stored in", "the function stored in");
    message.split('`').enumerate().map(|(i, s)| if i % 2 == 1 { type_paths(s) } else { s.to_string() }).collect::<Vec<_>>().join("`")
}

/// Rust code with Oxie's spelling of paths through a type: `Box::new` is `Box.new`,
/// `Ordering::Less` is `Ordering.Less`. Paths through a module (`std::mem::take`) keep `::`.
fn type_paths(code: &str) -> String {
    let mut out = String::new();
    let mut rest = code;
    while let Some(i) = rest.find(is_ident_char) {
        out.push_str(&rest[..i]);
        rest = &rest[i..];
        // the path's segments: names joined by `::`
        let mut segs = Vec::new();
        loop {
            let n = rest.find(|c: char| !is_ident_char(c)).unwrap_or(rest.len());
            segs.push(&rest[..n]);
            rest = &rest[n..];
            match rest.strip_prefix("::").filter(|r| r.starts_with(is_ident_char)) {
                Some(r) => rest = r,
                None => break,
            }
        }
        let types = segs[..segs.len() - 1].iter().all(|s| s.starts_with(char::is_uppercase));
        out.push_str(&segs.join(if types { "." } else { "::" }));
    }
    out + rest
}

// ---------------------------------------------------------------------------------------------
// helpers

fn rewrite(d: &mut MappedDiag, message: String) {
    if d.rust_message.is_none() {
        d.rust_message = Some(d.message.clone());
    }
    d.message = message;
}

fn help(s: &str) -> String {
    format!("help: {}", s)
}

fn edit(line: u32, col: u32, end_col: u32, text: &str) -> Edit {
    Edit { file: None, line, col, end_line: line, end_col, text: text.to_string() }
}

/// An edit that replaces the bytes `span` of `src`.
fn span_edit(src: &str, span: Span, text: &str) -> Edit {
    let idx = oxie_compiler::LineIndex::new(src);
    let ((l, c), (el, ec)) = (idx.line_col(src, span.start as usize), idx.line_col(src, span.end as usize));
    Edit { file: None, line: l as u32, col: c as u32, end_line: el as u32, end_col: ec as u32, text: text.to_string() }
}

/// A place of `this` as rustc names it (`self.name`, `*self`) in the words of the Oxie code
/// `written` at its position: a field is `name`, or `this.name` where the code spells out
/// `this` (after a `&`, `&mut` or `*`). None for other places.
fn oxie_place(place: &str, written: &str) -> Option<String> {
    let place = place.trim_start_matches('*');
    // `init` blocks build the object as `__self`
    let rest = place.strip_prefix("self").or_else(|| place.strip_prefix("__self"))?;
    if rest.is_empty() {
        return Some("this".into());
    }
    let field = rest.strip_prefix('.')?;
    let written = written.trim_start_matches(['&', '*', '(']).trim_start_matches("mut ");
    // a tuple field (`self.0`) can't be written alone
    Some(if written.starts_with("this.") || field.starts_with(|c: char| c.is_ascii_digit()) { format!("this.{}", field) } else { field.to_string() })
}

fn describe(path: &str) -> String {
    if path.is_empty() {
        "a value".into()
    } else {
        format!("`{}`", path.trim_start_matches('*'))
    }
}

fn last_segment(path: &str) -> &str {
    path.rsplit("::").next().unwrap_or(path)
}

/// Rust spellings of Option in messages, in Oxie terms.
fn oxie_types(s: &str) -> String {
    let mut out = s.to_string();
    while let Some(i) = out.find("Option<") {
        let rest = &out[i + 7..];
        let mut depth = 1;
        let mut end = None;
        for (j, ch) in rest.char_indices() {
            match ch {
                '<' => depth += 1,
                '>' => {
                    depth -= 1;
                    if depth == 0 {
                        end = Some(j);
                        break;
                    }
                }
                _ => {}
            }
        }
        let Some(j) = end else { break };
        let inner = rest[..j].to_string();
        out = format!("{}{}?{}", &out[..i], inner, &rest[j + 1..]);
    }
    out
}

/// Contents of `backticked` spans.
fn ticks(s: &str) -> Vec<String> {
    s.split('`').skip(1).step_by(2).map(|x| x.to_string()).collect()
}

fn is_ident_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// The identifier starting exactly at `col`, if any.
fn word_at(line: &str, col: usize) -> Option<&str> {
    let start = line.char_indices().nth(col).map(|(i, _)| i)?;
    if start > 0 && line[..start].chars().next_back().is_some_and(is_ident_char) {
        return None;
    }
    let len = line[start..].find(|c: char| !is_ident_char(c)).unwrap_or(line.len() - start);
    (len > 0).then(|| &line[start..start + len])
}

/// Character column of the whole-word occurrence of `word` nearest to `near` (preferring later ones).
fn find_word(line: &str, word: &str, near: usize) -> Option<usize> {
    if word.is_empty() {
        return None;
    }
    let mut best: Option<usize> = None;
    let mut from = 0;
    while let Some(i) = line[from..].find(word) {
        let i = from + i;
        let before = line[..i].chars().next_back();
        let after = line[i + word.len()..].chars().next();
        if !before.is_some_and(is_ident_char) && !after.is_some_and(is_ident_char) {
            let c = line[..i].chars().count();
            let better = match best {
                None => true,
                // the first one at or after `near`, else the last one before it
                Some(b) if c >= near => b < near || c < b,
                Some(b) => b < near && c > b,
            };
            if better {
                best = Some(c);
            }
        }
        from = i + word.len();
    }
    best
}

/// Moves the primary span onto `name` when the mapped position is only nearby.
fn refine(d: &mut MappedDiag, lines: &[&str], name: &str) {
    if name.is_empty() {
        return;
    }
    let line = lines.get(d.line as usize).copied().unwrap_or("");
    if word_at(line, d.col as usize) == Some(name) {
        d.end_col = d.col + name.chars().count() as u32;
        d.end_line = d.line;
        return;
    }
    if let Some(c) = find_word(line, name, d.col as usize) {
        d.col = c as u32;
        d.end_col = (c + name.chars().count()) as u32;
        d.end_line = d.line;
    }
}

fn refine_label(s: &mut Label, lines: &[&str], name: &str) {
    let line = lines.get(s.line as usize).copied().unwrap_or("");
    if let Some(c) = find_word(line, name, s.col as usize) {
        s.col = c as u32;
        s.end_col = (c + name.chars().count()) as u32;
    }
}

/// `val name` / `var name` declaring `name` at or before `line`: (line, column of the keyword).
fn find_decl(lines: &[&str], name: &str, line: u32, keyword: &str) -> Option<(u32, u32)> {
    for l in (0..=(line as usize).min(lines.len().saturating_sub(1))).rev() {
        let text = lines[l];
        let mut from = 0;
        while let Some(i) = text[from..].find(keyword) {
            let i = from + i;
            from = i + keyword.len();
            if text[..i].chars().next_back().is_some_and(is_ident_char) {
                continue;
            }
            let rest = &text[i + keyword.len()..];
            if !rest.starts_with(' ') {
                continue;
            }
            let rest = rest.trim_start();
            if rest.starts_with(name) && !rest[name.len()..].chars().next().is_some_and(is_ident_char) {
                return Some((l as u32, text[..i].chars().count() as u32));
            }
        }
        if l + 1 < line as usize && text.trim_start().starts_with("fun ") {
            break;
        }
    }
    None
}

/// The innermost call whose argument list contains `col`: (function name, argument index).
fn call_around(line: &str, col: usize) -> Option<(String, usize)> {
    let chars: Vec<char> = line.chars().collect();
    let mut depth = 0i32;
    let mut commas = 0;
    let mut i = col.min(chars.len());
    while i > 0 {
        i -= 1;
        match chars[i] {
            ')' | ']' | '}' => depth += 1,
            '[' | '{' if depth > 0 => depth -= 1,
            '[' | '{' => return None,
            '(' if depth > 0 => depth -= 1,
            '(' => {
                let name: String = chars[..i].iter().rev().take_while(|c| is_ident_char(**c)).collect::<Vec<_>>().into_iter().rev().collect();
                return (!name.is_empty() && !matches!(name.as_str(), "if" | "while" | "for" | "when")).then_some((name, commas));
            }
            ',' if depth == 0 => commas += 1,
            _ => {}
        }
    }
    None
}

/// Calls of `name(` on a line, method calls `x.name(` too when `methods`: (column after the
/// parenthesis, [(column offset, argument text)]).
fn calls_of(line: &str, name: &str, methods: bool) -> Vec<(usize, Vec<(usize, String)>)> {
    let mut out = Vec::new();
    let pat = format!("{}(", name);
    let mut from = 0;
    while let Some(i) = line[from..].find(&pat) {
        let i = from + i;
        from = i + pat.len();
        let before = line[..i].chars().next_back();
        if before.is_some_and(is_ident_char) || (!methods && before == Some('.')) || line[..i].trim_end().ends_with("fun") {
            continue;
        }
        let open = line[..i + pat.len()].chars().count();
        let args = split_args(&line.chars().skip(open).collect::<String>());
        out.push((open, args));
    }
    out
}

/// Splits `a, b(c, d), e) rest` at top-level commas up to the closing parenthesis.
fn split_args(s: &str) -> Vec<(usize, String)> {
    let mut out = Vec::new();
    let mut depth = 0;
    let mut start = 0;
    let chars: Vec<char> = s.chars().collect();
    for (i, &c) in chars.iter().enumerate() {
        match c {
            '(' | '[' | '{' | '<' => depth += 1,
            ')' | ']' | '}' | '>' if depth > 0 => depth -= 1,
            ')' => {
                push_arg(&chars, start, i, &mut out);
                return out;
            }
            ',' if depth == 0 => {
                push_arg(&chars, start, i, &mut out);
                start = i + 1;
            }
            _ => {}
        }
    }
    out
}

fn push_arg(chars: &[char], start: usize, end: usize, out: &mut Vec<(usize, String)>) {
    let text: String = chars[start..end].iter().collect();
    let lead = text.len() - text.trim_start().len();
    if !text.trim().is_empty() {
        out.push((start + text[..lead].chars().count(), text.trim().to_string()));
    }
}

/// The `fun` line enclosing `line`.
fn enclosing_fun<'a>(lines: &[&'a str], line: u32) -> Option<(u32, &'a str)> {
    (0..=(line as usize).min(lines.len().saturating_sub(1))).rev().find(|&l| lines[l].trim_start().starts_with("fun ") || lines[l].contains(" fun ")).map(|l| (l as u32, lines[l]))
}

/// Position of the ` {` that opens the enclosing function's body (to insert ` throws`).
fn enclosing_fun_brace(lines: &[&str], line: u32) -> Option<(u32, u32)> {
    let (l, text) = enclosing_fun(lines, line)?;
    if text.contains("throws") {
        return None;
    }
    let t = text.trim_end();
    let body = t.strip_suffix('{')?;
    let col = body.trim_end().chars().count() as u32;
    Some((l, col))
}

/// The parameter `name: Type` of the function enclosing `line`:
/// (line, column of the type, function name, parameter index).
fn param_decl(lines: &[&str], name: &str, line: u32) -> Option<(u32, u32, String, usize)> {
    let (l, text) = enclosing_fun(lines, line)?;
    let fun_at = text.find("fun ")?;
    let after = &text[fun_at + 4..];
    let fname: String = after.chars().take_while(|c| is_ident_char(*c)).collect();
    let open = text[fun_at..].find('(')? + fun_at;
    let params = split_args(&text[open + 1..]);
    let base = text[..open + 1].chars().count();
    for (index, (off, p)) in params.iter().enumerate() {
        let (pname, ty) = p.split_once(':')?;
        if pname.trim().trim_start_matches("var ").trim_start_matches("val ") == name {
            let ty_lead = ty.len() - ty.trim_start().len();
            let col = base + off + pname.chars().count() + 1 + ty[..ty_lead].chars().count();
            return Some((l, col as u32, fname, index));
        }
    }
    None
}

/// The line without a `//` comment at its end.
fn code(line: &str) -> &str {
    let (mut in_str, mut escaped) = (false, false);
    for (i, c) in line.char_indices() {
        match c {
            _ if escaped => escaped = false,
            '\\' if in_str => escaped = true,
            '"' => in_str = !in_str,
            '/' if !in_str && line[i + 1..].starts_with('/') => return &line[..i],
            _ => {}
        }
    }
    line
}

/// Does this line declare `fun name(` (outside a comment)?
fn declares_fun(line: &str, name: &str) -> bool {
    code(line).contains(&format!("fun {}(", name))
}

/// Parameter `index` of `fun name(...)` declared in these lines: (line, type start, type end, type).
fn fun_param(lines: &[&str], name: &str, index: usize) -> Option<(u32, u32, u32, String)> {
    let pat = format!("fun {}(", name);
    let l = lines.iter().position(|t| declares_fun(t, name))?;
    let text = lines[l];
    let open = text.find(&pat)? + pat.len();
    let params = split_args(&text[open..]);
    let base = text[..open].chars().count();
    let (off, p) = params.get(index)?;
    let (pname, ty) = p.split_once(':')?;
    let ty_text = ty.trim();
    let ty_text = ty_text.split(" = ").next().unwrap_or(ty_text).trim();
    let ty_lead = ty.len() - ty.trim_start().len();
    let col = base + off + pname.chars().count() + 1 + ty[..ty_lead].chars().count();
    Some((l as u32, col as u32, (col + ty_text.chars().count()) as u32, ty_text.to_string()))
}

/// Where and why the function `f` keeps its parameter number `index`: the file when it isn't
/// `current`, zero-based line and columns, and a clause like "it returns `v`". The rules that
/// decided it see the whole crate, as when it was compiled.
fn kept_reason(sources: &Sources, current: &Path, f: &str, index: usize) -> Option<(Option<PathBuf>, u32, u32, u32, String)> {
    let files: Vec<&oxie_compiler::ast::File> = sources.files.iter().map(|s| s.ast()).collect();
    let (i, span, why) = oxie_compiler::params::kept_reason(&files, f, index)?;
    let file = &sources.files[i];
    let idx = oxie_compiler::LineIndex::new(&file.text);
    let (l, c) = idx.line_col(&file.text, span.start as usize);
    let (el, ec) = idx.line_col(&file.text, span.end as usize);
    Some((sources.elsewhere(&file.path, current), l as u32, c as u32, if el == l { ec as u32 } else { c as u32 + 1 }, why))
}

/// Where and why the function `f` stores or returns its parameter number `index`, so lending it
/// can't work (`params::stored_reason`), like `kept_reason`.
fn stored_reason(sources: &Sources, current: &Path, f: &str, index: usize) -> Option<(Option<PathBuf>, u32, u32, u32, String)> {
    let files: Vec<&oxie_compiler::ast::File> = sources.files.iter().map(|s| s.ast()).collect();
    let (i, span, why) = oxie_compiler::params::stored_reason(&files, f, index)?;
    let file = &sources.files[i];
    let idx = oxie_compiler::LineIndex::new(&file.text);
    let (l, c) = idx.line_col(&file.text, span.start as usize);
    let (el, ec) = idx.line_col(&file.text, span.end as usize);
    Some((sources.elsewhere(&file.path, current), l as u32, c as u32, if el == l { ec as u32 } else { c as u32 + 1 }, why))
}

/// When the borrow that ends too early is an argument of a function declared in the crate whose
/// result Oxie tied to all its reference parameters, a hint with the signature that leaves this
/// argument out. Only a hint: Oxie can't tell whether the result really doesn't borrow from it.
fn lifetime_hint(d: &mut MappedDiag, sources: &Sources, lines: &[&str]) {
    let Some((f, index)) = call_around(lines.get(d.line as usize).copied().unwrap_or(""), d.col as usize) else { return };
    let Some(file) = sources.declaring(&d.file, |ls| narrower_lifetime(ls, &f, index).is_some()) else { return };
    let Some(sig) = narrower_lifetime(&file.lines(), &f, index) else { return };
    let place = match sources.elsewhere(&file.path, &d.file) {
        Some(p) => format!(" (in {})", p.file_name().unwrap_or_default().to_string_lossy()),
        None => String::new(),
    };
    d.notes.push(help(&format!(
        "`{}` returns a reference, and Oxie ties it to all of its reference parameters, so the result can't outlive this argument. If the result doesn't borrow from it, name the lifetime yourself{}: `{}`.",
        f, place, sig
    )));
}

/// The signature of the function `f` declared in these lines, when it returns a reference and has
/// no lifetimes written (so Oxie ties the result to all its reference parameters), with the
/// lifetime on every reference parameter except number `index`.
fn narrower_lifetime(lines: &[&str], f: &str, index: usize) -> Option<String> {
    let pat = format!("fun {}(", f);
    let text = code(lines.iter().find(|t| declares_fun(t, f))?).trim();
    if text.contains('\'') {
        return None;
    }
    let open = text.find(&pat)? + pat.len();
    let mut depth = 0;
    let close = text[open..].char_indices().find_map(|(i, c)| {
        match c {
            '(' | '[' | '{' | '<' => depth += 1,
            ')' if depth == 0 => return Some(open + i),
            ')' | ']' | '}' | '>' => depth -= 1,
            _ => {}
        }
        None
    })?;
    let ret = text[close + 1..].trim_start().strip_prefix(':')?.split(" = ").next()?.split(" {").next()?.trim();
    let params: Vec<(String, String)> = split_args(&text[open..]).into_iter().filter_map(|(_, p)| p.split_once(':').map(|(n, t)| (n.trim().to_string(), t.trim().to_string()))).collect();
    let refs = params.iter().filter(|(_, t)| t.starts_with('&')).count();
    if !ret.starts_with('&') || refs < 2 || !params.get(index)?.1.starts_with('&') {
        return None;
    }
    let tie = |t: &str| t.replacen('&', "&'a ", 1);
    let ps: Vec<String> = params.iter().enumerate().map(|(i, (n, t))| format!("{}: {}", n, if i != index && t.starts_with('&') { tie(t) } else { t.clone() })).collect();
    Some(format!("fun {}<'a>({}): {}", f, ps.join(", "), tie(ret)))
}

fn is_copy_type(ty: &str) -> bool {
    matches!(
        ty,
        "i8" | "i16" | "i32" | "i64" | "i128" | "isize" | "u8" | "u16" | "u32" | "u64" | "u128" | "usize" | "f32" | "f64" | "bool" | "char" | "Int" | "Long" | "Double" | "Float" | "Boolean" | "Char"
    )
}

/// The idiomatic borrowed form of an owned parameter type.
fn borrowed_type(ty: &str) -> String {
    if ty == "String" {
        return "&str".into();
    }
    if let Some(inner) = ty.strip_prefix("Vec<").and_then(|t| t.strip_suffix('>')) {
        return format!("&[{}]", inner);
    }
    format!("&{}", ty)
}

fn is_type_param(ty: &str) -> bool {
    let t = ty.trim_start_matches('&');
    t.len() <= 2 && t.chars().next().is_some_and(|c| c.is_ascii_uppercase())
}

/// How a type is declared in these lines: "data class", "class", "enum".
fn user_type_kind(lines: &[&str], name: &str) -> Option<&'static str> {
    for l in lines {
        let t = l.trim_start();
        for (prefix, kind) in [("data class ", "data class"), ("error class ", "data class"), ("enum class ", "enum"), ("error enum ", "enum"), ("enum ", "enum"), ("class ", "class"), ("struct ", "class")] {
            let t = t.trim_start_matches("pub ").trim_start_matches("private ").trim_start_matches("internal ");
            if let Some(rest) = t.strip_prefix(prefix) {
                if rest.starts_with(name) && !rest[name.len()..].chars().next().is_some_and(is_ident_char) {
                    return Some(kind);
                }
            }
        }
    }
    None
}

fn snake_case(s: &str) -> String {
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if c.is_uppercase() {
            if i > 0 {
                out.push('_');
            }
            out.extend(c.to_lowercase());
        } else {
            out.push(c);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn words_and_calls() {
        assert_eq!(find_word("    val n = consume(nums)", "nums", 0), Some(20));
        assert_eq!(word_at("consume(nums)", 8), Some("nums"));
        assert_eq!(call_around("    val n = consume(nums)", 20), Some(("consume".into(), 0)));
        assert_eq!(call_around("f(a, g(b), c)", 11), Some(("f".into(), 2)));
        assert_eq!(fun_param(&["fun consume(v: Vec<i32>): usize = v.len()"], "consume", 0), Some((0, 15, 23, "Vec<i32>".into())));
        assert_eq!(borrowed_type("Vec<i32>"), "&[i32]");
        assert_eq!(find_decl(&["fun main() {", "    val count = 0", "    count += 1"], "count", 2, "val"), Some((1, 4)));
        assert_eq!(param_decl(&["fun add_one(v: &Vec<i32>) {", "    v.push(1)"], "v", 1), Some((0, 15, "add_one".into(), 0)));
        assert_eq!(oxie_types("expected `Option<Vec<i32>>`"), "expected `Vec<i32>?`");
        assert_eq!(snake_case("toUpperCase"), "to_upper_case");
        let elvis = |s: &str| top_level_elvis(&s.chars().collect::<Vec<_>>());
        assert_eq!(elvis(r#"a?.b ?: "x""#), Some(5));
        assert_eq!(elvis(r#"f("?:", a ?: 1) ?: "x""#), Some(16));
        assert_eq!(elvis("a?.b"), None);
        assert_eq!(byte_offset("ab\ncdé f", 1, 4), 8);
        assert_eq!(between("the trait bound `String: ElvisOr<_>` is not satisfied", "the trait bound `", ": Elvis"), Some("String".into()));
        assert_eq!(code(r#"val s = "a // b" // note"#), r#"val s = "a // b" "#);
        assert_eq!(placeholder_len(r#"println("low: $low items")"#, 14), Some(4));
        assert_eq!(placeholder_len(r#"println("low: ${low.len()} items")"#, 14), Some(12));
        assert_eq!(placeholder_len(r#"println("low: ${low} items")"#, 15), None);
        assert_eq!(first_generic_arg("Vec<String>>"), "Vec<String>");
        assert_eq!(first_generic_arg("HashMap<K, V>, E>"), "HashMap<K, V>");
        assert_eq!(matching_brace(&["spawn { println(\"}\")", "  x }"], 0, 6), Some((1, 4)));
        assert_eq!(local_class(&["fun f(c: &mut Counter) {", "    val d = Counter()", "    val n: Vec<i32> = vec[]", "    val m = make()"], "d", 3), Some("Counter".into()));
        assert_eq!(local_class(&["fun f(c: &mut Counter) {", "    c.hit()"], "c", 1), Some("Counter".into()));
        assert_eq!(local_class(&["fun f() {", "    val m = make()"], "m", 1), None);
        assert!(declares_fun("fun keep(v: Vec<i32>) = v", "keep") && !declares_fun("// see fun keep(v)", "keep"));
        assert_eq!(element_type(&["fun f() {", "    var v = vec[1, 2, 3]", "    v[0] = 4"], "v", 2), Some("i32".into()));
        assert_eq!(element_type(&["    var w = vec[1.5, -2.0]"], "w", 0), Some("f64".into()));
        assert_eq!(element_type(&["    var w = vec[1, 2.5]"], "w", 0), None);
        assert_eq!(element_type(&["fun f(v: &mut Vec<u8>) {", "    v[0] = 1"], "v", 1), Some("u8".into()));
        assert_eq!(element_type(&["    var s: Vec<String> = vec[]"], "s", 0), Some("String".into()));
    }

    #[test]
    fn shapes_with_a_change_that_costs_nothing() {
        // the loop over `iter_mut()` and the body's statement, for the loop in `src`
        let looped = |src: &str| {
            let file = oxie_compiler::parser::parse_file(src).0;
            let mut found = None;
            oxie_compiler::visit::walk_items(&file.items, &mut |e| {
                if let ExprKind::For { pat: Pat::Tuple(ps), iter, body, .. } = &e.kind {
                    found = indexed_loop(src, ps, iter, body).map(|l| (l.head, l.stmt));
                }
                true
            });
            found
        };
        let body = |b: &str| looped(&format!("fun f(v: &mut Vec<i32>) {{\n    for ((i, x) in v.iter().enumerate()) {{ {} }}\n}}\n", b));
        assert_eq!(body("v[i] = x * 2"), Some(("x in v.iter_mut()".into(), Some("*x = *x * 2".into()))));
        assert_eq!(body("v[i] += x.pow(2) + i as i32"), Some(("(i, x) in v.iter_mut().enumerate()".into(), Some("*x += x.pow(2) + i as i32".into()))));
        // `x` passed on: the loop, without the statement
        assert_eq!(body("v[i] = g(x)"), Some(("x in v.iter_mut()".into(), None)));
        // another element, the list itself, or a change of something else
        assert_eq!(body("v[i] = v[0] + x"), None);
        assert_eq!(body("v[i] = x + v.len() as i32"), None);
        assert_eq!(body("n = x * 2"), None);

        let src = "class C(var m: HashMap<u32, String> = HashMap::new()) {\n    fun f(n: &mut BTreeMap<u32, u32>, s: &mut Vec<u32>) { m.len() }\n}\n";
        let file = oxie_compiler::parser::parse_file(src).0;
        let at = src.find("m.len()").unwrap() as u32;
        assert_eq!(map_declared(&file, at, "n"), Some(true));
        assert_eq!(map_declared(&file, at, "m"), Some(false));
        assert_eq!(map_declared(&file, at, "this.m"), Some(false));
        assert_eq!(map_declared(&file, at, "s"), None);

        let value = |v: &str| {
            let src = format!("fun f() {{ g({}) }}\n", v);
            let file = oxie_compiler::parser::parse_file(&src).0;
            let mut ok = None;
            oxie_compiler::visit::walk_items(&file.items, &mut |e| {
                if let ExprKind::Call { args, .. } = &e.kind {
                    ok = ok.or(Some(lambda_body_ok(&args[0].value, "cache")));
                }
                true
            });
            ok.unwrap()
        };
        assert!(value("load(id)"));
        assert!(!value("cache.len()") && !value("load(id)?") && !value("it + 1"));
    }

    /// A rustc error mapped to `file` (zero-based line and columns).
    fn diag(file: &str, code: &str, message: &str, line: u32, col: u32, end_col: u32) -> MappedDiag {
        MappedDiag {
            level: "error".into(),
            code: Some(code.into()),
            message: message.into(),
            file: PathBuf::from(file),
            line,
            col,
            end_line: line,
            end_col,
            label: None,
            notes: Vec::new(),
            from_oxie: true,
            secondary: Vec::new(),
            suggestions: Vec::new(),
            multipart: Vec::new(),
            raw_suggestions: Vec::new(),
            rust_message: None,
            gen_text: None,
            fixes: Vec::new(),
            covers_position: false,
        }
    }

    #[test]
    fn names_that_mean_the_opposite() {
        let opp = |a: &str, b: &str| opposite(&words(a), &words(b));
        let neg = |a: &str, b: &str| negates(&words(a), &words(b));
        assert!(neg("is_not_empty", "is_empty") && neg("is_empty", "isNotEmpty") && neg("has_no_items", "has_items") && neg("is_invalid", "is_valid"));
        assert!(opp("is_none", "is_some") && !neg("is_none", "is_some") && opp("to_lowercase", "to_uppercase"));
        // a negation, but not of a yes-or-no question, so `!` doesn't turn one into the other
        assert!(neg("filter_not", "filter") && !is_predicate(&words("filter")));
        // typos and other similar names
        assert!(!opp("is_emty", "is_empty") && !opp("lenght", "len") && !opp("push_str", "push") && !opp("to_owned", "to_string"));
    }

    #[test]
    fn iterators_by_the_names_rustc_gives_them() {
        for ty in ["std::slice::Iter<'a, T>", "Chars<'a>", "Map<I, F>", "std::ops::Range<Idx>", "impl Iterator<Item = i32>", "Box<dyn Iterator<Item = i32>>", "std::collections::hash_map::Keys<'a, K, V>"] {
            assert!(is_iterator(ty), "{}", ty);
        }
        for ty in ["&str", "String", "std::option::Option<T>", "std::collections::HashMap<K, V, S>", "geometry::Map"] {
            assert!(!is_iterator(ty), "{}", ty);
        }
        // collected, the items are the list's elements, not references to them
        assert!(values_iterator("std::str::Split<'a, P>") && values_iterator("Chars<'a>") && values_iterator("std::vec::IntoIter<T, A>"));
        assert!(!values_iterator("std::slice::Iter<'a, T>") && !values_iterator("Map<I, F>") && !values_iterator("std::ops::RangeFrom<Idx>"));
    }

    #[test]
    fn a_similar_name_that_means_the_opposite_is_negated() {
        // the line `value.is_not_empty()` is on, with the one fix offered for it, which rustc
        // suggests `is_empty` for
        let fixed = |line: &str| {
            let src = format!("fun f(value: &str) {{\n    {}\n}}\n", line);
            let col = 4 + line.find("is_not_empty").unwrap() as u32;
            let mut d = diag("/crate/main.oxi", "E0599", "no method named `is_not_empty` found for reference `&str` in the current scope", 1, col, col + 12);
            d.notes.push("help: there is a method `is_empty` with a similar name: `is_empty`".into());
            d.raw_suggestions.push("is_empty".into());
            d.suggestions.push(crate::cargo::Suggestion { line: 1, col, end_line: 1, end_col: col + 12, replacement: "is_empty".into() });
            explain(&mut d, &Sources::from_texts(&[("/crate/main.oxi", &src)]));
            let [fix] = &d.fixes[..] else { panic!("{:?}", d.fixes) };
            let mut chars: Vec<char> = src.lines().nth(1).unwrap().chars().collect();
            let mut edits = fix.edits.clone();
            edits.sort_by_key(|e| std::cmp::Reverse(e.col));
            for e in edits {
                chars.splice(e.col as usize..e.end_col as usize, e.text.chars());
            }
            chars.into_iter().collect::<String>().trim().to_string()
        };
        assert_eq!(fixed("if (value.is_not_empty()) g()"), "if (!value.is_empty()) g()");
        assert_eq!(fixed("if (!value.is_not_empty()) g()"), "if (value.is_empty()) g()");
        assert_eq!(fixed("val n = value.is_not_empty().then { 1 }"), "val n = (!value.is_empty()).then { 1 }");
        assert_eq!(fixed(r#"println("${value.is_not_empty()}")"#), r#"println("${!value.is_empty()}")"#);
    }

    #[test]
    fn an_ignored_error_is_explained_in_oxie_terms() {
        let src = "fun check(x: i32) throws { if (x < 0) throw \"negative\" }\nfun main() {\n    check(-1)\n}\n";
        let mut d = diag("/crate/main.oxi", "unused_must_use", "unused `Result` that must be used", 2, 4, 13);
        d.level = "warning".into();
        d.notes.push("help: use `let _ = ...` to ignore the resulting value: `let _ = `".into());
        explain(&mut d, &Sources::from_texts(&[("/crate/main.oxi", src)]));
        assert_eq!(d.message, "this call can fail, and its error is ignored");
        assert!(d.notes.iter().all(|n| !n.contains("let _")), "{:?}", d.notes);
        assert!(d.notes.iter().any(|n| n.contains("`fun main() throws`")), "{:?}", d.notes);
        let texts: Vec<&str> = d.fixes.iter().flat_map(|f| f.edits.iter().map(|e| e.text.as_str())).collect();
        assert_eq!(texts, [" throws", ".unwrap()"]);
    }

    #[test]
    fn suggestion_parts_in_oxie_terms() {
        assert_eq!(type_paths("Box::new("), "Box.new(");
        assert_eq!(type_paths("std::mem::take(&mut v)"), "std::mem::take(&mut v)");
        assert_eq!(type_paths("Vec::<i32>::new()"), "Vec::<i32>::new()");
        assert_eq!(oxie_code("Box::new("), Some("Box.new(".into()));
        assert_eq!(oxie_code(").into()"), Some(").into()".into()));
        assert_eq!(oxie_code("i64::from("), None);
        assert_eq!(oxie_code("let value = name.clone();\n"), None);
        assert_eq!(oxie_terms("store this in the heap by calling `Box::new`"), "store this in the heap by calling `Box.new`");
        assert_eq!(oxie_terms("to call the trait object stored in `run`, surround the field access with parentheses"), "to call the function stored in `run`, surround the field access with parentheses");
    }

    #[test]
    fn rust_places_in_oxie_words() {
        assert_eq!(oxie_place("self.name", "name"), Some("name".into()));
        assert_eq!(oxie_place("self.name", "this.name"), Some("this.name".into()));
        assert_eq!(oxie_place("self.address.city", "address.city"), Some("address.city".into()));
        assert_eq!(oxie_place("self.log", "&mut log)"), Some("log".into()));
        assert_eq!(oxie_place("self.log", "&this.log[0]"), Some("this.log".into()));
        assert_eq!(oxie_place("*self", "this"), Some("this".into()));
        assert_eq!(oxie_place("__self.items", "items"), Some("items".into()));
        assert_eq!(oxie_place("self.0", "0"), Some("this.0".into()));
        assert_eq!(oxie_place("selfie.name", "selfie.name"), None);
        assert_eq!(oxie_place("p.name", "p.name"), None);
        assert_eq!(oxie_places("a reference to `self.items` is still in use (`&mut self`)", "items"), "a reference to `items` is still in use (`&mut self`)");
    }

    #[test]
    fn qualified_clone_is_a_clone_call() {
        let part = |col: u32, text: &str| Suggestion { line: 0, col, end_line: 0, end_col: col, replacement: text.into() };
        let fix = |line: &str, start: u32, end: u32| clone_call(&[part(start, "<String as Clone>::clone(&"), part(end, ")")], &[line]);
        assert_eq!(fix("    return items[i + 1]", 11, 23), Some(vec![edit(0, 23, 23, ".clone()")]));
        assert_eq!(fix("    return (a + b)", 11, 18), Some(vec![edit(0, 18, 18, ".clone()")]));
        assert_eq!(fix("    return *r", 11, 13), Some(vec![edit(0, 11, 11, "("), edit(0, 13, 13, ").clone()")]));
        assert_eq!(clone_call(&[part(4, "Box::new("), part(9, ")")], &["val x = value"]), None);
    }

    #[test]
    fn callers_in_other_files_get_mut_but_not_methods_of_the_same_name() {
        let s = Sources::from_texts(&[
            ("/crate/text.oxi", "fun add_one(v: &Vec<i32>) {\n    v.push(1)\n}\n"),
            ("/crate/main.oxi", "class Tally(var n: i32 = 0) {\n    fun add_one(x: &i32) { n += *x }\n}\n\nfun main() {\n    var nums = vec[1]\n    add_one(&nums)\n    var t = Tally()\n    t.add_one(&nums[0])\n}\n"),
        ]);
        let mut d = diag("/crate/text.oxi", "E0596", "cannot borrow `*v` as mutable, as it is behind a `&` reference", 1, 4, 5);
        explain(&mut d, &s);
        let fix = d.fixes.iter().find(|f| f.title == "let `add_one` change `v` (`&mut`)").unwrap_or_else(|| panic!("{:?}", d.fixes));
        let at: Vec<(Option<&str>, u32, u32)> = fix.edits.iter().map(|e| (e.file.as_deref().and_then(|f| f.to_str()), e.line, e.col)).collect();
        // the parameter, and the call `add_one(&nums)` in main.oxi; not `t.add_one(&nums[0])`
        assert_eq!(at, vec![(None, 0, 15), (Some("/crate/main.oxi"), 6, 12)]);
    }

    #[test]
    fn declarations_are_found_in_other_files() {
        let s = Sources::from_texts(&[
            ("/crate/main.oxi", "fun main() {\n    keep(v)\n}\n"),
            ("/crate/a.oxi", "fun keep(v: Vec<i32>): Vec<i32> = v\nfun twice() {}\n"),
            ("/crate/b.oxi", "fun twice() {}\n"),
        ]);
        let declares = |name: &'static str| move |ls: &[&str]| ls.iter().any(|t| declares_fun(t, name));
        let main = Path::new("/crate/main.oxi");
        assert_eq!(s.declaring(main, declares("keep")).map(|f| f.path.clone()), Some(PathBuf::from("/crate/a.oxi")));
        assert_eq!(s.elsewhere(Path::new("/crate/a.oxi"), main), Some(PathBuf::from("/crate/a.oxi")));
        assert_eq!(s.elsewhere(main, main), None);
        // declared in two other files: which one is meant is unknown
        assert!(s.declaring(main, declares("twice")).is_none());
        // the error's own file wins
        assert_eq!(s.declaring(Path::new("/crate/b.oxi"), declares("twice")).map(|f| f.path.clone()), Some(PathBuf::from("/crate/b.oxi")));
    }
}
