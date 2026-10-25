//! What `oxie run` and `oxie test` show while a program runs: panics and failed tests point at
//! the `.oxi` source, not at the generated Rust.

use std::path::PathBuf;
use std::process::Command;

fn repo() -> PathBuf {
    oxie_compiler::compile::canonicalize(&PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")).unwrap()
}

fn oxie() -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_oxie"));
    c.env("NO_COLOR", "1").env("OXIE_HOME", repo()).env_remove("RUST_BACKTRACE");
    c
}

/// A fresh directory holding `name` with `src`.
fn script(name: &str, src: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("oxie-runtime-{}-{}", std::process::id(), name.trim_end_matches(".oxi")));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join(name), src).unwrap();
    oxie_compiler::compile::canonicalize(&dir).unwrap()
}

#[test]
fn panics_point_at_the_oxi_source() {
    let dir = script("panic.oxi", "fun nth(xs: &[i32], i: usize): i32 = xs[i]\n\nfun main() {\n    println(nth(vec[1, 2, 3], 7))\n}\n");
    let out = oxie().args(["run", "panic.oxi"]).current_dir(&dir).output().unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(101), "{stderr}");
    assert!(stderr.contains("panicked at panic.oxi:1:38:"), "{stderr}");
    assert!(!stderr.contains("main.rs"), "{stderr}");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn errors_from_main_show_each_cause_once() {
    let src = "error enum AppError {\n    BadNumber(std::num::ParseIntError)\n}\n\nfun parse(s: &str): i32 throws AppError {\n    val n: i32 = s.parse()?\n    n\n}\n\nfun main() throws {\n    parse(\"x\")\n}\n";
    let dir = script("report.oxi", src);
    let out = oxie().args(["run", "report.oxi"]).current_dir(&dir).output().unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("error: invalid digit found in string"), "{stderr}");
    assert!(!stderr.contains("caused by"), "{stderr}");
    let _ = std::fs::remove_dir_all(dir);
}

// backtrace frames on Windows have no column (`at C:\..\main.rs:3`), so they aren't rewritten
#[cfg(not(windows))]
#[test]
fn backtraces_name_main_and_point_at_the_oxi_source() {
    let dir = script("trace.oxi", "fun main() throws {\n    val v: Vec<i32> = vec[]\n    println(v[3])\n}\n");
    let out = oxie().args(["run", "trace.oxi"]).current_dir(&dir).env("RUST_BACKTRACE", "1").output().unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("panicked at trace.oxi:3:"), "{stderr}");
    // `main() throws` runs as a generated function called from Rust's `main`: both frames say
    // `main`, the generated one at the `fun main` line
    assert!(!stderr.contains("__oxie_main"), "{stderr}");
    let lines: Vec<&str> = stderr.lines().collect();
    let frames: Vec<&str> = lines.windows(2).filter(|w| w[0].ends_with("::main")).map(|w| w[1].trim()).collect();
    assert!(frames.iter().any(|at| at.starts_with("at trace.oxi:3:")), "{stderr}");
    assert!(frames.iter().any(|at| at.starts_with("at trace.oxi:1:")), "{stderr}");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn failed_tests_point_at_the_oxi_source() {
    let dir = script("failing.oxi", "fun double(x: i32): i32 = x * 2\n\ntest \"doubles\" {\n    assert_eq(double(2), 5)\n}\n");
    let out = oxie().args(["test", "failing.oxi"]).current_dir(&dir).output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(!out.status.success());
    assert!(stdout.contains("panicked at failing.oxi:4:5:"), "{stdout}");
    let _ = std::fs::remove_dir_all(dir);
}

/// A method of a shared object that reaches the same object through another handle panics with
/// an explanation at that line; an `@sync` one panics too instead of waiting for its own lock.
#[test]
fn shared_objects_reached_again_explain_the_panic() {
    let src = "shared class Node(var v: i32 = 0) {\n    fun copy_from(o: Node) { v = o.v }\n}\n\nfun main() {\n    val a = Node(1)\n    a.copy_from(a)\n}\n";
    for (name, src) in [("aliased.oxi", src.to_string()), ("locked.oxi", src.replace("shared class", "@sync shared class"))] {
        let dir = script(name, &src);
        let (tx, rx) = std::sync::mpsc::channel();
        let mut cmd = oxie();
        cmd.args(["run", name]).current_dir(&dir);
        std::thread::spawn(move || tx.send(cmd.output().unwrap()).unwrap());
        // a deadlock would never finish
        let out = rx.recv_timeout(std::time::Duration::from_secs(600)).expect("the program hangs");
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert_eq!(out.status.code(), Some(101), "{stderr}");
        assert!(stderr.contains(&format!("panicked at {}:2:34:", name)), "{stderr}");
        assert!(stderr.contains("this Node can't be"), "{stderr}");
        assert!(stderr.contains("reached the same object again"), "{stderr}");
        let _ = std::fs::remove_dir_all(dir);
    }
}
