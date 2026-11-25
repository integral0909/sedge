//! The interactive prompt.
//!
//! Each input is compiled together with the definitions and statements
//! entered so far, and the whole program is run again. Programs are
//! deterministic, so the output of earlier statements repeats exactly and
//! only the new part is shown. (Inputs that read the clock or call
//! `seed_random` with different values can break this.)

use crate::{build, SharedBuf};
use sedge_check::Ty;
use sedge_syntax::parser::parse_expr;

#[derive(Default)]
pub struct Session {
    items: Vec<String>,
    stmts: Vec<String>,
    shown: usize,
}

const HELP: &str = "\
Enter definitions (fn, struct, enum, impl) or statements. Expressions
print their value.
  :type <expr>   show the type of an expression
  :reset         forget everything entered so far
  :help          show this message
  :quit          leave (or press Ctrl-D)
";

pub enum Reply {
    Output(String),
    Quit,
}

fn is_item(input: &str) -> bool {
    let first = input.split_whitespace().next().unwrap_or("");
    matches!(first, "fn" | "struct" | "enum" | "impl") || input.starts_with("///")
}

impl Session {
    pub fn new() -> Session {
        Session::default()
    }

    fn source(&self, extra_items: &[&str], extra_stmts: &[&str]) -> String {
        let mut s = String::new();
        for item in self
            .items
            .iter()
            .map(|s| s.as_str())
            .chain(extra_items.iter().copied())
        {
            s.push_str(item);
            s.push('\n');
        }
        s.push_str("fn main() {\n");
        for st in self
            .stmts
            .iter()
            .map(|s| s.as_str())
            .chain(extra_stmts.iter().copied())
        {
            s.push_str(st);
            s.push('\n');
        }
        s.push_str("}\n");
        s
    }

    /// Whether the input looks finished (brackets balanced).
    pub fn is_complete(input: &str) -> bool {
        let mut depth = 0i32;
        let mut in_str = false;
        let mut prev = ' ';
        for c in input.chars() {
            if in_str {
                if c == '"' && prev != '\\' {
                    in_str = false;
                }
            } else {
                match c {
                    '"' => in_str = true,
                    '(' | '[' | '{' => depth += 1,
                    ')' | ']' | '}' => depth -= 1,
                    _ => {}
                }
            }
            prev = c;
        }
        depth <= 0 && !in_str
    }

    pub fn eval(&mut self, input: &str) -> Reply {
        let input = input.trim();
        if input.is_empty() {
            return Reply::Output(String::new());
        }
        match input {
            ":quit" | ":q" => return Reply::Quit,
            ":help" | ":h" => return Reply::Output(HELP.to_string()),
            ":reset" => {
                *self = Session::default();
                return Reply::Output("cleared\n".into());
            }
            _ => {}
        }
        if let Some(e) = input.strip_prefix(":type ") {
            return Reply::Output(self.type_of(e));
        }
        if is_item(input) {
            let src = self.source(&[input], &[]);
            return Reply::Output(match build(&[("repl".into(), src)], false) {
                Ok(_) => {
                    self.items.push(input.to_string());
                    String::new()
                }
                Err(diags) => diags,
            });
        }
        let mut id = 0;
        let is_expr = parse_expr(input, 0, &mut id).1.is_empty();
        if is_expr {
            let binding = format!("let it = ({input})");
            let src = self.source(&[], &[&binding]);
            if let Ok((built, _)) = build(&[("repl".into(), src.clone())], false) {
                let ty = built
                    .analysis
                    .typed
                    .locals
                    .iter()
                    .rev()
                    .find(|l| l.name == "it")
                    .map(|l| l.ty.clone());
                if ty.as_ref().is_some_and(|t| *t != Ty::Unit) {
                    let show = format!("{binding}\nprint(debug(it))");
                    let shown_ty = built.analysis.typed.show(ty.as_ref().unwrap());
                    let out = self.run_with(&show, false);
                    // Keep the binding so `it` refers to the last value.
                    return Reply::Output(match out {
                        Ok(text) => {
                            self.stmts.push(binding);
                            let mut lines = text;
                            if lines.ends_with('\n') {
                                lines.pop();
                            }
                            format!("{lines} : {shown_ty}\n")
                        }
                        Err(e) => e,
                    });
                }
            }
        }
        Reply::Output(match self.run_with(input, true) {
            Ok(text) => text,
            Err(e) => e,
        })
    }

    /// Runs the program with `stmt` appended; on success the statement is
    /// kept (if `keep`) and the new output returned.
    fn run_with(&mut self, stmt: &str, keep: bool) -> Result<String, String> {
        let src = self.source(&[], &[stmt]);
        let (built, _) = build(&[("repl".into(), src)], false)?;
        let buf = SharedBuf::default();
        let result = crate::run(&built, Vec::new(), Box::new(buf.clone()));
        let text = buf.text();
        let new = text
            .get(self.shown.min(text.len())..)
            .unwrap_or("")
            .to_string();
        match result {
            Ok(()) => {
                if keep {
                    self.stmts.push(stmt.to_string());
                    self.shown = text.len();
                } else {
                    // The display line is not part of the kept output.
                    let kept = text.len() - new.len();
                    self.shown = kept;
                }
                Ok(new)
            }
            Err(e) => Err(format!("{new}{e}")),
        }
    }

    fn type_of(&self, expr: &str) -> String {
        let binding = format!("let it = ({expr})");
        let src = self.source(&[], &[&binding]);
        match build(&[("repl".into(), src)], false) {
            Ok((built, _)) => {
                let t = &built.analysis.typed;
                match t.locals.iter().rev().find(|l| l.name == "it") {
                    Some(l) => format!("{}\n", t.show(&l.ty)),
                    None => "?\n".into(),
                }
            }
            Err(e) => e,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn out(s: &mut Session, input: &str) -> String {
        match s.eval(input) {
            Reply::Output(o) => o,
            Reply::Quit => "<quit>".into(),
        }
    }

    #[test]
    fn remembers_definitions_and_variables() {
        let mut s = Session::new();
        assert_eq!(out(&mut s, "fn sq(x: Int) -> Int { x * x }"), "");
        assert_eq!(out(&mut s, "var total = sq(4)"), "");
        assert_eq!(out(&mut s, "print(\"hi\")"), "hi\n");
        assert_eq!(out(&mut s, "total += 1"), "");
        assert_eq!(out(&mut s, "total"), "17 : Int\n");
        assert_eq!(out(&mut s, "[it, 2]"), "[17, 2] : [Int]\n");
        assert_eq!(out(&mut s, ":type sq"), "fn(Int) -> Int\n");
        assert!(out(&mut s, "undefined_name").contains("cannot find"));
        assert_eq!(out(&mut s, "\"s\""), "\"s\" : String\n");
        assert_eq!(out(&mut s, ":quit"), "<quit>");
    }

    #[test]
    fn completeness() {
        assert!(!Session::is_complete("fn f() {"));
        assert!(Session::is_complete("fn f() { \"}\" }"));
    }
}
