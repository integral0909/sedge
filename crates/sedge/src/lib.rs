//! The Sedge toolchain as a library: check, compile and run programs.

pub mod repl;

pub use sedge_check::{analyze, Analysis};
pub use sedge_vm::{ErrorKind, Program, RuntimeError, Vm};

use std::io::Write;

/// A program that checked and compiled.
pub struct Built {
    pub analysis: Analysis,
    pub program: Program,
}

/// Checks and compiles source files. On failure returns the rendered
/// diagnostics. Warnings are returned with a successful build.
pub fn build(files: &[(String, String)], color: bool) -> Result<(Built, String), String> {
    let analysis = analyze(files);
    let rendered = analysis.render_diagnostics(color);
    if analysis.has_errors() {
        return Err(rendered);
    }
    match sedge_compile::compile(&analysis) {
        Ok(program) => Ok((Built { analysis, program }, rendered)),
        Err(e) => {
            let d = sedge_syntax::Diagnostic::error(e.span, e.message);
            Err(d.render(&analysis.map, color))
        }
    }
}

pub fn build_one(name: &str, text: &str, color: bool) -> Result<(Built, String), String> {
    build(&[(name.to_string(), text.to_string())], color)
}

/// Checks that a program has a usable `main`.
pub fn check_main(built: &Built) -> Result<(), String> {
    let t = &built.analysis.typed;
    match t.main_fn() {
        None => Err(
            "error: no `main` function\n  = note: add `fn main() { ... }`, which runs first\n"
                .into(),
        ),
        Some(f) => {
            let def = &t.fns[f as usize];
            if !def.params.is_empty() || def.ret != sedge_check::Ty::Unit {
                Err(format!(
                    "error: `main` must take no parameters and return nothing\n --> {}\n",
                    built.analysis.map.describe(def.name_span)
                ))
            } else {
                Ok(())
            }
        }
    }
}

/// Runs `main`, writing output to `out`.
pub fn run(built: &Built, args: Vec<String>, out: Box<dyn Write>) -> Result<(), RuntimeError> {
    let mut vm = Vm::new(built.program.clone()).expect("natives resolve");
    vm.out = out;
    vm.args = args;
    vm.run_main()
}

#[derive(Debug, PartialEq)]
pub struct TestOutcome {
    pub name: String,
    pub error: Option<String>,
}

/// Runs every `test` block, each in a fresh VM.
pub fn run_tests(built: &Built, out: &mut dyn FnMut() -> Box<dyn Write>) -> Vec<TestOutcome> {
    let mut results = Vec::new();
    for (name, proto) in &built.program.tests {
        let mut vm = Vm::new(built.program.clone()).expect("natives resolve");
        vm.out = out();
        let error = match vm.call(*proto, &[]) {
            Ok(_) => None,
            Err(e) => Some(e.to_string()),
        };
        let _ = vm.out.flush();
        results.push(TestOutcome {
            name: name.clone(),
            error,
        });
    }
    results
}

/// A writer that appends to a shared buffer, for capturing output.
#[derive(Clone, Default)]
pub struct SharedBuf(pub std::rc::Rc<std::cell::RefCell<Vec<u8>>>);

impl SharedBuf {
    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.0.borrow()).into_owned()
    }
}

impl Write for SharedBuf {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.borrow_mut().extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Compiles and runs a single source text, returning its output or the
/// first error (diagnostics or a runtime error) as text.
pub fn eval(text: &str) -> Result<String, String> {
    let (built, _) = build_one("main.sg", text, false)?;
    check_main(&built)?;
    let buf = SharedBuf::default();
    match run(&built, Vec::new(), Box::new(buf.clone())) {
        Ok(()) => Ok(buf.text()),
        Err(e) => Err(format!("{}{}", buf.text(), e)),
    }
}
