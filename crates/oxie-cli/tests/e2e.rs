//! End-to-end tests: every `tests/e2e/*.oxi` program is built with `oxie run` and its stdout
//! compared with the matching `.out` file. Programs in `tests/diagnostics` must fail to compile
//! with the errors listed in their `// error: <line>: <text>` comments, and without those in
//! `// not: <line>: <text>` comments.

mod common;

use common::parallel;
use std::path::{Path, PathBuf};
use std::process::Command;

fn repo() -> PathBuf {
    oxie_compiler::compile::canonicalize(&PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")).unwrap()
}

fn oxie() -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_oxie"));
    // scripts and new projects build with this checkout's oxie-build, not the one on crates.io
    c.env("NO_COLOR", "1").env("OXIE_HOME", repo());
    c
}

fn programs(dir: &Path) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = std::fs::read_dir(dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().map_or(false, |e| e == "oxi"))
        .collect();
    v.sort();
    v
}

#[test]
fn e2e_programs() {
    run_programs("tests/e2e");
}

/// One small program per Rust feature (generics, lifetimes, GATs, unions, FFI, let chains, ...):
/// Oxie must accept the Rust feature and produce the expected output.
#[test]
fn rust_feature_coverage() {
    run_programs("tests/coverage");
}

fn run_programs(rel: &str) {
    let dir = repo().join(rel);
    let results = parallel(&programs(&dir), |p, cache| {
        let Ok(expected) = std::fs::read_to_string(p.with_extension("out")) else { return None };
        let out = oxie().arg("run").arg(p).current_dir(&dir).env("OXIE_CACHE", cache).output().unwrap();
        let stdout = String::from_utf8_lossy(&out.stdout);
        (stdout != expected).then(|| {
            format!(
                "{}:\n--- expected\n{}\n--- actual\n{}\n--- stderr\n{}",
                p.display(),
                expected,
                stdout,
                String::from_utf8_lossy(&out.stderr)
            )
        })
    });
    let failures: Vec<String> = results.into_iter().flatten().collect();
    assert!(failures.is_empty(), "{} program(s) failed:\n{}", failures.len(), failures.join("\n\n"));
}

/// Programs in `tests/diagnostics`, and projects there (directories with a `Cargo.toml`, built
/// with `oxie build`) whose errors are explained from declarations in other files. A comment in
/// any `.oxi` file of a project names a line of that file.
#[test]
fn diagnostics_point_at_oxie_sources() {
    let dir = repo().join("tests/diagnostics");
    let mut items = programs(&dir);
    let mut projects: Vec<PathBuf> = std::fs::read_dir(&dir).unwrap().filter_map(|e| e.ok()).map(|e| e.path()).filter(|p| p.join("Cargo.toml").exists()).collect();
    projects.sort();
    items.extend(projects);
    let results = parallel(&items, |p, cache| {
        let mut failures = Vec::new();
        // (file name, source) of the files whose comments list the errors
        let files: Vec<(String, String)> = if p.is_dir() { programs(&p.join("src")) } else { vec![p.clone()] }
            .iter()
            .map(|f| (f.file_name().unwrap().to_string_lossy().to_string(), std::fs::read_to_string(f).unwrap()))
            .collect();
        // (`file:line:`, or `file:line:col` and its line end for `// error: <line>:<col>: <text>`;
        // the text)
        let comments = |prefix: &str| -> Vec<(String, String)> {
            let mut out = Vec::new();
            for (name, src) in &files {
                for rest in src.lines().filter_map(|l| l.trim().strip_prefix(prefix)) {
                    let (line, text) = rest.trim().split_once(':').unwrap();
                    let loc = match text.split_once(':') {
                        Some((col, rest)) if !col.is_empty() && col.chars().all(|c| c.is_ascii_digit()) => (format!("{}:{}:{}\n", name, line.trim(), col), rest),
                        _ => (format!("{}:{}:", name, line.trim()), text),
                    };
                    out.push((loc.0, loc.1.trim().to_string()));
                }
            }
            out
        };
        let expected = comments("// error:");
        let out = if p.is_dir() {
            let target = cache.join("projects").join(p.file_name().unwrap());
            oxie().arg("build").current_dir(p).env("OXIE_CACHE", cache).env("CARGO_TARGET_DIR", target).output().unwrap()
        } else {
            oxie().arg("run").arg(p).current_dir(&dir).env("OXIE_CACHE", cache).output().unwrap()
        };
        let stderr = String::from_utf8_lossy(&out.stderr);
        if out.status.success() {
            failures.push(format!("{}: expected compilation to fail", p.display()));
            return failures;
        }
        // user errors must never be reported as Oxie generating invalid Rust
        if stderr.contains("a bug in Oxie") {
            failures.push(format!("{}: reported as a compiler bug\nstderr:\n{}", p.display(), stderr));
        }
        for (loc, text) in expected {
            let found = stderr.split("\n\n").any(|block| block.contains(&loc) && block.contains(&text));
            if !found {
                failures.push(format!("{}: missing error at {} containing `{}`\nstderr:\n{}", p.display(), loc.trim(), text, stderr));
            }
        }
        for (loc, text) in comments("// not:") {
            if stderr.split("\n\n").any(|block| block.contains(&loc) && block.contains(&text)) {
                failures.push(format!("{}: unexpected error at {} containing `{}`\nstderr:\n{}", p.display(), loc.trim(), text, stderr));
            }
        }
        // rustc reports some mistakes twice; each is shown once
        let mut shown = std::collections::HashSet::new();
        for block in stderr.split("\n\n") {
            let mut lines = block.lines();
            if let (Some(head), Some(loc)) = (lines.next(), lines.next()) {
                if head.starts_with("error") && !shown.insert((head, loc)) {
                    failures.push(format!("{}: shown twice: {} {}\nstderr:\n{}", p.display(), head, loc.trim(), stderr));
                }
            }
        }
        failures
    });
    let failures: Vec<String> = results.into_iter().flatten().collect();
    assert!(failures.is_empty(), "{}", failures.join("\n\n"));
}

/// Every example project under `examples/` with an `expected.txt` must print exactly that,
/// and every example's `test` blocks must pass.
#[test]
fn examples_run_and_test() {
    let dir = repo().join("examples");
    let mut entries: Vec<PathBuf> = std::fs::read_dir(&dir).unwrap().filter_map(|e| e.ok()).map(|e| e.path()).filter(|p| p.join("Cargo.toml").exists()).collect();
    entries.sort();
    let results = parallel(&entries, |ex, _| {
        let mut failures = Vec::new();
        if let Ok(expected) = std::fs::read_to_string(ex.join("expected.txt")) {
            let out = oxie().arg("run").current_dir(ex).env("TODO_FILE", "/nonexistent/todo.txt").output().unwrap();
            let stdout = String::from_utf8_lossy(&out.stdout);
            if stdout != expected {
                failures.push(format!("{}:\n--- expected\n{}\n--- actual\n{}\n--- stderr\n{}", ex.display(), expected, stdout, String::from_utf8_lossy(&out.stderr)));
            }
        }
        let out = oxie().arg("test").current_dir(ex).output().unwrap();
        if !out.status.success() {
            failures.push(format!("{}: `oxie test` failed\n{}{}", ex.display(), String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr)));
        }
        failures
    });
    let failures: Vec<String> = results.into_iter().flatten().collect();
    assert!(failures.is_empty(), "{}", failures.join("\n\n"));
}

/// Every ```oxie block with `fun main` in README.md, docs/*.md and the guide `oxie guide` prints
/// must compile and run. The output of those in README.md and docs/ is recorded in
/// `website/src/data/doc-outputs.json`, which the docs site shows beside each example (unless
/// it contains `// output varies`); `OXIE_BLESS=1` rewrites it when an example or its output
/// changes.
#[test]
fn documentation_examples_run() {
    let root = repo();
    let mut files = vec![root.join("README.md")];
    files.extend(markdown_files(&root.join("docs")));
    files.push(root.join("crates/oxie-cli/guide/writing-oxie.md"));
    // the same directory every time, so the next run reuses the builds of unchanged examples
    let tmp = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("doc-examples");
    std::fs::create_dir_all(&tmp).unwrap();
    // (file, its path relative to the repository, first line, source)
    let mut examples = Vec::new();
    for f in files {
        let rel = f.strip_prefix(&root).unwrap().to_string_lossy().replace('\\', "/");
        let text = std::fs::read_to_string(&f).unwrap();
        let mut in_block = false;
        let mut block = String::new();
        let mut start_line = 0;
        for (i, line) in text.lines().enumerate() {
            if !in_block && line.trim_start().starts_with("```oxie") {
                in_block = true;
                block.clear();
                start_line = i + 2;
                continue;
            }
            if in_block && line.trim_start().starts_with("```") {
                in_block = false;
                if block.contains("fun main") && !block.contains("// not-runnable") {
                    examples.push((f.clone(), rel.clone(), start_line, block.clone()));
                }
                continue;
            }
            if in_block {
                block.push_str(line);
                block.push('\n');
            }
        }
    }
    let count = examples.len();
    let results = parallel(&examples, |(f, _, start_line, block), cache| {
        let name = format!("{}_{}.oxi", f.file_stem().unwrap().to_string_lossy().replace('-', "_"), start_line);
        let path = tmp.join(&name);
        if std::fs::read_to_string(&path).ok().as_deref() != Some(block.as_str()) {
            std::fs::write(&path, block).unwrap();
        }
        let out = oxie().arg("run").arg(&path).current_dir(&tmp).env("OXIE_CACHE", cache).output().unwrap();
        let expect_failure = block.contains("throw") && block.contains("fun main() throws") && block.contains("// exits with an error");
        let failure = (out.status.success() == expect_failure).then(|| format!("{}:{}\n{}", f.display(), start_line, String::from_utf8_lossy(&out.stderr)));
        // a program that exits with an error shows the `Error: ...` line main prints
        let mut output = String::from_utf8_lossy(&out.stdout).into_owned();
        for l in String::from_utf8_lossy(&out.stderr).lines().filter(|l| expect_failure && l.starts_with("Error: ")) {
            output.push_str(l);
            output.push('\n');
        }
        (failure, output)
    });
    let mut failures = Vec::new();
    let mut outputs = Vec::new();
    for ((_, rel, start_line, block), (failure, output)) in examples.iter().zip(results) {
        failures.extend(failure);
        // no line numbers, so editing the text around an example doesn't make this stale
        if !block.contains("// output varies") && !rel.starts_with("crates/") {
            outputs.push((*start_line, serde_json::json!({ "file": rel, "source": block, "output": output })));
        }
    }
    assert!(count > 10, "found only {} runnable examples", count);
    assert!(failures.is_empty(), "{} documentation example(s) failed:\n{}", failures.len(), failures.join("\n\n"));

    // OS error messages in the output read differently on Windows
    if cfg!(windows) {
        return;
    }
    let recorded_path = root.join("website/src/data/doc-outputs.json");
    let actual = serde_json::Value::Array(outputs.iter().map(|(_, o)| o.clone()).collect());
    if std::env::var_os("OXIE_BLESS").is_some() {
        std::fs::write(&recorded_path, serde_json::to_string_pretty(&actual).unwrap() + "\n").unwrap();
        return;
    }
    let recorded: serde_json::Value = std::fs::read_to_string(&recorded_path).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
    let mut stale = Vec::new();
    for (line, example) in &outputs {
        let same = recorded.as_array().into_iter().flatten().find(|r| r["source"] == example["source"]);
        match same {
            Some(r) if r == example => {}
            Some(r) => stale.push(format!("{}:{}: output changed\n--- recorded\n{}--- actual\n{}", example["file"].as_str().unwrap(), line, r["output"].as_str().unwrap_or(""), example["output"].as_str().unwrap())),
            None => stale.push(format!("{}:{}: new or changed example", example["file"].as_str().unwrap(), line)),
        }
    }
    if stale.is_empty() && recorded != actual {
        stale.push("examples were removed or moved".into());
    }
    assert!(stale.is_empty(), "website/src/data/doc-outputs.json is out of date; after checking the changes below, update it with\n    OXIE_BLESS=1 cargo test -p oxie --test e2e documentation_examples_run\n\n{}", stale.join("\n\n"));
}

fn markdown_files(dir: &Path) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = std::fs::read_dir(dir).unwrap().filter_map(|e| e.ok()).map(|e| e.path()).filter(|p| p.extension().map_or(false, |e| e == "md")).collect();
    v.sort();
    v
}

/// The pages of docs/ that `oxie guide <topic>` prints are built into the binary from copies in
/// `crates/oxie-cli/guide/docs` (a published crate can't include files outside its directory).
/// They must equal docs/, and every page of docs/ is a topic except the ones an agent writing
/// Oxie doesn't need (NOT_TOPICS). `OXIE_BLESS=1` copies docs/ over them.
#[test]
fn guide_topics_match_docs() {
    // internals.md is for working on Oxie, ai-agents.md for setting an agent up: an agent
    // reading `oxie guide` needs neither
    const NOT_TOPICS: [&str; 2] = ["internals.md", "ai-agents.md"];
    let root = repo();
    let copies = root.join("crates/oxie-cli/guide/docs");
    let mut problems = Vec::new();
    for doc in markdown_files(&root.join("docs")) {
        let name = doc.file_name().unwrap().to_string_lossy().to_string();
        let copy = copies.join(&name);
        if NOT_TOPICS.contains(&name.as_str()) {
            assert!(!copy.exists(), "docs/{name} isn't a guide topic, but crates/oxie-cli/guide/docs/{name} exists");
            continue;
        }
        let text = std::fs::read_to_string(&doc).unwrap();
        if std::fs::read_to_string(&copy).ok().as_deref() == Some(text.as_str()) {
            continue;
        }
        if !copy.exists() {
            problems.push(format!("docs/{name} is new: add it to TOPICS in crates/oxie-cli/src/guide.rs (and to the topics in guide/writing-oxie.md), or to NOT_TOPICS here"));
        } else if std::env::var_os("OXIE_BLESS").is_some() {
            std::fs::write(&copy, text).unwrap();
        } else {
            problems.push(format!("crates/oxie-cli/guide/docs/{name} differs from docs/{name}"));
        }
    }
    for copy in markdown_files(&copies) {
        let name = copy.file_name().unwrap().to_string_lossy().to_string();
        if !root.join("docs").join(&name).exists() {
            problems.push(format!("crates/oxie-cli/guide/docs/{name} has no docs/{name} any more"));
        }
    }
    assert!(problems.is_empty(), "{}\n\nAfter checking docs/, update the copies with\n    OXIE_BLESS=1 cargo test -p oxie --test e2e guide_topics_match_docs", problems.join("\n"));
}

/// `oxie guide` prints the guide with this version, its topics print the pages of docs/, and
/// in a project that builds with another `oxie-build` it says so.
#[test]
fn guide_describes_this_version() {
    let version = env!("CARGO_PKG_VERSION");
    let out = oxie().arg("guide").current_dir(std::env::temp_dir()).output().unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success() && text.contains(&format!("oxie {version}")) && text.contains("# Writing Oxie"), "{text}");
    let reference = oxie().args(["guide", "reference"]).output().unwrap();
    assert_eq!(String::from_utf8_lossy(&reference.stdout), std::fs::read_to_string(repo().join("docs/reference.md")).unwrap());
    assert!(!oxie().args(["guide", "nonsense"]).output().unwrap().status.success());

    let tmp = std::env::temp_dir().join(format!("oxie-guide-{}", std::process::id()));
    std::fs::create_dir_all(&tmp).unwrap();
    assert!(oxie().args(["new", "older"]).current_dir(&tmp).status().unwrap().success());
    let lock = "version = 4\n\n[[package]]\nname = \"oxie-build\"\nversion = \"0.0.1\"\nsource = \"registry+https://github.com/rust-lang/crates.io-index\"\n";
    std::fs::write(tmp.join("older/Cargo.lock"), lock).unwrap();
    let out = oxie().arg("guide").current_dir(tmp.join("older/src")).output().unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("builds with oxie-build 0.0.1") && text.contains(&format!("`oxie-build = \"{version}\"`")), "{text}");
    let _ = std::fs::remove_dir_all(&tmp);
}

/// The Claude Code skill sends agents to `oxie guide` and teaches no Oxie itself: rules written
/// into it would be wrong for every other version of Oxie its users have installed.
#[test]
fn skill_defers_to_oxie_guide() {
    let skill = std::fs::read_to_string(repo().join("skills/oxie/SKILL.md")).unwrap();
    assert!(skill.starts_with("---\nname: oxie\ndescription: "), "SKILL.md needs its frontmatter");
    assert!(skill.contains("oxie guide"), "SKILL.md must send agents to `oxie guide`");
    assert!(!skill.contains("```oxie"), "SKILL.md must not contain Oxie code; the rules belong in crates/oxie-cli/guide/writing-oxie.md");
}

/// The reference solution of every task in `evals/tasks` builds and prints the task's
/// expected.out, and its tests pass.
#[test]
fn eval_solutions_pass() {
    let mut tasks: Vec<PathBuf> = std::fs::read_dir(repo().join("evals/tasks")).unwrap().filter_map(|e| e.ok()).map(|e| e.path()).filter(|p| p.join("expected.out").exists()).collect();
    tasks.sort();
    let tmp = std::env::temp_dir().join(format!("oxie-evals-{}", std::process::id()));
    let results = parallel(&tasks, |task, cache| {
        let name = task.file_name().unwrap().to_string_lossy().to_string();
        let dir = tmp.join(&name);
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&tmp).unwrap();
        assert!(oxie().args(["new", &name]).current_dir(&tmp).output().unwrap().status.success());
        let solution = task.join("solution/src");
        let mut has_tests = false;
        for f in programs(&solution) {
            let text = std::fs::read_to_string(&f).unwrap();
            has_tests |= text.contains("\ntest \"");
            std::fs::write(dir.join("src").join(f.file_name().unwrap()), text).unwrap();
        }
        let target = cache.join("evals");
        let out = oxie().arg("run").current_dir(&dir).env("CARGO_TARGET_DIR", &target).output().unwrap();
        let expected = std::fs::read_to_string(task.join("expected.out")).unwrap();
        let mut failures = Vec::new();
        if String::from_utf8_lossy(&out.stdout) != expected {
            failures.push(format!("{name}:\n--- expected\n{expected}--- actual\n{}--- stderr\n{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr)));
        }
        if has_tests {
            let out = oxie().arg("test").current_dir(&dir).env("CARGO_TARGET_DIR", &target).output().unwrap();
            if !out.status.success() {
                failures.push(format!("{name}: `oxie test` failed\n{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr)));
            }
        }
        failures
    });
    let _ = std::fs::remove_dir_all(&tmp);
    let failures: Vec<String> = results.into_iter().flatten().collect();
    assert!(tasks.len() > 10, "found only {} eval tasks", tasks.len());
    assert!(failures.is_empty(), "{}", failures.join("\n\n"));
}

/// Every command `oxie --help` lists is described in docs/tooling.md.
#[test]
fn commands_are_documented() {
    let help = oxie().arg("--help").output().unwrap();
    let help = String::from_utf8_lossy(&help.stdout);
    let docs = std::fs::read_to_string(repo().join("docs/tooling.md")).unwrap();
    // command lines are indented by four spaces, their continuation lines by more
    let commands = help.lines().skip_while(|l| !l.starts_with("COMMANDS:")).skip(1).take_while(|l| !l.trim().is_empty());
    let missing: Vec<&str> = commands.filter(|l| !l.starts_with("     ")).filter_map(|l| l.split_whitespace().next()).filter(|c| !docs.contains(&format!("`oxie {c}"))).collect();
    assert!(missing.is_empty(), "docs/tooling.md doesn't describe `oxie {}`", missing.join("`, `oxie "));
}

/// `oxie <command> --help` describes the command and runs nothing: `init --help` set up the
/// current directory, `fmt --help` formatted it, and `run --help` ran the project.
#[test]
fn help_never_runs_a_command() {
    let help = oxie().arg("--help").output().unwrap();
    let help = String::from_utf8_lossy(&help.stdout).to_string();
    let commands: Vec<&str> = help.lines().skip_while(|l| !l.starts_with("COMMANDS:")).skip(1).take_while(|l| !l.trim().is_empty()).filter(|l| !l.starts_with("     ")).filter_map(|l| l.split_whitespace().next()).collect();
    assert!(commands.len() > 10, "{}", help);
    let tmp = std::env::temp_dir().join(format!("oxie-help-{}", std::process::id()));
    std::fs::create_dir_all(&tmp).unwrap();
    // a crate, so that `init`, `run` or `test` would find something to act on
    std::fs::write(tmp.join("Cargo.toml"), "[package]\nname = \"helped\"\nversion = \"0.1.0\"\nedition = \"2021\"\n").unwrap();
    for cmd in commands.iter().copied().chain(["format"]) {
        // every way to use it that `oxie --help` lists (`from-rust <file.rs>` and `from-rust <dir>`)
        let forms: Vec<&str> = help.lines().filter_map(|l| l.strip_prefix("    ")).filter(|l| l.split_whitespace().next() == Some(cmd)).filter_map(|l| l.split("  ").next()).collect();
        for flag in ["--help", "-h"] {
            let out = oxie().args([cmd, flag]).current_dir(&tmp).output().unwrap();
            let stdout = String::from_utf8_lossy(&out.stdout);
            assert!(out.status.success() && stdout.starts_with("USAGE:\n    oxie "), "`oxie {} {}`:\n{}{}", cmd, flag, stdout, String::from_utf8_lossy(&out.stderr));
            for form in &forms {
                assert!(stdout.contains(&format!("oxie {}\n", form)), "`oxie {} {}` doesn't show `{}`:\n{}", cmd, flag, form, stdout);
            }
        }
    }
    let left: Vec<String> = std::fs::read_dir(&tmp).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().to_string()).collect();
    std::fs::remove_dir_all(&tmp).unwrap();
    assert_eq!(left, ["Cargo.toml"], "a `--help` changed the directory");
}

/// The reserved words listed in the language reference are the ones the parser reserves.
#[test]
fn keywords_are_documented() {
    let reference = std::fs::read_to_string(repo().join("docs/reference.md")).unwrap();
    let start = reference.find("Reserved: `").expect("docs/reference.md lists the reserved words after \"Reserved: `\"") + "Reserved: `".len();
    let listed = &reference[start..start + reference[start..].find('`').unwrap()];
    let mut listed: Vec<&str> = listed.split_whitespace().collect();
    let mut reserved = oxie_compiler::parser::RESERVED.to_vec();
    listed.sort();
    reserved.sort();
    assert_eq!(listed, reserved, "the reserved words in docs/reference.md differ from oxie_compiler::parser::RESERVED");
}

/// `oxie new` and `oxie init` take oxie-build from crates.io, or from `OXIE_HOME` when it is set.
#[test]
fn new_projects_depend_on_oxie_build() {
    let tmp = std::env::temp_dir().join(format!("oxie-new-{}", std::process::id()));
    std::fs::create_dir_all(&tmp).unwrap();
    let build_dep = |dir: &Path| {
        let manifest = std::fs::read_to_string(dir.join("Cargo.toml")).unwrap();
        manifest.lines().find(|l| l.starts_with("oxie-build")).unwrap_or_default().to_string()
    };

    assert!(oxie().env_remove("OXIE_HOME").args(["new", "from_crates_io"]).current_dir(&tmp).status().unwrap().success());
    assert_eq!(build_dep(&tmp.join("from_crates_io")), format!(r#"oxie-build = "{}""#, env!("CARGO_PKG_VERSION")));
    // agents are sent to `oxie guide`, which matches the installed Oxie, instead of being told rules
    let agents = std::fs::read_to_string(tmp.join("from_crates_io/AGENTS.md")).unwrap();
    assert!(agents.contains("run `oxie guide`") && !agents.contains("```"), "{agents}");

    assert!(oxie().args(["new", "from_checkout"]).current_dir(&tmp).status().unwrap().success());
    let local = repo().join("crates/oxie-build").display().to_string().replace('\\', "/");
    assert_eq!(build_dep(&tmp.join("from_checkout")), format!(r#"oxie-build = {{ path = "{local}" }}"#));

    let rust = tmp.join("rust_crate");
    std::fs::create_dir_all(rust.join("src")).unwrap();
    std::fs::write(rust.join("Cargo.toml"), "[package]\nname = \"rust_crate\"\nversion = \"0.1.0\"\nedition = \"2021\"\n").unwrap();
    assert!(oxie().env_remove("OXIE_HOME").arg("init").current_dir(&rust).status().unwrap().success());
    assert_eq!(build_dep(&rust), format!(r#"oxie-build = "{}""#, env!("CARGO_PKG_VERSION")));
    assert!(rust.join("build.rs").exists() && rust.join("AGENTS.md").exists());

    // a crate that already has build-dependencies gets oxie-build in the same table
    let with_deps = tmp.join("with_build_deps");
    std::fs::create_dir_all(with_deps.join("src")).unwrap();
    std::fs::write(with_deps.join("Cargo.toml"), "[package]\nname = \"with_build_deps\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[build-dependencies]\ncc = \"1\"\n").unwrap();
    std::fs::write(with_deps.join("src/main.rs"), "fn main() {}\n").unwrap();
    assert!(oxie().arg("init").current_dir(&with_deps).status().unwrap().success());
    let manifest = std::fs::read_to_string(with_deps.join("Cargo.toml")).unwrap();
    assert_eq!(manifest.matches("[build-dependencies]").count(), 1, "{manifest}");
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
    let read = Command::new(cargo).args(["metadata", "--no-deps", "--format-version", "1"]).current_dir(&with_deps).output().unwrap();
    assert!(read.status.success(), "{}", String::from_utf8_lossy(&read.stderr));

    let _ = std::fs::remove_dir_all(&tmp);
}
