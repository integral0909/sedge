//! Human-readable listings of compiled code.

use crate::bytecode::{Const, Op, Program, Proto};
use std::fmt::Write;

pub fn disassemble(program: &Program) -> String {
    let mut out = String::new();
    for (i, p) in program.protos.iter().enumerate() {
        if p.builtin {
            continue;
        }
        out.push_str(&disassemble_proto(program, i as u32, p));
        out.push('\n');
    }
    out
}

pub fn disassemble_proto(program: &Program, index: u32, p: &Proto) -> String {
    let mut out = String::new();
    let _ = writeln!(
        out,
        "fn #{index} {} (arity {}, {} registers)",
        p.name, p.arity, p.num_regs
    );
    let mut last_line = 0;
    for (ip, op) in p.code.iter().enumerate() {
        let line = p.lines.get(ip).copied().unwrap_or(0);
        let line_col = if line != last_line {
            last_line = line;
            format!("{line:>4}")
        } else {
            "   |".to_string()
        };
        let _ = writeln!(out, "{line_col}  {ip:>4}  {}", show_op(program, ip, op));
    }
    out
}

fn target(ip: usize, offset: i32) -> i64 {
    ip as i64 + 1 + offset as i64
}

fn show_op(program: &Program, ip: usize, op: &Op) -> String {
    let fname = |f: u32| {
        program
            .protos
            .get(f as usize)
            .map(|p| p.name.clone())
            .unwrap_or_else(|| "?".into())
    };
    match *op {
        Op::LoadConst { dst, idx } => {
            let c = match &program.consts[idx as usize] {
                Const::Int(i) => i.to_string(),
                Const::Float(f) => format!("{f:?}"),
                Const::Str(s) => format!("{s:?}"),
            };
            format!("r{dst} = {c}")
        }
        Op::LoadInt { dst, value } => format!("r{dst} = {value}"),
        Op::LoadBool { dst, value } => format!("r{dst} = {value}"),
        Op::LoadUnit { dst } => format!("r{dst} = ()"),
        Op::LoadTag { dst, adt, tag } => {
            let t = &program.types[adt as usize];
            format!("r{dst} = {}.{}", t.name, t.variants[tag as usize])
        }
        Op::Move { dst, src } => format!("r{dst} = r{src}"),
        Op::AddImm { dst, a, value } => format!("r{dst} = r{a} + {value}"),
        Op::JumpIf { cond, offset } => format!("if r{cond} goto {}", target(ip, offset)),
        Op::JumpIfNot { cond, offset } => format!("unless r{cond} goto {}", target(ip, offset)),
        Op::JumpIfNotLeI { a, b, offset } => {
            format!("unless r{a} <= r{b} goto {}", target(ip, offset))
        }
        Op::JumpIfNotLtI { a, b, offset } => {
            format!("unless r{a} < r{b} goto {}", target(ip, offset))
        }
        Op::Jump { offset } => format!("goto {}", target(ip, offset)),
        Op::Call { base, argc } => format!("r{base} = call r{base}(r{}..+{argc})", base + 1),
        Op::CallDirect { func, base, argc } => {
            format!("r{base} = {}(r{base}..+{argc})", fname(func))
        }
        Op::CallNative { native, base, argc } => {
            format!(
                "r{base} = native {}(r{base}..+{argc})",
                program.natives[native as usize]
            )
        }
        Op::Return { src } => format!("return r{src}"),
        Op::ReturnUnit => "return".into(),
        Op::MakeClosure {
            dst,
            func,
            base,
            count,
        } => {
            format!("r{dst} = closure {} [r{base}..+{count}]", fname(func))
        }
        Op::NewStruct {
            dst,
            adt,
            base,
            count,
        } => {
            format!(
                "r{dst} = {} {{ r{base}..+{count} }}",
                program.types[adt as usize].name
            )
        }
        Op::NewVariant {
            dst,
            adt,
            tag,
            base,
            count,
        } => {
            let t = &program.types[adt as usize];
            format!(
                "r{dst} = {}.{}(r{base}..+{count})",
                t.name, t.variants[tag as usize]
            )
        }
        Op::JumpIfNotTag { obj, tag, offset } => {
            format!("unless tag(r{obj}) == {tag} goto {}", target(ip, offset))
        }
        Op::Panic { msg } => format!("panic {:?}", program.consts[msg as usize]),
        other => {
            // Remaining instructions print as "name a b c".
            let s = format!("{other:?}");
            let (name, rest) = s.split_once(' ').unwrap_or((&s, ""));
            let args: Vec<String> = rest
                .trim_matches(|c| c == '{' || c == '}' || c == ' ')
                .split(", ")
                .filter(|p| !p.is_empty())
                .map(|p| match p.split_once(": ") {
                    Some((k, v)) if k != "idx" && k != "count" => format!("r{v}"),
                    Some((_, v)) => v.to_string(),
                    None => p.to_string(),
                })
                .collect();
            format!("{} {}", name.to_lowercase(), args.join(" "))
        }
    }
}
