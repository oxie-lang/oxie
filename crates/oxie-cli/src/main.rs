//! The `oxie` command line tool.

mod cargo;
mod explain;
mod from_rust;
mod guide;
mod locations;
mod lsp;
mod project;
mod sources;

use std::path::{Path, PathBuf};
use std::io::IsTerminal;
use std::process::{Command, ExitCode, Stdio};

use oxie_compiler::{diag, Severity};
use project::Project;

const HELP: &str = "oxie - compiler and tools for the Oxie programming language, which compiles to plain Rust

USAGE:
    oxie <command> [options]

COMMANDS:
    new <name> [--lib]     Create a new project
    init [--lib]           Turn the current directory into an Oxie project
    run [file.oxi] [--release] [-- args]
                           Build and run the project, or a single .oxi script
    build [--release]      Build the project
    check                  Type-check the project without producing binaries
    test [file.oxi] [name] [--release] [-- test options]
                           Run the `test \"...\" { }` blocks of the project, or of a script;
                           `name` picks the tests whose description contains it
    fmt [--check] [paths]  Format .oxi files
    emit <file.oxi> [--annotate]
                           Print the Rust code generated for a file; `--annotate` marks
                           each statement with the .oxi line it came from
    from-rust <file.rs> [-o out.oxi]
                           Translate Rust to Oxie (`-` reads stdin)
    from-rust <dir> -o <out dir>
                           Translate the .rs files under a directory as one crate
    guide [topic]          Print the guide to writing this version of Oxie (`topics` lists the rest)
    lsp                    Start the language server (stdio)
    clean                  Remove build artifacts
    version                Print version

Arguments after `--` are passed to the program (run), to cargo (build/check), or to the test
runner (test, such as `--nocapture`). `-r` is short for `--release`.
`oxie <command> --help` describes one command.";

/// `oxie <command> --help`: the command's entry in `HELP`.
fn command_help(cmd: &str) -> Option<String> {
    let cmd = if cmd == "format" { "fmt" } else { cmd };
    let commands = HELP.split("COMMANDS:\n").nth(1)?.split("\n\n").next()?;
    // `    new <name> [--lib]     Create a new project`, the description maybe on the next lines;
    // a command used in several ways (`from-rust <file.rs>`, `from-rust <dir>`) has an entry for each
    let mut entries: Vec<(&str, Vec<&str>)> = Vec::new();
    let mut in_entry = false;
    for line in commands.lines() {
        match line.strip_prefix("    ").filter(|l| !l.starts_with(' ')) {
            Some(l) if l.split_whitespace().next() == Some(cmd) => {
                let (u, a) = l.split_once("  ").unwrap_or((l, ""));
                entries.push((u, vec![a.trim()]));
                in_entry = true;
            }
            Some(_) => in_entry = false,
            None if in_entry => entries.last_mut().unwrap().1.push(line.trim()),
            _ => {}
        }
    }
    let about = |lines: &[&str]| lines.iter().filter(|a| !a.is_empty()).copied().collect::<Vec<_>>().join(" ");
    match entries.as_slice() {
        [] => None,
        [(usage, lines)] => Some(format!("USAGE:\n    oxie {}\n\n{}", usage, about(lines))),
        _ => Some(format!("USAGE:\n{}", entries.iter().map(|(u, lines)| format!("    oxie {}\n        {}", u, about(lines))).collect::<Vec<_>>().join("\n"))),
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cmd = args.first().map(|s| s.as_str()).unwrap_or("help");
    let rest = if args.is_empty() { &[][..] } else { &args[1..] };
    // `--help` describes the command and never runs it (`oxie init --help` would set up the
    // current directory); after `--` it belongs to the program
    let before_dashdash = rest.iter().take_while(|a| *a != "--");
    if before_dashdash.clone().any(|a| a == "--help" || a == "-h") {
        let name = if cmd.ends_with(".oxi") { "run" } else { cmd };
        if let Some(h) = command_help(name) {
            println!("{}", h);
            return ExitCode::SUCCESS;
        }
    }
    let r = match cmd {
        "new" => cmd_new(rest),
        "init" => cmd_init(rest),
        "run" => cmd_run(rest),
        "build" => cmd_build(rest, "build"),
        "check" => cmd_build(rest, "check"),
        "test" => cmd_test(rest),
        "fmt" | "format" => cmd_fmt(rest),
        "emit" => cmd_emit(rest),
        "from-rust" => from_rust::run(rest),
        "guide" => guide::run(rest),
        "lsp" => {
            lsp::run();
            Ok(ExitCode::SUCCESS)
        }
        "clean" => cmd_clean(rest),
        "version" | "--version" | "-V" => {
            println!("oxie {}", env!("CARGO_PKG_VERSION"));
            Ok(ExitCode::SUCCESS)
        }
        "help" | "--help" | "-h" => {
            println!("{}", HELP);
            Ok(ExitCode::SUCCESS)
        }
        f if f.ends_with(".oxi") => cmd_run(&args),
        other => {
            eprintln!("error: unknown command `{}`\n\n{}", other, HELP);
            Ok(ExitCode::from(2))
        }
    };
    match r {
        Ok(c) => c,
        Err(e) => {
            eprintln!("error: {}", e);
            ExitCode::FAILURE
        }
    }
}

type R = Result<ExitCode, String>;

fn cwd() -> PathBuf {
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
}

fn find_project() -> Result<Project, String> {
    let dir = cwd();
    Project::find(&dir).ok_or_else(|| {
        // a virtual workspace has members but no package of its own
        let workspace = dir.ancestors().map(|d| d.join("Cargo.toml")).find(|m| std::fs::read_to_string(m).is_ok_and(|t| t.lines().any(|l| l.trim() == "[workspace]")));
        match workspace {
            Some(m) => format!("{} is a workspace without a package of its own; run `oxie` in the directory of one of its members", m.display()),
            None => "no Cargo.toml found (run `oxie new <name>` to create a project, or `oxie run file.oxi` for a script)".to_string(),
        }
    })
}

/// The arguments of `oxie <command>`: the flags given (by their first spelling), the other
/// arguments, and those after `--`.
struct Args {
    flags: Vec<&'static str>,
    positional: Vec<String>,
    rest: Vec<String>,
}

impl Args {
    fn has(&self, flag: &str) -> bool {
        self.flags.contains(&flag)
    }
}

/// Splits the arguments of `oxie <cmd>`. `known` lists its flags, each with its other spellings
/// (`&["--release", "-r"]`); any other argument starting with `-` before `--` is an error that
/// names it, so a misspelled flag (`--relase`) never runs something else.
fn parse_args(cmd: &str, args: &[String], known: &[&[&'static str]]) -> Result<Args, String> {
    let (before, rest) = match args.iter().position(|a| a == "--") {
        Some(i) => (&args[..i], args[i + 1..].to_vec()),
        None => (args, Vec::new()),
    };
    let mut parsed = Args { flags: Vec::new(), positional: Vec::new(), rest };
    for a in before {
        // not an option: `-` (stdin), and a test's description like "-1 is negative"
        if !a.starts_with('-') || a == "-" || a.contains(char::is_whitespace) || a[1..].starts_with(|c: char| c.is_ascii_digit()) {
            parsed.positional.push(a.clone());
            continue;
        }
        match known.iter().find(|spellings| spellings.contains(&a.as_str())) {
            Some(spellings) => parsed.flags.push(spellings[0]),
            None => {
                let close = known.iter().flat_map(|s| s.iter()).filter(|k| k.starts_with("--")).find(|k| edit_distance(k, a) <= 2 || (a.len() > 3 && k.starts_with(a.as_str())));
                let hint = match (close, known) {
                    (Some(k), _) => format!("; did you mean `{}`?", k),
                    (None, []) => format!("; `oxie {}` takes no options", cmd),
                    (None, [only]) => format!("; its only option is `{}`", only[0]),
                    (None, _) => format!("; its options are {}", known.iter().map(|s| format!("`{}`", s[0])).collect::<Vec<_>>().join(", ")),
                };
                return Err(format!("unknown option `{}` for `oxie {}`{}", a, cmd, hint));
            }
        }
    }
    Ok(parsed)
}

/// The number of single-character edits between two words.
fn edit_distance(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut row: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.chars().enumerate() {
        let mut prev = row[0];
        row[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let cur = row[j + 1];
            row[j + 1] = (prev + usize::from(ca != *cb)).min(row[j] + 1).min(cur + 1);
            prev = cur;
        }
    }
    row[b.len()]
}

/// An error for arguments a command doesn't take.
fn unexpected(cmd: &str, extra: &[String]) -> Result<(), String> {
    match extra.first() {
        None => Ok(()),
        Some(a) => Err(format!("unexpected argument `{}` for `oxie {}` (see `oxie {} --help`)", a, cmd, cmd)),
    }
}

/// Checks that a new package can be called `name`, as cargo requires.
fn check_package_name(name: &str) -> Result<(), String> {
    const KEYWORDS: &[&str] = &[
        "as", "async", "await", "break", "const", "continue", "crate", "dyn", "else", "enum", "extern", "false", "fn", "for", "if", "impl", "in", "let", "loop", "match", "mod", "move", "mut", "pub", "ref", "return", "self", "Self", "static", "struct", "super", "trait", "true", "type", "unsafe", "use", "where", "while", "abstract", "become", "box", "do", "final", "gen", "macro", "override", "priv", "try", "typeof", "unsized", "virtual", "yield",
    ];
    const LIBRARIES: &[&str] = &["std", "core", "alloc", "proc_macro", "proc-macro", "test"];
    let why = if name.is_empty() {
        "it is empty".to_string()
    } else if let Some(c) = name.chars().find(|c| !(c.is_ascii_alphanumeric() || *c == '_' || *c == '-')) {
        format!("it contains `{}` (use letters, digits, `_` and `-`)", c)
    } else if name.starts_with(|c: char| c.is_ascii_digit()) {
        "it starts with a digit".to_string()
    } else if name.starts_with('-') {
        "it starts with `-`".to_string()
    } else if KEYWORDS.contains(&name) {
        "it is a Rust keyword".to_string()
    } else if LIBRARIES.contains(&name) {
        "it is the name of a Rust library".to_string()
    } else {
        return Ok(());
    };
    Err(format!("`{}` can't be the name of a package: {}", name, why))
}

fn cmd_new(args: &[String]) -> R {
    let a = parse_args("new", args, &[&["--lib"]])?;
    let lib = a.has("--lib");
    let (name, extra) = a.positional.split_first().ok_or("usage: oxie new <name> [--lib]")?;
    unexpected("new", extra)?;
    // `.`, `..` and `/` name no new directory
    let file_name = Path::new(name).file_name().ok_or_else(|| {
        format!("`oxie new {}` needs the name of a new directory; to make an existing directory a project, run `oxie init` in it", name)
    })?;
    let dir = cwd().join(name);
    if dir.join("Cargo.toml").exists() {
        return Err(format!("{} already contains a Cargo.toml", dir.display()));
    }
    let pkg = file_name.to_string_lossy().replace(' ', "_");
    check_package_name(&pkg)?;
    project::create(&dir, &pkg, lib).map_err(|e| e.to_string())?;
    println!("Created {} `{}`", if lib { "library" } else { "program" }, pkg);
    println!("\n    cd {}\n    oxie {}\n", name, if lib { "test" } else { "run" });
    Ok(ExitCode::SUCCESS)
}

fn cmd_init(args: &[String]) -> R {
    let a = parse_args("init", args, &[&["--lib"]])?;
    unexpected("init", &a.positional)?;
    let lib = a.has("--lib");
    let dir = cwd();
    let pkg = dir.file_name().map(|n| n.to_string_lossy().replace(' ', "_")).unwrap_or_else(|| "app".into());
    if dir.join("Cargo.toml").exists() {
        // existing Rust project: add build script support
        let manifest = std::fs::read_to_string(dir.join("Cargo.toml")).map_err(|e| e.to_string())?;
        if !manifest.contains("oxie-build") {
            let m = project::add_build_dependency(&manifest, &project::oxie_build_dep());
            std::fs::write(dir.join("Cargo.toml"), m).map_err(|e| e.to_string())?;
        }
        if !dir.join("build.rs").exists() {
            std::fs::write(dir.join("build.rs"), project::BUILD_RS).map_err(|e| e.to_string())?;
        }
        // notes for AI coding agents; a crate's own CLAUDE.md or AGENTS.md is kept
        for (name, text) in [("AGENTS.md", project::AGENTS_MD), ("CLAUDE.md", project::CLAUDE_MD)] {
            if !dir.join(name).exists() {
                std::fs::write(dir.join(name), text).map_err(|e| e.to_string())?;
            }
        }
        println!("Added Oxie to the existing crate. Write modules as src/<name>.oxi and add this to src/main.rs or src/lib.rs:\n\n    include!(concat!(env!(\"OUT_DIR\"), \"/oxie/mod.rs\"));\n");
        return Ok(ExitCode::SUCCESS);
    }
    check_package_name(&pkg).map_err(|e| format!("{} (the package is named after the directory)", e))?;
    project::create(&dir, &pkg, lib).map_err(|e| e.to_string())?;
    println!("Initialized Oxie project `{}`", pkg);
    Ok(ExitCode::SUCCESS)
}

/// Prints Oxie-level diagnostics. Returns false if there are errors.
fn report(out: &oxie_compiler::Output, root: &Path) -> bool {
    let color = cargo::color();
    let mut ok = true;
    for (path, text, diags) in &out.diags {
        let shown = path.strip_prefix(root).unwrap_or(path).display().to_string();
        for d in diags {
            if d.severity == Severity::Error {
                ok = false;
            }
            eprintln!("{}", diag::render(d, &shown, text, color));
        }
    }
    ok
}

fn precheck(p: &Project) -> Result<bool, String> {
    if let Some(warning) = project::version_warning(p) {
        eprintln!("warning: {warning}");
    }
    let out = p.check_sources().map_err(|e| e.to_string())?;
    Ok(report(&out, &p.root))
}

/// The project to build for `oxie run`/`oxie test` with an optional `.oxi` file: the file's
/// project when it is in a project's `src/`, else the hidden project of the script (also
/// returned).
fn project_of(file: Option<&str>) -> Result<(Project, Option<PathBuf>), String> {
    let Some(f) = file else { return Ok((find_project()?, None)) };
    let path = PathBuf::from(f);
    if !path.is_file() {
        return Err(format!("{} not found", f));
    }
    let abs = oxie_compiler::compile::canonicalize(&path).map_err(|e| format!("{}: {}", f, e))?;
    match abs.parent().and_then(Project::find) {
        Some(p) if abs.starts_with(p.src_dir()) && p.manifest.contains("oxie-build") => Ok((p, None)),
        _ => Ok((project::script_project(&abs).map_err(|e| format!("{}: {}", f, e))?, Some(abs))),
    }
}

/// How a script is named in cargo's messages instead of its hidden crate: by its file.
fn script_name(script: Option<&Path>) -> Option<String> {
    script.map(|s| s.strip_prefix(cwd()).unwrap_or(s).display().to_string())
}

/// Checks a script with the Oxie compiler before cargo builds it, like `precheck`. A script runs
/// its `fun main`, and `oxie test` may test one that only has `test` blocks.
fn precheck_script(proj: &Project, abs: &Path, testing: bool) -> Result<bool, String> {
    let src = std::fs::read_to_string(abs).map_err(|e| format!("{}: {}", abs.display(), e))?;
    let inputs = project::script_inputs(abs).map_err(|e| e.to_string())?;
    let out = oxie_compiler::compile(inputs, &project::script_options(&proj.name, &project::script_deps(&src)));
    if !report(&out, &cwd()) {
        return Ok(false);
    }
    let (file, _) = oxie_compiler::parser::parse_file(&src);
    let has = |f: &dyn Fn(&oxie_compiler::ast::ItemKind) -> bool| file.items.iter().any(|i| f(&i.kind));
    // also a `fn main` kept as Rust in a `rust { }` block
    let has_main = has(&|k| match k {
        oxie_compiler::ast::ItemKind::Fun(f) => f.name == "main" && f.receiver.is_none(),
        oxie_compiler::ast::ItemKind::Raw(r) => r.lines().any(|l| l.trim_start().trim_start_matches("pub ").starts_with("fn main(")),
        _ => false,
    });
    let has_tests = has(&|k| matches!(k, oxie_compiler::ast::ItemKind::Test(_)));
    if has_main || (testing && has_tests) {
        return Ok(true);
    }
    let help = if has_tests {
        format!("add `fun main() {{ ... }}`, which runs when the script runs; `oxie test {}` runs its tests", abs.file_name().unwrap_or_default().to_string_lossy())
    } else if testing {
        "add `test \"...\" { ... }` blocks for `oxie test`, or `fun main() { ... }` to run the script".to_string()
    } else {
        "add `fun main() { ... }`, which runs when the script runs".to_string()
    };
    let what = if testing { "neither `fun main` nor `test` blocks" } else { "no `fun main`" };
    let d = oxie_compiler::Diag::error(oxie_compiler::diag::Span::new(0, 0), format!("the script has {}", what)).with_help(help);
    let shown = abs.strip_prefix(cwd()).unwrap_or(abs).display().to_string();
    eprintln!("{}", diag::render(&d, &shown, &src, cargo::color()));
    Ok(false)
}

fn cmd_build(args: &[String], sub: &str) -> R {
    let a = parse_args(sub, args, &[&["--release", "-r"]])?;
    unexpected(sub, &a.positional)?;
    let p = find_project()?;
    if !precheck(&p)? {
        return Ok(ExitCode::FAILURE);
    }
    let mut cargs = vec![sub.to_string()];
    if a.has("--release") {
        cargs.push("--release".into());
    }
    cargs.extend(a.rest);
    let r = cargo::cargo_json(&p.root, &cargs, false, None).map_err(|e| e.to_string())?;
    Ok(if r.success { ExitCode::SUCCESS } else { ExitCode::FAILURE })
}

fn cmd_run(args: &[String]) -> R {
    let a = parse_args("run", args, &[&["--release", "-r"]])?;
    let release = a.has("--release");
    let prog_args = a.rest;
    let (file, extra) = match a.positional.split_first() {
        Some((f, extra)) if f.ends_with(".oxi") => (Some(f.clone()), extra),
        _ => (None, &a.positional[..]),
    };
    if let Some(arg) = extra.first() {
        let run = file.as_ref().map_or("oxie run".to_string(), |f| format!("oxie run {}", f));
        return Err(format!("unexpected argument `{}`: arguments for the program go after `--`: `{} -- {}`", arg, run, extra.join(" ")));
    }
    let (proj, script) = project_of(file.as_deref())?;
    // Oxie diagnostics first
    let ok = match &script {
        Some(abs) => precheck_script(&proj, abs, false)?,
        None => precheck(&proj)?,
    };
    if !ok {
        return Ok(ExitCode::FAILURE);
    }
    let mut cargs = vec!["build".to_string()];
    if release {
        cargs.push("--release".into());
    }
    if script.is_some() {
        std::env::set_var("CARGO_TARGET_DIR", project::cache_dir().join("target"));
    }
    let r = cargo::cargo_json(&proj.root, &cargs, script.is_some(), script_name(script.as_deref()).as_deref()).map_err(|e| e.to_string())?;
    if !r.success {
        return Ok(ExitCode::FAILURE);
    }
    let exe = r
        .executables
        .iter()
        .find(|e| e.file_stem().is_some_and(|s| s.to_string_lossy() == proj.name.replace('-', "_") || s.to_string_lossy() == proj.name))
        .or(r.executables.last())
        .cloned()
        .ok_or("no executable was built (is this a library? try `oxie test`)")?;
    // stderr goes through the location rewriter, so panics point at the .oxi source
    let mut child = Command::new(&exe).args(&prog_args).stderr(Stdio::piped()).spawn().map_err(|e| format!("cannot run {}: {}", exe.display(), e))?;
    let err = child.stderr.take().unwrap();
    let forwarder = std::thread::spawn(move || locations::forward(err, std::io::stderr()));
    let status = child.wait().map_err(|e| e.to_string())?;
    let _ = forwarder.join();
    Ok(match status.code() {
        Some(0) => ExitCode::SUCCESS,
        Some(c) => ExitCode::from(c.clamp(1, 255) as u8),
        None => ExitCode::FAILURE,
    })
}

fn cmd_test(args: &[String]) -> R {
    let a = parse_args("test", args, &[&["--release", "-r"]])?;
    let file = a.positional.iter().find(|a| a.ends_with(".oxi")).cloned();
    // `test "adds numbers"` becomes the Rust test `test_adds_numbers`; accept the readable name too
    let filter: Vec<String> = a.positional.iter().filter(|a| !a.ends_with(".oxi")).map(|f| test_filter(f)).collect();
    let (p, script) = project_of(file.as_deref())?;
    let ok = match &script {
        Some(abs) => precheck_script(&p, abs, true)?,
        None => precheck(&p)?,
    };
    if !ok {
        return Ok(ExitCode::FAILURE);
    }
    if script.is_some() {
        std::env::set_var("CARGO_TARGET_DIR", project::cache_dir().join("target"));
    }
    let mut cargs = vec!["test".to_string(), "--no-run".to_string()];
    if a.has("--release") {
        cargs.push("--release".into());
    }
    let r = cargo::cargo_json(&p.root, &cargs, true, script_name(script.as_deref()).as_deref()).map_err(|e| e.to_string())?;
    if !r.success {
        return Ok(ExitCode::FAILURE);
    }
    // the warnings of the build are shown; cargo shows them again when it runs the tests, as
    // JSON lines on stdout with this format, which are dropped
    let mut cmd = Command::new(std::env::var("CARGO").unwrap_or_else(|_| "cargo".into()));
    cmd.args(["test", "--quiet", "--message-format=json"]).current_dir(&p.root);
    if a.has("--release") {
        cmd.arg("--release");
    }
    // the output goes through the location rewriter (failed tests print panics on stdout), so
    // ask for the colors a terminal would get
    let mut harness: Vec<String> = filter.into_iter().chain(a.rest).collect();
    // `never` too, or a `CARGO_TERM_COLOR=always` in the environment colors what `NO_COLOR` or a
    // pipe asked to keep plain
    cmd.arg(if cargo::color() { "--color=always" } else { "--color=never" });
    if std::io::stdout().is_terminal() && std::env::var("NO_COLOR").is_err() && !harness.iter().any(|a| a.starts_with("--color")) {
        harness.push("--color=always".into());
    }
    if !harness.is_empty() {
        cmd.arg("--");
        cmd.args(&harness);
    }
    let mut child = cmd.stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().map_err(|e| e.to_string())?;
    let (out, err) = (child.stdout.take().unwrap(), child.stderr.take().unwrap());
    let forwarders = [
        std::thread::spawn(move || locations::forward(locations::EditLines::new(out, locations::is_cargo_message, |_| None), std::io::stdout())),
        // cargo's "to rerun pass `--bin <name>`" names a cargo target, which `oxie test` doesn't take
        std::thread::spawn(move || locations::forward(locations::EditLines::new(err, |_| true, locations::without_rerun_hint), std::io::stderr())),
    ];
    let status = child.wait().map_err(|e| e.to_string())?;
    for f in forwarders {
        let _ = f.join();
    }
    Ok(if status.success() { ExitCode::SUCCESS } else { ExitCode::FAILURE })
}

/// The libtest filter for a test named in `oxie test`: the description of a `test "..."` block,
/// or a part of it, as the compiler names its function (lowercase, `_` between words). A module
/// path (`math::adds`) keeps its `::`.
fn test_filter(name: &str) -> String {
    let slug = |part: &str| {
        let mut s: String = part.chars().map(|c| if c.is_alphanumeric() { c.to_ascii_lowercase() } else { '_' }).collect();
        while s.contains("__") {
            s = s.replace("__", "_");
        }
        s.trim_matches('_').to_string()
    };
    name.split("::").map(slug).collect::<Vec<_>>().join("::")
}

fn collect_oxie(path: &Path, out: &mut Vec<PathBuf>) {
    if path.is_dir() {
        if path.file_name().is_some_and(|n| n == "target" || n.to_string_lossy().starts_with('.')) {
            return;
        }
        if let Ok(rd) = std::fs::read_dir(path) {
            let mut entries: Vec<_> = rd.filter_map(|e| e.ok()).map(|e| e.path()).collect();
            entries.sort();
            for p in entries {
                collect_oxie(&p, out);
            }
        }
    } else if path.extension().is_some_and(|e| e == "oxi") {
        out.push(path.to_path_buf());
    }
}

fn cmd_fmt(args: &[String]) -> R {
    let a = parse_args("fmt", args, &[&["--check"]])?;
    unexpected("fmt", &a.rest)?;
    let check = a.has("--check");
    let paths: Vec<PathBuf> = a.positional.iter().map(PathBuf::from).collect();
    let mut files = Vec::new();
    if paths.is_empty() {
        // only a project's sources: outside one, formatting everything below the current
        // directory could reach unrelated files
        let p = Project::find(&cwd()).ok_or("not in an Oxie project (no Cargo.toml found): name the files or directories to format, like `oxie fmt .`")?;
        collect_oxie(&p.src_dir(), &mut files);
    } else {
        for p in &paths {
            if !p.exists() {
                return Err(format!("{}: no such file or directory", p.display()));
            }
            if p.is_file() && p.extension().is_none_or(|e| e != "oxi") {
                return Err(format!("{}: not an .oxi file", p.display()));
            }
            collect_oxie(p, &mut files);
        }
    }
    let color = cargo::color();
    let (mut changed, mut unchanged, mut failed) = (0, 0, 0);
    for f in &files {
        let text = std::fs::read_to_string(f).map_err(|e| format!("{}: {}", f.display(), e))?;
        let formatted = match oxie_compiler::format::format_checked(&text) {
            Ok(formatted) => formatted,
            Err(errors) => {
                failed += 1;
                for d in &errors {
                    eprintln!("{}", diag::render(d, &f.display().to_string(), &text, color));
                }
                continue;
            }
        };
        if formatted == text {
            unchanged += 1;
            continue;
        }
        changed += 1;
        if check {
            println!("would reformat {}", f.display());
        } else {
            std::fs::write(f, formatted).map_err(|e| format!("{}: {}", f.display(), e))?;
            println!("formatted {}", f.display());
        }
    }
    if failed > 0 {
        eprintln!("error: {} file(s) not formatted because of the errors above", failed);
    }
    if !check && changed == 0 && unchanged > 0 {
        println!("{} file(s) already formatted", unchanged);
    } else if files.is_empty() {
        println!("no .oxi files found");
    }
    Ok(if failed > 0 || (check && changed > 0) { ExitCode::FAILURE } else { ExitCode::SUCCESS })
}

fn cmd_emit(args: &[String]) -> R {
    let a = parse_args("emit", args, &[&["--annotate"]])?;
    let (file, extra) = a.positional.split_first().ok_or("usage: oxie emit <file.oxi> [--annotate]")?;
    unexpected("emit", extra)?;
    unexpected("emit", &a.rest)?;
    let annotate = a.has("--annotate");
    let abs = oxie_compiler::compile::canonicalize(file).map_err(|e| format!("{}: {}", file, e))?;
    let project = Project::find(abs.parent().unwrap()).filter(|p| abs.starts_with(p.src_dir()));
    let (out, root) = match &project {
        Some(p) => {
            let inputs = oxie_compiler::discover(&p.src_dir()).map_err(|e| e.to_string())?;
            (oxie_compiler::compile(inputs, &p.options(p.root.join("target/oxie"))), p.root.clone())
        }
        None => {
            let inputs = project::script_inputs(&abs).map_err(|e| e.to_string())?;
            let src = std::fs::read_to_string(&abs).unwrap_or_default();
            (oxie_compiler::compile(inputs, &project::script_options("main", &project::script_deps(&src))), cwd())
        }
    };
    report(&out, &root);
    let idx = out.diags.iter().position(|(p, _, _)| *p == abs).unwrap_or(0);
    if let Some(f) = out.files.get(idx) {
        if annotate {
            print!("{}", annotated(f));
        } else {
            print!("{}", f.text);
        }
    }
    Ok(if out.has_errors() { ExitCode::FAILURE } else { ExitCode::SUCCESS })
}

/// The generated text with a `// file.oxi:line` comment above each line where code from
/// another source line starts, and a `/* oxie: ... */` comment after each choice the source
/// doesn't spell out (an inferred `&mut self`, a lent parameter, a `&` added to an argument).
fn annotated(f: &oxie_compiler::compile::GenFile) -> String {
    let Some(map) = &f.map else { return f.text.clone() };
    let name = map.source.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    let mut out = String::with_capacity(f.text.len() * 2);
    let mut last = None;
    for (i, line) in f.text.lines().enumerate() {
        let indent = line.len() - line.trim_start().len();
        let at = map.entries.partition_point(|e| (e.0, e.1) < (i as u32, indent as u32));
        if let Some(&(gl, gc, sl, _)) = map.entries.get(at) {
            if (gl, gc) == (i as u32, indent as u32) && last != Some(sl) {
                out += &format!("{}// {}:{}\n", &line[..indent], name, sl + 1);
                last = Some(sl);
            }
        }
        // from the right, so the columns of the others stay valid
        let mut line = line.to_string();
        let mut here: Vec<_> = f.decisions.iter().filter(|d| d.gen_line == i as u32).collect();
        here.sort_by_key(|d| std::cmp::Reverse(d.gen_col));
        for d in here {
            let at = line.char_indices().nth(d.gen_col as usize).map_or(line.len(), |(b, _)| b);
            line.insert_str(at, &format!(" /* oxie: {} because {} */", d.chosen, d.why));
        }
        out += &line;
        out.push('\n');
    }
    out
}

fn cmd_clean(args: &[String]) -> R {
    let a = parse_args("clean", args, &[])?;
    unexpected("clean", &a.positional)?;
    let p = find_project()?;
    let status = Command::new("cargo").arg("clean").current_dir(&p.root).status().map_err(|e| e.to_string())?;
    Ok(if status.success() { ExitCode::SUCCESS } else { ExitCode::FAILURE })
}
