//! Golden tests: every program under `tests/` declares what it should
//! produce. `//> text` lines are expected standard output, in order, and
//! `//! text` lines are fragments that must appear in the diagnostics or
//! the runtime error.

use sedge::{build, check_main, run, SharedBuf};
use std::path::{Path, PathBuf};

fn files(dir: &str) -> Vec<PathBuf> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").join(dir);
    let mut out: Vec<PathBuf> = std::fs::read_dir(&root)
        .unwrap_or_else(|e| panic!("{}: {e}", root.display()))
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|x| x == "sg"))
        .collect();
    out.sort();
    assert!(!out.is_empty(), "no programs in {}", root.display());
    out
}

struct Expect {
    stdout: String,
    fragments: Vec<String>,
}

fn expectations(text: &str) -> Expect {
    let mut stdout = String::new();
    let mut fragments = Vec::new();
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("//>") {
            stdout.push_str(rest.strip_prefix(' ').unwrap_or(rest));
            stdout.push('\n');
        } else if let Some(rest) = line.strip_prefix("//!") {
            fragments.push(rest.trim().to_string());
        }
    }
    Expect { stdout, fragments }
}

enum Outcome {
    Ran(String),
    CompileError(String),
    Panicked { stdout: String, error: String },
}

fn execute(path: &Path) -> (Outcome, Expect) {
    let text = std::fs::read_to_string(path).unwrap();
    let name = path.file_name().unwrap().to_string_lossy().into_owned();
    let expect = expectations(&text);
    let outcome = match build(&[(name, text)], false) {
        Err(diags) => Outcome::CompileError(diags),
        Ok((built, _)) => match check_main(&built) {
            Err(e) => Outcome::CompileError(e),
            Ok(()) => {
                let buf = SharedBuf::default();
                match run(&built, Vec::new(), Box::new(buf.clone())) {
                    Ok(()) => Outcome::Ran(buf.text()),
                    Err(e) => Outcome::Panicked {
                        stdout: buf.text(),
                        error: e.to_string(),
                    },
                }
            }
        },
    };
    (outcome, expect)
}

fn assert_fragments(path: &Path, text: &str, fragments: &[String]) {
    assert!(!fragments.is_empty(), "{} has no `//!` lines", path.display());
    for f in fragments {
        assert!(
            text.contains(f.as_str()),
            "{}: expected the output to contain {f:?}, got:\n{text}",
            path.display()
        );
    }
}

#[test]
fn run_programs() {
    let mut failures = Vec::new();
    for path in files("tests/run") {
        let (outcome, expect) = execute(&path);
        match outcome {
            Outcome::Ran(out) if out == expect.stdout => {}
            Outcome::Ran(out) => failures.push(format!(
                "{}: output differs\n--- expected\n{}--- got\n{out}",
                path.display(),
                expect.stdout
            )),
            Outcome::CompileError(e) => failures.push(format!("{} failed to compile:\n{e}", path.display())),
            Outcome::Panicked { error, .. } => failures.push(format!("{} panicked:\n{error}", path.display())),
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn error_programs() {
    for path in files("tests/errors") {
        let (outcome, expect) = execute(&path);
        match outcome {
            Outcome::CompileError(e) => assert_fragments(&path, &e, &expect.fragments),
            _ => panic!("{} compiled, but it should not", path.display()),
        }
    }
}

#[test]
fn panic_programs() {
    for path in files("tests/panics") {
        let (outcome, expect) = execute(&path);
        match outcome {
            Outcome::Panicked { stdout, error } => {
                assert_eq!(stdout, expect.stdout, "{}", path.display());
                assert_fragments(&path, &error, &expect.fragments);
            }
            Outcome::Ran(_) => panic!("{} finished, but it should panic", path.display()),
            Outcome::CompileError(e) => panic!("{} failed to compile:\n{e}", path.display()),
        }
    }
}

#[test]
fn examples_compile() {
    for path in files("examples") {
        let text = std::fs::read_to_string(&path).unwrap();
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        if let Err(e) = build(&[(name, text)], false) {
            panic!("{} does not compile:\n{e}", path.display());
        }
    }
}
