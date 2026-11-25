//! Mutation testing of the front end: take every program in the repository,
//! damage it in many small ways, and check that analysis and compilation
//! report errors instead of panicking. A tiny deterministic generator keeps
//! the runs reproducible.

use sedge::{analyze, build};
use std::path::Path;

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }
}

const FRAGMENTS: &[&str] = &[
    "(",
    ")",
    "{",
    "}",
    "[",
    "]",
    ",",
    ".",
    "=>",
    "->",
    "=",
    "?",
    "|",
    "..",
    "\n",
    "\"",
    "${",
    "fn",
    "let",
    "var",
    "match",
    "if",
    "else",
    "return",
    "x",
    "0",
    "1.5",
    "Some",
    "None",
    "self",
    "_",
    "::",
    "+",
    "<",
    "Int",
    "[T]",
    "struct S {}",
    "enum E { A }",
];

fn mutate(src: &str, rng: &mut Rng) -> String {
    let chars: Vec<char> = src.chars().collect();
    let mut out: Vec<char> = chars.clone();
    for _ in 0..1 + rng.below(3) {
        if out.is_empty() {
            break;
        }
        let at = rng.below(out.len());
        match rng.below(4) {
            0 => {
                let len = 1 + rng.below(8);
                let end = (at + len).min(out.len());
                out.drain(at..end);
            }
            1 => {
                let frag = FRAGMENTS[rng.below(FRAGMENTS.len())];
                for (i, c) in frag.chars().enumerate() {
                    out.insert(at + i, c);
                }
            }
            2 => {
                let len = 1 + rng.below(20);
                let end = (at + len).min(out.len());
                let piece: Vec<char> = out[at..end].to_vec();
                let to = rng.below(out.len());
                for (i, c) in piece.into_iter().enumerate() {
                    out.insert(to + i, c);
                }
            }
            _ => {
                let other = rng.below(out.len());
                out.swap(at, other);
            }
        }
    }
    out.into_iter().collect()
}

fn programs() -> Vec<(String, String)> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut out = Vec::new();
    for dir in ["examples", "tests/run", "tests/errors", "tests/panics"] {
        for entry in std::fs::read_dir(root.join(dir)).unwrap() {
            let path = entry.unwrap().path();
            if path.extension().is_some_and(|e| e == "sg") {
                let name = path.file_name().unwrap().to_string_lossy().into_owned();
                out.push((name, std::fs::read_to_string(&path).unwrap()));
            }
        }
    }
    out.sort();
    out
}

#[test]
fn damaged_programs_never_panic() {
    let rounds: usize = std::env::var("SEDGE_MUTATIONS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(40);
    let mut rng = Rng(0x5EED_CAFE_F00D_1234);
    let mut compiled = 0;
    for (name, src) in programs() {
        for round in 0..rounds {
            let damaged = mutate(&src, &mut rng);
            let (tx, rx) = std::sync::mpsc::channel();
            let (n, d) = (name.clone(), damaged.clone());
            std::thread::spawn(move || {
                let result = std::panic::catch_unwind(|| {
                    let a = analyze(&[(n.clone(), d.clone())]);
                    let _ = a.render_diagnostics(false);
                    if !a.has_errors() {
                        build(&[(n.clone(), d.clone())], false).is_ok()
                    } else {
                        false
                    }
                });
                let _ = tx.send(result.is_ok().then(|| result.unwrap()));
            });
            match rx.recv_timeout(std::time::Duration::from_secs(10)) {
                Ok(Some(true)) => compiled += 1,
                Ok(Some(false)) => {}
                Ok(None) => panic!("{name}, round {round}: the front end panicked on:\n{damaged}"),
                Err(_) => panic!("{name}, round {round}: the front end hangs on:\n{damaged}"),
            }
        }
    }
    // Some mutations (inside comments or strings) still compile.
    assert!(compiled > 0);
}
