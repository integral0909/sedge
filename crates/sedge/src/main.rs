use sedge::repl::{Reply, Session};
use sedge::{build, check_main, run, run_tests, Built, ErrorKind};
use std::io::{self, BufRead, IsTerminal, Write};
use std::process::ExitCode;

const USAGE: &str = "\
sedge: a statically typed scripting language

usage:
  sedge run <file.sg> [args...]   check, compile and run a program
  sedge <file.sg> [args...]       same as `run`
  sedge check <file.sg>...        report errors and warnings
  sedge test <file.sg>...         run the `test` blocks
  sedge disasm <file.sg>          print the compiled bytecode
  sedge repl                      start the interactive prompt
  sedge lsp                       start the language server on stdio
  sedge version
";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let color = io::stderr().is_terminal() && std::env::var_os("NO_COLOR").is_none();
    match args.first().map(|s| s.as_str()) {
        None => {
            if io::stdin().is_terminal() {
                repl()
            } else {
                eprint!("{USAGE}");
                ExitCode::from(2)
            }
        }
        Some("help" | "-h" | "--help") => {
            print!("{USAGE}");
            ExitCode::SUCCESS
        }
        Some("version" | "-V" | "--version") => {
            println!("sedge {}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        Some("run") => match args.get(1) {
            Some(file) => run_file(file, args[2..].to_vec(), color),
            None => usage_error("`run` needs a file"),
        },
        Some("check") => {
            if args.len() < 2 {
                return usage_error("`check` needs at least one file");
            }
            check_files(&args[1..], color)
        }
        Some("test") => {
            if args.len() < 2 {
                return usage_error("`test` needs at least one file");
            }
            test_files(&args[1..], color)
        }
        Some("disasm") => match args.get(1) {
            Some(file) => disasm(file, color),
            None => usage_error("`disasm` needs a file"),
        },
        Some("repl") => repl(),
        Some("lsp") => {
            let stdin = io::stdin();
            let stdout = io::stdout();
            match sedge_lsp::serve(&mut stdin.lock(), &mut stdout.lock()) {
                Ok(()) => ExitCode::SUCCESS,
                Err(e) => {
                    eprintln!("sedge lsp: {e}");
                    ExitCode::FAILURE
                }
            }
        }
        Some(file) if file.ends_with(".sg") => run_file(file, args[1..].to_vec(), color),
        Some(other) => usage_error(&format!("unknown command `{other}`")),
    }
}

fn usage_error(msg: &str) -> ExitCode {
    eprintln!("sedge: {msg}\n");
    eprint!("{USAGE}");
    ExitCode::from(2)
}

fn read(file: &str) -> Result<(String, String), ExitCode> {
    match std::fs::read_to_string(file) {
        Ok(text) => Ok((file.to_string(), text)),
        Err(e) => {
            eprintln!("sedge: cannot read {file}: {e}");
            Err(ExitCode::from(2))
        }
    }
}

fn load(files: &[String], color: bool) -> Result<Built, ExitCode> {
    let mut sources = Vec::new();
    for f in files {
        sources.push(read(f)?);
    }
    match build(&sources, color) {
        Ok((built, warnings)) => {
            if !warnings.is_empty() {
                eprint!("{warnings}");
            }
            Ok(built)
        }
        Err(diags) => {
            eprint!("{diags}");
            Err(ExitCode::FAILURE)
        }
    }
}

fn run_file(file: &str, args: Vec<String>, color: bool) -> ExitCode {
    let built = match load(&[file.to_string()], color) {
        Ok(b) => b,
        Err(code) => return code,
    };
    if let Err(msg) = check_main(&built) {
        eprint!("{msg}");
        return ExitCode::FAILURE;
    }
    let out = Box::new(io::BufWriter::new(io::stdout()));
    match run(&built, args, out) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => match e.kind {
            ErrorKind::Exit(code) => ExitCode::from(code.clamp(0, 255) as u8),
            ErrorKind::Panic(_) => {
                let _ = io::stdout().flush();
                eprint!("{e}");
                ExitCode::from(101)
            }
        },
    }
}

fn check_files(files: &[String], color: bool) -> ExitCode {
    match load(files, color) {
        Ok(_) => ExitCode::SUCCESS,
        Err(code) => code,
    }
}

fn test_files(files: &[String], color: bool) -> ExitCode {
    let built = match load(files, color) {
        Ok(b) => b,
        Err(code) => return code,
    };
    let results = run_tests(&built, &mut || Box::new(io::sink()));
    if results.is_empty() {
        println!("no tests found");
        return ExitCode::SUCCESS;
    }
    let mut failed = 0;
    for r in &results {
        match &r.error {
            None => println!("ok    {}", r.name),
            Some(e) => {
                failed += 1;
                println!("FAIL  {}", r.name);
                for line in e.lines() {
                    println!("      {line}");
                }
            }
        }
    }
    println!(
        "\n{} passed, {} failed",
        results.len() - failed,
        failed
    );
    if failed == 0 {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

fn disasm(file: &str, color: bool) -> ExitCode {
    match load(&[file.to_string()], color) {
        Ok(built) => {
            print!("{}", sedge_vm::disasm::disassemble(&built.program));
            ExitCode::SUCCESS
        }
        Err(code) => code,
    }
}

fn repl() -> ExitCode {
    println!("sedge {} - type :help for help", env!("CARGO_PKG_VERSION"));
    let mut session = Session::new();
    let stdin = io::stdin();
    let mut lines = stdin.lock().lines();
    loop {
        print!("> ");
        let _ = io::stdout().flush();
        let mut input = match lines.next() {
            Some(Ok(l)) => l,
            _ => break,
        };
        while !Session::is_complete(&input) {
            print!(". ");
            let _ = io::stdout().flush();
            match lines.next() {
                Some(Ok(l)) => {
                    input.push('\n');
                    input.push_str(&l);
                }
                _ => break,
            }
        }
        match session.eval(&input) {
            Reply::Output(text) => print!("{text}"),
            Reply::Quit => break,
        }
    }
    println!();
    ExitCode::SUCCESS
}
